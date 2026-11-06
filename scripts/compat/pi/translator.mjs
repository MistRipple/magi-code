// pi → Magi 翻译器：识别与静态兼容分析。
//
// 状态：运行时与构建尚未实现（PI_IMPLEMENTED 为空）。因此任何需要翻译的能力都不会出现 mapped，
// 检查结果只用于回答“这个 pi 扩展里有多少能被承载、多少不能、为什么”，并阻塞构建。
// 入口发现依据 pi 的包约定（docs/packages.md 行 40–100，2026-11-05 读取）：
//   package.json 的 `pi.extensions`（数组，路径相对包根；支持 glob 与排除）→ 否则约定目录 `extensions/`。
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { buildReport } from '../core/inspect.mjs';
import { PI_FLOW } from '../core/scan.mjs';
import { classify } from './catalog.mjs';

const EXT = /\.(ts|tsx|js|mjs|cjs)$/;

function readPackage(dir) {
  const path = join(dir, 'package.json');
  if (!existsSync(path)) throw new Error(`缺少 package.json: ${dir}`);
  return JSON.parse(readFileSync(path, 'utf8'));
}

/** 极简 glob：只支持 `*`、`**` 与字面量，且拒绝 `!` 排除、`{}` 与 `?`——遇到就明确失败，而不是错配。 */
function globToRegExp(pattern) {
  if (/[!{}?[\]()]/.test(pattern)) throw new Error(`pi.extensions 使用了翻译器尚不支持的 glob 语法：${pattern}`);
  const escaped = pattern.replace(/^\.\//, '').replace(/[.+^$|\\]/g, '\\$&').replace(/\*\*\/?/g, '\u0000').replace(/\*/g, '[^/]*').replace(/\u0000/g, '(?:.*/)?');
  return new RegExp(`^${escaped}$`);
}

function listFiles(root, directory = root, out = []) {
  for (const name of readdirSync(directory)) {
    if (name === 'node_modules' || name === '.git') continue;
    const full = join(directory, name);
    if (statSync(full).isDirectory()) listFiles(root, full, out); else out.push(relative(root, full));
  }
  return out;
}

/** 解析 pi 扩展入口。返回绝对路径数组；找不到入口时抛错。 */
export function resolveEntries(dir, pkg) {
  const declared = pkg.pi?.extensions;
  if (declared !== undefined) {
    if (!Array.isArray(declared) || declared.some((item) => typeof item !== 'string')) throw new Error('package.json 的 pi.extensions 必须是字符串数组');
    const all = listFiles(dir);
    const files = new Set();
    for (const pattern of declared) {
      const normalized = pattern.replace(/^\.\//, '');
      // 含任何通配字符都走 glob 分支：不支持的语法在 globToRegExp 里明确失败，而不是当成字面路径后报“文件不存在”。
      if (!/[*!{}?[\]()]/.test(normalized)) {
        const full = join(dir, normalized);
        if (!existsSync(full)) throw new Error(`pi.extensions 指向的文件不存在：${pattern}`);
        if (statSync(full).isDirectory()) {
          const index = ['index.ts', 'index.js', 'index.mjs'].map((name) => join(normalized, name)).find((name) => existsSync(join(dir, name)));
          if (!index) throw new Error(`pi.extensions 目录 ${pattern} 下没有 index 入口`);
          files.add(index);
        } else files.add(normalized);
        continue;
      }
      const regex = globToRegExp(pattern);
      const matched = all.filter((file) => regex.test(file) && EXT.test(file));
      if (matched.length === 0) throw new Error(`pi.extensions 的 glob 没有匹配任何文件：${pattern}`);
      matched.forEach((file) => files.add(file));
    }
    return [...files].sort().map((file) => join(dir, file));
  }
  const conventional = join(dir, 'extensions');
  if (existsSync(conventional) && statSync(conventional).isDirectory()) {
    const entries = [];
    for (const name of readdirSync(conventional).sort()) {
      const full = join(conventional, name);
      if (statSync(full).isFile() && EXT.test(name) && !name.endsWith('.d.ts')) entries.push(full);
      else if (statSync(full).isDirectory()) {
        const index = ['index.ts', 'index.js', 'index.mjs'].map((file) => join(full, file)).find((file) => existsSync(file));
        if (index) entries.push(index);
      }
    }
    if (entries.length > 0) return entries;
  }
  throw new Error('找不到 pi 扩展入口：既没有 package.json 的 pi.extensions，也没有 extensions/ 目录里的入口文件');
}

export function createPiTranslator(options = {}) {
  return {
    kind: 'pi',

    async detect(dir) {
      let pkg;
      try { pkg = readPackage(dir); } catch { return 0; }
      let score = 0;
      if (pkg.pi && typeof pkg.pi === 'object') score += 5;
      if (Array.isArray(pkg.keywords) && pkg.keywords.includes('pi-package')) score += 3;
      const dependencies = { ...pkg.dependencies, ...pkg.peerDependencies, ...pkg.devDependencies };
      if (Object.keys(dependencies).some((name) => /^@(earendil-works|mariozechner)\/pi-(coding-agent|ai|agent-core)$/.test(name))) score += 3;
      return score;
    },

    async inspect(dir, inspectOptions = {}) {
      const pkg = readPackage(dir);
      const required = inspectOptions.required ?? options.required ?? ['tools'];
      const entries = resolveEntries(dir, pkg);
      return buildReport({
        translator: 'pi',
        upstream: { name: pkg.name, version: pkg.version },
        root: dir,
        scanOptions: { entry: entries, flow: PI_FLOW },
        classify, required, kinds: ['imports', 'symbols', 'calls', 'injects', 'events'],
      });
    },
  };
}
