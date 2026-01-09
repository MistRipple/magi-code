"use strict";
/**
 * Worker Agent - 执行者代理基类
 *
 * 核心职责：
 * - 接收编排者分配的任务
 * - 执行编码任务
 * - 向编排者汇报进度和结果
 *
 * 所有 Worker（包括 Worker Claude）都继承此基类
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.WorkerAgent = void 0;
const events_1 = require("events");
const message_bus_1 = require("./message-bus");
/**
 * Worker Agent 基类
 * 封装 CLI 适配器，提供统一的任务执行和汇报接口
 */
class WorkerAgent extends events_1.EventEmitter {
    id;
    type;
    cliFactory;
    messageBus;
    orchestratorId;
    _state = 'idle';
    currentTaskId = null;
    currentSubTaskId = null;
    abortController = null;
    unsubscribers = [];
    constructor(config) {
        super();
        this.id = `worker_${config.type}_${Date.now()}`;
        this.type = config.type;
        this.cliFactory = config.cliFactory;
        this.messageBus = config.messageBus || message_bus_1.globalMessageBus;
        this.orchestratorId = config.orchestratorId || 'orchestrator';
        this.setupMessageHandlers();
    }
    /** 获取当前状态 */
    get state() {
        return this._state;
    }
    /** 获取 Worker 信息 */
    get info() {
        return {
            id: this.id,
            type: this.type,
            state: this._state,
            currentTaskId: this.currentTaskId || undefined,
            lastActivity: Date.now(),
        };
    }
    /** 设置状态 */
    setState(state) {
        if (this._state !== state) {
            this._state = state;
            this.emit('stateChange', state);
        }
    }
    /** 设置消息处理器 */
    setupMessageHandlers() {
        // 订阅发送给自己的消息
        const unsubSelf = this.messageBus.subscribe(this.id, (msg) => this.handleMessage(msg));
        this.unsubscribers.push(unsubSelf);
        // 订阅任务分发消息
        const unsubDispatch = this.messageBus.subscribe('task_dispatch', (msg) => {
            if (msg.target === this.id) {
                this.handleMessage(msg);
            }
        });
        this.unsubscribers.push(unsubDispatch);
        // 订阅编排者命令
        const unsubCommand = this.messageBus.subscribe('orchestrator_command', (msg) => {
            this.handleMessage(msg);
        });
        this.unsubscribers.push(unsubCommand);
    }
    /** 处理消息 */
    async handleMessage(message) {
        switch (message.type) {
            case 'task_dispatch':
                await this.handleTaskDispatch(message);
                break;
            case 'task_cancel':
                await this.handleTaskCancel(message);
                break;
            case 'orchestrator_command':
                await this.handleOrchestratorCommand(message);
                break;
        }
    }
    /** 处理任务分发 */
    async handleTaskDispatch(message) {
        const { taskId, subTask, context } = message.payload;
        if (this._state !== 'idle') {
            console.warn(`[WorkerAgent ${this.id}] 收到任务但当前状态为 ${this._state}，忽略`);
            return;
        }
        await this.executeTask(taskId, subTask, context);
    }
    /** 处理任务取消 */
    async handleTaskCancel(message) {
        const { taskId, subTaskId } = message.payload;
        if (this.currentTaskId === taskId && (!subTaskId || this.currentSubTaskId === subTaskId)) {
            await this.cancel();
        }
    }
    /** 处理编排者命令 */
    async handleOrchestratorCommand(message) {
        const { command } = message.payload;
        switch (command) {
            case 'cancel_all':
                await this.cancel();
                break;
            case 'status_check':
                this.reportReady();
                break;
        }
    }
    /**
     * 执行任务
     * 核心方法：接收子任务，调用 CLI 执行，汇报结果
     */
    async executeTask(taskId, subTask, context) {
        const startTime = Date.now();
        this.currentTaskId = taskId;
        this.currentSubTaskId = subTask.id;
        this.abortController = new AbortController();
        this.setState('executing');
        // 汇报任务开始
        this.messageBus.reportProgress(this.id, this.orchestratorId, taskId, subTask.id, 'started', { message: `开始执行: ${subTask.description}` });
        try {
            // 构建执行 prompt
            const prompt = this.buildExecutionPrompt(subTask, context);
            // 调用 CLI 执行
            const response = await this.executeCLI(prompt);
            // 检查是否被取消
            if (this.abortController.signal.aborted) {
                throw new Error('任务已被取消');
            }
            const result = {
                workerId: this.id,
                workerType: this.type,
                taskId,
                subTaskId: subTask.id,
                result: response.content,
                success: !response.error,
                duration: Date.now() - startTime,
                modifiedFiles: response.fileChanges?.map(f => f.filePath),
                error: response.error,
            };
            this.setState('completed');
            // 汇报任务完成
            this.messageBus.reportTaskCompleted(this.id, this.orchestratorId, result);
            this.emit('completed', result);
            this.cleanup();
            return result;
        }
        catch (error) {
            const errorMsg = error instanceof Error ? error.message : String(error);
            this.setState('failed');
            // 汇报任务失败
            this.messageBus.reportTaskFailed(this.id, this.orchestratorId, taskId, subTask.id, errorMsg, true // canRetry
            );
            this.emit('failed', errorMsg);
            const result = {
                workerId: this.id,
                workerType: this.type,
                taskId,
                subTaskId: subTask.id,
                result: '',
                success: false,
                duration: Date.now() - startTime,
                error: errorMsg,
            };
            this.cleanup();
            return result;
        }
    }
    /**
     * 构建执行 prompt
     */
    buildExecutionPrompt(subTask, context) {
        const filesHint = subTask.targetFiles?.length
            ? `\n\n**Target Files**: ${subTask.targetFiles.join(', ')}`
            : '';
        const contextHint = context ? `\n\n**Context**:\n${context}` : '';
        return `${subTask.prompt}${filesHint}${contextHint}

**EXECUTION MODE**: Direct modification
- You have FULL write permission to modify files directly
- Make the necessary changes to complete the task
- Provide a brief summary of what you changed after completion`;
    }
    /**
     * 调用 CLI 执行任务
     */
    async executeCLI(prompt) {
        const adapter = this.cliFactory.getOrCreate(this.type);
        if (!adapter.isConnected) {
            await adapter.connect();
        }
        // 设置输出监听，转发流式输出
        const outputHandler = (chunk) => {
            this.emit('output', chunk);
            if (this.currentTaskId && this.currentSubTaskId) {
                this.messageBus.reportProgress(this.id, this.orchestratorId, this.currentTaskId, this.currentSubTaskId, 'in_progress', { output: chunk });
            }
        };
        adapter.on('output', outputHandler);
        try {
            const response = await adapter.sendMessage(prompt);
            return response;
        }
        finally {
            adapter.off('output', outputHandler);
        }
    }
    /**
     * 取消当前任务
     */
    async cancel() {
        if (this._state !== 'executing') {
            return;
        }
        console.log(`[WorkerAgent ${this.id}] 取消任务`);
        // 触发中断信号
        this.abortController?.abort();
        // 中断 CLI
        const adapter = this.cliFactory.getAdapter(this.type);
        if (adapter) {
            await adapter.interrupt();
        }
        this.setState('cancelled');
        this.cleanup();
    }
    /**
     * 汇报 Worker 就绪状态
     */
    reportReady() {
        this.messageBus.reportWorkerReady(this.id, this.orchestratorId, this.info);
    }
    /**
     * 清理任务状态
     */
    cleanup() {
        this.currentTaskId = null;
        this.currentSubTaskId = null;
        this.abortController = null;
        this.setState('idle');
    }
    /**
     * 销毁 Worker
     */
    dispose() {
        // 取消所有订阅
        this.unsubscribers.forEach(unsub => unsub());
        this.unsubscribers = [];
        // 清理状态
        this.cleanup();
        this.removeAllListeners();
    }
}
exports.WorkerAgent = WorkerAgent;
//# sourceMappingURL=worker-agent.js.map