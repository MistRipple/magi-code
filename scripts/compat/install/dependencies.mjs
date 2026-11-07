// 原包依赖安装：由随产品发行的 pacote 解包 npm 发布物，绝不运行 lifecycle 脚本。
import { readFileSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import semver from 'semver';
import pacote from 'pacote';
import { hostDependencies } from '../core/host-dependencies.mjs';

const NAME = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/;
const platformMatches = (values, value) => !values || (Array.isArray(values)
  && !values.includes(`!${value}`) && (values.includes('any') || !values.some((item) => !item.startsWith('!')) || values.includes(value)));

export async function installDependencies(root, sharedRoot, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org', compatibility) {
  const records = [];
  const shared = hostDependencies(sharedRoot);
  const work = async (directory, ancestors = new Map(), depth = 0) => {
    if (depth > 16 || records.length > 128) throw new Error('插件依赖图超过 16 层或 128 个包');
    const pkg = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
    if (pkg.engines?.node && !semver.satisfies(process.versions.node, pkg.engines.node)) throw new Error(`${pkg.name} 要求 Node ${pkg.engines.node}`);
    for (const [name, range] of Object.entries({ ...pkg.peerDependencies, ...pkg.dependencies, ...pkg.optionalDependencies }).sort(([a], [b]) => a.localeCompare(b, 'en'))) {
      if (!NAME.test(name) || !semver.validRange(range)) throw new Error(`依赖 ${name}@${range} 不是可验证的 npm 语义版本范围`);
      const peer = !Object.hasOwn(pkg.dependencies ?? {}, name) && !Object.hasOwn(pkg.optionalDependencies ?? {}, name);
      const optionalPeer = peer && pkg.peerDependenciesMeta?.[name]?.optional === true;
      if (optionalPeer && !shared.has(name) && !ancestors.has(name)) {
        records.push({ name, optional: true, skipped: 'optional-peer' });
        continue;
      }
      if (shared.has(name)) {
        const manifest = join(sharedRoot, 'node_modules', ...name.split('/'), 'package.json');
        if (!existsSync(manifest)) throw new Error(`缺少宿主依赖 ${name}`);
        const installed = JSON.parse(readFileSync(manifest, 'utf8'));
        // DSH app-boot 0.2.2-rc.1 的 plugin-compatibility 明确让预发布版本参与 DSH peer 范围。
        // 这不是 npm 普通依赖的默认规则；仅 DSH peer 使用上游策略，直接依赖与其他生态仍严格校验。
        const dshPeer = name.startsWith('@deepseek-ai/dsh-') && peer;
        if (semver.satisfies(installed.version, range, { includePrerelease: dshPeer }) || (depth === 0 && dshPeer && compatibility?.useRuntimePeers)) {
          records.push({ name, version: installed.version, host: true, ...(depth === 0 && dshPeer && compatibility?.exempted ? { versionExemption: true } : {}) });
          continue;
        }
        // Cordis peer 可以随译包从 registry 安装满足完整范围的精确版本；
        // minVersion 不是 registry 实际版本，不能据此猜测是否可以隔离。
        // Magi 自己的上下文仍使用随包实例，原包注册还须通过真实启动检查。
        if (name !== '@deepseek-ai/cordis' || !peer) throw new Error(`宿主依赖 ${name}@${installed.version} 不满足 ${range}`);
      }
      if (ancestors.has(name) && semver.satisfies(ancestors.get(name), range)) continue;
      // 可选依赖在当前平台确实不适用时才跳过；不以下载失败当作不可用。
      const manifest = await pacote.manifest(`${name}@${range}`, { registry, fullMetadata: true });
      if (!platformMatches(manifest.os, process.platform) || !platformMatches(manifest.cpu, process.arch)) {
        if (!Object.hasOwn(pkg.optionalDependencies ?? {}, name)) throw new Error(`依赖 ${name}@${manifest.version} 不支持当前平台 ${process.platform}/${process.arch}`);
        records.push({ name, version: manifest.version, optional: true, skipped: 'platform' });
        continue;
      }
      if (!manifest.dist?.integrity?.startsWith('sha512-')) throw new Error(`依赖 ${name} 缺少 sha512 完整性摘要`);
      const destination = join(directory, 'node_modules', ...name.split('/'));
      mkdirSync(destination, { recursive: true });
      await pacote.extract(manifest.dist.tarball, destination, { registry, integrity: manifest.dist.integrity, ignoreScripts: true });
      const saved = JSON.parse(readFileSync(join(destination, 'package.json'), 'utf8'));
      if (saved.name !== name || saved.version !== manifest.version) throw new Error(`依赖 ${name} 的包身份与 registry 不符`);
      records.push({ name, version: saved.version, integrity: manifest.dist.integrity, host: false, isolated: name === '@deepseek-ai/cordis' });
      await work(destination, new Map([...ancestors, [name, saved.version]]), depth + 1);
    }
  };
  await work(root);
  return records;
}
