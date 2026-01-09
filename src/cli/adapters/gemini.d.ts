/**
 * Gemini CLI 适配器
 *
 * Gemini CLI (Google) 使用独立进程模式，每次调用启动新进程。
 * 支持自动执行文件修改。
 * 支持 --resume 恢复之前的会话。
 *
 * 输出格式：JSONL 格式（使用 --output-format json）
 */
import { EventEmitter } from 'events';
import { ICLIAdapter, CLIType, CLIResponse, AdapterState, AdapterConfig, CLICapabilities } from '../types';
/**
 * Gemini CLI 适配器
 * 每次 sendMessage 启动新进程
 * 支持会话恢复功能
 */
export declare class GeminiAdapter extends EventEmitter implements ICLIAdapter {
    readonly type: CLIType;
    private config;
    private _state;
    private currentProcess;
    private sessionId;
    /**
     * 检查 Gemini CLI 是否已安装
     */
    static checkInstalled(): Promise<boolean>;
    constructor(config: Omit<AdapterConfig, 'type'>);
    get state(): AdapterState;
    get isConnected(): boolean;
    get isBusy(): boolean;
    /** 获取 CLI 能力 */
    get capabilities(): CLICapabilities;
    private setState;
    /** 连接（Gemini CLI 不需要持久连接） */
    connect(): Promise<void>;
    /** 断开连接 */
    disconnect(): Promise<void>;
    /** 发送消息（Gemini CLI 通过 read_file 工具读取图片，使用内置多模态能力分析） */
    sendMessage(message: string, imagePaths?: string[]): Promise<CLIResponse>;
    /** 中断当前操作 */
    interrupt(): Promise<void>;
    /** 构建命令行参数（Gemini CLI 不支持图片） */
    private buildArgs;
    /** 从输出中提取 session_id */
    private extractSessionId;
    /** 获取当前会话 ID */
    getSessionId(): string | null;
    /** 设置会话 ID（用于恢复之前的会话） */
    setSessionId(sessionId: string | null): void;
    /** 重置会话（开始新对话） */
    resetSession(): void;
    /** 解析 Gemini CLI stream-json 输出 */
    private parseOutput;
}
//# sourceMappingURL=gemini.d.ts.map