import { Context } from '@deepseek-ai/cordis';

const owner = Symbol('magi.plugin.activation');
const ownedFiber = Symbol('magi.plugin.fiber');
const states = ['waiting', 'starting', 'running', 'failed', 'stopped', 'stopping'];
const positive = (value) => Number.isSafeInteger(value) && value > 0;

function serviceReady(fiber, name) {
  const impl = fiber.ctx.reflect._getImpl(name, true);
  if (!impl) return false;
  if (!impl.check) return true;
  try { return Boolean(impl.check.call(fiber.ctx.reflect.trace(impl.value))); }
  catch { return false; }
}

function missingServices(fiber) {
  const missing = new Set();
  // A source/group can suspend before creating a descendant's fiber. Project
  // the actual framework dependency chain, rather than failing that descendant.
  while (fiber && fiber.uid !== 0) {
    for (const name of Object.keys(fiber.inject)) {
      if (!serviceReady(fiber, name)) missing.add(name);
    }
    fiber = fiber.parent.fiber;
  }
  return [...missing].sort();
}

/** Shared Cordis tree. Installation, enabled state and generation allocation belong to daemon. */
export class PluginLifecycle {
  #context = new Context();
  #plugins = new Map();
  #lastGeneration = new Map();
  #lastStatus = new Map();
  #hostGeneration;
  #closed = false;
  #cleanupErrors = [];
  #failureTasks = new Set();
  #releaseStatus;
  #releaseLogger;
  #onStatus;

  constructor({ hostGeneration, onStatus, onDiagnostic }) {
    if (!positive(hostGeneration) || typeof onStatus !== 'function' || typeof onDiagnostic !== 'function') {
      throw new TypeError('Host generation and status/diagnostic sinks are required');
    }
    this.#hostGeneration = hostGeneration;
    this.#onStatus = status => { this.#lastStatus.set(status.instanceId, status); onStatus(status); };
    this.#releaseStatus = this.#context.on('internal/status', (fiber) => {
      const record = fiber.ctx[owner];
      if (!record || this.#plugins.get(record.instanceId) !== record) return;
      // Service loss also invalidates calls, even when Cordis later resumes the same fiber.
      // Disposal clears uid before its final status notification. Preserve the
      // adopted uid on the context, including across framework proxy objects.
      const root = Object.hasOwn(fiber.ctx, ownedFiber) && fiber.ctx[ownedFiber] === record.fiberUid;
      if (root) record.activating = fiber.state === 1 && !record.stopping && !record.failed;
      if (root && ![1, 2].includes(fiber.state)) record.configurationCalls.abort(new Error('Plugin configuration scope ended'));
      if (root && [1, 2].includes(fiber.state) && record.configurationCalls.signal.aborted && !record.stopping) {
        if (!positive(record.configurationEpoch + 1)) throw new Error('Plugin configuration epoch exhausted');
        record.configurationEpoch++;
        record.configurationCalls = new AbortController();
      }
      if (root && fiber.state !== 2) record.calls.abort(new Error('Plugin activation is not running'));
      if (root && fiber.state === 2 && record.calls.signal.aborted && !record.stopping) {
        if (!positive(record.callEpoch + 1)) throw new Error('Plugin call epoch exhausted');
        record.callEpoch++;
        record.calls = new AbortController();
      }
      // A bundle may start children without awaiting them. Their startup failure
      // invalidates that package, rather than leaving its parent falsely ready.
      if (fiber.state === 3 && !record.failed) {
        record.failed = true;
        record.activating = false;
        record.calls.abort(new Error('Plugin startup failed'));
        record.configurationCalls.abort(new Error('Plugin startup failed'));
        const failed = this.#snapshot(record);
        const task = fiber.await().catch(async error => {
          record.error = error.message;
          this.#onStatus({ ...failed, state: 'failed', error: error.message });
          await this.#stop(record);
        });
        this.#failureTasks.add(task);
        void task.then(() => this.#failureTasks.delete(task), error => {
          this.#failureTasks.delete(task);
          this.#cleanupErrors.push(error);
        });
      }
      this.#onStatus(this.#snapshot(record));
    });
    this.#releaseLogger = this.#context.logger.exporter({
      export: (message) => {
        const fiber = message.fiber?.deref();
        const record = fiber?.ctx[owner];
        // Cordis reports disposer exceptions through its logger. Do not report successful cleanup.
        if (message.type === 'error' && fiber?.state === 5) {
          this.#cleanupErrors.push(new Error(`Plugin cleanup failed: ${record?.pluginId ?? 'host'}`, { cause: message.args[0] }));
        }
        onDiagnostic({
          hostGeneration, pluginId: record?.pluginId, activationGeneration: record?.activationGeneration,
          instanceId: record?.instanceId,
          type: message.type, message: message.args.map(value => value instanceof Error ? value.message : String(value)).join(' '),
        });
      },
    });
  }

  #assertHost(hostGeneration) {
    if (this.#closed || hostGeneration !== this.#hostGeneration || this.#cleanupErrors.length) {
      throw new Error('Plugin host is closed, stale or requires restart');
    }
  }

  #record(identity) {
    this.#assertHost(identity.hostGeneration);
    const record = this.#plugins.get(identity.instanceId);
    if (!record || record.pluginId !== identity.pluginId || record.activationGeneration !== identity.activationGeneration || record.stopping || record.failed) {
      throw new Error('Plugin activation is stale or stopping');
    }
    return record;
  }

  #snapshot(record) {
    const fiber = record.fiber;
    const suspended = record.blocker || (record.treeManaged && !record.selfDisposed
      && fiber?.state === 4 && !fiber.entry?.disabled);
    return Object.freeze({
      hostGeneration: this.#hostGeneration, pluginId: record.pluginId,
      instanceId: record.instanceId,
      activationGeneration: record.activationGeneration, version: record.version,
      configurationEpoch: record.configurationEpoch,
      callEpoch: record.callEpoch,
      activating: Boolean(record.activating && !record.stopping && !record.failed && !suspended),
      state: record.failed ? 'failed' : record.stopping ? 'stopping' : suspended ? 'waiting' : states[fiber?.state ?? 1],
      missingServices: suspended || fiber?.state === 0 ? missingServices(fiber ?? record.blocker) : [],
      ...(record.error ? { error: record.error } : {}),
    });
  }

  snapshot(identity) { return this.#snapshot(this.#record(identity)); }

  get context() { return this.#context; }

  get generation() { return this.#hostGeneration; }

  /** Allocate no identities here: the owner supplies package and configured-instance generations. */
  prepare({ hostGeneration, pluginId, instanceId, activationGeneration, version }) {
    this.#assertHost(hostGeneration);
    if (typeof pluginId !== 'string' || !pluginId || typeof instanceId !== 'string' || !instanceId
      || typeof version !== 'string' || !version || !positive(activationGeneration)
      || activationGeneration <= (this.#lastGeneration.get(instanceId) ?? 0)) throw new Error('Invalid plugin identity or reused activation generation');
    if (this.#plugins.has(instanceId)) throw new Error('Plugin must finish deactivation before replacement');
    this.#lastGeneration.set(instanceId, activationGeneration);
    const record = { pluginId, instanceId, activationGeneration, version, calls: new AbortController(),
      callEpoch: 1, configurationCalls: new AbortController(), configurationEpoch: 1, activating: false, stopping: false };
    this.#plugins.set(instanceId, record);
    this.#onStatus(this.#snapshot(record));
  }

  markActivating(identity) {
    const record = this.#record(identity);
    if (record.activating) return;
    record.activating = true;
    this.#onStatus(this.#snapshot(record));
  }

  finishInit(identity) {
    const record = this.#plugins.get(identity.instanceId);
    if (!record || record.activationGeneration !== identity.activationGeneration || record.stopping || record.failed) return;
    if (record.activating && record.fiber?.state !== 1) {
      record.activating = false;
      this.#onStatus(this.#snapshot(record));
    }
  }

  /** Attach the real Cordis fiber created by the shared tree, using the same resource owner. */
  adopt(identity, fiber, treeManaged = false) {
    const record = this.#record(identity);
    if (record.fiber && record.fiber.uid !== fiber.uid && !(record.treeManaged && record.fiber.uid === null)) {
      throw new Error('Activation already owns a fiber');
    }
    record.fiber = fiber;
    record.fiberUid = fiber.uid;
    record.treeManaged = treeManaged;
    record.blocker = undefined;
    if (![1, 2].includes(fiber.state)) record.configurationCalls.abort(new Error('Plugin configuration scope is not active'));
    Object.defineProperty(fiber.ctx, owner, { value: record, configurable: true });
    Object.defineProperty(fiber.ctx, ownedFiber, { value: record.fiberUid, configurable: true });
    this.#onStatus(this.#snapshot(record));
  }

  waitPrepared(identity, blocker) {
    const record = this.#record(identity);
    if (record.fiber || blocker.state !== 0) throw new Error('Expected a pending ancestor of an uncreated instance');
    record.activating = false;
    record.blocker = blocker;
    this.#onStatus(this.#snapshot(record));
  }

  observeSelfDisposal(identity) {
    const record = this.#plugins.get(identity.instanceId);
    if (record?.activationGeneration !== identity.activationGeneration || record.stopping) return;
    record.selfDisposed = true;
    record.activating = false;
    record.calls.abort(new Error('Plugin stopped itself'));
    record.configurationCalls.abort(new Error('Plugin stopped itself'));
  }

  async failPrepared(identity, error) {
    const record = this.#record(identity);
    record.failed = true;
    record.activating = false;
    record.error = error.message;
    record.calls.abort(error);
    record.configurationCalls.abort(error);
    this.#onStatus({ ...this.#snapshot(record), error: error.message });
    await this.#stop(record);
  }

  peek(identity) {
    this.#assertHost(identity.hostGeneration);
    const status = this.#lastStatus.get(identity.instanceId);
    if (!status || status.pluginId !== identity.pluginId || status.activationGeneration !== identity.activationGeneration) throw new Error('Plugin activation is stale');
    const record = this.#plugins.get(identity.instanceId);
    return record ? this.#snapshot(record) : status;
  }

  async release(identity) {
    const status = this.peek(identity);
    if (this.#plugins.has(identity.instanceId)) await this.#stop(this.#plugins.get(identity.instanceId));
    return status;
  }

  get requiresRestart() { return this.#cleanupErrors.length > 0; }

  /** The loader passes the unmodified upstream export and daemon's committed config. */
  async activate({ hostGeneration, pluginId, instanceId, activationGeneration, version, plugin, entry, config }) {
    const identity = { hostGeneration, pluginId, instanceId, activationGeneration };
    this.prepare({ ...identity, version });
    const record = this.#record(identity);
    try {
      record.activating = true;
      this.#onStatus(this.#snapshot(record));
      if (entry !== undefined) {
        if (plugin !== undefined || new URL(entry).protocol !== 'file:') throw new Error('Expected an immutable local package entry');
        const exported = await import(entry);
        this.#record(identity);
        plugin = exported.default ?? exported;
      }
      record.fiber = this.#context.extend({ [owner]: record }).plugin(plugin, structuredClone(config));
      this.adopt(identity, record.fiber);
      this.finishInit(identity);
      await record.fiber.await();
      this.#record(identity);
      this.#checkCleanup();
      return this.#snapshot(record);
    } catch (error) {
      await this.#stop(record);
      throw error;
    }
  }

  /** A lease cannot become valid again after dependency loss or reactivation. */
  acquire(identity) {
    const record = this.#record(identity);
    if (record.fiber?.state !== 2) throw new Error('Plugin is not running');
    const signal = record.calls.signal;
    return Object.freeze({ signal, epoch: record.callEpoch, assertCurrent: () => {
      signal.throwIfAborted();
      if (this.#record(identity) !== record || record.fiber.state !== 2) throw new Error('Plugin call is stale');
    } });
  }

  /** Source saves are valid during startup, but cannot outlive an unload or replacement. */
  acquireConfiguration(identity, callerContext) {
    const record = this.#record(identity);
    if (callerContext && callerContext[owner] !== record) throw new Error('Configuration caller is not owned by this plugin');
    const signal = record.configurationCalls.signal;
    const assertCurrent = () => {
      signal.throwIfAborted();
      if (this.#record(identity) !== record || ![1, 2].includes(record.fiber?.state)) throw new Error('Plugin configuration scope is not active');
      if (callerContext && ![1, 2].includes(callerContext.fiber.state)) throw new Error('Configuration caller is not active');
    };
    assertCurrent();
    return Object.freeze({ signal, assertCurrent, epoch: record.configurationEpoch });
  }

  /** Bind a registered contribution to the plugin and actual registering fiber. */
  hasActivation(context) {
    return Boolean(context?.[owner]);
  }

  ownerIdentity(context) {
    const record = context?.[owner];
    if (!record) return undefined;
    return Object.freeze({
      hostGeneration: this.#hostGeneration,
      pluginId: record.pluginId,
      instanceId: record.instanceId,
      activationGeneration: record.activationGeneration,
      version: record.version,
    });
  }

  /** Admit any live plugin page lease or host-page lease for shared host HTTP/WS channels. */
  acquireAnyInstance(instanceId, generation, epoch) {
    this.#assertHost(this.#hostGeneration);
    if (instanceId === '$host') {
      if (generation !== this.#hostGeneration || epoch !== 1) throw new Error('Stale host route lease');
      return Object.freeze({ signal: new AbortController().signal });
    }
    const record = this.#plugins.get(instanceId);
    if (!record || record.stopping || record.failed || record.activationGeneration !== generation) {
      throw new Error('Plugin activation is stale or stopping');
    }
    const lease = this.acquire({
      hostGeneration: this.#hostGeneration,
      pluginId: record.pluginId,
      instanceId: record.instanceId,
      activationGeneration: record.activationGeneration,
    });
    if (lease.epoch !== epoch) throw new Error('Plugin call epoch is stale');
    return lease;
  }

  contributionScope(context) {
    const record = context[owner];
    if (!record) throw new Error('Contribution caller has no plugin activation');
    const identity = Object.freeze({ hostGeneration: this.#hostGeneration, pluginId: record.pluginId,
      instanceId: record.instanceId, activationGeneration: record.activationGeneration, version: record.version });
    const registration = this.acquireConfiguration(identity, context);
    return Object.freeze({ identity, acquire: () => {
      registration.assertCurrent();
      return this.acquire(identity);
    } });
  }

  #checkCleanup() {
    if (this.#cleanupErrors.length) throw new AggregateError(this.#cleanupErrors, 'Plugin host requires restart after cleanup failure');
  }

  async #stop(record) {
    record.stopping = true;
    record.activating = false;
    record.calls.abort(new Error('Plugin activation ended'));
    record.configurationCalls.abort(new Error('Plugin activation ended'));
    record.stop ??= (async () => {
      await record.fiber?.dispose();
      if (this.#plugins.get(record.instanceId) === record) this.#plugins.delete(record.instanceId);
      if (!record.failed) this.#onStatus({ ...this.#snapshot(record), state: 'stopped' });
      this.#checkCleanup();
    })();
    return record.stop;
  }

  async deactivate(identity) { await this.#stop(this.#record(identity)); }

  /** Await framework transitions, including dependent unload/reload; no timers or polling. */
  async settled() {
    for (;;) {
      const pending = [...this.#context.registry.values()].flatMap(runtime => [...runtime.fibers])
        .map(fiber => fiber.inertia).filter(Boolean);
      pending.push(...this.#failureTasks);
      if (!pending.length) break;
      await Promise.all(pending);
    }
    this.#checkCleanup();
  }

  async close() {
    if (this.#closed) return;
    this.#closed = true;
    const results = await Promise.allSettled([...this.#plugins.values()].map(record => this.#stop(record)));
    this.#releaseStatus();
    this.#releaseLogger();
    const errors = results.filter(result => result.status === 'rejected').map(result => result.reason);
    if (errors.length) throw new AggregateError(errors, 'Plugin host shutdown failed');
  }
}
