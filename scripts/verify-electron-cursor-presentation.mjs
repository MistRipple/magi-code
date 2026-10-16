import { spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createRequire } from 'node:module';
import { build, transform } from 'esbuild';
import { compile, compileModule } from 'svelte/compiler';

const work = await mkdtemp(join(tmpdir(), 'magi-cursor-presentation-'));
try {
  await build({
    stdin: { contents: 'import { mount } from "svelte"; import Test from "./scripts/fixtures/cursor-presentation.svelte"; mount(Test,{target:document.getElementById("overlay")});', resolveDir: process.cwd() },
    bundle: true, platform: 'browser', format: 'iife', conditions: ['browser'], outfile: join(work, 'renderer.js'),
    plugins: [{ name: 'svelte', setup(b) {
      b.onLoad({ filter: /\.svelte$/ }, async args => ({ contents: compile(await readFile(args.path, 'utf8'), { filename: args.path, generate: 'client', css: 'injected' }).js.code, resolveDir: resolve(args.path, '..') }));
      b.onLoad({ filter: /\.svelte\.ts$/ }, async args => {
        const js = await transform(await readFile(args.path, 'utf8'), { loader: 'ts' });
        return { contents: compileModule(js.code, { filename: args.path, generate: 'client' }).js.code, resolveDir: resolve(args.path, '..') };
      });
    } }], logLevel: 'silent',
  });
  await writeFile(join(work, 'preload.cjs'), `const { contextBridge, ipcRenderer }=require('electron'); contextBridge.exposeInMainWorld('cursorTest',{subscribe:cb=>ipcRenderer.on('cursor-event',(_,e)=>cb(e)),complete:r=>ipcRenderer.invoke('magi-desktop:complete-browser-agent-cursor',r)});`);
  await build({ entryPoints: ['scripts/electron-cursor-presentation.ts'], bundle: true, platform: 'node', format: 'cjs', external: ['electron', 'lighthouse'], outfile: join(work, 'main.cjs'), logLevel: 'silent' });
  const child = spawn(createRequire(import.meta.url)('electron'), [join(work, 'main.cjs')], { env: { ...process.env, MAGI_CURSOR_TEST_ROOT: work }, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', value => { output += value; if (process.env.MAGI_CURSOR_TEST_VERBOSE) process.stdout.write(value); });
  child.stderr.on('data', value => { output += value; if (process.env.MAGI_CURSOR_TEST_VERBOSE) process.stderr.write(value); });
  const timeout = setTimeout(() => child.kill('SIGKILL'), 90_000);
  const code = await new Promise((resolve, reject) => { child.once('exit', resolve); child.once('error', reject); });
  clearTimeout(timeout);
  if (code !== 0 || !output.includes('PASS cursor presentation acceptance complete')) { process.stderr.write(output); process.exitCode = 1; }
  else if (!process.env.MAGI_CURSOR_TEST_VERBOSE) console.log(output.split('\n').filter(line => line.startsWith('PASS')).join('\n'));
} finally { await rm(work, { recursive: true, force: true }); }
