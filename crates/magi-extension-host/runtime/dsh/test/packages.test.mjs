import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkRuntimeRequirements, inspectOriginalPackage } from '../packages.mjs';

const manifestHash = bytes => createHash('sha256').update(bytes).digest('hex');

async function installed(t, manifest) {
  const projectDirectory = await realpath(await mkdtemp(join(tmpdir(), 'magi-native-package-')));
  t.after(() => rm(projectDirectory, { recursive: true, force: true }));
  const directory = join(projectDirectory, 'node_modules', manifest.name);
  await mkdir(directory, { recursive: true });
  await writeFile(join(projectDirectory, 'package.json'), '{"private":true,"type":"module"}');
  const content = JSON.stringify(manifest);
  await writeFile(join(directory, 'package.json'), content);
  return { projectDirectory, directory, name: manifest.name, version: manifest.version, manifestSha256: manifestHash(content) };
}

test('native import conditions select the original backend without running it or using require', async t => {
  const identity = await installed(t, { name: '@example/original', version: '1.0.0', type: 'module',
    exports: { '.': { types: './missing.d.ts', import: './import.js', require: './missing.cjs' }, './client': './client.js' } });
  const source = "throw new Error('inspection must not execute package code');";
  await writeFile(join(identity.directory, 'import.js'), source);
  const result = await inspectOriginalPackage(identity);
  assert.equal(result.entry, join(identity.directory, 'import.js'));
  assert.equal(result.clientDeclared, true);
  assert.equal(await readFile(result.entry, 'utf8'), source);
});

test('legacy npm main is resolved by Node; a missing import never falls back to require', async t => {
  const identity = await installed(t, { name: 'original', version: '1.0.0', main: './backend.cjs' });
  await writeFile(join(identity.directory, 'backend.cjs'), 'module.exports = {}');
  assert.equal((await inspectOriginalPackage(identity)).entry, join(identity.directory, 'backend.cjs'));
  const broken = await installed(t, { name: 'broken-original', version: '1.0.0', exports: { import: './missing.js', require: './backend.cjs' } });
  await writeFile(join(broken.directory, 'backend.cjs'), 'module.exports = {}');
  await assert.rejects(inspectOriginalPackage(broken), /ENOENT/);
});

test('manifest replacement, wrong project and backend paths outside the artifact are rejected', async t => {
  const identity = await installed(t, { name: 'original', version: '1.0.0', main: '../outside.js' });
  await writeFile(join(identity.projectDirectory, 'node_modules', 'outside.js'), 'export {}');
  await assert.rejects(inspectOriginalPackage(identity), /original package file/);
  await assert.rejects(inspectOriginalPackage({ ...identity, directory: identity.projectDirectory }), /selected loading project/);
  await writeFile(join(identity.directory, 'package.json'), '{"name":"different","version":"1.0.0"}');
  await assert.rejects(inspectOriginalPackage(identity), /manifest changed/);
});

test('DSH engines take precedence; peer rules include prereleases and formal workspace ranges', () => {
  checkRuntimeRequirements({ engines: { dsh: '^0.2.0' }, peerDependencies: { '@deepseek-ai/dsh-tools': '^9' } });
  for (const range of ['workspace:^', 'workspace:~', 'workspace:*', '^0.2.0', '>=0.1.7-rc.1 <0.3.0-0']) {
    checkRuntimeRequirements({ peerDependencies: { '@deepseek-ai/dsh-tools': range } });
  }
  checkRuntimeRequirements({ peerDependencies: { other: '^99' } });
  checkRuntimeRequirements({});
  for (const range of ['', '^9', 'nonsense', 'workspace:1.0.0']) {
    assert.throws(() => checkRuntimeRequirements({ peerDependencies: { '@deepseek-ai/dsh-tools': range } }), /requires/);
  }
  assert.throws(() => checkRuntimeRequirements({ engines: { dsh: 'workspace:*' } }), /requires/);
  assert.throws(() => checkRuntimeRequirements({ engines: [] }), /object/);
  assert.throws(() => checkRuntimeRequirements({ engines: { dsh: null } }), /string/);
  assert.throws(() => checkRuntimeRequirements({ peerDependencies: { unrelated: null } }), /string/);
});

test('shared SDK peers resolve to the host module; a separate copy is an admission error', async t => {
  const shared = await installed(t, { name: 'shared-sdk', version: '1.0.0', main: './index.mjs', peerDependencies: { '@deepseek-ai/cordis': '^4.1.0' } });
  await writeFile(join(shared.directory, 'index.mjs'), 'export {}');
  const scope = join(shared.projectDirectory, 'node_modules', '@deepseek-ai');
  await mkdir(scope);
  await symlink(fileURLToPath(new URL('../node_modules/@deepseek-ai/cordis', import.meta.url)), join(scope, 'cordis'), 'dir');
  const result = await inspectOriginalPackage(shared);
  assert.equal(result.sdkModules['@deepseek-ai/cordis'], import.meta.resolve('@deepseek-ai/cordis'));
  const separate = await installed(t, { name: 'separate-sdk', version: '1.0.0', main: './index.mjs', peerDependencies: { '@deepseek-ai/cordis': '^4.1.0' } });
  await writeFile(join(separate.directory, 'index.mjs'), 'export {}');
  const copy = join(separate.projectDirectory, 'node_modules', '@deepseek-ai', 'cordis');
  await mkdir(copy, { recursive: true });
  await writeFile(join(copy, 'package.json'), '{"name":"@deepseek-ai/cordis","version":"4.1.0","main":"index.js"}');
  await writeFile(join(copy, 'index.js'), 'export {}');
  await assert.rejects(inspectOriginalPackage(separate), /separate SDK module/);
});
