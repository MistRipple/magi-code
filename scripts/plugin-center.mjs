#!/usr/bin/env node
// 插件中心发布工具：把一组发布包整理成目录，可选地签名；也可校验目录。
//
// 最简单的用法不需要任何密钥：
//   1. node scripts/plugin-center.mjs publish --out catalog.json a-1.0.0.zip b-2.0.0.zip
//   2. 把 catalog.json 和这些 ZIP 放进同一个文件夹，托管到任意 HTTP(S) 静态地址（对象存储、
//      GitHub Pages/Releases、内网服务器）
//   3. 在 Magi「设置 → 插件 → 插件中心」填入 catalog.json 的地址即可
// 目录里的包地址默认是相对路径（相对目录文件自身），文件夹整体搬家不用重发布。
// 没有签名的目录按“指定地址”的信任级别对待：界面会标注未签名，包仍须与目录摘要一致，
// 权限仍须用户逐项授权。需要更强的发布者认证时再用 keygen + publish --key 签名，
// 并让用户在插件中心的“高级”里填入公钥——填了公钥之后只接受该密钥签名的目录。
//
// 用法：
//   node scripts/plugin-center.mjs publish --out <目录文件> [--base-url <包所在目录URL> | --prefix <相对路径前缀>] [--reproducible true] [--key <私钥文件> --key-id <标识>] <包.zip...>
//   node scripts/plugin-center.mjs keygen <私钥输出文件> [--key-id <标识>]
//   node scripts/plugin-center.mjs verify --catalog <目录文件> [--key-id <标识> --public-key <公钥>] [--packages <包所在目录>]
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from 'node:crypto';
import { chmodSync, existsSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { inflateRawSync } from 'node:zlib';

const MAX_CATALOG_BYTES = 1024 * 1024;
const CATALOG_FORMAT = 1;

function fail(message) {
  console.error(`错误: ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  const options = {};
  const positional = [];
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument.startsWith('--')) {
      const value = argv[index + 1];
      if (value === undefined || value.startsWith('--')) fail(`${argument} 缺少取值`);
      options[argument.slice(2)] = value;
      index += 1;
    } else {
      positional.push(argument);
    }
  }
  return { options, positional };
}

/** 原始 32 字节 Ed25519 公钥（SPKI DER 的最后 32 字节）。 */
function rawPublicKey(publicKey) {
  return publicKey.export({ type: 'spki', format: 'der' }).subarray(-32);
}

/** 读取 ZIP 中的一个文件（支持存储与 deflate），仅用于取 manifest.json。 */
function readZipEntry(bytes, wanted) {
  let end = -1;
  for (let index = bytes.length - 22; index >= Math.max(0, bytes.length - 65_557); index -= 1) {
    if (bytes.readUInt32LE(index) === 0x06054b50) { end = index; break; }
  }
  if (end < 0) throw new Error('不是有效的 ZIP（缺少目录结束记录）');
  const count = bytes.readUInt16LE(end + 10);
  let cursor = bytes.readUInt32LE(end + 16);
  for (let entry = 0; entry < count; entry += 1) {
    if (bytes.readUInt32LE(cursor) !== 0x02014b50) throw new Error('ZIP 中央目录损坏');
    const method = bytes.readUInt16LE(cursor + 10);
    const compressed = bytes.readUInt32LE(cursor + 20);
    const nameLength = bytes.readUInt16LE(cursor + 28);
    const extraLength = bytes.readUInt16LE(cursor + 30);
    const commentLength = bytes.readUInt16LE(cursor + 32);
    const localOffset = bytes.readUInt32LE(cursor + 42);
    const name = bytes.subarray(cursor + 46, cursor + 46 + nameLength).toString('utf8');
    if (name === wanted) {
      const dataStart = localOffset + 30 + bytes.readUInt16LE(localOffset + 26) + bytes.readUInt16LE(localOffset + 28);
      const data = bytes.subarray(dataStart, dataStart + compressed);
      if (method === 0) return data;
      if (method === 8) return inflateRawSync(data);
      throw new Error(`不支持的 ZIP 压缩方式 ${method}`);
    }
    cursor += 46 + nameLength + extraLength + commentLength;
  }
  throw new Error(`ZIP 中没有 ${wanted}`);
}

function keygen(positional, options) {
  const [output] = positional;
  if (!output) fail('用法: keygen <私钥输出文件> [--key-id <标识>]');
  if (existsSync(output)) fail(`${output} 已存在，拒绝覆盖私钥`);
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  writeFileSync(output, privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  chmodSync(output, 0o600);
  const keyId = options['key-id'] ?? `key-${new Date().toISOString().slice(0, 10)}`;
  console.log(`私钥已写入 ${output}（权限 600，请离线备份，切勿提交到仓库）`);
  console.log('请把下面两项填入 Magi「设置 → 插件 → 插件中心 → 配置」：');
  console.log(`  keyId:     ${keyId}`);
  console.log(`  publicKey: ${rawPublicKey(publicKey).toString('base64')}`);
}

function publish(positional, options) {
  const { key, 'key-id': keyId, 'base-url': baseUrl, out, prefix = '' } = options;
  // 允许 0 个包：生成空目录（官方插件中心在没有上架插件时就是空目录）。
  if (!out || Boolean(key) !== Boolean(keyId)) {
    fail('用法: publish --out <目录文件> [--base-url <包所在目录URL>] [--key <私钥文件> --key-id <标识>] <包.zip...>（--key 与 --key-id 须同时给出或同时省略）');
  }
  if (baseUrl && !/^https?:\/\//u.test(baseUrl)) fail('--base-url 必须是 http(s) 地址');
  const plugins = positional.map((file) => {
    const bytes = readFileSync(file);
    let manifest;
    try {
      manifest = JSON.parse(readZipEntry(bytes, 'manifest.json').toString('utf8'));
    } catch (error) {
      return fail(`${file}: ${error.message}`);
    }
    const name = encodeURIComponent(basename(file));
    return {
      id: manifest.id,
      name: manifest.name,
      description: manifest.description ?? '',
      version: manifest.version,
      url: baseUrl ? `${baseUrl.replace(/\/+$/u, '')}/${name}` : `${prefix}${name}`,
      sha256: createHash('sha256').update(bytes).digest('hex'),
      size: bytes.length,
    };
  });
  // --reproducible 省略发布时间，使同样的输入得到逐字节相同的目录（官方目录据此做一致性检查）。
  const catalog = options.reproducible === 'true'
    ? { format: CATALOG_FORMAT, plugins }
    : { format: CATALOG_FORMAT, issuedAt: Date.now(), plugins };
  let serialized;
  if (key) {
    const privateKey = createPrivateKey(readFileSync(key));
    const payload = Buffer.from(JSON.stringify(catalog));
    serialized = JSON.stringify({
      payload: payload.toString('base64'),
      keyId,
      signature: sign(null, payload, privateKey).toString('base64'),
    });
  } else {
    serialized = JSON.stringify(catalog, null, 2);
  }
  if (serialized.length > MAX_CATALOG_BYTES) fail('目录超过 1MiB 上限');
  writeFileSync(out, serialized);
  console.log(`已${key ? '签名' : '生成（未签名）'} ${plugins.length} 个条目 → ${out}`);
  for (const plugin of plugins) console.log(`  ${plugin.id}@${plugin.version}  ${plugin.sha256.slice(0, 12)}…  ${plugin.size}B`);
}

function verifyCatalog(options) {
  const { catalog, 'key-id': keyId, 'public-key': publicKey, packages } = options;
  if (!catalog || Boolean(keyId) !== Boolean(publicKey)) fail('用法: verify --catalog <目录文件> [--key-id <标识> --public-key <公钥>] [--packages <包所在目录>]');
  const parsed = JSON.parse(readFileSync(catalog, 'utf8'));
  let content;
  let signedNote = '未签名目录';
  if (parsed.payload !== undefined && parsed.signature !== undefined) {
    const payload = Buffer.from(parsed.payload, 'base64');
    if (keyId) {
      if (parsed.keyId !== keyId) fail(`目录由 ${parsed.keyId} 签名，不是 ${keyId}`);
      // 复原为 SPKI：Ed25519 的前缀是固定的 12 字节。
      const spki = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(publicKey, 'base64')]);
      const valid = verify(null, payload, createPublicKey({ key: spki, format: 'der', type: 'spki' }), Buffer.from(parsed.signature, 'base64'));
      if (!valid) fail('签名未通过该公钥校验');
      signedNote = '签名有效';
    } else {
      signedNote = '签名信封（未提供公钥，未验签）';
    }
    content = JSON.parse(payload.toString('utf8'));
  } else {
    if (keyId) fail('给了公钥，但该目录没有签名');
    content = parsed;
  }
  if (content.format !== CATALOG_FORMAT) fail(`不支持的目录格式 ${content.format}`);
  let problems = 0;
  for (const plugin of content.plugins) {
    let status = signedNote;
    if (packages) {
      const file = join(resolve(packages), decodeURIComponent(plugin.url.split('/').pop()));
      if (!existsSync(file)) { status = '缺少本地包'; problems += 1; }
      else if (statSync(file).size !== plugin.size || createHash('sha256').update(readFileSync(file)).digest('hex') !== plugin.sha256) {
        status = '本地包与目录摘要不一致'; problems += 1;
      } else status = `${signedNote}，本地包摘要一致`;
    }
    console.log(`  ${plugin.id}@${plugin.version}: ${status}`);
  }
  if (problems > 0) process.exit(1);
  console.log(`目录有效（${content.plugins.length} 个条目）`);
}

const [command, ...rest] = process.argv.slice(2);
const { options, positional } = parseArguments(rest);
switch (command) {
  case 'keygen': keygen(positional, options); break;
  case 'publish': publish(positional, options); break;
  case 'verify': verifyCatalog(options); break;
  default: fail('用法: node scripts/plugin-center.mjs <keygen|publish|verify> ...');
}
