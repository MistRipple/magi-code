import * as esbuild from 'esbuild';
import path from 'path';

const production = process.argv.includes('--production');
const vscodeShimPath = path.resolve('src/agent/shims/vscode.ts');

const result = await esbuild.build({
  entryPoints: ['src/agent/main.ts'],
  bundle: true,
  outfile: 'dist/agent.js',
  format: 'cjs',
  platform: 'node',
  target: 'node18',
  sourcemap: production ? false : true,
  minify: production,
  treeShaking: true,
  metafile: true,
  define: {
    'process.env.NODE_ENV': production ? '"production"' : '"development"',
  },
  plugins: [
    {
      name: 'agent-vscode-shim',
      setup(build) {
        build.onResolve({ filter: /^vscode$/ }, () => ({ path: vscodeShimPath }));
      },
    },
  ],
  logLevel: 'warning',
});

const fmt = (bytes) => bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)}kb` : `${(bytes / 1024 / 1024).toFixed(1)}mb`;
for (const [file, info] of Object.entries(result.metafile.outputs)) {
  console.log(`  ${file}  ${fmt(info.bytes)}`);
}
