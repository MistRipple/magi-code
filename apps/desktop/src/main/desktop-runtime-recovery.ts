export interface DesktopRuntimeRecoveryCallbacks {
  hasWindow: () => boolean;
  waitForHostReady: (
    connectionRevision: number,
    expectedGeneration: number,
    signal: AbortSignal,
  ) => Promise<boolean>;
  restoreAfterDaemonReady: () => Promise<void>;
  notifyBrowserRuntimeReady: () => void;
  onFailure: (cause: unknown) => void;
}

/**
 * 收口 Desktop Control、daemon 注册和窗口创建之间的启动竞态。
 *
 * 三个事件没有固定到达顺序，只有它们同时满足时才允许发布浏览器 ready：
 * Control WebSocket 已连接、daemon 已注册当前代次、至少一个桌面窗口已存在。
 */
export class DesktopRuntimeRecoveryCoordinator {
  readonly #callbacks: DesktopRuntimeRecoveryCallbacks;
  #connected = false;
  #connectionRevision = 0;
  #registeredGeneration: number | null = null;
  #recovery: Promise<void> | null = null;
  #recoveryAbort: AbortController | null = null;
  #scheduledKey: string | null = null;
  #closed = false;

  constructor(callbacks: DesktopRuntimeRecoveryCallbacks) {
    this.#callbacks = callbacks;
  }

  onControlConnection(connected: boolean): number {
    if (this.#closed) return this.#connectionRevision;
    this.#connected = connected;
    const revision = ++this.#connectionRevision;
    if (!connected) {
      this.#abortRecovery();
      this.#scheduledKey = null;
      return revision;
    }
    this.#scheduleIfReady();
    return revision;
  }

  onDaemonRegistered(generation: number): void {
    if (this.#closed) return;
    this.#registeredGeneration = generation;
    this.#scheduleIfReady();
  }

  onDaemonUnregistered(): void {
    if (this.#closed) return;
    this.#registeredGeneration = null;
    this.#abortRecovery();
    this.#scheduledKey = null;
  }

  onWindowAvailable(): void {
    if (this.#closed) return;
    this.#scheduleIfReady();
  }

  shutdown(): void {
    this.#closed = true;
    this.#connected = false;
    this.#registeredGeneration = null;
    this.#scheduledKey = null;
    this.#abortRecovery();
  }

  #scheduleIfReady(): void {
    if (
      this.#closed
      || !this.#connected
      || this.#registeredGeneration === null
      || !this.#callbacks.hasWindow()
    ) return;

    const connectionRevision = this.#connectionRevision;
    const expectedGeneration = this.#registeredGeneration;
    const key = `${connectionRevision}:${expectedGeneration}`;
    if (this.#scheduledKey === key) return;
    this.#scheduledKey = key;
    this.#abortRecovery();
    const abort = new AbortController();
    this.#recoveryAbort = abort;
    const previous = this.#recovery ?? Promise.resolve();
    const recovery = previous
      .catch(() => undefined)
      .then(async () => {
        try {
          const ready = await this.#callbacks.waitForHostReady(
            connectionRevision,
            expectedGeneration,
            abort.signal,
          );
          if (!ready || !this.#isCurrent(connectionRevision, expectedGeneration, abort.signal)) return;
          await this.#callbacks.restoreAfterDaemonReady();
          if (!this.#isCurrent(connectionRevision, expectedGeneration, abort.signal)) return;
          this.#callbacks.notifyBrowserRuntimeReady();
        } catch (cause) {
          if (!abort.signal.aborted) this.#callbacks.onFailure(cause);
        }
      });
    this.#recovery = recovery;
    void recovery.then(
      () => this.#clearRecovery(recovery, abort),
      () => this.#clearRecovery(recovery, abort),
    );
  }

  #isCurrent(
    connectionRevision: number,
    expectedGeneration: number,
    signal: AbortSignal,
  ): boolean {
    return !this.#closed
      && !signal.aborted
      && this.#connected
      && this.#connectionRevision === connectionRevision
      && this.#registeredGeneration === expectedGeneration;
  }

  #clearRecovery(recovery: Promise<void>, abort: AbortController): void {
    if (this.#recovery === recovery) this.#recovery = null;
    if (this.#recoveryAbort === abort) this.#recoveryAbort = null;
  }

  #abortRecovery(): void {
    this.#recoveryAbort?.abort();
    this.#recoveryAbort = null;
  }
}
