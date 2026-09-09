export type DesktopWindowReadinessState =
  | "waiting"
  | "available"
  | "failed"
  | "closed";

interface WindowReadinessWaiter {
  resolve: (windowId: string) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
  signal: AbortSignal | undefined;
  onAbort: (() => void) | undefined;
}

const DEFAULT_WINDOW_READY_TIMEOUT_MS = 15_000;

/**
 * 窗口创建是 Desktop 启动生命周期的一部分，不能用“当前没有窗口”
 * 这种同步异常表达。所有在窗口创建前到达的命令共享这一份就绪状态，
 * 由窗口创建、失败或关闭事件统一结算。
 */
export class DesktopWindowReadiness {
  readonly #timeoutMs: number;
  readonly #waiters = new Set<WindowReadinessWaiter>();
  #state: DesktopWindowReadinessState = "waiting";
  #windowId: string | null = null;
  #terminalError: Error | null = null;

  constructor(timeoutMs = DEFAULT_WINDOW_READY_TIMEOUT_MS) {
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
      throw new Error("desktop_window_ready_timeout_invalid");
    }
    this.#timeoutMs = timeoutMs;
  }

  get state(): DesktopWindowReadinessState {
    return this.#state;
  }

  markAvailable(windowId: string): void {
    if (!windowId.trim()) throw new Error("desktop_window_id_missing");
    if (this.#state === "closed") throw new Error("desktop_window_lifecycle_closed");
    if (this.#state === "failed") throw new Error("desktop_window_lifecycle_failed");
    this.#state = "available";
    this.#windowId = windowId;
    this.#terminalError = null;
    for (const waiter of [...this.#waiters]) this.finishWaiter(waiter, windowId);
  }

  markFailed(cause?: unknown): void {
    if (this.#state === "closed" || this.#state === "failed") return;
    this.#state = "failed";
    this.#windowId = null;
    this.#terminalError = new Error(
      cause instanceof Error && cause.message
        ? `desktop_window_creation_failed:${cause.message}`
        : "desktop_window_creation_failed",
    );
    for (const waiter of [...this.#waiters]) this.finishWaiter(waiter, undefined, this.#terminalError);
  }

  markClosed(): void {
    if (this.#state === "closed") return;
    this.#state = "closed";
    this.#windowId = null;
    this.#terminalError = new Error("desktop_window_closed");
    for (const waiter of [...this.#waiters]) this.finishWaiter(waiter, undefined, this.#terminalError);
  }

  wait(signal?: AbortSignal): Promise<string> {
    if (signal?.aborted) return Promise.reject(new Error("desktop_window_wait_cancelled"));
    if (this.#state === "available" && this.#windowId) return Promise.resolve(this.#windowId);
    if (this.#terminalError) return Promise.reject(this.#terminalError);

    let waiter!: WindowReadinessWaiter;
    const promise = new Promise<string>((resolve, reject) => {
      const onAbort = () => this.finishWaiter(waiter, undefined, new Error("desktop_window_wait_cancelled"));
      const timer = setTimeout(
        () => this.finishWaiter(waiter, undefined, new Error("desktop_window_not_ready")),
        this.#timeoutMs,
      );
      timer.unref();
      waiter = { resolve, reject, timer, signal, onAbort };
    });
    // 调用方可能在窗口关闭的同时退出等待。这里显式吞掉内部竞态产生的
    // 未处理拒绝，真正的调用方仍通过返回的 Promise 获取错误。
    void promise.catch(() => undefined);
    this.#waiters.add(waiter);
    const onAbort = waiter.onAbort;
    if (signal && onAbort) signal.addEventListener("abort", onAbort, { once: true });
    return promise;
  }

  private finishWaiter(
    waiter: WindowReadinessWaiter,
    windowId?: string,
    error?: Error,
  ): void {
    if (!this.#waiters.delete(waiter)) return;
    clearTimeout(waiter.timer);
    const onAbort = waiter.onAbort;
    if (waiter.signal && onAbort) {
      waiter.signal.removeEventListener("abort", onAbort);
    }
    if (error) waiter.reject(error);
    else if (windowId) waiter.resolve(windowId);
    else waiter.reject(new Error("desktop_window_not_ready"));
  }
}
