import { randomUUID } from "node:crypto";
import {
  type BrowserHostCommand,
  type BrowserCommandOutcome,
  type BrowserSurfaceBinding,
  type MainToWorkerMessage,
  type WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";
import {
  parseMainToWorkerMessage,
  parseWorkerToMainMessage,
} from "@magi/desktop-browser-contracts/validation";
import * as electron from "electron";
import type { UtilityProcess } from "electron";
import type { BrowserSurfaceManager, BrowserSurfaceEvent } from "./browser-surface-manager.js";

interface PendingCommand {
  child: UtilityProcess;
  workerEpoch: string;
  callId: string;
  binding: BrowserSurfaceBinding;
  resolve: (value: { outcome: BrowserCommandOutcome; binary?: Buffer }) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
  signalCleanup: (() => void) | null;
  cancelSent: boolean;
}

interface RebindWaiter {
  child: UtilityProcess;
  workerEpoch: string;
  rebindId: string;
  resolve: () => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}

interface StopRequest {
  child: UtilityProcess | null;
  exit: Promise<void>;
  bindings: BrowserSurfaceBinding[];
}

type UtilityProcessFork = (
  modulePath: string,
  args: string[],
  options: Parameters<typeof electron.utilityProcess.fork>[2],
) => UtilityProcess;

export type AutomationWorkerStatus = "starting" | "ready" | "restarting" | "failed" | "stopped";

type WorkerLifecycleCallback = (error?: Error) => Promise<void> | void;

const MAX_RECOVERY_ATTEMPTS = 3;
const RECOVERY_STABLE_MILLIS = 30_000;
const REBIND_TIMEOUT_MS = 10_000;
const PROCESS_EXIT_TIMEOUT_MS = 5_000;

export class AutomationWorker {
  readonly #entryPath: string;
  readonly #surfaceManager: BrowserSurfaceManager;
  readonly #uploadRoot: string | undefined;
  readonly #onFailure: WorkerLifecycleCallback | undefined;
  readonly #onReady: WorkerLifecycleCallback | undefined;
  readonly #fork: UtilityProcessFork;
  readonly #pending = new Map<string, PendingCommand>();
  #process: UtilityProcess | null = null;
  #workerEpoch = "";
  #status: AutomationWorkerStatus = "stopped";
  #ready = false;
  #readyWaiters = new Set<{ resolve: () => void; reject: (error: Error) => void }>();
  #stopping = false;
  #recoveryAttempts = 0;
  #recoveryTimer: NodeJS.Timeout | null = null;
  #stabilityTimer: NodeJS.Timeout | null = null;
  #failureNotified = false;
  #failureNotification: Promise<void> | null = null;
  #readyHandshakeProcess: UtilityProcess | null = null;
  #rebindWaiter: RebindWaiter | null = null;
  #processExitWaiters = new Map<UtilityProcess, { promise: Promise<void>; resolve: () => void }>();
  #lifecycle: Promise<void> = Promise.resolve();
  #lifecycleBusy = false;
  #lifecycleEpoch = 0;

  constructor(input: {
    entryPath: string;
    surfaceManager: BrowserSurfaceManager;
    uploadRoot?: string;
    onFailure?: WorkerLifecycleCallback;
    onReady?: WorkerLifecycleCallback;
    fork?: UtilityProcessFork;
  }) {
    this.#entryPath = input.entryPath;
    this.#surfaceManager = input.surfaceManager;
    this.#uploadRoot = input.uploadRoot;
    this.#onFailure = input.onFailure;
    this.#onReady = input.onReady;
    this.#fork = input.fork ?? ((entryPath, args, options) => (
      electron.utilityProcess.fork(entryPath, args, options)
    ));
  }

  get workerEpoch(): string {
    return this.#workerEpoch;
  }

  get status(): AutomationWorkerStatus {
    return this.#status;
  }

  async start(signal?: AbortSignal): Promise<void> {
    if (signal?.aborted) throw new Error("browser_command_cancelled");
    if (this.#ready) return;
    // Keep process creation synchronous for the first caller. This makes the
    // returned start promise a wait for readiness while still exposing the
    // freshly allocated worker epoch to immediately delivered IPC messages.
    if (!this.#process && !this.#lifecycleBusy) {
      this.#stopping = false;
      this.#recoveryAttempts = 0;
      this.launch("starting");
    }
    return this.enqueueLifecycle(async () => {
      if (signal?.aborted) throw new Error("browser_command_cancelled");
      if (this.#ready) return;
      if (!this.#process) {
        this.#stopping = false;
        this.#recoveryAttempts = 0;
        this.launch("starting");
      }
      await this.waitUntilReady(signal);
    });
  }

  private launch(status: "starting" | "restarting"): void {
    const lifecycleEpoch = ++this.#lifecycleEpoch;
    this.#status = status;
    this.#ready = false;
    this.#readyHandshakeProcess = null;
    this.#failureNotified = false;
    this.#workerEpoch = `worker-${randomUUID()}`;
    let child: UtilityProcess;
    try {
      child = this.#fork(this.#entryPath, [], {
        serviceName: "Magi Browser Automation",
        stdio: "pipe",
        env: {
          MAGI_BROWSER_WORKER_EPOCH: this.#workerEpoch,
          ...(this.#uploadRoot ? { MAGI_BROWSER_UPLOAD_ROOT: this.#uploadRoot } : {}),
        },
      });
    } catch (cause) {
      this.#status = "failed";
      const error = asError(cause, "browser_worker_fork_failed");
      void this.handleFailure(error);
      this.scheduleRecovery(error);
      return;
    }
    let resolveExit!: () => void;
    const exit = new Promise<void>((resolve) => {
      resolveExit = resolve;
    });
    this.#processExitWaiters.set(child, { promise: exit, resolve: resolveExit });
    child.on("message", (value) => {
      // A killed UtilityProcess can flush one last IPC message after a new
      // Worker has already been launched. Never let that stale process alter
      // the new Worker epoch or resolve its pending commands.
      if (this.#process !== child) return;
      try {
        this.accept(parseWorkerToMainMessage(value));
      } catch (cause) {
        this.rejectProtocol(child, cause);
      }
    });
    child.on("exit", (code) => {
      this.resolveProcessExit(child);
      if (this.#process !== child) return;
      const error = new Error(`browser_worker_exited:${code ?? "unknown"}`);
      console.error("[browser-worker] UtilityProcess exited", {
        code,
        workerEpoch: this.#workerEpoch,
      });
      this.closeProcess(child, error, false);
    });
    child.on("error", (cause) => {
      console.error("[browser-worker] UtilityProcess error", {
        workerEpoch: this.#workerEpoch,
        error: String(cause),
      });
    });
    child.stdout?.on("data", (chunk) => process.stdout.write(`[browser-worker] ${chunk}`));
    child.stderr?.on("data", (chunk) => process.stderr.write(`[browser-worker] ${chunk}`));
    this.#process = child;
    this.#ready = false;
  }

  async execute(
    binding: BrowserSurfaceBinding,
    command: BrowserHostCommand,
    signal?: AbortSignal,
  ): Promise<{ outcome: BrowserCommandOutcome; binary?: Buffer }> {
    await this.start(signal);
    const child = this.#process;
    if (!child) throw new Error("browser_worker_failed");
    if (signal?.aborted) throw new Error("browser_command_cancelled");
    const callId = `worker-call-${randomUUID()}`;
    const message: MainToWorkerMessage = {
      type: "worker_command",
      call_id: callId,
      binding,
      command,
    };
    return new Promise((resolve, reject) => {
      let pending!: PendingCommand;
      const timer = setTimeout(() => {
        this.sendCancel(pending);
      }, workerTimeout(command));
      timer.unref();
      pending = {
        child,
        workerEpoch: this.#workerEpoch,
        callId,
        binding,
        resolve,
        reject,
        timer,
        signalCleanup: null,
        cancelSent: false,
      };
      this.#pending.set(callId, pending);
      if (signal) {
        const onAbort = () => this.sendCancel(pending);
        pending.signalCleanup = () => signal.removeEventListener("abort", onAbort);
        signal.addEventListener("abort", onAbort, { once: true });
        if (signal.aborted) onAbort();
      }
      try {
        this.postMessage(child, message);
      } catch (cause) {
        this.#pending.delete(callId);
        this.cleanupPending(pending);
        reject(cause instanceof Error ? cause : new Error(String(cause)));
      }
    });
  }

  forwardSurfaceEvent(event: BrowserSurfaceEvent): void {
    if (event.type !== "cdp_event" || !this.#process || !this.#ready) return;
    const message: MainToWorkerMessage = {
      type: "cdp_event",
      binding: event.binding,
      method: event.method,
      params: event.params,
      ...(event.sessionId ? { session_id: event.sessionId } : {}),
    };
    this.postMessage(this.#process, message);
  }

  async stop(): Promise<void> {
    const request = this.requestStop();
    return this.enqueueLifecycle(async () => {
      await this.finishStop(request);
      this.#status = "stopped";
    });
  }

  async restart(): Promise<void> {
    const request = this.requestStop();
    return this.enqueueLifecycle(async () => {
      await this.finishStop(request);
      this.#stopping = false;
      this.#recoveryAttempts = 0;
      this.launch("restarting");
      await this.waitUntilReady();
    });
  }

  private requestStop(): StopRequest {
    this.#stopping = true;
    this.clearRecoveryTimer();
    this.clearStabilityTimer();
    const child = this.#process;
    const exit = child
      ? this.#processExitWaiters.get(child)?.promise ?? Promise.resolve()
      : Promise.resolve();
    const bindings = this.#surfaceManager.bindings();
    this.#process = null;
    this.#lifecycleEpoch += 1;
    this.#ready = false;
    this.#readyHandshakeProcess = null;
    this.rejectRebindWaiter(new Error("browser_worker_stopped"));
    if (child) {
      try {
        child.kill();
      } catch (cause) {
        console.warn("[browser-worker] UtilityProcess stop failed", errorMessage(cause));
      }
    }
    this.failPending(new Error("browser_worker_stopped"));
    this.failReadyWaiters(new Error("browser_worker_stopped"));
    return { child, exit, bindings };
  }

  private async finishStop(request: StopRequest): Promise<void> {
    await Promise.all([
      this.releaseSurfaceControls(request.bindings),
      waitForProcessExit(request.exit),
    ]);
    if (request.child && this.#processExitWaiters.get(request.child)?.promise === request.exit) {
      this.#processExitWaiters.delete(request.child);
    }
  }

  private enqueueLifecycle<T>(operation: () => Promise<T>): Promise<T> {
    const next = this.#lifecycle.then(operation, operation);
    this.#lifecycleBusy = true;
    const settled = next.then(() => undefined, () => undefined);
    this.#lifecycle = settled;
    void settled.then(
      () => {
        if (this.#lifecycle === settled) this.#lifecycleBusy = false;
      },
      () => {
        if (this.#lifecycle === settled) this.#lifecycleBusy = false;
      },
    );
    return next;
  }

  private waitForRebindAck(
    child: UtilityProcess,
    workerEpoch: string,
    rebindId: string,
  ): Promise<void> {
    this.rejectRebindWaiter(new Error("browser_worker_rebind_superseded"));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.#rebindWaiter?.rebindId !== rebindId) return;
        this.#rebindWaiter = null;
        reject(new Error("browser_worker_rebind_timeout"));
      }, REBIND_TIMEOUT_MS);
      timer.unref();
      this.#rebindWaiter = { child, workerEpoch, rebindId, resolve, reject, timer };
    });
  }

  private rejectRebindWaiter(error: Error): void {
    const waiter = this.#rebindWaiter;
    if (!waiter) return;
    this.#rebindWaiter = null;
    clearTimeout(waiter.timer);
    waiter.reject(error);
  }

  private scheduleRecovery(cause: unknown): void {
    if (this.#stopping || this.#recoveryTimer) return;
    if (this.#recoveryAttempts >= MAX_RECOVERY_ATTEMPTS) {
      this.#status = "failed";
      console.error("Browser Automation Worker 恢复失败", cause);
      return;
    }
    this.#recoveryAttempts += 1;
    this.#status = "restarting";
    this.#recoveryTimer = setTimeout(() => {
      this.#recoveryTimer = null;
      void this.enqueueLifecycle(async () => {
        if (!this.#stopping && !this.#process) this.launch("restarting");
      });
    }, this.#recoveryAttempts * 500);
    this.#recoveryTimer.unref();
  }

  private clearRecoveryTimer(): void {
    if (this.#recoveryTimer) clearTimeout(this.#recoveryTimer);
    this.#recoveryTimer = null;
  }

  private clearStabilityTimer(): void {
    if (this.#stabilityTimer) clearTimeout(this.#stabilityTimer);
    this.#stabilityTimer = null;
  }

  private accept(message: WorkerToMainMessage): void {
    if (message.type === "worker_ready") {
      const child = this.#process;
      if (!child || this.#ready || this.#readyHandshakeProcess === child) return;
      if (message.worker_epoch !== this.#workerEpoch) {
        this.closeProcess(child, new Error("browser_worker_epoch_mismatch"));
        return;
      }
      this.#readyHandshakeProcess = child;
      void this.finishReadyHandshake(child, this.#lifecycleEpoch);
      return;
    }
    if (message.type === "worker_rebind_ack") {
      const waiter = this.#rebindWaiter;
      if (!waiter
        || waiter.child !== this.#process
        || waiter.workerEpoch !== this.#workerEpoch
        || message.worker_epoch !== waiter.workerEpoch
        || message.rebind_id !== waiter.rebindId) {
        return;
      }
      this.#rebindWaiter = null;
      clearTimeout(waiter.timer);
      waiter.resolve();
      return;
    }
    if (message.type === "worker_result") {
      const pending = this.#pending.get(message.call_id);
      if (!pending) return;
      if (pending.child !== this.#process || pending.workerEpoch !== this.#workerEpoch) return;
      this.#pending.delete(message.call_id);
      this.cleanupPending(pending);
      if (!samePhysicalBinding(pending.binding, message.binding)) {
        pending.reject(new Error("browser_surface_stale"));
        return;
      }
      pending.resolve({
        outcome: message.outcome,
        ...(message.binary_base64
          ? { binary: Buffer.from(message.binary_base64, "base64") }
          : {}),
      });
      return;
    }
    const child = this.#process;
    const workerEpoch = this.#workerEpoch;
    if (!child || !this.#ready) return;
    void this.#surfaceManager.sendCdp(
      message.binding,
      message.method,
      message.params ?? {},
      message.session_id,
      { allowNavigationAdvance: message.allow_navigation_advance === true },
    )
      .then((result) => {
        if (this.#process !== child || this.#workerEpoch !== workerEpoch || !this.#ready) return;
        const response: MainToWorkerMessage = {
          type: "cdp_response",
          call_id: message.call_id,
          request_id: message.request_id,
          binding: message.binding,
          result,
        };
        this.postMessage(child, response);
      })
      .catch((cause) => {
        if (this.#process !== child || this.#workerEpoch !== workerEpoch || !this.#ready) return;
        const response: MainToWorkerMessage = {
          type: "cdp_response",
          call_id: message.call_id,
          request_id: message.request_id,
          binding: message.binding,
          error: {
            code: errorCode(cause),
            message: cause instanceof Error ? cause.message : String(cause),
            recoverable: true,
            side_effect_started: false,
            diagnostic: cause instanceof Error ? cause.stack ?? null : null,
          },
        };
        this.postMessage(child, response);
      });
  }

  private failPending(error: Error): void {
    for (const pending of this.#pending.values()) {
      this.cleanupPending(pending);
      pending.reject(error);
    }
    this.#pending.clear();
  }

  private sendCancel(pending: PendingCommand): void {
    if (pending.cancelSent) return;
    pending.cancelSent = true;
    try {
      this.postMessage(pending.child, { type: "worker_cancel", call_id: pending.callId });
    } catch (cause) {
      // The process exit handler owns final rejection. A failed cancel write
      // must not make the command appear settled while Chromium may still be
      // executing it.
      console.warn("[browser-worker] Worker cancel message failed", errorMessage(cause));
    }
  }

  private cleanupPending(pending: PendingCommand): void {
    clearTimeout(pending.timer);
    pending.signalCleanup?.();
    pending.signalCleanup = null;
  }

  private async finishReadyHandshake(child: UtilityProcess, lifecycleEpoch: number): Promise<void> {
    try {
      // A failed Worker first fences the old Desktop connection. Waiting here
      // makes the next worker_ready a real lifecycle boundary instead of a
      // local process restart that races the daemon's lease revocation.
      await this.#failureNotification?.catch(() => undefined);
      if (this.#process !== child || this.#stopping || this.#lifecycleEpoch !== lifecycleEpoch) return;
      const rebindId = `rebind-${randomUUID()}`;
      this.postMessage(child, {
        type: "worker_rebind",
        worker_epoch: this.#workerEpoch,
        rebind_id: rebindId,
        bindings: this.#surfaceManager.bindings(),
      });
      await this.waitForRebindAck(child, this.#workerEpoch, rebindId);
      if (this.#process !== child || this.#stopping || this.#lifecycleEpoch !== lifecycleEpoch) return;
      // The ack is the execution gate. Marking the worker ready before the
      // lifecycle callback is intentional: the callback replays annotations
      // through the same command path and must not wait behind restart's
      // own ready waiter.
      this.#ready = true;
      this.#status = "ready";
      // Desktop Host 只能在本地 Worker 已完成握手和 Surface 重绑后连接。
      // 这样恢复代次不会把 daemon 接到一个尚不能执行命令的 Chromium Worker。
      await this.#onReady?.();
      if (this.#process !== child || this.#stopping || this.#lifecycleEpoch !== lifecycleEpoch) return;
      for (const waiter of this.#readyWaiters) waiter.resolve();
      this.#readyWaiters.clear();
      this.#stabilityTimer = setTimeout(() => {
        this.#recoveryAttempts = 0;
        this.#stabilityTimer = null;
      }, RECOVERY_STABLE_MILLIS);
      this.#stabilityTimer.unref();
    } catch (cause) {
      const error = asError(cause, "browser_worker_ready_callback_failed");
      this.closeProcess(child, error);
    }
  }

  private postMessage(child: UtilityProcess, message: MainToWorkerMessage): void {
    child.postMessage(parseMainToWorkerMessage(message));
  }

  private rejectProtocol(child: UtilityProcess, cause: unknown): void {
    if (this.#process !== child) return;
    const error = asError(cause, "browser_worker_protocol_invalid");
    this.closeProcess(child, error);
    console.error("[browser-worker] Worker IPC protocol rejected", {
      workerEpoch: this.#workerEpoch,
      error: error.message,
    });
  }

  private closeProcess(child: UtilityProcess, error: Error, kill = true): void {
    if (this.#process !== child) return;
    // Fence the process before killing it. UtilityProcess may emit exit later,
    // and that late event must not be allowed to affect the replacement.
    this.#process = null;
    this.#lifecycleEpoch += 1;
    this.#ready = false;
    this.#readyHandshakeProcess = null;
    this.rejectRebindWaiter(error);
    this.clearStabilityTimer();
    this.#status = "failed";
    if (kill) {
      try {
        if (!child.kill()) {
          console.warn("[browser-worker] UtilityProcess kill returned false");
        }
      } catch (cause) {
        console.warn("[browser-worker] UtilityProcess kill failed", errorMessage(cause));
      }
    }
    this.failPending(error);
    this.failReadyWaiters(error);
    void this.handleFailure(error);
    if (!this.#stopping) this.scheduleRecovery(error);
  }

  private async handleFailure(error: Error): Promise<void> {
    if (this.#failureNotified) return this.#failureNotification ?? Promise.resolve();
    this.#failureNotified = true;
    this.#ready = false;
    this.failPending(error);
    this.failReadyWaiters(error);
    const notification = (async () => {
      await this.releaseSurfaceControls();
      await this.#onFailure?.(error);
    })();
    this.#failureNotification = notification.catch((cause) => {
      console.error("Browser Automation Worker 生命周期收口失败", errorMessage(cause));
    });
    return this.#failureNotification;
  }

  private async releaseSurfaceControls(bindings = this.#surfaceManager.bindings()): Promise<void> {
    await Promise.all(bindings.map(async (binding) => {
      try {
        await this.#surfaceManager.updateControl(
          binding.tab_id,
          binding.surface_id,
          { mode: "released", fence: 0 },
        );
      } catch {
        // Surface may have been closed or replaced during worker failure.
      }
    }));
  }

  private waitUntilReady(signal?: AbortSignal): Promise<void> {
    if (this.#ready) return Promise.resolve();
    return new Promise((resolve, reject) => {
      let settled = false;
      let waiter: { resolve: () => void; reject: (error: Error) => void };
      const cleanup = () => {
        clearTimeout(timer);
        this.#readyWaiters.delete(waiter);
        if (signal) signal.removeEventListener("abort", onAbort);
      };
      const onAbort = () => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(new Error("browser_command_cancelled"));
      };
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(new Error("browser_worker_handshake_timeout"));
      }, 10_000);
      timer.unref();
      waiter = {
        resolve: () => {
          if (settled) return;
          settled = true;
          cleanup();
          resolve();
        },
        reject: (error) => {
          if (settled) return;
          settled = true;
          cleanup();
          reject(error);
        },
      };
      this.#readyWaiters.add(waiter);
      if (signal) {
        signal.addEventListener("abort", onAbort, { once: true });
        if (signal.aborted) onAbort();
      }
    });
  }

  private failReadyWaiters(error: Error): void {
    for (const waiter of this.#readyWaiters) waiter.reject(error);
    this.#readyWaiters.clear();
  }

  private resolveProcessExit(child: UtilityProcess): void {
    const waiter = this.#processExitWaiters.get(child);
    if (!waiter) return;
    this.#processExitWaiters.delete(child);
    waiter.resolve();
  }
}

function samePhysicalBinding(left: BrowserSurfaceBinding, right: BrowserSurfaceBinding): boolean {
  return left.desktop_epoch === right.desktop_epoch
    && left.window_id === right.window_id
    && left.surface_id === right.surface_id
    && left.surface_revision === right.surface_revision
    && left.tab_id === right.tab_id
    && left.web_contents_id === right.web_contents_id
    && left.target_id === right.target_id
    && left.browser_context_id === right.browser_context_id
    && left.navigation_revision === right.navigation_revision;
}

function workerTimeout(command: BrowserHostCommand): number {
  if (command.type !== "devtools") return 30_000;
  if (["lighthouse", "heap"].includes(command.payload.operation)) return 120_000;
  if (command.payload.operation === "wait_for") {
    const requested = typeof command.payload.arguments.timeout_ms === "number"
      && Number.isFinite(command.payload.arguments.timeout_ms)
      ? Math.max(0, Math.min(60_000, command.payload.arguments.timeout_ms))
      : 5_000;
    return requested + 2_000;
  }
  return 30_000;
}

function errorCode(cause: unknown): string {
  if (!(cause instanceof Error)) return "browser_cdp_failed";
  const code = cause.message.split(":", 1)[0];
  return code?.startsWith("browser_") ? code : "browser_cdp_failed";
}

function asError(cause: unknown, fallback: string): Error {
  return cause instanceof Error ? cause : new Error(`${fallback}:${String(cause)}`);
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function waitForProcessExit(exit: Promise<void>): Promise<void> {
  let timer: NodeJS.Timeout | null = null;
  return new Promise((resolve) => {
    const finish = () => {
      if (timer) clearTimeout(timer);
      timer = null;
      resolve();
    };
    exit.then(finish, finish);
    timer = setTimeout(finish, PROCESS_EXIT_TIMEOUT_MS);
    timer.unref();
  });
}
