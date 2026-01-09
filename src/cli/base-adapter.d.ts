/**
 * CLI 适配器基类
 * 提供通用的进程管理和输出解析功能
 */
import { EventEmitter } from 'events';
import { ChildProcess } from 'child_process';
import { ICLIAdapter, CLIType, CLIResponse, AdapterState, AdapterConfig } from './types';
/**
 * CLI 适配器基类
 */
export declare abstract class BaseCLIAdapter extends EventEmitter implements ICLIAdapter {
    readonly type: CLIType;
    protected config: AdapterConfig;
    protected process: ChildProcess | null;
    protected outputBuffer: string;
    protected _state: AdapterState;
    protected currentResolve: ((response: CLIResponse) => void) | null;
    protected currentReject: ((error: Error) => void) | null;
    protected timeoutHandle: NodeJS.Timeout | null;
    constructor(config: AdapterConfig);
    get state(): AdapterState;
    get isConnected(): boolean;
    get isBusy(): boolean;
    protected setState(state: AdapterState): void;
    /** 获取 CLI 命令和参数（子类实现） */
    protected abstract getCommand(): string;
    protected abstract getArgs(): string[];
    /** 解析 CLI 输出（子类实现） */
    protected abstract parseOutput(output: string): CLIResponse;
    /** 检测响应是否完成（子类实现） */
    protected abstract isResponseComplete(output: string): boolean;
    /** 连接到 CLI */
    connect(): Promise<void>;
    /** 设置进程事件处理 */
    protected setupProcessHandlers(): void;
    /** 等待 CLI 就绪（子类可覆盖） */
    protected waitForReady(): Promise<void>;
    /** 检查响应是否完成 */
    protected checkResponseComplete(): void;
    /** 发送消息 */
    sendMessage(message: string): Promise<CLIResponse>;
    /** 中断当前操作 */
    interrupt(): Promise<void>;
    /** 断开连接 */
    disconnect(): Promise<void>;
    /** 清除超时 */
    protected clearTimeout(): void;
}
//# sourceMappingURL=base-adapter.d.ts.map