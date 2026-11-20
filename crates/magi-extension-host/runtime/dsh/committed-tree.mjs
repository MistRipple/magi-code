// Execution of committed graphs and public ctx.loader service backed by daemon admission.
import { Context, Service } from '@deepseek-ai/cordis';
import { EntryGroup, EntryTree, Group, Loader } from '@deepseek-ai/cordis-plugin-loader';
import { deepEqual } from '@deepseek-ai/cosmokit';
import { dirname, isAbsolute, join, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { describeExecutables, loadingEntryId } from './configuration.mjs';
import { ConfigurationAccess } from './configuration-access.mjs';
import { loadingObjects } from './loading-objects.mjs';

const positive = value => Number.isSafeInteger(value) && value > 0;
const identity = activation => ({ hostGeneration: activation.hostGeneration,
  pluginId: activation.executable.pluginId, instanceId: activation.executable.instanceId,
  activationGeneration: activation.activationGeneration, version: activation.executable.version });

/** One Cordis execution tree; installation/configuration and all generations belong to daemon. */
export class CommittedTree {
  #lifecycle;
  #hostGeneration;
  #diagnostic;
  #loader;
  #loaderFiber;
  #publicLoaderFiber;
  #current;
  #bindings = new Map();
  #trees = new Map();
  #treePlans = new Map();
  #errors = new Map();
  #draining = new Set();
  #sequence = 0;
  #revision = 0;
  #applying = false;
  #closed = false;
  #tainted = false;
  #configuration;

  constructor({ lifecycle, hostGeneration, onDiagnostic, requestOwner, pendingLimit = 256 }) {
    if (!positive(hostGeneration) || typeof onDiagnostic !== 'function') throw new Error('Committed tree requires its owner identity');
    this.#lifecycle = lifecycle;
    this.#hostGeneration = hostGeneration;
    this.#diagnostic = onDiagnostic;
    this.#configuration = new ConfigurationAccess({ requestOwner, currentRevision: () => this.#revision,
      onDiagnostic, pendingLimit });
  }

  #index(configuration) {
    const plans = new Map();
    const modules = new Map();
    const ancestors = new Map();
    function walk(nodes, scope, parents) {
      for (const node of nodes) {
        const id = loadingEntryId(scope, node.options.id);
        if (node.artifact) {
          const artifact = configuration.graph.artifacts[node.artifact];
          if (node.instanceId !== id || !artifact || !isAbsolute(artifact.entry) || modules.has(id)) throw new Error('Invalid committed module binding');
          modules.set(id, artifact);
          ancestors.set(id, parents);
        }
        if (node.source) {
          plans.set(id, node);
          walk(node.children, id, [...parents, id]);
        } else if (node.children) walk(node.children, scope, [...parents, id]);
      }
    }
    walk(configuration.graph.root.entries, '', []);
    return { plans, modules, ancestors };
  }

  async apply({ token, configuration, activations, instanceAction }) {
    if (this.#closed || this.#tainted || this.#applying || token?.hostGeneration !== this.#hostGeneration
      || !positive(token.sequence) || token.sequence <= this.#sequence || !positive(token.revision)
      || token.revision < this.#revision || configuration?.revision !== token.revision
      || !isAbsolute(configuration.sourceRoot ?? '') || configuration.graph?.version !== 2
      || !Array.isArray(configuration.graph.root?.entries) || !Array.isArray(activations)) throw new Error('Stale or invalid committed graph application');
    if (instanceAction != null) {
      const hasCarrier = instanceAction.entryId !== undefined && instanceAction.entryId !== null;
      const activateList = instanceAction.activateTargets ?? [];
      if (!['stop', 'refresh'].includes(instanceAction.kind) || !Array.isArray(instanceAction.targets)
        || !Array.isArray(activateList)) throw new Error('Invalid loading instance action');
      if (hasCarrier) {
        if (instanceAction.kind !== 'refresh' || typeof instanceAction.entryId !== 'string' || !instanceAction.entryId) {
          throw new Error('Invalid carrier instance action');
        }
      } else if (!instanceAction.targets.length || activateList.length) {
        throw new Error('Invalid loading instance action');
      }
    }
    if (!deepEqual(describeExecutables(configuration.graph, configuration.sourceRoot), configuration.executables)) throw new Error('Committed executable descriptors do not match their graph');
    const { plans, modules, ancestors } = this.#index(configuration);
    const desired = new Map();
    for (const activation of activations) {
      const executable = activation.executable;
      const artifact = modules.get(executable?.instanceId);
      if (activation.hostGeneration !== this.#hostGeneration || !positive(activation.activationGeneration)
        || !positive(activation.activationOrder) || !artifact || desired.has(executable.instanceId)
        || executable.pluginId !== artifact.name || executable.version !== artifact.version || executable.entry !== artifact.entry
        || !configuration.executables.some(value => deepEqual(value, executable))) throw new Error('Activation is not part of the committed graph');
      desired.set(executable.instanceId, activation);
      const old = this.#bindings.get(executable.instanceId);
      if (old?.activationGeneration === activation.activationGeneration && !deepEqual(old, activation)) throw new Error('Changed instance reused its activation generation');
    }
    if (desired.size !== modules.size || desired.size !== configuration.executables.length) throw new Error('Committed graph has unsigned instances');
    const runtimeTargets = new Set();
    for (const target of instanceAction?.targets ?? []) {
      const activation = desired.get(target.instanceId);
      if (!activation || activation.activationGeneration !== target.activationGeneration
        || !deepEqual(this.#bindings.get(target.instanceId), activation) || runtimeTargets.has(target.instanceId)) {
        throw new Error('Instance action is outside the committed application');
      }
      runtimeTargets.add(target.instanceId);
    }
    const reactivatedTargets = new Set();
    for (const target of instanceAction?.activateTargets ?? []) {
      const old = this.#bindings.get(target.instanceId);
      const activation = desired.get(target.instanceId);
      if (!old || !activation || old.activationGeneration !== target.activationGeneration
        || activation.activationGeneration <= old.activationGeneration
        || !deepEqual(activation.executable, old.executable)
        || runtimeTargets.has(target.instanceId) || reactivatedTargets.has(target.instanceId)) {
        throw new Error('Reactivated instance action is outside the committed application');
      }
      reactivatedTargets.add(target.instanceId);
    }
    if (instanceAction != null && (!deepEqual(configuration, this.#current)
      || [...desired].some(([id, activation]) => !reactivatedTargets.has(id) && !deepEqual(this.#bindings.get(id), activation)))) {
      throw new Error('Instance action cannot change the committed graph');
    }
    const receipt = () => ({ token, instances: [...desired.values()].map(value => this.#lifecycle.peek(identity(value))) });
    const currentReceipt = () => ({ token, instances: [...this.#bindings.values()].map(value => this.#lifecycle.peek(identity(value))) });
    this.#applying = true;
    this.#sequence = token.sequence;
    this.#revision = token.revision;
    this.#errors.clear();
    try {
      if (instanceAction?.kind === 'refresh') {
        const byId = new Map([...(this.#loader?.entries() ?? [])].map(entry => [entry.id, entry]));
        const carrierId = instanceAction.entryId ?? null;
        if (carrierId !== null) {
          const carrier = byId.get(carrierId);
          const activeValid = [...runtimeTargets].every(id => {
            const state = this.#lifecycle.peek(identity(this.#bindings.get(id))).state;
            const live = byId.get(id);
            return !['stopped', 'failed'].includes(state) && (!live || (live.fiber?.uid != null && !live.disabled));
          });
          const endedValid = [...reactivatedTargets].every(id => {
            const state = this.#lifecycle.peek(identity(this.#bindings.get(id))).state;
            return ['stopped', 'failed'].includes(state) && byId.get(id)?.fiber?.uid == null;
          });
          if (!carrier || carrier.fiber?.uid == null || carrier.disabled || !activeValid || !endedValid) {
            return { ...currentReceipt(), refreshConflict: true };
          }
          for (const id of reactivatedTargets) {
            const old = this.#bindings.get(id);
            this.#draining.add(id);
            await this.#lifecycle.release(identity(old));
            this.#draining.delete(id);
            const next = desired.get(id);
            this.#lifecycle.prepare(identity(next));
            this.#bindings.set(id, next);
          }
          await carrier.update({}, false, true);
          await this.#awaitStructure();
          const refreshedEntries = new Map([...this.#loader.entries()].map(entry => [entry.id, entry]));
          for (const id of reactivatedTargets) {
            const activation = desired.get(id);
            if (this.#lifecycle.peek(identity(activation)).state === 'starting') {
              const blocker = ancestors.get(id).map(parentId => refreshedEntries.get(parentId)?.fiber).find(fiber => fiber?.state === 0);
              if (blocker) { this.#lifecycle.waitPrepared(identity(activation), blocker); continue; }
              const live = refreshedEntries.get(id);
              if (live?.fiber) continue;
              if (live?.disabled || ancestors.get(id).some(parentId => refreshedEntries.get(parentId)?.disabled)) {
                this.#draining.add(id);
                await this.#lifecycle.release(identity(activation));
                this.#draining.delete(id);
                continue;
              }
              await this.#lifecycle.failPrepared(identity(activation), this.#errors.get(activation.executable.pluginId)
                ?? new Error('Committed entry did not create a Cordis fiber'));
            }
          }
          return receipt();
        }
        const entries = [...runtimeTargets].map(id => byId.get(id)).filter(Boolean);
        if (entries.length !== runtimeTargets.size || entries.some(entry => entry.fiber?.uid == null || entry.disabled)) {
          return { ...currentReceipt(), refreshConflict: true };
        }
        for (const entry of entries) await entry.update({}, false, true);
        return receipt();
      }
      for (const [id, old] of this.#bindings) {
        if (!runtimeTargets.has(id) && deepEqual(desired.get(id), old)) continue;
        this.#draining.add(id);
        await this.#lifecycle.release(identity(old));
        this.#draining.delete(id);
      }
      if (instanceAction?.kind === 'stop') return receipt();
      for (const [id, activation] of desired) {
        if (!deepEqual(this.#bindings.get(id), activation)) this.#lifecycle.prepare(identity(activation));
      }
      this.#bindings = desired;
      this.#treePlans = plans;
      this.#current = configuration;
      if (!this.#loader) await this.#startLoader();
      this.#loader.baseUrl = pathToFileURL(join(configuration.sourceRoot, 'cordis.yml')).href;
      await this.#loader.root.update(structuredClone(configuration.graph.root.entries.map(node => node.options)));
      // File content is already committed. Reconcile each live source tree, without reading live files.
      const visited = new Set();
      for (;;) {
        const next = [...this.#trees].find(([id, tree]) => this.#treePlans.has(id) && !visited.has(tree));
        if (!next) break;
        visited.add(next[1]);
        await next[1].reconcile();
      }
      // Imports and tree carriers must be materialized, but business startup
      // may itself request another configuration revision (e.g. enabling a
      // provider). Waiting for every plugin here would prevent that revision
      // from ever being applied. Readiness arrives through lifecycle status.
      await this.#awaitStructure();
      const entries = new Map([...this.#loader.entries()].map(entry => [entry.id, entry]));
      for (const activation of desired.values()) {
        const instanceId = activation.executable.instanceId;
        if (this.#lifecycle.peek(identity(activation)).state === 'starting') {
          const blocker = ancestors.get(instanceId)
            .map(id => entries.get(id)?.fiber).find(fiber => fiber?.state === 0);
          if (blocker) {
            this.#lifecycle.waitPrepared(identity(activation), blocker);
            continue;
          }
          const live = entries.get(instanceId);
          if (live?.fiber) continue;
          if (live?.disabled || ancestors.get(instanceId).some(id => entries.get(id)?.disabled)) {
            this.#draining.add(instanceId);
            await this.#lifecycle.release(identity(activation));
            this.#draining.delete(instanceId);
            continue;
          }
          await this.#lifecycle.failPrepared(identity(activation), this.#errors.get(activation.executable.pluginId)
            ?? new Error('Committed entry did not create a Cordis fiber'));
        }
      }
      return receipt();
    } catch (error) { this.#tainted = true; throw error; }
    finally { this.#applying = false; }
  }

  get requiresRestart() { return this.#tainted; }

  #configurationScope(activation, context) {
    const callerFiber = context.fiber, uid = callerFiber.uid;
    const lease = this.#lifecycle.acquireConfiguration(identity(activation), context);
    const assertCurrent = () => {
      lease.assertCurrent();
      callerFiber.assertActive();
      if (this.#closed || !deepEqual(this.#bindings.get(activation.executable.instanceId), activation)
        || callerFiber.uid !== uid) throw new Error('Configuration request belongs to an inactive instance');
    };
    assertCurrent();
    return Object.freeze({ assertCurrent, signal: lease.signal, epoch: lease.epoch,
      caller: Object.freeze({ ...identity(activation), configurationEpoch: lease.epoch }) });
  }

  #carrierExecutables(entryId) {
    function collectGroup(nodes, out) {
      for (const node of nodes) {
        if (typeof node.instanceId === 'string') out.push(node.instanceId);
        else if (node.source === undefined && node.options?.group === true && Array.isArray(node.children)) {
          collectGroup(node.children, out);
        }
      }
    }
    function find(nodes, scope) {
      for (const node of nodes) {
        const id = loadingEntryId(scope, node.options.id);
        if (id === entryId) {
          if (node.artifact || !Array.isArray(node.children)) return null;
          const out = [];
          if (node.source === undefined && node.options?.group === true) collectGroup(node.children, out);
          return out;
        }
        if (Array.isArray(node.children)) {
          const found = find(node.children, node.source !== undefined ? id : scope);
          if (found) return found;
        }
      }
      return null;
    }
    const ids = find(this.#current?.graph?.root?.entries ?? [], '');
    if (!ids) throw new Error('Structural carrier is not active in the committed configuration');
    const targets = [], activateTargets = [];
    for (const id of ids) {
      const binding = this.#bindings.get(id);
      if (!binding) throw new Error('Committed group executable has no active binding');
      const state = this.#lifecycle.peek(identity(binding)).state;
      if (['stopped', 'failed'].includes(state)) activateTargets.push(binding);
      else targets.push(binding);
    }
    return { targets, activateTargets };
  }

  /** Scoped configuration access backing both fiber saves and public ctx.loader handles. */
  configuration(context) {
    const id = this.#loader?.locate(context.fiber);
    const activation = this.#bindings.get(id);
    if (!activation) throw new Error('Configuration caller has no committed plugin identity');
    const scope = this.#configurationScope(activation, context);
    return Object.freeze({ assertCurrent: scope.assertCurrent, signal: scope.signal,
      edit: (operation, options) => this.#configuration.edit(scope, operation, options),
      activate: (entry, expectedRevision, action = 'activate') => {
        const target = this.#bindings.get(entry.id);
        if (!target) throw new Error('Entry has no admitted executable');
        return this.#configuration.changeInstances(scope, expectedRevision, action, [target]);
      },
      forceUpdate: (entry, expectedRevision) => {
        const target = this.#bindings.get(entry.id);
        if (target) {
          return this.#configuration.changeInstances(scope, expectedRevision,
            entry.fiber?.uid == null ? 'activate' : 'refresh', [target]);
        }
        const { targets, activateTargets } = this.#carrierExecutables(entry.id);
        return this.#configuration.changeInstances(scope, expectedRevision, 'refresh', targets, {
          entryId: entry.id, activateTargets,
        });
      },
      stop: ({ id, includeEntry }, expectedRevision) => {
        const { ancestors } = this.#index(this.#current);
        const targets = [...this.#bindings.values()].filter(({ executable }) =>
          id === null || (includeEntry && executable.instanceId === id) || ancestors.get(executable.instanceId).includes(id));
        return targets.length
          ? this.#configuration.changeInstances(scope, expectedRevision, 'stop', targets)
          : Promise.resolve();
      },
      synchronize: options => this.#configuration.synchronize(scope, options),
      restartHost: expectedRevision => this.#configuration.restartHost(scope, expectedRevision) });
  }

  /** Host-internal read-only snapshot of live tree entries for host services (e.g. pluginManager). */
  liveEntries() {
    if (!this.#loader) return [];
    const items = [];
    for (const entry of this.#loader.entries()) {
      items.push(Object.freeze({
        id: entry.id,
        rawId: typeof entry.options?.id === 'string' ? entry.options.id : '',
        moduleName: typeof entry.options?.name === 'string' ? entry.options.name : '',
        disabled: Boolean(entry.disabled),
      }));
    }
    return items;
  }

  /** Host-internal fiber instance lookup without constructing a loadingObjects view. */
  locateFiber(fiber) {
    return this.#loader?.locate(fiber);
  }

  /** Host-internal snapshot for MagiConfigEditor / SettingsForms. */
  configEditorSnapshot() {
    if (!this.#loader || !this.#current) {
      return { revision: this.#revision, sourceRoot: '', sources: [], entries: [] };
    }
    const candidates = [...this.#loader.entries()].filter(
      entry => entry.parent?.tree?.ctx?.fiber?.entry?.id === 'include',
    );
    const counts = new Map();
    for (const entry of candidates) {
      counts.set(entry.options.id, (counts.get(entry.options.id) ?? 0) + 1);
    }
    return {
      revision: this.#revision,
      sourceRoot: this.#current.sourceRoot,
      sources: this.#current.sources ?? [],
      entries: candidates.filter(entry => counts.get(entry.options.id) === 1),
    };
  }

  /** Host-internal snapshot for PluginInventoryService (`pluginInventory/list`). */
  inventorySnapshot() {
    if (!this.#loader) return { entries: [], packages: [], artifacts: {} };
    const entries = [];
    for (const entry of this.#loader.entries()) {
      if (entry.options?.group === true || ['cordis:group', 'cordis:include'].includes(entry.options?.name)) continue;
      const state = entry.fiber?.state;
      const fiberPhase = state === 0 ? 'pending'
        : state === 1 ? 'loading'
          : state === 2 ? 'active'
            : state === 3 ? 'failed'
              : state === 5 ? 'unloading' : null;
      let diagnostic;
      if (fiberPhase === 'failed') {
        const activation = this.#bindings.get(entry.id);
        const snapError = activation ? this.#lifecycle.peek(identity(activation)).error : undefined;
        diagnostic = snapError ?? (entry.fiber?._error instanceof Error ? entry.fiber._error.message : undefined);
      }
      entries.push({
        entryId: entry.id,
        moduleName: typeof entry.options?.name === 'string' ? entry.options.name : '',
        enabled: !entry.disabled,
        fiberPhase,
        ...(typeof diagnostic === 'string' && diagnostic ? { diagnostic } : {}),
      });
    }
    return {
      entries,
      packages: this.#current?.preparation?.packages ?? [],
      artifacts: this.#current?.graph?.artifacts ?? {},
    };
  }

  /** Context-bound projection over the live execution tree. */
  objects(context, { resolvePublicLoader } = {}) {
    return loadingObjects({ loader: this.#loader.root.tree, context, access: this.configuration(context),
      revision: () => this.#current.revision, resolvePublicLoader });
  }

  async #awaitStructure() {
    for (;;) {
      const tasks = [...this.#loader.entries()].flatMap(entry => [entry._initTask,
        ['cordis:include', 'cordis:group'].includes(entry.options.name) ? entry.fiber?.inertia : undefined]).filter(Boolean);
      if (!tasks.length) return;
      await Promise.all(tasks);
    }
  }

  async ensurePublicLoader() {
    if (!this.#loader) await this.#startLoader();
  }

  async #startLoader() {
    const engine = this;
    const root = this.#lifecycle.context;
    let publicScope;
    const privateContext = root.isolate('loader');
    const importArtifact = name => {
      const artifact = engine.#current.graph.artifacts[name];
      if (!artifact) throw new Error(`Module is not in the committed installation: ${name}`);
      return import(pathToFileURL(artifact.entry).href).catch(error => { engine.#errors.set(name, error); throw error; });
    };

    class CommittedInclude extends EntryTree {
      static inject = ['loader'];
      static [EntryGroup.key] = true;
      constructor(ctx, config) {
        super(ctx);
        this.setSource(config);
        engine.#trees.set(ctx.fiber.entry.id, this);
        ctx.on('internal/update', config => this.reconcile(config));
      }
      setSource(config) {
        const plan = engine.#treePlans.get(this.ctx.fiber.entry.id);
        if (!plan) throw new Error('Source tree is not in the committed revision');
        // Use the source selected during admission. Re-resolving raw options
        // here can select a different file or misinterpret path delimiters.
        this.filename = join(engine.#current.sourceRoot, plan.source);
        this.ctx.baseUrl = pathToFileURL(dirname(this.filename) + sep).href;
        this.config = { ...config, patches: plan.patches };
        this.enableLogs = config.enableLogs ?? this.ctx.fiber.entry.parent.tree.enableLogs ?? false;
        return plan;
      }
      async *[Service.init]() {
        yield async () => {
          if (engine.#trees.get(this.ctx.fiber.entry.id) === this) engine.#trees.delete(this.ctx.fiber.entry.id);
          const fibers = [...this.entries()].map(entry => entry.fiber).filter(Boolean);
          this.root.stop();
          await Promise.all(fibers.map(fiber => fiber.dispose()));
        };
        await this.reconcile();
      }
      async reconcile(config = this.config) {
        const plan = this.setSource(config);
        await this.root.update(structuredClone(plan.children.map(node => node.options)));
      }
      import(name, getOuterStack) {
        if (name.startsWith('cordis:')) return super.import(name, getOuterStack);
        return importArtifact(name);
      }
      commit(change) { engine.#observeMutation(this, change); }
    }

    class ExecutionLoader extends Loader {
      constructor(ctx) {
        super(ctx, { baseUrl: pathToFileURL(join(engine.#current?.sourceRoot ?? process.cwd(), 'cordis.yml')).href });
        this.builtins.group = Group;
        this.builtins.include = CommittedInclude;
        ctx.on('loader/entry-init', entry => {
          const init = entry.init;
          entry.init = async function () {
            const activation = engine.#bindings.get(this.id);
            // A failed or stopped generation remains in the committed graph.
            // Cordis group reconciliation cannot authorize a new startup.
            if (activation && ['failed', 'stopped'].includes(engine.#lifecycle.peek(identity(activation)).state)) return;
            if (activation && !this.disabled) engine.#lifecycle.markActivating(identity(activation));
            try {
              await init.call(this);
            } finally {
              if (activation) engine.#lifecycle.finishInit(identity(activation));
            }
          };
        });
        ctx.on('internal/plugin', fiber => {
          if (fiber.uid === null || !fiber.entry || fiber.parent.fiber?.entry === fiber.entry) return;
          const activation = engine.#bindings.get(fiber.entry.id);
          if (!activation) return;
          // The execution loader is private. Plugins receive the application's public service scope.
          fiber.ctx[Context.isolate] = Object.assign(Object.create(fiber.ctx[Context.isolate]), { loader: publicScope });
          engine.#lifecycle.adopt(identity(activation), fiber, true);
          engine.#ownConfigurationSave(activation, fiber);
        });
      }
      async *[Service.init]() {
        yield async () => {
          const fibers = [...this.entries()].map(entry => entry.fiber).filter(Boolean);
          this.root.stop();
          await Promise.all(fibers.map(fiber => fiber.dispose()));
        };
      }
      commit(change) { engine.#observeMutation(this, change); }
      import(name, getOuterStack) {
        return name.startsWith('cordis:') ? super.import(name, getOuterStack) : importArtifact(name);
      }
    }
    const views = new WeakMap();
    function viewFor(receiver) {
      const caller = root.reflect.trace(receiver.ctx);
      let cached = views.get(caller);
      if (!cached || cached.access.signal.aborted) {
        const access = engine.configuration(caller);
        const objects = loadingObjects({
          loader: engine.#loader.root.tree,
          context: caller,
          access,
          revision: () => engine.#current.revision,
          resolvePublicLoader: () => caller.get('loader') ?? objects,
        });
        cached = { access, objects };
        views.set(caller, cached);
      }
      return cached.objects;
    }

    class PublicLoader extends Service {
      constructor(ctx) {
        super(ctx, 'loader');
      }
      [Service.check]() {
        const config = Service.prototype[Service.resolveConfig].call(this);
        if (config.await && engine.#loader?.getTasks().length) return false;
        return true;
      }
      static {
        for (const key of [
          'root', 'store', 'context', 'enableLogs', 'filename', 'config', 'envData',
          'entries', 'resolve', 'resolveGroup', 'getTasks', 'await', 'ensureId',
          'create', 'update', 'remove', 'commit', 'import', 'flush', 'refresh',
          'locate', 'unwrapExports', 'exit',
        ]) {
          Object.defineProperty(this.prototype, key, {
            enumerable: true,
            get() {
              const caller = root.reflect.trace(this.ctx);
              if (key === 'await' && !engine.#lifecycle.hasActivation(caller)) {
                return () => Promise.resolve();
              }
              return viewFor(this)[key];
            },
          });
        }
      }
    }

    this.#publicLoaderFiber = root.plugin(PublicLoader);
    await this.#publicLoaderFiber.await();
    publicScope = root[Context.isolate].loader;
    this.#loaderFiber = privateContext.plugin(ExecutionLoader);
    await this.#loaderFiber.await();
    this.#loader = privateContext.loader;
    privateContext.on('internal/service', name => {
      if (name === 'loader') root.reflect.notify(['loader']);
    });
  }

  #ownConfigurationSave(activation, fiber) {
    const engine = this;
    const update = fiber.update;
    let lastAcceptedSave;
    Object.defineProperty(fiber, 'update', { configurable: true, value(config, noSave = false) {
      // Explicit instance-local updates retain Cordis semantics. They do not
      // change a source, the accepted graph, installation or enabled intent.
      if (noSave) return update.call(this, config, true);
      const currentFiber = this.ctx.fiber;
      const scope = engine.#configurationScope(activation, this.ctx);
      if (currentFiber.uid !== fiber.uid) throw new Error('Configuration save belongs to another fiber');
      currentFiber.resolveConfig(config);
      const schema = currentFiber.runtime?.Config;
      const raw = schema?.simplify ? schema.simplify(config) : config;
      // Capture the proposal now. Mutating a plugin-owned object while a prior
      // save is in flight cannot change the queued source operation.
      const options = structuredClone({ config: raw ?? null });
      const entry = fiber.entry;
      const entryId = entry.id, localId = entry.options.id;
      const observedRevision = engine.#revision;
      const observedConfig = structuredClone(entry.options.config ?? null);
      const treeId = entry.parent.tree.ctx.fiber.entry?.id ?? '';
      if (treeId === '' && entry.parent.tree.root !== engine.#loader.root) throw new Error('Configuration save has no loading owner');
      return engine.#configuration.edit(scope, () => {
        const expectedConfig = lastAcceptedSave?.epoch === scope.epoch && lastAcceptedSave.revision >= observedRevision
          ? lastAcceptedSave.config : observedConfig;
        return { kind: 'update', treeId, id: localId, expectedConfig, options };
      }, { validateResult: result => result === entryId,
        onAccepted: receipt => { lastAcceptedSave = { epoch: scope.epoch, revision: receipt.revision, config: options.config }; },
      }).then(() => undefined);
    } });
  }

  #observeMutation(tree, change) {
    const prefix = tree.ctx.fiber.entry?.id;
    const id = prefix ? `${prefix}:${change.id}` : change.id;
    if (this.#closed) return;
    const activation = this.#bindings.get(id);
    const live = tree.store[change.id];
    // The upstream Loader sets disabled before reporting self-disposal. Keep
    // that signal in the lifecycle and restore the accepted source options.
    if (activation && live && live.options === change.options && change.legacy && !live.fiber?.uid) {
      if (Object.hasOwn(change.legacy, 'disabled')) live.options.disabled = change.legacy.disabled;
      else delete live.options.disabled;
      if (!this.#draining.has(id)) this.#lifecycle.observeSelfDisposal(identity(activation));
    }
    if (this.#draining.has(id)) return;
    this.#diagnostic({ hostGeneration: this.#hostGeneration, type: 'warning', instanceId: id,
      message: 'Runtime entry changed locally; canonical loading configuration was not changed' });
  }

  async close() {
    if (this.#closed) return;
    this.#closed = true;
    const results = await Promise.allSettled([...this.#bindings.values()].map(value => this.#lifecycle.release(identity(value))));
    await this.#loaderFiber?.dispose();
    await this.#publicLoaderFiber?.dispose();
    await this.#lifecycle.settled();
    const failed = results.filter(result => result.status === 'rejected');
    if (failed.length) throw new AggregateError(failed.map(result => result.reason), 'Committed tree cleanup failed');
  }
}
