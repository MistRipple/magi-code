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
  // 原生输入/下拉菜单与拖拽分别使用全新 Chromium，避免上一组焦点/系统菜单污染下一组。
  for (const group of ['input', 'pointer']) {
    const child = spawn(createRequire(import.meta.url)('electron'), [bundle], {
      env: { ...process.env, MAGI_ACTIONS_TEST_DATA: join(workdir, group), MAGI_ACTIONS_TEST_GROUP: group },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });
    // 隐藏 guest 的原生输入在 Chromium 中会逐帧结算，完整场景可能超过一分钟。
    const timer = setTimeout(() => child.kill('SIGKILL'), 180_000);
    const code = await new Promise((resolve, reject) => { child.on('exit', resolve); child.on('error', reject); });
    clearTimeout(timer);
    const completion = group === 'input' ? 'PASS disabled/obscured/readonly rejection' : 'PASS navigation invalidates prior node references';
    if (code !== 0 || !output.includes(completion)) { process.stderr.write(output); process.exitCode = 1; }
    else console.log(output.split(String.fromCharCode(10)).filter((line) => line.startsWith('PASS')).join(String.fromCharCode(10)));
  }
} finally {
  await rm(workdir, { recursive: true, force: true });
}
