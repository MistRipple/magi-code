/**
 * 快照管理器
 * 文件快照创建、存储、还原
 */
import { FileSnapshot, CLIType, PendingChange } from './types';
import { SessionManager } from './session-manager';
/**
 * 快照管理器
 */
export declare class SnapshotManager {
    private sessionManager;
    private workspaceRoot;
    private snapshotDir;
    constructor(sessionManager: SessionManager, workspaceRoot: string);
    /** 确保快照目录存在 */
    private ensureSnapshotDir;
    /** 创建文件快照 */
    createSnapshot(filePath: string, modifiedBy: CLIType, subTaskId: string): FileSnapshot | null;
    /** 保存快照内容到文件 */
    private saveSnapshotContent;
    /** 还原文件到快照状态 */
    revertToSnapshot(filePath: string): boolean;
    /** 获取待处理变更列表 */
    getPendingChanges(): PendingChange[];
    /** 计算变更行数 */
    private countChanges;
    /** 接受变更（删除快照，保留当前文件） */
    acceptChange(filePath: string): boolean;
    /** 批量接受所有变更 */
    acceptAllChanges(): number;
    /** 批量还原所有变更 */
    revertAllChanges(): number;
    /** 为多个文件创建快照 */
    createSnapshots(filePaths: string[], modifiedBy: CLIType, subTaskId: string): FileSnapshot[];
    /** 清理会话的所有快照 */
    cleanupSession(sessionId: string): void;
}
//# sourceMappingURL=snapshot-manager.d.ts.map