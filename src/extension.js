"use strict";
/**
 * CLI Arranger VSCode 扩展入口
 */
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.deactivate = deactivate;
const vscode = __importStar(require("vscode"));
const webview_provider_1 = require("./ui/webview-provider");
const cli_detector_1 = require("./cli-detector");
const events_1 = require("./events");
let webviewProvider;
let statusBarItem;
/**
 * 扩展激活
 */
async function activate(context) {
    console.log('CLI Arranger 扩展已激活');
    // 获取工作区根目录
    const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!workspaceRoot) {
        vscode.window.showWarningMessage('CLI Arranger: 请先打开一个工作区');
        return;
    }
    // 检测 CLI 可用性
    const cliStatus = await detectAndNotifyCLIs();
    // 如果 Claude CLI 不可用，显示警告但仍然启动插件
    if (!cliStatus.claudeAvailable) {
        // 不阻止插件启动，但会在 UI 中显示状态
    }
    // 创建 Webview Provider
    webviewProvider = new webview_provider_1.WebviewProvider(context.extensionUri, context, workspaceRoot);
    // 注册 Webview Provider
    context.subscriptions.push(vscode.window.registerWebviewViewProvider(webview_provider_1.WebviewProvider.viewType, webviewProvider));
    // 创建状态栏项
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    updateStatusBar('idle');
    statusBarItem.command = 'cliArranger.showPanel';
    statusBarItem.show();
    context.subscriptions.push(statusBarItem);
    // 监听任务状态变化，更新状态栏
    events_1.globalEventBus.on('task:started', () => {
        updateStatusBar('running');
        vscode.commands.executeCommand('setContext', 'cliArrangerTaskRunning', true);
    });
    events_1.globalEventBus.on('task:completed', () => {
        updateStatusBar('completed');
        vscode.commands.executeCommand('setContext', 'cliArrangerTaskRunning', false);
    });
    events_1.globalEventBus.on('task:failed', () => {
        updateStatusBar('failed');
        vscode.commands.executeCommand('setContext', 'cliArrangerTaskRunning', false);
    });
    events_1.globalEventBus.on('task:interrupted', () => {
        updateStatusBar('interrupted');
        vscode.commands.executeCommand('setContext', 'cliArrangerTaskRunning', false);
    });
    // 注册命令
    registerCommands(context);
    // 启动健康检查
    cli_detector_1.cliDetector.startHealthCheck();
    context.subscriptions.push({
        dispose: () => cli_detector_1.cliDetector.stopHealthCheck()
    });
    console.log('CLI Arranger 初始化完成');
}
/**
 * 更新状态栏显示
 */
function updateStatusBar(status) {
    if (!statusBarItem)
        return;
    switch (status) {
        case 'idle':
            statusBarItem.text = '$(robot) CLI Arranger';
            statusBarItem.tooltip = '点击打开 CLI Arranger';
            statusBarItem.backgroundColor = undefined;
            break;
        case 'running':
            statusBarItem.text = '$(sync~spin) CLI Arranger';
            statusBarItem.tooltip = '任务执行中... 按 Escape 打断';
            statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
            break;
        case 'completed':
            statusBarItem.text = '$(check) CLI Arranger';
            statusBarItem.tooltip = '任务已完成';
            statusBarItem.backgroundColor = undefined;
            // 3秒后恢复默认状态
            setTimeout(() => updateStatusBar('idle'), 3000);
            break;
        case 'failed':
            statusBarItem.text = '$(error) CLI Arranger';
            statusBarItem.tooltip = '任务执行失败';
            statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.errorBackground');
            // 5秒后恢复默认状态
            setTimeout(() => updateStatusBar('idle'), 5000);
            break;
        case 'interrupted':
            statusBarItem.text = '$(debug-pause) CLI Arranger';
            statusBarItem.tooltip = '任务已打断';
            statusBarItem.backgroundColor = undefined;
            // 3秒后恢复默认状态
            setTimeout(() => updateStatusBar('idle'), 3000);
            break;
    }
}
/**
 * 注册所有命令
 */
function registerCommands(context) {
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.openPanel', () => {
        vscode.commands.executeCommand('workbench.view.extension.cliArranger');
    }));
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.showPanel', () => {
        vscode.commands.executeCommand('workbench.view.extension.cliArranger');
    }));
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.newSession', () => {
        webviewProvider?.getSessionManager().createSession();
        vscode.window.showInformationMessage('CLI Arranger: 新会话已创建');
    }));
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.showStatus', async () => {
        const summary = await cli_detector_1.cliDetector.getStatusSummary();
        vscode.window.showInformationMessage(`CLI Arranger: ${summary.available}/${summary.total} CLI 可用\n${summary.recommendation}`);
    }));
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.checkCLIs', async () => {
        await detectAndNotifyCLIs();
    }));
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.interruptTask', () => {
        events_1.globalEventBus.emitEvent('task:interrupt', {});
        vscode.window.showInformationMessage('CLI Arranger: 正在打断任务...');
    }));
    context.subscriptions.push(vscode.commands.registerCommand('cliArranger.stopTask', () => {
        events_1.globalEventBus.emitEvent('task:interrupt', {});
    }));
}
/**
 * 检测 CLI 并显示通知
 */
async function detectAndNotifyCLIs() {
    const statuses = await cli_detector_1.cliDetector.checkAllCLIs(true);
    const claudeStatus = statuses.find(s => s.type === 'claude');
    const codexStatus = statuses.find(s => s.type === 'codex');
    const geminiStatus = statuses.find(s => s.type === 'gemini');
    const claudeAvailable = claudeStatus?.available ?? false;
    const codexAvailable = codexStatus?.available ?? false;
    const geminiAvailable = geminiStatus?.available ?? false;
    // Claude CLI 是必需的
    if (!claudeAvailable) {
        const action = await vscode.window.showErrorMessage(`CLI Arranger: Claude CLI 未安装或不可用。${claudeStatus?.error || ''}`, '安装指南', '重新检测');
        if (action === '安装指南') {
            vscode.env.openExternal(vscode.Uri.parse('https://docs.anthropic.com/claude-code/getting-started'));
        }
        else if (action === '重新检测') {
            return detectAndNotifyCLIs();
        }
    }
    else {
        // Claude 可用，显示版本信息
        console.log(`Claude CLI 已就绪: v${claudeStatus?.version}`);
    }
    // Codex 和 Gemini 是可选的
    if (!codexAvailable && claudeAvailable) {
        vscode.window.showInformationMessage(`CLI Arranger: Codex CLI 不可用，Bug修复任务将由 Claude 处理。`, '了解更多').then(action => {
            if (action === '了解更多') {
                vscode.env.openExternal(vscode.Uri.parse('https://github.com/openai/codex'));
            }
        });
    }
    if (!geminiAvailable && claudeAvailable) {
        vscode.window.showInformationMessage(`CLI Arranger: Gemini CLI 不可用，前端任务将由 Claude 处理。`, '了解更多').then(action => {
            if (action === '了解更多') {
                vscode.env.openExternal(vscode.Uri.parse('https://github.com/google/gemini-cli'));
            }
        });
    }
    return { claudeAvailable, codexAvailable, geminiAvailable };
}
/**
 * 扩展停用 - 🆕 增强版：确保所有资源被正确清理
 */
async function deactivate() {
    console.log('CLI Arranger 扩展正在停用...');
    try {
        // 1. 停止健康检查
        cli_detector_1.cliDetector.stopHealthCheck();
        console.log('[deactivate] 健康检查已停止');
        // 2. 清理 WebviewProvider（包括 CLI 进程、编排器、事件监听器）
        if (webviewProvider) {
            await webviewProvider.dispose();
            webviewProvider = undefined;
            console.log('[deactivate] WebviewProvider 已清理');
        }
        // 3. 清理状态栏
        if (statusBarItem) {
            statusBarItem.dispose();
            statusBarItem = undefined;
            console.log('[deactivate] 状态栏已清理');
        }
        console.log('CLI Arranger 扩展已完全停用');
    }
    catch (error) {
        console.error('[deactivate] 清理资源时出错:', error);
    }
}
//# sourceMappingURL=extension.js.map