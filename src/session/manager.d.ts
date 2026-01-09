/**
 * 会话管理器
 * 协调 CLI 适配器和会话存储
 */
import * as vscode from 'vscode';
import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { Session, SessionMessage, SessionMeta, CLIType, CLIResponse, ICLIAdapter } from '../cli/types';
/** 会话管理器事件 */
export interface SessionManagerEvents {
    sessionCreated: (session: Session) => void;
    sessionSwitched: (session: Session) => void;
    sessionUpdated: (session: Session) => void;
    sessionDeleted: (id: string) => void;
    messageAdded: (message: SessionMessage) => void;
    cliOutput: (cli: CLIType, chunk: string) => void;
    cliResponse: (cli: CLIType, response: CLIResponse) => void;
    error: (error: Error) => void;
}
/**
 * 会话管理器
 */
export declare class SessionManager extends EventEmitter {
    private factory;
    private storage;
    private currentSession;
    private sessions;
    constructor(context: vscode.ExtensionContext, cwd: string);
    /** 获取当前会话 */
    get current(): Session | null;
    /** 获取当前会话 ID */
    get currentId(): string | null;
    /** 初始化，加载会话列表 */
    initialize(): Promise<void>;
    /** 创建新会话 */
    createSession(name?: string): Promise<Session>;
    /** 切换会话 */
    switchSession(id: string): Promise<Session | null>;
    /** 重命名会话 */
    renameSession(id: string, name: string): Promise<void>;
    /** 删除会话 */
    deleteSession(id: string): Promise<void>;
    /** 获取会话列表 */
    listSessions(): Promise<SessionMeta[]>;
    /** 发送消息到 CLI */
    sendMessage(content: string, cli?: CLIType): Promise<CLIResponse>;
    /** 添加消息到当前会话 */
    private addMessage;
    /** 中断当前 CLI 操作 */
    interrupt(cli?: CLIType): Promise<void>;
    /** 获取 CLI 适配器 */
    getAdapter(cli: CLIType): ICLIAdapter | undefined;
    /** 获取适配器工厂 */
    getFactory(): CLIAdapterFactory;
    /** 销毁管理器 */
    dispose(): Promise<void>;
    /** 事件监听类型 */
    on<K extends keyof SessionManagerEvents>(event: K, listener: SessionManagerEvents[K]): this;
    off<K extends keyof SessionManagerEvents>(event: K, listener: SessionManagerEvents[K]): this;
    emit<K extends keyof SessionManagerEvents>(event: K, ...args: Parameters<SessionManagerEvents[K]>): boolean;
}
//# sourceMappingURL=manager.d.ts.map