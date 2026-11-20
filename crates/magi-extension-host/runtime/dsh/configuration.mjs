// Parse a captured source; admission and graph application belong to daemon's loading transaction.
import { createHash, randomBytes } from 'node:crypto';
import { extname, isAbsolute, join, relative, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { applyEntryPatches, entryListSchema } from '@deepseek-ai/cordis-plugin-include';
import { deepEqual } from '@deepseek-ai/cosmokit';
import { inspectOriginalPackage } from './packages.mjs';
import { dump, load } from 'js-yaml';

/** Preserve the locked Include YAML dialect, including unevaluated !!js nodes. */
export function parseConfigurationSource({ name, content }) {
  if (typeof name !== 'string' || typeof content !== 'string' || Buffer.byteLength(content) > 1024 * 1024) {
    throw new Error('Invalid or oversized loading configuration source');
  }
  const extension = extname(name);
  let entries;
  if (extension === '.json') {
    try { entries = JSON.parse(content); }
    catch (error) { throw new Error(`Loading configuration ${name}: ${error.message}`, { cause: error }); }
  }
  else if (extension === '.yaml' || extension === '.yml') entries = load(content, { schema: entryListSchema, filename: name });
  else throw new Error(`Unsupported loading configuration extension: ${extension}`);
  validateEntries(entries);
  return { name, sha256: createHash('sha256').update(content).digest('hex'), entries };
}

function validateEntries(entries) {
  if (!Array.isArray(entries)) throw new Error('Loading configuration must be a top-level array');
  // IPC carries JSON values. Reject cycles and bound expanded aliases before serializing.
  const ancestors = new Set();
  let nodes = 0, bytes = 0;
  function check(value, depth) {
    if (++nodes > 32768 || depth > 64) throw new Error('Loading configuration exceeds structural limits');
    if (['undefined', 'function', 'symbol', 'bigint'].includes(typeof value)
      || (typeof value === 'number' && !Number.isFinite(value))) {
      throw new Error('Loading configuration contains a non-JSON value');
    }
    if (typeof value === 'string') bytes += Buffer.byteLength(value);
    if (value && typeof value === 'object') {
      if (ancestors.has(value)) throw new Error('Loading configuration contains a cyclic alias');
      ancestors.add(value);
      for (const [key, child] of Object.entries(value)) {
        bytes += Buffer.byteLength(key);
        check(child, depth + 1);
      }
      ancestors.delete(value);
    }
    if (bytes > 1024 * 1024) throw new Error('Expanded loading configuration exceeds transport limits');
  }
  check(entries, 0);
}

export function serializeConfigurationSource(name, entries) {
  validateEntries(entries);
  return extname(name) === '.json' ? `${JSON.stringify(entries, null, 2)}\n` : dump(entries, { schema: entryListSchema });
}

/** Build a plan from captured sources and admitted artifacts; never start a plugin. */
export async function prepareConfiguration(input) {
  return (await compileConfiguration(input)).plan;
}

export const loadingEntryId = (treeId, id) => treeId ? `${treeId}:${id}` : id;

export class LoadingSourcesRequired extends Error {
  constructor(sources) {
    super(`Loading sources were not captured: ${[...sources.keys()].join(', ')}`);
    this.sources = [...sources].map(([name, defaults]) => ({
      name,
      initial: defaults.map(entries => serializeConfigurationSource(name, entries)),
    }));
  }
}

/** The source editor consumes this transient model; only the plan crosses IPC. */
export async function compileConfiguration({ projectDirectory, sourceDirectory, sources,
  base, overlays = [], packages = [], previous, memoryRoot }) {
  if (typeof projectDirectory !== 'string' || !isAbsolute(projectDirectory)
    || typeof sourceDirectory !== 'string' || !isAbsolute(sourceDirectory) || !Array.isArray(sources)
    || !sources.length || sources.length > 64 || typeof base !== 'string' || !base
    || !Array.isArray(overlays) || overlays.some(name => typeof name !== 'string' || !name)
    || !Array.isArray(packages)) {
    throw new Error('Invalid loading preparation inputs');
  }
  const captured = sources.map(source => ({ name: source.name, content: source.content, sha256: source.sha256 }))
    .sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
  const documents = new Map();
  let bytes = 0;
  for (const source of captured) {
    const parsed = parseConfigurationSource(source);
    if (documents.has(source.name) || parsed.sha256 !== source.sha256) throw new Error('Loading source identity mismatch');
    if (source.name.includes('\\') || source.name.split('/').some(part => ['', '.', '..'].includes(part)) || isAbsolute(source.name)) {
      throw new Error('Loading sources must be relative to the real project');
    }
    bytes += Buffer.byteLength(source.content);
    if (bytes > 4 * 1024 * 1024) throw new Error('Loading source set is oversized');
    documents.set(source.name, parsed.entries);
  }
  const used = new Set();
  const missing = new Map();
  const read = name => {
    used.add(name);
    if (!documents.has(name) && !missing.has(name)) missing.set(name, []);
    return documents.get(name);
  };
  for (const name of [base, ...overlays]) read(name);
  if (missing.size) throw new LoadingSourcesRequired(missing);
  const warnings = [];
  const patch = (entries, patches) => applyEntryPatches(entries, patches, (format, ...args) => warnings.push({ format, args }));
  const available = new Map();
  for (const artifact of packages) {
    if (artifact.projectDirectory !== projectDirectory || available.has(artifact.name)) throw new Error('Invalid or duplicate loading artifact binding');
    available.set(artifact.name, artifact);
  }
  const admitted = new Map();
  const anonymous = [];
  const trees = new Map();
  const previousAnonymous = Array.isArray(previous?.anonymous) ? [...previous.anonymous] : [];
  let nodes = 0;
  if (previous && previous.version !== 2) throw new Error('Unsupported development loading graph; reinstall the original packages');
  const rootOptions = memoryRoot ?? previous?.root.entries.map(node => node.options)
    ?? [{ id: 'include', name: 'cordis:include', config: { path: pathToFileURL(join(sourceDirectory, base)).href } }];
  validateEntries(rootOptions);

  async function compile(entries, source, scope, chain, inheritedDisabled = false, patches = [], parent = null, patchSources = []) {
    if (chain.length > 64) throw new Error('Loading include tree is too deep');
    entries = structuredClone(entries);
    patches = structuredClone(patches);
    const ids = new Set();
    const reserved = new Set();
    function reserve(items, used = reserved, strict = false) {
      for (const options of items) {
        if (!options || typeof options !== 'object' || Array.isArray(options)) throw new Error('Loading entry must be an object');
        if (options.id !== undefined && options.id !== null && options.id !== '') {
          if (typeof options.id !== 'string' || (strict && used.has(options.id))) throw new Error(`Duplicate or invalid loading entry id: ${options.id}`);
          used.add(options.id);
        }
        if ((options.group || options.name === 'cordis:group') && Array.isArray(options.config)) reserve(options.config, used, strict);
      }
    }
    reserve(entries);
    for (const layer of patches) if (Array.isArray(layer.insert)) reserve(layer.insert);
    function assign(items, parent, final = false) {
      for (const options of items) {
        if (!options || typeof options !== 'object' || Array.isArray(options)
          || typeof options.name !== 'string' || !options.name) throw new Error('Loading entry must declare a module name');
        if (final && ++nodes > 4096) throw new Error('Loading entry count exceeded');
        if (options.id === undefined || options.id === null || options.id === '') {
          const raw = structuredClone(options);
          const index = previousAnonymous.findIndex(record => record.source === source && record.scope === scope
            && record.parent === parent && deepEqual(record.options, raw));
          let id = index < 0 ? undefined : previousAnonymous.splice(index, 1)[0].id;
          while (!id || ids.has(id) || reserved.has(id)) id = randomBytes(8).toString('hex');
          options.id = id;
          if (source !== null) anonymous.push({ source, scope, parent, id, options: raw });
        }
        if (typeof options.id !== 'string' || (final && ids.has(options.id))) throw new Error(`Duplicate or invalid loading entry id: ${options.id}`);
        ids.add(options.id);
        if ((options.group || options.name === 'cordis:group') && Array.isArray(options.config)) assign(options.config, options.id, final);
      }
    }
    // Include assigns raw source identities before applying patches. Otherwise
    // a patch cannot address an anonymous row's already published stable id.
    assign(entries, null);
    for (const layer of patches) if (Array.isArray(layer.insert)) assign(layer.insert, layer.id ?? null);
    const data = entries;
    entries = patch(entries, patches);
    const effectiveIds = new Set();
    reserve(entries, effectiveIds, true);
    for (const id of effectiveIds) reserved.add(id);
    ids.clear();
    assign(entries, null, true);
    trees.set(scope, { id: scope, source, data, patches, effective: entries, parent, patchSources, depth: chain.length });
    async function visit(items, inheritedDisabled = false) {
      return await Promise.all(items.map(async options => {
        const childrenDisabled = inheritedDisabled || options.disabled === true;
        const disabled = !options.group && childrenDisabled;
        const node = { options };
        if (disabled) return node;
        if (options.name === 'cordis:group') {
          if (!Array.isArray(options.config)) throw new Error('A loading group requires an entry array');
          node.children = await visit(options.config, childrenDisabled);
        } else if (options.name === 'cordis:include') {
          if (typeof options.config?.path !== 'string') throw new Error('An include source path must be resolved before admission');
          const parent = pathToFileURL(join(sourceDirectory, source ?? 'cordis.yml'));
          const file = isAbsolute(options.config.path) ? pathToFileURL(options.config.path) : new URL(options.config.path, parent);
          const name = relative(sourceDirectory, fileURLToPath(file)).split(sep).join('/');
          if (name === '..' || name.startsWith('../') || isAbsolute(name)) throw new Error('Include source leaves the captured loading project');
          if (chain.includes(name)) throw new Error(`Loading include cycle: ${[...chain, name].join(' -> ')}`);
          node.source = name;
          const layers = name === base ? overlays : [];
          const localPatches = options.config.patches ?? [];
          if (!Array.isArray(localPatches)) throw new Error('Include patches require an array');
          const document = read(name);
          if (!document && options.config.initial !== undefined) {
            validateEntries(options.config.initial);
            const initial = missing.get(name);
            if (!initial.some(entries => deepEqual(entries, options.config.initial))) initial.push(options.config.initial);
          }
          const layerEntries = layers.map(read);
          if (!document || layerEntries.some(entries => !entries)) return node;
          const patches = [...layerEntries.flat(), ...localPatches];
          const owners = [...layers.flatMap((name, index) => layerEntries[index].map(() => name)), ...localPatches.map(() => null)];
          const childScope = loadingEntryId(scope, options.id);
          node.children = await compile(document, name, childScope, [...chain, name], childrenDisabled,
            patches, { treeId: scope, id: options.id }, owners);
          node.patches = structuredClone(trees.get(childScope).patches);
        } else {
          const artifact = available.get(options.name);
          if (!artifact) throw new Error(`No admitted artifact for loading module: ${options.name}`);
          if (!admitted.has(options.name)) admitted.set(options.name, inspectOriginalPackage(artifact));
          await admitted.get(options.name);
          node.artifact = options.name;
          node.instanceId = loadingEntryId(scope, options.id);
        }
        return node;
      }));
    }
    return await visit(entries, inheritedDisabled);
  }
  const root = { entries: await compile(rootOptions, null, '', []) };
  if (missing.size) throw new LoadingSourcesRequired(missing);
  const artifacts = Object.fromEntries(await Promise.all([...admitted].map(async ([name, pending]) => [name, await pending])));
  const graph = { version: 2, root, artifacts, anonymous, warnings };
  const digest = entries => createHash('sha256').update(JSON.stringify(entries)).digest('hex');
  const activeSources = captured.filter(source => used.has(source.name));
  return { inputSha256: digest(captured), plan: { sourceRoot: sourceDirectory,
    sourceNames: activeSources.map(source => source.name), sourcesSha256: digest(activeSources),
    preparation: { projectDirectory, base, overlays, packages },
    graph, executables: describeExecutables(graph, sourceDirectory) }, trees };
}

/** Stable execution identities shared by preparation and application validation. */
export function describeExecutables(graph, sourceDirectory) {
  const executables = [];
  const instanceIds = new Set();
  function canonical(value) {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])]));
    return value;
  }
  function describe(nodes, source, ancestry) {
    for (const node of nodes) {
      if (node.artifact) {
        if (instanceIds.has(node.instanceId)) throw new Error(`Ambiguous loading instance id: ${node.instanceId}`);
        instanceIds.add(node.instanceId);
        const artifact = graph.artifacts[node.artifact];
        // Config updates belong to the existing Cordis fiber (including volatile refs).
        // Module/context replacement, not ordinary settings, allocates another activation.
        const { config, ...runtimeOptions } = node.options;
        const fingerprint = createHash('sha256').update(JSON.stringify(canonical({ sourceDirectory, source,
          ancestry, options: runtimeOptions, artifact }))).digest('hex');
        executables.push({ instanceId: node.instanceId, pluginId: artifact.name, version: artifact.version, entry: artifact.entry, fingerprint });
      }
      if (node.children) {
        const { config, ...scope } = node.options;
        describe(node.children, node.source ?? source, [...ancestry, { source, scope }]);
      }
    }
  }
  describe(graph.root.entries, null, []);
  return executables;
}
