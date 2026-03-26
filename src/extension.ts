/**
 * Magi VSCode 扩展入口
 */

import { logger, LogCategory } from './logging';
import * as vscode from 'vscode';
import { WebviewProvider } from './ui/webview-provider';
import { MermaidPanel } from './ui/mermaid-panel';
import { globalEventBus } from './events';
import { t, setLocale as setExtensionLocale } from './i18n';
import { ConfigManager } from './config';
import { LocalAgentManager } from './agent/client/local-agent-manager';

let webviewProvider: WebviewProvider | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;
let localAgentManager: LocalAgentManager | undefined;

/**
 * 扩展激活
 */
export async function activate(context: vscode.ExtensionContext): Promise<void> {
  logger.info('扩展.激活.开始', undefined, LogCategory.SYSTEM);
  const locale = ConfigManager.getInstance().get('locale');
  setExtensionLocale(locale === 'en-US' ? 'en-US' : 'zh-CN');

  // 获取工作区目录列表
  const workspaceFolders = vscode.workspace.workspaceFolders?.map(folder => ({
    name: folder.name,
    path: folder.uri.fsPath,
  })) || [];
  if (workspaceFolders.length === 0) {
    vscode.window.showWarningMessage(t('extension.openWorkspaceFirst'));
    return;
  }

  // 启动门闩：Magi 仅支持 Agent 单链，必须先确保 Local Agent 后台可用。
  localAgentManager = new LocalAgentManager(context.extensionPath);
  let agentBaseUrl = '';
  try {
    await localAgentManager.ensureStarted();
    agentBaseUrl = localAgentManager.getBaseUrl();
    logger.info('扩展.Agent.已就绪', { baseUrl: agentBaseUrl }, LogCategory.SYSTEM);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    logger.error('扩展.Agent.启动失败', { error: message }, LogCategory.SYSTEM);
    await localAgentManager.dispose().catch((disposeError) => {
      logger.warn('扩展.Agent.失败后清理异常', {
        error: disposeError instanceof Error ? disposeError.message : String(disposeError),
      }, LogCategory.SYSTEM);
    });
    localAgentManager = undefined;
    vscode.window.showErrorMessage(`Local Agent 启动失败，Magi 仅支持 Agent 模式：${message}`);
    throw new Error(`Local Agent 启动失败，无法进入 Agent-only 单链模式：${message}`);
  }

  // 创建 WebviewProvider：插件面板仅作为 Agent Web 客户端壳层。
  webviewProvider = new WebviewProvider(
    context.extensionUri,
    context,
    workspaceFolders,
    agentBaseUrl,
  );

  // 注册 Webview Provider（壳）
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      WebviewProvider.viewType,
      webviewProvider,
      {
        webviewOptions: {
          retainContextWhenHidden: true
        }
      }
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
  globalEventBus.on('task:paused', () => {
    updateStatusBar('paused');
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
function updateStatusBar(status: 'idle' | 'running' | 'completed' | 'failed' | 'paused' | 'cancelled'): void {
  if (!statusBarItem) return;

  switch (status) {
    case 'idle':
      statusBarItem.text = '$(robot) Magi';
      statusBarItem.tooltip = t('extension.tooltip.clickToOpen');
      statusBarItem.backgroundColor = undefined;
      break;
    case 'running':
      statusBarItem.text = '$(sync~spin) Magi';
      statusBarItem.tooltip = t('extension.tooltip.taskRunning');
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
      break;
    case 'completed':
      statusBarItem.text = '$(check) Magi';
      statusBarItem.tooltip = t('extension.tooltip.taskCompleted');
      statusBarItem.backgroundColor = undefined;
      // 3秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 3000);
      break;
    case 'failed':
      statusBarItem.text = '$(error) Magi';
      statusBarItem.tooltip = t('extension.tooltip.taskFailed');
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.errorBackground');
      // 5秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 5000);
      break;
    case 'paused':
      statusBarItem.text = '$(debug-pause) Magi';
      statusBarItem.tooltip = t('extension.tooltip.taskRunning');
      statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
      // 3秒后恢复默认状态
      setTimeout(() => updateStatusBar('idle'), 3000);
      break;
    case 'cancelled':
      statusBarItem.text = '$(debug-pause) Magi';
      statusBarItem.tooltip = t('extension.tooltip.taskCancelled');
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
        vscode.window.showWarningMessage(t('extension.panelNotInit'));
        return;
      }
      try {
        await webviewProvider.createNewSession();
        vscode.window.showInformationMessage(t('extension.newSessionCreated'));
      } catch (error) {
        const msg = error instanceof Error ? error.message : String(error);
        vscode.window.showErrorMessage(t('extension.createSessionFailed', { error: msg }));
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.showStatus', async () => {
      vscode.window.showInformationMessage(t('extension.llmApiMode'));
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.openWebClient', async () => {
      if (!localAgentManager) {
        vscode.window.showWarningMessage('Local Agent 尚未初始化');
        return;
      }
      try {
        await localAgentManager.openWebClient();
      } catch (error) {
        const msg = error instanceof Error ? error.message : String(error);
        vscode.window.showErrorMessage(`打开 Web 客户端失败：${msg}`);
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.restartAgent', async () => {
      if (!localAgentManager) {
        vscode.window.showWarningMessage('Local Agent 尚未初始化');
        return;
      }
      try {
        await localAgentManager.restart();
        if (webviewProvider) {
          await webviewProvider.refreshAgentBaseUrl(localAgentManager.getBaseUrl());
        }
        vscode.window.showInformationMessage('Local Agent 已重启');
      } catch (error) {
        const msg = error instanceof Error ? error.message : String(error);
        vscode.window.showErrorMessage(`重启 Local Agent 失败：${msg}`);
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.showAgentStatus', async () => {
      if (!localAgentManager) {
        vscode.window.showWarningMessage('Local Agent 尚未初始化');
        return;
      }
      const healthy = await localAgentManager.isHealthy();
      vscode.window.showInformationMessage(
        healthy
          ? `Local Agent 已连接：${localAgentManager.getBaseUrl()}`
          : 'Local Agent 当前未运行'
      );
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('magi.interruptTask', () => {
      globalEventBus.emitEvent('task:cancelled', {});
      vscode.window.showInformationMessage(t('extension.cancelingTask'));
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

    if (localAgentManager) {
      await localAgentManager.dispose();
      localAgentManager = undefined;
      logger.info('扩展.停用.Agent.租约已释放', undefined, LogCategory.SYSTEM);
    }

    logger.info('扩展.停用.完成', undefined, LogCategory.SYSTEM);
  } catch (error) {
    logger.error('扩展.停用.失败', error, LogCategory.SYSTEM);
  }
}
