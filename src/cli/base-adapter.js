"use strict";
/**
 * CLI 适配器基类
 * 提供通用的进程管理和输出解析功能
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.BaseCLIAdapter = void 0;
const events_1 = require("events");
const child_process_1 = require("child_process");
/** 默认超时时间：5分钟 */
const DEFAULT_TIMEOUT = 5 * 60 * 1000;
/**
 * CLI 适配器基类
 */
class BaseCLIAdapter extends events_1.EventEmitter {
    type;
    config;
    process = null;
    outputBuffer = '';
    _state = 'idle';
    currentResolve = null;
    currentReject = null;
    timeoutHandle = null;
    constructor(config) {
        super();
        this.type = config.type;
        this.config = {
            timeout: DEFAULT_TIMEOUT,
            ...config,
        };
    }
    get state() {
        return this._state;
    }
    get isConnected() {
        return this._state === 'ready' || this._state === 'busy';
    }
    get isBusy() {
        return this._state === 'busy';
    }
    setState(state) {
        if (this._state !== state) {
            this._state = state;
            this.emit('stateChange', state);
        }
    }
    /** 连接到 CLI */
    async connect() {
        if (this.isConnected) {
            return;
        }
        this.setState('connecting');
        try {
            const command = this.config.command || this.getCommand();
            const args = this.config.args || this.getArgs();
            this.process = (0, child_process_1.spawn)(command, args, {
                cwd: this.config.cwd,
                env: { ...process.env, ...this.config.env },
                shell: true,
            });
            this.setupProcessHandlers();
            // 等待进程就绪
            await this.waitForReady();
            this.setState('ready');
        }
        catch (error) {
            this.setState('error');
            throw error;
        }
    }
    /** 设置进程事件处理 */
    setupProcessHandlers() {
        if (!this.process)
            return;
        this.process.stdout?.on('data', (data) => {
            const chunk = data.toString();
            this.outputBuffer += chunk;
            this.emit('output', chunk);
            this.checkResponseComplete();
        });
        this.process.stderr?.on('data', (data) => {
            const chunk = data.toString();
            this.outputBuffer += chunk;
            this.emit('output', chunk);
        });
        this.process.on('close', (code) => {
            this.setState('disconnected');
            if (this.currentReject) {
                this.currentReject(new Error(`CLI process exited with code ${code}`));
                this.currentReject = null;
                this.currentResolve = null;
            }
        });
        this.process.on('error', (error) => {
            this.setState('error');
            this.emit('error', error);
            if (this.currentReject) {
                this.currentReject(error);
                this.currentReject = null;
                this.currentResolve = null;
            }
        });
    }
    /** 等待 CLI 就绪（子类可覆盖） */
    async waitForReady() {
        // 默认等待 1 秒
        return new Promise((resolve) => setTimeout(resolve, 1000));
    }
    /** 检查响应是否完成 */
    checkResponseComplete() {
        if (!this.currentResolve)
            return;
        if (this.isResponseComplete(this.outputBuffer)) {
            this.clearTimeout();
            const response = this.parseOutput(this.outputBuffer);
            this.outputBuffer = '';
            this.setState('ready');
            this.emit('response', response);
            // 处理文件变更
            if (response.fileChanges) {
                response.fileChanges.forEach(change => this.emit('fileChange', change));
            }
            const resolve = this.currentResolve;
            this.currentResolve = null;
            this.currentReject = null;
            resolve(response);
        }
    }
    /** 发送消息 */
    async sendMessage(message) {
        if (!this.isConnected) {
            throw new Error('CLI not connected');
        }
        if (this.isBusy) {
            throw new Error('CLI is busy');
        }
        this.setState('busy');
        this.outputBuffer = '';
        return new Promise((resolve, reject) => {
            this.currentResolve = resolve;
            this.currentReject = reject;
            // 设置超时
            this.timeoutHandle = setTimeout(() => {
                this.setState('ready');
                const error = new Error('Request timeout');
                this.currentReject = null;
                this.currentResolve = null;
                reject(error);
            }, this.config.timeout);
            // 发送消息到进程
            this.process?.stdin?.write(message + '\n');
        });
    }
    /** 中断当前操作 */
    async interrupt() {
        if (!this.process)
            return;
        this.clearTimeout();
        // 发送 SIGINT 信号
        this.process.kill('SIGINT');
        if (this.currentReject) {
            this.currentReject(new Error('Operation interrupted'));
            this.currentReject = null;
            this.currentResolve = null;
        }
        this.outputBuffer = '';
        this.setState('ready');
    }
    /** 断开连接 */
    async disconnect() {
        this.clearTimeout();
        if (this.process) {
            this.process.kill();
            this.process = null;
        }
        this.outputBuffer = '';
        this.currentResolve = null;
        this.currentReject = null;
        this.setState('disconnected');
    }
    /** 清除超时 */
    clearTimeout() {
        if (this.timeoutHandle) {
            clearTimeout(this.timeoutHandle);
            this.timeoutHandle = null;
        }
    }
}
exports.BaseCLIAdapter = BaseCLIAdapter;
//# sourceMappingURL=base-adapter.js.map