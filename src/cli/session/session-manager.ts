import { EventEmitter } from 'events';
import { v4 as uuidv4 } from 'uuid';
import { PrintSession } from './print-session';
import type { CLIType } from '../types';
import type { SessionMessage, SessionProcess, SessionProcessOptions, SessionResponse } from './types';

type QueueItem = {
  message: SessionMessage;
  enqueuedAt: number;
  attempts: number;
  resolve: (value: SessionResponse) => void;
  reject: (error: Error) => void;
};

interface ManagedSession {
  process: SessionProcess;
  busy: boolean;
  queue: QueueItem[];
  processing: boolean;
  failures: number;
  lastOutputAt: number;
  lastStartAt?: number;
  healthTimer?: NodeJS.Timeout;
  suppressOutput: boolean;
  lastSnapshot?: string;
}

export class SessionManager extends EventEmitter {
  private sessions: Map<string, ManagedSession> = new Map();
  private readonly softInterruptMs: number;
  private readonly hardTimeoutMs: number;

  constructor(private readonly options: SessionProcessOptions) {
    super();
    const idleMs = options.idleTimeoutMs ?? 120000;
    this.softInterruptMs = Math.max(15000, Math.floor(idleMs * 0.7));
    this.hardTimeoutMs = Math.max(30000, idleMs);
  }

  private getKey(cli: CLIType, role: 'worker' | 'orchestrator'): string {
    return `${role}:${cli}`;
  }

  async startSession(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void> {
    const key = this.getKey(cli, role);
    if (this.sessions.has(key)) return;

    const args: string[] = [];
    if (cli === 'claude') {
      args.push('-p', '--output-format', 'stream-json', '--verbose', '--permission-mode', 'acceptEdits', '--continue');
    }
    if (cli === 'codex') {
      args.push('exec', '--full-auto', '--skip-git-repo-check', '--output-format', 'stream-json');
    }
    if (cli === 'gemini') {
      args.push('--output-format', 'stream-json', '--approval-mode', 'yolo');
    }

    const command = this.options.commandOverrides?.[cli] ?? cli;
    const process = new PrintSession({
      cli,
      cwd: this.options.cwd,
      env: this.options.env,
      idleTimeoutMs: this.options.idleTimeoutMs,
      command,
      args,
    });
    process.on('output', (chunk: string) => {
      const entry = this.sessions.get(key);
      if (entry) {
        entry.lastOutputAt = Date.now();
        if (entry.suppressOutput) {
          return;
        }
      }
      this.emit('output', { cli, role, chunk });
    });
    process.on('exit', () => {
      const entry = this.sessions.get(key);
      if (entry) {
        entry.failures += 1;
        this.emit('sessionEvent', { type: 'exit', cli, role });
      }
    });
    await process.start();
    const entry: ManagedSession = {
      process,
      busy: false,
      queue: [],
      processing: false,
      failures: 0,
      lastOutputAt: Date.now(),
      suppressOutput: false,
      lastSnapshot: undefined,
    };
    entry.healthTimer = this.startHealthMonitor(cli, role);
    this.sessions.set(key, entry);
    this.emit('sessionEvent', { type: 'start', cli, role });
  }

  async stopSession(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) return;
    if (entry.healthTimer) {
      clearInterval(entry.healthTimer);
    }
    entry.queue = [];
    entry.suppressOutput = false;
    await entry.process.stop();
    this.sessions.delete(key);
  }

  async send(cli: CLIType, role: 'worker' | 'orchestrator', message: SessionMessage): Promise<SessionResponse> {
    const key = this.getKey(cli, role);
    let entry = this.sessions.get(key);
    if (!entry) {
      await this.startSession(cli, role);
      entry = this.sessions.get(key);
    }
    if (!entry) {
      throw new Error(`Session not available: ${cli}`);
    }
    return new Promise<SessionResponse>((resolve, reject) => {
      entry!.queue.push({
        message,
        enqueuedAt: Date.now(),
        attempts: 0,
        resolve,
        reject,
      });
      this.emit('sessionEvent', { type: 'enqueue', cli, role, requestId: message.requestId });
      void this.processQueue(cli, role);
    });
  }

  async interrupt(cli: CLIType, role: 'worker' | 'orchestrator', reason?: string): Promise<void> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) return;
    await entry.process.interrupt(reason);
  }

  async stopAll(): Promise<void> {
    const entries = Array.from(this.sessions.values());
    entries.forEach(entry => {
      if (entry.healthTimer) {
        clearInterval(entry.healthTimer);
      }
      entry.queue = [];
      entry.suppressOutput = false;
    });
    await Promise.all(entries.map(entry => entry.process.stop()));
    this.sessions.clear();
  }

  private async processQueue(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry || entry.processing) return;
    entry.processing = true;
    try {
      while (entry.queue.length > 0) {
        const item = entry.queue.shift();
        if (!item) break;
        try {
          const response = await this.executeItem(cli, role, entry, item);
          item.resolve(response);
        } catch (error) {
          const err = error instanceof Error ? error : new Error(String(error));
          item.reject(err);
        }
      }
    } finally {
      entry.processing = false;
    }
  }

  private async executeItem(
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    entry: ManagedSession,
    item: QueueItem
  ): Promise<SessionResponse> {
    if (!entry.process.isAlive) {
      entry = await this.restartSession(cli, role, 'process_dead');
    }

    entry.busy = true;
    entry.suppressOutput = Boolean(item.message.silent);
    entry.lastStartAt = Date.now();
    this.emit('sessionEvent', { type: 'start', cli, role, requestId: item.message.requestId });

    let softTimer: NodeJS.Timeout | undefined;
    softTimer = setTimeout(() => {
      void entry.process.interrupt('soft_timeout');
      this.emit('sessionEvent', { type: 'soft_interrupt', cli, role, requestId: item.message.requestId });
    }, this.softInterruptMs);

    let hardTimer: NodeJS.Timeout | undefined;
    const hardTimeoutPromise = new Promise<SessionResponse>((_, reject) => {
      hardTimer = setTimeout(() => {
        void this.restartSession(cli, role, 'hard_timeout');
        reject(new Error(`${cli} session hard timeout`));
      }, this.hardTimeoutMs);
    });

    try {
      const response = await Promise.race([entry.process.send(item.message), hardTimeoutPromise]);
      const snapshot = this.extractSnapshot(item.message.metadata);
      if (snapshot) {
        entry.lastSnapshot = snapshot;
      }
      entry.failures = 0;
      this.emit('sessionEvent', { type: 'complete', cli, role, requestId: item.message.requestId });
      return response;
    } catch (error) {
      const err = error instanceof Error ? error : new Error(String(error));
      if (this.isRecoverable(err) && item.attempts < 1) {
        item.attempts += 1;
        entry = await this.restartSession(cli, role, 'recoverable_error');
        this.emit('sessionEvent', { type: 'retry', cli, role, requestId: item.message.requestId });
        return await this.executeItem(cli, role, entry, item);
      }
      entry.failures += 1;
      this.emit('sessionEvent', { type: 'failed', cli, role, requestId: item.message.requestId });
      throw err;
    } finally {
      if (softTimer) clearTimeout(softTimer);
      if (hardTimer) clearTimeout(hardTimer);
      entry.busy = false;
      entry.suppressOutput = false;
    }
  }

  private isRecoverable(error: Error): boolean {
    const message = error.message.toLowerCase();
    return (
      message.includes('timeout') ||
      message.includes('protocol') ||
      message.includes('output overflow') ||
      message.includes('session exited')
    );
  }

  private async restartSession(
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    reason: string
  ): Promise<ManagedSession> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (entry) {
      await entry.process.stop();
      this.sessions.delete(key);
      this.emit('sessionEvent', { type: 'restart', cli, role, reason });
    }
    await this.startSession(cli, role);
    const next = this.sessions.get(key);
    if (!next) {
      throw new Error(`${cli} session restart failed`);
    }
    if (entry?.lastSnapshot) {
      await this.rehydrateSession(cli, role, next, entry.lastSnapshot);
    }
    return next;
  }

  private extractSnapshot(metadata?: Record<string, unknown>): string | null {
    const value = metadata?.['contextSnapshot'];
    if (!value || typeof value !== 'string') return null;
    return value.length > 6000 ? value.slice(0, 6000) : value;
  }

  private async rehydrateSession(
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    entry: ManagedSession,
    snapshot: string
  ): Promise<void> {
    const requestId = uuidv4();
    const content = [
      '会话恢复：以下是最近上下文摘要，仅用于记忆，不要执行任务或修改文件。',
      '',
      snapshot.trim(),
      '',
      '请回复“OK”。'
    ].join('\n');
    const message: SessionMessage = {
      requestId,
      cli,
      role,
      content,
      metadata: { intent: 'rehydrate' },
      silent: true,
    };
    entry.suppressOutput = true;
    try {
      await entry.process.send(message);
      this.emit('sessionEvent', { type: 'rehydrate', cli, role, requestId });
    } catch (error) {
      const err = error instanceof Error ? error.message : String(error);
      this.emit('sessionEvent', { type: 'rehydrate_failed', cli, role, requestId, error: err });
    } finally {
      entry.suppressOutput = false;
    }
  }

  private startHealthMonitor(cli: CLIType, role: 'worker' | 'orchestrator'): NodeJS.Timeout {
    const intervalMs = this.options.heartbeatMs ?? 15000;
    return setInterval(() => {
      const key = this.getKey(cli, role);
      const entry = this.sessions.get(key);
      if (!entry) return;
      if (!entry.process.isAlive) {
        void this.restartSession(cli, role, 'not_alive');
        return;
      }
      if (entry.busy && entry.lastStartAt) {
        const elapsed = Date.now() - entry.lastStartAt;
        if (elapsed > this.hardTimeoutMs + 5000) {
          void this.restartSession(cli, role, 'stuck');
        }
      }
    }, intervalMs);
  }
}
