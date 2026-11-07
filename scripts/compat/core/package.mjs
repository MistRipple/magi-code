import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export const readPackage = directory => JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));

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
