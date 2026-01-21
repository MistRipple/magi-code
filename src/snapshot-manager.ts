/**
 * 快照管理器
 * 文件快照创建、存储、还原
 *
 * 存储路径：.multicli/sessions/{sessionId}/snapshots/
 */

import { logger, LogCategory } from './logging';
import * as fs from 'fs';
import * as path from 'path';
import { FileSnapshot, PendingChange } from './types';
import { AgentType } from './types/agent-types';
import { UnifiedSessionManager, FileSnapshotMeta } from './session';
import { globalEventBus } from './events';

/** 生成唯一 ID */
function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * 快照操作结果
 */
interface SnapshotOperationResult {
  success: boolean;
  snapshotId?: string;
  error?: string;
}

/**
 * 快照管理器
 */
export class SnapshotManager {
  private sessionManager: UnifiedSessionManager;
  private workspaceRoot: string;

  // 文件内容缓存（优化重复 I/O）
  private fileContentCache: Map<string, string> = new Map();
  private snapshotContentCache: Map<string, string> = new Map();
  private readonly MAX_CACHE_SIZE = 100; // 最大缓存条目数

  // 操作锁（防止并发写入冲突）
  private operationLocks: Set<string> = new Set();

  constructor(sessionManager: UnifiedSessionManager, workspaceRoot: string) {
    this.sessionManager = sessionManager;
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 获取操作锁
   * @returns 是否成功获取锁
   */
  private acquireLock(key: string): boolean {
    if (this.operationLocks.has(key)) {
      return false;
    }
    this.operationLocks.add(key);
    return true;
  }

  /**
   * 释放操作锁
   */
  private releaseLock(key: string): void {
    this.operationLocks.delete(key);
  }

  /**
   * 原子性写入快照（写文件 + 更新元数据）
   * 如果任何步骤失败，回滚所有更改
   */
  private atomicWriteSnapshot(
    sessionId: string,
    snapshotId: string,
    snapshotFile: string,
    content: string,
    meta: FileSnapshotMeta
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
      this.ensureSnapshotDir(sessionId);
      fs.writeFileSync(snapshotFile, content, 'utf-8');

      // 步骤 2: 更新元数据
      try {
        this.sessionManager.addSnapshot(sessionId, meta);
      } catch (metaError) {
        // 元数据更新失败，回滚文件写入
        try {
          fs.unlinkSync(snapshotFile);
        } catch {
          // 回滚失败也记录日志
          logger.error('快照.原子操作.回滚失败', { snapshotFile }, LogCategory.RECOVERY);
        }
        throw metaError;
      }

      // 步骤 3: 更新缓存
      this.addToCache(this.snapshotContentCache, snapshotFile, content);

      return { success: true, snapshotId };
    } catch (error) {
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
   */
  private atomicDeleteSnapshot(
    sessionId: string,
    snapshotId: string,
    snapshotFile: string,
    filePath: string
  ): SnapshotOperationResult {
    const lockKey = `snapshot:${sessionId}:${filePath}`;

    if (!this.acquireLock(lockKey)) {
      return {
        success: false,
        error: `Operation in progress for file: ${filePath}`,
      };
    }

    // 备份内容以便回滚
    let backupContent: string | null = null;
    if (fs.existsSync(snapshotFile)) {
      try {
        backupContent = fs.readFileSync(snapshotFile, 'utf-8');
      } catch {
        // 读取失败继续，不阻塞删除
      }
    }

    try {
      // 步骤 1: 删除快照文件
      if (fs.existsSync(snapshotFile)) {
        fs.unlinkSync(snapshotFile);
      }

      // 步骤 2: 移除元数据
      try {
        this.sessionManager.removeSnapshot(sessionId, filePath);
      } catch (metaError) {
        // 元数据删除失败，尝试恢复文件
        if (backupContent !== null) {
          try {
            fs.writeFileSync(snapshotFile, backupContent, 'utf-8');
          } catch {
            logger.error('快照.原子删除.回滚失败', { snapshotFile }, LogCategory.RECOVERY);
          }
        }
        throw metaError;
      }

      // 步骤 3: 清除缓存
      this.invalidateSnapshotCache(snapshotFile);

      return { success: true, snapshotId };
    } catch (error) {
      logger.error('快照.原子删除.失败', { snapshotId, error }, LogCategory.RECOVERY);
      return {
        success: false,
        error: error instanceof Error ? error.message : String(error),
      };
    } finally {
      this.releaseLock(lockKey);
    }
  }

  /** 获取快照目录（基于会话） */
  private getSnapshotDir(sessionId: string): string {
    return this.sessionManager.getSnapshotsDir(sessionId);
  }

  /** 确保快照目录存在 */
  private ensureSnapshotDir(sessionId: string): void {
    const dir = this.getSnapshotDir(sessionId);
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }
  }

  /** 读取文件内容（带缓存） */
  private readFileWithCache(filePath: string): string {
    if (this.fileContentCache.has(filePath)) {
      return this.fileContentCache.get(filePath)!;
    }

    let content = '';
    if (fs.existsSync(filePath)) {
      content = fs.readFileSync(filePath, 'utf-8');
      this.addToCache(this.fileContentCache, filePath, content);
    }
    return content;
  }

  /** 读取快照文件内容（带缓存） */
  private readSnapshotWithCache(snapshotFilePath: string): string {
    if (this.snapshotContentCache.has(snapshotFilePath)) {
      return this.snapshotContentCache.get(snapshotFilePath)!;
    }

    let content = '';
    if (fs.existsSync(snapshotFilePath)) {
      content = fs.readFileSync(snapshotFilePath, 'utf-8');
      this.addToCache(this.snapshotContentCache, snapshotFilePath, content);
    }
    return content;
  }

  /** 添加到缓存（带大小限制，LRU策略） */
  private addToCache(cache: Map<string, string>, key: string, value: string): void {
    // 如果缓存已满，删除最早的条目（Map 保持插入顺序）
    if (cache.size >= this.MAX_CACHE_SIZE) {
      const firstKey = cache.keys().next().value;
      if (firstKey) {
        cache.delete(firstKey);
      }
    }
    cache.set(key, value);
  }

  /** 清除文件缓存 */
  private invalidateFileCache(filePath: string): void {
    this.fileContentCache.delete(filePath);
  }

  /** 清除快照缓存 */
  private invalidateSnapshotCache(snapshotFilePath: string): void {
    this.snapshotContentCache.delete(snapshotFilePath);
  }

  /** 清除所有缓存 */
  clearCache(): void {
    this.fileContentCache.clear();
    this.snapshotContentCache.clear();
  }

  /** 清理指定文件的历史快照（避免旧任务阻塞新任务） */
  clearSnapshotsForFiles(filePaths: string[], keepSubTaskId?: string): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session || filePaths.length === 0) return 0;

    const normalizedRoot = path.normalize(this.workspaceRoot);
    const targets = new Set<string>();
    for (const filePath of filePaths) {
      const absolutePath = path.resolve(this.workspaceRoot, filePath);
      if (!absolutePath.startsWith(normalizedRoot)) continue;
      targets.add(path.relative(this.workspaceRoot, absolutePath));
    }
    if (targets.size === 0) return 0;

    let removed = 0;
    for (const snapshot of [...session.snapshots]) {
      if (!targets.has(snapshot.filePath)) continue;
      if (keepSubTaskId && snapshot.subTaskId === keepSubTaskId) continue;

      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.清理.文件.失败', { filePath: snapshot.filePath, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshot(session.id, snapshot.filePath);
      removed++;
    }

    if (removed > 0) {
      logger.info('快照.清理.目标文件', { count: removed }, LogCategory.RECOVERY);
    }
    return removed;
  }

  /** 验证快照完整性（检查元数据与文件是否一致） */
  validateSnapshotIntegrity(sessionId: string): {
    valid: number;
    orphaned: string[];
    missing: string[];
  } {
    const session = this.sessionManager.getSession(sessionId);
    if (!session) {
      return { valid: 0, orphaned: [], missing: [] };
    }

    const orphaned: string[] = []; // 有元数据但文件不存在
    const missing: string[] = [];  // 有文件但元数据不存在
    let valid = 0;

    // 检查元数据对应的快照文件是否存在
    for (const snapshot of session.snapshots) {
      const snapshotFile = path.join(this.getSnapshotDir(sessionId), `${snapshot.id}.snapshot`);
      if (!fs.existsSync(snapshotFile)) {
        orphaned.push(snapshot.id);
      } else {
        valid++;
      }
    }

    // 检查快照目录中是否有未记录的快照文件
    const snapshotDir = this.getSnapshotDir(sessionId);
    if (fs.existsSync(snapshotDir)) {
      const files = fs.readdirSync(snapshotDir);
      const recordedIds = new Set(session.snapshots.map(s => s.id));

      for (const file of files) {
        if (file.endsWith('.snapshot')) {
          const snapshotId = file.replace('.snapshot', '');
          if (!recordedIds.has(snapshotId)) {
            missing.push(snapshotId);
          }
        }
      }
    }

    return { valid, orphaned, missing };
  }

  /** 清理孤立的快照元数据（元数据存在但文件不存在） */
  cleanupOrphanedMetadata(sessionId: string): number {
    const session = this.sessionManager.getSession(sessionId);
    if (!session) return 0;

    let cleaned = 0;
    const toRemove: string[] = [];

    for (const snapshot of session.snapshots) {
      const snapshotFile = path.join(this.getSnapshotDir(sessionId), `${snapshot.id}.snapshot`);
      if (!fs.existsSync(snapshotFile)) {
        toRemove.push(snapshot.filePath);
        cleaned++;
      }
    }

    // 移除孤立的元数据
    for (const filePath of toRemove) {
      this.sessionManager.removeSnapshot(sessionId, filePath);
    }

    if (cleaned > 0) {
      logger.info('快照.清理.孤立元数据', { count: cleaned }, LogCategory.RECOVERY);
    }

    return cleaned;
  }

  /** 清理未记录的快照文件（文件存在但元数据不存在） */
  cleanupUnrecordedFiles(sessionId: string): number {
    const session = this.sessionManager.getSession(sessionId);
    if (!session) return 0;

    const snapshotDir = this.getSnapshotDir(sessionId);
    if (!fs.existsSync(snapshotDir)) return 0;

    const recordedIds = new Set(session.snapshots.map(s => s.id));
    const files = fs.readdirSync(snapshotDir);
    let cleaned = 0;

    for (const file of files) {
      if (file.endsWith('.snapshot')) {
        const snapshotId = file.replace('.snapshot', '');
        if (!recordedIds.has(snapshotId)) {
          const filePath = path.join(snapshotDir, file);
          try {
            fs.unlinkSync(filePath);
            this.invalidateSnapshotCache(filePath);
            cleaned++;
          } catch (error) {
            logger.error('快照.清理.未记录.失败', { filePath: file, error }, LogCategory.RECOVERY);
          }
        }
      }
    }

    if (cleaned > 0) {
      logger.info('快照.清理.未记录.完成', { count: cleaned }, LogCategory.RECOVERY);
    }

    return cleaned;
  }

  /** 修复快照完整性（清理孤立元数据和未记录文件） */
  repairSnapshotIntegrity(sessionId: string): {
    orphanedCleaned: number;
    unrecordedCleaned: number;
  } {
    const orphanedCleaned = this.cleanupOrphanedMetadata(sessionId);
    const unrecordedCleaned = this.cleanupUnrecordedFiles(sessionId);

    return { orphanedCleaned, unrecordedCleaned };
  }

  /** 创建文件快照 */
  createSnapshot(
    filePath: string,
    modifiedBy: AgentType,  // ✅ 使用 AgentType
    subTaskId: string,
    priority: number = 5  // SubTask 优先级 (1-10, 1 最高)，默认 5
  ): FileSnapshot | null {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return null;

    const absolutePath = path.isAbsolute(filePath)
      ? filePath
      : path.join(this.workspaceRoot, filePath);

    // 安全检查：防止路径遍历攻击
    const normalizedPath = path.normalize(absolutePath);
    const normalizedRoot = path.normalize(this.workspaceRoot);
    if (!normalizedPath.startsWith(normalizedRoot)) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      throw new Error(`Path traversal detected: file must be within workspace`);
    }

    const relativePath = path.relative(this.workspaceRoot, absolutePath);

    // 检查是否已有该文件的快照（同一 SubTask）
    const existingSnapshot = session.snapshots.find(
      s => s.filePath === relativePath && s.subTaskId === subTaskId
    );

    if (existingSnapshot) {
      // 同一 SubTask 重复创建快照，直接返回现有快照
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${existingSnapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);

      return {
        ...existingSnapshot,
        sessionId: session.id,
        originalContent,
      };
    }

    // 检查是否有其他 SubTask 已经创建了该文件的快照
    const otherSnapshot = session.snapshots.find(
      s => s.filePath === relativePath && s.subTaskId !== subTaskId
    );

    if (otherSnapshot) {
      // 冲突检测：多个 SubTask 修改同一文件
      // 使用优先级解决冲突：数字越小优先级越高
      if (priority < otherSnapshot.priority) {
        // 新 SubTask 优先级更高，允许覆盖历史快照
        logger.info(
          '快照.冲突.覆盖',
          {
            filePath: relativePath,
            subTaskId,
            priority,
            previousSubTaskId: otherSnapshot.subTaskId,
            previousPriority: otherSnapshot.priority,
          },
          LogCategory.RECOVERY
        );

        // 删除历史快照文件
        const oldSnapshotFile = path.join(this.getSnapshotDir(session.id), `${otherSnapshot.id}.snapshot`);
        if (fs.existsSync(oldSnapshotFile)) {
          try {
            fs.unlinkSync(oldSnapshotFile);
            // 清除历史快照的缓存
            this.invalidateSnapshotCache(oldSnapshotFile);
          } catch (error) {
            logger.error('快照.清理.旧快照.失败', { path: oldSnapshotFile, error }, LogCategory.RECOVERY);
            throw new Error(`Failed to delete previous snapshot: ${error}`);
          }
        }

        // 从 session 中移除历史快照元数据
        this.sessionManager.removeSnapshot(session.id, relativePath);

        // 继续创建新快照（下面的代码会处理）
      } else {
        // 新 SubTask 优先级更低或相等，拒绝创建新快照
        logger.warn(
          '快照.冲突.阻塞',
          {
            filePath: relativePath,
            subTaskId,
            priority,
            existingSubTaskId: otherSnapshot.subTaskId,
            existingPriority: otherSnapshot.priority,
          },
          LogCategory.RECOVERY
        );

        // 返回现有快照
        const snapshotFile = path.join(this.getSnapshotDir(session.id), `${otherSnapshot.id}.snapshot`);
        const originalContent = this.readSnapshotWithCache(snapshotFile);
        return {
          ...otherSnapshot,
          sessionId: session.id,
          originalContent,
        };
      }
    }

    // 读取原始文件内容（使用缓存）
    const originalContent = this.readFileWithCache(absolutePath);

    const snapshotId = generateId();
    const snapshotMeta: FileSnapshotMeta = {
      id: snapshotId,
      filePath: relativePath,
      lastModifiedBy: modifiedBy,
      lastModifiedAt: Date.now(),
      subTaskId,
      priority,  // 添加优先级字段
    };

    // 使用原子操作保存快照
    const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshotId}.snapshot`);
    const result = this.atomicWriteSnapshot(
      session.id,
      snapshotId,
      snapshotFile,
      originalContent,
      snapshotMeta
    );

    if (!result.success) {
      logger.error('快照.创建.失败', { path: snapshotFile, error: result.error }, LogCategory.RECOVERY);
      throw new Error(`Failed to create snapshot: ${result.error}`);
    }

    globalEventBus.emitEvent('snapshot:created', {
      sessionId: session.id,
      data: { filePath: relativePath, snapshotId },
    });

    return {
      ...snapshotMeta,
      sessionId: session.id,
      originalContent,
    };
  }

  /**
   * 创建文件快照（使用基线内容，适用于任务后补快照）
   */
  createSnapshotFromBaseline(
    filePath: string,
    modifiedBy: AgentType,  // ✅ 使用 AgentType
    subTaskId: string,
    priority: number = 5,
    baselineContent: string = ''
  ): FileSnapshot | null {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return null;

    const absolutePath = path.isAbsolute(filePath)
      ? filePath
      : path.join(this.workspaceRoot, filePath);

    // 安全检查：防止路径遍历攻击
    const normalizedPath = path.normalize(absolutePath);
    const normalizedRoot = path.normalize(this.workspaceRoot);
    if (!normalizedPath.startsWith(normalizedRoot)) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      throw new Error(`Path traversal detected: file must be within workspace`);
    }

    const relativePath = path.relative(this.workspaceRoot, absolutePath);

    // 检查是否已有该文件的快照（同一 SubTask）
    const existingSnapshot = session.snapshots.find(
      s => s.filePath === relativePath && s.subTaskId === subTaskId
    );

    if (existingSnapshot) {
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${existingSnapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);

      return {
        ...existingSnapshot,
        sessionId: session.id,
        originalContent,
      };
    }

    // 检查是否有其他 SubTask 已经创建了该文件的快照
    const otherSnapshot = session.snapshots.find(
      s => s.filePath === relativePath && s.subTaskId !== subTaskId
    );

    if (otherSnapshot) {
      if (priority < otherSnapshot.priority) {
        logger.info(
          '快照.冲突.覆盖',
          {
            filePath: relativePath,
            subTaskId,
            priority,
            previousSubTaskId: otherSnapshot.subTaskId,
            previousPriority: otherSnapshot.priority,
          },
          LogCategory.RECOVERY
        );

        const oldSnapshotFile = path.join(this.getSnapshotDir(session.id), `${otherSnapshot.id}.snapshot`);
        if (fs.existsSync(oldSnapshotFile)) {
          try {
            fs.unlinkSync(oldSnapshotFile);
            this.invalidateSnapshotCache(oldSnapshotFile);
          } catch (error) {
            logger.error('快照.清理.旧快照.失败', { path: oldSnapshotFile, error }, LogCategory.RECOVERY);
            throw new Error(`Failed to delete previous snapshot: ${error}`);
          }
        }

        this.sessionManager.removeSnapshot(session.id, relativePath);
      } else {
        logger.warn(
          '快照.冲突.复用',
          {
            filePath: relativePath,
            subTaskId,
            priority,
            existingSubTaskId: otherSnapshot.subTaskId,
            existingPriority: otherSnapshot.priority,
          },
          LogCategory.RECOVERY
        );

        const snapshotFile = path.join(this.getSnapshotDir(session.id), `${otherSnapshot.id}.snapshot`);
        const originalContent = this.readSnapshotWithCache(snapshotFile);
        return {
          ...otherSnapshot,
          sessionId: session.id,
          originalContent,
        };
      }
    }

    const originalContent = baselineContent ?? '';
    const snapshotId = generateId();
    const snapshotMeta: FileSnapshotMeta = {
      id: snapshotId,
      filePath: relativePath,
      lastModifiedBy: modifiedBy,
      lastModifiedAt: Date.now(),
      subTaskId,
      priority,
    };

    // 使用原子操作保存快照
    const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshotId}.snapshot`);
    const result = this.atomicWriteSnapshot(
      session.id,
      snapshotId,
      snapshotFile,
      originalContent,
      snapshotMeta
    );

    if (!result.success) {
      logger.error('快照.创建.失败', { path: snapshotFile, error: result.error }, LogCategory.RECOVERY);
      throw new Error(`Failed to create snapshot: ${result.error}`);
    }

    globalEventBus.emitEvent('snapshot:created', {
      sessionId: session.id,
      data: { filePath: relativePath, snapshotId },
    });

    return {
      ...snapshotMeta,
      sessionId: session.id,
      originalContent,
    };
  }


  /** 还原文件到快照状态 */
  revertToSnapshot(filePath: string): boolean {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return false;

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) return false;

    const absolutePath = path.join(this.workspaceRoot, relativePath);

    // 安全检查：防止路径遍历攻击
    const normalizedPath = path.normalize(absolutePath);
    const normalizedRoot = path.normalize(this.workspaceRoot);
    if (!normalizedPath.startsWith(normalizedRoot)) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      return false;
    }

    // 读取快照内容（使用缓存）
    const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
    const content = this.readSnapshotWithCache(snapshotFile);

    // 还原文件
    if (content === '' && fs.existsSync(absolutePath)) {
      // 原本不存在的文件，删除它
      fs.unlinkSync(absolutePath);
      this.invalidateFileCache(absolutePath);
    } else {
      // 确保目录存在
      const dir = path.dirname(absolutePath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      fs.writeFileSync(absolutePath, content, 'utf-8');
      this.invalidateFileCache(absolutePath); // 文件被修改，清除缓存
    }

    globalEventBus.emitEvent('snapshot:reverted', {
      sessionId: session.id,
      data: { filePath: relativePath },
    });

    return true;
  }

  /** 获取待处理变更列表（去重 + 排序） */
  getPendingChanges(): PendingChange[] {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return [];

    const changesMap = new Map<string, PendingChange & { lastModifiedAt?: number }>();

    for (const snapshot of session.snapshots) {
      const absolutePath = path.join(this.workspaceRoot, snapshot.filePath);
      const currentContent = this.readFileWithCache(absolutePath);

      // 读取原始内容（使用缓存）
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);

      // 计算变更行数
      const { additions, deletions } = this.countChanges(originalContent, currentContent);

      if (additions > 0 || deletions > 0) {
        const existing = changesMap.get(snapshot.filePath);
        const currentModifiedAt = snapshot.lastModifiedAt ?? 0;
        if (!existing) {
          changesMap.set(snapshot.filePath, {
            filePath: snapshot.filePath,
            snapshotId: snapshot.id,
            lastModifiedBy: snapshot.lastModifiedBy,
            additions,
            deletions,
            status: 'pending',
            subTaskId: snapshot.subTaskId,
            lastModifiedAt: currentModifiedAt,
          });
          continue;
        }

        const merged = {
          ...existing,
          additions: Math.max(existing.additions, additions),
          deletions: Math.max(existing.deletions, deletions),
        };

        if (currentModifiedAt >= (existing.lastModifiedAt ?? 0)) {
          merged.snapshotId = snapshot.id;
          merged.lastModifiedBy = snapshot.lastModifiedBy;
          merged.subTaskId = snapshot.subTaskId;
          merged.lastModifiedAt = currentModifiedAt;
        }

        changesMap.set(snapshot.filePath, merged);
      }
    }

    return Array.from(changesMap.values())
      .map(({ lastModifiedAt, ...change }) => change)
      .sort((a, b) => a.filePath.localeCompare(b.filePath));
  }

  /** 获取指定子任务的实际变更文件 */
  getChangedFilesForSubTask(subTaskId: string): string[] {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return [];

    const files: string[] = [];
    for (const snapshot of session.snapshots) {
      if (snapshot.subTaskId !== subTaskId) {
        continue;
      }
      const absolutePath = path.join(this.workspaceRoot, snapshot.filePath);
      const currentContent = this.readFileWithCache(absolutePath);

      // 读取原始内容（使用缓存）
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);

      if (currentContent !== originalContent) {
        files.push(snapshot.filePath);
      }
    }
    return files;
  }

  /** 计算变更行数 */
  private countChanges(original: string, current: string): { additions: number; deletions: number } {
    const originalLines = original.split('\n');
    const currentLines = current.split('\n');

    const additions = Math.max(0, currentLines.length - originalLines.length);
    const deletions = Math.max(0, originalLines.length - currentLines.length);

    if (additions === 0 && deletions === 0 && original !== current) {
      let changedLines = 0;
      const minLen = Math.min(originalLines.length, currentLines.length);
      for (let i = 0; i < minLen; i++) {
        if (originalLines[i] !== currentLines[i]) changedLines++;
      }
      return { additions: changedLines, deletions: changedLines };
    }

    return { additions, deletions };
  }

  /** 接受变更（删除历史快照，创建新基准快照） */
  acceptChange(filePath: string): boolean {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return false;

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) return false;

    // 1. 删除历史快照文件
    const oldSnapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
    if (fs.existsSync(oldSnapshotFile)) {
      fs.unlinkSync(oldSnapshotFile);
    }

    // 2. 从 session 中移除历史快照元数据
    this.sessionManager.removeSnapshot(session.id, relativePath);

    // ========================================
    // 修复: 创建新基准快照
    // ========================================
    const absolutePath = path.join(this.workspaceRoot, relativePath);

    // 安全检查: 防止路径遍历攻击
    const normalizedPath = path.normalize(absolutePath);
    const normalizedRoot = path.normalize(this.workspaceRoot);
    if (!normalizedPath.startsWith(normalizedRoot)) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      return false;
    }

    // 读取当前文件内容 (确认后的状态，使用缓存)
    const currentContent = this.readFileWithCache(absolutePath);

    // 创建新快照 ID
    const newSnapshotId = generateId();
    const newSnapshotMeta: FileSnapshotMeta = {
      id: newSnapshotId,
      filePath: relativePath,
      lastModifiedBy: snapshot.lastModifiedBy,
      lastModifiedAt: Date.now(),
      subTaskId: snapshot.subTaskId, // 继承原 subTaskId
      priority: snapshot.priority, // 继承原优先级
    };

    // 保存新快照内容到文件
    this.ensureSnapshotDir(session.id);
    const newSnapshotFile = path.join(this.getSnapshotDir(session.id), `${newSnapshotId}.snapshot`);
    fs.writeFileSync(newSnapshotFile, currentContent, 'utf-8');

    // 清除历史快照缓存，添加新快照到缓存
    this.invalidateSnapshotCache(oldSnapshotFile);
    this.snapshotContentCache.set(newSnapshotFile, currentContent);

    // 添加新快照元数据到 Session
    this.sessionManager.addSnapshot(session.id, newSnapshotMeta);

    logger.info('快照.接受.完成', { filePath: relativePath, oldSnapshotId: snapshot.id, newSnapshotId }, LogCategory.RECOVERY);

    globalEventBus.emitEvent('snapshot:accepted', {
      sessionId: session.id,
      data: {
        filePath: relativePath,
        oldSnapshotId: snapshot.id,
        newSnapshotId: newSnapshotId,
      },
    });

    return true;
  }

  /** 批量接受所有变更 */
  acceptAllChanges(): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    const snapshots = [...session.snapshots];
    let count = 0;

    for (const snapshot of snapshots) {
      if (this.acceptChange(snapshot.filePath)) {
        count++;
      }
    }

    return count;
  }

  /** 批量还原所有变更 */
  revertAllChanges(): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    const snapshots = [...session.snapshots];
    let count = 0;

    for (const snapshot of snapshots) {
      if (this.revertToSnapshot(snapshot.filePath)) {
        count++;
      }
    }

    return count;
  }

  /** 检查当前会话是否有快照 */
  hasSnapshots(): boolean {
    const session = this.sessionManager.getCurrentSession();
    return session ? session.snapshots.length > 0 : false;
  }

  /** 为多个文件创建快照 */
  createSnapshots(filePaths: string[], modifiedBy: AgentType, subTaskId: string, priority: number = 5): FileSnapshot[] {  // ✅ 使用 AgentType
    const snapshots: FileSnapshot[] = [];
    for (const filePath of filePaths) {
      const snapshot = this.createSnapshot(filePath, modifiedBy, subTaskId, priority);
      if (snapshot) {
        snapshots.push(snapshot);
      }
    }
    return snapshots;
  }

  /** 清理会话的所有快照（删除会话时不需要单独调用，会话目录会整体删除） */
  cleanupSession(sessionId: string): void {
    const snapshotDir = this.getSnapshotDir(sessionId);
    if (fs.existsSync(snapshotDir)) {
      fs.rmSync(snapshotDir, { recursive: true, force: true });
    }
  }
}
