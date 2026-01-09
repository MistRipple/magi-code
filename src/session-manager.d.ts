/**
 * Session 管理器
 * 管理 Session 生命周期、持久化
 */
import { Session, Task, FileSnapshot } from './types';
/**
 * Session 管理器
 */
export declare class SessionManager {
    private sessions;
    private currentSessionId;
    private storageDir;
    constructor(workspaceRoot: string);
    /** 确保存储目录存在 */
    private ensureStorageDir;
    /** 创建新 Session */
    createSession(): Session;
    /** 获取当前 Session */
    getCurrentSession(): Session | null;
    /** 获取或创建当前 Session */
    getOrCreateCurrentSession(): Session;
    /** 切换 Session */
    switchSession(sessionId: string): Session | null;
    /** 获取 Session */
    getSession(sessionId: string): Session | null;
    /** 获取所有 Session */
    getAllSessions(): Session[];
    /** 结束 Session */
    endSession(sessionId: string): void;
    /** 添加 Task 到 Session */
    addTask(sessionId: string, task: Task): void;
    /** 更新 Task */
    updateTask(sessionId: string, taskId: string, updates: Partial<Task>): void;
    /** 添加快照到 Session */
    addSnapshot(sessionId: string, snapshot: FileSnapshot): void;
    /** 获取文件快照 */
    getSnapshot(sessionId: string, filePath: string): FileSnapshot | null;
    /** 移除文件快照 */
    removeSnapshot(sessionId: string, filePath: string): boolean;
    /** 保存 Session 到文件 */
    private saveSession;
    /** 保存当前 Session（公开方法，供外部调用） */
    saveCurrentSession(): void;
    /** 从文件加载 Session */
    loadSession(sessionId: string): Session | null;
    /** 加载所有 Session */
    loadAllSessions(): void;
    /** 删除 Session */
    deleteSession(sessionId: string): void;
}
//# sourceMappingURL=session-manager.d.ts.map