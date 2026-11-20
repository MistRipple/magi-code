// One ordered, bounded admission queue for configured plugin objects and fibers.
// Receipts are owner facts; no entry, source or lifecycle is mutated here.
const positive = value => Number.isSafeInteger(value) && value > 0;

function cancellable(task, signal) {
  return new Promise((resolve, reject) => {
    const aborted = () => reject(signal.reason ?? new Error('Plugin configuration scope ended'));
    signal.addEventListener('abort', aborted, { once: true });
    Promise.resolve(task).then(resolve, reject).finally(() => signal.removeEventListener('abort', aborted));
    if (signal.aborted) aborted();
  });
}

export class ConfigurationAccess {
  #requestOwner;
  #currentRevision;
  #diagnostic;
  #limit;
  #acceptedRevision = 0;
  #queue = Promise.resolve();
  #pending = 0;

  constructor({ requestOwner, currentRevision, onDiagnostic, pendingLimit = 256 }) {
    if (typeof currentRevision !== 'function' || typeof onDiagnostic !== 'function' || !positive(pendingLimit)) {
      throw new Error('Configuration access requires revision, diagnostics and a pending limit');
    }
    this.#requestOwner = requestOwner;
    this.#currentRevision = currentRevision;
    this.#diagnostic = onDiagnostic;
    this.#limit = pendingLimit;
  }

  #enqueue(scope, action) {
    scope.assertCurrent();
    if (typeof this.#requestOwner !== 'function') throw new Error('Configuration owner service is unavailable');
    if (this.#pending >= this.#limit) throw new Error('Configuration request limit exceeded');
    this.#pending++;
    const task = this.#queue.then(() => { scope.assertCurrent(); return action(); })
      .finally(() => { this.#pending--; });
    // Every rejected action remains rejected for its caller. Subsequent explicit
    // actions can proceed, without retrying or replaying an earlier request.
    this.#queue = task.catch(error => this.#diagnostic({ ...scope.caller, type: 'error', message: error.message }));
    return task;
  }

  edit(scope, operation, { validateResult, onAccepted, expectedRevision } = {}) {
    if (expectedRevision !== undefined && !positive(expectedRevision)) throw new Error('Invalid configuration edit revision');
    // Ordinary operations are snapshotted now; a fiber save may supply a factory
    // to compare against its own most recent accepted save in this same queue.
    const candidate = typeof operation === 'function' ? operation : structuredClone(operation);
    return this.#enqueue(scope, async () => {
      const revision = expectedRevision ?? Math.max(this.#acceptedRevision, this.#currentRevision());
      const receipt = await this.#requestOwner('loading.edit.submit', {
        caller: scope.caller,
        expectedRevision: revision,
        operation: typeof candidate === 'function' ? candidate() : candidate,
      });
      if (!positive(receipt?.revision) || receipt.revision < revision || (validateResult && !validateResult(receipt.result))) {
        throw new Error('Invalid source acceptance receipt');
      }
      this.#acceptedRevision = Math.max(this.#acceptedRevision, receipt.revision);
      onAccepted?.(receipt);
      return receipt;
    });
  }

  changeInstances(scope, expectedRevision, action, bindings, options = {}) {
    const targets = bindings.map(({ executable, activationGeneration }) => ({
      instanceId: executable.instanceId, activationGeneration,
    }));
    const extra = {};
    if (options.entryId !== undefined) extra.entryId = options.entryId;
    if (options.activateTargets !== undefined) {
      extra.activateTargets = options.activateTargets.map(({ executable, activationGeneration }) => ({
        instanceId: executable.instanceId, activationGeneration,
      }));
    }
    const task = this.#enqueue(scope, async () => {
      const receipt = await this.#requestOwner('loading.instances.change', {
        caller: scope.caller, expectedRevision, action, targets, ...extra,
      });
      scope.assertCurrent();
      if (receipt?.revision !== expectedRevision || this.#currentRevision() !== expectedRevision) {
        throw new Error('Loading instance action has no matching graph revision');
      }
    });
    return cancellable(task, scope.signal);
  }

  synchronize(scope, { refresh = false } = {}) {
    if (typeof refresh !== 'boolean') throw new Error('Invalid configuration synchronization option');
    const task = this.#enqueue(scope, async () => {
      const minimumRevision = Math.max(this.#acceptedRevision, this.#currentRevision());
      const receipt = await cancellable(this.#requestOwner('loading.sources.synchronize', {
        caller: scope.caller,
        minimumRevision,
        refresh,
      }), scope.signal);
      scope.assertCurrent();
      if (!positive(receipt?.revision) || receipt.revision < minimumRevision || receipt.revision > this.#currentRevision()) {
        throw new Error('Configuration synchronization has no matching local graph');
      }
      this.#acceptedRevision = Math.max(this.#acceptedRevision, receipt.revision);
      return receipt;
    });
    return cancellable(task, scope.signal);
  }

  restartHost(scope, expectedRevision) {
    if (!positive(expectedRevision)) throw new Error('Invalid host restart revision');
    const task = this.#enqueue(scope, async () => {
      const minimumRevision = Math.max(this.#acceptedRevision, expectedRevision, this.#currentRevision());
      await cancellable(this.#requestOwner('loading.host.restart', {
        caller: scope.caller,
        minimumRevision,
      }), scope.signal);
    });
    return cancellable(task, scope.signal);
  }
}
