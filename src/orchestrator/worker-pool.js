"use strict";
/**
 * Worker Pool - Worker 池管理
 *
 * 核心功能：
 * - 管理所有 Worker 实例
 * - 提供 Worker 获取和分配
 * - 监控 Worker 状态
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.WorkerPool = void 0;
const events_1 = require("events");
const worker_agent_1 = require("./worker-agent");
const message_bus_1 = require("./message-bus");
/**
 * Worker Pool
 * 管理所有 Worker 实例，提供统一的访问接口
 */
class WorkerPool extends events_1.EventEmitter {
    workers = new Map();
    cliFactory;
    messageBus;
    orchestratorId;
    unsubscribers = [];
    constructor(config) {
        super();
        this.cliFactory = config.cliFactory;
        this.messageBus = config.messageBus || message_bus_1.globalMessageBus;
        this.orchestratorId = config.orchestratorId || 'orchestrator';
        this.setupMessageHandlers();
    }
    /**
     * 初始化所有 Worker
     */
    async initialize() {
        const workerTypes = ['claude', 'codex', 'gemini'];
        for (const type of workerTypes) {
            await this.createWorker(type);
        }
        console.log(`[WorkerPool] 初始化完成，共 ${this.workers.size} 个 Worker`);
    }
    /**
     * 创建单个 Worker
     */
    async createWorker(type) {
        if (this.workers.has(type)) {
            return this.workers.get(type);
        }
        const worker = new worker_agent_1.WorkerAgent({
            type,
            cliFactory: this.cliFactory,
            messageBus: this.messageBus,
            orchestratorId: this.orchestratorId,
        });
        // 监听 Worker 状态变更
        worker.on('stateChange', (newState) => {
            this.emit('workerStateChange', {
                workerId: worker.id,
                workerType: type,
                newState,
            });
        });
        // 监听 Worker 输出
        worker.on('output', (chunk) => {
            this.emit('workerOutput', { workerId: worker.id, workerType: type, chunk });
        });
        this.workers.set(type, worker);
        console.log(`[WorkerPool] 创建 Worker: ${worker.id}`);
        return worker;
    }
    /**
     * 设置消息处理器
     */
    setupMessageHandlers() {
        // 监听任务完成消息
        const unsubCompleted = this.messageBus.subscribe('task_completed', (msg) => {
            const message = msg;
            this.emit('taskCompleted', message.payload.result);
        });
        this.unsubscribers.push(unsubCompleted);
        // 监听任务失败消息
        const unsubFailed = this.messageBus.subscribe('task_failed', (msg) => {
            const message = msg;
            this.emit('taskFailed', message.payload);
        });
        this.unsubscribers.push(unsubFailed);
        // 监听进度汇报消息
        const unsubProgress = this.messageBus.subscribe('progress_report', (msg) => {
            const message = msg;
            this.emit('workerProgress', message.payload);
        });
        this.unsubscribers.push(unsubProgress);
    }
    /**
     * 获取指定类型的 Worker
     */
    getWorker(type) {
        return this.workers.get(type);
    }
    /**
     * 获取或创建 Worker
     */
    async getOrCreateWorker(type) {
        const existing = this.workers.get(type);
        if (existing) {
            return existing;
        }
        return this.createWorker(type);
    }
    /**
     * 获取所有 Worker
     */
    getAllWorkers() {
        return Array.from(this.workers.values());
    }
    /**
     * 获取所有 Worker 信息
     */
    getAllWorkerInfo() {
        return this.getAllWorkers().map(w => w.info);
    }
    /**
     * 获取空闲的 Worker
     */
    getIdleWorkers() {
        return this.getAllWorkers().filter(w => w.state === 'idle');
    }
    /**
     * 获取指定类型的空闲 Worker
     */
    getIdleWorker(type) {
        const worker = this.workers.get(type);
        return worker?.state === 'idle' ? worker : undefined;
    }
    /**
     * 检查指定类型的 Worker 是否空闲
     */
    isWorkerIdle(type) {
        const worker = this.workers.get(type);
        return (worker?.state === 'idle') || false;
    }
    /**
     * 分发任务给指定 Worker
     */
    async dispatchTask(type, taskId, subTask, context) {
        const worker = await this.getOrCreateWorker(type);
        if (worker.state !== 'idle') {
            throw new Error(`Worker ${type} 当前状态为 ${worker.state}，无法接受新任务`);
        }
        return worker.executeTask(taskId, subTask, context);
    }
    /**
     * 通过消息总线分发任务（异步）
     */
    dispatchTaskAsync(type, taskId, subTask, context) {
        const worker = this.workers.get(type);
        if (!worker) {
            console.error(`[WorkerPool] Worker ${type} 不存在`);
            return;
        }
        this.messageBus.dispatchTask(this.orchestratorId, worker.id, taskId, subTask, context);
    }
    /**
     * 取消指定 Worker 的任务
     */
    async cancelWorkerTask(type) {
        const worker = this.workers.get(type);
        if (worker) {
            await worker.cancel();
        }
    }
    /**
     * 取消所有 Worker 的任务
     */
    async cancelAllTasks() {
        const promises = this.getAllWorkers().map(w => w.cancel());
        await Promise.all(promises);
    }
    /**
     * 广播取消命令
     */
    broadcastCancel() {
        this.messageBus.broadcastCommand(this.orchestratorId, 'cancel_all');
    }
    /**
     * 销毁 Worker Pool
     */
    dispose() {
        // 取消所有订阅
        this.unsubscribers.forEach(unsub => unsub());
        this.unsubscribers = [];
        // 销毁所有 Worker
        this.workers.forEach(worker => worker.dispose());
        this.workers.clear();
        this.removeAllListeners();
        console.log('[WorkerPool] 已销毁');
    }
}
exports.WorkerPool = WorkerPool;
//# sourceMappingURL=worker-pool.js.map