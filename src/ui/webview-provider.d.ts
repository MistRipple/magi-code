/**
 * WebviewProvider - Webview 面板提供者
 * 负责：对话面板、任务视图、变更视图、CLI 输出
 */
import * as vscode from 'vscode';
import { SessionManager } from '../session-manager';
import { TaskManager } from '../task-manager';
import { SnapshotManager } from '../snapshot-manager';
import { DiffGenerator } from '../diff-generator';
export declare class WebviewProvider implements vscode.WebviewViewProvider {
    private readonly extensionUri;
    private readonly context;
    private readonly workspaceRoot;
    static readonly viewType = "cliArranger.mainView";
    private _view?;
    private sessionManager;
    private chatSessionManager;
    private taskManager;
    private snapshotManager;
    private diffGenerator;
    private cliStatuses;
    private cliOutputs;
    private cliFactory;
    private taskAnalyzer;
    private cliSelector;
    private intelligentOrchestrator;
    private pendingConfirmation;
    private pendingPlanData;
    private selectedCli;
    private activeSessionId;
    constructor(extensionUri: vscode.Uri, context: vscode.ExtensionContext, workspaceRoot: string);
    /** 设置所有 CLI 适配器事件监听 */
    private setupCLIAdapters;
    /** 设置智能编排器的 Hard Stop 确认回调 */
    private setupOrchestratorConfirmation;
    /** 处理用户对执行计划的确认响应 */
    private handlePlanConfirmation;
    /** 绑定全局事件 */
    private bindEvents;
    /** 🆕 打断当前任务 - 增强版：添加等待和超时机制 */
    private interruptCurrentTask;
    /** 实现 WebviewViewProvider 接口 */
    resolveWebviewView(webviewView: vscode.WebviewView, _context: vscode.WebviewViewResolveContext, _token: vscode.CancellationToken): void;
    /** 检测所有 CLI 的可用性并更新状态 */
    private checkCliAvailability;
    /** 处理 Webview 消息 */
    private handleMessage;
    /** 处理设置交互模式 */
    private handleSetInteractionMode;
    /** 获取模式显示名称 */
    private getModeDisplayName;
    /** 恢复确认回调的 Promise resolver */
    private recoveryConfirmationResolver;
    /** 处理恢复确认 */
    private handleRecoveryConfirmation;
    /** 🆕 处理补充内容消息 */
    private handleAppendMessage;
    /** 处理设置更新 */
    private handleSettingUpdate;
    /** 执行任务 */
    private executeTask;
    /** 智能编排模式执行 */
    private executeWithIntelligentOrchestrator;
    /** 直接 CLI 执行模式 */
    private executeWithDirectCli;
    /** 发送状态更新到 Webview */
    private sendStateUpdate;
    /** 切换到指定会话 */
    private switchToSession;
    /** 保存当前会话的 CLI sessionIds */
    private saveCurrentSessionCliIds;
    /** 保存消息到当前会话 */
    private saveMessageToSession;
    /** 保存当前会话的完整数据（从前端同步） */
    private saveCurrentSessionData;
    /** 构建 UI 状态 */
    private buildUIState;
    /** 发送消息到 Webview */
    private postMessage;
    /** 获取 HTML 内容 - 从外部模板文件加载 */
    private getHtmlContent;
    /** 获取管理器实例 */
    getSessionManager(): SessionManager;
    getTaskManager(): TaskManager;
    getSnapshotManager(): SnapshotManager;
    getDiffGenerator(): DiffGenerator;
    /** 🆕 清理所有资源 - VSCode 关闭时调用 */
    dispose(): Promise<void>;
}
//# sourceMappingURL=webview-provider.d.ts.map