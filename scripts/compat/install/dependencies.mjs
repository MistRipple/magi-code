// 原包依赖安装：由随产品发行的 pacote 解包 npm 发布物，绝不运行 lifecycle 脚本。
import { readFileSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import semver from 'semver';
import pacote from 'pacote';
import { hostDependencies } from '../core/host-dependencies.mjs';
import { registryOptions } from '../core/registry-options.mjs';

const NAME = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/;
const platformMatches = (values, value) => !values || (Array.isArray(values)
  && !values.includes(`!${value}`) && (values.includes('any') || !values.some((item) => !item.startsWith('!')) || values.includes(value)));

// 隔离安装的宿主运行库：始终落在使用它的原包自己的 node_modules 下，不提升，
// 共享解析钩子按“止于当前原包目录”的规则查找它们。
const ISOLATED = new Set(['@deepseek-ai/cordis', '@deepseek-ai/schemastery']);

export async function installDependencies(root, sharedRoot, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org', compatibility) {
  const records = [];
  const shared = hostDependencies(sharedRoot);
  // 已落盘的依赖：安装目录 → 版本。Node 从使用者所在目录逐层向上查 node_modules：
  // 最近的同名包满足范围就复用；没有同名包时装到根目录（尽可能共享）；最近的同名包版本冲突时
  // 嵌套装在使用者自己的 node_modules 下遮蔽它。这样同一包不会被各分支重复安装。
  const placed = new Map();
  const locate = (directory, name) => join(directory, 'node_modules', ...name.split('/'));
  // chain：使用者目录及其向上的安装目录（Node 的查找顺序），止于 root。
  const work = async (directory, chain, ancestors = new Map(), depth = 0) => {
    if (depth > 16 || records.length > 128) throw new Error('插件依赖图超过 16 层或 128 个包');
    const pkg = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
    if (pkg.engines?.node && !semver.satisfies(process.versions.node, pkg.engines.node)) throw new Error(`${pkg.name} 要求 Node ${pkg.engines.node}`);
    // 先放置本包的全部直接依赖，再逐个进入它们的依赖：直接依赖优先占用靠上的位置，
    // 更深处的包才在冲突时嵌套，不会抢走使用者自己声明的版本。
    const descend = [];
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
        // Cordis peer 与原包 Schema 库可从 registry 安装满足完整范围的精确版本；
        // minVersion 不是 registry 实际版本，不能据此猜测是否可以隔离。
        // Magi 自己的上下文仍使用随包实例，原包注册还须通过真实启动检查。
        if ((name !== '@deepseek-ai/cordis' || !peer) && name !== '@deepseek-ai/schemastery') throw new Error(`宿主依赖 ${name}@${installed.version} 不满足 ${range}`);
      }
      if (ancestors.has(name) && semver.satisfies(ancestors.get(name), range)) continue;
      // 最近的同名包决定 Node 实际解析到谁：它满足范围即复用；不满足则必须在使用者目录嵌套安装来遮蔽它。
      const nearest = chain.map((candidate) => placed.get(locate(candidate, name))).find((version) => version !== undefined);
      if (nearest !== undefined && semver.satisfies(nearest, range)) continue;
      // 可选依赖在当前平台确实不适用时才跳过；不以下载失败当作不可用。
      const manifest = await pacote.manifest(`${name}@${range}`, registryOptions(registry));
      if (!platformMatches(manifest.os, process.platform) || !platformMatches(manifest.cpu, process.arch)) {
        if (!Object.hasOwn(pkg.optionalDependencies ?? {}, name)) throw new Error(`依赖 ${name}@${manifest.version} 不支持当前平台 ${process.platform}/${process.arch}`);
        records.push({ name, version: manifest.version, optional: true, skipped: 'platform' });
        continue;
      }
      if (!manifest.dist?.integrity?.startsWith('sha512-')) throw new Error(`依赖 ${name} 缺少 sha512 完整性摘要`);
      const isolated = ISOLATED.has(name);
      // 没有任何同名包时装到根目录供所有分支共享；隔离库和版本冲突的包嵌套在使用者下。
      const hoist = nearest === undefined && !isolated;
      const parentChain = hoist ? [root] : chain;
      const destination = locate(hoist ? root : directory, name);
      mkdirSync(destination, { recursive: true });
      await pacote.extract(manifest.dist.tarball, destination, { ...registryOptions(registry), integrity: manifest.dist.integrity });
      const saved = JSON.parse(readFileSync(join(destination, 'package.json'), 'utf8'));
      if (saved.name !== name || saved.version !== manifest.version) throw new Error(`依赖 ${name} 的包身份与 registry 不符`);
      placed.set(destination, saved.version);
      records.push({ name, version: saved.version, integrity: manifest.dist.integrity, host: false, isolated });
      descend.push([destination, [destination, ...parentChain], new Map([...ancestors, [name, saved.version]])]);
    }
    for (const [destination, nextChain, nextAncestors] of descend) await work(destination, nextChain, nextAncestors, depth + 1);
  };
  await work(root, [root]);
  return records;
}
