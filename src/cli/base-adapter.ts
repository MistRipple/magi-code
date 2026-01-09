/**
 * CLI 适配器基类
 * 提供通用的进程管理和输出解析功能
 */

import { EventEmitter } from 'events';
import { ChildProcess, spawn } from 'child_process';
import {
  ICLIAdapter,
  CLIType,
  CLIResponse,
  AdapterState,
  AdapterConfig,
  FileChange,
} from './types';

/** 默认超时时间：5分钟 */
const DEFAULT_TIMEOUT = 5 * 60 * 1000;

/**
 * CLI 适配器基类
 */
export abstract class BaseCLIAdapter extends EventEmitter implements ICLIAdapter {
  readonly type: CLIType;
  protected config: AdapterConfig;
  protected process: ChildProcess | null = null;
  protected outputBuffer: string = '';
  protected _state: AdapterState = 'idle';
  protected currentResolve: ((response: CLIResponse) => void) | null = null;
  protected currentReject: ((error: Error) => void) | null = null;
  protected timeoutHandle: NodeJS.Timeout | null = null;

  constructor(config: AdapterConfig) {
    super();
    this.type = config.type;
    this.config = {
      timeout: DEFAULT_TIMEOUT,
      ...config,
    };
  }

  get state(): AdapterState {
    return this._state;
  }

  get isConnected(): boolean {
    return this._state === 'ready' || this._state === 'busy';
  }

  get isBusy(): boolean {
    return this._state === 'busy';
  }

  protected setState(state: AdapterState): void {
    if (this._state !== state) {
      this._state = state;
      this.emit('stateChange', state);
    }
  }

  /** 获取 CLI 命令和参数（子类实现） */
  protected abstract getCommand(): string;
  protected abstract getArgs(): string[];

  /** 解析 CLI 输出（子类实现） */
  protected abstract parseOutput(output: string): CLIResponse;

  /** 检测响应是否完成（子类实现） */
  protected abstract isResponseComplete(output: string): boolean;

  /** 连接到 CLI */
  async connect(): Promise<void> {
    if (this.isConnected) {
      return;
    }

    this.setState('connecting');

    try {
      const command = this.config.command || this.getCommand();
      const args = this.config.args || this.getArgs();

      this.process = spawn(command, args, {
        cwd: this.config.cwd,
        env: { ...process.env, ...this.config.env },
        shell: true,
      });

      this.setupProcessHandlers();
      
      // 等待进程就绪
      await this.waitForReady();
      this.setState('ready');
    } catch (error) {
      this.setState('error');
      throw error;
    }
  }

  /** 设置进程事件处理 */
  protected setupProcessHandlers(): void {
    if (!this.process) return;

    this.process.stdout?.on('data', (data: Buffer) => {
      const chunk = data.toString();
      this.outputBuffer += chunk;
      this.emit('output', chunk);
      this.checkResponseComplete();
    });

    this.process.stderr?.on('data', (data: Buffer) => {
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
  protected async waitForReady(): Promise<void> {
    // 默认等待 1 秒
    return new Promise((resolve) => setTimeout(resolve, 1000));
  }

  /** 检查响应是否完成 */
  protected checkResponseComplete(): void {
    if (!this.currentResolve) return;

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
  async sendMessage(message: string): Promise<CLIResponse> {
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
  async interrupt(): Promise<void> {
    if (!this.process) return;

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
  async disconnect(): Promise<void> {
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
  protected clearTimeout(): void {
    if (this.timeoutHandle) {
      clearTimeout(this.timeoutHandle);
      this.timeoutHandle = null;
    }
  }
}
