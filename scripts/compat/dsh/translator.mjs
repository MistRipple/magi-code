// DSH → Magi 翻译器。当前阶段：识别与静态兼容分析；构建与产物生成在后续阶段加入。
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { scanSources } from '../core/scan.mjs';
import { summarize, verdictFor } from '../core/report.mjs';
import { classify } from './catalog.mjs';

function readPackage(dir) {
  const path = join(dir, 'package.json');
  if (!existsSync(path)) throw new Error(`缺少 package.json: ${dir}`);
  return JSON.parse(readFileSync(path, 'utf8'));
}

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
      const scan = scanSources(dir, { directories: inspectOptions.directories ?? ['src'] });
      const entries = [];
      for (const kind of ['imports', 'calls', 'injects']) {
        for (const [name, usage] of Object.entries(scan[kind])) {
          const rule = classify(kind, name);
          entries.push({ kind, name, category: rule.category, status: rule.status, target: rule.target, note: rule.note, count: usage.count, files: usage.files });
        }
      }
      const native = findNative(dir);
      for (const file of native) {
        entries.push({ kind: 'native', name: file, category: 'native', status: 'dropped', target: '无', note: 'Magi 扩展不支持包内原生模块或 WASM', count: 1, files: [file] });
      }
      return {
        translator: 'dsh',
        upstream: { name: pkg.name, version: pkg.version },
        scanned: { files: scan.fileCount, lines: scan.lineCount },
        entries,
        summary: summarize(entries),
        verdict: verdictFor(entries, required),
      };
    },
  };
}
