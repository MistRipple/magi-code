import test from 'node:test';
import assert from 'node:assert/strict';
import nativeFs from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureProfile, withProfile, profilePath, loaderEntries } from './runtime/profile.mjs';
import fs from './runtime/profile-fs.mjs';
import { load, dump } from 'js-yaml';
import { installSharedResolution } from './runtime/shared-resolution.mjs';

test('profile 同步读取真实调用快照，启停提案经 daemon 与摘要校验提交，无物理副本和失败写入', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-test-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let enabled = true, digest = 'current';
  const calls = [];
  const off = configureProfile({ plugins: {
    async list() { return { plugins: [{ manifest: { id: 'dsh.example', description: 'real' }, digest, source: { kind: 'upstream', location: 'dsh:@scope/example@1.2.3' }, enabled, active: enabled, runtime: { state: enabled ? 'running' : 'stopped' } }] }; },
    async setEnabled(id, value, expected) { assert.equal(id, 'dsh.example'); assert.equal(expected, digest); calls.push(value); enabled = value; },
  } }, { home });
  t.after(off);
  const root = profilePath();
  assert.equal(nativeFs.existsSync(root), false);
  assert.throws(() => fs.readFileSync(join(root, 'package.json')), /调用快照/);
  await withProfile(async () => {
    const manifest = JSON.parse(fs.readFileSync(join(root, 'package.json'), 'utf8'));
    assert.equal(manifest.dependencies['@scope/example'], '1.2.3');
    assert.deepEqual(manifest.dsh.profile.bundles, ['@scope/example']);
    assert.equal(JSON.parse(await fs.promises.readFile(join(root, 'node_modules/@scope/example/package.json'), 'utf8')).name, '@scope/example');
    assert.deepEqual(fs.readdirSync(join(root, '..')), ['magi']);
    const entries = loaderEntries(); assert.equal(entries[1].options.name, '@scope/example'); assert.equal(entries[1].fiber.state, 2);
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: true }]));
    assert.equal(enabled, true, '调用结束前没有第二条状态写入路径');
  });
  assert.equal(enabled, false); assert.deepEqual(calls, [false]);
  await withProfile(async () => { assert.equal(loaderEntries()[1].disabled, true); });
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: false }]));
    throw new Error('business failed');
  }), /business failed/);
  assert.equal(enabled, false);
  await withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: false }]));
    return { status: 500 };
  });
  assert.equal(enabled, false);
  await assert.rejects(withProfile(async () => { fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'unknown', disabled: false }])); }), /当前已安装/);
  await assert.rejects(withProfile(async () => { fs.writeFileSync(join(root, 'package.json'), JSON.stringify({ dependencies: { unsafe: '*' } })); }), /不一致/);
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: false }]));
    digest = 'replaced';
  }), /版本.*变化/);
  assert.equal(nativeFs.existsSync(root), false);
});

test('Node 导入外观仅覆盖上游的 profile 访问，其他物理文件仍由原生 fs 读取', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'magi-profile-import-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const shared = new URL('.', import.meta.url).pathname;
  const hooks = installSharedResolution(shared, { '@magi/compat-upstream-root': dir + '/' });
  t.after(() => hooks.deregister());
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home: dir }); t.after(off);
  const physical = join(dir, 'real.txt'); await writeFile(physical, 'physical');
  const entry = join(dir, 'entry.mjs');
  await writeFile(entry, `import {readFileSync} from 'node:fs';import {readFile,opendir,constants} from 'node:fs/promises';export const read=()=>readFileSync(process.env.DSH_PROFILE_DIR+'/package.json','utf8');export const physical=()=>readFile(${JSON.stringify(physical)},'utf8');export async function directories(){const names=[];for await(const entry of await opendir(process.env.DSH_PROFILE_DIR))names.push(entry.name);return names;}export const accessMode=constants.R_OK;`);
  const upstream = await import(entry);
  await withProfile(async () => {
    assert.deepEqual(JSON.parse(upstream.read()).dependencies, {});
    assert.equal(await upstream.physical(), 'physical');
    assert.ok((await upstream.directories()).includes('package.json'));
    assert.equal(upstream.accessMode, nativeFs.constants.R_OK);
  });
  const cjsEntry = join(dir, 'entry.cjs');
  await writeFile(cjsEntry, `const fs=require('node:fs');const promises=require('fs/promises');module.exports={read:()=>fs.readFileSync(process.env.DSH_PROFILE_DIR+'/package.json','utf8'),scan:async()=>{const names=[];for await(const entry of await promises.opendir(process.env.DSH_PROFILE_DIR))names.push(entry.name);return names;}};`);
  const cjs = (await import(cjsEntry)).default;
  await withProfile(async () => {
    assert.deepEqual(JSON.parse(cjs.read()).dependencies, {});
    assert.ok((await cjs.scan()).includes('package.json'));
  });
});

test('通用原子写入后缀只在调用内存在，取消不提交启停提案', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-atomic-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home });
  t.after(off);
  const root = profilePath(), temporary = join(root, 'package.json.tmp-random');
  await withProfile(async () => {
    fs.writeFileSync(temporary, fs.readFileSync(join(root, 'package.json')));
    fs.renameSync(temporary, join(root, 'package.json'));
    assert.equal(fs.existsSync(temporary), false);
  });
  const controller = new AbortController();
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), '[]');
    controller.abort();
  }, controller.signal), { name: 'AbortError' });
  assert.equal(nativeFs.existsSync(root), false);
});

test('fs/promises 的目录扫描保留 DSH profile 的异步迭代语义', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-opendir-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home });
  t.after(off);
  const root = profilePath();
  await withProfile(async () => {
    const dir = await fs.promises.opendir(root);
    const names = [];
    for await (const entry of dir) names.push(entry.name);
    assert.deepEqual(names.sort(), ['.npmrc', 'cordis.patch.yml', 'cordis.yml', 'package.json']);
    await assert.rejects(dir.read(), { code: 'ERR_DIR_CLOSED' });
    const callbackDir = await new Promise((resolve, reject) => fs.opendir(root, (error, dir) => error ? reject(error) : resolve(dir)));
    const entry = await new Promise((resolve, reject) => callbackDir.read((error, entry) => error ? reject(error) : resolve(entry)));
    assert.equal(entry.isFile(), true);
    await callbackDir.close();
  });
});
