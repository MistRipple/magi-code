/**
 * Atomic Operations - 原子操作模块
 *
 * 职责：
 * - 原子性写入快照
 * - 原子性删除快照
 * - 操作锁管理
 * - 回滚机制
 */

import * as fs from 'fs';
import { logger, LogCategory } from '../logging';
import { UnifiedSessionManager, FileSnapshotMeta } from '../session';

/**
 * 快照操作结果
 */
export interface SnapshotOperationResult {
  success: boolean;
  snapshotId?: string;
  error?: string;
}

export class AtomicOperations {
  private operationLocks: Set<string> = new Set();

  constructor(
    private sessionManager: UnifiedSessionManager
  ) {}

  /**
   * 获取操作锁
   */
  acquireLock(key: string): boolean {
    if (this.operationLocks.has(key)) {
      return false;
    }
    this.operationLocks.add(key);
    return true;
  }

  /**
   * 释放操作锁
   */
  releaseLock(key: string): void {
    this.operationLocks.delete(key);
  }

  /**
   * 原子性写入快照（写文件 + 更新元数据）
   * 如果任何步骤失败，回滚所有更改
   */
  atomicWriteSnapshot(
    sessionId: string,
    snapshotId: string,
    snapshotFile: string,
    content: string,
    meta: FileSnapshotMeta,
    ensureDir: () => void,
    addToCache: (snapshotFile: string, content: string) => void
  ): SnapshotOperationResult {
    const lockKey = `snapshot:${sessionId}:${meta.filePath}`;

    // 尝试获取锁
    if (!this.acquireLock(lockKey)) {
      return {
        success: false,
        error: `Operation in progress for file: ${meta.filePath}`,
      };
    }

    try {
      // 步骤 1: 写入快照文件
      ensureDir();
      fs.writeFileSync(snapshotFile, content, 'utf-8');

      // 步骤 2: 更新元数据
      try {
        this.sessionManager.addSnapshot(sessionId, meta);
      } catch (metaError) {
        // 元数据更新失败，回滚文件写入
        try {
          fs.unlinkSync(snapshotFile);
        } catch (rollbackError) {
          logger.error('快照.原子操作.回滚失败', { snapshotFile }, LogCategory.RECOVERY);
        }
        throw metaError;
      }

      // 步骤 3: 更新缓存
      addToCache(snapshotFile, content);

      return { success: true, snapshotId };
    } catch (error: any) {
      logger.error('快照.原子操作.失败', { snapshotId, error }, LogCategory.RECOVERY);
      return {
        success: false,
        error: error instanceof Error ? error.message : String(error),
      };
    } finally {
      this.releaseLock(lockKey);
    }
  }

  /**
   * 原子性删除快照（删除文件 + 更新元数据）
   * 如果任何步骤失败，回滚所有更改
   */
  atomicDeleteSnapshot(
    sessionId: string,
    snapshotId: string,
    snapshotFile: string,
    filePath: string,
    invalidateCache: (snapshotFile: string) => void
  ): SnapshotOperationResult {
    const lockKey = `snapshot:${sessionId}:${filePath}`;

    // 尝试获取锁
    if (!this.acquireLock(lockKey)) {
      return {
        success: false,
        error: `Operation in progress for file: ${filePath}`,
      };
    }

    let backupContent: string | null = null;

    try {
      // 步骤 1: 备份快照内容（用于回滚）
      if (fs.existsSync(snapshotFile)) {
        try {
          backupContent = fs.readFileSync(snapshotFile, 'utf-8');
        } catch (readError) {
          // 读取失败，继续删除
          logger.warn('快照.原子删除.备份失败', { snapshotFile }, LogCategory.RECOVERY);
        }
      }

      // 步骤 2: 删除快照文件
      if (fs.existsSync(snapshotFile)) {
        fs.unlinkSync(snapshotFile);
      }

      // 步骤 3: 更新元数据
      try {
        this.sessionManager.removeSnapshot(sessionId, filePath);
      } catch (metaError) {
        // 元数据更新失败，回滚文件删除
        if (backupContent !== null) {
          try {
            fs.writeFileSync(snapshotFile, backupContent, 'utf-8');
          } catch (rollbackError) {
            logger.error('快照.原子删除.回滚失败', { snapshotFile }, LogCategory.RECOVERY);
          }
        }
        throw metaError;
      }

      // 步骤 4: 清除缓存
      invalidateCache(snapshotFile);

      return { success: true, snapshotId };
    } catch (error: any) {
      logger.error('快照.原子删除.失败', { snapshotId, error }, LogCategory.RECOVERY);
      return {
        success: false,
        error: error instanceof Error ? error.message : String(error),
      };
    } finally {
      this.releaseLock(lockKey);
    }
  }
}
