// Stateless bundle-layer compiler and public ctx.pluginManager service.
// Daemon owns archive acquisition, dependency staging, CAS commits and cancellation.
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { entryListSchema } from '@deepseek-ai/cordis-plugin-include';
import { assertBundlePlatform, readPluginMeta, UnsupportedBundlePlatformError } from '@deepseek-ai/dsh-app-boot';
import { PROTECTED_PLUGINS } from '@deepseek-ai/dsh-official-bundles';
import { Remote, RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol';
import { deepEqual } from '@deepseek-ai/cosmokit';
import semver from 'semver';
import yaml from 'js-yaml';
import { runtimeVersion } from './packages.mjs';

function dumpYaml(entries) {
  if (!entries.length) return '[]\n';
  return yaml.dump(entries, { schema: entryListSchema });
}

function loadYamlArray(content, label) {
  const parsed = yaml.load(content, { schema: entryListSchema });
  if (parsed === undefined || parsed === null) return [];
  if (!Array.isArray(parsed)) throw new Error(`${label} must be a top-level YAML array`);
  return parsed;
}

export function diagnoseRuntimeCompatibility(manifest, version = runtimeVersion) {
  const incompatible = [];
  if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)) {
    throw new Error('Plugin manifest must be an object');
  }
  const engines = Object.hasOwn(manifest, 'engines') && manifest.engines && typeof manifest.engines === 'object'
    ? manifest.engines
    : (manifest.dsh?.engines && typeof manifest.dsh.engines === 'object' ? manifest.dsh.engines : {});
  const requirements = {};
  const satisfies = range => typeof range === 'string' && range.trim() !== ''
    && semver.satisfies(version, range, { includePrerelease: true });
  if (Object.hasOwn(engines, 'dsh')) {
    if (!satisfies(engines.dsh)) requirements['engines.dsh'] = String(engines.dsh ?? '');
  } else if (manifest.peerDependencies && typeof manifest.peerDependencies === 'object') {
    for (const [dep, range] of Object.entries(manifest.peerDependencies)) {
      if (dep !== '@deepseek-ai/dsh' && !dep.startsWith('@deepseek-ai/dsh-')) continue;
      const effective = ['workspace:^', 'workspace:~', 'workspace:*'].includes(range) ? version : range;
      if (!satisfies(effective)) requirements[dep] = String(range ?? '');
    }
  }
  if (Object.keys(requirements).length) {
    incompatible.push({
      name: String(manifest.name ?? ''),
      version: String(manifest.version ?? ''),
      runtimeVersion: version,
      requirements,
    });
  }
  return incompatible;
}

async function readDeclaredBundle(packageDir) {
  const manifestPath = join(packageDir, 'package.json');
  const manifestRaw = await readFile(manifestPath, 'utf8');
  const manifest = JSON.parse(manifestRaw);
  const incompatible = diagnoseRuntimeCompatibility(manifest);
  let unsupportedPlatform = false;
  try {
    assertBundlePlatform(manifest.dsh?.bundle?.platforms, process.platform);
  } catch (error) {
    if (error instanceof UnsupportedBundlePlatformError) unsupportedPlatform = true;
    else throw error;
  }
  let meta;
  try {
    meta = readPluginMeta(manifest.name ?? '', pathToFileURL(manifestPath).href);
  } catch {
    meta = undefined;
  }
  const declaredPatch = manifest.dsh?.bundle?.patch;
  if (typeof declaredPatch !== 'string' || !declaredPatch.trim()) {
    return { manifest, meta, incompatible, unsupportedPlatform, isBundle: false, patchEntries: [], rows: [], overrides: [] };
  }
  const patchFile = resolve(packageDir, declaredPatch);
  const rel = relative(packageDir, patchFile);
  if (!rel || isAbsolute(rel) || rel === '..' || rel.startsWith(`..${sep}`)) {
    throw new Error(`Bundle patch path escapes package directory: ${declaredPatch}`);
  }
  const patchText = await readFile(patchFile, 'utf8');
  const patchEntries = loadYamlArray(patchText, `${manifest.name}:${declaredPatch}`);
  const rows = [];
  const overrides = [];
  for (const patch of patchEntries) {
    if (!patch || typeof patch !== 'object' || Array.isArray(patch)) continue;
    if (Array.isArray(patch.insert)) {
      for (const item of patch.insert) {
        if (item && typeof item === 'object' && typeof item.id === 'string' && item.id) {
          rows.push({
            rowId: item.id,
            moduleName: typeof item.name === 'string' ? item.name : '',
            ...(meta !== undefined ? { meta } : {}),
          });
        }
      }
    } else if (typeof patch.id === 'string' && patch.id) {
      overrides.push(patch.id);
    }
  }
  return {
    manifest,
    meta,
    incompatible,
    unsupportedPlatform,
    isBundle: true,
    patchPath: rel.split(sep).join('/'),
    patchEntries,
    rows,
    overrides,
  };
}

function patchOwnedByRows(patch, rowIdSet, patchTemplateList) {
  if (!patch || typeof patch !== 'object' || Array.isArray(patch)) return false;
  if (Array.isArray(patch.insert)) {
    return patch.insert.some(item => item && typeof item === 'object' && rowIdSet.has(item.id));
  }
  return patchTemplateList.some(template => deepEqual(template, patch));
}

export async function prepareBundleMutation({
  action,
  sources,
  packages,
  previousPackageDir,
  targetName,
  enabled = true,
  preserveEnabled = false,
}) {
  const byName = new Map((sources ?? []).map(source => [source.name, source]));
  const bundleSource = byName.get('bundle.patch.yml');
  const userSource = byName.get('cordis.patch.yml');
  if (!bundleSource || !userSource) throw new Error('Bundle mutation requires captured bundle.patch.yml and cordis.patch.yml');
  const currentBundleEntries = loadYamlArray(bundleSource.content, 'bundle.patch.yml');
  const currentUserEntries = loadYamlArray(userSource.content, 'cordis.patch.yml');

  if (action === 'inspectBundles') {
    const bundles = [];
    for (const pkg of packages ?? []) {
      const info = await readDeclaredBundle(pkg.directory);
      const rowIds = new Set(info.rows.map(r => r.rowId));
      const isSelected = info.isBundle && (
        rowIds.size
          ? currentBundleEntries.some(entry => Array.isArray(entry?.insert) && entry.insert.some(item => rowIds.has(item?.id)))
          : info.patchEntries.every(template => currentBundleEntries.some(entry => deepEqual(entry, template)))
      );
      bundles.push({
        name: pkg.name,
        official: pkg.name.startsWith('@deepseek-ai/'),
        availability: 'profile',
        version: pkg.version,
        ...(typeof info.manifest.description === 'string' && info.manifest.description !== ''
          ? { description: info.manifest.description } : {}),
        ...(info.meta !== undefined ? { meta: info.meta } : {}),
        ...(typeof info.manifest.dsh?.bundle?.listedByDefault === 'boolean'
          ? { listedByDefault: info.manifest.dsh.bundle.listedByDefault } : {}),
        ...(info.unsupportedPlatform ? { listedByDefault: false, error: { code: 'unsupported-platform' } } : {}),
        enabled: isSelected,
        installed: true,
        source: pkg.version,
        optional: false,
        removable: true,
        update: 'registry',
        rows: info.rows,
        overrides: info.overrides,
      });
    }
    return { bundles };
  }

  if (action === 'remove') {
    if (!previousPackageDir) throw new Error('Removing a bundle requires its installed package directory');
    const oldInfo = await readDeclaredBundle(previousPackageDir);
    const oldRowIds = new Set(oldInfo.rows.map(r => r.rowId));
    const nextBundle = currentBundleEntries.filter(entry => !patchOwnedByRows(entry, oldRowIds, oldInfo.patchEntries));
    const nextUser = currentUserEntries.filter(entry => !(entry && typeof entry === 'object' && !Array.isArray(entry)
      && typeof entry.id === 'string' && oldRowIds.has(entry.id)));
    const edits = [];
    const nextBundleText = dumpYaml(nextBundle);
    if (nextBundleText !== bundleSource.content) {
      edits.push({ name: 'bundle.patch.yml', expectedSha256: bundleSource.sha256, content: nextBundleText });
    }
    const nextUserText = dumpYaml(nextUser);
    if (nextUserText !== userSource.content) {
      edits.push({ name: 'cordis.patch.yml', expectedSha256: userSource.sha256, content: nextUserText });
    }
    return { ok: true, edits };
  }

  const targetPkg = (packages ?? []).find(pkg => pkg.name === targetName);
  if (!targetPkg) throw new Error(`Target bundle is missing from candidate packages: ${targetName}`);
  const targetInfo = await readDeclaredBundle(targetPkg.directory);
  if (targetInfo.unsupportedPlatform) {
    return {
      ok: false,
      error: {
        code: 'unsupported-platform',
        diagnostic: `插件 ${targetPkg.name}@${targetPkg.version} 不支持当前运行平台 ${process.platform}`,
      },
    };
  }
  if (targetInfo.incompatible.length) {
    return {
      ok: false,
      error: {
        code: 'incompatible-version',
        diagnostic: `插件 ${targetPkg.name}@${targetPkg.version} 不兼容当前 DSH 运行时 ${runtimeVersion}`,
        incompatible: targetInfo.incompatible,
      },
    };
  }
  if (!targetInfo.isBundle) {
    return {
      ok: false,
      error: {
        code: 'not-bundle',
        diagnostic: `包 ${targetPkg.name}@${targetPkg.version} 未声明 dsh.bundle.patch`,
      },
    };
  }

  const oldInfo = previousPackageDir ? await readDeclaredBundle(previousPackageDir) : null;
  const targetRowIds = new Set([
    ...targetInfo.rows.map(r => r.rowId),
    ...(oldInfo ? oldInfo.rows.map(r => r.rowId) : []),
  ]);
  const oldTemplates = [...targetInfo.patchEntries, ...(oldInfo ? oldInfo.patchEntries : [])];
  const preservedConfigs = new Map();
  for (const entry of currentBundleEntries) {
    if (Array.isArray(entry?.insert)) {
      for (const item of entry.insert) {
        if (item && typeof item === 'object' && targetRowIds.has(item.id) && item.config !== undefined) {
          preservedConfigs.set(item.id, structuredClone(item.config));
        }
      }
    }
  }

  const strippedBundle = currentBundleEntries.filter(entry => !patchOwnedByRows(entry, targetRowIds, oldTemplates));
  let nextBundle = strippedBundle;
  const includeBundle = preserveEnabled && oldInfo
    ? currentBundleEntries.some(entry => patchOwnedByRows(entry, targetRowIds, oldTemplates))
    : enabled;
  if (includeBundle) {
    const additions = structuredClone(targetInfo.patchEntries).map(entry => {
      if (Array.isArray(entry?.insert)) {
        for (const item of entry.insert) {
          if (item && typeof item === 'object' && preservedConfigs.has(item.id)) {
            item.config = preservedConfigs.get(item.id);
          }
        }
      }
      return entry;
    });
    nextBundle = [...strippedBundle, ...additions];
  }

  const edits = [];
  const nextBundleText = dumpYaml(nextBundle);
  if (nextBundleText !== bundleSource.content) {
    edits.push({ name: 'bundle.patch.yml', expectedSha256: bundleSource.sha256, content: nextBundleText });
  }
  return {
    ok: true,
    changed: edits.length > 0 || action === 'install' || action === 'upgrade',
    edits,
    version: targetPkg.version,
    rows: targetInfo.rows,
    overrides: targetInfo.overrides,
  };
}

function markRemoteMethod(prototype, method) {
  const decorator = Remote(method);
  decorator(prototype[method], {
    kind: 'method',
    name: method,
    private: false,
    static: false,
    addInitializer(fn) {
      fn.call(Object.create(prototype));
    },
  });
}

export async function openPluginManager({ lifecycle, tree, hostGeneration, requestOwner }) {
  const root = lifecycle.context;

  function resolveCaller(serviceInstance) {
    if (serviceInstance.ctx.invocation !== undefined) {
      const signal = serviceInstance.ctx.invocation.signal;
      return {
        caller: { kind: 'host', hostGeneration },
        assertActive() { signal?.throwIfAborted(); },
      };
    }
    const callerCtx = root.reflect.trace(serviceInstance.ctx);
    if (!callerCtx?.fiber || callerCtx.fiber.uid === 0 || !lifecycle.hasActivation(callerCtx)) {
      return {
        caller: { kind: 'host', hostGeneration },
        assertActive() {},
      };
    }
    const scope = tree.configuration(callerCtx);
    scope.assertCurrent();
    const id = tree.locateFiber(callerCtx.fiber);
    const status = lifecycle.peek({
      hostGeneration,
      pluginId: lifecycle.contributionScope(callerCtx).identity.pluginId,
      instanceId: id,
      activationGeneration: lifecycle.contributionScope(callerCtx).identity.activationGeneration,
    });
    return {
      caller: {
        kind: 'plugin',
        hostGeneration,
        pluginId: status.pluginId,
        instanceId: status.instanceId,
        activationGeneration: status.activationGeneration,
        version: status.version,
        configurationEpoch: status.configurationEpoch,
      },
      assertActive: scope.assertCurrent,
    };
  }

  function emitOperation(payload) {
    void root.parallel('plugin-manager/operation', payload).catch(() => {});
  }

  class HostPluginManager extends TypertRemoteService {
    constructor(ctx) {
      super(ctx, 'pluginManager');
    }

    listVersionExemptions() {
      return { exemptions: {}, warnings: [] };
    }

    async setVersionExemption(packageVersion, runtimeVersion, enabled, acceptRisk) {
      void runtimeVersion;
      void acceptRisk;
      if (!enabled) {
        return {
          changed: false,
          application: 'applied',
          stage: 'enable',
          target: String(packageVersion ?? ''),
          enabled: false,
        };
      }
      return {
        changed: false,
        application: 'failed',
        stage: 'enable',
        target: String(packageVersion ?? ''),
        enabled: false,
        error: {
          code: 'management-required',
          diagnostic: 'Magi 不启用插件版本豁免策略；请安装满足当前 DSH 运行时版本要求的包版本',
        },
      };
    }

    async registries() {
      const { caller } = resolveCaller(this);
      return await requestOwner('pluginManager.registries', { caller });
    }

    async inspect(spec, options, signal) {
      const { caller } = resolveCaller(this);
      signal?.throwIfAborted();
      return await requestOwner('pluginManager.inspect', { caller, spec: String(spec ?? ''), options: options ?? {} });
    }

    async listVersions(name, options, signal) {
      const { caller } = resolveCaller(this);
      signal?.throwIfAborted();
      return await requestOwner('pluginManager.listVersions', { caller, name: String(name ?? ''), options: options ?? {} });
    }

    async listPlugins() {
      const { caller } = resolveCaller(this);
      const state = await requestOwner('pluginManager.listPlugins', { caller });
      const liveById = new Map(tree.liveEntries().map(entry => [entry.id, entry]));
      return (state.plugins ?? []).map(plugin => {
        const actual = liveById.get(plugin.entryId);
        const isProtected = PROTECTED_PLUGINS.has(plugin.moduleName);
        if (isProtected) {
          const { patchId: _omit, ...rest } = plugin;
          return { ...rest, readOnlyReason: 'management-required' };
        }
        if (actual && actual.disabled !== undefined) {
          return { ...plugin, disabled: Boolean(actual.disabled) };
        }
        return plugin;
      });
    }

    async listBundles() {
      const { caller } = resolveCaller(this);
      const state = await requestOwner('pluginManager.listBundles', { caller });
      const liveEntryIds = new Map();
      for (const entry of tree.liveEntries()) {
        if (entry.rawId) {
          liveEntryIds.set(entry.rawId, liveEntryIds.has(entry.rawId) ? null : entry.id);
        }
      }
      return (state.bundles ?? []).map(bundle => ({
        ...bundle,
        rows: (bundle.rows ?? []).map(row => {
          const entryId = liveEntryIds.get(row.rowId);
          return typeof entryId === 'string' ? { ...row, entryId } : row;
        }),
      }));
    }

    async readBundleDetail(name, language, options) {
      void options;
      const { caller } = resolveCaller(this);
      if (typeof language !== 'string' || !/^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})*$/.test(language)) {
        throw new RemoteError('plugin-manager/invalid-language', `Invalid BCP 47 language: ${language}`, { language: String(language) });
      }
      const reply = await requestOwner('pluginManager.readBundleDetail', { caller, name: String(name ?? ''), language });
      if (reply.error === 'unknown-bundle') {
        throw new RemoteError('plugin-manager/unknown-bundle', `Unknown bundle: ${name}`, { name: String(name) });
      }
      return reply.detail;
    }

    async setPluginEnabled(id, enabled) {
      const { caller } = resolveCaller(this);
      const opId = randomUUID();
      emitOperation({ id: opId, time: new Date().toISOString(), reason: 'plugin', target: String(id), stage: 'enable', enabled: Boolean(enabled), phase: 'started' });
      const result = await requestOwner('pluginManager.setPluginEnabled', { caller, id: String(id), enabled: Boolean(enabled) });
      if (result.changed && result.application !== 'failed') root.emit('plugin-manager/changed', { reason: 'plugin' });
      emitOperation({ id: opId, time: new Date().toISOString(), reason: 'plugin', target: String(id), stage: 'enable', enabled: Boolean(enabled), phase: 'settled', application: result.application, changed: Boolean(result.changed), errorCode: result.error?.code });
      return result;
    }

    async setBundleEnabled(name, enabled) {
      const { caller } = resolveCaller(this);
      const opId = randomUUID();
      emitOperation({ id: opId, time: new Date().toISOString(), reason: 'bundle', target: String(name), stage: 'enable', enabled: Boolean(enabled), phase: 'started' });
      const result = await requestOwner('pluginManager.setBundleEnabled', { caller, name: String(name), enabled: Boolean(enabled) });
      if (result.changed && result.application !== 'failed') root.emit('plugin-manager/changed', { reason: 'bundle' });
      emitOperation({ id: opId, time: new Date().toISOString(), reason: 'bundle', target: String(name), stage: 'enable', enabled: Boolean(enabled), phase: 'settled', application: result.application, changed: Boolean(result.changed), errorCode: result.error?.code });
      return result;
    }

    async installBundle(spec, options) {
      const { caller } = resolveCaller(this);
      const opId = randomUUID();
      const requestId = typeof options?.requestId === 'string' && options.requestId ? options.requestId : undefined;
      emitOperation({ id: opId, time: new Date().toISOString(), reason: 'install', target: null, stage: 'install', enabled: options?.enabled ?? true, phase: 'started' });
      if (requestId) {
        root.emit('plugin-manager/install-state', {
          requestId,
          phase: 'installing',
          attempt: { registry: options?.registry ?? null, index: 1, total: 1 },
        });
      }
      const result = await requestOwner('pluginManager.installBundle', {
        caller,
        spec: String(spec ?? ''),
        options: {
          enabled: options?.enabled ?? true,
          ...(requestId ? { requestId } : {}),
          ...(options?.registry !== undefined ? { registry: options.registry } : {}),
          ...(options?.expected !== undefined ? { expected: options.expected } : {}),
        },
      });
      if (result.changed && result.application !== 'failed' && result.application !== 'cancelled') {
        root.emit('plugin-manager/changed', { reason: 'install' });
      }
      emitOperation({
        id: opId,
        time: new Date().toISOString(),
        reason: 'install',
        target: result.bundle ?? null,
        stage: result.stage ?? 'install',
        enabled: result.enabled,
        version: result.version,
        phase: 'settled',
        application: result.application,
        changed: Boolean(result.changed),
        errorCode: result.error?.code,
      });
      return result;
    }

    async waitForInstall(requestId) {
      const { caller } = resolveCaller(this);
      const reply = await requestOwner('pluginManager.waitForInstall', { caller, requestId: String(requestId ?? '') });
      return reply?.result ?? null;
    }

    async cancelInstall(requestId) {
      const { caller } = resolveCaller(this);
      return await requestOwner('pluginManager.cancelInstall', { caller, requestId: String(requestId ?? '') });
    }

    async removeBundle(name) {
      const { caller } = resolveCaller(this);
      const opId = randomUUID();
      emitOperation({ id: opId, time: new Date().toISOString(), reason: 'remove', target: String(name), stage: 'remove', phase: 'started' });
      const result = await requestOwner('pluginManager.removeBundle', { caller, name: String(name ?? '') });
      if (result.changed && result.application !== 'failed') {
        root.emit('plugin-manager/changed', { reason: 'remove' });
      }
      emitOperation({
        id: opId,
        time: new Date().toISOString(),
        reason: 'remove',
        target: String(name),
        stage: 'remove',
        phase: 'settled',
        application: result.application,
        changed: Boolean(result.changed),
        errorCode: result.error?.code,
      });
      return result;
    }

    async updateBundle(name, version, options) {
      const { caller } = resolveCaller(this);
      const requestId = typeof options?.requestId === 'string' && options.requestId ? options.requestId : undefined;
      if (requestId) root.emit('plugin-manager/update-state', { requestId, step: 'installing' });
      const outcome = await requestOwner('pluginManager.updateBundle', {
        caller,
        name: String(name ?? ''),
        version: String(version ?? ''),
        options: options ?? {},
      });
      if (outcome?.result?.changed && outcome.result.application !== 'failed') {
        root.emit('plugin-manager/changed', { reason: 'install' });
      }
      return outcome;
    }
  }

  for (const method of [
    'listVersionExemptions',
    'setVersionExemption',
    'registries',
    'inspect',
    'listVersions',
    'listPlugins',
    'listBundles',
    'readBundleDetail',
    'setPluginEnabled',
    'setBundleEnabled',
    'installBundle',
    'waitForInstall',
    'cancelInstall',
    'removeBundle',
    'updateBundle',
  ]) {
    markRemoteMethod(HostPluginManager.prototype, method);
  }

  const fiber = root.plugin(HostPluginManager);
  await fiber.await();
  return {
    async close() {
      await fiber.dispose();
    },
  };
}
