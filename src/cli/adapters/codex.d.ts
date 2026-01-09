/**
 * Codex CLI 适配器
 *
 * Codex CLI (OpenAI) 使用独立进程模式，每次调用启动新进程。
 * 支持 --full-auto 模式自动执行文件修改。
 * 支持 exec resume 恢复之前的会话。
 *
 * 输出格式：JSONL 格式
 */
import { EventEmitter } from 'events';
import { ICLIAdapter, CLIType, CLIResponse, AdapterState, AdapterConfig, CLICapabilities } from '../types';
/**
 * Codex CLI 适配器
 * 每次 sendMessage 启动新进程
 * 支持会话恢复功能
 */
export declare class CodexAdapter extends EventEmitter implements ICLIAdapter {
    readonly type: CLIType;
    private config;
    private _state;
    private currentProcess;
    private sessionId;
    /**
     * 检查 Codex CLI 是否已安装
     */
    static checkInstalled(): Promise<boolean>;
    /**
     * 使用 Codex CLI 描述图片内容
     *
     * ⚠️ 注意：此方法现在仅用于以下场景：
     * - Codex 会话恢复模式（exec resume 不支持 -i 参数）
     *
     * Claude 和 Gemini CLI 已原生支持图片识别：
     * - Claude: 通过 Read 工具 + analyze_image MCP 工具
     * - Gemini: 通过 read_file 工具 + 内置多模态能力
     *
     * @param imagePaths 图片路径数组
     * @param cwd 工作目录
     * @param timeout 超时时间（毫秒）
     * @returns 图片描述文本
     */
    static describeImages(imagePaths: string[], cwd?: string, timeout?: number): Promise<string>;
    /**
     * 从 Codex 输出中提取描述文本
     */
    private static extractDescriptionFromOutput;
    constructor(config: Omit<AdapterConfig, 'type'>);
    get state(): AdapterState;
    get isConnected(): boolean;
    get isBusy(): boolean;
    /** 获取 CLI 能力 */
    get capabilities(): CLICapabilities;
    private setState;
    /** 连接（Codex CLI 不需要持久连接） */
    connect(): Promise<void>;
    /** 断开连接 */
    disconnect(): Promise<void>;
    /** 发送消息（支持图片） */
    sendMessage(message: string, imagePaths?: string[]): Promise<CLIResponse>;
    /** 中断当前操作 */
    interrupt(): Promise<void>;
    /** 构建命令行参数 */
    private buildArgs;
    /** 从输出中提取 session_id (thread_id) */
    private extractSessionId;
    /** 获取当前会话 ID */
    getSessionId(): string | null;
    /** 设置会话 ID（用于恢复之前的会话） */
    setSessionId(sessionId: string | null): void;
    /** 重置会话（开始新对话） */
    resetSession(): void;
    /** 解析 Codex CLI 输出（支持 JSONL 和纯文本两种格式） */
    private parseOutput;
}
//# sourceMappingURL=codex.d.ts.map