import { randomUUID } from "node:crypto";
import type {
  BrowserHostCommand,
  BrowserCommandOutcome,
  BrowserSurfaceBinding,
  MainToWorkerMessage,
  WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";
import { utilityProcess, type UtilityProcess } from "electron";
import type { BrowserSurfaceManager, BrowserSurfaceEvent } from "./browser-surface-manager.js";

interface PendingCommand {
  binding: BrowserSurfaceBinding;
  resolve: (value: { outcome: BrowserCommandOutcome; binary?: Buffer }) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}

export type AutomationWorkerStatus = "starting" | "ready" | "restarting" | "failed" | "stopped";

type WorkerLifecycleCallback = (error?: Error) => Promise<void> | void;

const MAX_RECOVERY_ATTEMPTS = 3;
const RECOVERY_STABLE_MILLIS = 30_000;

export class AutomationWorker {
  readonly #entryPath: string;
  readonly #surfaceManager: BrowserSurfaceManager;
  readonly #uploadRoot: string | undefined;
  readonly #onFailure: WorkerLifecycleCallback | undefined;
  readonly #onReady: WorkerLifecycleCallback | undefined;
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

  constructor(input: {
    entryPath: string;
    surfaceManager: BrowserSurfaceManager;
    uploadRoot?: string;
    onFailure?: WorkerLifecycleCallback;
    onReady?: WorkerLifecycleCallback;
  }) {
    this.#entryPath = input.entryPath;
    this.#surfaceManager = input.surfaceManager;
    this.#uploadRoot = input.uploadRoot;
    this.#onFailure = input.onFailure;
    this.#onReady = input.onReady;
  }

  get workerEpoch(): string {
    return this.#workerEpoch;
  }

  get status(): AutomationWorkerStatus {
    return this.#status;
  }

  start(): void {
    if (this.#process) return;
    this.#stopping = false;
    this.#recoveryAttempts = 0;
    this.launch("starting");
  }

  private launch(status: "starting" | "restarting"): void {
    this.#status = status;
    this.#ready = false;
    this.#failureNotified = false;
    this.#workerEpoch = `worker-${randomUUID()}`;
    let child: UtilityProcess;
    try {
      child = utilityProcess.fork(this.#entryPath, [], {
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
    child.on("message", (message) => {
      // A killed UtilityProcess can flush one last IPC message after a new
      // Worker has already been launched. Never let that stale process alter
      // the new Worker epoch or resolve its pending commands.
      if (this.#process !== child) return;
      this.accept(message as WorkerToMainMessage);
    });
    child.on("exit", (code) => {
      if (this.#process !== child) return;
      this.#process = null;
      this.clearStabilityTimer();
      const error = new Error(`browser_worker_exited:${code ?? "unknown"}`);
      this.failPending(error);
      this.failReadyWaiters(error);
      void this.handleFailure(error);
      if (!this.#stopping) this.scheduleRecovery(error);
    });
    child.stdout?.on("data", (chunk) => process.stdout.write(`[browser-worker] ${chunk}`));
    child.stderr?.on("data", (chunk) => process.stderr.write(`[browser-worker] ${chunk}`));
    this.#process = child;
    this.#ready = false;
  }

  async execute(
    binding: BrowserSurfaceBinding,
    command: BrowserHostCommand,
  ): Promise<{ outcome: BrowserCommandOutcome; binary?: Buffer }> {
    this.start();
    await this.waitUntilReady();
    const child = this.#process;
    if (!child) throw new Error("browser_worker_failed");
    const callId = `worker-call-${randomUUID()}`;
    const message: MainToWorkerMessage = {
      type: "worker_command",
      call_id: callId,
      binding,
      command,
    };
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(callId);
        reject(new Error("browser_worker_timeout"));
      }, workerTimeout(command));
      timer.unref();
      this.#pending.set(callId, { binding, resolve, reject, timer });
      child.postMessage(message);
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
    this.#process.postMessage(message);
  }

  stop(): void {
    this.#stopping = true;
    this.clearRecoveryTimer();
    this.clearStabilityTimer();
    const child = this.#process;
    this.#process = null;
    this.#ready = false;
    if (child) child.kill();
    void this.releaseSurfaceControls();
    this.failPending(new Error("browser_worker_stopped"));
    this.failReadyWaiters(new Error("browser_worker_stopped"));
    this.#status = "stopped";
  }

  async restart(): Promise<void> {
    this.stop();
    this.#stopping = false;
    this.#recoveryAttempts = 0;
    this.start();
    await this.waitUntilReady();
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
      if (!this.#stopping && !this.#process) this.launch("restarting");
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
      if (message.worker_epoch !== this.#workerEpoch) {
        this.failReadyWaiters(new Error("browser_worker_epoch_mismatch"));
        return;
      }
      void this.finishReadyHandshake();
      return;
    }
    if (message.type === "worker_result") {
      const pending = this.#pending.get(message.call_id);
      if (!pending) return;
      this.#pending.delete(message.call_id);
      clearTimeout(pending.timer);
      if (!sameBinding(pending.binding, message.binding)) {
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
    if (!child || !this.#ready) return;
    void this.#surfaceManager.sendCdp(
      message.binding,
      message.method,
      message.params ?? {},
      message.session_id,
    )
      .then((result) => {
        const response: MainToWorkerMessage = {
          type: "cdp_response",
          request_id: message.request_id,
          binding: message.binding,
          result,
        };
        child.postMessage(response);
      })
      .catch((cause) => {
        const response: MainToWorkerMessage = {
          type: "cdp_response",
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
        child.postMessage(response);
      });
  }

  private failPending(error: Error): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#pending.clear();
  }

  private async finishReadyHandshake(): Promise<void> {
    try {
      // A failed Worker first fences the old Desktop connection. Waiting here
      // makes the next worker_ready a real lifecycle boundary instead of a
      // local process restart that races the daemon's lease revocation.
      await this.#failureNotification?.catch(() => undefined);
      await this.#onReady?.();
      if (this.#stopping || !this.#process) return;
      this.#ready = true;
      this.#status = "ready";
      const process = this.#process;
      process.postMessage({
        type: "worker_rebind",
        bindings: this.#surfaceManager.bindings(),
      } satisfies MainToWorkerMessage);
      for (const waiter of this.#readyWaiters) waiter.resolve();
      this.#readyWaiters.clear();
      this.#stabilityTimer = setTimeout(() => {
        this.#recoveryAttempts = 0;
        this.#stabilityTimer = null;
      }, RECOVERY_STABLE_MILLIS);
      this.#stabilityTimer.unref();
    } catch (cause) {
      const error = asError(cause, "browser_worker_ready_callback_failed");
      this.#status = "failed";
      this.failReadyWaiters(error);
      void this.handleFailure(error);
      if (!this.#stopping) this.scheduleRecovery(error);
    }
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

  private async releaseSurfaceControls(): Promise<void> {
    await Promise.all(this.#surfaceManager.bindings().map(async (binding) => {
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

  private waitUntilReady(): Promise<void> {
    if (this.#ready) return Promise.resolve();
    return new Promise((resolve, reject) => {
      let waiter: { resolve: () => void; reject: (error: Error) => void };
      const timer = setTimeout(() => {
        this.#readyWaiters.delete(waiter);
        reject(new Error("browser_worker_handshake_timeout"));
      }, 10_000);
      timer.unref();
      waiter = {
        resolve: () => {
          clearTimeout(timer);
          resolve();
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      };
      this.#readyWaiters.add(waiter);
    });
  }

  private failReadyWaiters(error: Error): void {
    for (const waiter of this.#readyWaiters) waiter.reject(error);
    this.#readyWaiters.clear();
  }
}

function sameBinding(left: BrowserSurfaceBinding, right: BrowserSurfaceBinding): boolean {
  return left.desktop_epoch === right.desktop_epoch
    && left.surface_id === right.surface_id
    && left.surface_revision === right.surface_revision
    && left.target_id === right.target_id;
}

function workerTimeout(command: BrowserHostCommand): number {
  return command.type === "devtools" && ["lighthouse", "heap"].includes(command.payload.operation)
    ? 120_000
    : 30_000;
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
