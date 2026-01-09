/**
 * CLI 适配器工厂
 * 统一管理和创建 CLI 适配器实例
 */
import { EventEmitter } from 'events';
import { ICLIAdapter, CLIType, CLIResponse } from './types';
/** 适配器状态信息 */
export interface AdapterStatus {
    type: CLIType;
    connected: boolean;
    busy: boolean;
    state: string;
    installed?: boolean;
}
/** 工厂配置 */
export interface FactoryConfig {
    cwd: string;
    timeout?: number;
    env?: Record<string, string>;
}
/**
 * CLI 适配器工厂
 * 提供统一的适配器创建、管理和事件转发
 */
export declare class CLIAdapterFactory extends EventEmitter {
    private adapters;
    private config;
    constructor(config: FactoryConfig);
    /**
     * 创建或获取适配器实例
     */
    create(type: CLIType): ICLIAdapter;
    /**
     * 设置适配器事件转发
     */
    private setupAdapterEvents;
    /**
     * 获取已创建的适配器
     */
    getAdapter(type: CLIType): ICLIAdapter | undefined;
    /**
     * 检查 CLI 是否可用（已创建且已连接）
     */
    isAvailable(type: CLIType): boolean;
    /**
     * 获取或创建适配器
     */
    getOrCreate(type: CLIType): ICLIAdapter;
    /**
     * 获取所有已创建的适配器
     */
    getAllAdapters(): ICLIAdapter[];
    /**
     * 获取所有适配器状态
     */
    getAllStatus(): AdapterStatus[];
    /**
     * 获取所有已连接的适配器
     */
    getConnectedAdapters(): ICLIAdapter[];
    /**
     * 获取所有可用（已连接且不忙）的适配器
     */
    getAvailableAdapters(): ICLIAdapter[];
    /**
     * 连接指定类型的适配器
     */
    connect(type: CLIType): Promise<ICLIAdapter>;
    /**
     * 连接所有适配器
     */
    connectAll(): Promise<void>;
    /**
     * 检查所有 CLI 的安装状态（轻量检测，不启动进程）
     */
    checkAllAvailability(): Promise<Record<CLIType, boolean>>;
    /**
     * 断开指定类型的适配器
     */
    disconnect(type: CLIType): Promise<void>;
    /**
     * 断开所有适配器
     */
    disconnectAll(): Promise<void>;
    /**
     * 发送消息到指定 CLI
     * 如果目标 CLI 不支持图片或处于会话恢复模式，会先用 Codex 描述图片
     */
    sendMessage(type: CLIType, message: string, imagePaths?: string[]): Promise<CLIResponse>;
    /**
     * 判断是否需要用 Codex 描述图片
     * @returns true 如果需要描述图片
     */
    private shouldDescribeImages;
    /**
     * 中断指定 CLI 的执行
     */
    interrupt(type: CLIType): Promise<void>;
    /**
     * 中断所有 CLI 的执行
     */
    interruptAll(): Promise<void>;
    /**
     * 获取指定 CLI 的会话 ID
     */
    getSessionId(type: CLIType): string | null;
    /**
     * 设置指定 CLI 的会话 ID
     */
    setSessionId(type: CLIType, sessionId: string | null): void;
    /**
     * 重置指定 CLI 的会话
     */
    resetSession(type: CLIType): void;
    /**
     * 重置所有 CLI 的会话
     */
    resetAllSessions(): void;
    /**
     * 获取所有 CLI 的会话 ID
     */
    getAllSessionIds(): {
        claude?: string;
        codex?: string;
        gemini?: string;
    };
    /**
     * 设置所有 CLI 的会话 ID
     */
    setAllSessionIds(sessionIds: {
        claude?: string;
        codex?: string;
        gemini?: string;
    }): void;
    /**
     * 销毁工厂，清理所有资源
     */
    dispose(): Promise<void>;
}
//# sourceMappingURL=adapter-factory.d.ts.map