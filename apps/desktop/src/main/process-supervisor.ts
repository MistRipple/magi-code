import { spawn, type ChildProcess } from "node:child_process";
import { access } from "node:fs/promises";

const MAX_START_ATTEMPTS = 3;
const MAX_RECOVERY_ATTEMPTS = 3;
const EXTERNAL_HEALTH_POLL_INTERVAL = 500;
const READY_REGISTRATION_RETRY_BASE_DELAY = 1_000;
const READY_REGISTRATION_RETRY_MAX_DELAY = 8_000;

export type DaemonProcessStatus = "starting" | "ready" | "restarting" | "failed" | "stopped";

type HealthSnapshot = { runtimeEpoch: string };

export class ProcessSupervisor {
  readonly #daemonPath: string;
  readonly #agentOrigin: string;
  readonly #environment: NodeJS.ProcessEnv;
  readonly #reuseExistingDaemon: boolean;
  readonly #onReady: (() => Promise<void>) | undefined;
  #daemon: ChildProcess | null = null;
  #stopping = false;
  #ready = false;
  #recovery: Promise<void> | null = null;
  #externalHealthMonitor: Promise<void> | null = null;
  #readyRegistrationPending = false;
  #readyRegistrationInFlight: Promise<boolean> | null = null;
  #status: DaemonProcessStatus = "stopped";
  #runtimeEpoch: string | null = null;
  #lifecycleGeneration = 0;
  #lifecycleQueue: Promise<void> = Promise.resolve();
  #lifecycleAbort = new AbortController();
  #lastReadyCallbackGeneration: number | null = null;
  #lastReadyCallbackEpoch: string | null = null;
  #lastReadyCallbackAccepted = false;
  #readyRegistrationRetryAt = 0;
  #readyRegistrationRetryDelay = READY_REGISTRATION_RETRY_BASE_DELAY;

  constructor(input: {
    daemonPath: string;
    agentOrigin: string;
    environment: NodeJS.ProcessEnv;
    onReady?: () => Promise<void>;
  }) {
    this.#daemonPath = input.daemonPath;
    this.#agentOrigin = input.agentOrigin;
    this.#environment = input.environment;
    this.#reuseExistingDaemon = input.environment.MAGI_DESKTOP_REUSE_DAEMON === "1";
    this.#onReady = input.onReady;
  }

  async start(): Promise<void> {
    this.#lifecycleAbort.abort();
    this.#lifecycleAbort = new AbortController();
    const signal = this.#lifecycleAbort.signal;
    const generation = ++this.#lifecycleGeneration;
    this.#stopping = false;
    return this.enqueue(() => this.startInternal(generation, signal));
  }

  get status(): DaemonProcessStatus {
    return this.#status;
  }

  get processId(): number | null {
    return this.#daemon?.pid ?? null;
  }

  async stop(): Promise<void> {
    this.#lifecycleAbort.abort();
    const generation = ++this.#lifecycleGeneration;
    this.#stopping = true;
    this.#ready = false;
    this.#status = "stopped";
    return this.enqueue(() => this.stopInternal(generation));
  }

  private enqueue(task: () => Promise<void>): Promise<void> {
    const next = this.#lifecycleQueue.then(task, task);
    this.#lifecycleQueue = next.catch(() => undefined);
    return next;
  }

  private async startInternal(generation: number, signal: AbortSignal): Promise<void> {
    if (generation !== this.#lifecycleGeneration || this.#stopping) return;
    if (this.#daemon || this.#externalHealthMonitor) return;
    this.#status = "starting";
    if (this.#reuseExistingDaemon) {
      const health = await waitForHealth(this.#agentOrigin, 60_000, signal);
      this.assertCurrent(generation);
      this.#runtimeEpoch = health.runtimeEpoch;
      this.#ready = false;
      this.#status = "starting";
      this.resetReadyRegistrationRetry();
      this.#readyRegistrationPending = this.#onReady !== undefined;
      const registered = await this.tryRegisterReady(false, generation);
      if (registered) {
        this.#ready = true;
        this.#status = "ready";
      }
      this.assertCurrent(generation);
      this.#externalHealthMonitor = this.monitorExternalDaemon(generation);
      return;
    }
    await access(this.#daemonPath);
    let lastError: unknown;
    for (let attempt = 1; attempt <= MAX_START_ATTEMPTS; attempt += 1) {
      this.assertCurrent(generation);
      try {
        this.#runtimeEpoch = await this.startAttempt(generation, signal);
        this.resetReadyRegistrationRetry();
        const registered = await this.tryRegisterReady(true, generation);
        this.assertCurrent(generation);
        if (registered) {
          this.#ready = true;
          this.#status = "ready";
        }
        return;
      } catch (cause) {
        lastError = cause;
        await this.terminateCurrent();
        if (attempt < MAX_START_ATTEMPTS) await delay(attempt * 250, this.#lifecycleAbort.signal);
      }
    }
    this.#status = "failed";
    throw new Error(`magi_daemon_start_failed:${errorMessage(lastError)}`);
  }

  private async startAttempt(generation: number, signal: AbortSignal): Promise<string> {
    const child = spawn(this.#daemonPath, [], {
      env: this.#environment,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    child.stdout?.on("data", (chunk) => process.stdout.write(`[daemon] ${chunk}`));
    child.stderr?.on("data", (chunk) => process.stderr.write(`[daemon] ${chunk}`));
    this.#daemon = child;
    const earlyExit = new Promise<never>((_resolve, reject) => {
      child.once("exit", (code, signal) => {
        const owned = this.#daemon === child && generation === this.#lifecycleGeneration;
        if (this.#daemon === child) this.#daemon = null;
        reject(new Error(`magi_daemon_exited:${code ?? signal ?? "unknown"}`));
        if (owned && this.#ready && !this.#stopping) {
          this.#ready = false;
          this.#status = "restarting";
          this.scheduleRecovery(generation);
        }
      });
      child.once("error", reject);
    });
    return (await Promise.race([waitForHealth(this.#agentOrigin, 60_000, signal), earlyExit])).runtimeEpoch;
  }

  private scheduleRecovery(generation: number): void {
    if (this.#recovery || this.#stopping || generation !== this.#lifecycleGeneration) return;
    this.#recovery = this.enqueue(() => this.recover(generation)).finally(() => {
      this.#recovery = null;
    });
  }

  private async recover(generation: number): Promise<void> {
    const signal = this.#lifecycleAbort.signal;
    let lastError: unknown;
    for (let attempt = 1; attempt <= MAX_RECOVERY_ATTEMPTS; attempt += 1) {
      if (this.#stopping || generation !== this.#lifecycleGeneration) return;
      await delay(attempt * 500, signal);
      try {
        this.#runtimeEpoch = await this.startAttempt(generation, signal);
        this.resetReadyRegistrationRetry();
        const registered = await this.tryRegisterReady(true, generation);
        this.assertCurrent(generation);
        if (registered) {
          this.#ready = true;
          this.#status = "ready";
        }
        return;
      } catch (cause) {
        lastError = cause;
        await this.terminateCurrent();
      }
    }
    if (!this.#stopping && generation === this.#lifecycleGeneration) {
      this.#status = "failed";
      console.error("Magi daemon 恢复失败", errorMessage(lastError));
    }
  }

  private async stopInternal(_generation: number): Promise<void> {
    await this.terminateCurrent();
    await this.#externalHealthMonitor?.catch(() => undefined);
    this.#externalHealthMonitor = null;
    this.#recovery = null;
    this.#runtimeEpoch = null;
    this.#readyRegistrationPending = false;
    this.#lastReadyCallbackGeneration = null;
    this.#lastReadyCallbackEpoch = null;
    this.#lastReadyCallbackAccepted = false;
    this.resetReadyRegistrationRetry();
  }

  private async tryRegisterReady(required: boolean, generation: number): Promise<boolean> {
    if (!this.#onReady) {
      this.#readyRegistrationPending = false;
      return true;
    }
    if (
      !required
      && this.#lastReadyCallbackGeneration === generation
      && this.#lastReadyCallbackEpoch === this.#runtimeEpoch
      && this.#lastReadyCallbackAccepted
    ) {
      this.#readyRegistrationPending = false;
      return true;
    }
    if (!required && Date.now() < this.#readyRegistrationRetryAt) return false;
    if (this.#readyRegistrationInFlight) return this.#readyRegistrationInFlight;
    this.#lastReadyCallbackGeneration = generation;
    this.#lastReadyCallbackEpoch = this.#runtimeEpoch;
    this.#lastReadyCallbackAccepted = false;
    const registration = (async () => {
      try {
        await this.#onReady?.();
        this.#readyRegistrationPending = false;
        this.#lastReadyCallbackAccepted = true;
        this.resetReadyRegistrationRetry();
        return true;
      } catch (cause) {
        this.#readyRegistrationPending = true;
        this.#readyRegistrationRetryAt = Date.now() + this.#readyRegistrationRetryDelay;
        this.#readyRegistrationRetryDelay = Math.min(
          this.#readyRegistrationRetryDelay * 2,
          READY_REGISTRATION_RETRY_MAX_DELAY,
        );
        if (required) throw cause;
        console.error("Magi daemon 已就绪，但桌面浏览器连接注册失败", errorMessage(cause));
        return false;
      } finally {
        this.#readyRegistrationInFlight = null;
      }
    })();
    this.#readyRegistrationInFlight = registration;
    return registration;
  }

  private async monitorExternalDaemon(generation: number): Promise<void> {
    let healthy = true;
    while (!this.#stopping && generation === this.#lifecycleGeneration) {
      await delay(EXTERNAL_HEALTH_POLL_INTERVAL);
      if (this.#stopping || generation !== this.#lifecycleGeneration) return;
      const snapshot = await readHealth(this.#agentOrigin);
      const reachable = snapshot !== null;
      const epochChanged = reachable && this.#runtimeEpoch !== snapshot.runtimeEpoch;
      if (!reachable) {
        if (healthy) {
          healthy = false;
          this.#ready = false;
          this.#status = "restarting";
        }
        continue;
      }
      if (epochChanged) {
        healthy = true;
        this.#runtimeEpoch = snapshot.runtimeEpoch;
        this.#ready = false;
        this.#status = "starting";
        this.resetReadyRegistrationRetry();
        this.#readyRegistrationPending = this.#onReady !== undefined;
        if (this.#readyRegistrationPending) {
          if (await this.tryRegisterReady(false, generation)) {
            this.#ready = true;
            this.#status = "ready";
          }
        } else {
          this.#ready = true;
          this.#status = "ready";
        }
        continue;
      }
      if (!healthy) {
        healthy = true;
        this.#runtimeEpoch = snapshot.runtimeEpoch;
        this.#ready = false;
        this.#status = "starting";
        this.resetReadyRegistrationRetry();
        this.#readyRegistrationPending = this.#onReady !== undefined;
      }
      if (this.#readyRegistrationPending) {
        if (await this.tryRegisterReady(false, generation)) {
          this.#ready = true;
          this.#status = "ready";
        }
      }
    }
  }

  private async terminateCurrent(): Promise<void> {
    const child = this.#daemon;
    this.#daemon = null;
    if (!child || hasExited(child)) return;
    const exit = waitForChildExit(child, 5_000);
    child.kill("SIGTERM");
    if (await exit) return;
    if (hasExited(child)) return;
    const killedExit = waitForChildExit(child, 5_000);
    child.kill("SIGKILL");
    await killedExit;
  }

  private assertCurrent(generation: number): void {
    if (generation !== this.#lifecycleGeneration || this.#stopping) {
      throw new Error("magi_daemon_lifecycle_cancelled");
    }
  }

  private resetReadyRegistrationRetry(): void {
    this.#readyRegistrationRetryAt = 0;
    this.#readyRegistrationRetryDelay = READY_REGISTRATION_RETRY_BASE_DELAY;
  }
}

function delay(milliseconds: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(new Error("magi_daemon_lifecycle_cancelled"));
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, milliseconds);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new Error("magi_daemon_lifecycle_cancelled"));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value ?? "unknown");
}

async function waitForHealth(
  origin: string,
  timeoutMs: number,
  signal?: AbortSignal,
): Promise<HealthSnapshot> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (signal?.aborted) throw new Error("magi_daemon_lifecycle_cancelled");
    const snapshot = await readHealth(origin, signal);
    if (snapshot) return snapshot;
    lastError = new Error("health unavailable or runtime epoch missing");
    await delay(150, signal);
  }
  throw new Error(`magi_daemon_health_timeout:${String(lastError ?? "unknown")}`);
}

async function readHealth(origin: string, signal?: AbortSignal): Promise<HealthSnapshot | null> {
  try {
    const response = await fetch(new URL("/health", origin), {
      cache: "no-store",
      signal: signal ?? null,
    });
    if (!response.ok) return null;
    const payload: unknown = await response.json();
    if (!payload || typeof payload !== "object") return null;
    const runtimeEpoch = (payload as { runtimeEpoch?: unknown }).runtimeEpoch;
    return typeof runtimeEpoch === "string" && runtimeEpoch.length > 0 ? { runtimeEpoch } : null;
  } catch {
    return null;
  }
}

function hasExited(child: ChildProcess): boolean {
  return child.exitCode !== null || child.signalCode !== null;
}

function waitForChildExit(child: ChildProcess, timeoutMs: number): Promise<boolean> {
  if (hasExited(child)) return Promise.resolve(true);
  return new Promise((resolve) => {
    let settled = false;
    let timer: NodeJS.Timeout;
    const finish = (value: boolean) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(value);
    };
    timer = setTimeout(() => finish(false), timeoutMs);
    child.once("exit", () => finish(true));
    child.once("close", () => finish(true));
  });
}
