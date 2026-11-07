// DSH 兼容准入分析。这里只读原包，不改写业务、页面或生成静态贡献。
import { existsSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { DSH_FLOW } from '../core/scan.mjs';
import { buildReport } from '../core/inspect.mjs';
import { classify } from './catalog.mjs';
import { backendEntry, readPackage } from '../core/package.mjs';

/** 原生模块：`.node`/`.wasm` 文件，或随包 `native/` 目录里有构建产物。 */
function findNative(dir) {
  const found = [];
  const walk = (directory, depth) => {
    let entries;
    try { entries = readdirSync(directory, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      if (entry.name === 'node_modules' || entry.name === '.git') continue;
      const full = join(directory, entry.name);
      if (entry.isDirectory()) { if (depth < 4) walk(full, depth + 1); }
      else if (/\.(node|wasm)$/.test(entry.name)) found.push(full.slice(dir.length + 1));
    }
  };
  if (existsSync(join(dir, 'native'))) walk(join(dir, 'native'), 0);
  return found;
}

export function createDshTranslator(options = {}) {
  return {
    kind: 'dsh',

    async detect(dir) {
      let pkg;
      try { pkg = readPackage(dir); } catch { return 0; }
      const dependencies = { ...pkg.dependencies, ...pkg.peerDependencies, ...pkg.devDependencies };
      let score = 0;
      if (pkg.dsh && typeof pkg.dsh === 'object') score += 5;
      if (Object.keys(dependencies).some((name) => name.startsWith('@deepseek-ai/dsh-'))) score += 3;
      if (dependencies['@deepseek-ai/cordis'] || dependencies.cordis) score += 1;
      if (existsSync(join(dir, 'cordis.patch.yml'))) score += 1;
      return score;
    },

    async inspect(dir, inspectOptions = {}) {
      const pkg = readPackage(dir);
      const required = inspectOptions.required ?? options.required ?? ['llm'];
      // 已发布的 npm 产物没有 src/：默认从 package.json 的 main 出发沿相对导入扫描；显式给出 directories 时按源码目录扫描。
      const mainFile = join(dir, backendEntry(pkg));
      const scanOptions = inspectOptions.directories || !existsSync(mainFile)
        ? { directories: inspectOptions.directories ?? ['src'], flow: DSH_FLOW }
        : { entry: mainFile, flow: DSH_FLOW };
      const native = findNative(dir).map((file) => ({
        kind: 'native', name: file, category: 'native', status: 'dropped', target: '无',
        note: 'Magi 扩展不支持包内原生模块或 WASM', count: 1, files: [file],
      }));
      return buildReport({
        translator: 'dsh', upstream: { name: pkg.name, version: pkg.version }, root: dir,
        scanOptions, classify, required, extraEntries: native,
      });
    },
  };
}
