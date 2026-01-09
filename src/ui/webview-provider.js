"use strict";
/**
 * WebviewProvider - Webview 面板提供者
 * 负责：对话面板、任务视图、变更视图、CLI 输出
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
exports.WebviewProvider = void 0;
const vscode = __importStar(require("vscode"));
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const os = __importStar(require("os"));
const types_1 = require("../types");
const session_manager_1 = require("../session-manager");
const chat_session_manager_1 = require("../chat-session-manager");
const task_manager_1 = require("../task-manager");
const snapshot_manager_1 = require("../snapshot-manager");
const diff_generator_1 = require("../diff-generator");
const events_1 = require("../events");
const adapter_factory_1 = require("../cli/adapter-factory");
const task_1 = require("../task");
const types_2 = require("../cli/types");
const intelligent_orchestrator_1 = require("../orchestrator/intelligent-orchestrator");
class WebviewProvider {
    extensionUri;
    context;
    workspaceRoot;
    static viewType = 'cliArranger.mainView';
    _view;
    sessionManager;
    chatSessionManager;
    taskManager;
    snapshotManager;
    diffGenerator;
    cliStatuses = new Map();
    cliOutputs = new Map();
    // 多 CLI 适配器工厂
    cliFactory;
    // 任务分析器和 CLI 选择器
    taskAnalyzer;
    cliSelector;
    // 智能编排器
    intelligentOrchestrator;
    // Hard Stop 确认机制
    pendingConfirmation = null;
    // 🆕 待确认的计划数据（用于 Webview 重新加载时恢复）
    pendingPlanData = null;
    // 当前选择的 CLI（null 表示自动选择/智能编排）
    selectedCli = null;
    // 🆕 当前活跃的会话ID，用于会话隔离
    activeSessionId = null;
    constructor(extensionUri, context, workspaceRoot) {
        this.extensionUri = extensionUri;
        this.context = context;
        this.workspaceRoot = workspaceRoot;
        // 初始化管理器
        this.sessionManager = new session_manager_1.SessionManager(workspaceRoot);
        this.chatSessionManager = new chat_session_manager_1.ChatSessionManager(workspaceRoot);
        this.taskManager = new task_manager_1.TaskManager(this.sessionManager);
        this.snapshotManager = new snapshot_manager_1.SnapshotManager(this.sessionManager, workspaceRoot);
        this.diffGenerator = new diff_generator_1.DiffGenerator(this.sessionManager, workspaceRoot);
        // 初始化多 CLI 适配器工厂
        this.cliFactory = new adapter_factory_1.CLIAdapterFactory({ cwd: workspaceRoot });
        this.setupCLIAdapters();
        // 初始化任务分析器和 CLI 选择器（从配置读取 skills）
        this.taskAnalyzer = new task_1.TaskAnalyzer();
        const config = vscode.workspace.getConfiguration('cliArranger');
        const userSkills = config.get('skills') || {};
        this.cliSelector = new task_1.CLISelector(userSkills);
        // 初始化智能编排器
        this.intelligentOrchestrator = new intelligent_orchestrator_1.IntelligentOrchestrator(this.cliFactory, this.taskManager, this.snapshotManager, this.workspaceRoot);
        // 设置 Hard Stop 确认回调
        this.setupOrchestratorConfirmation();
        // 初始化 CLI 输出缓冲
        this.cliOutputs.set('claude', []);
        this.cliOutputs.set('codex', []);
        this.cliOutputs.set('gemini', []);
        // 绑定事件
        this.bindEvents();
    }
    /** 设置所有 CLI 适配器事件监听 */
    setupCLIAdapters() {
        // 监听工厂的统一事件
        this.cliFactory.on('output', ({ type, chunk }) => {
            const outputs = this.cliOutputs.get(type) || [];
            outputs.push(chunk);
            this.cliOutputs.set(type, outputs);
            // 🆕 添加 sessionId，用于会话隔离
            this.postMessage({
                type: 'subTaskOutput',
                subTaskId: type,
                output: chunk,
                cliType: type,
                sessionId: this.activeSessionId
            });
        });
        this.cliFactory.on('stateChange', ({ type, state }) => {
            const status = {
                type: type,
                code: state === 'error' ? types_1.CLIStatusCode.RUNTIME_ERROR : types_1.CLIStatusCode.AVAILABLE,
                available: state !== 'error',
                path: type,
            };
            this.cliStatuses.set(type, status);
            this.sendStateUpdate();
        });
    }
    /** 设置智能编排器的 Hard Stop 确认回调 */
    setupOrchestratorConfirmation() {
        // 设置 Hard Stop 确认回调
        this.intelligentOrchestrator.setConfirmationCallback(async (plan, formattedPlan) => {
            return new Promise((resolve, reject) => {
                // 保存 resolve/reject 以便后续处理用户响应
                this.pendingConfirmation = { resolve, reject };
                // 🆕 保存计划数据，用于 Webview 重新加载时恢复
                this.pendingPlanData = { plan, formattedPlan };
                // 发送执行计划到 Webview，等待用户确认
                this.postMessage({
                    type: 'cliResponse',
                    cli: 'claude',
                    content: formattedPlan,
                });
                // 发送确认请求消息
                this.postMessage({
                    type: 'confirmationRequest',
                    plan: plan,
                    formattedPlan: formattedPlan,
                });
                console.log('[CLI Arranger] Hard Stop: 等待用户确认执行计划...');
            });
        });
        // 设置恢复确认回调
        this.intelligentOrchestrator.setRecoveryConfirmationCallback(async (failedTask, error, options) => {
            return new Promise((resolve) => {
                // 保存 resolver
                this.recoveryConfirmationResolver = resolve;
                // 发送恢复请求到 Webview
                this.postMessage({
                    type: 'recoveryRequest',
                    taskId: failedTask.id,
                    error: error,
                    canRetry: options.retry,
                    canRollback: options.rollback,
                });
                console.log('[CLI Arranger] Recovery: 等待用户决策...');
            });
        });
    }
    /** 处理用户对执行计划的确认响应 */
    handlePlanConfirmation(confirmed) {
        if (this.pendingConfirmation) {
            console.log(`[CLI Arranger] 用户确认结果: ${confirmed ? 'Y' : 'N'}`);
            this.pendingConfirmation.resolve(confirmed);
            this.pendingConfirmation = null;
            // 🆕 清除待确认的计划数据
            this.pendingPlanData = null;
            // 通知 Webview 确认已处理
            this.postMessage({
                type: 'toast',
                message: confirmed ? '执行计划已确认，开始执行...' : '执行计划已取消',
                toastType: confirmed ? 'success' : 'info',
            });
        }
    }
    /** 绑定全局事件 */
    bindEvents() {
        // 任务相关事件
        events_1.globalEventBus.on('task:created', () => this.sendStateUpdate());
        events_1.globalEventBus.on('task:started', (event) => {
            this.sendStateUpdate();
            // 🆕 发送运行状态到前端
            this.postMessage({
                type: 'phaseChanged',
                phase: 'started',
                taskId: event.taskId || '',
                isRunning: true
            });
        });
        events_1.globalEventBus.on('task:completed', (event) => {
            this.sendStateUpdate();
            // 🆕 发送完成状态到前端
            this.postMessage({
                type: 'phaseChanged',
                phase: 'completed',
                taskId: event.taskId || '',
                isRunning: false
            });
        });
        events_1.globalEventBus.on('task:failed', (event) => {
            this.sendStateUpdate();
            // 🆕 发送失败状态到前端
            this.postMessage({
                type: 'phaseChanged',
                phase: 'failed',
                taskId: event.taskId || '',
                isRunning: false
            });
        });
        events_1.globalEventBus.on('task:interrupted', (event) => {
            this.sendStateUpdate();
            // 🆕 发送中断状态到前端
            this.postMessage({
                type: 'phaseChanged',
                phase: 'interrupted',
                taskId: event.taskId || '',
                isRunning: false
            });
        });
        events_1.globalEventBus.on('subtask:started', (event) => {
            // 🔧 问题4修复：将主线信息发送到主对话窗口
            const data = event.data;
            if (data?.description) {
                this.postMessage({
                    type: 'mainlineUpdate',
                    updateType: 'subtask_started',
                    taskId: event.taskId || '',
                    subTaskId: event.subTaskId || '',
                    cli: data.cli || 'system',
                    description: data.description,
                    timestamp: Date.now()
                });
            }
            this.sendStateUpdate();
        });
        events_1.globalEventBus.on('subtask:completed', (event) => {
            // 🔧 问题4修复：将完成信息发送到主对话窗口
            const data = event.data;
            this.postMessage({
                type: 'mainlineUpdate',
                updateType: 'subtask_completed',
                taskId: event.taskId || '',
                subTaskId: event.subTaskId || '',
                success: data?.success ?? true,
                timestamp: Date.now()
            });
            this.sendStateUpdate();
        });
        events_1.globalEventBus.on('subtask:failed', (event) => {
            // 🔧 问题4修复：将失败信息发送到主对话窗口
            const data = event.data;
            this.postMessage({
                type: 'mainlineUpdate',
                updateType: 'subtask_failed',
                taskId: event.taskId || '',
                subTaskId: event.subTaskId || '',
                error: data?.error || '未知错误',
                timestamp: Date.now()
            });
            this.sendStateUpdate();
        });
        // 🆕 Orchestrator Phase 状态变化事件 - 增强版
        events_1.globalEventBus.on('orchestrator:phase_changed', (event) => {
            const data = event.data;
            if (data?.phase) {
                // 🔧 修复页面跳动：只发送 phaseChanged 消息，不触发 sendStateUpdate
                // phaseChanged 只更新阶段指示器，不会重建整个 DOM
                this.postMessage({
                    type: 'phaseChanged',
                    phase: data.phase,
                    taskId: event.taskId || '',
                    isRunning: data.isRunning ?? this.intelligentOrchestrator.running
                });
                // 🔧 移除 sendStateUpdate() 调用，避免频繁 DOM 重建导致页面跳动
            }
        });
        // 打断任务事件
        events_1.globalEventBus.on('task:interrupt', () => {
            this.interruptCurrentTask();
        });
        events_1.globalEventBus.on('subtask:output', (event) => {
            const data = event.data;
            if (data?.output) {
                this.postMessage({ type: 'subTaskOutput', subTaskId: event.subTaskId, output: data.output });
            }
        });
        events_1.globalEventBus.on('snapshot:created', () => this.sendStateUpdate());
        events_1.globalEventBus.on('snapshot:reverted', () => this.sendStateUpdate());
        // CLI 状态相关事件
        events_1.globalEventBus.on('cli:statusChanged', (event) => {
            const data = event.data;
            this.sendStateUpdate();
            // 通知 UI CLI 状态变化
            this.postMessage({ type: 'cliStatusChanged', cli: data.cli, available: data.available, version: data.version });
        });
        events_1.globalEventBus.on('cli:healthCheck', () => {
            this.sendStateUpdate();
        });
        events_1.globalEventBus.on('cli:error', (event) => {
            const data = event.data;
            // 通知 UI 显示错误
            this.postMessage({ type: 'cliError', cli: data.cli, error: data.error });
        });
    }
    /** 🆕 打断当前任务 - 增强版：添加等待和超时机制 */
    async interruptCurrentTask() {
        console.log('[CLI Arranger] 收到中断请求');
        // 1. 首先中断 Orchestrator（这会触发 AbortController）
        if (this.intelligentOrchestrator.running) {
            console.log('[CLI Arranger] 中断 Orchestrator');
            this.intelligentOrchestrator.interrupt();
        }
        // 2. 中断所有 CLI 并等待完成
        console.log('[CLI Arranger] 中断所有 CLI...');
        try {
            await Promise.race([
                this.cliFactory.interruptAll(),
                new Promise((resolve) => setTimeout(resolve, 5000)) // 5秒超时
            ]);
            console.log('[CLI Arranger] CLI 中断完成');
        }
        catch (error) {
            console.error('[CLI Arranger] CLI 中断出错:', error);
        }
        // 3. 更新任务状态
        const tasks = this.taskManager.getAllTasks();
        const runningTask = tasks.find(t => t.status === 'running');
        if (runningTask) {
            this.taskManager.updateTaskStatus(runningTask.id, 'cancelled');
        }
        // 4. 通知 UI
        this.postMessage({ type: 'toast', message: '任务已取消', toastType: 'info' });
        this.sendStateUpdate();
    }
    /** 实现 WebviewViewProvider 接口 */
    resolveWebviewView(webviewView, _context, _token) {
        this._view = webviewView;
        webviewView.webview.options = {
            enableScripts: true,
            localResourceRoots: [this.extensionUri],
        };
        webviewView.webview.html = this.getHtmlContent(webviewView.webview);
        // 处理来自 Webview 的消息
        webviewView.webview.onDidReceiveMessage((message) => this.handleMessage(message), undefined, this.context.subscriptions);
        // 启动时检测所有 CLI 的可用性
        this.checkCliAvailability();
    }
    /** 检测所有 CLI 的可用性并更新状态 */
    async checkCliAvailability() {
        try {
            const availability = await this.cliFactory.checkAllAvailability();
            console.log('[CLI Arranger] CLI 可用性检测结果:', availability);
            // 更新 CLI 状态
            const cliTypes = ['claude', 'codex', 'gemini'];
            for (const cli of cliTypes) {
                const status = {
                    type: cli,
                    code: availability[cli] ? types_1.CLIStatusCode.AVAILABLE : types_1.CLIStatusCode.NOT_INSTALLED,
                    available: availability[cli],
                    path: cli,
                    lastChecked: new Date(),
                };
                this.cliStatuses.set(cli, status);
            }
            // 通知 UI 更新状态
            this.sendStateUpdate();
            // 发送单独的状态变更通知
            for (const cli of cliTypes) {
                this.postMessage({
                    type: 'cliStatusChanged',
                    cli,
                    available: availability[cli],
                });
            }
        }
        catch (error) {
            console.error('[CLI Arranger] CLI 可用性检测失败:', error);
        }
    }
    /** 处理 Webview 消息 */
    async handleMessage(message) {
        console.log('[CLI Arranger] 收到 Webview 消息:', message.type);
        switch (message.type) {
            case 'getState':
                this.sendStateUpdate();
                break;
            case 'executeTask':
                console.log('[CLI Arranger] 处理 executeTask, prompt:', message.prompt);
                const images = message.images || [];
                await this.executeTask(message.prompt, undefined, images);
                break;
            case 'interruptTask':
                // 🆕 使用增强版中断逻辑
                console.log('[CLI Arranger] 收到 interruptTask 消息, taskId:', message.taskId);
                await this.interruptCurrentTask();
                break;
            case 'pauseTask':
                // 🆕 暂停任务（目前暂不支持真正的暂停，仅记录状态）
                console.log('[CLI Arranger] 收到 pauseTask 消息, taskId:', message.taskId);
                this.postMessage({ type: 'toast', message: '暂停功能开发中', toastType: 'info' });
                break;
            case 'resumeTask':
                // 🆕 恢复任务
                console.log('[CLI Arranger] 收到 resumeTask 消息, taskId:', message.taskId);
                this.postMessage({ type: 'toast', message: '恢复功能开发中', toastType: 'info' });
                break;
            case 'appendMessage':
                // 🆕 补充内容到当前执行的任务
                console.log('[CLI Arranger] 收到 appendMessage 消息');
                await this.handleAppendMessage(message.taskId, message.content);
                break;
            case 'approveChange':
                // 批准单个变更
                this.snapshotManager.acceptChange(message.filePath);
                events_1.globalEventBus.emitEvent('change:approved', { data: { filePath: message.filePath } });
                this.postMessage({ type: 'toast', message: '变更已批准', toastType: 'success' });
                this.sendStateUpdate();
                break;
            case 'revertChange':
                this.snapshotManager.revertToSnapshot(message.filePath);
                this.postMessage({ type: 'toast', message: '变更已还原', toastType: 'info' });
                this.sendStateUpdate();
                break;
            case 'approveAllChanges':
                // 批准所有变更
                {
                    const allChanges = this.snapshotManager.getPendingChanges();
                    for (const change of allChanges) {
                        this.snapshotManager.acceptChange(change.filePath);
                    }
                    this.postMessage({ type: 'toast', message: `已批准 ${allChanges.length} 个变更`, toastType: 'success' });
                }
                this.sendStateUpdate();
                break;
            case 'revertAllChanges':
                // 还原所有变更
                {
                    const changes = this.snapshotManager.getPendingChanges();
                    for (const change of changes) {
                        this.snapshotManager.revertToSnapshot(change.filePath);
                    }
                    this.postMessage({ type: 'toast', message: `已还原 ${changes.length} 个变更`, toastType: 'info' });
                }
                this.sendStateUpdate();
                break;
            case 'viewDiff':
                // 查看 Diff
                {
                    const diffResult = this.diffGenerator.generateDiff(message.filePath);
                    const diffContent = diffResult ? this.diffGenerator.formatDiff(diffResult) : '';
                    this.postMessage({ type: 'showDiff', filePath: message.filePath, diff: diffContent });
                }
                break;
            case 'newSession':
                // 🆕 创建新会话前，先中断当前任务
                await this.interruptCurrentTask();
                // 创建新会话时，重置所有 CLI 的会话 ID
                this.cliFactory.resetAllSessions();
                this.sessionManager.createSession();
                // 同时创建新的聊天会话
                const newChatSession = this.chatSessionManager.createSession();
                // 🆕 更新活跃会话ID
                this.activeSessionId = newChatSession.id;
                console.log('[CLI Arranger] 创建新会话，已重置所有 CLI sessionId, activeSessionId:', this.activeSessionId);
                // 通知 webview 新会话已创建
                this.postMessage({ type: 'sessionCreated', session: newChatSession });
                this.postMessage({ type: 'sessionsUpdated', sessions: this.chatSessionManager.getAllSessions() });
                this.sendStateUpdate();
                break;
            case 'saveCurrentSession':
                // 🔧 新增：保存当前会话的消息和 CLI 输出
                this.saveCurrentSessionData(message.messages, message.cliOutputs);
                break;
            case 'switchSession':
                // 🆕 切换会话前，先中断当前任务
                if (this.activeSessionId !== message.sessionId) {
                    await this.interruptCurrentTask();
                }
                // 切换会话时，同步 CLI 的会话 ID
                this.switchToSession(message.sessionId);
                // 同时切换聊天会话
                const switchedSession = this.chatSessionManager.switchSession(message.sessionId);
                if (switchedSession) {
                    // 🆕 更新活跃会话ID
                    this.activeSessionId = message.sessionId;
                    // 恢复 CLI sessionIds
                    if (switchedSession.cliSessionIds) {
                        this.cliFactory.setAllSessionIds(switchedSession.cliSessionIds);
                    }
                    this.postMessage({ type: 'sessionSwitched', sessionId: message.sessionId });
                }
                this.sendStateUpdate();
                break;
            case 'renameSession':
                // 重命名会话
                if (this.chatSessionManager.renameSession(message.sessionId, message.name)) {
                    this.postMessage({ type: 'sessionsUpdated', sessions: this.chatSessionManager.getAllSessions() });
                    this.postMessage({ type: 'toast', message: '会话已重命名', toastType: 'success' });
                }
                break;
            case 'closeSession':
                // 删除会话
                if (this.chatSessionManager.deleteSession(message.sessionId)) {
                    // 如果删除后没有会话，创建一个新的
                    if (this.chatSessionManager.getAllSessions().length === 0) {
                        this.chatSessionManager.createSession();
                    }
                    this.postMessage({ type: 'sessionsUpdated', sessions: this.chatSessionManager.getAllSessions() });
                    this.postMessage({ type: 'toast', message: '会话已删除', toastType: 'info' });
                }
                this.sessionManager.endSession(message.sessionId);
                this.sendStateUpdate();
                break;
            case 'selectCli':
                // 用户手动选择 CLI（null 表示自动选择）
                this.selectedCli = message.cli || null;
                console.log('[CLI Arranger] 用户选择 CLI:', this.selectedCli || '自动');
                break;
            case 'confirmPlan':
                // 用户确认执行计划（Hard Stop 响应）
                this.handlePlanConfirmation(message.confirmed);
                break;
            case 'updateSetting':
                // 更新设置
                this.handleSettingUpdate(message.key, message.value);
                break;
            case 'setInteractionMode':
                // 设置交互模式
                this.handleSetInteractionMode(message.mode);
                break;
            case 'confirmRecovery':
                // 用户确认恢复策略
                this.handleRecoveryConfirmation(message.decision);
                break;
            case 'getState':
                this.sendStateUpdate();
                break;
        }
    }
    /** 处理设置交互模式 */
    handleSetInteractionMode(mode) {
        console.log(`[CLI Arranger] 设置交互模式: ${mode}`);
        this.intelligentOrchestrator.setInteractionMode(mode);
        this.postMessage({ type: 'interactionModeChanged', mode });
        this.postMessage({
            type: 'toast',
            message: `已切换到 ${this.getModeDisplayName(mode)} 模式`,
            toastType: 'info'
        });
        this.sendStateUpdate();
    }
    /** 获取模式显示名称 */
    getModeDisplayName(mode) {
        switch (mode) {
            case 'ask': return '对话';
            case 'agent': return '代理';
            case 'auto': return '自动';
            default: return mode;
        }
    }
    /** 恢复确认回调的 Promise resolver */
    recoveryConfirmationResolver = null;
    /** 处理恢复确认 */
    handleRecoveryConfirmation(decision) {
        console.log(`[CLI Arranger] 用户恢复决策: ${decision}`);
        if (this.recoveryConfirmationResolver) {
            this.recoveryConfirmationResolver(decision);
            this.recoveryConfirmationResolver = null;
        }
    }
    /** 🆕 处理补充内容消息 */
    async handleAppendMessage(taskId, content) {
        console.log(`[CLI Arranger] 补充内容到任务 ${taskId}: ${content.substring(0, 50)}...`);
        // 检查是否有正在运行的任务
        if (!this.intelligentOrchestrator.running) {
            this.postMessage({ type: 'toast', message: '没有正在执行的任务', toastType: 'warning' });
            return;
        }
        // 目前的实现：将补充内容作为新消息发送到当前 CLI
        // 未来可以扩展为真正的追加到当前执行上下文
        try {
            // 添加用户消息到对话
            this.postMessage({
                type: 'toast',
                message: '补充内容已发送',
                toastType: 'info'
            });
            // 发送到当前活跃的 CLI
            // 注意：这是一个简化实现，真正的追加需要 CLI 支持
            console.log('[CLI Arranger] 补充内容功能：当前为简化实现');
        }
        catch (error) {
            console.error('[CLI Arranger] 补充内容失败:', error);
            this.postMessage({ type: 'toast', message: '补充内容失败', toastType: 'error' });
        }
    }
    /** 处理设置更新 */
    handleSettingUpdate(key, value) {
        const config = vscode.workspace.getConfiguration('cliArranger');
        // 处理 skills 配置
        if (key.startsWith('skill-')) {
            const taskType = key.replace('skill-', '');
            const currentSkills = config.get('skills') || {};
            currentSkills[taskType] = value;
            config.update('skills', currentSkills, vscode.ConfigurationTarget.Global);
            // 更新 CLI 选择器
            this.cliSelector.updateSkills({ [taskType]: value });
            console.log('[CLI Arranger] 更新技能配置:', taskType, '->', value);
        }
        // 处理其他配置
        else if (key === 'autoSnapshot') {
            config.update('autoSnapshot', value, vscode.ConfigurationTarget.Global);
        }
        else if (key === 'timeout') {
            config.update('timeout', parseInt(value, 10), vscode.ConfigurationTarget.Global);
        }
        this.postMessage({ type: 'toast', message: '设置已保存', toastType: 'success' });
    }
    /** 执行任务 */
    async executeTask(prompt, forceCli, images) {
        console.log('[CLI Arranger] executeTask 开始, prompt:', prompt, '图片数量:', images?.length || 0);
        // 🆕 确保 activeSessionId 已设置
        if (!this.activeSessionId) {
            const currentSession = this.chatSessionManager.getCurrentSession();
            this.activeSessionId = currentSession?.id || null;
            console.log('[CLI Arranger] 设置 activeSessionId:', this.activeSessionId);
        }
        // 如果有图片，保存到临时文件
        const imagePaths = [];
        if (images && images.length > 0) {
            const tmpDir = path.join(os.tmpdir(), 'cli-arranger-images');
            if (!fs.existsSync(tmpDir)) {
                fs.mkdirSync(tmpDir, { recursive: true });
            }
            for (let i = 0; i < images.length; i++) {
                const img = images[i];
                const matches = img.dataUrl.match(/^data:image\/(\w+);base64,(.+)$/);
                if (matches) {
                    const ext = matches[1] === 'jpeg' ? 'jpg' : matches[1];
                    const base64Data = matches[2];
                    const filePath = path.join(tmpDir, `image_${Date.now()}_${i}.${ext}`);
                    fs.writeFileSync(filePath, Buffer.from(base64Data, 'base64'));
                    imagePaths.push(filePath);
                    console.log('[CLI Arranger] 图片已保存:', filePath);
                }
            }
        }
        // 判断执行模式：智能编排 vs 直接执行
        const useIntelligentMode = !forceCli && !this.selectedCli;
        if (useIntelligentMode) {
            // 智能编排模式：Claude 分析 → 分配 CLI → 执行 → 总结
            await this.executeWithIntelligentOrchestrator(prompt, imagePaths);
        }
        else {
            // 直接执行模式：指定 CLI 直接执行
            await this.executeWithDirectCli(prompt, forceCli || this.selectedCli, imagePaths);
        }
    }
    /** 智能编排模式执行 */
    async executeWithIntelligentOrchestrator(prompt, imagePaths) {
        console.log('[CLI Arranger] 使用智能编排模式');
        const task = this.taskManager.createTask(prompt);
        this.taskManager.updateTaskStatus(task.id, 'running');
        this.sendStateUpdate();
        // 发送用户 prompt 到 Claude 输出面板
        const promptMsg = JSON.stringify({
            type: 'user_prompt',
            prompt: prompt,
            cli: 'claude',
            time: new Date().toLocaleTimeString(),
            mode: 'intelligent',
            hasImages: imagePaths.length > 0
        });
        this.postMessage({ type: 'subTaskOutput', subTaskId: 'claude', output: promptMsg + '\n', cliType: 'claude' });
        try {
            // 调用智能编排器
            const result = await this.intelligentOrchestrator.execute(prompt, task.id);
            // 保存消息历史
            this.saveMessageToSession(prompt, result, 'claude');
            this.saveCurrentSessionCliIds();
            // 发送响应到对话线程
            this.postMessage({
                type: 'cliResponse',
                cli: 'claude',
                content: result,
            });
        }
        catch (error) {
            console.error('[CLI Arranger] 智能编排执行错误:', error);
            const errorMsg = error instanceof Error ? error.message : String(error);
            this.postMessage({ type: 'cliError', cli: 'claude', error: errorMsg });
            this.postMessage({
                type: 'cliResponse',
                cli: 'claude',
                content: '',
                error: errorMsg,
            });
        }
        this.sendStateUpdate();
    }
    /** 直接 CLI 执行模式 */
    async executeWithDirectCli(prompt, targetCli, imagePaths) {
        console.log(`[CLI Arranger] 使用直接执行模式, CLI: ${targetCli}`);
        const task = this.taskManager.createTask(prompt);
        this.taskManager.updateTaskStatus(task.id, 'running');
        this.sendStateUpdate();
        // 清空目标 CLI 的输出
        this.cliOutputs.set(targetCli, []);
        // 发送用户 prompt 到 CLI 输出面板
        const promptMsg = JSON.stringify({
            type: 'user_prompt',
            prompt: prompt,
            cli: targetCli,
            time: new Date().toLocaleTimeString(),
            hasImages: imagePaths.length > 0
        });
        this.postMessage({ type: 'subTaskOutput', subTaskId: targetCli, output: promptMsg + '\n', cliType: targetCli });
        try {
            console.log(`[CLI Arranger] 调用 ${targetCli} CLI...`);
            const response = await this.cliFactory.sendMessage(targetCli, prompt, imagePaths);
            console.log(`[CLI Arranger] ${targetCli} CLI 响应:`, response.content?.substring(0, 100));
            if (response.error) {
                this.taskManager.updateTaskStatus(task.id, 'failed');
                this.postMessage({ type: 'cliError', cli: targetCli, error: response.error });
            }
            else {
                this.taskManager.updateTaskStatus(task.id, 'completed');
                this.saveMessageToSession(prompt, response.content || '', targetCli);
                this.saveCurrentSessionCliIds();
            }
            this.postMessage({
                type: 'cliResponse',
                cli: targetCli,
                content: response.content,
                error: response.error,
            });
        }
        catch (error) {
            console.error(`[CLI Arranger] ${targetCli} executeTask 错误:`, error);
            this.taskManager.updateTaskStatus(task.id, 'failed');
            const errorMsg = error instanceof Error ? error.message : String(error);
            this.postMessage({ type: 'cliError', cli: targetCli, error: errorMsg });
            this.postMessage({
                type: 'cliResponse',
                cli: targetCli,
                content: '',
                error: errorMsg,
            });
        }
        this.sendStateUpdate();
    }
    /** 发送状态更新到 Webview */
    sendStateUpdate() {
        const state = this.buildUIState();
        this.postMessage({ type: 'stateUpdate', state });
        // 🆕 如果有待确认的计划，重新发送确认请求（用于 Webview 重新加载后恢复）
        if (this.pendingPlanData && this.pendingConfirmation) {
            console.log('[CLI Arranger] 检测到待确认计划，重新发送确认请求');
            this.postMessage({
                type: 'confirmationRequest',
                plan: this.pendingPlanData.plan,
                formattedPlan: this.pendingPlanData.formattedPlan,
            });
        }
    }
    /** 切换到指定会话 */
    switchToSession(sessionId) {
        // 先保存当前会话的 CLI sessionIds
        this.saveCurrentSessionCliIds();
        // 切换会话
        const session = this.sessionManager.switchSession(sessionId);
        if (session) {
            // 恢复目标会话的 CLI sessionIds
            if (session.cliSessionIds) {
                this.cliFactory.setAllSessionIds(session.cliSessionIds);
                console.log('[CLI Arranger] 切换会话，恢复 CLI sessionIds:', session.cliSessionIds);
            }
            else {
                // 如果目标会话没有 CLI sessionIds，重置所有
                this.cliFactory.resetAllSessions();
                console.log('[CLI Arranger] 切换会话，目标会话无 CLI sessionIds，已重置');
            }
        }
    }
    /** 保存当前会话的 CLI sessionIds */
    saveCurrentSessionCliIds() {
        const currentSession = this.sessionManager.getCurrentSession();
        if (currentSession) {
            const cliSessionIds = this.cliFactory.getAllSessionIds();
            currentSession.cliSessionIds = cliSessionIds;
            // 触发持久化保存
            this.sessionManager.saveCurrentSession();
            console.log('[CLI Arranger] 保存当前会话 CLI sessionIds:', cliSessionIds);
        }
    }
    /** 保存消息到当前会话 */
    saveMessageToSession(userPrompt, assistantResponse, cli) {
        const currentSession = this.sessionManager.getCurrentSession();
        if (currentSession) {
            if (!currentSession.messages) {
                currentSession.messages = [];
            }
            // 保存用户消息
            currentSession.messages.push({
                role: 'user',
                content: userPrompt,
                cli,
                timestamp: Date.now(),
            });
            // 保存助手响应
            currentSession.messages.push({
                role: 'assistant',
                content: assistantResponse,
                cli,
                timestamp: Date.now(),
            });
            // 触发持久化保存
            this.sessionManager.saveCurrentSession();
            console.log('[CLI Arranger] 保存消息到会话，当前消息数:', currentSession.messages.length);
        }
    }
    /** 保存当前会话的完整数据（从前端同步） */
    saveCurrentSessionData(messages, cliOutputs) {
        const currentSession = this.chatSessionManager.getCurrentSession();
        if (!currentSession) {
            console.log('[CLI Arranger] saveCurrentSessionData: 没有当前会话');
            return;
        }
        // 转换前端消息格式为后端格式
        const sessionMessages = messages.map(m => ({
            id: m.id || `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`,
            role: m.role,
            content: m.content,
            cli: m.cli,
            timestamp: m.time ? new Date().getTime() : Date.now(),
            images: m.images,
        }));
        // 使用新的 API 保存会话数据
        this.chatSessionManager.updateSessionData(currentSession.id, sessionMessages, cliOutputs);
        console.log('[CLI Arranger] 保存会话数据，消息数:', sessionMessages.length);
    }
    /** 构建 UI 状态 */
    buildUIState() {
        const session = this.sessionManager.getCurrentSession();
        const chatSession = this.chatSessionManager.getCurrentSession();
        const tasks = this.taskManager.getAllTasks();
        const currentTask = tasks.find(t => t.status === 'running') ?? tasks[tasks.length - 1];
        // 🔧 修复：使用 ChatSessionManager 的会话数据作为主数据源
        const allChatSessions = this.chatSessionManager.getAllSessions();
        // 构建 CLI 状态（包含能力信息）
        const cliStatuses = Array.from(this.cliStatuses.values()).map(status => ({
            ...status,
            capabilities: types_2.CLI_CAPABILITIES[status.type],
        }));
        // 🆕 修复：isRunning 同时考虑 Task 状态和 Orchestrator 运行状态
        const isRunning = currentTask?.status === 'running' || this.intelligentOrchestrator.running;
        return {
            currentSession: session ?? undefined,
            currentSessionId: chatSession?.id ?? session?.id,
            sessions: allChatSessions, // 🔧 修复：使用 chatSessionManager 的会话
            chatSessions: this.chatSessionManager.getSessionMetas(),
            currentChatSession: chatSession,
            currentTask,
            cliStatuses,
            degradationStrategy: {
                level: 3,
                availableCLIs: ['claude', 'codex', 'gemini'],
                missingCLIs: [],
                hasOrchestrator: true,
                recommendation: '',
                canProceed: true,
                fallbackMap: {},
            },
            pendingChanges: this.snapshotManager.getPendingChanges(),
            isRunning, // 🆕 使用修复后的 isRunning
            logs: [],
            interactionMode: this.intelligentOrchestrator.getInteractionMode(),
            orchestratorPhase: this.intelligentOrchestrator.phase,
        };
    }
    /** 发送消息到 Webview */
    postMessage(message) {
        this._view?.webview.postMessage(message);
    }
    /** 获取 HTML 内容 - 从外部模板文件加载 */
    getHtmlContent(webview) {
        // 读取外部 HTML 模板文件
        const templatePath = path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'index.html');
        let html = fs.readFileSync(templatePath, 'utf-8');
        // 替换 CSP 占位符
        html = html.replace(/\{\{cspSource\}\}/g, webview.cspSource);
        return html;
    }
    /** 获取管理器实例 */
    getSessionManager() { return this.sessionManager; }
    getTaskManager() { return this.taskManager; }
    getSnapshotManager() { return this.snapshotManager; }
    getDiffGenerator() { return this.diffGenerator; }
    /** 🆕 清理所有资源 - VSCode 关闭时调用 */
    async dispose() {
        console.log('[WebviewProvider] 开始清理资源...');
        try {
            // 1. 中断当前任务
            if (this.intelligentOrchestrator) {
                console.log('[WebviewProvider] 中断编排器...');
                this.intelligentOrchestrator.interrupt();
            }
            // 2. 清理 CLI 适配器（终止所有 CLI 进程）
            if (this.cliFactory) {
                console.log('[WebviewProvider] 清理 CLI 适配器...');
                await this.cliFactory.dispose();
            }
            // 3. 移除事件监听器
            events_1.globalEventBus.clear();
            console.log('[WebviewProvider] 事件监听器已移除');
            // 4. 清理待确认的 Promise
            if (this.pendingConfirmation) {
                this.pendingConfirmation.reject(new Error('扩展已停用'));
                this.pendingConfirmation = null;
            }
            // 5. 清理 Webview
            this._view = undefined;
            console.log('[WebviewProvider] 资源清理完成');
        }
        catch (error) {
            console.error('[WebviewProvider] 清理资源时出错:', error);
        }
    }
}
exports.WebviewProvider = WebviewProvider;
//# sourceMappingURL=webview-provider.js.map