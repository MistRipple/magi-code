import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { build } from 'esbuild';

const workdir = await mkdtemp(join(tmpdir(), 'magi-browser-actions-'));
try {
  const bundle = join(workdir, 'acceptance.cjs');
  await build({
    entryPoints: ['scripts/electron-browser-actions.ts'],
    bundle: true, platform: 'node', format: 'cjs',
    external: ['electron', 'lighthouse'], outfile: bundle, logLevel: 'silent',
  });
  const child = spawn(createRequire(import.meta.url)('electron'), [bundle], {
    env: { ...process.env, MAGI_ACTIONS_TEST_DATA: join(workdir, 'data') },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let output = '';
  child.stdout.on('data', (chunk) => { output += chunk; });
  child.stderr.on('data', (chunk) => { output += chunk; });
  const timer = setTimeout(() => child.kill('SIGKILL'), 60_000);
  const code = await new Promise((resolve, reject) => { child.on('exit', resolve); child.on('error', reject); });
  clearTimeout(timer);
  if (code !== 0) { process.stderr.write(output); process.exitCode = 1; }
  else console.log(output.split(String.fromCharCode(10)).filter((line) => line.startsWith('PASS')).join(String.fromCharCode(10)));
} finally {
  await rm(workdir, { recursive: true, force: true });
}
