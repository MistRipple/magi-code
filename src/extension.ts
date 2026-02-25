/**
 * Magi VSCode 扩展入口
 */

import { logger, LogCategory } from './logging';
import * as vscode from 'vscode';
import { WebviewProvider } from './ui/webview-provider';
import { MermaidPanel } from './ui/mermaid-panel';
import { globalEventBus } from './events';

let webviewProvider: WebviewProvider | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;

/**
 * 扩展激活
 */
export async function activate(context: vscode.ExtensionContext): Promise<void> {
  logger.info('扩展.激活.开始', undefined, LogCategory.SYSTEM);

  // 获取工作区目录列表
  const workspaceFolders = vscode.workspace.workspaceFolders?.map(folder => ({
    name: folder.name,
    path: folder.uri.fsPath,
  })) || [];
  if (workspaceFolders.length === 0) {
    vscode.window.showWarningMessage('Magi: 请先打开一个工作区');
    return;
  }

  // 创建 Webview Provider
  webviewProvider = new WebviewProvider(
    context.extensionUri,
    context,
    workspaceFolders
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
  statusBarItem.command = 'magi.showPanel';
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // 监听任务状态变化，更新状态栏
  globalEventBus.on('task:started', () => {
    updateStatusBar('running');
    vscode.commands.executeCommand('setContext', 'magiTaskRunning', true);
  });
  globalEventBus.on('task:completed', () => {
    updateStatusBar('completed');
    vscode.commands.executeCommand('setContext', 'magiTaskRunning', false);
  });
  globalEventBus.on('task:failed', () => {
    updateStatusBar('failed');
    vscode.commands.executeCommand('setContext', 'magiTaskRunning', false);
  });
  globalEventBus.on('task:cancelled', () => {
    updateStatusBar('cancelled');
    vscode.commands.executeCommand('setContext', 'magiTaskRunning', false);
  });

  // 注册命令
  registerCommands(context);

  // 注册 Mermaid 面板序列化器（用于持久化恢复）
  MermaidPanel.registerSerializer(context);

  logger.info('扩展.初始化.完成', undefined, LogCategory.SYSTEM);
}

/**
 * 更新状态栏显示
 */
function updateStatusBar(status: 'idle' | 'running' | 'completed' | 'failed' | 'cancelled'): void {
  if (!statusBarItem) return;

  switch (status) {
    case 'idle':
      statusBarItem.text = '$(robot) Magi';
      statusBarItem.tooltip = '点击打开 Magi';
      statusBarItem.backgroundColor = undefined;
      break;
    case 'running':
      statusBarItem.text = '$(sync~spin) Magi';
      statusBarItem.tooltip = '任务执行中... 按 Escape 打断';
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
      break;
    case 'completed':
      statusBarItem.text = '$(check) Magi';
      statusBarItem.tooltip = '任务已完成';
      statusBarItem.backgroundColor = undefined;
      // 3秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 3000);
      break;
    case 'failed':
      statusBarItem.text = '$(error) Magi';
      statusBarItem.tooltip = '任务执行失败';
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.errorBackground');
      // 5秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 5000);
      break;
    case 'cancelled':
      statusBarItem.text = '$(debug-pause) Magi';
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
    vscode.commands.registerCommand('magi.openPanel', () => {
      vscode.commands.executeCommand('workbench.view.extension.magi');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.showPanel', () => {
      vscode.commands.executeCommand('workbench.view.extension.magi');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.startTask', () => {
      vscode.commands.executeCommand('workbench.view.extension.magi');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.newSession', async () => {
      if (!webviewProvider) {
        vscode.window.showWarningMessage('Magi: 面板未初始化');
        return;
      }
      try {
        await webviewProvider.createNewSession();
        vscode.window.showInformationMessage('Magi: 新会话已创建');
      } catch (error) {
        const msg = error instanceof Error ? error.message : String(error);
        vscode.window.showErrorMessage(`Magi: 创建会话失败 - ${msg}`);
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.showStatus', async () => {
      vscode.window.showInformationMessage('Magi: 使用 LLM API 模式运行');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.interruptTask', () => {
      globalEventBus.emitEvent('task:cancelled', {});
      vscode.window.showInformationMessage('Magi: 正在取消任务...');
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.stopTask', () => {
      globalEventBus.emitEvent('task:cancelled', {});
    })
  );

  // 注册 Mermaid 图表在新标签页打开命令
  context.subscriptions.push(
    vscode.commands.registerCommand('magi.openMermaidPanel', (code: string, title?: string) => {
      MermaidPanel.createOrShow(context.extensionUri, code, title);
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
