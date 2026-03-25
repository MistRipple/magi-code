import * as esbuild from 'esbuild';
import { copyFileSync, mkdirSync, existsSync } from 'fs';
import { join, dirname } from 'path';

const production = process.argv.includes('--production');

// tiktoken 使用 WASM，需要复制到输出目录
const tiktokenWasm = join('node_modules', 'tiktoken', 'tiktoken_bg.wasm');
const outWasm = join('dist', 'tiktoken_bg.wasm');
if (existsSync(tiktokenWasm)) {
  mkdirSync(dirname(outWasm), { recursive: true });
  copyFileSync(tiktokenWasm, outWasm);
}

const result = await esbuild.build({
  entryPoints: ['src/extension.ts'],
  bundle: true,
  outfile: 'dist/extension.js',
  external: [
    'vscode',        // VSCode API，运行时由宿主提供
  ],
  format: 'cjs',
  platform: 'node',
  target: 'node18',
  sourcemap: production ? false : true,
  minify: production,
  treeShaking: true,
  metafile: true,
  loader: {
    '.wasm': 'file',
  },
  define: {
    'process.env.NODE_ENV': production ? '"production"' : '"development"',
  },
  logLevel: 'warning',
});

const fmt = (bytes) => bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)}kb` : `${(bytes / 1024 / 1024).toFixed(1)}mb`;
for (const [file, info] of Object.entries(result.metafile.outputs)) {
  console.log(`  ${file}  ${fmt(info.bytes)}`);
}
