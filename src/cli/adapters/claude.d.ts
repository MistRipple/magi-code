/**
 * Claude CLI 适配器
 *
 * Claude CLI 使用 --print --output-format json 模式，每次调用是独立进程。
 * 使用 --continue 或 --resume <session_id> 来继续之前的会话。
 *
 * JSON 输出格式：
 * - {"type":"system","subtype":"init",...} - 初始化信息
 * - {"type":"assistant","message":{...},...} - 助手响应
 * - {"type":"result","subtype":"success",...} - 最终结果
 */
import { EventEmitter } from 'events';
import { ICLIAdapter, CLIType, CLIResponse, AdapterState, AdapterConfig, CLICapabilities } from '../types';
/**
 * Claude CLI 适配器
 * 每次 sendMessage 启动新进程，使用 session_id 保持会话连续性
 */
export declare class ClaudeAdapter extends EventEmitter implements ICLIAdapter {
    readonly type: CLIType;
    private config;
    private _state;
    private sessionId;
    private currentProcess;
    private _installed;
    /**
     * 检查 Claude CLI 是否已安装
     */
    static checkInstalled(): Promise<boolean>;
    constructor(config: Omit<AdapterConfig, 'type'>);
    get state(): AdapterState;
    get isConnected(): boolean;
    get isBusy(): boolean;
    /** 获取 CLI 能力 */
    get capabilities(): CLICapabilities;
    private setState;
    /** 连接（Claude CLI 不需要持久连接） */
    connect(): Promise<void>;
    /** 断开连接 */
    disconnect(): Promise<void>;
    /** 发送消息（Claude CLI 通过 Read 工具读取图片，在 prompt 中引用路径） */
    sendMessage(message: string, imagePaths?: string[]): Promise<CLIResponse>;
    /** 中断当前操作 - 🆕 增强版：添加超时机制和强制kill */
    interrupt(): Promise<void>;
    /** 构建命令行参数（Claude CLI 不支持图片） */
    private buildArgs;
    /** 从输出中提取 session_id */
    private extractSessionId;
    /** 解析 Claude CLI 输出 */
    private parseOutput;
    /** 获取当前会话 ID */
    getSessionId(): string | null;
    /** 设置会话 ID（用于恢复之前的会话） */
    setSessionId(sessionId: string | null): void;
    /** 重置会话（开始新对话） */
    resetSession(): void;
}
//# sourceMappingURL=claude.d.ts.map