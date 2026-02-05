/**
 * VSCode Events 完成检测策略
 *
 * 基于 VSCode Shell Integration API 的策略
 * 当 Shell Integration 可用时使用
 */

import * as vscode from 'vscode';
import {
  CompletionStrategy,
  CompletionCheckResult,
  ShellType,
} from './types';
import { logger, LogCategory } from '../../logging';

/**
 * 获取禁用历史展开命令
 */
function getDisableHistExpansionCommand(shellName: ShellType): string {
  switch (shellName.toLowerCase()) {
    case 'bash':
    case 'zsh':
      return 'set +o histexpand';
    case 'fish':
    case 'powershell':
    default:
      return '';
  }
}

/**
 * 获取清屏命令
 */
function getClearCommand(shellName: ShellType): string {
  switch (shellName.toLowerCase()) {
    case 'bash':
    case 'zsh':
    case 'fish':
      return 'clear';
    case 'powershell':
      return 'cls';
    default:
      return 'clear';
  }
}

/**
 * VSCode Events 完成检测策略
 *
 * 依赖 VSCode Shell Integration API
 */
export class VSCodeEventsStrategy implements CompletionStrategy {
  private static readonly LOG_PREFIX = 'VSCodeEventsStrategy';

  name(): string {
    return 'vscode_events';
  }

  async setupTerminal(
    terminal: vscode.Terminal,
    shellName: ShellType,
    startupScript?: string
  ): Promise<boolean> {
    // 执行启动脚本
    if (startupScript) {
      terminal.sendText(startupScript, true);
      await this.delay(100);
    }

    // 禁用历史展开
    const disableHistCmd = getDisableHistExpansionCommand(shellName);
    if (disableHistCmd) {
      await this.runCommandUntilCompletion(terminal, disableHistCmd, 'disable history expansion');
    }

    // 清屏
    const clearCmd = getClearCommand(shellName);
    await this.runCommandUntilCompletion(terminal, clearCmd, 'clear command');

    return true;
  }

  wrapCommand(
    command: string,
    _processId: number,
    _terminal: vscode.Terminal,
    _captureOutput: boolean
  ): string {
    // VSCode Events 策略不需要包装命令
    return command;
  }

  checkCompleted(
    _processId: number,
    _terminal: vscode.Terminal
  ): CompletionCheckResult {
    // VSCode Events 策略依赖 Shell Integration 的流完成事件
    // 这里返回未完成，实际完成由流事件处理
    return { isCompleted: false };
  }

  cleanupTerminal(_terminal: vscode.Terminal): void {
    // VSCode Events 策略无需特殊清理
  }

  isReady(terminal: vscode.Terminal): boolean {
    // 检查 Shell Integration 是否可用
    return !!terminal.shellIntegration;
  }

  async runCommandUntilCompletion(
    terminal: vscode.Terminal,
    command: string,
    description: string,
    _eventName?: string,
    _shellName?: ShellType,
    timeout: number = 1000
  ): Promise<boolean> {
    terminal.sendText(command, true);

    // 简单等待命令完成
    await this.delay(timeout);

    logger.debug(
      `${VSCodeEventsStrategy.LOG_PREFIX}: ${description} 完成`,
      { command },
      LogCategory.SHELL
    );

    return true;
  }

  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
