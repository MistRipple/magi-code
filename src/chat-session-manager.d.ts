/**
 * 对话会话管理器
 * 管理对话历史、会话切换、持久化
 */
import { CLIType } from './types';
import { Session, SessionMessage, SessionMeta } from './cli/types';
/**
 * 对话会话管理器
 * 专门管理聊天对话历史
 */
export declare class ChatSessionManager {
    private sessions;
    private currentSessionId;
    private storageDir;
    constructor(workspaceRoot: string);
    /** 确保存储目录存在 */
    private ensureStorageDir;
    /** 创建新会话 */
    createSession(name?: string): Session;
    /** 获取当前会话 */
    getCurrentSession(): Session | null;
    /** 获取或创建当前会话 */
    getOrCreateCurrentSession(): Session;
    /** 切换会话 */
    switchSession(sessionId: string): Session | null;
    /** 获取会话 */
    getSession(sessionId: string): Session | null;
    /** 获取所有会话（按更新时间倒序） */
    getAllSessions(): Session[];
    /** 获取会话元数据列表（用于 UI 显示） */
    getSessionMetas(): SessionMeta[];
    /** 获取会话预览（第一条用户消息的前 50 个字符） */
    private getSessionPreview;
    /** 添加消息到当前会话 */
    addMessage(role: 'user' | 'assistant', content: string, cli?: CLIType): SessionMessage;
    /**
     * 生成会话标题 - 参考 Augment 风格的智能命名
     * 1. 移除冗余词汇（帮我、请、能不能等）
     * 2. 提取关键动词+对象
     * 3. 识别代码相关名称（函数名、文件名等）
     * 4. 智能截断，不切断单词
     */
    private generateSessionTitle;
    /** 重命名会话 */
    renameSession(sessionId: string, name: string): boolean;
    /** 更新会话数据（从前端同步） */
    updateSessionData(sessionId: string, messages: SessionMessage[], cliOutputs?: Record<string, any[]>): boolean;
    /** 保存当前会话 */
    saveCurrentSession(): void;
    /** 更新 CLI 会话 ID */
    updateCliSessionId(cli: CLIType, cliSessionId: string | null): void;
    /** 获取 CLI 会话 ID */
    getCliSessionId(cli: CLIType): string | undefined;
    /** 删除会话 */
    deleteSession(sessionId: string): boolean;
    /** 保存会话到文件 */
    private saveSession;
    /** 从文件加载会话 */
    private loadSession;
    /** 加载所有会话 */
    private loadAllSessions;
    /** 获取当前会话 ID */
    getCurrentSessionId(): string | null;
    /** 清空当前会话的消息（但保留会话） */
    clearCurrentSessionMessages(): void;
}
//# sourceMappingURL=chat-session-manager.d.ts.map