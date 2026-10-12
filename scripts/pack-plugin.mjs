#!/usr/bin/env node
// 把插件目录打成 Magi 发布包（ZIP）。无第三方依赖，输出可复现（固定时间戳、按路径排序）。
//
// 用法：node scripts/pack-plugin.mjs <插件目录> [输出文件]
// 目录须包含 manifest.json 与 plugin.mjs，视图资源放在 ui/ 下。
// 这里只做打包；包的结构、大小和内容校验由 Magi 安装时统一执行，不在脚本里重复一套规则。
import { deflateRawSync, crc32 } from 'node:zlib';
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';

const [directory, output] = process.argv.slice(2);
if (!directory) {
  console.error('用法: node scripts/pack-plugin.mjs <插件目录> [输出文件]');
  process.exit(2);
}
const root = resolve(directory);

function collect(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = join(dir, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`不允许符号链接: ${full}`);
    return entry.isDirectory() ? collect(full) : [full];
  });
}

const files = collect(root)
  .map((full) => relative(root, full).split(sep).join('/'))
  .sort();
for (const required of ['manifest.json', 'plugin.mjs']) {
  if (!files.includes(required)) throw new Error(`缺少 ${required}`);
}

// ZIP 的 DOS 时间戳不能早于 1980-01-01；固定为该值使同一内容得到同一摘要。
const DOS_TIME = 0;
const DOS_DATE = (0 << 9) | (1 << 5) | 1;
const chunks = [];
const central = [];
let offset = 0;
for (const name of files) {
  const data = readFileSync(join(root, name));
  const packed = deflateRawSync(data, { level: 9 });
  const nameBytes = Buffer.from(name, 'utf8');
  const crc = crc32(data);
  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50, 0);
  local.writeUInt16LE(20, 4);
  local.writeUInt16LE(0x0800, 6); // UTF-8 文件名
  local.writeUInt16LE(8, 8);
  local.writeUInt16LE(DOS_TIME, 10);
  local.writeUInt16LE(DOS_DATE, 12);
  local.writeUInt32LE(crc, 14);
  local.writeUInt32LE(packed.length, 18);
  local.writeUInt32LE(data.length, 22);
  local.writeUInt16LE(nameBytes.length, 26);
  chunks.push(local, nameBytes, packed);
  const entry = Buffer.alloc(46);
  entry.writeUInt32LE(0x02014b50, 0);
  entry.writeUInt16LE(20, 4);
  entry.writeUInt16LE(20, 6);
  entry.writeUInt16LE(0x0800, 8);
  entry.writeUInt16LE(8, 10);
  entry.writeUInt16LE(DOS_TIME, 12);
  entry.writeUInt16LE(DOS_DATE, 14);
  entry.writeUInt32LE(crc, 16);
  entry.writeUInt32LE(packed.length, 20);
  entry.writeUInt32LE(data.length, 24);
  entry.writeUInt16LE(nameBytes.length, 28);
  entry.writeUInt32LE(offset, 42);
  central.push(entry, nameBytes);
  offset += local.length + nameBytes.length + packed.length;
}
const centralSize = central.reduce((total, part) => total + part.length, 0);
const end = Buffer.alloc(22);
end.writeUInt32LE(0x06054b50, 0);
end.writeUInt16LE(files.length, 8);
end.writeUInt16LE(files.length, 10);
end.writeUInt32LE(centralSize, 12);
end.writeUInt32LE(offset, 16);

const manifest = JSON.parse(readFileSync(join(root, 'manifest.json'), 'utf8'));
const defaultName = `${manifest.id}-${manifest.version}.zip`;
let target = resolve(output ?? defaultName);
// 传入已存在的目录时，把默认文件名放进该目录。
if (statSync(target, { throwIfNoEntry: false })?.isDirectory()) target = join(target, defaultName);
writeFileSync(target, Buffer.concat([...chunks, ...central, end]));
console.log(`${target} (${statSync(target).size} 字节, ${files.length} 个文件)`);
