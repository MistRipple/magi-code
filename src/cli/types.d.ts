/**
 * CLI 适配器类型定义
 */
import { EventEmitter } from 'events';
/** CLI 类型 */
export type CLIType = 'claude' | 'codex' | 'gemini';
/** CLI 能力定义 */
export interface CLICapabilities {
    /** 是否支持图片输入 */
    supportsImage: boolean;
    /** 是否支持文件附件 */
    supportsFileAttachment: boolean;
    /** 是否支持流式输出 */
    supportsStreaming: boolean;
    /** 是否支持会话恢复 */
    supportsSessionResume: boolean;
    /** 是否支持代码执行 */
    supportsCodeExecution: boolean;
    /** 是否支持 Web 搜索 */
    supportsWebSearch: boolean;
    /** 最大上下文长度 */
    maxContextLength?: number;
    /** 图片参数名（如 --image） */
    imageParam?: string;
}
/** 预设 CLI 能力配置 */
export declare const CLI_CAPABILITIES: Record<CLIType, CLICapabilities>;
/** CLI 响应类型 */
export interface CLIResponse {
    /** 响应内容 */
    content: string;
    /** 是否完成 */
    done: boolean;
    /** 文件变更 */
    fileChanges?: FileChange[];
    /** 错误信息 */
    error?: string;
    /** 原始输出 */
    raw?: string;
}
/** 文件变更 */
export interface FileChange {
    /** 文件路径 */
    filePath: string;
    /** 变更类型 */
    type: 'create' | 'modify' | 'delete';
    /** 新内容 */
    content?: string;
    /** Diff 内容 */
    diff?: string;
    /** 添加行数 */
    additions?: number;
    /** 删除行数 */
    deletions?: number;
}
/** CLI 适配器状态 */
export type AdapterState = 'idle' | 'connecting' | 'ready' | 'busy' | 'error' | 'disconnected';
/** CLI 适配器事件 */
export interface AdapterEvents {
    /** 状态变更 */
    stateChange: (state: AdapterState) => void;
    /** 输出流 */
    output: (chunk: string) => void;
    /** 响应完成 */
    response: (response: CLIResponse) => void;
    /** 文件变更 */
    fileChange: (change: FileChange) => void;
    /** 错误 */
    error: (error: Error) => void;
}
/** CLI 适配器配置 */
export interface AdapterConfig {
    /** CLI 类型 */
    type: CLIType;
    /** 工作目录 */
    cwd: string;
    /** 命令路径 */
    command?: string;
    /** 命令参数 */
    args?: string[];
    /** 环境变量 */
    env?: Record<string, string>;
    /** 超时时间（毫秒） */
    timeout?: number;
}
/** CLI 适配器接口 */
export interface ICLIAdapter extends EventEmitter {
    /** CLI 类型 */
    readonly type: CLIType;
    /** 当前状态 */
    readonly state: AdapterState;
    /** 是否已连接 */
    readonly isConnected: boolean;
    /** 是否忙碌 */
    readonly isBusy: boolean;
    /** 连接到 CLI */
    connect(): Promise<void>;
    /** 断开连接 */
    disconnect(): Promise<void>;
    /** 发送消息（可选图片路径） */
    sendMessage(message: string, imagePaths?: string[]): Promise<CLIResponse>;
    /** 中断当前操作 */
    interrupt(): Promise<void>;
    /** 事件监听 */
    on<K extends keyof AdapterEvents>(event: K, listener: AdapterEvents[K]): this;
    off<K extends keyof AdapterEvents>(event: K, listener: AdapterEvents[K]): this;
    emit<K extends keyof AdapterEvents>(event: K, ...args: Parameters<AdapterEvents[K]>): boolean;
}
/** 会话消息 */
export interface SessionMessage {
    id: string;
    role: 'user' | 'assistant';
    content: string;
    cli?: CLIType;
    timestamp: number;
    fileChanges?: FileChange[];
    /** 附件（图片等） */
    attachments?: MessageAttachment[];
}
/** 消息附件 */
export interface MessageAttachment {
    type: 'image' | 'file';
    path: string;
    name: string;
    mimeType?: string;
}
/** 会话 */
export interface Session {
    id: string;
    name?: string;
    messages: SessionMessage[];
    createdAt: number;
    updatedAt: number;
    /** 各 CLI 的会话 ID */
    cliSessionIds?: Partial<Record<CLIType, string>>;
}
/** 会话元数据（用于列表显示） */
export interface SessionMeta {
    id: string;
    name?: string;
    messageCount: number;
    createdAt: number;
    updatedAt: number;
    /** 第一条用户消息的预览 */
    preview?: string;
}
//# sourceMappingURL=types.d.ts.map