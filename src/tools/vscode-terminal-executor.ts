/**
 * VSCode Terminal Executor
 * 提供基于VSCode Terminal API的命令执行能力
 *
 * 采用双策略模式：
 * 1. VSCodeEventsStrategy - 当 Shell Integration 可用时使用
 * 2. ScriptCaptureStrategy - 降级策略，使用 script 命令捕获输出
 *
 * 参考 Augment 插件实现
 */

import * as vscode from 'vscode';
import { ShellExecuteOptions, ShellExecuteResult } from './types';
import { logger, LogCategory } from '../logging';
import {
  ShellType,
  TerminalProcess,
  ProcessState,
} from './terminal/types';
import { VSCodeEventsStrategy } from './terminal/vscode-events-strategy';
import { ScriptCaptureStrategy } from './terminal/script-capture-strategy';

/**
 * 检测 Shell 类型
 */
function detectShellType(terminal?: vscode.Terminal): ShellType {
  const shellPath = (terminal as any)?._creationOptions?.shellPath
    || vscode.env.shell
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
  private mainTerminalCwd: string | undefined = undefined;
  private mainTerminalBusy: boolean = false;
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
      if (this.mainTerminal === closedTerminal) {
        logger.debug('主终端被用户关闭', undefined, LogCategory.SHELL);
        this.cleanupTerminal(closedTerminal);
        this.mainTerminal = null;
        this.mainTerminalCwd = undefined;
        this.mainTerminalBusy = false;
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

    // 清理所有终端
    if (this.mainTerminal) {
      this.cleanupTerminal(this.mainTerminal);
      this.mainTerminal.dispose();
      this.mainTerminal = null;
    }

    this.terminalInitialized.clear();
    this.terminalShellType.clear();
  }

  /**
   * 执行 Shell 命令（使用VSCode Terminal）
   */
  async execute(options: ShellExecuteOptions): Promise<ShellExecuteResult> {
    const startTime = Date.now();
    const timeout = Math.min(
      options.timeout || this.defaultTimeout,
      this.maxTimeout
    );

    logger.debug('执行 Shell 命令', {
      command: options.command,
      cwd: options.cwd,
      timeout,
      showTerminal: options.showTerminal,
    }, LogCategory.SHELL);

    try {
      // 创建或复用终端
      const terminal = await this.getOrCreateTerminal(options);
      const processId = this.nextId++;

      // 标记主终端为忙碌状态
      const isMainTerminal = terminal === this.mainTerminal;
      if (isMainTerminal) {
        this.mainTerminalBusy = true;
      }

      // 如果需要显示终端
      if (options.showTerminal) {
        terminal.show(true);
      }

      // 获取 Shell 类型
      const shellType = this.terminalShellType.get(terminal) || detectShellType(terminal);

      // 注册进程
      const process: TerminalProcess = {
        id: processId,
        terminal,
        command: options.command,
        actualCommand: options.command,
        lastCommand: '',
        startTime,
        output: '',
        exitCode: null,
        state: 'running' as ProcessState,
      };
      this.processes.set(processId, process);

      // 执行命令
      await this.executeCommand(process, options.command, timeout, shellType);

      const duration = Date.now() - startTime;

      const result: ShellExecuteResult = {
        stdout: process.output,
        stderr: '',
        exitCode: process.exitCode || 0,
        duration,
      };

      logger.debug('Shell 命令完成', {
        command: options.command,
        exitCode: result.exitCode,
        duration,
        outputLength: result.stdout.length,
      }, LogCategory.SHELL);

      // 清理进程记录
      this.processes.delete(processId);

      // 命令完成后，标记主终端为空闲
      if (isMainTerminal) {
        this.mainTerminalBusy = false;
      }

      // 终端复用：只有在明确要求关闭且不是主终端时才关闭
      if (!options.keepTerminalOpen && terminal !== this.mainTerminal) {
        terminal.dispose();
      }

      return result;
    } catch (error: any) {
      // 异常时也要重置忙碌状态
      this.mainTerminalBusy = false;

      const duration = Date.now() - startTime;

      const result: ShellExecuteResult = {
        stdout: '',
        stderr: error.message,
        exitCode: 1,
        duration,
      };

      logger.error('Shell 命令失败', {
        command: options.command,
        duration,
        error: error.message,
      }, LogCategory.SHELL);

      return result;
    }
  }

  /**
   * 获取或创建终端
   */
  private async getOrCreateTerminal(options: ShellExecuteOptions): Promise<vscode.Terminal> {
    const terminalName = options.name || 'MultiCLI';
    const targetCwd = options.cwd;

    // 检查是否可以复用主终端（存活且空闲）
    if (this.mainTerminal && this.isTerminalAlive(this.mainTerminal) && !this.mainTerminalBusy) {
      logger.debug('复用现有主终端 (空闲)', {
        currentCwd: this.mainTerminalCwd,
        targetCwd
      }, LogCategory.SHELL);

      // 如果工作目录不同，先切换目录
      if (targetCwd && targetCwd !== this.mainTerminalCwd) {
        this.mainTerminal.sendText(`cd "${targetCwd}"`);
        this.mainTerminalCwd = targetCwd;
        await this.delay(100);
      }

      // 确保终端策略可用
      await this.ensureTerminalReady(this.mainTerminal);

      return this.mainTerminal;
    }

    // 主终端被占用时，创建新终端
    if (this.mainTerminal && this.isTerminalAlive(this.mainTerminal) && this.mainTerminalBusy) {
      logger.debug('主终端忙碌，创建新终端', undefined, LogCategory.SHELL);
    }

    // 创建新终端
    const terminalOptions: vscode.TerminalOptions = {
      name: terminalName,
      cwd: targetCwd,
      env: options.env,
      isTransient: false,
    };

    logger.debug('创建新 VSCode 终端', terminalOptions, LogCategory.SHELL);

    const terminal = vscode.window.createTerminal(terminalOptions);

    // 等待终端准备就绪
    await this.waitForTerminalReady(terminal);

    // 检测 Shell 类型
    const shellType = detectShellType(terminal);
    this.terminalShellType.set(terminal, shellType);

    // 初始化终端策略
    await this.initializeTerminalStrategy(terminal, shellType);

    // 设为主终端
    if (!this.mainTerminal || !this.isTerminalAlive(this.mainTerminal)) {
      this.mainTerminal = terminal;
      this.mainTerminalCwd = targetCwd;
      this.mainTerminalBusy = false;
    }

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

    // 降级到 ScriptCapture 策略
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
        setTimeout(() => reject(new Error('终端初始化超时')), 5000)
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
      setTimeout(poll, 100);
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
      }, 500);
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

  /**
   * 获取工具定义（用于 LLM）
   */
  getToolDefinition() {
    return {
      name: 'execute_shell',
      description: 'Execute a shell command in a VSCode terminal window. The terminal is shown to the user for visibility and interactive commands.',
      input_schema: {
        type: 'object' as const,
        properties: {
          command: {
            type: 'string' as const,
            description: 'The shell command to execute',
            required: true,
          },
          cwd: {
            type: 'string' as const,
            description: 'Working directory for the command (optional)',
            required: false,
          },
          timeout: {
            type: 'number' as const,
            description: 'Timeout in milliseconds (default: 30000, max: 300000)',
            required: false,
          },
          showTerminal: {
            type: 'boolean' as const,
            description: 'Whether to show the terminal window to the user (default: true)',
            required: false,
          },
          keepTerminalOpen: {
            type: 'boolean' as const,
            description: 'Whether to keep the terminal open after command completes (default: false)',
            required: false,
          },
          name: {
            type: 'string' as const,
            description: 'Name for the terminal window (default: "MultiCLI")',
            required: false,
          },
        },
        required: ['command'],
      },
    };
  }

  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
