/**
 * MultiCLI VSCode 扩展入口
 */

import { logger, LogCategory } from './logging';
import * as vscode from 'vscode';
import { WebviewProvider } from './ui/webview-provider';
import { globalEventBus } from './events';
import { registerTerminalTest } from './test/terminal-test';

let webviewProvider: WebviewProvider | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;

/**
 * 扩展激活
 */
export async function activate(context: vscode.ExtensionContext): Promise<void> {
  logger.info('扩展.激活.开始', undefined, LogCategory.SYSTEM);

  // 获取工作区根目录
  const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!workspaceRoot) {
    vscode.window.showWarningMessage('MultiCLI: 请先打开一个工作区');
    return;
  }

  // 创建 Webview Provider
  webviewProvider = new WebviewProvider(
    context.extensionUri,
    context,
    workspaceRoot
  );

  // 注册 Webview Provider
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      WebviewProvider.viewType,
      webviewProvider
    )
  );

  // 创建状态栏项
  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    100
  );
  updateStatusBar('idle');
  statusBarItem.command = 'multiCli.showPanel';
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // 监听任务状态变化，更新状态栏
  globalEventBus.on('task:started', () => {
    updateStatusBar('running');
    vscode.commands.executeCommand('setContext', 'multiCliTaskRunning', true);
  });
  globalEventBus.on('task:completed', () => {
    updateStatusBar('completed');
    vscode.commands.executeCommand('setContext', 'multiCliTaskRunning', false);
  });
  globalEventBus.on('task:failed', () => {
    updateStatusBar('failed');
    vscode.commands.executeCommand('setContext', 'multiCliTaskRunning', false);
  });
  globalEventBus.on('task:cancelled', () => {
    updateStatusBar('cancelled');
    vscode.commands.executeCommand('setContext', 'multiCliTaskRunning', false);
  });

  // 注册命令
  registerCommands(context);

  // 注册终端测试命令
  registerTerminalTest(context);

  logger.info('扩展.初始化.完成', undefined, LogCategory.SYSTEM);
}

/**
 * 更新状态栏显示
 */
function updateStatusBar(status: 'idle' | 'running' | 'completed' | 'failed' | 'cancelled'): void {
  if (!statusBarItem) return;

  switch (status) {
    case 'idle':
      statusBarItem.text = '$(robot) MultiCLI';
      statusBarItem.tooltip = '点击打开 MultiCLI';
      statusBarItem.backgroundColor = undefined;
      break;
    case 'running':
      statusBarItem.text = '$(sync~spin) MultiCLI';
      statusBarItem.tooltip = '任务执行中... 按 Escape 打断';
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
      break;
    case 'completed':
      statusBarItem.text = '$(check) MultiCLI';
      statusBarItem.tooltip = '任务已完成';
      statusBarItem.backgroundColor = undefined;
      // 3秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 3000);
      break;
    case 'failed':
      statusBarItem.text = '$(error) MultiCLI';
      statusBarItem.tooltip = '任务执行失败';
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.errorBackground');
      // 5秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 5000);
      break;
    case 'cancelled':
      statusBarItem.text = '$(debug-pause) MultiCLI';
      statusBarItem.tooltip = '任务已取消';
      statusBarItem.backgroundColor = undefined;
      // 3秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 3000);
      break;
  }
}

/**
 * 注册所有命令
 */
function registerCommands(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.openPanel', () => {
      vscode.commands.executeCommand('workbench.view.extension.multiCli');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.showPanel', () => {
      vscode.commands.executeCommand('workbench.view.extension.multiCli');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.startTask', () => {
      vscode.commands.executeCommand('workbench.view.extension.multiCli');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.newSession', async () => {
      if (!webviewProvider) {
        vscode.window.showWarningMessage('MultiCLI: 面板未初始化');
        return;
      }
      try {
        await webviewProvider.createNewSession();
        vscode.window.showInformationMessage('MultiCLI: 新会话已创建');
      } catch (error) {
        const msg = error instanceof Error ? error.message : String(error);
        vscode.window.showErrorMessage(`MultiCLI: 创建会话失败 - ${msg}`);
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.showStatus', async () => {
      vscode.window.showInformationMessage('MultiCLI: 使用 LLM API 模式运行');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.interruptTask', () => {
      globalEventBus.emitEvent('task:cancelled', {});
      vscode.window.showInformationMessage('MultiCLI: 正在取消任务...');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('multiCli.stopTask', () => {
      globalEventBus.emitEvent('task:cancelled', {});
    })
  );
}

/**
 * 扩展停用 - 确保所有资源被正确清理
 */
export async function deactivate(): Promise<void> {
  logger.info('扩展.停用.开始', undefined, LogCategory.SYSTEM);

  try {
    // 清理 WebviewProvider（包括编排器、事件监听器）
    if (webviewProvider) {
      await webviewProvider.dispose();
      webviewProvider = undefined;
      logger.info('扩展.停用.Webview.已清理', undefined, LogCategory.UI);
    }

    // 清理状态栏
    if (statusBarItem) {
      statusBarItem.dispose();
      statusBarItem = undefined;
      logger.info('扩展.停用.状态栏.已清理', undefined, LogCategory.SYSTEM);
    }

    logger.info('扩展.停用.完成', undefined, LogCategory.SYSTEM);
  } catch (error) {
    logger.error('扩展.停用.失败', error, LogCategory.SYSTEM);
  }
}
