/**
 * ProcessPool - 进程预热池
 * 
 * 通过预先启动 Claude CLI 进程来减少响应延迟。
 * 
 * 工作原理：
 * 1. 当前请求处理完成后，后台预启动下一个进程
 * 2. 进程启动后进入"就绪"状态，等待消息
 * 3. 新请求到来时，从池中获取已就绪的进程
 * 4. 空闲超时的进程自动销毁
 */

import { EventEmitter } from 'events';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';
import type { CLIType } from '../types';

/**
 * 预热进程状态
 */
export type WarmProcessState = 'warming' | 'ready' | 'busy' | 'expired' | 'error';

/**
 * 预热进程封装
 */
export interface WarmProcess {
  /** 进程实例 */
  process: ChildProcessWithoutNullStreams;
  /** 进程状态 */
  state: WarmProcessState;
  /** CLI 类型 */
  cli: CLIType;
  /** 角色 */
  role: 'orchestrator' | 'worker';
  /** 创建时间 */
  createdAt: number;
  /** 最后使用时间 */
  lastUsedAt: number;
  /** 会话 ID */
  sessionId: string;
  /** 初始化输出（用于检测就绪状态） */
  initOutput: string;
}

/**
 * 进程池配置
 */
export interface ProcessPoolOptions {
  /** 工作目录 */
  cwd: string;
  /** 环境变量 */
  env?: Record<string, string>;
  /** 每个 CLI/角色 的最大池大小 */
  maxPoolSize?: number;
  /** 预热超时时间（毫秒） */
  warmupTimeoutMs?: number;
  /** 空闲超时时间（毫秒） */
  idleTimeoutMs?: number;
  /** 命令覆盖 */
  commandOverrides?: Partial<Record<CLIType, string>>;
}

/**
 * 进程池管理器
 */
export class ProcessPool extends EventEmitter {
  private readonly cwd: string;
  private readonly env: Record<string, string>;
  private readonly maxPoolSize: number;
  private readonly warmupTimeoutMs: number;
  private readonly idleTimeoutMs: number;
  private readonly commandOverrides: Partial<Record<CLIType, string>>;
  
  /** 进程池：key = `${cli}-${role}` */
  private pool: Map<string, WarmProcess[]> = new Map();
  /** 清理定时器 */
  private cleanupInterval?: NodeJS.Timeout;
  /** 是否已关闭 */
  private closed = false;

  constructor(options: ProcessPoolOptions) {
    super();
    this.cwd = options.cwd;
    this.maxPoolSize = options.maxPoolSize ?? 2;
    this.warmupTimeoutMs = options.warmupTimeoutMs ?? 30000;
    this.idleTimeoutMs = options.idleTimeoutMs ?? 60000;
    this.commandOverrides = options.commandOverrides ?? {};

    // 合并环境变量
    const mergedEnv: Record<string, string> = {};
    const rawEnv = { ...process.env, ...options.env };
    Object.entries(rawEnv).forEach(([key, value]) => {
      if (value !== undefined) {
        mergedEnv[key] = value;
      }
    });
    this.env = mergedEnv;

    // 启动定期清理
    this.startCleanupInterval();
  }

  /**
   * 获取池键
   */
  private getKey(cli: CLIType, role: 'orchestrator' | 'worker'): string {
    return `${cli}-${role}`;
  }

  /**
   * 获取预热进程
   * 如果池中有就绪的进程，直接返回；否则创建新进程
   */
  async acquire(
    cli: CLIType,
    role: 'orchestrator' | 'worker',
    args: string[],
    sessionId: string
  ): Promise<WarmProcess | null> {
    if (this.closed) {
      return null;
    }

    const key = this.getKey(cli, role);
    const processes = this.pool.get(key) ?? [];

    // 查找就绪的进程
    const readyProcess = processes.find(p => p.state === 'ready');
    if (readyProcess) {
      readyProcess.state = 'busy';
      readyProcess.lastUsedAt = Date.now();
      this.emit('log', `[ProcessPool] 从池中获取预热进程: ${key}`);
      return readyProcess;
    }

    // 没有就绪进程，返回 null（调用者需要自己创建）
    this.emit('log', `[ProcessPool] 池中无就绪进程: ${key}`);
    return null;
  }

  /**
   * 释放进程（进程完成后调用）
   * 由于 Claude CLI -p 模式是单次执行，进程完成后直接销毁
   */
  release(warmProcess: WarmProcess): void {
    warmProcess.state = 'expired';
    this.destroyProcess(warmProcess);
  }

  /**
   * 预热进程
   * 在后台启动一个新进程，等待就绪
   */
  async warmup(
    cli: CLIType,
    role: 'orchestrator' | 'worker',
    args: string[],
    sessionId: string
  ): Promise<void> {
    if (this.closed) {
      return;
    }

    const key = this.getKey(cli, role);
    const processes = this.pool.get(key) ?? [];

    // 检查池是否已满
    const activeCount = processes.filter(p =>
      p.state === 'warming' || p.state === 'ready'
    ).length;

    if (activeCount >= this.maxPoolSize) {
      this.emit('log', `[ProcessPool] 池已满，跳过预热: ${key}`);
      return;
    }

    this.emit('log', `[ProcessPool] 开始预热进程: ${key}`);

    const command = this.commandOverrides[cli] ?? cli;

    // 启动进程但不发送消息（等待消息到来）
    const process = spawn(command, args, {
      cwd: this.cwd,
      env: this.env,
    });

    const warmProcess: WarmProcess = {
      process,
      state: 'warming',
      cli,
      role,
      createdAt: Date.now(),
      lastUsedAt: Date.now(),
      sessionId,
      initOutput: '',
    };

    // 添加到池
    if (!this.pool.has(key)) {
      this.pool.set(key, []);
    }
    this.pool.get(key)!.push(warmProcess);

    // 监听输出，检测就绪状态
    process.stdout.on('data', (data) => {
      warmProcess.initOutput += data.toString();
      // 检测 stream-json 的 init 消息，表示进程已就绪
      if (warmProcess.state === 'warming' && warmProcess.initOutput.includes('"type":"system"')) {
        warmProcess.state = 'ready';
        this.emit('log', `[ProcessPool] 进程预热完成: ${key}`);
        this.emit('processReady', { cli, role, sessionId });
      }
    });

    process.stderr.on('data', (data) => {
      warmProcess.initOutput += data.toString();
    });

    process.on('error', (error) => {
      warmProcess.state = 'error';
      this.emit('log', `[ProcessPool] 进程预热失败: ${key} - ${error.message}`);
      this.removeFromPool(warmProcess);
    });

    process.on('close', (code) => {
      if (warmProcess.state === 'warming' || warmProcess.state === 'ready') {
        this.emit('log', `[ProcessPool] 预热进程意外退出: ${key} (code: ${code})`);
      }
      this.removeFromPool(warmProcess);
    });

    // 预热超时
    setTimeout(() => {
      if (warmProcess.state === 'warming') {
        warmProcess.state = 'expired';
        this.emit('log', `[ProcessPool] 进程预热超时: ${key}`);
        this.destroyProcess(warmProcess);
      }
    }, this.warmupTimeoutMs);
  }

  /**
   * 从池中移除进程
   */
  private removeFromPool(warmProcess: WarmProcess): void {
    const key = this.getKey(warmProcess.cli, warmProcess.role);
    const processes = this.pool.get(key);
    if (processes) {
      const index = processes.indexOf(warmProcess);
      if (index !== -1) {
        processes.splice(index, 1);
      }
    }
  }

  /**
   * 销毁进程
   */
  private destroyProcess(warmProcess: WarmProcess): void {
    if (warmProcess.process && !warmProcess.process.killed) {
      warmProcess.process.kill('SIGTERM');
    }
    this.removeFromPool(warmProcess);
  }

  /**
   * 启动定期清理
   */
  private startCleanupInterval(): void {
    this.cleanupInterval = setInterval(() => {
      this.cleanup();
    }, 30000); // 每 30 秒清理一次
  }

  /**
   * 清理过期进程
   */
  cleanup(): void {
    const now = Date.now();

    for (const [key, processes] of this.pool.entries()) {
      const toRemove: WarmProcess[] = [];

      for (const warmProcess of processes) {
        // 检查空闲超时
        if (warmProcess.state === 'ready' &&
            now - warmProcess.lastUsedAt > this.idleTimeoutMs) {
          warmProcess.state = 'expired';
          toRemove.push(warmProcess);
          this.emit('log', `[ProcessPool] 进程空闲超时: ${key}`);
        }
      }

      // 销毁过期进程
      for (const warmProcess of toRemove) {
        this.destroyProcess(warmProcess);
      }
    }
  }

  /**
   * 获取池状态
   */
  getStats(): Record<string, { warming: number; ready: number; busy: number }> {
    const stats: Record<string, { warming: number; ready: number; busy: number }> = {};

    for (const [key, processes] of this.pool.entries()) {
      stats[key] = {
        warming: processes.filter(p => p.state === 'warming').length,
        ready: processes.filter(p => p.state === 'ready').length,
        busy: processes.filter(p => p.state === 'busy').length,
      };
    }

    return stats;
  }

  /**
   * 关闭进程池
   */
  async close(): Promise<void> {
    this.closed = true;

    if (this.cleanupInterval) {
      clearInterval(this.cleanupInterval);
      this.cleanupInterval = undefined;
    }

    // 销毁所有进程
    for (const processes of this.pool.values()) {
      for (const warmProcess of processes) {
        this.destroyProcess(warmProcess);
      }
    }

    this.pool.clear();
    this.emit('log', '[ProcessPool] 进程池已关闭');
  }
}
