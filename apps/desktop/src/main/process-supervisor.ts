import { randomUUID } from "node:crypto";
import { spawn, type ChildProcess } from "node:child_process";
import { access } from "node:fs/promises";

const MAX_START_ATTEMPTS = 3;
const MAX_RECOVERY_ATTEMPTS = 3;
const EXTERNAL_HEALTH_POLL_INTERVAL = 500;
const READY_REGISTRATION_RETRY_BASE_DELAY = 1_000;
const READY_REGISTRATION_RETRY_MAX_DELAY = 8_000;
const READY_REGISTRATION_TIMEOUT_MS = 10_000;

export type DaemonProcessStatus = "starting" | "ready" | "restarting" | "failed" | "stopped";

export interface DaemonIdentityExpectation {
  serviceName: string;
  productVersion: string;
  buildIdentity: string;
}

type HealthSnapshot = {
  runtimeEpoch: string;
  serviceName: string;
  productVersion: string;
  buildIdentity: string;
  startupNonce: string;
};

type HealthReadResult =
  | { kind: "ready"; snapshot: HealthSnapshot }
  | { kind: "unavailable" }
  | { kind: "mismatch"; reason: string };

export class ProcessSupervisor {
  readonly #daemonPath: string;
  readonly #agentOrigin: string;
  readonly #environment: NodeJS.ProcessEnv;
  readonly #daemonIdentity: DaemonIdentityExpectation;
  readonly #reuseExistingDaemon: boolean;
  readonly #onReady: (() => Promise<void>) | undefined;
  readonly #spawnDaemon: typeof spawn;
  #daemon: ChildProcess | null = null;
  #stopping = false;
  #ready = false;
  #recovery: Promise<void> | null = null;
  #externalHealthMonitor: Promise<void> | null = null;
  #externalHealthMonitorGeneration: number | null = null;
  #readyRegistrationPending = false;
  #readyRegistrationInFlight: {
    generation: number;
    runtimeEpoch: string | null;
    promise: Promise<boolean>;
  } | null = null;
  #startPromise: Promise<void> | null = null;
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
    daemonIdentity: DaemonIdentityExpectation;
    onReady?: () => Promise<void>;
    spawnDaemon?: typeof spawn;
  }) {
    this.#daemonPath = input.daemonPath;
    this.#agentOrigin = input.agentOrigin;
    this.#environment = input.environment;
    this.#daemonIdentity = input.daemonIdentity;
    this.#reuseExistingDaemon = input.environment.MAGI_DESKTOP_REUSE_DAEMON === "1";
    this.#onReady = input.onReady;
    this.#spawnDaemon = input.spawnDaemon ?? spawn;
  }

  start(): Promise<void> {
    // start 是生命周期操作而不是“再启动一次”按钮。同一生命周期内的
    // 并发调用必须共享同一个 Promise，否则第二次调用会 abort 第一次
    // 的健康探测，并把 ready 状态置于无人监控的中间态。
    if (!this.#stopping) {
      if (this.#startPromise) return this.#startPromise;
      if (this.#ready && (this.#daemon || this.#externalHealthMonitor)) {
        return Promise.resolve();
      }
    }
    this.#lifecycleAbort.abort();
    const controller = new AbortController();
    this.#lifecycleAbort = controller;
    const signal = controller.signal;
    const generation = ++this.#lifecycleGeneration;
    this.#stopping = false;
    const startPromise = this.enqueue(() => this.startInternal(generation, signal));
    this.#startPromise = startPromise;
    // 仅附加带拒绝处理的清理分支；不能用裸 finally，否则原始启动
    // Promise 被拒绝时，清理 Promise 也会形成未处理拒绝。
    void startPromise.then(
      () => {
        if (this.#startPromise === startPromise) this.#startPromise = null;
      },
      () => {
        if (this.#startPromise === startPromise) this.#startPromise = null;
      },
    );
    return startPromise;
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
    const stopPromise = this.enqueue(() => this.stopInternal(generation));
    void stopPromise.then(
      () => {
        if (this.#startPromise && this.#stopping) this.#startPromise = null;
      },
      () => {
        if (this.#startPromise && this.#stopping) this.#startPromise = null;
      },
    );
    return stopPromise;
  }

  private enqueue(task: () => Promise<void>): Promise<void> {
    const next = this.#lifecycleQueue.then(task, task);
    this.#lifecycleQueue = next.catch(() => undefined);
    return next;
  }

  private async startInternal(generation: number, signal: AbortSignal): Promise<void> {
    if (generation !== this.#lifecycleGeneration || this.#stopping) return;
    if (this.#daemon) return;
    const previousExternalMonitor = this.#externalHealthMonitor;
    if (
      previousExternalMonitor
      && this.#externalHealthMonitorGeneration !== generation
    ) {
      // 新生命周期开始前，先让旧 monitor 在旧 AbortSignal 上收口。不能
      // 只看 Promise 是否存在，否则 retry/restart 会误以为仍有有效 daemon
      // monitor，直接跳过新的健康探测和桌面连接注册。
      await previousExternalMonitor.catch(() => undefined);
      if (generation !== this.#lifecycleGeneration || this.#stopping) return;
    }
    if (this.#externalHealthMonitor) return;
    this.#status = "starting";
    if (this.#reuseExistingDaemon) {
      let health: HealthSnapshot;
      try {
        health = await waitForHealth(this.#agentOrigin, 60_000, this.#daemonIdentity, signal);
      } catch (cause) {
        if (generation === this.#lifecycleGeneration && !this.#stopping) this.#status = "failed";
        throw cause;
      }
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
      this.startExternalHealthMonitor(generation, signal);
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
    const startupNonce = randomUUID();
    const child = this.#spawnDaemon(this.#daemonPath, [], {
      env: {
        ...this.#environment,
        MAGI_DAEMON_START_NONCE: startupNonce,
      },
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
        // 任意受管子进程的非主动退出都必须进入同一恢复队列。不能依赖 #ready：
        // health 已通过而桌面连接仍在注册时它为 false，此时退出若被忽略，启动
        // Promise 仍可能成功收口成一个没有 daemon 的假 ready 状态。
        if (owned && !this.#stopping) {
          this.#ready = false;
          this.#status = "restarting";
          this.scheduleRecovery(generation);
        }
      });
      child.once("error", reject);
    });
    return (
      await Promise.race([
        waitForHealth(
          this.#agentOrigin,
          60_000,
          { ...this.#daemonIdentity, startupNonce },
          signal,
        ),
        earlyExit,
      ])
    ).runtimeEpoch;
  }

  private scheduleRecovery(generation: number): void {
    if (this.#recovery || this.#stopping || generation !== this.#lifecycleGeneration) return;
    const recovery = this.enqueue(() => this.recover(generation));
    this.#recovery = recovery;
    // recovery 是后台生命周期任务，不能把裸拒绝暴露给 Node 的
    // unhandledRejection。错误仍由 recovery 的调用方状态和日志表达，
    // 清理分支本身必须显式消费成功与失败两条路径。
    void recovery.then(
      () => {
        if (this.#recovery === recovery) this.#recovery = null;
      },
      (error) => {
        if (this.#recovery === recovery) this.#recovery = null;
        if (!this.#stopping && generation === this.#lifecycleGeneration) {
          console.error("Magi daemon 恢复任务异常结束", errorMessage(error));
        }
      },
    );
  }

  private async recover(generation: number): Promise<void> {
    const signal = this.#lifecycleAbort.signal;
    let lastError: unknown;
    for (let attempt = 1; attempt <= MAX_RECOVERY_ATTEMPTS; attempt += 1) {
      if (this.#stopping || generation !== this.#lifecycleGeneration) return;
      // 初始启动路径可能已经在同一生命周期内完成下一次尝试。恢复任务排到
      // lifecycle queue 后必须复用该受管进程，不能再并行拉起第二个 daemon。
      if (this.#daemon && !hasExited(this.#daemon)) return;
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
    const externalHealthMonitor = this.#externalHealthMonitor;
    await externalHealthMonitor?.catch(() => undefined);
    if (this.#externalHealthMonitor === externalHealthMonitor) {
      this.#externalHealthMonitor = null;
      this.#externalHealthMonitorGeneration = null;
    }
    this.#recovery = null;
    this.#readyRegistrationInFlight = null;
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
    const runtimeEpoch = this.#runtimeEpoch;
    const inFlight = this.#readyRegistrationInFlight;
    if (inFlight) {
      if (inFlight.generation === generation && inFlight.runtimeEpoch === runtimeEpoch) {
        return this.awaitReadyRegistration(inFlight.promise);
      }
      // 旧生命周期的回调不能被新生命周期复用。等待它结束后，新代次
      // 重新执行一次注册，避免旧连接结果污染当前状态。
      await this.awaitReadyRegistration(inFlight.promise).catch(() => false);
      if (generation !== this.#lifecycleGeneration || this.#stopping) return false;
    }
    this.#lastReadyCallbackGeneration = generation;
    this.#lastReadyCallbackEpoch = runtimeEpoch;
    this.#lastReadyCallbackAccepted = false;
    // 先给闭包一个已初始化的占位 Promise，避免 finally 在类型层面
    // 读取尚未完成赋值的 const；真正的注册任务仍会在下一行替换它。
    let registration: Promise<boolean> = Promise.resolve(false);
    registration = (async () => {
      try {
        await this.awaitReadyRegistration(
          Promise.resolve().then(() => this.#onReady?.()),
        );
        if (
          generation !== this.#lifecycleGeneration
          || this.#runtimeEpoch !== runtimeEpoch
          || this.#stopping
        ) return false;
        this.#readyRegistrationPending = false;
        this.#lastReadyCallbackAccepted = true;
        this.resetReadyRegistrationRetry();
        return true;
      } catch (cause) {
        if (
          generation !== this.#lifecycleGeneration
          || this.#runtimeEpoch !== runtimeEpoch
          || this.#stopping
        ) return false;
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
        if (this.#readyRegistrationInFlight?.promise === registration) {
          this.#readyRegistrationInFlight = null;
        }
      }
    })();
    this.#readyRegistrationInFlight = { generation, runtimeEpoch, promise: registration };
    return registration;
  }

  private startExternalHealthMonitor(generation: number, signal: AbortSignal): void {
    const monitor = this.monitorExternalDaemon(generation, signal);
    this.#externalHealthMonitor = monitor;
    this.#externalHealthMonitorGeneration = generation;
    void monitor.then(
      () => {
        if (this.#externalHealthMonitor === monitor) {
          this.#externalHealthMonitor = null;
          this.#externalHealthMonitorGeneration = null;
        }
      },
      (error) => {
        if (this.#externalHealthMonitor === monitor) {
          this.#externalHealthMonitor = null;
          this.#externalHealthMonitorGeneration = null;
        }
        if (!this.#stopping && generation === this.#lifecycleGeneration) {
          console.error("Magi daemon 外部健康监控异常结束", errorMessage(error));
        }
      },
    );
  }

  private async monitorExternalDaemon(generation: number, signal: AbortSignal): Promise<void> {
    let healthy = true;
    try {
      while (!this.#stopping && generation === this.#lifecycleGeneration) {
        await delay(EXTERNAL_HEALTH_POLL_INTERVAL, signal);
        if (this.#stopping || generation !== this.#lifecycleGeneration) return;
        const health = await readHealth(this.#agentOrigin, this.#daemonIdentity, signal);
        if (health.kind === "mismatch") {
          healthy = false;
          this.#ready = false;
          this.#status = "failed";
          this.#readyRegistrationPending = this.#onReady !== undefined;
          this.#lastReadyCallbackAccepted = false;
          continue;
        }
        const snapshot = health.kind === "ready" ? health.snapshot : null;
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
    } catch (error) {
      if (!signal.aborted && !this.#stopping && generation === this.#lifecycleGeneration) {
        throw error;
      }
    }
  }

  private awaitReadyRegistration<T>(promise: Promise<T>): Promise<T> {
    return withAbortAndTimeout(
      promise,
      this.#lifecycleAbort.signal,
      READY_REGISTRATION_TIMEOUT_MS,
      "magi_desktop_ready_registration_timeout",
    );
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

function withAbortAndTimeout<T>(
  promise: Promise<T>,
  signal: AbortSignal,
  timeoutMs: number,
  timeoutMessage: string,
): Promise<T> {
  if (signal.aborted) return Promise.reject(new Error("magi_daemon_lifecycle_cancelled"));
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    let timer: NodeJS.Timeout | null = null;

    const cleanup = () => {
      if (timer) clearTimeout(timer);
      timer = null;
      signal.removeEventListener("abort", onAbort);
    };
    const settle = (settler: () => void) => {
      if (settled) return;
      settled = true;
      cleanup();
      settler();
    };
    const onAbort = () => settle(() => reject(new Error("magi_daemon_lifecycle_cancelled")));

    signal.addEventListener("abort", onAbort, { once: true });
    timer = setTimeout(
      () => settle(() => reject(new Error(timeoutMessage))),
      Math.max(1, timeoutMs),
    );
    timer.unref();
    promise.then(
      (value) => settle(() => resolve(value)),
      (error) => settle(() => reject(error)),
    );
  });
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value ?? "unknown");
}

async function waitForHealth(
  origin: string,
  timeoutMs: number,
  expectedIdentity: DaemonIdentityExpectation & { startupNonce?: string },
  signal?: AbortSignal,
): Promise<HealthSnapshot> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (signal?.aborted) throw new Error("magi_daemon_lifecycle_cancelled");
    const result = await readHealth(origin, expectedIdentity, signal);
    if (result.kind === "ready") return result.snapshot;
    if (result.kind === "mismatch") {
      throw new Error(`magi_daemon_identity_mismatch:${result.reason}`);
    }
    lastError = new Error("health unavailable or identity missing");
    await delay(150, signal);
  }
  throw new Error(`magi_daemon_health_timeout:${String(lastError ?? "unknown")}`);
}

async function readHealth(
  origin: string,
  expectedIdentity: DaemonIdentityExpectation & { startupNonce?: string },
  signal?: AbortSignal,
): Promise<HealthReadResult> {
  try {
    const response = await fetch(new URL("/health", origin), {
      cache: "no-store",
      signal: signal ?? null,
    });
    if (!response.ok) return { kind: "unavailable" };
    const payload: unknown = await response.json();
    if (!payload || typeof payload !== "object") return { kind: "unavailable" };
    const record = payload as Record<string, unknown>;
    const serviceName = nonEmptyString(record.serviceName);
    const productVersion = nonEmptyString(record.productVersion);
    const buildIdentity = nonEmptyString(record.buildIdentity);
    const startupNonce = nonEmptyString(record.startupNonce);
    const runtimeEpoch = nonEmptyString(record.runtimeEpoch);
    if (!serviceName || !productVersion || !buildIdentity || !startupNonce || !runtimeEpoch) {
      return { kind: "mismatch", reason: "identity_fields_missing" };
    }
    const mismatchedField = [
      ["service_name", serviceName, expectedIdentity.serviceName],
      ["product_version", productVersion, expectedIdentity.productVersion],
      ["build_identity", buildIdentity, expectedIdentity.buildIdentity],
      ...(expectedIdentity.startupNonce
        ? [["startup_nonce", startupNonce, expectedIdentity.startupNonce]]
        : []),
    ].find(([, actual, expected]) => actual !== expected)?.[0];
    if (mismatchedField) return { kind: "mismatch", reason: `${mismatchedField}_mismatch` };
    return {
      kind: "ready",
      snapshot: { runtimeEpoch, serviceName, productVersion, buildIdentity, startupNonce },
    };
  } catch {
    return { kind: "unavailable" };
  }
}

function nonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
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
