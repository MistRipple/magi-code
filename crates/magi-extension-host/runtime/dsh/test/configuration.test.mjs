import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { parseConfigurationSource, prepareConfiguration } from '../configuration.mjs';

const captured = (name, content) => ({ name, content, sha256: createHash('sha256').update(content).digest('hex') });

test('Include expressions remain data and source identity survives YAML and JSON parsing', () => {
  const content = '- id: business\n  name: example\n  disabled: !!js process.exit(1)\n  config:\n    enabled: true\n';
  const source = parseConfigurationSource({ name: 'cordis.yml', content });
  assert.deepEqual(source.entries, [{ id: 'business', name: 'example', disabled: { __jsExpr: 'process.exit(1)' }, config: { enabled: true } }]);
  assert.equal(source.sha256.length, 64);
  assert.notEqual(parseConfigurationSource({ name: 'cordis.yml', content: `${content}\n` }).sha256, source.sha256);
  assert.deepEqual(parseConfigurationSource({ name: 'cordis.json', content: JSON.stringify(source.entries) }).entries, source.entries);
});

test('patch sources preserve explicit false and insertion data instead of guessing enabled state', () => {
  const source = parseConfigurationSource({ name: 'cordis.patch.yml', content: '- id: business\n  disabled: false\n- insert:\n    - id: next\n      name: next-plugin\n' });
  assert.deepEqual(source.entries, [{ id: 'business', disabled: false }, { insert: [{ id: 'next', name: 'next-plugin' }] }]);
});

test('invalid, half-written and non-serializable sources fail without producing a candidate', () => {
  for (const content of ['', '# editing\n', 'name: object', '- name: [broken', '- &cycle { child: *cycle }', '- name: example\n  config: { value: .inf }']) {
    assert.throws(() => parseConfigurationSource({ name: 'cordis.yml', content }));
  }
  assert.throws(() => parseConfigurationSource({ name: 'configuration.js', content: '[]' }), /extension/);
  assert.throws(() => parseConfigurationSource({ name: 'cordis.yml', content: ' '.repeat(1024 * 1024 + 1) }), /oversized/);
  let content = '- &a [1, 2, 3, 4, 5, 6, 7, 8]\n';
  let previous = 'a';
  for (const alias of ['b', 'c', 'd', 'e', 'f']) {
    content += `- &${alias} [${Array(8).fill(`*${previous}`).join(', ')}]\n`;
    previous = alias;
  }
  assert.throws(() => parseConfigurationSource({ name: 'cordis.yml', content }), /structural limits/);
});

test('a captured bundle layer resolves a real package into an inert plan with the exact source-set digest', async t => {
  const projectDirectory = await realpath(await mkdtemp(join(tmpdir(), 'magi-configuration-')));
  t.after(() => rm(projectDirectory, { recursive: true, force: true }));
  const directory = join(projectDirectory, 'node_modules', 'business');
  await mkdir(directory, { recursive: true });
  await writeFile(join(projectDirectory, 'package.json'), '{"private":true}');
  const manifest = '{"name":"business","version":"1.0.0","main":"index.mjs"}';
  await writeFile(join(directory, 'package.json'), manifest);
  await writeFile(join(directory, 'index.mjs'), "throw new Error('preparation must not execute');");
  const packages = [{ name: 'business', version: '1.0.0', directory, projectDirectory,
    manifestSha256: createHash('sha256').update(manifest).digest('hex') }];
  const sources = [captured('cordis.yml', '[]\n'), captured('bundle.patch.yml', '- insert:\n    - id: business\n      name: business\n'),
    captured('cordis.patch.yml', '- id: business\n  disabled: false\n')].sort((a,b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
  const plan = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources, base: 'cordis.yml', overlays: ['bundle.patch.yml', 'cordis.patch.yml'], packages });
  assert.equal(plan.sourcesSha256, createHash('sha256').update(JSON.stringify(sources)).digest('hex'));
  assert.equal(plan.graph.root.entries[0].children[0].artifact, 'business');
  assert.equal(plan.graph.artifacts.business.entry, join(directory, 'index.mjs'));
  assert.equal(plan.graph.root.entries[0].children[0].options.disabled, false);
  const disabled = sources.map(source => source.name !== 'cordis.patch.yml' ? source : captured(source.name, '- id: business\n  disabled: true\n'));
  const stopped = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: disabled, base: 'cordis.yml', overlays: ['bundle.patch.yml', 'cordis.patch.yml'] });
  assert.equal(stopped.graph.root.entries[0].children[0].options.disabled, true);
  assert.equal(stopped.graph.root.entries[0].children[0].artifact, undefined);
  await assert.rejects(prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources, base: 'cordis.yml', overlays: ['bundle.patch.yml', 'cordis.patch.yml'] }), /No admitted artifact/);
});

test('include closure, groups, anonymous identities and repeated preparation remain stable', async () => {
  const projectDirectory = '/managed-loading-project';
  const root = captured('cordis.yml', '- id: include\n  name: cordis:include\n  config:\n    path: nested.yml\n');
  const nested = captured('nested.yml', '- name: one\n  disabled: true\n- name: two\n  disabled: true\n');
  const first = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: [root, nested], base: root.name });
  const same = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: [nested, root], base: root.name, previous: first.graph });
  assert.deepEqual(same, first);
  const changed = captured('nested.yml', '- id: inserted\n  name: other\n  disabled: true\n- disabled: true\n  name: one\n- name: two\n  disabled: true\n');
  const next = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: [root, changed], base: root.name, previous: first.graph });
  assert.equal(next.graph.root.entries[0].children[0].children[1].options.id, first.graph.root.entries[0].children[0].children[0].options.id);
  assert.equal(next.graph.root.entries[0].children[0].children[2].options.id, first.graph.root.entries[0].children[0].children[1].options.id);
  await assert.rejects(prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: [root], base: root.name }), /not captured/);
  const cycle = captured('nested.yml', '- id: again\n  name: cordis:include\n  config:\n    path: cordis.yml\n');
  await assert.rejects(prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: [root, cycle], base: root.name }), /include cycle/);
  const duplicate = captured('cordis.yml', '- id: shared\n  name: cordis:group\n  group: true\n  config:\n    - id: shared\n      name: other\n      disabled: true\n');
  await assert.rejects(prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory, sources: [duplicate], base: duplicate.name }), /Duplicate/);
});

test('patches can address the stable id of an anonymous base row before applying overrides', async () => {
  const projectDirectory = '/managed-loading-project';
  const root = captured('cordis.yml', '- name: business\n  disabled: true\n  config: { label: inherited }\n');
  const empty = captured('cordis.patch.yml', '[]\n');
  const first = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory,
    sources: [root, empty], base: root.name, overlays: [empty.name] });
  const id = first.graph.root.entries[0].children[0].options.id;
  const override = captured(empty.name, `- id: ${id}\n  config: { label: edited }\n`);
  const second = await prepareConfiguration({ projectDirectory, sourceDirectory: projectDirectory,
    sources: [root, override], base: root.name, overlays: [override.name], previous: first.graph });
  assert.equal(second.graph.root.entries[0].children[0].options.id, id);
  assert.equal(second.graph.root.entries[0].children[0].options.config.label, 'edited');
  assert.equal(second.graph.warnings.length, 0);
});
