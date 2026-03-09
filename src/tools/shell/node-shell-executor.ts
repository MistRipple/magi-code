/**
 * Node Shell Executor
 * 基于 Node.js child_process 的 IShellExecutor 实现
 *
 * 用于脱离 VSCode 的宿主环境（如 IDEA 插件、CLI 等）。
 * 使用 ScriptCaptureStrategy 做完成检测和输出捕获。
 */

import { spawn, ChildProcess } from 'child_process';
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
  ShellType,
  TerminalProcess,
  ProcessState,
} from '../terminal/types';
import type { IShellExecutor, IShellSession, ProcessRecord } from './types';
import { ScriptCaptureStrategy } from '../terminal/script-capture-strategy';

// ============================================================================
// Node Shell 会话
// ============================================================================

/**
 * 基于 child_process 的 IShellSession 实现
 *
 * 包装一个持久化的交互式 shell 进程（bash/zsh），
 * 通过 stdin.write 实现 sendText。
 */
class NodeShellSession implements IShellSession {
  private static nextId = 1;

  readonly id: string;
  readonly name: string;
  readonly shellProcess: ChildProcess;

  constructor(name: string, cwd?: string) {
    this.id = `node-shell-${NodeShellSession.nextId++}`;
    this.name = name;

    const shellPath = process.env.SHELL || '/bin/bash';
    this.shellProcess = spawn(shellPath, ['-i'], {
      cwd: cwd || process.cwd(),
      env: { ...process.env, TERM: 'dumb' },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  }

  sendText(text: string, addNewLine?: boolean): void {
    const suffix = addNewLine !== false ? '\n' : '';
    this.shellProcess.stdin?.write(text + suffix);
  }

  /**
   * 检查底层进程是否存活
   */
  isAlive(): boolean {
    return this.shellProcess.exitCode === null && !this.shellProcess.killed;
  }

  /**
   * 销毁底层 shell 进程
   */
  dispose(): void {
    if (this.isAlive()) {
      this.shellProcess.kill('SIGTERM');
    }
  }
}

// ============================================================================
// 常量
// ============================================================================

/** 轮询起始延迟 (ms) */
const POLL_START_DELAY_MS = 100;
/** 进程状态轮询间隔 (ms) */
const PROCESS_WAIT_POLL_MS = 100;
/** 兜底总时长硬上限 (ms) */
const PROCESS_HARD_TIMEOUT_MS = 6 * 60 * 60 * 1000; // 6 小时
/** service 后台监督轮询间隔 (ms) */
const SERVICE_SUPERVISOR_INTERVAL_MS = 1000;
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

/**
 * 检测 Shell 类型
 */
function detectShellType(): ShellType {
  const shellPath = (process.env.SHELL || '').toLowerCase();
  if (shellPath.includes('zsh')) return 'zsh';
  if (shellPath.includes('bash')) return 'bash';
  if (shellPath.includes('fish')) return 'fish';
  if (shellPath.includes('powershell') || shellPath.includes('pwsh')) return 'powershell';
  return 'bash';
}

// ============================================================================
// 内部类型
// ============================================================================

interface ServiceLease {
  processId: number;
  agentName: string;
  lockedAt: number;
}

interface ServiceRuntimeState {
  readyPatterns: RegExp[];
  startupStatus: 'pending' | 'confirmed' | 'timeout' | 'failed' | 'skipped';
  startupConfirmed: boolean;
  startupMessage?: string;
  startupDeadlineAt?: number;
  lastHeartbeatAt: number;
}

// ============================================================================
// NodeShellExecutor
// ============================================================================

/**
 * 基于 Node.js child_process 的 Shell 执行器
 *
 * 核心设计与统一 Shell 执行语义保持一致：
 * - 每个 agent（orchestrator、worker-claude 等）有独立的 shell 进程
 * - 使用 ScriptCaptureStrategy 做命令完成检测和输出捕获
 * - 支持 task/service 两种运行模式
 * - 支持主终端 + 溢出池的分配策略
 */
export class NodeShellExecutor implements IShellExecutor {
  private processes: Map<number, TerminalProcess> = new Map();
  private nextId: number = 1;
  private readonly defaultTimeout: number = 30000; // 30 秒
  private readonly maxTimeout: number = 3600000;   // 1 小时

  // 会话管理
  private managedSessions: Set<NodeShellSession> = new Set();
  private agentSessions: Map<string, NodeShellSession> = new Map();
  /** 溢出终端池：主会话被占用时分配 */
  private agentOverflowSessions: Map<string, Set<NodeShellSession>> = new Map();
  private sessionCwds: Map<NodeShellSession, string | undefined> = new Map();
  private sessionAgentNames: Map<NodeShellSession, string> = new Map();
  private sessionInitialized: Map<NodeShellSession, boolean> = new Map();
  private sessionShellType: Map<NodeShellSession, ShellType> = new Map();

  // 完成检测策略（仅 ScriptCapture，不依赖 VSCode）
  private scriptCaptureStrategy: ScriptCaptureStrategy;

  // 进程管理
  private stopProcessTasks: Map<number, Promise<void>> = new Map();
  /** ScriptCapture 不可用时，task 模式的直连子进程（可靠输出兜底） */
  private directTaskProcesses: Map<number, ChildProcess> = new Map();
  private serviceLeases: Map<NodeShellSession, ServiceLease> = new Map();
  private serviceRuntime: Map<number, ServiceRuntimeState> = new Map();
  private serviceSupervisorTimer: NodeJS.Timeout | null = null;
  private serviceSupervisorTickInFlight = false;

  constructor() {
    this.scriptCaptureStrategy = new ScriptCaptureStrategy();
    this.serviceSupervisorTimer = setInterval(() => {
      void this.runServiceSupervisorTick();
    }, SERVICE_SUPERVISOR_INTERVAL_MS);
  }

  // ============================================================================
  // IShellExecutor 接口实现
  // ============================================================================

  /**
   * 验证命令安全性
   *
   * 仅拦截系统安全级威胁（rm -rf /、fork bomb 等）。
   */
  validateCommand(command: string): { valid: boolean; reason?: string } {
    const dangerousRules: Array<{ pattern: RegExp; reason: string }> = [
      {
        pattern: /rm\s+-rf\s+\//,
        reason: '命令包含系统级危险操作：删除根目录',
      },
      {
        pattern: /:\(\)\{.*\}/,
        reason: '命令包含系统级危险操作：fork bomb',
      },
      {
        pattern: />\s*\/dev\/sda/,
        reason: '命令包含系统级危险操作：写入磁盘设备',
      },
    ];

    for (const rule of dangerousRules) {
      if (rule.pattern.test(command)) {
        return { valid: false, reason: rule.reason };
      }
    }

    return { valid: true };
  }

  async launchProcess(options: LaunchProcessOptions, signal?: AbortSignal): Promise<LaunchProcessResult> {
    const idleTimeoutMs = this.normalizeIdleTimeoutMs(options.maxWaitSeconds);
    const agentName = (options.name || '').trim();
    if (!agentName) {
      throw new Error('launch-process 必须提供 agent 终端名称（orchestrator、worker-claude、worker-gemini、worker-codex）');
    }
    if (!ALLOWED_AGENT_TERMINAL_NAMES.has(agentName)) {
      throw new Error('launch-process name 仅支持 orchestrator、worker-claude、worker-gemini、worker-codex');
    }

    const runMode: ProcessRunMode = options.runMode ?? (options.wait ? 'task' : 'service');
    const startupWaitSeconds = Number.isFinite(options.startupWaitSeconds)
      ? Math.max(0, options.startupWaitSeconds as number)
      : SERVICE_STARTUP_WAIT_SECONDS_DEFAULT;
    const readyPatterns = this.compileReadyPatterns(options.readyPatterns);
    const session = await this.getOrCreateSession({
      cwd: options.cwd,
      name: agentName,
    });

    const processId = this.nextId++;
    const now = Date.now();
    const proc: TerminalProcess = {
      id: processId,
      session,
      command: options.command,
      actualCommand: options.command,
      lastCommand: '',
      startTime: now,
      output: '',
      outputCursor: 0,
      outputStartCursor: 0,
      exitCode: null,
      state: 'starting',
      updatedAt: now,
      runMode,
      agentName,
      terminalName: session.name,
      serviceLocked: false,
    };
    this.processes.set(processId, proc);

    if (runMode === 'service') {
      this.acquireServiceLease(proc);
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

    proc.state = 'running';
    void this.executeCommand(proc, options.command)
      .then(() => {
        if (proc.state !== 'running' && proc.state !== 'starting') {
          return;
        }
        if (proc.runMode === 'service') {
          proc.state = 'running';
          proc.endTime = undefined;
          return;
        }
        proc.state = proc.exitCode === 0 ? 'completed' : 'failed';
      })
      .catch((error: any) => {
        if (proc.state !== 'killed' && proc.state !== 'timeout') {
          proc.state = 'failed';
          proc.exitCode = proc.exitCode ?? 1;
          this.replaceProcessOutputSnapshot(proc, proc.output || String(error?.message || error));
          this.releaseServiceLease(proc);
        }
      })
      .finally(() => {
        if (proc.state !== 'running' && proc.state !== 'starting') {
          proc.endTime = Date.now();
        } else {
          proc.endTime = undefined;
        }
      });

    if (options.wait) {
      if (runMode === 'task') {
        await this.waitForProcessState(processId, idleTimeoutMs, signal, false, false);
      } else {
        const startupTimeoutMs = Math.max(1, startupWaitSeconds) * 1000;
        await this.waitForServiceStartup(processId, startupTimeoutMs, signal);
      }
    }

    await this.refreshProcessSnapshot(proc, false);
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
      if (proc.runMode === 'service') {
        await this.waitForServiceProgress(terminalId, idleTimeoutMs, signal);
      } else {
        await this.waitForProcessState(terminalId, idleTimeoutMs, signal, false, false);
      }
    }

    await this.refreshProcessSnapshot(proc, false);
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

    proc.session.sendText(inputText);
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

    const session = proc.session as NodeShellSession;
    const hadLease = this.isSessionServiceLocked(session);
    await this.forceStopProcess(proc, 'killed', 'kill-process');

    return {
      killed: true,
      final_output: proc.output,
      return_code: proc.exitCode,
      run_mode: proc.runMode,
      terminal_name: proc.terminalName,
      released_lock: hadLease,
    };
  }

  listProcessRecords(): ProcessRecord[] {
    const now = Date.now();
    const result: ProcessRecord[] = [];

    for (const [id, proc] of this.processes.entries()) {
      const session = proc.session as NodeShellSession;
      const endTime = proc.endTime ?? now;
      result.push({
        terminal_id: id,
        status: proc.state,
        command: proc.command,
        cwd: this.sessionCwds.get(session),
        started_at: proc.startTime,
        elapsed_seconds: Math.round((endTime - proc.startTime) / 1000),
        run_mode: proc.runMode,
        phase: this.getProcessPhase(proc),
        locked: this.isSessionServiceLocked(session),
        terminal_name: proc.terminalName,
        return_code: proc.exitCode,
        output_cursor: proc.outputCursor,
      });
    }

    return result;
  }

  dispose(): void {
    if (this.serviceSupervisorTimer) {
      clearInterval(this.serviceSupervisorTimer);
      this.serviceSupervisorTimer = null;
    }

    for (const session of this.managedSessions) {
      this.cleanupSession(session);
      session.dispose();
    }

    this.managedSessions.clear();
    this.agentSessions.clear();
    this.agentOverflowSessions.clear();
    this.sessionCwds.clear();
    this.sessionAgentNames.clear();
    this.sessionInitialized.clear();
    this.sessionShellType.clear();
    this.stopProcessTasks.clear();
    for (const child of this.directTaskProcesses.values()) {
      try {
        if (child.exitCode === null && !child.killed) {
          child.kill('SIGTERM');
        }
      } catch {
        // 进程可能已退出，忽略
      }
    }
    this.directTaskProcesses.clear();
    this.serviceLeases.clear();
    this.serviceRuntime.clear();
  }

  // ============================================================================
  // 会话分配策略
  // ============================================================================

  /**
   * 获取或创建 Shell 会话
   *
   * 分配策略与统一 Shell 执行语义一致：
   * 1. 主会话 idle → 复用
   * 2. 主会话被占用 → 从溢出池找空闲会话
   * 3. 溢出池无空闲 → 创建新会话加入溢出池
   * 4. 主会话已死 → 创建新主会话
   */
  private async getOrCreateSession(
    options: { cwd?: string; name: string }
  ): Promise<NodeShellSession> {
    const agentName = options.name;
    const targetCwd = options.cwd;

    const agentSession = this.agentSessions.get(agentName);

    if (agentSession && agentSession.isAlive()) {
      const occupation = this.getSessionOccupation(agentSession);
      // 路径 1：主会话 alive + 未占用 → 复用
      if (!occupation.occupied) {
        return await this.reuseSession(agentSession, agentName, targetCwd);
      }

      // 路径 2：主会话 alive + 已占用 → 从溢出池找空闲会话
      const overflowSession = this.findIdleOverflowSession(agentName);
      if (overflowSession) {
        return await this.reuseSession(overflowSession, agentName, targetCwd);
      }
      logger.debug('主会话被占用，分配溢出会话', {
        agentName,
        occupationReason: occupation.reason,
        occupiedProcessIds: occupation.processIds,
      }, LogCategory.SHELL);
      // 溢出池无可用 → 创建新溢出会话
      return await this.createOverflowSession(agentName, targetCwd);
    }

    // 主会话已死或不存在 → 创建新主会话
    if (agentSession && !agentSession.isAlive()) {
      this.cleanupSession(agentSession);
    }

    return await this.createPrimarySession(agentName, targetCwd);
  }

  /**
   * 复用已有会话（切换 cwd、确保策略就绪）
   */
  private async reuseSession(session: NodeShellSession, agentName: string, targetCwd?: string): Promise<NodeShellSession> {
    const currentCwd = this.sessionCwds.get(session);
    logger.debug('复用 Shell 会话', { agentName, currentCwd, targetCwd }, LogCategory.SHELL);

    if (targetCwd && targetCwd !== currentCwd) {
      session.sendText(`cd "${targetCwd}"`);
      this.sessionCwds.set(session, targetCwd);
      await this.delay(100);
    }

    await this.ensureSessionReady(session);
    return session;
  }

  /**
   * 创建 agent 主会话
   */
  private async createPrimarySession(agentName: string, cwd?: string): Promise<NodeShellSession> {
    logger.debug('创建 agent 主 Shell 会话', { agentName, cwd }, LogCategory.SHELL);
    const session = await this.createAndInitSession(agentName, cwd);
    this.agentSessions.set(agentName, session);
    this.sessionAgentNames.set(session, agentName);
    return session;
  }

  /**
   * 创建溢出会话（加入溢出池，不覆盖主会话映射）
   */
  private async createOverflowSession(agentName: string, cwd?: string): Promise<NodeShellSession> {
    const pool = this.agentOverflowSessions.get(agentName) || new Set();
    const overflowIndex = pool.size + 1;
    const sessionName = `${agentName}-${overflowIndex}`;

    logger.debug('创建溢出 Shell 会话（主会话已占用）', { agentName, sessionName, cwd }, LogCategory.SHELL);
    const session = await this.createAndInitSession(sessionName, cwd);

    pool.add(session);
    this.agentOverflowSessions.set(agentName, pool);
    this.sessionAgentNames.set(session, agentName);
    return session;
  }

  /**
   * 创建并初始化 Shell 会话（公共逻辑）
   */
  private async createAndInitSession(name: string, cwd?: string): Promise<NodeShellSession> {
    const session = new NodeShellSession(name, cwd);

    // 等待 shell 进程就绪
    await this.waitForSessionReady(session);

    const shellType = detectShellType();
    this.sessionShellType.set(session, shellType);
    await this.initializeSessionStrategy(session, shellType);

    this.managedSessions.add(session);
    this.sessionCwds.set(session, cwd);

    return session;
  }

  /**
   * 从溢出池中查找空闲会话（同时清理已死会话）
   */
  private findIdleOverflowSession(agentName: string): NodeShellSession | null {
    const pool = this.agentOverflowSessions.get(agentName);
    if (!pool) return null;

    const deadSessions: NodeShellSession[] = [];
    let idleSession: NodeShellSession | null = null;

    for (const session of pool) {
      if (!session.isAlive()) {
        deadSessions.push(session);
        continue;
      }
      const occupation = this.getSessionOccupation(session);
      if (!occupation.occupied && !idleSession) {
        idleSession = session;
      }
    }

    // 清理已死会话
    for (const dead of deadSessions) {
      pool.delete(dead);
      this.cleanupSession(dead);
    }
    if (pool.size === 0) {
      this.agentOverflowSessions.delete(agentName);
    }

    return idleSession;
  }

  // ============================================================================
  // 会话初始化
  // ============================================================================

  /**
   * 等待 Shell 会话就绪
   */
  private async waitForSessionReady(session: NodeShellSession): Promise<void> {
    // 等待 shell 进程 spawn 完成
    await this.delay(300);
    if (!session.isAlive()) {
      throw new Error(`Shell 进程启动失败: ${session.name}`);
    }
    logger.debug('Shell 会话就绪', { name: session.name }, LogCategory.SHELL);
  }

  /**
   * 初始化 ScriptCapture 策略
   */
  private async initializeSessionStrategy(session: NodeShellSession, shellType: ShellType): Promise<void> {
    if (this.sessionInitialized.get(session)) {
      return;
    }

    logger.debug('初始化 ScriptCapture 策略', { shellType }, LogCategory.SHELL);
    const success = await this.scriptCaptureStrategy.setupTerminal(session, shellType);

    if (success) {
      logger.debug('ScriptCapture 策略初始化成功', undefined, LogCategory.SHELL);
    } else {
      logger.warn(
        'ScriptCapture 策略初始化失败，将使用基础模式',
        { shellType },
        LogCategory.SHELL
      );
    }
    // 无论成功与否都标记为已初始化，避免无限重试
    this.sessionInitialized.set(session, true);
  }

  /**
   * 确保 Shell 会话策略可用
   */
  private async ensureSessionReady(session: NodeShellSession): Promise<void> {
    if (!this.sessionInitialized.get(session)) {
      const shellType = this.sessionShellType.get(session) || detectShellType();
      await this.initializeSessionStrategy(session, shellType);
      return;
    }

    if (this.scriptCaptureStrategy.isReady(session)) {
      return;
    }

    // 策略失效，尝试重新初始化
    const shellType = this.sessionShellType.get(session) || detectShellType();
    await this.scriptCaptureStrategy.ensureTerminalSessionActive?.(session, shellType);
  }

  // ============================================================================
  // 命令执行
  // ============================================================================

  /**
   * 执行命令
   */
  private async executeCommand(proc: TerminalProcess, command: string): Promise<void> {
    const session = proc.session as NodeShellSession;

    if (this.scriptCaptureStrategy.isReady(session)) {
      logger.debug('使用 ScriptCapture 策略执行命令', undefined, LogCategory.SHELL);
      await this.executeWithScriptCapture(proc, command);
      return;
    }

    // task 模式在无 ScriptCapture 时改走直连子进程，避免依赖 script/pgrep。
    if (proc.runMode === 'task') {
      logger.warn('ScriptCapture 不可用，task 模式切换为直连子进程执行', {
        processId: proc.id,
        command,
      }, LogCategory.SHELL);
      await this.executeTaskWithoutScriptCapture(proc, command);
      return;
    }

    // service 模式允许降级发送命令，但明确标记为基础模式。
    logger.debug('使用基础模式执行 service 命令（输出能力受限）', undefined, LogCategory.SHELL);
    await this.executeWithSendText(proc, command);
  }

  /**
   * 使用 ScriptCapture 策略执行命令
   */
  private async executeWithScriptCapture(proc: TerminalProcess, command: string): Promise<void> {
    const session = proc.session as NodeShellSession;

    // 包装命令（更新文件位置等）
    const wrappedCommand = this.scriptCaptureStrategy.wrapCommand(
      command,
      proc.id,
      session,
      true
    );
    proc.actualCommand = wrappedCommand;

    // 发送命令
    session.sendText(wrappedCommand, true);

    if (proc.runMode === 'service') {
      await this.delay(POLL_START_DELAY_MS);
      const outputResult = this.scriptCaptureStrategy.getOutputAndReturnCode?.(
        proc.id,
        session,
        wrappedCommand,
        false
      );
      if (typeof outputResult === 'object') {
        this.replaceProcessOutputSnapshot(proc, outputResult.output);
      } else if (typeof outputResult === 'string' && outputResult.trim().length > 0) {
        this.replaceProcessOutputSnapshot(proc, outputResult);
      }
      this.markProcessActivity(proc);
      return;
    }

    // task 模式：轮询检测完成状态
    return new Promise((resolve) => {
      const pollInterval = 150;

      const poll = () => {
        if (!session.isAlive()) {
          this.markProcessFailedOnSessionClose(proc, 'Shell 进程已退出，task 执行中断');
          resolve();
          return;
        }
        if (proc.state === 'killed' || proc.state === 'timeout') {
          resolve();
          return;
        }
        if (proc.state === 'completed' || proc.state === 'failed') {
          resolve();
          return;
        }

        if (this.scriptCaptureStrategy.hasOutputActivity(session)) {
          this.markProcessActivity(proc);
        }

        const result = this.scriptCaptureStrategy.checkCompleted(proc.id, session);

        if (result.isCompleted) {
          const outputResult = this.scriptCaptureStrategy.getOutputAndReturnCode?.(
            proc.id,
            session,
            wrappedCommand,
            true
          );

          if (typeof outputResult === 'object') {
            this.replaceProcessOutputSnapshot(proc, outputResult.output);
            proc.exitCode = outputResult.returnCode;
          } else if (typeof outputResult === 'string') {
            this.replaceProcessOutputSnapshot(proc, outputResult);
            proc.exitCode = 0;
          }

          proc.state = proc.exitCode !== null && proc.exitCode !== 0
            ? 'failed'
            : 'completed';
          this.markProcessActivity(proc);
          resolve();
          return;
        }

        setTimeout(poll, pollInterval);
      };

      setTimeout(poll, POLL_START_DELAY_MS);
    });
  }

  /**
   * 使用基础模式执行命令（无 ScriptCapture）
   */
  private async executeWithSendText(proc: TerminalProcess, command: string): Promise<void> {
    proc.session.sendText(command);

    if (proc.runMode === 'service') {
      this.replaceProcessOutputSnapshot(proc, '(service 命令已发送到 shell（基础模式），请使用 read-process 观察输出)');
      this.markProcessActivity(proc);
      return;
    }

    return new Promise((resolve) => {
      setTimeout(() => {
        proc.state = 'completed';
        proc.exitCode = 0;
        this.replaceProcessOutputSnapshot(proc, '(命令已发送到 shell，基础模式无法捕获输出)');
        this.markProcessActivity(proc);
        resolve();
      }, 500);
    });
  }

  /**
   * ScriptCapture 不可用时，task 模式使用一次性子进程执行命令。
   * 该路径不依赖 script/pgrep，确保基础终端能力可用。
   */
  private async executeTaskWithoutScriptCapture(proc: TerminalProcess, command: string): Promise<void> {
    const session = proc.session as NodeShellSession;
    const cwd = this.sessionCwds.get(session) || process.cwd();
    const shellPath = process.env.SHELL || '/bin/bash';

    await new Promise<void>((resolve, reject) => {
      const child = spawn(shellPath, ['-lc', command], {
        cwd,
        env: { ...process.env, TERM: 'dumb' },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      this.directTaskProcesses.set(proc.id, child);

      let mergedOutput = '';
      let settled = false;
      const appendChunk = (chunk: Buffer | string): void => {
        const text = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
        if (!text) {
          return;
        }
        mergedOutput += text;
        this.replaceProcessOutputSnapshot(proc, mergedOutput);
      };

      child.stdout?.on('data', appendChunk);
      child.stderr?.on('data', appendChunk);

      child.once('error', (error) => {
        if (settled) {
          return;
        }
        settled = true;
        this.directTaskProcesses.delete(proc.id);
        const message = error?.message || String(error);
        if (message && !mergedOutput.includes(message)) {
          mergedOutput = mergedOutput ? `${mergedOutput}\n${message}` : message;
          this.replaceProcessOutputSnapshot(proc, mergedOutput);
        }
        proc.exitCode = 1;
        reject(error);
      });

      child.once('close', (code, signal) => {
        if (settled) {
          return;
        }
        settled = true;
        this.directTaskProcesses.delete(proc.id);
        if (signal && (code === null || code === undefined)) {
          proc.exitCode = 1;
          if (!mergedOutput.trim()) {
            this.replaceProcessOutputSnapshot(proc, `process terminated by signal: ${signal}`);
          }
        } else {
          proc.exitCode = Number.isFinite(code as number) ? (code as number) : 0;
        }
        resolve();
      });
    });
  }

  // ============================================================================
  // 进程等待
  // ============================================================================

  /**
   * 等待进程状态变化
   */
  private async waitForProcessState(
    processId: number,
    idleTimeoutMs: number,
    signal?: AbortSignal,
    killOnTimeout: boolean = false,
    killOnAbort: boolean = false,
  ): Promise<void> {
    while (true) {
      if (signal?.aborted) {
        if (killOnAbort) {
          const proc = this.processes.get(processId);
          if (proc && (proc.state === 'running' || proc.state === 'starting')) {
            await this.forceStopProcess(proc, 'killed', 'abort-signal');
          }
        }
        return;
      }

      const proc = this.processes.get(processId);
      if (!proc) {
        return;
      }
      const directTaskRunning = this.directTaskProcesses.has(processId);

      // 直连 task 子进程已退出但状态尚未由执行 Promise 收敛时，主动收敛为终态，避免 read/wait 看到短暂 running。
      if (!directTaskRunning && proc.runMode === 'task' && proc.exitCode !== null) {
        if (proc.state === 'running' || proc.state === 'starting') {
          proc.state = proc.exitCode === 0 ? 'completed' : 'failed';
          proc.endTime = Date.now();
        }
        return;
      }

      if (proc.state !== 'running' && proc.state !== 'starting') {
        return;
      }

      await this.refreshProcessSnapshot(proc, false);
      if (proc.state !== 'running' && proc.state !== 'starting') {
        return;
      }

      const now = Date.now();
      const lastActivityAt = proc.updatedAt ?? proc.startTime;
      if (!directTaskRunning && now - lastActivityAt >= idleTimeoutMs) {
        if (killOnTimeout) {
          await this.forceStopProcess(proc, 'timeout', `idle-timeout:${idleTimeoutMs}ms`);
        }
        return;
      }

      if (now - proc.startTime >= PROCESS_HARD_TIMEOUT_MS) {
        if (killOnTimeout) {
          await this.forceStopProcess(proc, 'timeout', `hard-timeout:${PROCESS_HARD_TIMEOUT_MS}ms`);
        }
        return;
      }

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
        const activeProcess = this.processes.get(processId);
        if (activeProcess && (activeProcess.state === 'running' || activeProcess.state === 'starting')) {
          this.updateServiceStartupStatus(activeProcess, 'skipped', '启动握手等待被中断，service 继续后台运行');
        }
        return;
      }

      const proc = this.processes.get(processId);
      if (!proc) {
        return;
      }

      await this.refreshProcessSnapshot(proc, false);

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

      if (proc.state !== 'running' && proc.state !== 'starting') {
        return;
      }

      if (Date.now() - startedAt >= timeoutMs) {
        this.updateServiceStartupStatus(proc, 'timeout', `启动握手超时（${Math.ceil(timeoutMs / 1000)}s）`);
        return;
      }

      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  /**
   * 等待 service 输出进展
   */
  private async waitForServiceProgress(
    processId: number,
    idleTimeoutMs: number,
    signal?: AbortSignal,
  ): Promise<void> {
    const proc = this.processes.get(processId);
    if (!proc) {
      return;
    }
    const initialCursor = proc.outputCursor;
    const startAt = Date.now();

    while (true) {
      if (signal?.aborted) {
        return;
      }

      const activeProcess = this.processes.get(processId);
      if (!activeProcess) {
        return;
      }

      await this.refreshProcessSnapshot(activeProcess, false);
      if (activeProcess.state !== 'running' && activeProcess.state !== 'starting') {
        return;
      }

      if (
        activeProcess.outputCursor > initialCursor
        || this.getProcessPhase(activeProcess) === 'ready'
      ) {
        return;
      }

      if (Date.now() - startAt >= idleTimeoutMs) {
        return;
      }

      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  // ============================================================================
  // 进程管理
  // ============================================================================

  /**
   * 强制停止进程
   */
  private async forceStopProcess(
    proc: TerminalProcess,
    targetState: 'killed' | 'timeout',
    reason: string
  ): Promise<void> {
    const existing = this.stopProcessTasks.get(proc.id);
    if (existing) {
      await existing;
      return;
    }

    const stopTask = (async () => {
      if (proc.state === 'completed' || proc.state === 'failed') {
        return;
      }
      if (proc.state === 'killed' || proc.state === 'timeout') {
        return;
      }

      const session = proc.session as NodeShellSession;
      const directTask = this.directTaskProcesses.get(proc.id);
      logger.warn('强制停止 Shell 进程', {
        processId: proc.id,
        reason,
        targetState,
      }, LogCategory.SHELL);

      if (directTask && directTask.exitCode === null && !directTask.killed) {
        try {
          directTask.kill('SIGTERM');
          await this.delay(100);
          if (directTask.exitCode === null && !directTask.killed) {
            directTask.kill('SIGKILL');
          }
        } catch (error) {
          logger.debug('终止直连 task 子进程失败', { processId: proc.id, error }, LogCategory.SHELL);
        }
      }
      this.directTaskProcesses.delete(proc.id);

      // 发送 Ctrl+C
      session.sendText('\x03', false);
      await this.delay(100);

      try {
        await this.scriptCaptureStrategy.interruptActiveCommand(session);
      } catch (error) {
        logger.debug('终止子进程树失败', { processId: proc.id, error }, LogCategory.SHELL);
      }

      this.releaseServiceLease(proc);

      // 清理并销毁会话
      this.cleanupSession(session);
      session.dispose();

      proc.state = targetState;
      proc.exitCode = -1;
      proc.endTime = Date.now();
      this.updateServiceStartupStatus(proc, targetState === 'killed' ? 'failed' : 'timeout',
        `进程已${targetState === 'killed' ? '终止' : '超时终止'}`);
    })().finally(() => {
      this.stopProcessTasks.delete(proc.id);
    });

    this.stopProcessTasks.set(proc.id, stopTask);
    await stopTask;
  }

  /**
   * 刷新进程输出快照
   */
  private async refreshProcessSnapshot(proc: TerminalProcess, isCompletedHint: boolean): Promise<void> {
    if (proc.state === 'killed' || proc.state === 'timeout') {
      return;
    }

    const session = proc.session as NodeShellSession;
    if (!session.isAlive()) {
      this.markProcessFailedOnSessionClose(proc, 'Shell 进程已退出，进程不可用');
      return;
    }

    const scriptReady = this.scriptCaptureStrategy.isReady(session);
    let completed = isCompletedHint;

    if (scriptReady) {
      if (this.scriptCaptureStrategy.hasOutputActivity(session)) {
        this.markProcessActivity(proc);
      }

      const completion = proc.runMode === 'service'
        ? this.scriptCaptureStrategy.checkCompletedByMarker(proc.id, session)
        : this.scriptCaptureStrategy.checkCompleted(proc.id, session);
      completed = completion.isCompleted || isCompletedHint;

      const outputResult = this.scriptCaptureStrategy.getOutputAndReturnCode?.(
        proc.id,
        session,
        proc.actualCommand,
        completed
      );

      if (typeof outputResult === 'object') {
        this.replaceProcessOutputSnapshot(proc, outputResult.output);
        if (completed && outputResult.returnCode !== null) {
          proc.exitCode = outputResult.returnCode;
        }
      } else if (typeof outputResult === 'string' && outputResult.trim().length > 0) {
        this.replaceProcessOutputSnapshot(proc, outputResult);
      }
    }

    // 推进 service 就绪状态判断
    this.refreshServiceReadiness(proc);

    if (scriptReady && completed && (proc.state === 'running' || proc.state === 'starting')) {
      proc.state = proc.exitCode !== null && proc.exitCode !== 0 ? 'failed' : 'completed';
      proc.endTime = Date.now();
      this.releaseServiceLease(proc);
      this.updateServiceStartupStatus(proc, proc.state === 'completed' ? 'confirmed' : 'failed');
      this.markProcessActivity(proc);
    }
  }

  // ============================================================================
  // 会话占用检测
  // ============================================================================

  private isProcessActive(proc: TerminalProcess): boolean {
    return proc.state === 'running' || proc.state === 'starting';
  }

  private markProcessFailedOnSessionClose(proc: TerminalProcess, message: string): void {
    if (!this.isProcessActive(proc)) {
      return;
    }
    proc.state = 'failed';
    proc.exitCode = proc.exitCode ?? 1;
    proc.endTime = Date.now();
    this.releaseServiceLease(proc);
    this.updateServiceStartupStatus(proc, 'failed', message);
    this.markProcessActivity(proc);
  }

  private getSessionActiveProcesses(session: NodeShellSession): TerminalProcess[] {
    const active: TerminalProcess[] = [];
    for (const proc of this.processes.values()) {
      if (proc.session === session && this.isProcessActive(proc)) {
        active.push(proc);
      }
    }
    return active;
  }

  private getSessionOccupation(session: NodeShellSession): {
    occupied: boolean;
    reason: 'service-lock' | 'active-process' | 'none';
    processIds: number[];
  } {
    if (!session.isAlive()) {
      return { occupied: false, reason: 'none', processIds: [] };
    }

    const lease = this.serviceLeases.get(session);
    if (lease && this.isSessionServiceLocked(session)) {
      return {
        occupied: true,
        reason: 'service-lock',
        processIds: [lease.processId],
      };
    }

    const activeProcesses = this.getSessionActiveProcesses(session);
    if (activeProcesses.length > 0) {
      return {
        occupied: true,
        reason: 'active-process',
        processIds: activeProcesses.map((p) => p.id),
      };
    }

    return { occupied: false, reason: 'none', processIds: [] };
  }

  // ============================================================================
  // 会话清理
  // ============================================================================

  private cleanupSession(session: NodeShellSession): void {
    this.scriptCaptureStrategy.cleanupTerminal(session);
    this.serviceLeases.delete(session);
    this.sessionInitialized.delete(session);
    this.sessionShellType.delete(session);
    this.sessionCwds.delete(session);
    this.managedSessions.delete(session);

    const agentName = this.sessionAgentNames.get(session);
    if (agentName) {
      const mappedSession = this.agentSessions.get(agentName);
      if (mappedSession === session) {
        this.agentSessions.delete(agentName);
      }
      const pool = this.agentOverflowSessions.get(agentName);
      if (pool) {
        pool.delete(session);
        if (pool.size === 0) {
          this.agentOverflowSessions.delete(agentName);
        }
      }
      this.sessionAgentNames.delete(session);
    }
  }

  // ============================================================================
  // Service 租约管理
  // ============================================================================

  private acquireServiceLease(proc: TerminalProcess): void {
    this.serviceLeases.set(proc.session as NodeShellSession, {
      processId: proc.id,
      agentName: proc.agentName,
      lockedAt: Date.now(),
    });
    proc.serviceLocked = true;
  }

  private releaseServiceLease(proc: TerminalProcess): boolean {
    const session = proc.session as NodeShellSession;
    const lease = this.serviceLeases.get(session);
    if (!lease || lease.processId !== proc.id) {
      proc.serviceLocked = false;
      return false;
    }
    this.serviceLeases.delete(session);
    proc.serviceLocked = false;
    return true;
  }

  private isSessionServiceLocked(session: NodeShellSession): boolean {
    const lease = this.serviceLeases.get(session);
    if (!lease) {
      return false;
    }
    const owner = this.processes.get(lease.processId);
    if (!owner) {
      this.serviceLeases.delete(session);
      return false;
    }
    if (owner.state !== 'running' && owner.state !== 'starting') {
      this.serviceLeases.delete(session);
      owner.serviceLocked = false;
      return false;
    }
    owner.serviceLocked = true;
    return true;
  }

  // ============================================================================
  // Service 运行时
  // ============================================================================

  private refreshServiceReadiness(proc: TerminalProcess): void {
    if (proc.runMode !== 'service') {
      return;
    }

    const runtime = this.serviceRuntime.get(proc.id);
    if (!runtime) {
      return;
    }

    if (!runtime.startupConfirmed && this.hasServiceReadySignal(proc.output, runtime.readyPatterns)) {
      this.updateServiceStartupStatus(proc, 'confirmed', '检测到服务就绪信号');
      return;
    }

    if (
      runtime.startupStatus === 'pending'
      && runtime.startupDeadlineAt
      && Date.now() >= runtime.startupDeadlineAt
    ) {
      const waitSeconds = Math.max(1, Math.ceil((runtime.startupDeadlineAt - proc.startTime) / 1000));
      this.updateServiceStartupStatus(proc, 'timeout', `启动握手超时（${waitSeconds}s）`);
    }
  }

  private updateServiceStartupStatus(
    proc: TerminalProcess,
    status: ServiceRuntimeState['startupStatus'],
    message?: string,
  ): void {
    if (proc.runMode !== 'service') {
      return;
    }

    const runtime = this.serviceRuntime.get(proc.id);
    if (!runtime) {
      return;
    }

    runtime.startupStatus = status;
    runtime.startupConfirmed = status === 'confirmed';
    if (status !== 'pending') {
      runtime.startupDeadlineAt = undefined;
    }

    if (message) {
      runtime.startupMessage = message;
      return;
    }

    if (status === 'confirmed') {
      runtime.startupMessage = '服务启动成功，已确认就绪';
      return;
    }
    if (status === 'failed') {
      runtime.startupMessage = '服务启动失败';
    }
  }

  private hasServiceReadySignal(output: string, readyPatterns?: RegExp[]): boolean {
    if (!output) {
      return false;
    }
    const patterns = readyPatterns && readyPatterns.length > 0
      ? readyPatterns
      : DEFAULT_SERVICE_READY_PATTERNS;
    return patterns.some((pattern) => pattern.test(output));
  }

  private async runServiceSupervisorTick(): Promise<void> {
    if (this.serviceSupervisorTickInFlight) {
      return;
    }

    this.serviceSupervisorTickInFlight = true;
    try {
      const targets = Array.from(this.processes.values()).filter(
        (proc) => proc.runMode === 'service' && (proc.state === 'running' || proc.state === 'starting')
      );

      for (const proc of targets) {
        const session = proc.session as NodeShellSession;
        if (!session.isAlive()) {
          proc.state = 'failed';
          proc.exitCode = proc.exitCode ?? 1;
          proc.endTime = Date.now();
          this.releaseServiceLease(proc);
          this.updateServiceStartupStatus(proc, 'failed', 'Shell 进程已退出，service 进程不可用');
          this.markProcessActivity(proc);
          continue;
        }

        await this.refreshProcessSnapshot(proc, false);

        const runtime = this.serviceRuntime.get(proc.id);
        if (
          runtime
          && runtime.startupStatus === 'pending'
          && runtime.startupDeadlineAt
          && Date.now() >= runtime.startupDeadlineAt
        ) {
          const waitSeconds = Math.max(1, Math.ceil((runtime.startupDeadlineAt - proc.startTime) / 1000));
          this.updateServiceStartupStatus(proc, 'timeout', `启动握手超时（${waitSeconds}s）`);
        }
      }
    } catch (error: any) {
      logger.warn(
        'service supervisor tick 执行失败',
        { error: error?.message || String(error) },
        LogCategory.SHELL
      );
    } finally {
      this.serviceSupervisorTickInFlight = false;
    }
  }

  // ============================================================================
  // 结果构建
  // ============================================================================

  private buildLaunchResult(proc: TerminalProcess): LaunchProcessResult {
    const session = proc.session as NodeShellSession;
    const cwd = this.sessionCwds.get(session) || this.scriptCaptureStrategy.getCurrentCwd?.(session);
    const runtime = this.serviceRuntime.get(proc.id);
    return {
      terminal_id: proc.id,
      status: proc.state,
      output: proc.output,
      return_code: proc.exitCode,
      run_mode: proc.runMode,
      phase: this.getProcessPhase(proc),
      locked: this.isSessionServiceLocked(session),
      terminal_name: proc.terminalName,
      cwd,
      output_cursor: proc.outputCursor,
      output_start_cursor: proc.outputStartCursor,
      message: proc.runMode === 'service'
        ? 'service 终端已锁定，后续命令将自动分配到溢出终端。'
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
    const session = proc.session as NodeShellSession;
    const cwd = this.sessionCwds.get(session) || this.scriptCaptureStrategy.getCurrentCwd?.(session);

    return {
      status: proc.state,
      output,
      return_code: proc.exitCode,
      run_mode: proc.runMode,
      phase: this.getProcessPhase(proc),
      locked: this.isSessionServiceLocked(session),
      terminal_name: proc.terminalName,
      cwd,
      from_cursor: clampedStart,
      output_start_cursor: proc.outputStartCursor,
      next_cursor: proc.outputCursor,
      delta,
      truncated: delta && normalizedFromCursor < proc.outputStartCursor,
      output_cursor: proc.outputCursor,
    };
  }

  private getProcessPhase(proc: TerminalProcess): ProcessPhase {
    if (proc.state === 'starting') {
      return 'starting';
    }
    if (proc.state === 'running') {
      if (proc.runMode === 'service') {
        const runtime = this.serviceRuntime.get(proc.id);
        if (runtime?.startupConfirmed || this.hasServiceReadySignal(proc.output, runtime?.readyPatterns)) {
          return 'ready';
        }
      }
      return 'running';
    }
    if (proc.state === 'completed') {
      return 'completed';
    }
    if (proc.state === 'failed') {
      return 'failed';
    }
    if (proc.state === 'killed') {
      return 'killed';
    }
    return 'timeout';
  }

  // ============================================================================
  // 输出管理
  // ============================================================================

  private replaceProcessOutputSnapshot(proc: TerminalProcess, output: string): void {
    const nextOutput = typeof output === 'string' ? output : '';
    const previousOutput = proc.output || '';
    if (nextOutput === previousOutput) {
      return;
    }

    let appendedLength = 0;
    if (!previousOutput) {
      appendedLength = nextOutput.length;
    } else if (nextOutput.startsWith(previousOutput)) {
      appendedLength = nextOutput.length - previousOutput.length;
    } else if (previousOutput.includes(nextOutput)) {
      appendedLength = 0;
    } else {
      const overlap = this.computeOutputOverlap(previousOutput, nextOutput);
      appendedLength = Math.max(0, nextOutput.length - overlap);
    }

    proc.outputCursor += appendedLength;
    proc.output = nextOutput;
    this.trimProcessOutputBuffer(proc);
    this.markProcessActivity(proc);
  }

  private trimProcessOutputBuffer(proc: TerminalProcess): void {
    if (proc.output.length > PROCESS_OUTPUT_BUFFER_LIMIT) {
      const overflow = proc.output.length - PROCESS_OUTPUT_BUFFER_LIMIT;
      proc.output = proc.output.slice(overflow);
    }
    proc.outputStartCursor = Math.max(0, proc.outputCursor - proc.output.length);
  }

  private computeOutputOverlap(previous: string, next: string): number {
    if (!previous || !next) {
      return 0;
    }

    const maxOverlap = Math.min(previous.length, next.length);
    for (let length = maxOverlap; length > 0; length -= 1) {
      if (previous.slice(previous.length - length) === next.slice(0, length)) {
        return length;
      }
    }
    return 0;
  }

  // ============================================================================
  // 工具方法
  // ============================================================================

  private compileReadyPatterns(patterns?: string[]): RegExp[] {
    const result = [...DEFAULT_SERVICE_READY_PATTERNS];
    if (!Array.isArray(patterns)) {
      return result;
    }

    for (const rawPattern of patterns) {
      if (typeof rawPattern !== 'string') {
        continue;
      }
      const pattern = rawPattern.trim();
      if (!pattern) {
        continue;
      }
      try {
        result.push(new RegExp(pattern, 'i'));
      } catch (error: any) {
        logger.warn('忽略非法 ready pattern', {
          pattern,
          error: error?.message || String(error),
        }, LogCategory.SHELL);
      }
    }

    return result;
  }

  private normalizeIdleTimeoutMs(maxWaitSeconds: number): number {
    const seconds = Number.isFinite(maxWaitSeconds) ? maxWaitSeconds : this.defaultTimeout / 1000;
    return Math.min(Math.max(seconds, 1) * 1000, this.maxTimeout);
  }

  private markProcessActivity(proc: TerminalProcess): void {
    const now = Date.now();
    proc.updatedAt = now;
    const runtime = this.serviceRuntime.get(proc.id);
    if (runtime) {
      runtime.lastHeartbeatAt = now;
    }
  }

  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
