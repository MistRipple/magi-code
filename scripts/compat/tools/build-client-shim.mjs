// 预先构建 DSH 页面运行时（React + 垫片）为 dist/dsh-client-host.js。随 Magi 发行，不在用户机器上构建。
import { build } from 'esbuild';
import { mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// 发行构建直接使用暂存目录已按锁文件安装的依赖，不要求源码目录另做 npm install。
const compat = process.argv[2] ? resolve(process.argv[2]) : join(dirname(fileURLToPath(import.meta.url)), '..');
mkdirSync(join(compat, 'dist'), { recursive: true });
await build({
  entryPoints: [join(compat, 'runtime/client/entry.mjs')], outfile: join(compat, 'dist/dsh-client-host.js'),
  bundle: true, platform: 'browser', format: 'iife', target: 'es2022', minify: true, legalComments: 'none', logLevel: 'silent',
  nodePaths: [join(compat, 'node_modules')], define: { 'process.env.NODE_ENV': '"production"' },
});
console.log('dist/dsh-client-host.js 已构建');
