/**
 * Node Shell Executor
 * 基于 Node.js child_process 的 IShellExecutor 实现
 *
 * 纯命令执行模型（非交互式）：
 * - 每次 process_launch 直接 spawn(shell, shellArgs)
 * - 通过 stdout/stderr pipe 事件驱动捕获输出
 * - 不依赖 script 命令、pgrep、ANSI marker 或任何 PTY 能力
 * - 不依赖 VSCode 的任何 API
 */

import { spawn, ChildProcess } from 'child_process';
import { EventEmitter } from 'events';
import * as fs from 'fs';
import * as path from 'path';
import {
  KillProcessResult,
  LaunchProcessOptions,
  LaunchProcessResult,
  ProcessPhase,
  ProcessRunMode,
  ReadProcessResult,
  WriteProcessResult,
} from '../types';
import { logger, LogCategory } from '../../logging';
import {
  TerminalProcess,
  ProcessState,
} from '../terminal/types';
import type { IShellExecutor, ProcessRecord, ShellProcessEvents, ShellEventListener } from './types';

// ============================================================================
// 常量
// ============================================================================

/** 进程状态轮询间隔 (ms) */
const PROCESS_WAIT_POLL_MS = 100;
/** 兜底总时长硬上限 (ms) */
const PROCESS_HARD_TIMEOUT_MS = 6 * 60 * 60 * 1000; // 6 小时
/** 输出缓冲上限（字符数） */
const PROCESS_OUTPUT_BUFFER_LIMIT = 200_000;
/** service 默认启动握手等待秒数 */
const SERVICE_STARTUP_WAIT_SECONDS_DEFAULT = 5;
/** service 默认就绪信号 */
const DEFAULT_SERVICE_READY_PATTERNS: RegExp[] = [
  /ready/i,
  /listening/i,
  /running at/i,
  /server started/i,
  /compiled successfully/i,
  /local:\s*https?:\/\//i,
  /dev server/i,
];

/** 允许的 agent 终端名称 */
const ALLOWED_AGENT_TERMINAL_NAMES = new Set([
  'orchestrator',
  'worker-claude',
  'worker-gemini',
  'worker-codex',
]);

// ============================================================================
// 内部类型
// ============================================================================

interface ServiceRuntimeState {
  readyPatterns: RegExp[];
  startupStatus: 'pending' | 'confirmed' | 'timeout' | 'failed' | 'skipped';
  startupConfirmed: boolean;
  startupMessage?: string;
  startupDeadlineAt?: number;
  lastHeartbeatAt: number;
}

interface ShellLaunchSpec {
  executable: string;
  args: string[];
  env: NodeJS.ProcessEnv;
}

function resolveWindowsShellExecutable(): string {
  const explicitShell = (process.env.MAGI_SHELL || process.env.MAGI_WINDOWS_SHELL || '').trim();
  if (explicitShell) return explicitShell;

  const comspec = (process.env.ComSpec || '').trim();
  if (comspec) {
    if (!path.isAbsolute(comspec) || fs.existsSync(comspec)) {
      return comspec;
    }
  }

  return 'powershell.exe';
}

function resolveUnixShellExecutable(): string {
  const candidates = [
    process.env.MAGI_SHELL,
    process.env.SHELL,
    '/bin/bash',
    '/bin/zsh',
    '/bin/sh',
  ];

  for (const candidate of candidates) {
    if (typeof candidate !== 'string') continue;
    const normalized = candidate.trim();
    if (!normalized) continue;
    if (path.isAbsolute(normalized) && !fs.existsSync(normalized)) {
      continue;
    }
    return normalized;
  }

  return '/bin/sh';
}

function resolveShellLaunchSpec(command: string): ShellLaunchSpec {
  const baseEnv: NodeJS.ProcessEnv = { ...process.env };
  const platform = process.platform;

  if (platform === 'win32') {
    delete baseEnv.TERM;
    const shellExecutable = resolveWindowsShellExecutable();
    const shellName = path.basename(shellExecutable).toLowerCase();

    if (shellName === 'cmd.exe' || shellName === 'cmd') {
      return {
        executable: shellExecutable,
        args: ['/d', '/s', '/c', command],
        env: baseEnv,
      };
    }

    return {
      executable: shellExecutable,
      args: ['-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', command],
      env: baseEnv,
    };
  }

  baseEnv.TERM = baseEnv.TERM || 'dumb';
  const shellExecutable = resolveUnixShellExecutable();
  return {
    executable: shellExecutable,
    args: ['-lc', command],
    env: baseEnv,
  };
}

// ============================================================================
// NodeShellExecutor
// ============================================================================

/**
 * 基于 Node.js child_process 的 Shell 执行器
 *
 * 核心设计：
 * - 每次 process_launch 直接 spawn 一个一次性子进程（严格进程隔离）
 * - stdout/stderr pipe 事件驱动，输出实时推送
 * - task 模式：等待子进程退出
 * - service 模式：后台运行，通过 ready pattern 检测启动成功
 * - 通过 EventEmitter 向上层推送 processOutput/processCompleted 事件
 */
export class NodeShellExecutor implements IShellExecutor {
  private processes: Map<number, TerminalProcess> = new Map();
  private childProcesses: Map<number, ChildProcess> = new Map();
  private nextId: number = 1;
  private readonly defaultTimeout: number = 30000;
  private readonly maxTimeout: number = 3600000;

  private stopProcessTasks: Map<number, Promise<void>> = new Map();
  private serviceRuntime: Map<number, ServiceRuntimeState> = new Map();

  private emitter = new EventEmitter();

  constructor() {
    this.emitter.setMaxListeners(50);
  }

  // ============================================================================
  // IShellExecutor 事件接口
  // ============================================================================

  on<K extends keyof ShellProcessEvents>(event: K, listener: ShellEventListener<K>): void {
    this.emitter.on(event, listener);
  }

  off<K extends keyof ShellProcessEvents>(event: K, listener: ShellEventListener<K>): void {
    this.emitter.off(event, listener);
  }

  private emitProcessOutput(processId: number, output: string, fromCursor: number, cursor: number): void {
    this.emitter.emit('processOutput', { processId, output, fromCursor, cursor });
  }

  private emitProcessCompleted(proc: TerminalProcess): void {
    this.emitter.emit('processCompleted', {
      processId: proc.id,
      state: proc.state,
      exitCode: proc.exitCode,
      output: proc.output,
      cursor: proc.outputCursor,
    });
  }

  // ============================================================================
  // IShellExecutor 接口实现
  // ============================================================================

  validateCommand(command: string): { valid: boolean; reason?: string } {
    const dangerousRules: Array<{ pattern: RegExp; reason: string }> = [
      { pattern: /rm\s+-rf\s+\//, reason: '命令包含系统级危险操作：删除根目录' },
      { pattern: /:\(\)\{.*\}/, reason: '命令包含系统级危险操作：fork bomb' },
      { pattern: />\s*\/dev\/sda/, reason: '命令包含系统级危险操作：写入磁盘设备' },
    ];

    for (const rule of dangerousRules) {
      if (rule.pattern.test(command)) {
        return { valid: false, reason: rule.reason };
      }
    }

    return { valid: true };
  }

  async launchProcess(options: LaunchProcessOptions, signal?: AbortSignal): Promise<LaunchProcessResult> {
    const agentName = (options.name || '').trim();
    if (!agentName) {
      throw new Error('process_launch 必须提供 agent 终端名称（orchestrator、worker-claude、worker-gemini、worker-codex）');
    }
    if (!ALLOWED_AGENT_TERMINAL_NAMES.has(agentName)) {
      throw new Error('process_launch name 仅支持 orchestrator、worker-claude、worker-gemini、worker-codex');
    }

    const runMode: ProcessRunMode = options.runMode ?? (options.wait ? 'task' : 'service');
    const startupWaitSeconds = Number.isFinite(options.startupWaitSeconds)
      ? Math.max(0, options.startupWaitSeconds as number)
      : SERVICE_STARTUP_WAIT_SECONDS_DEFAULT;
    const readyPatterns = this.compileReadyPatterns(options.readyPatterns);
    const processId = this.nextId++;
    const terminalName = `${agentName}-p${processId}`;
    const now = Date.now();

    const proc: TerminalProcess = {
      id: processId,
      cwd: options.cwd,
      command: options.command,
      startTime: now,
      output: '',
      outputCursor: 0,
      outputStartCursor: 0,
      exitCode: null,
      state: 'starting',
      updatedAt: now,
      runMode,
      agentName,
      terminalName,
      serviceLocked: runMode === 'service',
    };
    this.processes.set(processId, proc);

    if (runMode === 'service') {
      const startupStatus: ServiceRuntimeState['startupStatus'] = options.wait ? 'pending' : 'skipped';
      this.serviceRuntime.set(proc.id, {
        readyPatterns,
        startupStatus,
        startupConfirmed: false,
        startupDeadlineAt: options.wait ? Date.now() + startupWaitSeconds * 1000 : undefined,
        startupMessage: options.wait
          ? `等待服务启动握手（${startupWaitSeconds}s）`
          : '未等待启动握手（wait=false）',
        lastHeartbeatAt: Date.now(),
      });
    }

    // 启动子进程
    this.spawnChildProcess(proc);

    // 根据模式等待
    if (options.wait) {
      if (runMode === 'task') {
        const idleTimeoutMs = this.normalizeIdleTimeoutMs(options.maxWaitSeconds);
        await this.waitForProcessCompletion(processId, idleTimeoutMs, signal);
      } else {
        const startupTimeoutMs = Math.max(1, startupWaitSeconds) * 1000;
        await this.waitForServiceStartup(processId, startupTimeoutMs, signal);
      }
    }

    return this.buildLaunchResult(proc);
  }

  async readProcess(
    terminalId: number,
    wait: boolean,
    maxWaitSeconds: number,
    fromCursor?: number,
    signal?: AbortSignal,
  ): Promise<ReadProcessResult> {
    const proc = this.processes.get(terminalId);
    if (!proc) {
      throw new Error(`终端进程不存在: ${terminalId}`);
    }

    if (wait && (proc.state === 'running' || proc.state === 'starting')) {
      const idleTimeoutMs = this.normalizeIdleTimeoutMs(maxWaitSeconds);
      const requestedCursor = Number.isInteger(fromCursor) && fromCursor !== undefined && fromCursor >= 0
        ? fromCursor
        : proc.outputCursor;
      await this.waitForProgress(terminalId, requestedCursor, idleTimeoutMs, signal);
    }

    return this.buildReadResult(proc, fromCursor);
  }

  async writeProcess(terminalId: number, inputText: string): Promise<WriteProcessResult> {
    const proc = this.processes.get(terminalId);
    if (!proc) {
      throw new Error(`终端进程不存在: ${terminalId}`);
    }

    if (proc.state !== 'running') {
      return {
        accepted: false,
        status: proc.state,
        run_mode: proc.runMode,
        terminal_name: proc.terminalName,
        message: 'process 非 running 状态，无法写入',
      };
    }

    const child = this.childProcesses.get(terminalId);
    if (!child || !child.stdin) {
      return {
        accepted: false,
        status: proc.state,
        run_mode: proc.runMode,
        terminal_name: proc.terminalName,
        message: '子进程 stdin 不可用',
      };
    }

    child.stdin.write(inputText);
    return {
      accepted: true,
      status: proc.state,
      run_mode: proc.runMode,
      terminal_name: proc.terminalName,
    };
  }

  async killProcess(terminalId: number): Promise<KillProcessResult> {
    const proc = this.processes.get(terminalId);
    if (!proc) {
      return {
        killed: false,
        final_output: '',
        return_code: null,
        released_lock: false,
      };
    }

    const hadLock = proc.serviceLocked;
    await this.forceStopProcess(proc, 'killed', 'process_kill');

    return {
      killed: true,
      final_output: proc.output,
      return_code: proc.exitCode,
      run_mode: proc.runMode,
      terminal_name: proc.terminalName,
      released_lock: hadLock,
    };
  }

  listProcessRecords(): ProcessRecord[] {
    const now = Date.now();
    const result: ProcessRecord[] = [];

    for (const [id, proc] of this.processes.entries()) {
      const endTime = proc.endTime ?? now;
      result.push({
        terminal_id: id,
        status: proc.state,
        command: proc.command,
        cwd: proc.cwd,
        started_at: proc.startTime,
        elapsed_seconds: Math.round((endTime - proc.startTime) / 1000),
        run_mode: proc.runMode,
        phase: this.getProcessPhase(proc),
        locked: proc.serviceLocked,
        terminal_name: proc.terminalName,
        return_code: proc.exitCode,
        output_cursor: proc.outputCursor,
      });
    }

    return result;
  }

  dispose(): void {
    for (const [id, child] of this.childProcesses.entries()) {
      try {
        if (child.exitCode === null && !child.killed) {
          this.terminateChildProcess(child);
        }
      } catch {
        // 进程可能已退出
      }
      this.childProcesses.delete(id);
    }
    this.processes.clear();
    this.stopProcessTasks.clear();
    this.serviceRuntime.clear();
    this.emitter.removeAllListeners();
  }

  // ============================================================================
  // 子进程管理
  // ============================================================================

  /**
   * 启动子进程
   *
   * 核心路径：spawn(shell, shellArgs)
   * - stdout/stderr pipe 事件驱动捕获输出
   * - close 事件触发终态转换
   */
  private spawnChildProcess(proc: TerminalProcess): void {
    const cwd = proc.cwd || process.cwd();
    const shellSpec = resolveShellLaunchSpec(proc.command);

    const child = spawn(shellSpec.executable, shellSpec.args, {
      cwd,
      env: shellSpec.env,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: process.platform === 'win32',
    });
    this.childProcesses.set(proc.id, child);

    proc.state = 'running';

    const appendChunk = (chunk: Buffer | string): void => {
      const text = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
      if (!text) return;
      const fromCursor = proc.outputCursor;
      this.appendOutput(proc, text);
      this.emitProcessOutput(proc.id, text, fromCursor, proc.outputCursor);
    };

    child.stdout?.on('data', appendChunk);
    child.stderr?.on('data', appendChunk);

    child.once('error', (error) => {
      if (this.isTerminalState(proc.state)) return;
      const message = error?.message || String(error);
      if (message && !proc.output.includes(message)) {
        this.appendOutput(proc, message);
      }
      proc.exitCode = 1;
      proc.state = 'failed';
      proc.endTime = Date.now();
      this.childProcesses.delete(proc.id);
      this.releaseServiceLock(proc);
      this.emitProcessCompleted(proc);
    });

    child.once('close', (code, signal) => {
      if (this.isTerminalState(proc.state)) return;
      this.childProcesses.delete(proc.id);

      if (signal && (code === null || code === undefined)) {
        proc.exitCode = 1;
        if (!proc.output.trim()) {
          this.appendOutput(proc, `process terminated by signal: ${signal}`);
        }
      } else {
        proc.exitCode = Number.isFinite(code as number) ? (code as number) : 0;
      }

      if (proc.runMode === 'service') {
        // service 退出 = 异常终止
        proc.state = 'failed';
        this.updateServiceStartupStatus(proc, 'failed', '服务进程已退出');
      } else {
        proc.state = proc.exitCode === 0 ? 'completed' : 'failed';
      }
      proc.endTime = Date.now();
      this.releaseServiceLock(proc);
      this.emitProcessCompleted(proc);
    });

    // service 模式：监控就绪状态
    if (proc.runMode === 'service') {
      this.setupServiceReadinessMonitor(proc);
    }

    logger.debug('子进程已启动', {
      processId: proc.id,
      command: proc.command,
      cwd,
      runMode: proc.runMode,
      pid: child.pid,
    }, LogCategory.SHELL);
  }

  // ============================================================================
  // 进程等待
  // ============================================================================

  /**
   * 等待 task 进程完成或超时
   */
  private async waitForProcessCompletion(
    processId: number,
    idleTimeoutMs: number,
    signal?: AbortSignal,
  ): Promise<void> {
    while (true) {
      if (signal?.aborted) return;

      const proc = this.processes.get(processId);
      if (!proc) return;
      if (this.isTerminalState(proc.state)) return;

      const now = Date.now();
      const lastActivityAt = proc.updatedAt ?? proc.startTime;
      if (now - lastActivityAt >= idleTimeoutMs) return;
      if (now - proc.startTime >= PROCESS_HARD_TIMEOUT_MS) return;

      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  /**
   * 等待 service 启动握手
   */
  private async waitForServiceStartup(
    processId: number,
    timeoutMs: number,
    signal?: AbortSignal,
  ): Promise<void> {
    const startedAt = Date.now();

    while (true) {
      if (signal?.aborted) {
        const proc = this.processes.get(processId);
        if (proc && !this.isTerminalState(proc.state)) {
          this.updateServiceStartupStatus(proc, 'skipped', '启动握手等待被中断');
        }
        return;
      }

      const proc = this.processes.get(processId);
      if (!proc) return;
      if (this.isTerminalState(proc.state)) return;

      const runtime = this.serviceRuntime.get(processId);
      if (runtime) {
        if (
          runtime.startupStatus === 'confirmed'
          || runtime.startupStatus === 'failed'
          || runtime.startupStatus === 'timeout'
          || runtime.startupStatus === 'skipped'
        ) {
          return;
        }
      }

      if (Date.now() - startedAt >= timeoutMs) {
        this.updateServiceStartupStatus(proc, 'timeout', `启动握手超时（${Math.ceil(timeoutMs / 1000)}s）`);
        return;
      }

      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  /**
   * 等待进程输出进展（process_read wait=true）
   */
  private async waitForProgress(
    processId: number,
    requestedCursor: number,
    idleTimeoutMs: number,
    signal?: AbortSignal,
  ): Promise<void> {
    const startAt = Date.now();

    while (true) {
      if (signal?.aborted) return;

      const proc = this.processes.get(processId);
      if (!proc) return;
      if (this.isTerminalState(proc.state)) return;
      if (proc.outputCursor > requestedCursor) return;

      if (Date.now() - startAt >= idleTimeoutMs) return;
      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  // ============================================================================
  // 进程终止
  // ============================================================================

  private async forceStopProcess(
    proc: TerminalProcess,
    targetState: 'killed' | 'timeout',
    reason: string,
  ): Promise<void> {
    const existing = this.stopProcessTasks.get(proc.id);
    if (existing) {
      await existing;
      return;
    }

    const stopTask = (async () => {
      if (this.isTerminalState(proc.state)) return;

      logger.warn('强制停止进程', {
        processId: proc.id,
        reason,
        targetState,
      }, LogCategory.SHELL);

      const child = this.childProcesses.get(proc.id);
      if (child && child.exitCode === null && !child.killed) {
        await this.terminateChildProcess(child);
      }
      this.childProcesses.delete(proc.id);

      proc.state = targetState;
      proc.exitCode = -1;
      proc.endTime = Date.now();
      this.releaseServiceLock(proc);
      this.updateServiceStartupStatus(proc, targetState === 'killed' ? 'failed' : 'timeout',
        `进程已${targetState === 'killed' ? '终止' : '超时终止'}`);
      this.emitProcessCompleted(proc);
    })().finally(() => {
      this.stopProcessTasks.delete(proc.id);
    });

    this.stopProcessTasks.set(proc.id, stopTask);
    await stopTask;
  }

  // ============================================================================
  // Service 就绪检测
  // ============================================================================

  private setupServiceReadinessMonitor(proc: TerminalProcess): void {
    const runtime = this.serviceRuntime.get(proc.id);
    if (!runtime) return;

    // 利用 processOutput 事件检测就绪信号
    const listener = (event: { processId: number }) => {
      if (event.processId !== proc.id) return;
      if (runtime.startupConfirmed) return;

      if (this.hasServiceReadySignal(proc.output, runtime.readyPatterns)) {
        this.updateServiceStartupStatus(proc, 'confirmed', '检测到服务就绪信号');
        this.emitter.off('processOutput', listener);
      }

      // 检查超时
      if (
        runtime.startupStatus === 'pending'
        && runtime.startupDeadlineAt
        && Date.now() >= runtime.startupDeadlineAt
      ) {
        const waitSeconds = Math.max(1, Math.ceil((runtime.startupDeadlineAt - proc.startTime) / 1000));
        this.updateServiceStartupStatus(proc, 'timeout', `启动握手超时（${waitSeconds}s）`);
        this.emitter.off('processOutput', listener);
      }
    };

    this.emitter.on('processOutput', listener);

    // 进程结束时移除监听
    const completedListener = (event: { processId: number }) => {
      if (event.processId !== proc.id) return;
      this.emitter.off('processOutput', listener);
      this.emitter.off('processCompleted', completedListener);
    };
    this.emitter.on('processCompleted', completedListener);
  }

  private updateServiceStartupStatus(
    proc: TerminalProcess,
    status: ServiceRuntimeState['startupStatus'],
    message?: string,
  ): void {
    if (proc.runMode !== 'service') return;

    const runtime = this.serviceRuntime.get(proc.id);
    if (!runtime) return;

    runtime.startupStatus = status;
    runtime.startupConfirmed = status === 'confirmed';
    if (status !== 'pending') {
      runtime.startupDeadlineAt = undefined;
    }

    if (message) {
      runtime.startupMessage = message;
    } else if (status === 'confirmed') {
      runtime.startupMessage = '服务启动成功，已确认就绪';
    } else if (status === 'failed') {
      runtime.startupMessage = '服务启动失败';
    }
  }

  private hasServiceReadySignal(output: string, readyPatterns?: RegExp[]): boolean {
    if (!output) return false;
    const patterns = readyPatterns && readyPatterns.length > 0
      ? readyPatterns
      : DEFAULT_SERVICE_READY_PATTERNS;
    return patterns.some((pattern) => pattern.test(output));
  }

  // ============================================================================
  // 输出管理
  // ============================================================================

  private appendOutput(proc: TerminalProcess, text: string): void {
    proc.output += text;
    proc.outputCursor += text.length;
    this.trimOutputBuffer(proc);
    proc.updatedAt = Date.now();
  }

  private trimOutputBuffer(proc: TerminalProcess): void {
    if (proc.output.length > PROCESS_OUTPUT_BUFFER_LIMIT) {
      const overflow = proc.output.length - PROCESS_OUTPUT_BUFFER_LIMIT;
      proc.output = proc.output.slice(overflow);
      proc.outputStartCursor += overflow;
    }
  }

  // ============================================================================
  // 结果构建
  // ============================================================================

  private buildLaunchResult(proc: TerminalProcess): LaunchProcessResult {
    const runtime = this.serviceRuntime.get(proc.id);
    return {
      terminal_id: proc.id,
      status: proc.state,
      output: proc.output,
      return_code: proc.exitCode,
      run_mode: proc.runMode,
      phase: this.getProcessPhase(proc),
      locked: proc.serviceLocked,
      terminal_name: proc.terminalName,
      cwd: proc.cwd,
      output_cursor: proc.outputCursor,
      output_start_cursor: proc.outputStartCursor,
      message: proc.runMode === 'service'
        ? 'service 进程后台运行，仅可通过对应 terminal_id 读取与终止。'
        : undefined,
      startup_status: runtime?.startupStatus,
      startup_confirmed: runtime?.startupConfirmed,
      startup_message: runtime?.startupMessage,
    };
  }

  private buildReadResult(proc: TerminalProcess, fromCursor?: number): ReadProcessResult {
    const normalizedFromCursor = Number.isInteger(fromCursor) && fromCursor !== undefined && fromCursor >= 0
      ? fromCursor
      : 0;
    const requestedStart = Math.min(normalizedFromCursor, proc.outputCursor);
    const clampedStart = Math.max(requestedStart, proc.outputStartCursor);
    const delta = fromCursor !== undefined;
    const relativeStart = Math.max(0, clampedStart - proc.outputStartCursor);
    const output = delta ? proc.output.slice(relativeStart) : proc.output;

    return {
      terminal_id: proc.id,
      status: proc.state,
      output,
      return_code: proc.exitCode,
      run_mode: proc.runMode,
      phase: this.getProcessPhase(proc),
      locked: proc.serviceLocked,
      terminal_name: proc.terminalName,
      cwd: proc.cwd,
      from_cursor: clampedStart,
      output_start_cursor: proc.outputStartCursor,
      next_cursor: proc.outputCursor,
      delta,
      truncated: delta && normalizedFromCursor < proc.outputStartCursor,
      output_cursor: proc.outputCursor,
    };
  }

  private getProcessPhase(proc: TerminalProcess): ProcessPhase {
    if (proc.state === 'starting') return 'starting';
    if (proc.state === 'running') {
      if (proc.runMode === 'service') {
        const runtime = this.serviceRuntime.get(proc.id);
        if (runtime?.startupConfirmed || this.hasServiceReadySignal(proc.output, runtime?.readyPatterns)) {
          return 'ready';
        }
      }
      return 'running';
    }
    if (proc.state === 'completed') return 'completed';
    if (proc.state === 'failed') return 'failed';
    if (proc.state === 'killed') return 'killed';
    return 'timeout';
  }

  // ============================================================================
  // 工具方法
  // ============================================================================

  private isTerminalState(state: ProcessState): boolean {
    return state === 'completed' || state === 'failed' || state === 'killed' || state === 'timeout';
  }

  private releaseServiceLock(proc: TerminalProcess): void {
    proc.serviceLocked = false;
  }

  private compileReadyPatterns(patterns?: string[]): RegExp[] {
    const result = [...DEFAULT_SERVICE_READY_PATTERNS];
    if (!Array.isArray(patterns)) return result;

    for (const raw of patterns) {
      if (typeof raw !== 'string' || !raw.trim()) continue;
      try {
        result.push(new RegExp(raw, 'i'));
      } catch {
        logger.debug('无效的 ready pattern，已跳过', { pattern: raw }, LogCategory.SHELL);
      }
    }

    return result;
  }

  private normalizeIdleTimeoutMs(maxWaitSeconds: number): number {
    const seconds = Number.isFinite(maxWaitSeconds) ? maxWaitSeconds : this.defaultTimeout / 1000;
    return Math.min(Math.max(seconds, 1) * 1000, this.maxTimeout);
  }

  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  private async terminateChildProcess(child: ChildProcess): Promise<void> {
    if (process.platform === 'win32' && Number.isInteger(child.pid) && (child.pid as number) > 0) {
      await new Promise<void>((resolve) => {
        const killer = spawn('taskkill', ['/pid', String(child.pid), '/t', '/f'], {
          stdio: 'ignore',
          windowsHide: true,
        });
        killer.once('error', () => {
          try {
            child.kill();
          } catch {
            // 进程可能已退出
          }
          resolve();
        });
        killer.once('close', () => resolve());
      });
      return;
    }

    try {
      child.kill('SIGTERM');
      await this.delay(200);
      if (child.exitCode === null && !child.killed) {
        child.kill('SIGKILL');
      }
    } catch {
      // 进程可能已退出
    }
  }
}
