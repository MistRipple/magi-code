// 预先构建 DSH 页面运行时（React + 垫片）为 dist/dsh-client-host.js。随 Magi 发行，不在用户机器上构建。
import { build } from 'esbuild';
import { mkdirSync, existsSync, statSync, writeFileSync, rmSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { BROWSER_LIBRARIES, SHARED_BROWSER_MODULES } from '../runtime/client/modules.mjs';
import { writeTranslationRuntime } from '../core/translation-runtime.mjs';
import { dshThemeAliases } from '../runtime/client/theme.mjs';

// 发行构建直接使用暂存目录已按锁文件安装的依赖，不要求源码目录另做 npm install。
const compat = process.argv[2] ? resolve(process.argv[2]) : join(dirname(fileURLToPath(import.meta.url)), '..');
const output = join(compat, 'dist');
rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });
const require = createRequire(join(compat, 'package.json'));
// 依赖更新时核对真实 SDK 的公共语义引用；遗漏颜色会使原包静默使用
// 默认值甚至让整条 CSS 声明失效，不能等某个插件页面才偶然发现。
const aliases = new Set(Object.keys(dshThemeAliases({})));
for (const library of BROWSER_LIBRARIES.filter(item => item.name.startsWith('@deepseek-ai/dsh-client-ui-'))) {
  const missing = new Set();
  const inspect = directory => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory() && entry.name !== 'node_modules') inspect(path);
      else if (entry.isFile() && /\.(css|js)$/.test(entry.name)) {
        for (const match of readFileSync(path, 'utf8').matchAll(/var\(\s*(--dsw-alias-[\w-]+)/g)) {
          if (!aliases.has(match[1])) missing.add(match[1]);
        }
      }
    }
  };
  inspect(dirname(require.resolve(library.name)));
  if (missing.size) throw new Error(`公共主题未覆盖 ${library.name} 的语义：${[...missing].sort().join(', ')}`);
}
const configuration = {
  bundle: true, platform: 'browser', format: 'iife', target: 'es2022', minify: true, legalComments: 'none', logLevel: 'silent',
  nodePaths: [join(compat, 'node_modules')], define: { 'process.env.NODE_ENV': '"production"' },
  loader: { '.woff2': 'dataurl', '.woff': 'dataurl', '.ttf': 'dataurl', '.module.css': 'local-css' },
};
await build({ ...configuration, entryPoints: [join(compat, 'runtime/client/entry.mjs')], outfile: join(output, 'dsh-client-host.js') });
const exported = new Map();
for (const name of new Set([...SHARED_BROWSER_MODULES, ...BROWSER_LIBRARIES.flatMap((item) => item.dependencies)])) {
  exported.set(name, Object.keys(await import(pathToFileURL(require.resolve(name)).href)));
}
const manifest = {};
for (const library of BROWSER_LIBRARIES) {
  const external = new Set([...SHARED_BROWSER_MODULES, ...library.dependencies]);
  await build({
    ...configuration,
    stdin: { contents: `import * as value from ${JSON.stringify(library.name)}; globalThis.__MAGI_DSH_MODULES__[${JSON.stringify(library.name)}] = value;`, resolveDir: compat },
    outfile: join(output, library.file),
    plugins: [{ name: 'shared-browser-modules', setup(builder) {
      builder.onResolve({ filter: /.*/ }, (args) => external.has(args.path) ? { path: args.path, namespace: 'shared-browser' } : undefined);
      builder.onLoad({ filter: /.*/, namespace: 'shared-browser' }, ({ path }) => ({ contents: exported.get(path).filter((name) => name !== 'module.exports').map((name) => name === 'default'
        ? `export default globalThis.__MAGI_DSH_MODULES__[${JSON.stringify(path)}].default ?? globalThis.__MAGI_DSH_MODULES__[${JSON.stringify(path)}];`
        : `export const ${name} = globalThis.__MAGI_DSH_MODULES__[${JSON.stringify(path)}][${JSON.stringify(name)}];`).join('\n') }));
    } }],
  });
  const css = library.file.replace(/\.js$/, '.css');
  const files = [library.file, ...(existsSync(join(output, css)) ? [css] : [])];
  manifest[library.name] = files;
  for (const file of files) if (statSync(join(output, file)).size > 4 * 1024 * 1024) throw new Error(`浏览器模块 ${file} 超过插件单文件上限，必须拆分运行库`);
}
writeFileSync(join(output, 'browser-modules.json'), JSON.stringify(manifest, null, 2));
writeTranslationRuntime(compat);
console.log('DSH 浏览器宿主与分模块运行库已构建');
