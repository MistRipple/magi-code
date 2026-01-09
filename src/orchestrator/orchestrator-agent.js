"use strict";
/**
 * Orchestrator Agent - 独立编排者 Claude
 *
 * 核心职责：
 * - 专职编排，不执行任何编码任务
 * - 实现事件循环，实时监控所有 Worker
 * - 响应用户交互和 Worker 反馈
 * - 动态调度和错误处理
 *
 * 架构理念：
 * - 编排者是"永远在线"的协调者
 * - 100% 时间用于监控和协调
 * - 可以立即响应任何事件
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.OrchestratorAgent = void 0;
const events_1 = require("events");
const events_2 = require("../events");
const message_bus_1 = require("./message-bus");
const worker_pool_1 = require("./worker-pool");
const verification_runner_1 = require("./verification-runner");
const orchestrator_prompts_1 = require("./prompts/orchestrator-prompts");
/** 默认配置 */
const DEFAULT_CONFIG = {
    timeout: 300000, // 5 分钟
    maxRetries: 3,
    verification: {
        compileCheck: true,
        lintCheck: true,
        testCheck: false,
    },
};
/**
 * Orchestrator Agent
 * 独立编排者 Claude 的核心实现
 */
class OrchestratorAgent extends events_1.EventEmitter {
    id = 'orchestrator';
    cliFactory;
    messageBus;
    workerPool;
    config;
    // 验证组件
    verificationRunner = null;
    workspaceRoot = '';
    _state = 'idle';
    currentContext = null;
    confirmationCallback = null;
    abortController = null;
    unsubscribers = [];
    // 任务执行状态
    pendingTasks = new Map();
    completedResults = [];
    failedTasks = new Map();
    constructor(cliFactory, config, workspaceRoot) {
        super();
        this.cliFactory = cliFactory;
        this.config = { ...DEFAULT_CONFIG, ...config };
        this.messageBus = message_bus_1.globalMessageBus;
        this.workspaceRoot = workspaceRoot || '';
        // 创建 Worker Pool
        this.workerPool = new worker_pool_1.WorkerPool({
            cliFactory,
            messageBus: this.messageBus,
            orchestratorId: this.id,
        });
        // 初始化验证组件
        if (this.workspaceRoot && this.config.verification) {
            this.verificationRunner = new verification_runner_1.VerificationRunner(this.workspaceRoot, {
                compileCheck: this.config.verification.compileCheck ?? true,
                lintCheck: this.config.verification.lintCheck ?? false,
                testCheck: this.config.verification.testCheck ?? false,
            });
        }
        this.setupMessageHandlers();
        this.setupWorkerPoolHandlers();
    }
    /** 获取当前状态 */
    get state() {
        return this._state;
    }
    /** 获取当前任务上下文 */
    get context() {
        return this.currentContext;
    }
    /** 设置状态 */
    setState(state) {
        if (this._state !== state) {
            const oldState = this._state;
            this._state = state;
            this.emit('stateChange', state);
            console.log(`[OrchestratorAgent] 状态变更: ${oldState} -> ${state}`);
        }
    }
    /** 设置确认回调 */
    setConfirmationCallback(callback) {
        this.confirmationCallback = callback;
    }
    /** 初始化 */
    async initialize() {
        await this.workerPool.initialize();
        console.log('[OrchestratorAgent] 初始化完成');
    }
    /** 设置消息处理器 */
    setupMessageHandlers() {
        // 监听任务完成消息
        const unsubCompleted = this.messageBus.subscribe('task_completed', (msg) => {
            this.handleTaskCompleted(msg);
        });
        this.unsubscribers.push(unsubCompleted);
        // 监听任务失败消息
        const unsubFailed = this.messageBus.subscribe('task_failed', (msg) => {
            this.handleTaskFailed(msg);
        });
        this.unsubscribers.push(unsubFailed);
        // 监听进度汇报消息
        const unsubProgress = this.messageBus.subscribe('progress_report', (msg) => {
            this.handleProgressReport(msg);
        });
        this.unsubscribers.push(unsubProgress);
    }
    /** 设置 Worker Pool 事件处理 */
    setupWorkerPoolHandlers() {
        this.workerPool.on('workerOutput', ({ workerId, workerType, chunk }) => {
            this.emitUIMessage('worker_output', chunk, { workerId, workerType });
        });
    }
    // =========================================================================
    // 核心执行流程
    // =========================================================================
    /**
     * 执行任务 - 主入口
     */
    async execute(userPrompt, taskId) {
        if (this._state !== 'idle') {
            throw new Error(`编排者当前状态为 ${this._state}，无法接受新任务`);
        }
        // 初始化任务上下文
        this.currentContext = {
            taskId,
            userPrompt,
            results: [],
            startTime: Date.now(),
        };
        this.abortController = new AbortController();
        this.completedResults = [];
        this.pendingTasks.clear();
        this.failedTasks.clear();
        try {
            // Phase 1: 任务分析
            this.setState('analyzing');
            const plan = await this.analyzeTask(userPrompt);
            if (!plan) {
                throw new Error('任务分析失败');
            }
            this.currentContext.plan = plan;
            this.checkAborted();
            // Phase 2: 等待用户确认
            this.setState('waiting_confirmation');
            const confirmed = await this.waitForConfirmation(plan);
            if (!confirmed) {
                this.setState('idle');
                return '任务已取消。';
            }
            this.checkAborted();
            // Phase 3: 分发任务给 Worker
            this.setState('dispatching');
            await this.dispatchTasks(plan);
            // Phase 4: 监控执行
            this.setState('monitoring');
            await this.monitorExecution(plan);
            this.checkAborted();
            // Phase 5: 验证阶段（如果配置了验证）
            let verificationResult = null;
            if (this.verificationRunner) {
                this.setState('verifying');
                verificationResult = await this.runVerification(taskId);
                // 如果验证失败，记录错误但继续汇总
                if (!verificationResult.success) {
                    this.emitUIMessage('error', `验证失败: ${verificationResult.summary}`);
                }
            }
            this.checkAborted();
            // Phase 6: 汇总结果
            this.setState('summarizing');
            const summary = await this.summarizeResults(userPrompt, this.completedResults, verificationResult);
            this.setState('completed');
            this.currentContext.endTime = Date.now();
            return summary;
        }
        catch (error) {
            const errorMsg = error instanceof Error ? error.message : String(error);
            if (this.abortController?.signal.aborted) {
                this.setState('idle');
                return '任务已被取消。';
            }
            this.setState('failed');
            this.emitUIMessage('error', `任务执行失败: ${errorMsg}`);
            throw error;
        }
        finally {
            this.cleanup();
        }
    }
    /** 检查是否被中断 */
    checkAborted() {
        if (this.abortController?.signal.aborted) {
            throw new Error('任务已被用户取消');
        }
    }
    /** 取消当前任务 */
    async cancel() {
        console.log('[OrchestratorAgent] 取消任务');
        this.abortController?.abort();
        await this.workerPool.cancelAllTasks();
        this.setState('idle');
    }
    /** 清理状态 */
    cleanup() {
        this.abortController = null;
        this.pendingTasks.clear();
    }
    // =========================================================================
    // Phase 1: 任务分析
    // =========================================================================
    /**
     * 分析任务，生成执行计划
     */
    async analyzeTask(userPrompt) {
        console.log('[OrchestratorAgent] Phase 1: 任务分析...');
        const availableWorkers = ['claude', 'codex', 'gemini'];
        const analysisPrompt = (0, orchestrator_prompts_1.buildOrchestratorAnalysisPrompt)(userPrompt, availableWorkers);
        try {
            // 使用 Claude 进行分析（编排者专用会话）
            const response = await this.cliFactory.sendMessage('claude', analysisPrompt);
            if (response.error) {
                console.error('[OrchestratorAgent] 分析失败:', response.error);
                return null;
            }
            const plan = this.parseExecutionPlan(response.content);
            if (plan) {
                this.emitUIMessage('plan_ready', (0, orchestrator_prompts_1.formatPlanForUser)(plan), { plan });
                events_2.globalEventBus.emitEvent('orchestrator:plan_ready', {
                    taskId: this.currentContext?.taskId,
                    data: { plan },
                });
            }
            return plan;
        }
        catch (error) {
            console.error('[OrchestratorAgent] 分析异常:', error);
            return null;
        }
    }
    /**
     * 解析执行计划 JSON
     */
    parseExecutionPlan(content) {
        try {
            const jsonMatch = content.match(/```json\s*([\s\S]*?)\s*```/);
            const jsonStr = jsonMatch ? jsonMatch[1] : content;
            const parsed = JSON.parse(jsonStr);
            return {
                id: `plan_${Date.now()}`,
                analysis: parsed.analysis || '',
                isSimpleTask: parsed.isSimpleTask || false,
                skipReason: parsed.skipReason,
                needsCollaboration: parsed.needsCollaboration ?? true,
                subTasks: (parsed.subTasks || []).map((t, i) => ({
                    id: t.id || String(i + 1),
                    description: t.description || '',
                    assignedWorker: t.assignedWorker || t.assignedCli || 'claude',
                    reason: t.reason || '',
                    targetFiles: t.targetFiles || [],
                    dependencies: t.dependencies || [],
                    prompt: t.prompt || '',
                })),
                executionMode: parsed.executionMode || 'sequential',
                summary: parsed.summary || '',
                createdAt: Date.now(),
            };
        }
        catch (error) {
            console.error('[OrchestratorAgent] 解析执行计划失败:', error);
            return null;
        }
    }
    // =========================================================================
    // Phase 2: 等待用户确认
    // =========================================================================
    /**
     * 等待用户确认执行计划
     */
    async waitForConfirmation(plan) {
        if (!this.confirmationCallback) {
            console.log('[OrchestratorAgent] 未设置确认回调，自动确认');
            return true;
        }
        const formattedPlan = (0, orchestrator_prompts_1.formatPlanForUser)(plan);
        events_2.globalEventBus.emitEvent('orchestrator:waiting_confirmation', {
            taskId: this.currentContext?.taskId,
            data: { plan, formattedPlan },
        });
        try {
            const confirmed = await this.confirmationCallback(plan, formattedPlan);
            console.log(`[OrchestratorAgent] 用户确认结果: ${confirmed ? 'Y' : 'N'}`);
            return confirmed;
        }
        catch (error) {
            console.error('[OrchestratorAgent] 等待确认异常:', error);
            return false;
        }
    }
    // =========================================================================
    // Phase 3: 分发任务
    // =========================================================================
    /** 分发任务给 Worker */
    async dispatchTasks(plan) {
        console.log('[OrchestratorAgent] Phase 3: 分发任务...');
        for (const subTask of plan.subTasks) {
            this.pendingTasks.set(subTask.id, subTask);
        }
        if (plan.executionMode === 'parallel') {
            await this.dispatchParallel(plan.subTasks);
        }
        else {
            await this.dispatchSequential(plan.subTasks);
        }
    }
    /** 并行分发任务 */
    async dispatchParallel(subTasks) {
        for (const subTask of subTasks) {
            const worker = this.workerPool.getWorker(subTask.assignedWorker);
            if (!worker)
                continue;
            this.emitUIMessage('progress_update', `分发任务给 ${subTask.assignedWorker}: ${subTask.description}`, { subTaskId: subTask.id, workerType: subTask.assignedWorker });
            this.messageBus.dispatchTask(this.id, worker.id, this.currentContext.taskId, subTask);
        }
    }
    /** 串行分发任务 */
    async dispatchSequential(subTasks) {
        for (const subTask of subTasks) {
            this.checkAborted();
            this.emitUIMessage('progress_update', `分发任务给 ${subTask.assignedWorker}: ${subTask.description}`, { subTaskId: subTask.id, workerType: subTask.assignedWorker });
            const result = await this.workerPool.dispatchTask(subTask.assignedWorker, this.currentContext.taskId, subTask);
            this.completedResults.push(result);
            this.pendingTasks.delete(subTask.id);
            if (!result.success)
                break;
        }
    }
    // =========================================================================
    // Phase 4: 监控执行
    // =========================================================================
    /** 监控任务执行（用于并行模式） */
    async monitorExecution(plan) {
        if (plan.executionMode !== 'parallel')
            return;
        console.log('[OrchestratorAgent] Phase 4: 监控执行...');
        return new Promise((resolve, reject) => {
            const interval = setInterval(() => {
                if (this.abortController?.signal.aborted) {
                    clearInterval(interval);
                    reject(new Error('任务已被取消'));
                    return;
                }
                if (this.pendingTasks.size === 0) {
                    clearInterval(interval);
                    resolve();
                }
            }, 1000);
            setTimeout(() => {
                clearInterval(interval);
                if (this.pendingTasks.size > 0)
                    reject(new Error('任务执行超时'));
            }, this.config.timeout);
        });
    }
    // =========================================================================
    // Phase 5: 验证阶段
    // =========================================================================
    /** 执行验证 */
    async runVerification(taskId) {
        console.log('[OrchestratorAgent] Phase 5: 验证阶段...');
        if (!this.verificationRunner) {
            return { success: true, summary: '跳过验证（未配置）' };
        }
        this.emitUIMessage('progress_update', '正在执行验证检查...');
        // 收集所有修改的文件
        const modifiedFiles = this.completedResults
            .flatMap(r => r.modifiedFiles || [])
            .filter((f, i, arr) => arr.indexOf(f) === i); // 去重
        try {
            const result = await this.verificationRunner.runVerification(taskId, modifiedFiles);
            if (result.success) {
                this.emitUIMessage('progress_update', `✅ 验证通过: ${result.summary}`);
            }
            else {
                this.emitUIMessage('error', `❌ 验证失败: ${result.summary}`);
            }
            return result;
        }
        catch (error) {
            const errorMsg = error instanceof Error ? error.message : String(error);
            return { success: false, summary: `验证执行出错: ${errorMsg}` };
        }
    }
    // =========================================================================
    // Phase 6: 汇总结果
    // =========================================================================
    /** 汇总执行结果 */
    async summarizeResults(userPrompt, results, verificationResult) {
        console.log('[OrchestratorAgent] Phase 6: 汇总结果...');
        if (results.length === 0) {
            return '没有执行任何任务。';
        }
        // 构建包含验证结果的汇总 prompt
        let summaryPrompt = (0, orchestrator_prompts_1.buildOrchestratorSummaryPrompt)(userPrompt, results);
        if (verificationResult) {
            summaryPrompt += `\n\n## 验证结果\n${verificationResult.summary}`;
        }
        try {
            const response = await this.cliFactory.sendMessage('claude', summaryPrompt);
            if (response.error) {
                return `任务执行完成，但汇总失败: ${response.error}`;
            }
            this.emitUIMessage('summary', response.content);
            return response.content;
        }
        catch (error) {
            const errorMsg = error instanceof Error ? error.message : String(error);
            return `任务执行完成，但汇总失败: ${errorMsg}`;
        }
    }
    // =========================================================================
    // 消息处理
    // =========================================================================
    /** 处理任务完成消息 */
    handleTaskCompleted(message) {
        const { result } = message.payload;
        this.completedResults.push(result);
        this.pendingTasks.delete(result.subTaskId);
        const total = this.currentContext?.plan?.subTasks.length || 0;
        const completed = this.completedResults.length;
        this.emitUIMessage('progress_update', (0, orchestrator_prompts_1.buildProgressMessage)(completed, total, result.workerType), { progress: Math.round((completed / total) * 100), result });
        this.emit('taskCompleted', result);
    }
    /** 处理任务失败消息 */
    handleTaskFailed(message) {
        const { taskId, subTaskId, error, canRetry } = message.payload;
        const subTask = this.pendingTasks.get(subTaskId);
        if (subTask) {
            const existing = this.failedTasks.get(subTaskId);
            const retries = existing ? existing.retries + 1 : 1;
            if (canRetry && retries < this.config.maxRetries) {
                this.failedTasks.set(subTaskId, { task: subTask, error, retries });
                // 实现重试逻辑
                this.emitUIMessage('progress_update', `子任务失败，正在重试 (${retries}/${this.config.maxRetries}): ${error}`, { subTaskId });
                // 延迟重试，避免立即重试导致相同错误
                setTimeout(() => {
                    this.retryTask(subTask, retries);
                }, 1000 * retries); // 递增延迟
            }
            else {
                // 超过最大重试次数，标记为最终失败
                this.pendingTasks.delete(subTaskId);
                this.failedTasks.delete(subTaskId);
                // 记录失败结果
                const failedResult = {
                    workerId: 'unknown',
                    workerType: subTask.assignedWorker,
                    taskId,
                    subTaskId,
                    result: '',
                    success: false,
                    duration: 0,
                    error: `任务失败（已重试 ${retries} 次）: ${error}`,
                };
                this.completedResults.push(failedResult);
                this.emitUIMessage('error', `子任务最终失败: ${error}`, { subTaskId });
            }
        }
    }
    /** 重试失败的任务 */
    async retryTask(subTask, retryCount) {
        if (this.abortController?.signal.aborted) {
            return;
        }
        console.log(`[OrchestratorAgent] 重试任务 ${subTask.id}，第 ${retryCount} 次`);
        try {
            const result = await this.workerPool.dispatchTask(subTask.assignedWorker, this.currentContext.taskId, subTask);
            // 重试成功，清理失败记录
            if (result.success) {
                this.failedTasks.delete(subTask.id);
                this.pendingTasks.delete(subTask.id);
                this.completedResults.push(result);
                this.emitUIMessage('progress_update', `任务重试成功: ${subTask.description}`, { subTaskId: subTask.id });
            }
            // 如果重试仍然失败，handleTaskFailed 会再次被调用
        }
        catch (error) {
            console.error(`[OrchestratorAgent] 重试任务失败:`, error);
        }
    }
    /** 处理进度汇报消息 */
    handleProgressReport(message) {
        const { subTaskId, status, progress, message: msg, output } = message.payload;
        if (output) {
            this.emitUIMessage('worker_output', output, { subTaskId });
        }
        if (msg) {
            this.emitUIMessage('progress_update', msg, { subTaskId, progress });
        }
    }
    // =========================================================================
    // UI 消息发送
    // =========================================================================
    /** 发送 UI 消息 */
    emitUIMessage(type, content, metadata) {
        const message = {
            type,
            taskId: this.currentContext?.taskId || '',
            timestamp: Date.now(),
            content,
            metadata: { phase: this._state, ...metadata },
        };
        events_2.globalEventBus.emitEvent('orchestrator:ui_message', { data: message });
        this.emit('uiMessage', message);
    }
    // =========================================================================
    // 生命周期
    // =========================================================================
    /** 销毁编排者 */
    dispose() {
        this.unsubscribers.forEach(unsub => unsub());
        this.unsubscribers = [];
        this.workerPool.dispose();
        this.cleanup();
        this.removeAllListeners();
        console.log('[OrchestratorAgent] 已销毁');
    }
}
exports.OrchestratorAgent = OrchestratorAgent;
//# sourceMappingURL=orchestrator-agent.js.map