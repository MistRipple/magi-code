"use strict";
/**
 * Worker 基类
 * 定义 CLI 执行的抽象接口
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.BaseWorker = void 0;
const child_process_1 = require("child_process");
const events_1 = require("../events");
/**
 * 抽象 Worker 基类
 * 各 CLI Worker 需要继承此类并实现抽象方法
 */
class BaseWorker extends events_1.EventEmitter {
    config;
    process = null;
    isRunning = false;
    outputBuffer = [];
    constructor(config) {
        super();
        this.config = config;
    }
    /** 执行子任务 */
    async execute(options) {
        const { subTask, workingDirectory, timeout, onOutput } = options;
        const startTime = Date.now();
        this.isRunning = true;
        this.outputBuffer = [];
        events_1.globalEventBus.emitEvent('subtask:started', {
            taskId: subTask.taskId, subTaskId: subTask.id,
        });
        try {
            const args = this.buildArgs(subTask);
            const output = await this.runProcess(args, workingDirectory, timeout, onOutput);
            const parsed = this.parseOutput(output);
            const result = {
                workerId: `${this.cliType}-${subTask.id}`,
                cliType: this.cliType,
                success: true,
                output,
                duration: Date.now() - startTime,
                timestamp: new Date(),
                ...parsed,
            };
            events_1.globalEventBus.emitEvent('subtask:completed', {
                taskId: subTask.taskId, subTaskId: subTask.id, data: result,
            });
            return result;
        }
        catch (error) {
            const errorMessage = error instanceof Error ? error.message : String(error);
            events_1.globalEventBus.emitEvent('subtask:failed', {
                taskId: subTask.taskId, subTaskId: subTask.id, data: { error: errorMessage },
            });
            return {
                workerId: `${this.cliType}-${subTask.id}`,
                cliType: this.cliType,
                success: false,
                error: errorMessage,
                duration: Date.now() - startTime,
                timestamp: new Date(),
            };
        }
        finally {
            this.isRunning = false;
            this.process = null;
        }
    }
    /** 运行 CLI 进程 */
    runProcess(args, cwd, timeout, onOutput) {
        return new Promise((resolve, reject) => {
            const effectiveTimeout = timeout ?? this.config.timeout;
            let output = '';
            let timeoutId;
            this.process = (0, child_process_1.spawn)(this.config.cliPath, args, { cwd, shell: true, env: { ...process.env } });
            if (effectiveTimeout > 0) {
                timeoutId = setTimeout(() => {
                    this.interrupt();
                    reject(new Error(`执行超时 (${effectiveTimeout}ms)`));
                }, effectiveTimeout);
            }
            this.process.stdout?.on('data', (data) => {
                const chunk = data.toString();
                output += chunk;
                this.outputBuffer.push(chunk);
                onOutput?.(chunk);
                events_1.globalEventBus.emitEvent('subtask:output', { data: { output: chunk } });
            });
            this.process.stderr?.on('data', (data) => {
                const chunk = data.toString();
                output += chunk;
                this.outputBuffer.push(chunk);
                onOutput?.(chunk);
            });
            this.process.on('close', (code) => {
                if (timeoutId)
                    clearTimeout(timeoutId);
                code === 0 ? resolve(output) : reject(new Error(`进程退出码: ${code}\n${output}`));
            });
            this.process.on('error', (error) => {
                if (timeoutId)
                    clearTimeout(timeoutId);
                reject(error);
            });
        });
    }
    /** 打断执行 */
    interrupt() {
        if (this.process && this.isRunning) {
            this.process.kill('SIGTERM');
            this.isRunning = false;
            return true;
        }
        return false;
    }
    get running() { return this.isRunning; }
    getOutput() { return [...this.outputBuffer]; }
    getConfig() { return { ...this.config }; }
}
exports.BaseWorker = BaseWorker;
//# sourceMappingURL=base-worker.js.map