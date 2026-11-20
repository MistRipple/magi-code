import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PluginLifecycle } from '../lifecycle.mjs';
import { CommittedTree } from '../committed-tree.mjs';
import { prepareConfiguration } from '../configuration.mjs';
import { diagnoseRuntimeCompatibility, openPluginManager, prepareBundleMutation } from '../plugin-manager.mjs';

const sha256 = text => createHash('sha256').update(text).digest('hex');

test('diagnoseRuntimeCompatibility checks engines.dsh, dsh.engines.dsh, and DSH peerDependencies with prereleases', () => {
  assert.deepEqual(diagnoseRuntimeCompatibility({
    name: 'ok-plugin',
    version: '1.0.0',
    dsh: { engines: { dsh: '>=0.2.1-rc.2 <0.4.0-0' } },
  }, '0.2.2-rc.3'), []);

  assert.deepEqual(diagnoseRuntimeCompatibility({
    name: 'bad-engine',
    version: '1.0.0',
    engines: { dsh: '>=0.3.0' },
  }, '0.2.2-rc.3'), [{
    name: 'bad-engine',
    version: '1.0.0',
    runtimeVersion: '0.2.2-rc.3',
    requirements: { 'engines.dsh': '>=0.3.0' },
  }]);

  assert.deepEqual(diagnoseRuntimeCompatibility({
    name: 'bad-peer',
    version: '2.0.0',
    peerDependencies: {
      '@deepseek-ai/cordis': '^4.1.0',
      '@deepseek-ai/dsh-tools': '>=0.3.0-rc.1',
    },
  }, '0.2.2-rc.3'), [{
    name: 'bad-peer',
    version: '2.0.0',
    runtimeVersion: '0.2.2-rc.3',
    requirements: { '@deepseek-ai/dsh-tools': '>=0.3.0-rc.1' },
  }]);
});

test('prepareBundleMutation compiles install, upgrade preserving row config, toggle, and remove edits', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'magi-bundle-mutation-'));
  try {
    const v1Dir = join(dir, 'pkg-v1');
    const v2Dir = join(dir, 'pkg-v2');
    const nonBundleDir = join(dir, 'not-bundle');
    await mkdir(v1Dir, { recursive: true });
    await mkdir(v2Dir, { recursive: true });
    await mkdir(nonBundleDir, { recursive: true });

    await writeFile(join(v1Dir, 'package.json'), JSON.stringify({
      name: 'demo-bundle',
      version: '1.0.0',
      description: 'Demo v1',
      dsh: { bundle: { patch: './cordis.patch.yml' } },
    }));
    await writeFile(join(v1Dir, 'cordis.patch.yml'), '- insert:\n    - id: demo\n      name: demo-bundle\n      config:\n        mode: default\n');

    await writeFile(join(v2Dir, 'package.json'), JSON.stringify({
      name: 'demo-bundle',
      version: '1.1.0',
      description: 'Demo v2',
      dsh: { bundle: { patch: './cordis.patch.yml' } },
    }));
    await writeFile(join(v2Dir, 'cordis.patch.yml'), '- insert:\n    - id: demo\n      name: demo-bundle\n      config:\n        mode: v2-default\n');

    await writeFile(join(nonBundleDir, 'package.json'), JSON.stringify({
      name: 'plain-lib',
      version: '0.1.0',
    }));

    const baseSources = [
      { name: 'bundle.patch.yml', content: '[]\n', sha256: sha256('[]\n') },
      { name: 'cordis.patch.yml', content: '[]\n', sha256: sha256('[]\n') },
    ];

    const refused = await prepareBundleMutation({
      action: 'install',
      sources: baseSources,
      packages: [{ name: 'plain-lib', version: '0.1.0', directory: nonBundleDir }],
      targetName: 'plain-lib',
    });
    assert.equal(refused.ok, false);
    assert.equal(refused.error.code, 'not-bundle');

    const installed = await prepareBundleMutation({
      action: 'install',
      sources: baseSources,
      packages: [{ name: 'demo-bundle', version: '1.0.0', directory: v1Dir }],
      targetName: 'demo-bundle',
      enabled: true,
    });
    assert.equal(installed.ok, true);
    assert.equal(installed.edits.length, 1);
    assert.equal(installed.edits[0].name, 'bundle.patch.yml');
    assert.match(installed.edits[0].content, /id: demo/);

    const customizedBundle = '- insert:\n    - id: demo\n      name: demo-bundle\n      config:\n        mode: custom-user\n';
    const userPatch = '- id: demo\n  disabled: true\n';
    const upgraded = await prepareBundleMutation({
      action: 'upgrade',
      sources: [
        { name: 'bundle.patch.yml', content: customizedBundle, sha256: sha256(customizedBundle) },
        { name: 'cordis.patch.yml', content: userPatch, sha256: sha256(userPatch) },
      ],
      packages: [{ name: 'demo-bundle', version: '1.1.0', directory: v2Dir }],
      previousPackageDir: v1Dir,
      targetName: 'demo-bundle',
      preserveEnabled: true,
    });
    assert.equal(upgraded.ok, true);
    assert.equal(upgraded.version, '1.1.0');
    assert.equal(upgraded.edits.length, 0);

    const pausedUpgrade = await prepareBundleMutation({
      action: 'upgrade',
      sources: baseSources,
      packages: [{ name: 'demo-bundle', version: '1.1.0', directory: v2Dir }],
      previousPackageDir: v1Dir,
      targetName: 'demo-bundle',
      preserveEnabled: true,
    });
    assert.equal(pausedUpgrade.ok, true);
    assert.equal(pausedUpgrade.version, '1.1.0');
    assert.deepEqual(pausedUpgrade.edits, []);

    const inspected = await prepareBundleMutation({
      action: 'inspectBundles',
      sources: [
        { name: 'bundle.patch.yml', content: customizedBundle, sha256: sha256(customizedBundle) },
        { name: 'cordis.patch.yml', content: userPatch, sha256: sha256(userPatch) },
      ],
      packages: [{ name: 'demo-bundle', version: '1.1.0', directory: v2Dir }],
    });
    assert.equal(inspected.bundles.length, 1);
    assert.equal(inspected.bundles[0].enabled, true);
    assert.deepEqual(inspected.bundles[0].rows.map(r => r.rowId), ['demo']);

    const removed = await prepareBundleMutation({
      action: 'remove',
      sources: [
        { name: 'bundle.patch.yml', content: customizedBundle, sha256: sha256(customizedBundle) },
        { name: 'cordis.patch.yml', content: userPatch, sha256: sha256(userPatch) },
      ],
      packages: [],
      previousPackageDir: v2Dir,
    });
    assert.equal(removed.ok, true);
    assert.deepEqual(removed.edits.map(e => e.name), ['bundle.patch.yml', 'cordis.patch.yml']);
    assert.equal(removed.edits[0].content, '[]\n');
    assert.equal(removed.edits[1].content, '[]\n');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('openPluginManager exposes ctx.pluginManager, refuses version exemptions, and fences revoked plugin callers', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'magi-plugin-mgr-service-'));
  const calls = [];
  let lifecycle;
  let tree;
  let service;
  try {
    const projectDir = join(dir, 'project');
    const sourceDir = join(dir, 'sources');
    const pkgDir = join(projectDir, 'node_modules', 'caller-plugin');
    await mkdir(pkgDir, { recursive: true });
    await mkdir(sourceDir, { recursive: true });
    const manifestText = JSON.stringify({
      name: 'caller-plugin',
      version: '1.0.0',
      type: 'module',
      exports: './index.mjs',
    });
    await writeFile(join(pkgDir, 'package.json'), manifestText);
    await writeFile(join(pkgDir, 'index.mjs'), `
      export const name = 'caller-plugin';
      export const inject = ['pluginManager'];
      export let capturedService;
      export function apply(ctx) {
        capturedService = ctx.pluginManager;
      }
    `);
    const cordisYml = '- id: caller\n  name: caller-plugin\n';
    await writeFile(join(sourceDir, 'cordis.yml'), cordisYml);

    const pkgInspection = {
      projectDirectory: projectDir,
      directory: pkgDir,
      name: 'caller-plugin',
      version: '1.0.0',
      manifestSha256: sha256(manifestText),
    };
    const plan = await prepareConfiguration({
      projectDirectory: projectDir,
      sourceDirectory: sourceDir,
      sources: [{ name: 'cordis.yml', content: cordisYml, sha256: sha256(cordisYml) }],
      base: 'cordis.yml',
      overlays: [],
      packages: [pkgInspection],
    });

    const requestOwner = async (method, params) => {
      calls.push({ method, params });
      if (method === 'pluginManager.registries') {
        return { registry: 'https://registry.npmjs.org', fallbackRegistries: [], resolved: 'https://registry.npmjs.org' };
      }
      if (method === 'pluginManager.listBundles') {
        return {
          bundles: [{
            name: 'caller-plugin',
            official: false,
            availability: 'profile',
            version: '1.0.0',
            enabled: true,
            installed: true,
            optional: false,
            removable: true,
            rows: [{ rowId: 'caller', moduleName: 'caller-plugin' }],
            overrides: [],
          }],
        };
      }
      return {};
    };

    lifecycle = new PluginLifecycle({
      hostGeneration: 1,
      onStatus() {},
      onDiagnostic() {},
    });
    tree = new CommittedTree({
      lifecycle,
      hostGeneration: 1,
      requestOwner,
      onDiagnostic() {},
    });
    service = await openPluginManager({ lifecycle, tree, hostGeneration: 1, requestOwner });

    await tree.apply({
      token: { hostGeneration: 1, revision: 1, sequence: 1 },
      configuration: {
        revision: 1,
        sources: [{ name: 'cordis.yml', content: cordisYml, sha256: sha256(cordisYml) }],
        sourcesSha256: plan.sourcesSha256,
        sourceRoot: sourceDir,
        preparation: plan.preparation,
        graph: plan.graph,
        executables: plan.executables,
      },
      activations: plan.executables.map(executable => ({
        executable,
        hostGeneration: 1,
        activationGeneration: 1,
        activationOrder: 1,
      })),
    });

    const mod = await import(join(pkgDir, 'index.mjs'));
    assert.ok(mod.capturedService);
    assert.deepEqual(mod.capturedService.listVersionExemptions(), { exemptions: {}, warnings: [] });
    const refusedExemption = await mod.capturedService.setVersionExemption('caller-plugin@1.0.0', '0.2.2-rc.3', true, true);
    assert.equal(refusedExemption.application, 'failed');
    assert.equal(refusedExemption.error.code, 'management-required');

    const registries = await mod.capturedService.registries();
    assert.equal(registries.registry, 'https://registry.npmjs.org');
    assert.equal(calls.at(-1).params.caller.kind, 'plugin');
    assert.equal(calls.at(-1).params.caller.pluginId, 'caller-plugin');
    assert.equal(calls.at(-1).params.caller.instanceId, 'include:caller');

    const bundles = await mod.capturedService.listBundles();
    assert.equal(bundles[0].rows[0].entryId, 'include:caller');

    // Stop all entries by applying an empty configuration; stale handle must reject!
    const emptyYml = '[]\n';
    const emptyPlan = await prepareConfiguration({
      projectDirectory: projectDir,
      sourceDirectory: sourceDir,
      sources: [{ name: 'cordis.yml', content: emptyYml, sha256: sha256(emptyYml) }],
      base: 'cordis.yml',
      overlays: [],
      packages: [],
    });
    await tree.apply({
      token: { hostGeneration: 1, revision: 2, sequence: 2 },
      configuration: {
        revision: 2,
        sources: [{ name: 'cordis.yml', content: emptyYml, sha256: sha256(emptyYml) }],
        sourcesSha256: emptyPlan.sourcesSha256,
        sourceRoot: sourceDir,
        preparation: emptyPlan.preparation,
        graph: emptyPlan.graph,
        executables: emptyPlan.executables,
      },
      activations: [],
    });

    const beforeRevokedCalls = calls.length;
    await assert.rejects(
      () => mod.capturedService.registries(),
      /ended|not active|no committed plugin identity/i,
    );
    assert.equal(calls.length, beforeRevokedCalls);
  } finally {
    await service?.close();
    await tree?.close();
    await lifecycle?.close();
    await rm(dir, { recursive: true, force: true });
  }
});
