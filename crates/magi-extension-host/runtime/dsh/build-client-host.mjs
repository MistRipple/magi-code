import { build } from 'esbuild';
import { resolve } from 'node:path';
import { mkdir as mkdirFs } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('.', import.meta.url));
const outdir = process.argv[2] ? resolve(process.argv[2]) : resolve(root, 'dist');

await mkdirFs(outdir, { recursive: true });

await build({
  absWorkingDir: root,
  entryPoints: {
    'dsh-client-host': 'client-host/index.mjs',
  },
  outdir,
  bundle: true,
  platform: 'browser',
  target: ['es2022'],
  format: 'iife',
  jsx: 'automatic',
  minify: true,
  legalComments: 'none',
  define: {
    'process.env.NODE_ENV': '"production"',
  },
  loader: {
    '.module.css': 'local-css',
    '.css': 'css',
    '.woff': 'dataurl',
    '.woff2': 'dataurl',
    '.ttf': 'dataurl',
  },
});
