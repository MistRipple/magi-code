import { existsSync, readFileSync } from 'node:fs';
import { join, resolve, sep } from 'node:path';
import { load } from 'js-yaml';
import { validSettingsNamespace } from '../runtime/registered-settings.mjs';

export const readPackage = directory => JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));

// 设置以 bundle 的实际根入口 ID 寻址；npm 名称只是没有入口声明时的默认值。
// 只读取本包根入口身份，不执行修改其他插件的 patch，也不建立另一套 profile。
export function settingsNamespace(directory, pkg) {
  const declared = pkg.dsh?.bundle?.patch ?? 'cordis.patch.yml';
  const files = Array.isArray(declared) ? declared : [declared], ids = new Set();
  const root = resolve(directory);
  if (files.length > 32) throw new Error('上游 bundle patch 声明超过限制');
  for (const file of files) {
    if (typeof file !== 'string' || !resolve(root, file).startsWith(root + sep)) throw new Error('上游 bundle patch 路径越界或无效');
    const path = resolve(root, file);
    if (!existsSync(path)) {
      if (pkg.dsh?.bundle?.patch !== undefined) throw new Error('上游声明的 bundle patch 不存在');
      continue;
    }
    const patches = load(readFileSync(path, 'utf8'));
    if (!Array.isArray(patches)) throw new Error('上游 bundle patch 必须是操作列表');
    for (const patch of patches) for (const entry of Array.isArray(patch?.insert) ? patch.insert : []) {
      if (entry?.name !== pkg.name) continue;
      if (!validSettingsNamespace(entry.id) || entry.id === 'web') throw new Error('上游根入口设置身份无效或占用宿主命名空间');
      ids.add(entry.id);
    }
  }
  if (ids.size > 1) throw new Error('上游声明多个本包根入口，当前单实例设置无法确定归属');
  return ids.values().next().value ?? pkg.name;
}

// Node ESM 的条件导出按声明顺序选择。扫描、启动检查和安装必须检查同一入口。
export function backendEntry(pkg) {
  const exported = pkg.exports;
  const select = target => {
    if (typeof target === 'string') return target;
    if (!target || typeof target !== 'object' || Array.isArray(target)) return;
    for (const [condition, value] of Object.entries(target)) {
      if (!['node', 'import', 'default'].includes(condition)) continue;
      const result = select(value);
      if (result) return result;
    }
  };
  const target = exported && typeof exported === 'object' && Object.keys(exported).some(key => key.startsWith('.')) ? exported['.'] : exported;
  const entry = exported === undefined ? pkg.main ?? 'index.js' : select(target);
  if (typeof entry !== 'string' || !entry || entry.startsWith('/') || entry.split(/[\\/]/).includes('..')) throw new Error('上游入口无效或没有 Node ESM 导出');
  return entry.replace(/^\.\//, '');
}
