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

await esbuild.build({
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
  // WASM 文件作为文件资源处理
  loader: {
    '.wasm': 'file',
  },
  // 确保 __dirname 在 bundle 中正确工作
  define: {
    'process.env.NODE_ENV': production ? '"production"' : '"development"',
  },
  logLevel: 'info',
});
