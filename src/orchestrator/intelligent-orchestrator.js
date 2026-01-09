"use strict";
/**
 * 智能编排器 - 独立编排者架构
 *
 * 架构重构：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.IntelligentOrchestrator = void 0;
const types_1 = require("../types");
const events_1 = require("../events");
const orchestrator_agent_1 = require("./orchestrator-agent");
const verification_runner_1 = require("./verification-runner");
const DEFAULT_CONFIG = {
    timeout: 300000,
    maxRetries: 3,
};
/**
 * 智能编排器 - 基于独立编排者架构
 */
class IntelligentOrchestrator {
    cliFactory;
    taskManager;
    snapshotManager;
    config;
    workspaceRoot;
    // 核心：独立编排者 Agent
    orchestratorAgent;
    // 交互模式
    interactionMode = 'agent';
    modeConfig = types_1.INTERACTION_MODE_CONFIGS.agent;
    // 验证器
    verificationRunner = null;
    // 状态
    isRunning = false;
    currentTaskId = null;
    abortController = null;
    statusUpdateInterval = null;
    constructor(cliFactory, taskManager, snapshotManager, workspaceRoot, config) {
        this.cliFactory = cliFactory;
        this.taskManager = taskManager;
        this.snapshotManager = snapshotManager;
        this.workspaceRoot = workspaceRoot;
        this.config = { ...DEFAULT_CONFIG, ...config };
        // 创建独立编排者 Agent，传递 workspaceRoot 以支持验证功能
        this.orchestratorAgent = new orchestrator_agent_1.OrchestratorAgent(cliFactory, {
            timeout: this.config.timeout,
            maxRetries: this.config.maxRetries,
            verification: this.config.verification,
        }, workspaceRoot);
        this.setupOrchestratorEvents();
    }
    /** 设置编排者事件监听 */
    setupOrchestratorEvents() {
        this.orchestratorAgent.on('stateChange', (state) => {
            events_1.globalEventBus.emitEvent('orchestrator:phase_changed', {
                taskId: this.currentTaskId || undefined,
                data: { phase: state, isRunning: this.isRunning },
            });
        });
        this.orchestratorAgent.on('uiMessage', (message) => {
            if (message.type === 'worker_output') {
                events_1.globalEventBus.emitEvent('cli:output', {
                    taskId: this.currentTaskId || undefined,
                    data: { cli: message.metadata?.workerType, chunk: message.content },
                });
            }
        });
    }
    /** 设置交互模式 */
    setInteractionMode(mode) {
        this.interactionMode = mode;
        this.modeConfig = types_1.INTERACTION_MODE_CONFIGS[mode];
        console.log(`[IntelligentOrchestrator] 交互模式设置为: ${mode}`);
        events_1.globalEventBus.emitEvent('orchestrator:mode_changed', { data: { mode } });
    }
    /** 获取当前交互模式 */
    getInteractionMode() {
        return this.interactionMode;
    }
    /** 设置用户确认回调 */
    setConfirmationCallback(callback) {
        this.orchestratorAgent.setConfirmationCallback(callback);
    }
    /** 设置恢复确认回调（向后兼容） */
    setRecoveryConfirmationCallback(_callback) {
        // TODO: 实现恢复确认逻辑
    }
    /** 获取当前阶段 */
    get phase() {
        return this.orchestratorAgent.state;
    }
    /** 获取当前执行计划 */
    get plan() {
        return this.orchestratorAgent.context?.plan || null;
    }
    /** 是否正在运行（向后兼容） */
    get running() {
        return this.isRunning;
    }
    /** 中断当前任务（向后兼容） */
    async interrupt() {
        await this.cancel();
    }
    /** 初始化编排者 */
    async initialize() {
        await this.orchestratorAgent.initialize();
        if (this.config.verification) {
            this.verificationRunner = new verification_runner_1.VerificationRunner(this.workspaceRoot, this.config.verification);
        }
    }
    /**
     * 执行任务 - 主入口
     */
    async execute(userPrompt, taskId) {
        if (this.isRunning) {
            throw new Error('编排器正在运行中');
        }
        this.isRunning = true;
        this.currentTaskId = taskId;
        this.abortController = new AbortController();
        this.taskManager.updateTaskStatus(taskId, 'running');
        events_1.globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
        this.startStatusUpdates(taskId);
        try {
            // ask 模式：仅对话
            if (this.interactionMode === 'ask') {
                return await this.executeAskMode(userPrompt, taskId);
            }
            // agent/auto 模式：使用独立编排者执行
            const result = await this.orchestratorAgent.execute(userPrompt, taskId);
            this.taskManager.updateTaskStatus(taskId, 'completed');
            events_1.globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });
            return result;
        }
        catch (error) {
            const errorMsg = error instanceof Error ? error.message : String(error);
            if (this.abortController?.signal.aborted) {
                this.taskManager.updateTaskStatus(taskId, 'cancelled');
                events_1.globalEventBus.emitEvent('task:interrupted', { taskId, data: { isRunning: false } });
                return '任务已被取消。';
            }
            this.taskManager.updateTaskStatus(taskId, 'failed');
            events_1.globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
            throw error;
        }
        finally {
            this.isRunning = false;
            this.stopStatusUpdates();
            this.abortController = null;
            this.currentTaskId = null;
        }
    }
    /** ask 模式：仅对话 */
    async executeAskMode(userPrompt, taskId) {
        console.log('[IntelligentOrchestrator] ask 模式：仅对话');
        const response = await this.cliFactory.sendMessage('claude', userPrompt);
        if (response.error) {
            throw new Error(response.error);
        }
        this.taskManager.updateTaskStatus(taskId, 'completed');
        events_1.globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });
        return response.content || '';
    }
    /** 取消当前任务 */
    async cancel() {
        console.log('[IntelligentOrchestrator] 取消任务');
        this.abortController?.abort();
        await this.orchestratorAgent.cancel();
    }
    /** 开始状态更新定时器 */
    startStatusUpdates(taskId) {
        this.stopStatusUpdates();
        this.statusUpdateInterval = setInterval(() => {
            if (this.isRunning) {
                events_1.globalEventBus.emitEvent('orchestrator:phase_changed', {
                    taskId,
                    data: { phase: this.orchestratorAgent.state, isRunning: true },
                });
            }
        }, 2000);
    }
    /** 停止状态更新定时器 */
    stopStatusUpdates() {
        if (this.statusUpdateInterval) {
            clearInterval(this.statusUpdateInterval);
            this.statusUpdateInterval = null;
        }
    }
    /** 获取可用的 CLI 列表 */
    getAvailableCLIs() {
        return ['claude', 'codex', 'gemini'];
    }
    /** 销毁编排器 */
    dispose() {
        this.stopStatusUpdates();
        this.orchestratorAgent.dispose();
        console.log('[IntelligentOrchestrator] 已销毁');
    }
}
exports.IntelligentOrchestrator = IntelligentOrchestrator;
//# sourceMappingURL=intelligent-orchestrator.js.map