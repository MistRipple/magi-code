/**
 * Snapshot Cleaner - 快照清理模块
 *
 * 职责：
 * - 清理过期快照
 * - 清理孤立快照
 * - 清理会话快照
 * - 磁盘空间管理
 */

import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../logging';
import { UnifiedSessionManager } from '../session';

export interface CleanupResult {
  deletedCount: number;
  freedSpace: number; // bytes
  errors: string[];
}

export interface CleanupOptions {
  /** 保留最近 N 个快照 */
  keepRecent?: number;
  /** 删除超过 N 天的快照 */
  olderThanDays?: number;
  /** 是否删除孤立快照（文件已不存在） */
  removeOrphaned?: boolean;
  /** 是否强制删除（即使有错误） */
  force?: boolean;
}

export class SnapshotCleaner {
  constructor(
    private sessionManager: UnifiedSessionManager,
    private workspaceRoot: string
  ) {}

  /**
   * 清理会话的所有快照
   */
  cleanupSession(sessionId: string, options: CleanupOptions = {}): CleanupResult {
    const result: CleanupResult = {
      deletedCount: 0,
      freedSpace: 0,
      errors: [],
    };

    try {
      const snapshots = this.sessionManager.getSession(sessionId)?.snapshots || [];
      const snapshotDir = this.sessionManager.getSnapshotsDir(sessionId);

      // 按时间排序（最新的在前）
      const sortedSnapshots = [...snapshots].sort((a, b) => b.lastModifiedAt - a.lastModifiedAt);

      for (let i = 0; i < sortedSnapshots.length; i++) {
        const snapshot = sortedSnapshots[i];

        // 检查是否应该保留
        if (options.keepRecent && i < options.keepRecent) {
          continue;
        }

        // 检查是否过期
        if (options.olderThanDays) {
          const age = Date.now() - snapshot.lastModifiedAt;
          const days = age / (1000 * 60 * 60 * 24);
          if (days < options.olderThanDays) {
            continue;
          }
        }

        // 检查是否是孤立快照
        if (options.removeOrphaned) {
          const fullPath = path.resolve(this.workspaceRoot, snapshot.filePath);
          if (fs.existsSync(fullPath)) {
            continue; // 文件仍存在，不删除
          }
        }

        // 删除快照
        const snapshotFile = path.join(snapshotDir, `${snapshot.id}.snapshot`);
        try {
          if (fs.existsSync(snapshotFile)) {
            const stats = fs.statSync(snapshotFile);
            fs.unlinkSync(snapshotFile);
            result.freedSpace += stats.size;
            result.deletedCount++;
          }

          // 从元数据中移除
          this.sessionManager.removeSnapshot(sessionId, snapshot.filePath);
        } catch (error: any) {
          const errorMsg = `删除快照失败: ${snapshot.id} - ${error.message}`;
          result.errors.push(errorMsg);
          logger.error('快照.清理失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);

          if (!options.force) {
            break; // 非强制模式下，遇到错误停止
          }
        }
      }

      logger.info('快照.清理完成', {
        sessionId,
        deletedCount: result.deletedCount,
        freedSpace: result.freedSpace,
      }, LogCategory.RECOVERY);
    } catch (error: any) {
      result.errors.push(`清理会话快照失败: ${error.message}`);
      logger.error('快照.清理会话失败', { sessionId, error }, LogCategory.RECOVERY);
    }

    return result;
  }

  /**
   * 清理所有会话的快照
   */
  cleanupAllSessions(options: CleanupOptions = {}): CleanupResult {
    const totalResult: CleanupResult = {
      deletedCount: 0,
      freedSpace: 0,
      errors: [],
    };

    try {
      const sessions = this.sessionManager.getAllSessions();

      for (const session of sessions) {
        const result = this.cleanupSession(session.id, options);
        totalResult.deletedCount += result.deletedCount;
        totalResult.freedSpace += result.freedSpace;
        totalResult.errors.push(...result.errors);
      }

      logger.info('快照.全局清理完成', {
        sessionsCount: sessions.length,
        deletedCount: totalResult.deletedCount,
        freedSpace: totalResult.freedSpace,
      }, LogCategory.RECOVERY);
    } catch (error: any) {
      totalResult.errors.push(`清理所有会话失败: ${error.message}`);
      logger.error('快照.全局清理失败', { error }, LogCategory.RECOVERY);
    }

    return totalResult;
  }

  /**
   * 清理孤立快照（文件已不存在的快照）
   */
  cleanupOrphanedSnapshots(sessionId: string): CleanupResult {
    return this.cleanupSession(sessionId, {
      removeOrphaned: true,
      force: true,
    });
  }

  /**
   * 清理过期快照
   */
  cleanupExpiredSnapshots(sessionId: string, days: number): CleanupResult {
    return this.cleanupSession(sessionId, {
      olderThanDays: days,
      force: true,
    });
  }

  /**
   * 获取快照占用的磁盘空间
   */
  getSnapshotDiskUsage(sessionId: string): {
    totalSize: number;
    snapshotCount: number;
    averageSize: number;
  } {
    let totalSize = 0;
    let snapshotCount = 0;

    try {
      const snapshotDir = this.sessionManager.getSnapshotsDir(sessionId);

      if (fs.existsSync(snapshotDir)) {
        const files = fs.readdirSync(snapshotDir);

        for (const file of files) {
          if (file.endsWith('.snapshot')) {
            const filePath = path.join(snapshotDir, file);
            try {
              const stats = fs.statSync(filePath);
              totalSize += stats.size;
              snapshotCount++;
            } catch (error) {
              // 忽略无法访问的文件
            }
          }
        }
      }
    } catch (error: any) {
      logger.error('快照.磁盘使用统计失败', { sessionId, error }, LogCategory.RECOVERY);
    }

    return {
      totalSize,
      snapshotCount,
      averageSize: snapshotCount > 0 ? totalSize / snapshotCount : 0,
    };
  }

  /**
   * 清理空的快照目录
   */
  cleanupEmptyDirectories(sessionId: string): number {
    let deletedCount = 0;

    try {
      const snapshotDir = this.sessionManager.getSnapshotsDir(sessionId);

      if (fs.existsSync(snapshotDir)) {
        const files = fs.readdirSync(snapshotDir);

        if (files.length === 0) {
          fs.rmdirSync(snapshotDir);
          deletedCount++;
          logger.info('快照.删除空目录', { snapshotDir }, LogCategory.RECOVERY);
        }
      }
    } catch (error: any) {
      logger.error('快照.清理空目录失败', { sessionId, error }, LogCategory.RECOVERY);
    }

    return deletedCount;
  }
}
