// DSH app-boot 的版本门禁语义。这里只判断上游的版本声明；API 准入、
// 原包身份、运行期启动与 Magi 授权仍独立检查，版本豁免不能跳过它们。
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import semver from 'semver';

export function dshEnvironment(sharedRoot) {
  const manifest = JSON.parse(readFileSync(join(sharedRoot, 'node_modules/@deepseek-ai/dsh-tools/package.json'), 'utf8'));
  if (!semver.valid(manifest.version)) throw new Error('DSH SDK 版本无效');
  return { runtimeVersion: manifest.version, runtimeId: createHash('sha256').update(readFileSync(join(sharedRoot, 'package-lock.json'))).digest('hex').slice(0, 24) };
}

export function unmetDshRequirements(pkg, runtimeVersion) {
  if (!semver.valid(runtimeVersion)) throw new Error('DSH SDK 必须有精确语义版本');
  const accepts = (range) => typeof range === 'string' && range.trim() && semver.validRange(range) && semver.satisfies(runtimeVersion, range, { includePrerelease: true });
  if (semver.gte(runtimeVersion, '0.2.1-alpha.4') && Object.hasOwn(pkg, 'engines')) {
    if (!pkg.engines || typeof pkg.engines !== 'object' || Array.isArray(pkg.engines)) return { engines: '(not an object)' };
    if (Object.hasOwn(pkg.engines, 'dsh')) return accepts(pkg.engines.dsh) ? null : { 'engines.dsh': pkg.engines.dsh };
  }
  const unmet = {};
  for (const [name, range] of Object.entries(pkg.peerDependencies ?? {})) {
    if (name !== '@deepseek-ai/dsh' && !name.startsWith('@deepseek-ai/dsh-')) continue;
    if (['workspace:^', 'workspace:~', 'workspace:*'].includes(range)) continue;
    if (!accepts(range)) unmet[name] = range;
  }
  return Object.keys(unmet).length ? unmet : null;
}

export function checkDshCompatibility(pkg, environment, policy) {
  if (policy && (policy.runtimeVersion !== environment.runtimeVersion || policy.runtimeId !== environment.runtimeId)) throw new Error('daemon 的兼容策略与当前运行时不一致，请重新预览');
  const requirements = unmetDshRequirements(pkg, environment.runtimeVersion);
  const exempted = Boolean(requirements && policy?.exemptions?.[`${pkg.name}@${pkg.version}`]?.includes(environment.runtimeVersion));
  if (requirements && !exempted) {
    const error = new Error(`${pkg.name}@${pkg.version} 的 DSH 版本声明不接受 ${environment.runtimeVersion}：${JSON.stringify(requirements)}；需要此精确版本的兼容豁免`);
    error.code = 'incompatible-version';
    error.incompatible = [{ name: pkg.name, version: pkg.version, runtimeVersion: environment.runtimeVersion, requirements }];
    throw error;
  }
  return { ...environment, requirements, exempted, useRuntimePeers: exempted || Object.hasOwn(pkg.engines ?? {}, 'dsh') };
}
