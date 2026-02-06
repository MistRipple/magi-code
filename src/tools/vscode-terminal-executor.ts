/**
 * VSCode Terminal Executor
 * 提供基于VSCode Terminal API的命令执行能力
 *
 * 采用双策略模式：
 * 1. VSCodeEventsStrategy - 当 Shell Integration 可用时使用
 * 2. ScriptCaptureStrategy - 备用策略，使用 script 命令捕获输出
 *
 * 参考 Augment 插件实现
 */

import * as vscode from 'vscode';
import {
  KillProcessResult,
  LaunchProcessOptions,
  LaunchProcessResult,
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

// ============================================================================
// 超时常量
// ============================================================================

/** 终端初始化超时 (ms) */
const TERMINAL_INIT_TIMEOUT_MS = 5000;

/** 基础模式命令发送后等待时间 (ms) */
const SEND_TEXT_WAIT_MS = 500;

/** 轮询起始延迟 (ms) */
const POLL_START_DELAY_MS = 100;

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

/**
 * VSCode Terminal 执行器
 *
 * 实现终端复用和双策略模式
 */
export class VSCodeTerminalExecutor {
  private processes: Map<number, TerminalProcess> = new Map();
  private nextId: number = 1;
  private readonly defaultTimeout: number = 30000; // 30 秒
  private readonly maxTimeout: number = 300000; // 5 分钟

  // 终端复用
  private mainTerminal: vscode.Terminal | null = null;
  private terminalCwds: Map<vscode.Terminal, string | undefined> = new Map();
  private terminalBusy: Map<vscode.Terminal, boolean> = new Map();
  private managedTerminals: Set<vscode.Terminal> = new Set();
  private agentTerminals: Map<string, vscode.Terminal> = new Map();
  private terminalAgentNames: Map<vscode.Terminal, string> = new Map();
  private terminalCloseListener: vscode.Disposable | null = null;

  // 双策略
  private vscodeEventsStrategy: VSCodeEventsStrategy;
  private scriptCaptureStrategy: ScriptCaptureStrategy;

  // 终端初始化状态
  private terminalInitialized: Map<vscode.Terminal, boolean> = new Map();
  private terminalShellType: Map<vscode.Terminal, ShellType> = new Map();

  constructor() {
    this.vscodeEventsStrategy = new VSCodeEventsStrategy();
    this.scriptCaptureStrategy = new ScriptCaptureStrategy();

    // 监听终端关闭事件
    this.terminalCloseListener = vscode.window.onDidCloseTerminal((closedTerminal) => {
      this.cleanupTerminal(closedTerminal);

      if (this.mainTerminal === closedTerminal) {
        logger.debug('主终端被用户关闭', undefined, LogCategory.SHELL);
      }
    });
  }

  /**
   * 清理资源
   */
  dispose(): void {
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
    this.terminalBusy.clear();
    this.managedTerminals.clear();
    this.agentTerminals.clear();
    this.terminalAgentNames.clear();
    this.terminalInitialized.clear();
    this.terminalShellType.clear();
  }


  async launchProcess(options: LaunchProcessOptions): Promise<LaunchProcessResult> {
    const timeout = Math.min(Math.max(options.maxWaitSeconds, 1) * 1000, this.maxTimeout);
    const agentName = (options.name || '').trim();
    if (!agentName) {
      throw new Error('launch-process 必须提供 agent 终端名称（orchestrator、worker-claude、worker-gemini、worker-codex）');
    }
    if (!ALLOWED_AGENT_TERMINAL_NAMES.has(agentName)) {
      throw new Error('launch-process name 仅支持 orchestrator、worker-claude、worker-gemini、worker-codex');
    }

    const terminal = await this.getOrCreateTerminal({
      cwd: options.cwd,
      env: undefined,
      name: agentName,
    });

    if (options.showTerminal ?? true) {
      terminal.show(true);
    }

    const processId = this.nextId++;
    const shellType = this.terminalShellType.get(terminal) || detectShellType(terminal);
    const process: TerminalProcess = {
      id: processId,
      terminal,
      command: options.command,
      actualCommand: options.command,
      lastCommand: '',
      startTime: Date.now(),
      output: '',
      exitCode: null,
      state: 'starting',
    };
    this.processes.set(processId, process);
    this.terminalBusy.set(terminal, true);

    process.state = 'running';
    void this.executeCommand(process, options.command, timeout, shellType)
      .then(() => {
        if (process.state === 'running') {
          process.state = process.exitCode === 0 ? 'completed' : 'failed';
        }
      })
      .catch((error: any) => {
        if (process.state !== 'killed' && process.state !== 'timeout') {
          process.state = 'failed';
          process.exitCode = process.exitCode ?? 1;
          process.output = process.output || String(error?.message || error);
        }
      })
      .finally(() => {
        process.endTime = Date.now();
        this.terminalBusy.set(terminal, false);
      });

    if (options.wait) {
      await this.waitForProcessState(processId, timeout);
    }

    return {
      terminal_id: processId,
      status: process.state,
      output: process.output,
      return_code: process.exitCode,
    };
  }

  async readProcess(terminalId: number, wait: boolean, maxWaitSeconds: number): Promise<ReadProcessResult> {
    const process = this.processes.get(terminalId);
    if (!process) {
      throw new Error(`终端进程不存在: ${terminalId}`);
    }

    if (wait && (process.state === 'running' || process.state === 'starting')) {
      const timeout = Math.min(Math.max(maxWaitSeconds, 1) * 1000, this.maxTimeout);
      await this.waitForProcessState(terminalId, timeout);
    }

    const cwd = this.terminalCwds.get(process.terminal) || this.getCwd(process.terminal);
    return {
      status: process.state,
      output: process.output,
      return_code: process.exitCode,
      cwd,
    };
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
      };
    }

    process.terminal.sendText(inputText);
    return {
      accepted: true,
      status: process.state,
    };
  }

  async killProcess(terminalId: number): Promise<KillProcessResult> {
    const process = this.processes.get(terminalId);
    if (!process) {
      return {
        killed: false,
        final_output: '',
        return_code: null,
      };
    }

    process.state = 'killed';
    process.exitCode = process.exitCode ?? -1;
    process.terminal.sendText('\x03');
    await this.delay(100);

    process.endTime = Date.now();
    this.terminalBusy.set(process.terminal, false);

    return {
      killed: true,
      final_output: process.output,
      return_code: process.exitCode,
    };
  }

  listProcessRecords(): Array<{
    terminal_id: number;
    status: ProcessState;
    command: string;
    started_at: number;
  }> {
    const result: Array<{
      terminal_id: number;
      status: ProcessState;
      command: string;
      started_at: number;
    }> = [];

    for (const [id, process] of this.processes.entries()) {
      result.push({
        terminal_id: id,
        status: process.state,
        command: process.command,
        started_at: process.startTime,
      });
    }

    return result;
  }

  private async waitForProcessState(processId: number, timeoutMs: number): Promise<void> {
    const startedAt = Date.now();

    while (Date.now() - startedAt < timeoutMs) {
      const process = this.processes.get(processId);
      if (!process) {
        return;
      }

      if (process.state !== 'running' && process.state !== 'starting') {
        return;
      }

      await this.delay(100);
    }

    const process = this.processes.get(processId);
    if (process && (process.state === 'running' || process.state === 'starting')) {
      process.state = 'timeout';
      process.exitCode = process.exitCode ?? null;
      process.endTime = Date.now();
      this.terminalBusy.set(process.terminal, false);
    }
  }



  /**
   * 获取或创建终端
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

    if (agentTerminal && this.isTerminalAlive(agentTerminal) && !this.terminalBusy.get(agentTerminal)) {
      const currentCwd = this.terminalCwds.get(agentTerminal);
      logger.debug('复用 agent 专属终端', { agentName, currentCwd, targetCwd }, LogCategory.SHELL);

      if (targetCwd && targetCwd !== currentCwd) {
        agentTerminal.sendText(`cd "${targetCwd}"`);
        this.terminalCwds.set(agentTerminal, targetCwd);
        await this.delay(100);
      }

      await this.ensureTerminalReady(agentTerminal);
      this.mainTerminal = agentTerminal;
      return agentTerminal;
    }

    if (agentTerminal && !this.isTerminalAlive(agentTerminal)) {
      this.cleanupTerminal(agentTerminal);
    }

    logger.debug('创建 agent 专属终端', { agentName, cwd: targetCwd }, LogCategory.SHELL);
    const terminal = vscode.window.createTerminal({
      name: agentName,
      cwd: targetCwd,
      env: options.env,
      isTransient: false,
    });
    await this.waitForTerminalReady(terminal);

    const shellType = detectShellType(terminal);
    this.terminalShellType.set(terminal, shellType);
    await this.initializeTerminalStrategy(terminal, shellType);

    this.managedTerminals.add(terminal);
    this.terminalCwds.set(terminal, targetCwd);
    this.terminalBusy.set(terminal, false);
    this.agentTerminals.set(agentName, terminal);
    this.terminalAgentNames.set(terminal, agentName);
    this.mainTerminal = terminal;

    return terminal;
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
    this.terminalInitialized.delete(terminal);
    this.terminalShellType.delete(terminal);
    this.terminalCwds.delete(terminal);
    this.terminalBusy.delete(terminal);
    this.managedTerminals.delete(terminal);

    const agentName = this.terminalAgentNames.get(terminal);
    if (agentName) {
      const mappedTerminal = this.agentTerminals.get(agentName);
      if (mappedTerminal === terminal) {
        this.agentTerminals.delete(agentName);
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
    command: string,
    timeout: number,
    shellType: ShellType
  ): Promise<void> {
    const terminal = process.terminal;

    // 使用 Shell Integration（如果可用）
    if (terminal.shellIntegration) {
      logger.debug('使用 Shell Integration 执行命令', undefined, LogCategory.SHELL);
      await this.executeWithShellIntegration(process, command, timeout);
      return;
    }

    // 使用 ScriptCapture 策略
    if (this.scriptCaptureStrategy.isReady(terminal)) {
      logger.debug('使用 ScriptCapture 策略执行命令', undefined, LogCategory.SHELL);
      await this.executeWithScriptCapture(process, command, timeout, shellType);
      return;
    }

    // 基础模式：只发送命令，无法获取输出
    logger.debug('使用基础模式执行命令（无法获取输出）', undefined, LogCategory.SHELL);
    await this.executeWithSendText(process, command, timeout);
  }

  /**
   * 使用 Shell Integration 执行命令
   */
  private async executeWithShellIntegration(
    process: TerminalProcess,
    command: string,
    timeout: number
  ): Promise<void> {
    const terminal = process.terminal;
    const shellIntegration = terminal.shellIntegration!;

    const execution = shellIntegration.executeCommand(command);
    process.execution = execution;

    const stream = execution.read();

    return new Promise((resolve, reject) => {
      const timeoutId = setTimeout(() => {
        process.state = 'killed';
        process.exitCode = -1;
        reject(new Error(`命令执行超时 (${timeout}ms)`));
      }, timeout);

      let output = '';

      (async () => {
        try {
          for await (const data of stream) {
            output += data;
            process.output = output;
          }

          clearTimeout(timeoutId);
          process.state = 'completed';
          process.exitCode = 0;
          process.output = output;
          resolve();
        } catch (error: any) {
          clearTimeout(timeoutId);
          process.state = 'completed';
          process.exitCode = 1;
          process.output = output;
          reject(error);
        }
      })();
    });
  }

  /**
   * 使用 ScriptCapture 策略执行命令
   */
  private async executeWithScriptCapture(
    process: TerminalProcess,
    command: string,
    timeout: number,
    shellType: ShellType
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

    // 轮询检测完成状态
    return new Promise((resolve, reject) => {
      const startTime = Date.now();
      const pollInterval = 200; // 200ms 轮询间隔

      const poll = () => {
        // 检查超时
        if (Date.now() - startTime > timeout) {
          process.state = 'killed';
          process.exitCode = -1;
          reject(new Error(`命令执行超时 (${timeout}ms)`));
          return;
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
            process.output = outputResult.output;
            process.exitCode = outputResult.returnCode;
          } else if (typeof outputResult === 'string') {
            process.output = outputResult;
            process.exitCode = 0;
          }

          process.state = 'completed';
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
    command: string,
    timeout: number
  ): Promise<void> {
    const terminal = process.terminal;

    terminal.sendText(command);

    return new Promise((resolve) => {
      setTimeout(() => {
        process.state = 'completed';
        process.exitCode = 0;
        process.output = '(命令已发送到终端，请查看终端窗口获取输出)';
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

    logger.debug('终止终端进程', { processId }, LogCategory.SHELL);

    process.state = 'killed';
    process.exitCode = -1;

    // 发送 Ctrl+C
    process.terminal.sendText('\x03');

    await this.delay(100);

    // 关闭终端
    process.terminal.dispose();

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

    for (const [id, process] of this.processes.entries()) {
      if (process.state === 'running') {
        process.terminal.sendText('\x03');
      }
      this.cleanupTerminal(process.terminal);
      process.terminal.dispose();
    }

    this.processes.clear();
  }

  /**
   * 验证命令是否安全
   */
  validateCommand(command: string): { valid: boolean; reason?: string } {
    const dangerousPatterns = [
      /rm\s+-rf\s+\//, // 删除根目录
      /:\(\)\{.*\}/, // Fork bomb
      />\s*\/dev\/sda/, // 写入磁盘设备
    ];

    for (const pattern of dangerousPatterns) {
      if (pattern.test(command)) {
        return {
          valid: false,
          reason: `命令包含危险模式: ${pattern}`,
        };
      }
    }

    return { valid: true };
  }


  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
