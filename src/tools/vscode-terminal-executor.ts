/**
 * VSCode Terminal Executor
 * 提供基于VSCode Terminal API的命令执行能力
 *
 * 采用双策略模式：
 * 1. ScriptCaptureStrategy - 主策略，使用 script 命令捕获输出
 * 2. VSCodeEventsStrategy - Shell Integration 可用时使用
 */

import * as vscode from 'vscode';
import {
  KillProcessResult,
  LaunchProcessOptions,
  LaunchProcessResult,
  ProcessPhase,
  ProcessRunMode,
  ReadProcessResult,
  WriteProcessResult,
} from './types';
import { logger, LogCategory } from '../logging';
import {
  ShellType,
  TerminalProcess,
  ProcessState,
} from './terminal/types';
import { VSCodeEventsStrategy } from './terminal/vscode-events-strategy';
import { ScriptCaptureStrategy } from './terminal/script-capture-strategy';
import { FileMutex } from '../utils/file-mutex';

// ============================================================================
// 超时常量
// ============================================================================

/** 终端初始化超时 (ms) */
const TERMINAL_INIT_TIMEOUT_MS = 5000;

/** 基础模式命令发送后等待时间 (ms) */
const SEND_TEXT_WAIT_MS = 500;

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

/**
 * 检测 Shell 类型
 */
function detectShellType(terminal?: vscode.Terminal): ShellType {
  const shellPath = (terminal as any)?._creationOptions?.shellPath
    || vscode.env?.shell
    || process.env.SHELL
    || '';

  const shellName = shellPath.toLowerCase();

  if (shellName.includes('zsh')) return 'zsh';
  if (shellName.includes('bash')) return 'bash';
  if (shellName.includes('fish')) return 'fish';
  if (shellName.includes('powershell') || shellName.includes('pwsh')) return 'powershell';
  if (shellName.includes('cmd')) return 'cmd';

  return 'bash'; // 默认 bash
}

const ALLOWED_AGENT_TERMINAL_NAMES = new Set([
  'orchestrator',
  'worker-claude',
  'worker-gemini',
  'worker-codex',
]);

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

/**
 * VSCode Terminal 执行器
 *
 * 实现终端复用和双策略模式
 */
export class VSCodeTerminalExecutor {
  private processes: Map<number, TerminalProcess> = new Map();
  private nextId: number = 1;
  private readonly defaultTimeout: number = 30000; // 30 秒
  private readonly maxTimeout: number = 3600000; // 1 小时（用于空闲超时参数上限）

  // 终端复用
  private mainTerminal: vscode.Terminal | null = null;
  private terminalCwds: Map<vscode.Terminal, string | undefined> = new Map();
  private managedTerminals: Set<vscode.Terminal> = new Set();
  private agentTerminals: Map<string, vscode.Terminal> = new Map();
  /** 溢出终端池：主终端 busy 时，后续命令在此池中分配独立终端 */
  private agentOverflowTerminals: Map<string, Set<vscode.Terminal>> = new Map();
  private terminalAgentNames: Map<vscode.Terminal, string> = new Map();
  private terminalCloseListener: vscode.Disposable | null = null;
  private stopProcessTasks: Map<number, Promise<void>> = new Map();
  private serviceLeases: Map<vscode.Terminal, ServiceLease> = new Map();
  private serviceRuntime: Map<number, ServiceRuntimeState> = new Map();
  private serviceSupervisorTimer: NodeJS.Timeout | null = null;
  private serviceSupervisorTickInFlight = false;

  // 双策略
  private vscodeEventsStrategy: VSCodeEventsStrategy;
  private scriptCaptureStrategy: ScriptCaptureStrategy;

  // 全局文件锁，用于与 file-executor 协同
  private fileMutex?: FileMutex;

  // 终端初始化状态
  private terminalInitialized: Map<vscode.Terminal, boolean> = new Map();
  private terminalShellType: Map<vscode.Terminal, ShellType> = new Map();

  constructor(fileMutex?: FileMutex) {
    this.vscodeEventsStrategy = new VSCodeEventsStrategy();
    this.scriptCaptureStrategy = new ScriptCaptureStrategy();
    this.fileMutex = fileMutex;

    // 监听终端关闭事件
    this.terminalCloseListener = vscode.window.onDidCloseTerminal((closedTerminal) => {
      this.markTerminalProcessesFailedOnClose(closedTerminal, '终端已关闭，进程已结束');
      this.cleanupTerminal(closedTerminal);

      if (this.mainTerminal === closedTerminal) {
        logger.debug('主终端被用户关闭', undefined, LogCategory.SHELL);
      }
    });
    this.serviceSupervisorTimer = setInterval(() => {
      void this.runServiceSupervisorTick();
    }, SERVICE_SUPERVISOR_INTERVAL_MS);
  }

  /**
   * 清理资源
   */
  dispose(): void {
    if (this.serviceSupervisorTimer) {
      clearInterval(this.serviceSupervisorTimer);
      this.serviceSupervisorTimer = null;
    }

    if (this.terminalCloseListener) {
      this.terminalCloseListener.dispose();
      this.terminalCloseListener = null;
    }

    for (const terminal of this.managedTerminals) {
      this.cleanupTerminal(terminal);
      terminal.dispose();
    }

    this.mainTerminal = null;
    this.terminalCwds.clear();
    this.managedTerminals.clear();
    this.agentTerminals.clear();
    this.agentOverflowTerminals.clear();
    this.terminalAgentNames.clear();
    this.terminalInitialized.clear();
    this.terminalShellType.clear();
    this.stopProcessTasks.clear();
    this.serviceLeases.clear();
    this.serviceRuntime.clear();
  }


  async launchProcess(options: LaunchProcessOptions, signal?: AbortSignal): Promise<LaunchProcessResult> {
    // 等待所有正在进行的文件读写锁释放（全局安全点）
    // 防止 file_edit（通过 WorkspaceEdit）还没完全落盘/保存完就被终端命令读取
    if (this.fileMutex) {
      await this.fileMutex.waitForAll();
    }

    // 强制将 VSCode 内存中的脏文档落盘，防止终端进程读到磁盘上的旧快照
    // 仅保存当前工作区内的 file:// scheme 文档，跳过虚拟文档及非工作区文件
    const dirtyDocs = vscode.workspace.textDocuments.filter(
      doc => doc.isDirty
        && doc.uri.scheme === 'file'
        && vscode.workspace.getWorkspaceFolder(doc.uri) !== undefined
    );
    if (dirtyDocs.length > 0) {
      const results = await Promise.allSettled(dirtyDocs.map(doc => doc.save()));
      const failures = results.filter(r => r.status === 'rejected');
      if (failures.length > 0) {
        logger.warn(
          `${failures.length} 个文件保存失败，终端命令仍将继续执行`,
          { failures: failures.map(f => (f as PromiseRejectedResult).reason?.message) },
          LogCategory.SHELL
        );
      }
      logger.info(
        `终端命令执行前，强制保存了 ${dirtyDocs.length - failures.length} 个未落盘文件，以保证底层进程读取状态最新。`,
        undefined,
        LogCategory.SHELL
      );
    }

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
    const terminal = await this.getOrCreateTerminal({
      cwd: options.cwd,
      env: undefined,
      name: agentName,
    });

    if (options.showTerminal ?? true) {
      terminal.show(true);
    }

    const processId = this.nextId++;
    const now = Date.now();
    const process: TerminalProcess = {
      id: processId,
      terminal,
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
      terminalName: terminal.name,
      serviceLocked: false,
    };
    this.processes.set(processId, process);
    if (runMode === 'service') {
      this.acquireServiceLease(process);
      const startupStatus: ServiceRuntimeState['startupStatus'] = options.wait ? 'pending' : 'skipped';
      this.serviceRuntime.set(process.id, {
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

    process.state = 'running';
    void this.executeCommand(process, options.command)
      .then(() => {
        if (process.state !== 'running' && process.state !== 'starting') {
          return;
        }
        if (process.runMode === 'service') {
          process.state = 'running';
          process.endTime = undefined;
          return;
        }
        process.state = process.exitCode === 0 ? 'completed' : 'failed';
      })
      .catch((error: any) => {
        if (process.state !== 'killed' && process.state !== 'timeout') {
          process.state = 'failed';
          process.exitCode = process.exitCode ?? 1;
          this.replaceProcessOutputSnapshot(process, process.output || String(error?.message || error));
          this.releaseServiceLease(process);
        }
      })
      .finally(() => {
        if (process.state !== 'running' && process.state !== 'starting') {
          process.endTime = Date.now();
        } else {
          process.endTime = undefined;
        }
      });

    if (options.wait) {
      if (runMode === 'task') {
        // task 模式等待超时/中断仅结束等待，不隐式终止进程
        await this.waitForProcessState(processId, idleTimeoutMs, signal, false, false);
      } else {
        const startupTimeoutMs = Math.max(1, startupWaitSeconds) * 1000;
        await this.waitForServiceStartup(processId, startupTimeoutMs, signal);
      }
    }

    await this.refreshProcessSnapshot(process, false);
    return this.buildLaunchResult(process);
  }

  async readProcess(
    terminalId: number,
    wait: boolean,
    maxWaitSeconds: number,
    fromCursor?: number,
    signal?: AbortSignal,
  ): Promise<ReadProcessResult> {
    const process = this.processes.get(terminalId);
    if (!process) {
      throw new Error(`终端进程不存在: ${terminalId}`);
    }

    if (wait && (process.state === 'running' || process.state === 'starting')) {
      const idleTimeoutMs = this.normalizeIdleTimeoutMs(maxWaitSeconds);
      if (process.runMode === 'service') {
        await this.waitForServiceProgress(terminalId, idleTimeoutMs, signal);
      } else {
        // read_process 只是观察，超时/中断后不应杀进程
        await this.waitForProcessState(terminalId, idleTimeoutMs, signal, false, false);
      }
    }

    await this.refreshProcessSnapshot(process, false);
    return this.buildReadResult(process, fromCursor);
  }

  async writeProcess(terminalId: number, inputText: string): Promise<WriteProcessResult> {
    const process = this.processes.get(terminalId);
    if (!process) {
      throw new Error(`终端进程不存在: ${terminalId}`);
    }

    if (process.state !== 'running') {
      return {
        accepted: false,
        status: process.state,
        run_mode: process.runMode,
        terminal_name: process.terminalName,
        message: 'process 非 running 状态，无法写入',
      };
    }

    process.terminal.sendText(inputText);
    return {
      accepted: true,
      status: process.state,
      run_mode: process.runMode,
      terminal_name: process.terminalName,
    };
  }

  async killProcess(terminalId: number): Promise<KillProcessResult> {
    const process = this.processes.get(terminalId);
    if (!process) {
      return {
        killed: false,
        final_output: '',
        return_code: null,
        released_lock: false,
      };
    }

    const hadLease = this.isTerminalServiceLocked(process.terminal);
    await this.forceStopProcess(process, 'killed', 'kill-process');

    return {
      killed: true,
      final_output: process.output,
      return_code: process.exitCode,
      run_mode: process.runMode,
      terminal_name: process.terminalName,
      released_lock: hadLease,
    };
  }

  listProcessRecords(): Array<{
    terminal_id: number;
    status: ProcessState;
    command: string;
    cwd: string | undefined;
    started_at: number;
    elapsed_seconds: number;
    run_mode: ProcessRunMode;
    phase: ProcessPhase;
    locked: boolean;
    terminal_name: string;
    return_code: number | null;
    output_cursor: number;
  }> {
    const now = Date.now();
    const result: Array<{
      terminal_id: number;
      status: ProcessState;
      command: string;
      cwd: string | undefined;
      started_at: number;
      elapsed_seconds: number;
      run_mode: ProcessRunMode;
      phase: ProcessPhase;
      locked: boolean;
      terminal_name: string;
      return_code: number | null;
      output_cursor: number;
    }> = [];

    for (const [id, process] of this.processes.entries()) {
      const endTime = process.endTime ?? now;
      result.push({
        terminal_id: id,
        status: process.state,
        command: process.command,
        cwd: this.terminalCwds.get(process.terminal),
        started_at: process.startTime,
        elapsed_seconds: Math.round((endTime - process.startTime) / 1000),
        run_mode: process.runMode,
        phase: this.getProcessPhase(process),
        locked: this.isTerminalServiceLocked(process.terminal),
        terminal_name: process.terminalName,
        return_code: process.exitCode,
        output_cursor: process.outputCursor,
      });
    }

    return result;
  }

  /**
   * 等待进程状态变化
   * @param killOnTimeout 超时时是否强制终止进程。
   * @param killOnAbort 收到 abort 信号时是否强制终止进程。
   * 默认策略：仅结束等待，不隐式杀进程；终止应由 kill-process 显式触发。
   */
  private async waitForProcessState(
    processId: number,
    idleTimeoutMs: number,
    signal?: AbortSignal,
    killOnTimeout: boolean = false,
    killOnAbort: boolean = false,
  ): Promise<void> {
    while (true) {
      // 中断检查：收到 abort 信号后退出等待（是否 kill 由参数控制）
      if (signal?.aborted) {
        if (killOnAbort) {
          const process = this.processes.get(processId);
          if (process && (process.state === 'running' || process.state === 'starting')) {
            await this.forceStopProcess(process, 'killed', 'abort-signal');
          }
        }
        return;
      }

      const process = this.processes.get(processId);
      if (!process) {
        return;
      }

      if (process.state !== 'running' && process.state !== 'starting') {
        return;
      }

      await this.refreshProcessSnapshot(process, false);
      if (process.state !== 'running' && process.state !== 'starting') {
        return;
      }

      const now = Date.now();
      const lastActivityAt = process.updatedAt ?? process.startTime;
      if (now - lastActivityAt >= idleTimeoutMs) {
        if (killOnTimeout) {
          await this.forceStopProcess(process, 'timeout', `idle-timeout:${idleTimeoutMs}ms`);
        }
        // 不管是否 kill，超时都应退出等待循环
        return;
      }

      if (now - process.startTime >= PROCESS_HARD_TIMEOUT_MS) {
        if (killOnTimeout) {
          await this.forceStopProcess(process, 'timeout', `hard-timeout:${PROCESS_HARD_TIMEOUT_MS}ms`);
        }
        return;
      }

      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  private isProcessActive(process: TerminalProcess): boolean {
    return process.state === 'running' || process.state === 'starting';
  }

  private markProcessFailedOnTerminalClose(process: TerminalProcess, message: string): void {
    if (!this.isProcessActive(process)) {
      return;
    }
    process.state = 'failed';
    process.exitCode = process.exitCode ?? 1;
    process.endTime = Date.now();
    this.releaseServiceLease(process);
    this.updateServiceStartupStatus(process, 'failed', message);
    this.markProcessActivity(process);
  }

  private markTerminalProcessesFailedOnClose(terminal: vscode.Terminal, message: string): void {
    for (const process of this.processes.values()) {
      if (process.terminal !== terminal) {
        continue;
      }
      this.markProcessFailedOnTerminalClose(process, message);
    }
  }

  private getTerminalActiveProcesses(terminal: vscode.Terminal): TerminalProcess[] {
    const active: TerminalProcess[] = [];
    for (const process of this.processes.values()) {
      if (process.terminal === terminal && this.isProcessActive(process)) {
        active.push(process);
      }
    }
    return active;
  }

  private getTerminalOccupation(terminal: vscode.Terminal): {
    occupied: boolean;
    reason: 'service-lock' | 'active-process' | 'none';
    processIds: number[];
  } {
    if (!this.isTerminalAlive(terminal)) {
      return { occupied: false, reason: 'none', processIds: [] };
    }

    const lease = this.serviceLeases.get(terminal);
    if (lease && this.isTerminalServiceLocked(terminal)) {
      return {
        occupied: true,
        reason: 'service-lock',
        processIds: [lease.processId],
      };
    }

    const activeProcesses = this.getTerminalActiveProcesses(terminal);
    if (activeProcesses.length > 0) {
      return {
        occupied: true,
        reason: 'active-process',
        processIds: activeProcesses.map((p) => p.id),
      };
    }

    return { occupied: false, reason: 'none', processIds: [] };
  }



  /**
   * 获取或创建终端
   *
   * 分配策略：
   * 1. 主终端 idle → 复用主终端
   * 2. 主终端被占用（运行中 task/service）→ 从溢出池找空闲终端 → 复用
   * 3. 溢出池全被占用或为空 → 创建新终端加入溢出池
   * 4. 主终端已死 → 创建新主终端
   *
   * 关键：主终端 busy 时创建的溢出终端**不会覆盖 agentTerminals 映射**，
   * 避免长驻服务终端变成孤儿。
   */
  private async getOrCreateTerminal(
    options: { cwd?: string; env?: Record<string, string>; name?: string }
  ): Promise<vscode.Terminal> {
    const agentName = (options.name || '').trim();
    const targetCwd = options.cwd;

    if (!agentName) {
      throw new Error('终端名称不能为空');
    }

    const agentTerminal = this.agentTerminals.get(agentName);

    if (agentTerminal && this.isTerminalAlive(agentTerminal)) {
      const occupation = this.getTerminalOccupation(agentTerminal);
      // 路径 1：主终端 alive + 未占用 → 复用
      if (!occupation.occupied) {
        return await this.reuseTerminal(agentTerminal, agentName, targetCwd);
      }

      // 路径 2：主终端 alive + 已占用 → 从溢出池找空闲终端
      const overflowTerminal = this.findIdleOverflowTerminal(agentName);
      if (overflowTerminal) {
        return await this.reuseTerminal(overflowTerminal, agentName, targetCwd);
      }
      logger.debug('主终端被占用，分配溢出终端', {
        agentName,
        occupationReason: occupation.reason,
        occupiedProcessIds: occupation.processIds,
      }, LogCategory.SHELL);
      // 溢出池无可用终端 → 创建新的溢出终端（不覆盖主终端映射）
      return await this.createOverflowTerminal(agentName, targetCwd, options.env);
    }

    // 路径 3/4：主终端已死或不存在 → 创建新主终端
    if (agentTerminal && !this.isTerminalAlive(agentTerminal)) {
      this.cleanupTerminal(agentTerminal);
    }

    return await this.createPrimaryTerminal(agentName, targetCwd, options.env);
  }

  /**
   * 复用已有终端（切换 cwd、确保策略就绪）
   */
  private async reuseTerminal(terminal: vscode.Terminal, agentName: string, targetCwd?: string): Promise<vscode.Terminal> {
    const currentCwd = this.terminalCwds.get(terminal);
    logger.debug('复用终端', { agentName, currentCwd, targetCwd }, LogCategory.SHELL);

    if (targetCwd && targetCwd !== currentCwd) {
      terminal.sendText(`cd "${targetCwd}"`);
      this.terminalCwds.set(terminal, targetCwd);
      await this.delay(100);
    }

    await this.ensureTerminalReady(terminal);
    this.mainTerminal = terminal;
    return terminal;
  }

  /**
   * 创建 agent 主终端（注册到 agentTerminals 映射）
   */
  private async createPrimaryTerminal(agentName: string, cwd?: string, env?: Record<string, string>): Promise<vscode.Terminal> {
    logger.debug('创建 agent 主终端', { agentName, cwd }, LogCategory.SHELL);
    const terminal = await this.createAndInitTerminal(agentName, cwd, env);
    this.agentTerminals.set(agentName, terminal);
    this.terminalAgentNames.set(terminal, agentName);
    return terminal;
  }

  /**
   * 创建溢出终端（加入溢出池，不覆盖主终端映射）
   */
  private async createOverflowTerminal(agentName: string, cwd?: string, env?: Record<string, string>): Promise<vscode.Terminal> {
    const pool = this.agentOverflowTerminals.get(agentName) || new Set();
    const overflowIndex = pool.size + 1;
    const terminalName = `${agentName}-${overflowIndex}`;

    logger.debug('创建溢出终端（主终端已占用）', { agentName, terminalName, cwd }, LogCategory.SHELL);
    const terminal = await this.createAndInitTerminal(terminalName, cwd, env);

    pool.add(terminal);
    this.agentOverflowTerminals.set(agentName, pool);
    // 溢出终端也关联到 agentName（用于 cleanupTerminal 反查）
    this.terminalAgentNames.set(terminal, agentName);
    return terminal;
  }

  /**
   * 创建并初始化终端（公共逻辑抽取）
   * 注意：不设置 terminalAgentNames，由调用方根据场景决定关联的 agentName
   */
  private async createAndInitTerminal(name: string, cwd?: string, env?: Record<string, string>): Promise<vscode.Terminal> {
    const terminal = vscode.window.createTerminal({
      name,
      cwd,
      env,
      isTransient: false,
    });
    await this.waitForTerminalReady(terminal);

    const shellType = detectShellType(terminal);
    this.terminalShellType.set(terminal, shellType);
    await this.initializeTerminalStrategy(terminal, shellType);

    this.managedTerminals.add(terminal);
    this.terminalCwds.set(terminal, cwd);
    this.mainTerminal = terminal;

    return terminal;
  }

  /**
   * 从溢出池中查找空闲终端（同时清理已死终端）
   */
  private findIdleOverflowTerminal(agentName: string): vscode.Terminal | null {
    const pool = this.agentOverflowTerminals.get(agentName);
    if (!pool) return null;

    const deadTerminals: vscode.Terminal[] = [];
    let idleTerminal: vscode.Terminal | null = null;

    for (const terminal of pool) {
      if (!this.isTerminalAlive(terminal)) {
        deadTerminals.push(terminal);
        continue;
      }
      const occupation = this.getTerminalOccupation(terminal);
      if (!occupation.occupied && !idleTerminal) {
        idleTerminal = terminal;
      }
    }

    // 清理已死终端
    for (const dead of deadTerminals) {
      pool.delete(dead);
      this.cleanupTerminal(dead);
    }
    if (pool.size === 0) {
      this.agentOverflowTerminals.delete(agentName);
    }

    return idleTerminal;
  }

  /**
   * 初始化终端策略
   */
  private async initializeTerminalStrategy(terminal: vscode.Terminal, shellType: ShellType): Promise<void> {
    if (this.terminalInitialized.get(terminal)) {
      return;
    }

    // 优先使用 VSCode Shell Integration
    if (terminal.shellIntegration) {
      logger.debug('使用 VSCode Shell Integration 策略', { shellType }, LogCategory.SHELL);
      await this.vscodeEventsStrategy.setupTerminal(terminal, shellType);
      this.terminalInitialized.set(terminal, true);
      return;
    }

    // 切换到 ScriptCapture 策略
    logger.debug('Shell Integration 不可用，使用 ScriptCapture 策略', { shellType }, LogCategory.SHELL);
    const success = await this.scriptCaptureStrategy.setupTerminal(terminal, shellType);

    if (success) {
      logger.debug('ScriptCapture 策略初始化成功', undefined, LogCategory.SHELL);
      this.terminalInitialized.set(terminal, true);
    } else {
      logger.warn(
        'ScriptCapture 策略初始化失败，将使用基础模式。建议检查 shell 配置或重启终端。',
        { shellType },
        LogCategory.SHELL
      );
      // 初始化失败时仍标记为已初始化，避免无限重试
      // 但使用基础模式执行命令
      this.terminalInitialized.set(terminal, true);
    }
  }

  /**
   * 确保终端策略可用
   */
  private async ensureTerminalReady(terminal: vscode.Terminal): Promise<void> {
    if (!this.terminalInitialized.get(terminal)) {
      const shellType = this.terminalShellType.get(terminal) || detectShellType(terminal);
      await this.initializeTerminalStrategy(terminal, shellType);
      return;
    }

    // 如果有 Shell Integration，无需额外检查
    if (terminal.shellIntegration) {
      return;
    }

    // 检查 ScriptCapture 策略是否仍然可用
    if (this.scriptCaptureStrategy.isReady(terminal)) {
      return;
    }

    // 策略失效，尝试重新初始化
    const shellType = this.terminalShellType.get(terminal) || detectShellType(terminal);
    await this.scriptCaptureStrategy.ensureTerminalSessionActive?.(terminal, shellType);
  }

  /**
   * 清理终端资源
   */
  private cleanupTerminal(terminal: vscode.Terminal): void {
    this.vscodeEventsStrategy.cleanupTerminal(terminal);
    this.scriptCaptureStrategy.cleanupTerminal(terminal);
    this.serviceLeases.delete(terminal);
    this.terminalInitialized.delete(terminal);
    this.terminalShellType.delete(terminal);
    this.terminalCwds.delete(terminal);
    this.managedTerminals.delete(terminal);

    const agentName = this.terminalAgentNames.get(terminal);
    if (agentName) {
      // 从主终端映射移除
      const mappedTerminal = this.agentTerminals.get(agentName);
      if (mappedTerminal === terminal) {
        this.agentTerminals.delete(agentName);
      }
      // 从溢出池移除
      const pool = this.agentOverflowTerminals.get(agentName);
      if (pool) {
        pool.delete(terminal);
        if (pool.size === 0) {
          this.agentOverflowTerminals.delete(agentName);
        }
      }
      this.terminalAgentNames.delete(terminal);
    }

    if (this.mainTerminal === terminal) {
      this.mainTerminal = null;
    }
  }

  /**
   * 检查终端是否存活
   */
  private isTerminalAlive(terminal: vscode.Terminal): boolean {
    return vscode.window.terminals.includes(terminal);
  }

  /**
   * 等待终端准备就绪
   */
  private async waitForTerminalReady(terminal: vscode.Terminal): Promise<void> {
    const processId = await Promise.race([
      terminal.processId,
      new Promise<number>((_, reject) =>
        setTimeout(() => reject(new Error('终端初始化超时')), TERMINAL_INIT_TIMEOUT_MS)
      ),
    ]);

    logger.debug('终端就绪', { processId }, LogCategory.SHELL);
  }

  /**
   * 执行命令
   */
  private async executeCommand(
    process: TerminalProcess,
    command: string
  ): Promise<void> {
    const terminal = process.terminal;

    // 优先使用初始化时选定的策略，避免策略混用
    // ScriptCapture 已就绪时，即使 Shell Integration 后来变为可用，也继续使用 ScriptCapture
    if (this.scriptCaptureStrategy.isReady(terminal)) {
      logger.debug('使用 ScriptCapture 策略执行命令', undefined, LogCategory.SHELL);
      await this.executeWithScriptCapture(process, command);
      return;
    }

    // 使用 Shell Integration（仅在 ScriptCapture 未初始化时）
    if (terminal.shellIntegration) {
      logger.debug('使用 Shell Integration 执行命令', undefined, LogCategory.SHELL);
      await this.executeWithShellIntegration(process, command);
      return;
    }

    // 基础模式：只发送命令，无法获取输出
    logger.debug('使用基础模式执行命令（无法获取输出）', undefined, LogCategory.SHELL);
    await this.executeWithSendText(process, command);
  }

  /**
   * 使用 Shell Integration 执行命令
   *
   * 同时监听 onDidEndTerminalShellExecution 事件和流结束，
   * 以事件为主（可获取退出码）、流结束为兜底，避免流不终止导致挂起。
   */
  private async executeWithShellIntegration(
    process: TerminalProcess,
    command: string
  ): Promise<void> {
    const terminal = process.terminal;
    const shellIntegration = terminal.shellIntegration!;

    const execution = shellIntegration.executeCommand(command);
    process.execution = execution;

    const stream = execution.read();
    let output = '';

    if (process.runMode === 'service') {
      const endListener = vscode.window.onDidEndTerminalShellExecution?.((e) => {
        if (e.execution !== execution) {
          return;
        }
        process.exitCode = e.exitCode ?? 0;
        process.state = process.exitCode === 0 ? 'completed' : 'failed';
        process.endTime = Date.now();
        this.replaceProcessOutputSnapshot(process, output);
        this.releaseServiceLease(process);
        this.markProcessActivity(process);
        endListener?.dispose();
      });

      void (async () => {
        try {
          for await (const data of stream) {
            if (process.state !== 'running' && process.state !== 'starting') {
              break;
            }
            output += data;
            this.appendProcessOutputChunk(process, data);
            this.markProcessActivity(process);
          }
          if (process.state === 'running' || process.state === 'starting') {
            process.state = 'completed';
            process.exitCode = process.exitCode ?? 0;
            process.endTime = Date.now();
            this.replaceProcessOutputSnapshot(process, output);
            this.releaseServiceLease(process);
            this.markProcessActivity(process);
          }
        } catch (error: any) {
          if (process.state === 'killed' || process.state === 'timeout') {
            return;
          }
          process.exitCode = 1;
          process.state = 'failed';
          process.endTime = Date.now();
          this.replaceProcessOutputSnapshot(process, output || String(error?.message || error));
          this.releaseServiceLease(process);
          this.markProcessActivity(process);
        } finally {
          endListener?.dispose();
        }
      })();

      return;
    }

    return new Promise((resolve, reject) => {
      let settled = false;
      const stateWatcher = setInterval(() => {
        if (settled) {
          return;
        }
        if (!this.isTerminalAlive(terminal)) {
          this.markProcessFailedOnTerminalClose(process, '终端已关闭，task 执行中断');
          settle('resolve');
          return;
        }
        if (process.state !== 'running' && process.state !== 'starting') {
          if (output) {
            this.replaceProcessOutputSnapshot(process, output);
          }
          settle('resolve');
        }
      }, PROCESS_WAIT_POLL_MS);

      const settle = (action: 'resolve' | 'reject', error?: Error) => {
        if (settled) return;
        settled = true;
        clearInterval(stateWatcher);
        endListener?.dispose();
        if (action === 'resolve') {
          resolve();
        } else {
          reject(error);
        }
      };

      // 方式1: 监听 onDidEndTerminalShellExecution 事件（可获取退出码）
      const endListener = vscode.window.onDidEndTerminalShellExecution?.((e) => {
        if (e.execution === execution) {
          process.exitCode = e.exitCode ?? 0;
          process.state = process.exitCode === 0 ? 'completed' : 'failed';
          this.replaceProcessOutputSnapshot(process, output);
          this.markProcessActivity(process);
          settle('resolve');
        }
      });

      // 方式2: 读取流收集输出，流结束作为兜底完成信号
      (async () => {
        try {
          for await (const data of stream) {
            if (settled) break;
            output += data;
            this.appendProcessOutputChunk(process, data);
            this.markProcessActivity(process);
          }
          // 流结束 — 如果事件还没触发，以流结束为准
          if (!settled) {
            process.state = 'completed';
            process.exitCode = process.exitCode ?? 0;
            this.replaceProcessOutputSnapshot(process, output);
            this.markProcessActivity(process);
            settle('resolve');
          }
        } catch (error: any) {
          if (process.state === 'killed' || process.state === 'timeout') {
            settle('resolve');
            return;
          }
          process.exitCode = 1;
          this.replaceProcessOutputSnapshot(process, output);
          settle('reject', error);
        }
      })();
    });
  }

  /**
   * 使用 ScriptCapture 策略执行命令
   */
  private async executeWithScriptCapture(
    process: TerminalProcess,
    command: string
  ): Promise<void> {
    const terminal = process.terminal;

    // 包装命令（更新文件位置等）
    const wrappedCommand = this.scriptCaptureStrategy.wrapCommand(
      command,
      process.id,
      terminal,
      true
    );
    process.actualCommand = wrappedCommand;

    // 发送命令
    terminal.sendText(wrappedCommand, true);

    if (process.runMode === 'service') {
      await this.delay(POLL_START_DELAY_MS);
      const outputResult = this.scriptCaptureStrategy.getOutputAndReturnCode?.(
        process.id,
        terminal,
        wrappedCommand,
        false
      );
      if (typeof outputResult === 'object') {
        this.replaceProcessOutputSnapshot(process, outputResult.output);
      } else if (typeof outputResult === 'string' && outputResult.trim().length > 0) {
        this.replaceProcessOutputSnapshot(process, outputResult);
      }
      this.markProcessActivity(process);
      return;
    }

    // 轮询检测完成状态
    return new Promise((resolve) => {
      const pollInterval = 150; // 150ms 轮询间隔

      const poll = () => {
        if (!this.isTerminalAlive(terminal)) {
          this.markProcessFailedOnTerminalClose(process, '终端已关闭，task 执行中断');
          resolve();
          return;
        }
        if (process.state === 'killed' || process.state === 'timeout') {
          resolve();
          return;
        }

        if (process.state === 'completed' || process.state === 'failed') {
          resolve();
          return;
        }

        if (this.scriptCaptureStrategy.hasOutputActivity(terminal)) {
          this.markProcessActivity(process);
        }

        // 检查完成状态
        const result = this.scriptCaptureStrategy.checkCompleted(process.id, terminal);

        if (result.isCompleted) {
          // 获取输出
          const outputResult = this.scriptCaptureStrategy.getOutputAndReturnCode?.(
            process.id,
            terminal,
            wrappedCommand,
            true
          );

          if (typeof outputResult === 'object') {
            this.replaceProcessOutputSnapshot(process, outputResult.output);
            process.exitCode = outputResult.returnCode;
          } else if (typeof outputResult === 'string') {
            this.replaceProcessOutputSnapshot(process, outputResult);
            process.exitCode = 0;
          }

          process.state = process.exitCode !== null && process.exitCode !== 0
            ? 'failed'
            : 'completed';
          this.markProcessActivity(process);
          resolve();
          return;
        }

        // 继续轮询
        setTimeout(poll, pollInterval);
      };

      // 等待一小段时间让命令开始执行
      setTimeout(poll, POLL_START_DELAY_MS);
    });
  }

  /**
   * 使用 sendText 执行命令（基础模式）
   */
  private async executeWithSendText(
    process: TerminalProcess,
    command: string
  ): Promise<void> {
    const terminal = process.terminal;

    terminal.sendText(command);

    if (process.runMode === 'service') {
      this.replaceProcessOutputSnapshot(process, '(service 命令已发送到终端（基础模式），请使用 read-process 观察输出)');
      this.markProcessActivity(process);
      return;
    }

    return new Promise((resolve) => {
      setTimeout(() => {
        process.state = 'completed';
        process.exitCode = 0;
        this.replaceProcessOutputSnapshot(process, '(命令已发送到终端，请查看终端窗口获取输出)');
        this.markProcessActivity(process);
        logger.info('命令已发送到终端 (基础模式)', {
          command,
          note: '无法捕获输出，请查看终端窗口',
        }, LogCategory.SHELL);
        resolve();
      }, SEND_TEXT_WAIT_MS);
    });
  }

  /**
   * 获取当前工作目录
   */
  getCwd(terminal?: vscode.Terminal): string | undefined {
    const t = terminal || this.mainTerminal;
    if (!t) return undefined;

    return this.scriptCaptureStrategy.getCurrentCwd?.(t);
  }

  /**
   * 显示终端
   */
  showTerminal(processId: number): boolean {
    const process = this.processes.get(processId);
    if (!process) {
      return false;
    }

    process.terminal.show(true);
    return true;
  }

  /**
   * 终止进程
   */
  async kill(processId: number): Promise<void> {
    const process = this.processes.get(processId);
    if (!process) {
      return;
    }

    await this.forceStopProcess(process, 'killed', 'legacy-kill');

    this.processes.delete(processId);
  }

  /**
   * 获取进程状态
   */
  getProcessStatus(processId: number): ProcessState | undefined {
    const process = this.processes.get(processId);
    return process?.state;
  }

  /**
   * 列出所有进程
   */
  listProcesses(): Array<{
    id: number;
    command: string;
    state: ProcessState;
    exitCode: number | null;
  }> {
    const result = [];
    for (const [id, process] of this.processes.entries()) {
      result.push({
        id,
        command: process.command,
        state: process.state,
        exitCode: process.exitCode,
      });
    }
    return result;
  }

  /**
   * 清理所有进程
   */
  cleanup(): void {
    logger.debug('清理所有终端进程', undefined, LogCategory.SHELL);

    for (const [, process] of this.processes.entries()) {
      if (process.state === 'running') {
        process.terminal.sendText('\x03');
      }
      this.cleanupTerminal(process.terminal);
      process.terminal.dispose();
    }

    this.processes.clear();
    this.stopProcessTasks.clear();
  }

  private async forceStopProcess(
    process: TerminalProcess,
    targetState: 'killed' | 'timeout',
    reason: string
  ): Promise<void> {
    const existing = this.stopProcessTasks.get(process.id);
    if (existing) {
      await existing;
      return;
    }

    const stopTask = (async () => {
      if (process.state === 'completed' || process.state === 'failed') {
        return;
      }
      if (process.state === 'killed' || process.state === 'timeout') {
        return;
      }

      const terminal = process.terminal;
      logger.warn('强制停止终端进程', {
        processId: process.id,
        reason,
        targetState,
        command: process.command,
      }, LogCategory.SHELL);

      try {
        terminal.sendText('\x03', false);
      } catch (error) {
        logger.debug('发送 Ctrl+C 失败', { processId: process.id, error }, LogCategory.SHELL);
      }

      await this.delay(100);

      try {
        await this.scriptCaptureStrategy.interruptActiveCommand(terminal);
      } catch (error) {
        logger.debug('终止子进程树失败', { processId: process.id, error }, LogCategory.SHELL);
      }

      this.releaseServiceLease(process);
      this.cleanupTerminal(terminal);
      terminal.dispose();

      process.state = targetState;
      process.exitCode = -1;
      process.endTime = Date.now();
      this.updateServiceStartupStatus(process, targetState === 'killed' ? 'failed' : 'timeout', `进程已${targetState === 'killed' ? '终止' : '超时终止'}`);
    })().finally(() => {
      this.stopProcessTasks.delete(process.id);
    });

    this.stopProcessTasks.set(process.id, stopTask);
    await stopTask;
  }

  /**
   * 验证命令是否安全
   *
   * 仅拦截系统安全级威胁（rm -rf /、fork bomb 等）。
   * 文件编辑场景允许通过脚本/命令执行，由上层流程自行约束与审计。
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

  private async waitForServiceProgress(
    processId: number,
    idleTimeoutMs: number,
    signal?: AbortSignal,
  ): Promise<void> {
    const process = this.processes.get(processId);
    if (!process) {
      return;
    }
    const initialCursor = process.outputCursor;
    const startAt = Date.now();

    while (true) {
      if (signal?.aborted) {
        // service 的 read/wait 观察被中断时，仅终止等待，不应终止后台进程
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
          // 启动握手等待被上层中断（如用户取消当前 LLM 轮次）时，
          // service 进程应继续后台运行，不应被隐式终止。
          this.updateServiceStartupStatus(activeProcess, 'skipped', '启动握手等待被中断，service 继续后台运行');
        }
        return;
      }

      const process = this.processes.get(processId);
      if (!process) {
        return;
      }

      await this.refreshProcessSnapshot(process, false);

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

      if (process.state !== 'running' && process.state !== 'starting') {
        return;
      }

      if (Date.now() - startedAt >= timeoutMs) {
        this.updateServiceStartupStatus(process, 'timeout', `启动握手超时（${Math.ceil(timeoutMs / 1000)}s）`);
        return;
      }

      await this.delay(PROCESS_WAIT_POLL_MS);
    }
  }

  private async runServiceSupervisorTick(): Promise<void> {
    if (this.serviceSupervisorTickInFlight) {
      return;
    }

    this.serviceSupervisorTickInFlight = true;
    try {
      const targets = Array.from(this.processes.values()).filter(
        (process) => process.runMode === 'service' && (process.state === 'running' || process.state === 'starting')
      );

      for (const process of targets) {
        if (!this.isTerminalAlive(process.terminal)) {
          process.state = 'failed';
          process.exitCode = process.exitCode ?? 1;
          process.endTime = Date.now();
          this.releaseServiceLease(process);
          this.updateServiceStartupStatus(process, 'failed', '终端已关闭，service 进程不可用');
          this.markProcessActivity(process);
          continue;
        }

        await this.refreshProcessSnapshot(process, false);

        const runtime = this.serviceRuntime.get(process.id);
        if (
          runtime
          && runtime.startupStatus === 'pending'
          && runtime.startupDeadlineAt
          && Date.now() >= runtime.startupDeadlineAt
        ) {
          const waitSeconds = Math.max(1, Math.ceil((runtime.startupDeadlineAt - process.startTime) / 1000));
          this.updateServiceStartupStatus(process, 'timeout', `启动握手超时（${waitSeconds}s）`);
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

  private replaceProcessOutputSnapshot(process: TerminalProcess, output: string): void {
    const nextOutput = typeof output === 'string' ? output : '';
    const previousOutput = process.output || '';
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

    process.outputCursor += appendedLength;
    process.output = nextOutput;
    this.trimProcessOutputBuffer(process);
    this.markProcessActivity(process);
  }

  private appendProcessOutputChunk(process: TerminalProcess, chunk: string): void {
    if (!chunk) {
      return;
    }
    process.output += chunk;
    process.outputCursor += chunk.length;
    this.trimProcessOutputBuffer(process);
    this.markProcessActivity(process);
  }

  private trimProcessOutputBuffer(process: TerminalProcess): void {
    if (process.output.length > PROCESS_OUTPUT_BUFFER_LIMIT) {
      const overflow = process.output.length - PROCESS_OUTPUT_BUFFER_LIMIT;
      process.output = process.output.slice(overflow);
    }
    process.outputStartCursor = Math.max(0, process.outputCursor - process.output.length);
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

  private refreshServiceReadiness(process: TerminalProcess): void {
    if (process.runMode !== 'service') {
      return;
    }

    const runtime = this.serviceRuntime.get(process.id);
    if (!runtime) {
      return;
    }

    if (!runtime.startupConfirmed && this.hasServiceReadySignal(process.output, runtime.readyPatterns)) {
      this.updateServiceStartupStatus(process, 'confirmed', '检测到服务就绪信号');
      return;
    }

    if (
      runtime.startupStatus === 'pending'
      && runtime.startupDeadlineAt
      && Date.now() >= runtime.startupDeadlineAt
    ) {
      const waitSeconds = Math.max(1, Math.ceil((runtime.startupDeadlineAt - process.startTime) / 1000));
      this.updateServiceStartupStatus(process, 'timeout', `启动握手超时（${waitSeconds}s）`);
    }
  }

  private updateServiceStartupStatus(
    process: TerminalProcess,
    status: ServiceRuntimeState['startupStatus'],
    message?: string,
  ): void {
    if (process.runMode !== 'service') {
      return;
    }

    const runtime = this.serviceRuntime.get(process.id);
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
      return;
    }
    if (status === 'timeout') {
      runtime.startupMessage = '服务启动握手超时';
      return;
    }
    if (status === 'skipped') {
      runtime.startupMessage = '未执行启动握手';
    }
  }

  private async refreshProcessSnapshot(process: TerminalProcess, isCompletedHint: boolean): Promise<void> {
    if (process.state === 'killed' || process.state === 'timeout') {
      return;
    }

    const terminal = process.terminal;
    if (!this.isTerminalAlive(terminal)) {
      this.markProcessFailedOnTerminalClose(process, '终端已关闭，进程不可用');
      return;
    }

    const scriptReady = this.scriptCaptureStrategy.isReady(terminal);
    let completed = isCompletedHint;

    if (scriptReady) {
      if (this.scriptCaptureStrategy.hasOutputActivity(terminal)) {
        this.markProcessActivity(process);
      }

      const completion = process.runMode === 'service'
        ? this.scriptCaptureStrategy.checkCompletedByMarker(process.id, terminal)
        : this.scriptCaptureStrategy.checkCompleted(process.id, terminal);
      completed = completion.isCompleted || isCompletedHint;

      const outputResult = this.scriptCaptureStrategy.getOutputAndReturnCode?.(
        process.id,
        terminal,
        process.actualCommand,
        completed
      );

      if (typeof outputResult === 'object') {
        this.replaceProcessOutputSnapshot(process, outputResult.output);
        if (completed && outputResult.returnCode !== null) {
          process.exitCode = outputResult.returnCode;
        }
      } else if (typeof outputResult === 'string' && outputResult.trim().length > 0) {
        this.replaceProcessOutputSnapshot(process, outputResult);
      }
    }

    // 无论是否使用 ScriptCapture，都要推进 service 就绪状态判断。
    // Shell Integration 分支的输出是通过流实时写入 process.output，此处负责把 startup_status 同步为 confirmed/timeout。
    this.refreshServiceReadiness(process);

    if (scriptReady && completed && (process.state === 'running' || process.state === 'starting')) {
      process.state = process.exitCode !== null && process.exitCode !== 0 ? 'failed' : 'completed';
      process.endTime = Date.now();
      this.releaseServiceLease(process);
      this.updateServiceStartupStatus(process, process.state === 'completed' ? 'confirmed' : 'failed');
      this.markProcessActivity(process);
    }
  }

  private buildLaunchResult(process: TerminalProcess): LaunchProcessResult {
    const cwd = this.terminalCwds.get(process.terminal) || this.getCwd(process.terminal);
    const runtime = this.serviceRuntime.get(process.id);
    return {
      terminal_id: process.id,
      status: process.state,
      output: process.output,
      return_code: process.exitCode,
      run_mode: process.runMode,
      phase: this.getProcessPhase(process),
      locked: this.isTerminalServiceLocked(process.terminal),
      terminal_name: process.terminalName,
      cwd,
      output_cursor: process.outputCursor,
      output_start_cursor: process.outputStartCursor,
      message: process.runMode === 'service'
        ? 'service 终端已锁定，后续命令将自动分配到溢出终端。'
        : undefined,
      startup_status: runtime?.startupStatus,
      startup_confirmed: runtime?.startupConfirmed,
      startup_message: runtime?.startupMessage,
    };
  }

  private buildReadResult(process: TerminalProcess, fromCursor?: number): ReadProcessResult {
    const normalizedFromCursor = Number.isInteger(fromCursor) && fromCursor !== undefined && fromCursor >= 0
      ? fromCursor
      : 0;
    const requestedStart = Math.min(normalizedFromCursor, process.outputCursor);
    const clampedStart = Math.max(requestedStart, process.outputStartCursor);
    const delta = fromCursor !== undefined;
    const relativeStart = Math.max(0, clampedStart - process.outputStartCursor);
    const output = delta ? process.output.slice(relativeStart) : process.output;
    const cwd = this.terminalCwds.get(process.terminal) || this.getCwd(process.terminal);

    return {
      status: process.state,
      output,
      return_code: process.exitCode,
      run_mode: process.runMode,
      phase: this.getProcessPhase(process),
      locked: this.isTerminalServiceLocked(process.terminal),
      terminal_name: process.terminalName,
      cwd,
      from_cursor: clampedStart,
      output_start_cursor: process.outputStartCursor,
      next_cursor: process.outputCursor,
      delta,
      truncated: delta && normalizedFromCursor < process.outputStartCursor,
      output_cursor: process.outputCursor,
    };
  }

  private getProcessPhase(process: TerminalProcess): ProcessPhase {
    if (process.state === 'starting') {
      return 'starting';
    }
    if (process.state === 'running') {
      if (process.runMode === 'service') {
        const runtime = this.serviceRuntime.get(process.id);
        if (runtime?.startupConfirmed || this.hasServiceReadySignal(process.output, runtime?.readyPatterns)) {
          return 'ready';
        }
      }
      return 'running';
    }
    if (process.state === 'completed') {
      return 'completed';
    }
    if (process.state === 'failed') {
      return 'failed';
    }
    if (process.state === 'killed') {
      return 'killed';
    }
    return 'timeout';
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

  private acquireServiceLease(process: TerminalProcess): void {
    this.serviceLeases.set(process.terminal, {
      processId: process.id,
      agentName: process.agentName,
      lockedAt: Date.now(),
    });
    process.serviceLocked = true;
  }

  private releaseServiceLease(process: TerminalProcess): boolean {
    const lease = this.serviceLeases.get(process.terminal);
    if (!lease || lease.processId !== process.id) {
      process.serviceLocked = false;
      return false;
    }
    this.serviceLeases.delete(process.terminal);
    process.serviceLocked = false;
    return true;
  }

  private isTerminalServiceLocked(terminal: vscode.Terminal): boolean {
    const lease = this.serviceLeases.get(terminal);
    if (!lease) {
      return false;
    }
    const owner = this.processes.get(lease.processId);
    if (!owner) {
      this.serviceLeases.delete(terminal);
      return false;
    }
    if (owner.state !== 'running' && owner.state !== 'starting') {
      this.serviceLeases.delete(terminal);
      owner.serviceLocked = false;
      return false;
    }
    owner.serviceLocked = true;
    return true;
  }

  private normalizeIdleTimeoutMs(maxWaitSeconds: number): number {
    const seconds = Number.isFinite(maxWaitSeconds) ? maxWaitSeconds : this.defaultTimeout / 1000;
    return Math.min(Math.max(seconds, 1) * 1000, this.maxTimeout);
  }

  private markProcessActivity(process: TerminalProcess): void {
    const now = Date.now();
    process.updatedAt = now;
    const runtime = this.serviceRuntime.get(process.id);
    if (runtime) {
      runtime.lastHeartbeatAt = now;
    }
  }


  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
