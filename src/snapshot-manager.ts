/**
 * 快照管理器
 * 文件快照创建、存储、还原
 *
 * 存储路径：.magi/sessions/{sessionId}/snapshots/
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
        this.sessionManager.removeSnapshotById(sessionId, snapshotId);
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

    return this.readFileFresh(filePath);
  }

  /** 读取文件内容（强制从磁盘读取，并更新缓存） */
  private readFileFresh(filePath: string): string {
    let content = '';
    if (fs.existsSync(filePath)) {
      content = fs.readFileSync(filePath, 'utf-8');
    }
    this.addToCache(this.fileContentCache, filePath, content);
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

  /** 清理指定文件的历史快照（Mission 版本） */
  clearSnapshotsForFiles(filePaths: string[], keepTodoId?: string): number {
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
      if (keepTodoId && snapshot.todoId === keepTodoId) continue;

      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.清理.文件.失败', { filePath: snapshot.filePath, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshotById(session.id, snapshot.id);
      removed++;
    }

    if (removed > 0) {
      logger.info('快照.清理.目标文件', { count: removed }, LogCategory.RECOVERY);
    }
    return removed;
  }

  /** 按 Mission 清理快照 */
  clearSnapshotsForMission(missionId: string): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    let removed = 0;
    for (const snapshot of [...session.snapshots]) {
      if (snapshot.missionId !== missionId) continue;

      const snapshotFile = path.join(
        this.getSnapshotDir(session.id),
        `${snapshot.id}.snapshot`
      );
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.清理.失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshotById(session.id, snapshot.id);
      removed++;
    }

    if (removed > 0) {
      logger.info('快照.清理.Mission', { missionId, count: removed }, LogCategory.RECOVERY);
    }
    return removed;
  }

  /** 按 Assignment 清理快照 */
  clearSnapshotsForAssignment(assignmentId: string): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    let removed = 0;
    for (const snapshot of [...session.snapshots]) {
      if (snapshot.assignmentId !== assignmentId) continue;

      const snapshotFile = path.join(
        this.getSnapshotDir(session.id),
        `${snapshot.id}.snapshot`
      );
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.清理.失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshotById(session.id, snapshot.id);
      removed++;
    }

    if (removed > 0) {
      logger.info('快照.清理.Assignment', { assignmentId, count: removed }, LogCategory.RECOVERY);
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
        toRemove.push(snapshot.id);
        cleaned++;
      }
    }

    // 移除孤立的元数据
    for (const snapshotId of toRemove) {
      this.sessionManager.removeSnapshotById(sessionId, snapshotId);
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

  /** 创建文件快照（Mission 版本） */
  createSnapshotForMission(
    filePath: string,
    missionId: string,
    assignmentId: string,
    todoId: string,
    workerId: string,
    reason?: string
  ): FileSnapshot | null {
    if (!filePath || typeof filePath !== 'string' || filePath.trim().length === 0) {
      throw new Error('Snapshot filePath is required');
    }
    const session = this.sessionManager.getCurrentSession();
    if (!session) return null;

    const absolutePath = path.isAbsolute(filePath)
      ? filePath
      : path.join(this.workspaceRoot, filePath.trim());

    // 安全检查：防止路径遍历攻击
    const normalizedPath = path.normalize(absolutePath);
    const normalizedRoot = path.normalize(this.workspaceRoot);
    if (!normalizedPath.startsWith(normalizedRoot)) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      throw new Error(`Path traversal detected: file must be within workspace`);
    }

    const relativePath = path.relative(this.workspaceRoot, absolutePath);
    if (!relativePath || relativePath === '.' || relativePath.trim().length === 0) {
      throw new Error('Snapshot filePath must be a file within workspace');
    }

    // 检查是否已有该文件的快照（同一 Todo）
    const existingSnapshot = session.snapshots.find(
      s => s.filePath === relativePath && s.todoId === todoId
    );

    if (existingSnapshot) {
      // 同一 Todo 重复创建快照，直接返回现有快照
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${existingSnapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);

      return {
        id: existingSnapshot.id,
        sessionId: session.id,
        filePath: relativePath,
        timestamp: existingSnapshot.timestamp,
        missionId: existingSnapshot.missionId,
        assignmentId: existingSnapshot.assignmentId,
        todoId: existingSnapshot.todoId,
        workerId: existingSnapshot.workerId,
        agentType: existingSnapshot.agentType,
        reason: existingSnapshot.reason,
        originalContent,
      };
    }

    // 读取原始文件内容（使用缓存）
    const originalContent = this.readFileFresh(absolutePath);

    const snapshotId = generateId();
    const snapshotMeta: FileSnapshotMeta = {
      id: snapshotId,
      filePath: relativePath,
      timestamp: Date.now(),
      missionId,
      assignmentId,
      todoId,
      workerId,
      contributors: [workerId],
      reason,
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
      data: { filePath: relativePath, snapshotId, missionId, assignmentId, todoId },
    });

    return {
      id: snapshotId,
      sessionId: session.id,
      filePath: relativePath,
      timestamp: snapshotMeta.timestamp,
      missionId,
      assignmentId,
      todoId,
      workerId,
      contributors: snapshotMeta.contributors,
      reason,
      originalContent,
    };
  }

  /** 归一化文件路径并做 workspace 越界校验 */
  private normalizeRelativePath(filePath: string): { relativePath: string; absolutePath: string } | null {
    const absolutePath = path.isAbsolute(filePath)
      ? filePath
      : path.join(this.workspaceRoot, filePath);
    const normalizedPath = path.normalize(absolutePath);
    const normalizedRoot = path.normalize(this.workspaceRoot);
    if (!normalizedPath.startsWith(normalizedRoot)) {
      return null;
    }
    return {
      relativePath: path.relative(this.workspaceRoot, normalizedPath),
      absolutePath: normalizedPath,
    };
  }

  /** 按文件删除快照文件与元数据（精确按 snapshotId） */
  private removeSnapshotsByIds(sessionId: string, snapshots: FileSnapshotMeta[]): void {
    for (const snapshot of snapshots) {
      const snapshotFile = path.join(this.getSnapshotDir(sessionId), `${snapshot.id}.snapshot`);
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.删除.失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshotById(sessionId, snapshot.id);
    }
  }

  /** 还原文件到快照状态（单文件：回退该文件全部未接受变更） */
  revertToSnapshot(filePath: string): boolean {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return false;

    const normalized = this.normalizeRelativePath(filePath);
    if (!normalized) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      return false;
    }
    const { relativePath, absolutePath } = normalized;

    const snapshots = this.sessionManager.getSnapshotsByFile(session.id, relativePath);
    if (snapshots.length === 0) return false;

    const currentContent = this.readFileFresh(absolutePath);
    const differingSnapshots = snapshots.filter(snapshot => {
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);
      return originalContent !== currentContent;
    });
    if (differingSnapshots.length === 0) return false;

    // 取最早未对齐快照，回退该文件的全部未接受变更
    const targetSnapshot = differingSnapshots[0];
    const targetSnapshotFile = path.join(this.getSnapshotDir(session.id), `${targetSnapshot.id}.snapshot`);
    const content = this.readSnapshotWithCache(targetSnapshotFile);

    if (content === '' && fs.existsSync(absolutePath)) {
      fs.unlinkSync(absolutePath);
      this.invalidateFileCache(absolutePath);
    } else if (content !== '') {
      const dir = path.dirname(absolutePath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      fs.writeFileSync(absolutePath, content, 'utf-8');
      this.invalidateFileCache(absolutePath);
    }

    this.removeSnapshotsByIds(session.id, snapshots);

    globalEventBus.emitEvent('snapshot:reverted', {
      sessionId: session.id,
      data: { filePath: relativePath },
    });

    return true;
  }

  /** 获取待处理变更列表（按文件聚合，missionId 取最新变更轮次） */
  getPendingChanges(): PendingChange[] {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return [];

    const snapshotsByFile = new Map<string, FileSnapshotMeta[]>();
    for (const snapshot of session.snapshots) {
      const group = snapshotsByFile.get(snapshot.filePath) ?? [];
      group.push(snapshot);
      snapshotsByFile.set(snapshot.filePath, group);
    }

    type FilePendingContext = {
      filePath: string;
      sortedSnapshots: FileSnapshotMeta[];
      currentContent: string;
      snapshotContentById: Map<string, string>;
      differingSnapshots: FileSnapshotMeta[];
      latestDiffering: FileSnapshotMeta;
    };

    const contexts: FilePendingContext[] = [];
    for (const [filePath, snapshots] of snapshotsByFile.entries()) {
      const sortedSnapshots = [...snapshots].sort(
        (a, b) => (a.timestamp - b.timestamp) || a.id.localeCompare(b.id),
      );
      const absolutePath = path.join(this.workspaceRoot, filePath);
      const currentContent = this.readFileFresh(absolutePath);
      const snapshotContentById = new Map<string, string>();
      const differingSnapshots: FileSnapshotMeta[] = [];

      for (const snapshot of sortedSnapshots) {
        const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
        const originalContent = this.readSnapshotWithCache(snapshotFile);
        snapshotContentById.set(snapshot.id, originalContent);
        if (originalContent !== currentContent) {
          differingSnapshots.push(snapshot);
        }
      }

      if (differingSnapshots.length === 0) {
        continue;
      }

      contexts.push({
        filePath,
        sortedSnapshots,
        currentContent,
        snapshotContentById,
        differingSnapshots,
        latestDiffering: differingSnapshots[differingSnapshots.length - 1],
      });
    }

    if (contexts.length === 0) {
      return [];
    }

    const latestMissionContext = [...contexts].sort(
      (a, b) =>
        (a.latestDiffering.timestamp - b.latestDiffering.timestamp)
        || a.latestDiffering.id.localeCompare(b.latestDiffering.id),
    )[contexts.length - 1];
    const latestMissionId = latestMissionContext.latestDiffering.missionId;

    const changes: Array<PendingChange & { timestamp: number }> = [];

    for (const context of contexts) {
      const { filePath, sortedSnapshots, currentContent, snapshotContentById } = context;
      const latestMissionSnapshots = sortedSnapshots.filter(
        snapshot => snapshot.missionId === latestMissionId
          && snapshotContentById.get(snapshot.id) !== currentContent,
      );

      let baselineBeforeLatest = currentContent;
      if (latestMissionSnapshots.length > 0) {
        const latestRoundStart = latestMissionSnapshots[0];
        const latestRoundEnd = latestMissionSnapshots[latestMissionSnapshots.length - 1];
        baselineBeforeLatest = snapshotContentById.get(latestRoundStart.id) ?? currentContent;
        const { additions, deletions } = this.countChanges(baselineBeforeLatest, currentContent);
        const contributors = new Set<string>();
        for (const snapshot of latestMissionSnapshots) {
          for (const contributor of snapshot.contributors ?? [snapshot.workerId]) {
            contributors.add(contributor);
          }
        }
        if (additions > 0 || deletions > 0) {
          changes.push({
            filePath,
            snapshotId: latestRoundStart.id,
            missionId: latestMissionId,
            assignmentId: latestRoundEnd.assignmentId,
            todoId: latestRoundEnd.todoId,
            workerId: latestRoundEnd.workerId,
            contributors: Array.from(contributors),
            additions,
            deletions,
            status: 'pending',
            timestamp: latestRoundEnd.timestamp ?? 0,
          });
        }
      }

      const stagedSnapshots = sortedSnapshots.filter(
        snapshot => snapshot.missionId !== latestMissionId
          && snapshotContentById.get(snapshot.id) !== baselineBeforeLatest,
      );
      if (stagedSnapshots.length === 0) {
        continue;
      }

      const stagedStart = stagedSnapshots[0];
      const stagedEnd = stagedSnapshots[stagedSnapshots.length - 1];
      const stagedBaseline = snapshotContentById.get(stagedStart.id) ?? '';
      const { additions, deletions } = this.countChanges(stagedBaseline, baselineBeforeLatest);
      if (additions === 0 && deletions === 0) {
        continue;
      }
      const stagedContributors = new Set<string>();
      for (const snapshot of stagedSnapshots) {
        for (const contributor of snapshot.contributors ?? [snapshot.workerId]) {
          stagedContributors.add(contributor);
        }
      }
      changes.push({
        filePath,
        snapshotId: stagedStart.id,
        missionId: stagedEnd.missionId,
        assignmentId: stagedEnd.assignmentId,
        todoId: stagedEnd.todoId,
        workerId: stagedEnd.workerId,
        contributors: Array.from(stagedContributors),
        additions,
        deletions,
        status: 'pending',
        timestamp: stagedEnd.timestamp ?? 0,
      });
    }

    return changes
      .sort((a, b) => (a.timestamp - b.timestamp) || a.filePath.localeCompare(b.filePath))
      .map(({ timestamp, ...change }) => change);
  }

  /** 获取指定 Todo 的实际变更文件 */
  getChangedFilesForTodo(todoId: string): string[] {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return [];

    const files = new Set<string>();
    for (const snapshot of session.snapshots) {
      if (snapshot.todoId !== todoId) {
        continue;
      }
      const absolutePath = path.join(this.workspaceRoot, snapshot.filePath);
      const currentContent = this.readFileFresh(absolutePath);

      // 读取原始内容（使用缓存）
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const originalContent = this.readSnapshotWithCache(snapshotFile);

      if (currentContent !== originalContent) {
        files.add(snapshot.filePath);
      }
    }
    return Array.from(files);
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

  /** 接受变更（清理该文件历史快照，并创建确认后基准快照） */
  acceptChange(filePath: string): boolean {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return false;

    const normalized = this.normalizeRelativePath(filePath);
    if (!normalized) {
      logger.error('快照.安全.路径穿越', { filePath }, LogCategory.RECOVERY);
      return false;
    }
    const { relativePath, absolutePath } = normalized;

    const snapshots = this.sessionManager.getSnapshotsByFile(session.id, relativePath);
    if (snapshots.length === 0) return false;

    // 删除该文件所有历史快照（保留磁盘与元数据一致性）
    const oldSnapshotIds = snapshots.map(snapshot => snapshot.id);
    this.removeSnapshotsByIds(session.id, snapshots);

    const currentContent = this.readFileFresh(absolutePath);
    const latestSnapshot = [...snapshots].sort(
      (a, b) => (a.timestamp - b.timestamp) || a.id.localeCompare(b.id),
    )[snapshots.length - 1];

    // 创建新基准快照，作为后续编辑回退基线
    const newSnapshotId = generateId();
    const newSnapshotMeta: FileSnapshotMeta = {
      id: newSnapshotId,
      filePath: relativePath,
      timestamp: Date.now(),
      missionId: latestSnapshot.missionId,
      assignmentId: latestSnapshot.assignmentId,
      todoId: latestSnapshot.todoId,
      workerId: latestSnapshot.workerId,
      contributors: latestSnapshot.contributors ?? [latestSnapshot.workerId],
      agentType: latestSnapshot.agentType,
      reason: 'Accepted change',
    };
    this.ensureSnapshotDir(session.id);
    const newSnapshotFile = path.join(this.getSnapshotDir(session.id), `${newSnapshotId}.snapshot`);
    fs.writeFileSync(newSnapshotFile, currentContent, 'utf-8');
    this.addToCache(this.snapshotContentCache, newSnapshotFile, currentContent);
    this.sessionManager.addSnapshot(session.id, newSnapshotMeta);

    logger.info('快照.接受.完成', {
      filePath: relativePath,
      removedSnapshots: oldSnapshotIds.length,
      newSnapshotId,
    }, LogCategory.RECOVERY);

    globalEventBus.emitEvent('snapshot:accepted', {
      sessionId: session.id,
      data: {
        filePath: relativePath,
        oldSnapshotIds,
        newSnapshotId,
      },
    });

    return true;
  }

  /** 批量接受所有变更 */
  acceptAllChanges(): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    const pendingChanges = this.getPendingChanges();
    let count = 0;

    for (const change of pendingChanges) {
      if (this.acceptChange(change.filePath)) {
        count++;
      }
    }

    return count;
  }

  /** 按 Mission（对话轮次）还原变更：精确找到该 Mission 创建的快照并逐文件还原 */
  revertMission(missionId: string): { reverted: number; files: string[] } {
    const session = this.sessionManager.getCurrentSession();
    if (!session || !missionId) return { reverted: 0, files: [] };

    // 1. 筛出属于该 Mission 的所有快照
    const missionSnapshots = session.snapshots.filter(s => s.missionId === missionId);
    if (missionSnapshots.length === 0) return { reverted: 0, files: [] };

    // 2. 同一文件可能被同一 Mission 的多个 Todo 快照，取最早的（记录的是该 Mission 首次触碰前的状态）
    const earliestByFile = new Map<string, typeof missionSnapshots[0]>();
    for (const snapshot of missionSnapshots) {
      const existing = earliestByFile.get(snapshot.filePath);
      if (!existing || (snapshot.timestamp ?? 0) < (existing.timestamp ?? 0)) {
        earliestByFile.set(snapshot.filePath, snapshot);
      }
    }

    const revertedFiles: string[] = [];

    // 3. 逐文件还原到该 Mission 修改前的状态
    for (const [relativePath, snapshot] of earliestByFile) {
      const absolutePath = path.join(this.workspaceRoot, relativePath);

      // 安全检查
      const normalizedPath = path.normalize(absolutePath);
      const normalizedRoot = path.normalize(this.workspaceRoot);
      if (!normalizedPath.startsWith(normalizedRoot)) {
        logger.error('快照.Mission还原.路径穿越', { filePath: relativePath }, LogCategory.RECOVERY);
        continue;
      }

      // 读取该 Mission 快照记录的原始内容（即该轮修改前的最新文件状态）
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const content = this.readSnapshotWithCache(snapshotFile);

      // 还原文件
      if (content === '' && fs.existsSync(absolutePath)) {
        // 该文件在本轮之前不存在 → 删除
        fs.unlinkSync(absolutePath);
        this.invalidateFileCache(absolutePath);
      } else if (content !== '') {
        const dir = path.dirname(absolutePath);
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true });
        }
        fs.writeFileSync(absolutePath, content, 'utf-8');
        this.invalidateFileCache(absolutePath);
      }
      revertedFiles.push(relativePath);
    }

    // 4. 批量删除该 Mission 的所有快照文件
    const missionSnapshotIds = new Set(missionSnapshots.map(s => s.id));
    for (const snapshot of missionSnapshots) {
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.Mission还原.删除失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);
        }
      }
    }

    // 5. 批量从 session.snapshots 中移除（按 id 精确匹配，避免误删其他 Mission 的同文件快照）
    for (const snapshotId of missionSnapshotIds) {
      this.sessionManager.removeSnapshotById(session.id, snapshotId);
    }

    if (revertedFiles.length > 0) {
      logger.info('快照.Mission还原.完成', {
        missionId,
        count: revertedFiles.length,
        files: revertedFiles.slice(0, 10).join(', '),
      }, LogCategory.RECOVERY);

      globalEventBus.emitEvent('snapshot:reverted', {
        sessionId: session.id,
        data: { missionId, files: revertedFiles },
      });
    }

    return { reverted: revertedFiles.length, files: revertedFiles };
  }

  /** 批量还原所有变更 */
  revertAllChanges(): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    const pendingChanges = this.getPendingChanges();
    let count = 0;

    for (const change of pendingChanges) {
      if (this.revertToSnapshot(change.filePath)) {
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

  /** 清理会话的所有快照（删除会话时不需要单独调用，会话目录会整体删除） */
  cleanupSession(sessionId: string): void {
    const snapshotDir = this.getSnapshotDir(sessionId);
    if (fs.existsSync(snapshotDir)) {
      fs.rmSync(snapshotDir, { recursive: true, force: true });
    }
  }
}
