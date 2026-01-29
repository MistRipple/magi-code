/**
 * VSCode Terminal Executor
 * 提供基于VSCode Terminal API的命令执行能力
 * 参考Augment插件实现
 */

import * as vscode from 'vscode';
import { ShellExecuteOptions, ShellExecuteResult } from './types';
import { logger, LogCategory } from '../logging';

/**
 * 终端进程信息
 */
interface TerminalProcess {
  terminal: vscode.Terminal;
  command: string;
  startTime: number;
  output: string;
  exitCode: number | null;
  state: 'running' | 'completed' | 'killed';
  execution?: vscode.TerminalShellExecution;
}

/**
 * VSCode Terminal 执行器
 *
 * 🔧 优化：实现终端复用，避免每次命令都创建新终端
 * 只有当终端被占用（正在执行命令）时才创建新终端
 */
export class VSCodeTerminalExecutor {
  private processes: Map<number, TerminalProcess> = new Map();
  private nextId: number = 1;
  private readonly defaultTimeout: number = 30000; // 30 秒
  private readonly maxTimeout: number = 300000; // 5 分钟

  // 🔧 终端复用：主终端实例
  private mainTerminal: vscode.Terminal | null = null;
  private mainTerminalCwd: string | undefined = undefined;
  private mainTerminalBusy: boolean = false;  // 🔧 新增：终端是否被占用
  private terminalCloseListener: vscode.Disposable | null = null;

  constructor() {
    // 监听终端关闭事件，清理主终端引用
    this.terminalCloseListener = vscode.window.onDidCloseTerminal((closedTerminal) => {
      if (this.mainTerminal === closedTerminal) {
        logger.debug('Main terminal closed by user', undefined, LogCategory.SHELL);
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
    if (this.mainTerminal) {
      this.mainTerminal.dispose();
      this.mainTerminal = null;
    }
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

    logger.debug('Executing shell command in VSCode terminal', {
      command: options.command,
      cwd: options.cwd,
      timeout,
      showTerminal: options.showTerminal,
    }, LogCategory.SHELL);

    try {
      // 创建或复用终端
      const terminal = await this.createTerminal(options);
      const processId = this.nextId++;

      // 🔧 标记主终端为忙碌状态
      const isMainTerminal = terminal === this.mainTerminal;
      if (isMainTerminal) {
        this.mainTerminalBusy = true;
      }

      // 如果需要显示终端，则显示
      if (options.showTerminal) {
        terminal.show(true); // true = preserveFocus
        logger.debug('Terminal window shown to user', undefined, LogCategory.SHELL);
      }

      // 注册进程
      const process: TerminalProcess = {
        terminal,
        command: options.command,
        startTime,
        output: '',
        exitCode: null,
        state: 'running',
      };
      this.processes.set(processId, process);

      // 执行命令
      await this.executeCommand(process, options.command, timeout);

      const duration = Date.now() - startTime;

      const result: ShellExecuteResult = {
        stdout: process.output,
        stderr: '',
        exitCode: process.exitCode || 0,
        duration,
      };

      logger.debug('Shell command completed in terminal', {
        command: options.command,
        exitCode: result.exitCode,
        duration,
        outputLength: result.stdout.length,
      }, LogCategory.SHELL);

      // 清理进程记录
      this.processes.delete(processId);

      // 🔧 命令完成后，标记主终端为空闲
      if (isMainTerminal) {
        this.mainTerminalBusy = false;
      }

      // 🔧 终端复用：只有在明确要求关闭且不是主终端时才关闭
      // 主终端始终保持打开以便复用
      if (!options.keepTerminalOpen && terminal !== this.mainTerminal) {
        terminal.dispose();
      }

      return result;
    } catch (error: any) {
      // 🔧 异常时也要重置忙碌状态
      this.mainTerminalBusy = false;

      const duration = Date.now() - startTime;

      const result: ShellExecuteResult = {
        stdout: '',
        stderr: error.message,
        exitCode: 1,
        duration,
      };

      logger.error('Shell command failed in terminal', {
        command: options.command,
        duration,
        error: error.message,
      }, LogCategory.SHELL);

      return result;
    }
  }

  /**
   * 创建或复用 VSCode 终端
   *
   * 🔧 优化：实现终端复用
   * - 如果主终端存在且空闲，复用它
   * - 如果主终端被占用（正在运行服务等长时间命令），创建新终端
   */
  private async createTerminal(options: ShellExecuteOptions): Promise<vscode.Terminal> {
    const terminalName = options.name || 'MultiCLI';
    const targetCwd = options.cwd;

    // 检查是否可以复用主终端（存活且空闲）
    if (this.mainTerminal && this.isTerminalAlive(this.mainTerminal) && !this.mainTerminalBusy) {
      logger.debug('Reusing existing main terminal (idle)', {
        currentCwd: this.mainTerminalCwd,
        targetCwd
      }, LogCategory.SHELL);

      // 如果工作目录不同，先切换目录
      if (targetCwd && targetCwd !== this.mainTerminalCwd) {
        this.mainTerminal.sendText(`cd "${targetCwd}"`);
        this.mainTerminalCwd = targetCwd;
        // 等待 cd 命令执行
        await new Promise(resolve => setTimeout(resolve, 100));
      }

      return this.mainTerminal;
    }

    // 🔧 主终端被占用时，创建新终端（例如运行服务）
    if (this.mainTerminal && this.isTerminalAlive(this.mainTerminal) && this.mainTerminalBusy) {
      logger.debug('Main terminal is busy, creating new terminal', undefined, LogCategory.SHELL);
    }

    // 创建新终端
    const terminalOptions: vscode.TerminalOptions = {
      name: terminalName,
      cwd: targetCwd,
      env: options.env,
      isTransient: false, // 保留到终端历史以便复用
    };

    logger.debug('Creating new VSCode terminal', terminalOptions, LogCategory.SHELL);

    const terminal = vscode.window.createTerminal(terminalOptions);

    // 等待终端准备就绪
    await this.waitForTerminalReady(terminal);

    // 只有当主终端不存在或已关闭时，才将新终端设为主终端
    if (!this.mainTerminal || !this.isTerminalAlive(this.mainTerminal)) {
      this.mainTerminal = terminal;
      this.mainTerminalCwd = targetCwd;
      this.mainTerminalBusy = false;
    }

    return terminal;
  }

  /**
   * 检查终端是否仍然存活
   */
  private isTerminalAlive(terminal: vscode.Terminal): boolean {
    // 检查终端是否在当前打开的终端列表中
    return vscode.window.terminals.includes(terminal);
  }

  /**
   * 等待终端准备就绪
   */
  private async waitForTerminalReady(terminal: vscode.Terminal): Promise<void> {
    // 等待终端进程ID可用
    const processId = await Promise.race([
      terminal.processId,
      new Promise<number>((_, reject) =>
        setTimeout(() => reject(new Error('Terminal initialization timeout')), 5000)
      ),
    ]);

    logger.debug('Terminal ready', { processId }, LogCategory.SHELL);
  }

  /**
   * 执行命令
   */
  private async executeCommand(
    process: TerminalProcess,
    command: string,
    timeout: number
  ): Promise<void> {
    const terminal = process.terminal;

    // 尝试使用Shell Integration（VSCode 1.93+）
    if (terminal.shellIntegration) {
      logger.debug('Using shell integration to execute command', undefined, LogCategory.SHELL);
      await this.executeWithShellIntegration(process, command, timeout);
    } else {
      logger.debug('Shell integration not available, using sendText', undefined, LogCategory.SHELL);
      await this.executeWithSendText(process, command, timeout);
    }
  }

  /**
   * 使用Shell Integration执行命令
   */
  private async executeWithShellIntegration(
    process: TerminalProcess,
    command: string,
    timeout: number
  ): Promise<void> {
    const terminal = process.terminal;
    const shellIntegration = terminal.shellIntegration!;

    // 执行命令
    const execution = shellIntegration.executeCommand(command);
    process.execution = execution;

    // 读取输出流
    const stream = execution.read();

    return new Promise((resolve, reject) => {
      const timeoutId = setTimeout(() => {
        process.state = 'killed';
        process.exitCode = -1;
        reject(new Error(`Command execution timeout after ${timeout}ms`));
      }, timeout);

      let output = '';

      // 使用 async iterator 读取流
      (async () => {
        try {
          for await (const data of stream) {
            output += data;
            process.output = output;
          }

          // 流结束，命令执行完成
          // 注意：VSCode Shell Integration 可能不提供退出码
          // 我们假设成功完成（退出码0）
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
   * 使用sendText执行命令（降级方案）
   * 注意：没有Shell Integration时，无法准确获取输出和退出码
   * 这是一个降级方案，只适用于用户想要显示终端的场景
   */
  private async executeWithSendText(
    process: TerminalProcess,
    command: string,
    timeout: number
  ): Promise<void> {
    const terminal = process.terminal;

    // 发送命令
    terminal.sendText(command);

    // 由于没有 Shell Integration，我们无法获取命令输出
    // 设置一个较短的等待时间让命令开始执行，然后返回
    // 终端会保持打开，用户可以看到输出
    return new Promise((resolve) => {
      // 给命令一个短暂的启动时间
      setTimeout(() => {
        process.state = 'completed';
        process.exitCode = 0;
        process.output = '(命令已发送到终端，请查看终端窗口获取输出)';
        logger.info('Command sent to terminal (no shell integration)', {
          command,
          note: 'Output not captured, please check terminal window',
        }, LogCategory.SHELL);
        resolve();
      }, 500); // 给命令500ms启动时间
    });
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

    logger.debug('Killing terminal process', { processId }, LogCategory.SHELL);

    process.state = 'killed';
    process.exitCode = -1;

    // 发送Ctrl+C
    process.terminal.sendText('\x03');

    // 等待一小段时间
    await new Promise(resolve => setTimeout(resolve, 100));

    // 关闭终端
    process.terminal.dispose();

    this.processes.delete(processId);
  }

  /**
   * 获取进程状态
   */
  getProcessStatus(processId: number): 'running' | 'completed' | 'killed' | undefined {
    const process = this.processes.get(processId);
    return process?.state;
  }

  /**
   * 列出所有进程
   */
  listProcesses(): Array<{
    id: number;
    command: string;
    state: 'running' | 'completed' | 'killed';
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
    logger.debug('Cleaning up all terminal processes', undefined, LogCategory.SHELL);

    for (const [id, process] of this.processes.entries()) {
      if (process.state === 'running') {
        process.terminal.sendText('\x03'); // Ctrl+C
      }
      process.terminal.dispose();
    }

    this.processes.clear();
  }

  /**
   * 验证命令是否安全
   */
  validateCommand(command: string): { valid: boolean; reason?: string } {
    // 基本的安全检查
    const dangerousPatterns = [
      /rm\s+-rf\s+\//, // 删除根目录
      /:\(\)\{.*\}/, // Fork bomb
      />\s*\/dev\/sda/, // 写入磁盘设备
    ];

    for (const pattern of dangerousPatterns) {
      if (pattern.test(command)) {
        return {
          valid: false,
          reason: `Command contains dangerous pattern: ${pattern}`,
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
      name: 'execute_shell',  // 统一使用 execute_shell 作为工具名
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
}

