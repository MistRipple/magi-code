"use strict";
/**
 * 执行调度器
 * 支持并行/串行执行策略，管理任务队列
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.ExecutionScheduler = void 0;
const events_1 = require("events");
const DEFAULT_CONFIG = {
    maxParallel: 3,
    timeout: 300000,
    retryCount: 1,
};
/**
 * 执行调度器类
 */
class ExecutionScheduler extends events_1.EventEmitter {
    factory;
    config;
    queue = [];
    running = new Map();
    results = new Map();
    isRunning = false;
    isCancelled = false;
    constructor(factory, config) {
        super();
        this.factory = factory;
        this.config = { ...DEFAULT_CONFIG, ...config };
    }
    /**
     * 执行拆分结果
     */
    async execute(splitResult) {
        this.reset();
        this.isRunning = true;
        this.queue = [...splitResult.subTasks];
        this.emit('start', { total: this.queue.length, mode: splitResult.executionMode });
        try {
            if (splitResult.executionMode === 'parallel') {
                await this.executeParallel();
            }
            else {
                await this.executeSequential();
            }
        }
        catch (error) {
            this.emit('error', error);
        }
        this.isRunning = false;
        const results = Array.from(this.results.values());
        this.emit('complete', { results, cancelled: this.isCancelled });
        return results;
    }
    /**
     * 串行执行
     */
    async executeSequential() {
        while (this.queue.length > 0 && !this.isCancelled) {
            const task = this.queue.shift();
            if (!task)
                break;
            // 检查依赖是否完成
            if (!this.checkDependencies(task)) {
                this.queue.push(task);
                continue;
            }
            const result = await this.executeSubTask(task);
            if (result.status === 'failed') {
                this.emit('taskFailed', { task, result });
                break;
            }
        }
    }
    /**
     * 并行执行
     */
    async executeParallel() {
        const promises = [];
        while ((this.queue.length > 0 || this.running.size > 0) && !this.isCancelled) {
            // 启动新任务
            while (this.queue.length > 0 && this.running.size < this.config.maxParallel) {
                const task = this.findReadyTask();
                if (!task)
                    break;
                this.running.set(task.id, task);
                promises.push(this.executeSubTask(task).then(() => {
                    this.running.delete(task.id);
                }));
            }
            if (this.running.size > 0) {
                await Promise.race(promises.filter(p => p));
            }
        }
        await Promise.all(promises);
    }
    /**
     * 查找可执行的任务
     */
    findReadyTask() {
        const index = this.queue.findIndex(t => this.checkDependencies(t));
        if (index === -1)
            return null;
        return this.queue.splice(index, 1)[0];
    }
    /**
     * 检查依赖是否满足
     */
    checkDependencies(task) {
        return task.dependencies.every(depId => {
            const result = this.results.get(depId);
            return result && result.status === 'completed';
        });
    }
    /**
     * 执行单个子任务（带重试机制）
     */
    async executeSubTask(task, retryCount = 0) {
        const result = {
            subTaskId: task.id,
            cli: task.assignedCli,
            status: 'running',
            startTime: Date.now(),
        };
        this.emit('taskStart', { task, result, retry: retryCount });
        try {
            const response = await this.executeWithTimeout(task);
            result.response = response;
            result.status = response.error ? 'failed' : 'completed';
            if (response.error)
                result.error = response.error;
        }
        catch (error) {
            result.status = 'failed';
            result.error = error instanceof Error ? error.message : String(error);
        }
        result.endTime = Date.now();
        result.duration = result.endTime - result.startTime;
        // 重试逻辑
        if (result.status === 'failed' && retryCount < this.config.retryCount) {
            const shouldRetry = this.shouldRetry(result.error);
            if (shouldRetry) {
                this.emit('taskRetry', { task, result, attempt: retryCount + 1, maxRetries: this.config.retryCount });
                await this.delay(this.getRetryDelay(retryCount));
                return this.executeSubTask(task, retryCount + 1);
            }
        }
        this.results.set(task.id, result);
        this.emit('taskComplete', { task, result, retries: retryCount });
        return result;
    }
    /**
     * 带超时的执行
     */
    async executeWithTimeout(task) {
        return new Promise((resolve, reject) => {
            const timeoutId = setTimeout(() => {
                reject(new Error(`任务执行超时 (${this.config.timeout}ms)`));
            }, this.config.timeout);
            this.factory.sendMessage(task.assignedCli, task.description)
                .then(response => {
                clearTimeout(timeoutId);
                resolve(response);
            })
                .catch(error => {
                clearTimeout(timeoutId);
                reject(error);
            });
        });
    }
    /**
     * 判断是否应该重试
     */
    shouldRetry(error) {
        if (!error)
            return false;
        // 可重试的错误类型
        const retryableErrors = [
            'timeout', '超时', 'ETIMEDOUT', 'ECONNRESET',
            'rate limit', '限流', 'overloaded', '过载',
            'temporary', '临时', 'retry', '重试',
        ];
        const lowerError = error.toLowerCase();
        return retryableErrors.some(e => lowerError.includes(e.toLowerCase()));
    }
    /**
     * 计算重试延迟（指数退避）
     */
    getRetryDelay(retryCount) {
        const baseDelay = 1000; // 1秒
        const maxDelay = 30000; // 最大30秒
        const delay = Math.min(baseDelay * Math.pow(2, retryCount), maxDelay);
        // 添加随机抖动
        return delay + Math.random() * 1000;
    }
    /**
     * 延迟函数
     */
    delay(ms) {
        return new Promise(resolve => setTimeout(resolve, ms));
    }
    /** 取消执行 */
    cancel() {
        this.isCancelled = true;
        this.factory.interruptAll();
        // 标记所有运行中的任务为取消状态
        for (const [id, task] of this.running) {
            const result = this.results.get(id);
            if (result && result.status === 'running') {
                result.status = 'cancelled';
                result.endTime = Date.now();
                result.duration = result.endTime - result.startTime;
            }
        }
        this.emit('cancelled', { pendingTasks: this.queue.length, runningTasks: this.running.size });
    }
    /** 获取可恢复的任务 */
    getResumableTasks() {
        // 返回队列中未执行的任务
        return [...this.queue];
    }
    /** 恢复执行 */
    async resume() {
        if (this.queue.length === 0) {
            return Array.from(this.results.values());
        }
        this.isCancelled = false;
        this.isRunning = true;
        this.emit('resumed', { pendingTasks: this.queue.length });
        // 继续执行剩余任务
        await this.executeSequential();
        this.isRunning = false;
        const results = Array.from(this.results.values());
        this.emit('complete', { results, resumed: true });
        return results;
    }
    /** 获取执行快照（用于持久化） */
    getSnapshot() {
        return {
            queue: [...this.queue],
            results: Array.from(this.results.values()),
            isRunning: this.isRunning,
            isCancelled: this.isCancelled,
            timestamp: Date.now(),
        };
    }
    /** 从快照恢复 */
    restoreFromSnapshot(snapshot) {
        this.queue = [...snapshot.queue];
        this.results.clear();
        for (const result of snapshot.results) {
            this.results.set(result.subTaskId, result);
        }
        this.isCancelled = snapshot.isCancelled;
    }
    /** 重置状态 */
    reset() {
        this.queue = [];
        this.running.clear();
        this.results.clear();
        this.isCancelled = false;
    }
    /** 获取执行状态 */
    get status() {
        return {
            running: this.isRunning,
            cancelled: this.isCancelled,
            pending: this.queue.length,
            completed: this.results.size,
        };
    }
}
exports.ExecutionScheduler = ExecutionScheduler;
//# sourceMappingURL=execution-scheduler.js.map