/**
 * 会话存储
 * 处理会话的持久化存储
 */
import * as vscode from 'vscode';
import { Session, SessionMeta } from '../cli/types';
/**
 * 会话存储类
 */
export declare class SessionStorage {
    private context;
    constructor(context: vscode.ExtensionContext);
    /**
     * 保存会话
     */
    save(session: Session): Promise<void>;
    /**
     * 加载会话
     */
    load(id: string): Promise<Session | undefined>;
    /**
     * 删除会话
     */
    delete(id: string): Promise<void>;
    /**
     * 获取会话列表（元数据）
     */
    list(): Promise<SessionMeta[]>;
    /**
     * 更新会话列表
     */
    private updateSessionList;
    /**
     * 清空所有会话
     */
    clear(): Promise<void>;
    /**
     * 获取会话数量
     */
    count(): Promise<number>;
}
//# sourceMappingURL=storage.d.ts.map