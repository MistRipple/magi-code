import { spawn, type ChildProcess } from "node:child_process";
import { access } from "node:fs/promises";

const MAX_START_ATTEMPTS = 3;
const MAX_RECOVERY_ATTEMPTS = 3;

export type DaemonProcessStatus = "starting" | "ready" | "restarting" | "failed" | "stopped";

export class ProcessSupervisor {
  readonly #daemonPath: string;
  readonly #agentOrigin: string;
  readonly #environment: NodeJS.ProcessEnv;
  #daemon: ChildProcess | null = null;
  #stopping = false;
  #ready = false;
  #recovery: Promise<void> | null = null;
  #status: DaemonProcessStatus = "stopped";

  constructor(input: {
    daemonPath: string;
    agentOrigin: string;
    environment: NodeJS.ProcessEnv;
  }) {
    this.#daemonPath = input.daemonPath;
    this.#agentOrigin = input.agentOrigin;
    this.#environment = input.environment;
  }

  async start(): Promise<void> {
    if (this.#daemon) return;
    this.#stopping = false;
    this.#status = "starting";
    await access(this.#daemonPath);
    let lastError: unknown;
    for (let attempt = 1; attempt <= MAX_START_ATTEMPTS; attempt += 1) {
      try {
        await this.startAttempt();
        this.#ready = true;
        this.#status = "ready";
        return;
      } catch (cause) {
        lastError = cause;
        await this.terminateCurrent();
        if (attempt < MAX_START_ATTEMPTS) await delay(attempt * 250);
      }
    }
    this.#status = "failed";
    throw new Error(`magi_daemon_start_failed:${errorMessage(lastError)}`);
  }

  get status(): DaemonProcessStatus {
    return this.#status;
  }

  get processId(): number | null {
    return this.#daemon?.pid ?? null;
  }

  private async startAttempt(): Promise<void> {
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
        const owned = this.#daemon === child;
        const shouldRecover = owned && this.#ready && !this.#stopping;
        if (owned) this.#daemon = null;
        reject(new Error(`magi_daemon_exited:${code ?? signal ?? "unknown"}`));
        if (shouldRecover) {
          this.#ready = false;
          this.#status = "restarting";
          this.scheduleRecovery();
        }
      });
      child.once("error", reject);
    });
    await Promise.race([waitForHealth(this.#agentOrigin, 60_000), earlyExit]);
  }

  async stop(): Promise<void> {
    this.#stopping = true;
    this.#ready = false;
    this.#status = "stopped";
    await this.terminateCurrent();
    await this.#recovery?.catch(() => undefined);
    this.#recovery = null;
  }

  private scheduleRecovery(): void {
    if (this.#recovery || this.#stopping) return;
    this.#recovery = this.recover().finally(() => {
      this.#recovery = null;
    });
  }

  private async recover(): Promise<void> {
    let lastError: unknown;
    for (let attempt = 1; attempt <= MAX_RECOVERY_ATTEMPTS && !this.#stopping; attempt += 1) {
      await delay(attempt * 500);
      try {
        await this.startAttempt();
        this.#ready = true;
        this.#status = "ready";
        return;
      } catch (cause) {
        lastError = cause;
        await this.terminateCurrent();
      }
    }
    if (!this.#stopping) {
      this.#status = "failed";
      console.error("Magi daemon 恢复失败", errorMessage(lastError));
    }
  }

  private async terminateCurrent(): Promise<void> {
    const child = this.#daemon;
    this.#daemon = null;
    if (!child || child.killed) return;
    child.kill("SIGTERM");
    const exited = await Promise.race([
      new Promise<boolean>((resolve) => child.once("exit", () => resolve(true))),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 5_000)),
    ]);
    if (!exited && !child.killed) child.kill("SIGKILL");
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value ?? "unknown");
}

async function waitForHealth(origin: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(new URL("/health", origin), { cache: "no-store" });
      if (response.ok) return;
      lastError = new Error(`health returned ${response.status}`);
    } catch (cause) {
      lastError = cause;
    }
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  throw new Error(`magi_daemon_health_timeout:${String(lastError ?? "unknown")}`);
}
