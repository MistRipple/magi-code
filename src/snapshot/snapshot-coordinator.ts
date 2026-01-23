/**
 * Snapshot Coordinator - 快照协调器
 *
 * 职责：
 * - 协调各个快照模块
 * - 提供统一的快照管理接口
 * - 创建、恢复、比较快照
 * - 管理快照生命周期
 */

import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../logging';
import { FileSnapshot, WorkerSlot, PendingChange } from '../types';
import { UnifiedSessionManager, FileSnapshotMeta } from '../session';
import { globalEventBus } from '../events';
import { IDGenerator } from '../utils/id-generator';

import { AtomicOperations, SnapshotOperationResult } from './atomic-operations';
import { SnapshotCache } from './snapshot-cache';
import { SnapshotValidator, ValidationResult } from './snapshot-validator';
import { SnapshotCleaner, CleanupResult, CleanupOptions } from './snapshot-cleaner';

export class SnapshotCoordinator {
  private atomicOps: AtomicOperations;
  private cache: SnapshotCache;
  private validator: SnapshotValidator;
  private cleaner: SnapshotCleaner;

  constructor(
    private sessionManager: UnifiedSessionManager,
    private workspaceRoot: string
  ) {
    this.atomicOps = new AtomicOperations(sessionManager);
    this.cache = new SnapshotCache(100);
    this.validator = new SnapshotValidator(sessionManager, workspaceRoot);
    this.cleaner = new SnapshotCleaner(sessionManager, workspaceRoot);
  }

  /**
   * 创建快照
   */
  createSnapshot(
    filePath: string,
    modifiedBy: WorkerSlot,
    subTaskId: string,
    priority: number = 5
  ): FileSnapshot | null {
    const sessionId = this.sessionManager.getCurrentSessionId();
    if (!sessionId) {
      logger.error('快照.创建失败.无会话', { filePath }, LogCategory.RECOVERY);
      return null;
    }

    const fullPath = path.resolve(this.workspaceRoot, filePath);

    // 验证文件路径
    if (!fullPath.startsWith(this.workspaceRoot)) {
      logger.error('快照.创建失败.路径非法', { filePath }, LogCategory.RECOVERY);
      return null;
    }

    // 读取文件内容
    let content: string;
    try {
      content = this.cache.readFileWithCache(fullPath);
    } catch (error) {
      logger.error('快照.创建失败.读取文件失败', { filePath, error }, LogCategory.RECOVERY);
      return null;
    }

    // 创建快照对象
    const snapshotId = IDGenerator.generate('snapshot');
    const snapshot: FileSnapshot = {
      id: snapshotId,
      sessionId,
      filePath,
      originalContent: content,
      timestamp: Date.now(),
      missionId: 'legacy-mission',
      assignmentId: 'legacy-assignment',
      todoId: subTaskId,
      workerId: modifiedBy,
    };

    // 验证快照
    const validation = this.validator.validateSnapshot(snapshot);
    if (!validation.valid) {
      logger.error('快照.创建失败.验证失败', {
        filePath,
        errors: validation.errors,
      }, LogCategory.RECOVERY);
      return null;
    }

    // 准备元数据
    const meta: FileSnapshotMeta = {
      id: snapshotId,
      filePath,
      timestamp: snapshot.timestamp,
      missionId: 'legacy-mission',
      assignmentId: 'legacy-assignment',
      todoId: subTaskId,
      workerId: modifiedBy,
    };

    // 原子性写入快照
    const snapshotDir = this.getSnapshotDir(sessionId);
    const snapshotFile = path.join(snapshotDir, `${snapshotId}.snapshot`);

    const result = this.atomicOps.atomicWriteSnapshot(
      sessionId,
      snapshotId,
      snapshotFile,
      content,
      meta,
      () => this.ensureSnapshotDir(sessionId),
      (file, content) => this.cache.addToCache(this.cache['snapshotContentCache'], file, content)
    );

    if (!result.success) {
      logger.error('快照.创建失败', { filePath, error: result.error }, LogCategory.RECOVERY);
      return null;
    }

    // 发送事件
    globalEventBus.emit({
      type: 'snapshot:created',
      timestamp: Date.now(),
      sessionId,
      data: { snapshot },
    });

    logger.info('快照.创建成功', {
      snapshotId,
      filePath,
      modifiedBy,
      subTaskId,
    }, LogCategory.RECOVERY);

    return snapshot;
  }

  /**
   * 批量创建快照
   */
  createSnapshots(
    filePaths: string[],
    modifiedBy: WorkerSlot,
    subTaskId: string,
    priority: number = 5
  ): FileSnapshot[] {
    const snapshots: FileSnapshot[] = [];

    for (const filePath of filePaths) {
      const snapshot = this.createSnapshot(filePath, modifiedBy, subTaskId, priority);
      if (snapshot) {
        snapshots.push(snapshot);
      }
    }

    return snapshots;
  }

  /**
   * 恢复快照
   */
  restoreSnapshot(snapshotId: string): boolean {
    const sessionId = this.sessionManager.getCurrentSessionId();
    if (!sessionId) {
      logger.error('快照.恢复失败.无会话', { snapshotId }, LogCategory.RECOVERY);
      return false;
    }

    // 查找快照元数据
    const snapshots = this.sessionManager.getSession(sessionId)?.snapshots || [];
    const meta = snapshots.find((s: FileSnapshotMeta) => s.id === snapshotId);

    if (!meta) {
      logger.error('快照.恢复失败.快照不存在', { snapshotId }, LogCategory.RECOVERY);
      return false;
    }

    // 读取快照内容
    const snapshotDir = this.getSnapshotDir(sessionId);
    const snapshotFile = path.join(snapshotDir, `${snapshotId}.snapshot`);

    let content: string;
    try {
      content = this.cache.readSnapshotWithCache(snapshotFile);
    } catch (error) {
      logger.error('快照.恢复失败.读取快照失败', { snapshotId, error }, LogCategory.RECOVERY);
      return false;
    }

    // 恢复文件
    const fullPath = path.resolve(this.workspaceRoot, meta.filePath);

    try {
      fs.writeFileSync(fullPath, content, 'utf-8');
      this.cache.invalidateFileCache(fullPath);

      // 发送事件 (使用 snapshot:reverted 作为恢复事件)
      globalEventBus.emit({
        type: 'snapshot:reverted',
        timestamp: Date.now(),
        sessionId,
        data: { snapshotId, filePath: meta.filePath },
      });

      logger.info('快照.恢复成功', { snapshotId, filePath: meta.filePath }, LogCategory.RECOVERY);
      return true;
    } catch (error) {
      logger.error('快照.恢复失败.写入文件失败', {
        snapshotId,
        filePath: meta.filePath,
        error,
      }, LogCategory.RECOVERY);
      return false;
    }
  }

  /**
   * 删除快照
   */
  deleteSnapshot(snapshotId: string): boolean {
    const sessionId = this.sessionManager.getCurrentSessionId();
    if (!sessionId) {
      logger.error('快照.删除失败.无会话', { snapshotId }, LogCategory.RECOVERY);
      return false;
    }

    // 查找快照元数据
    const snapshots = this.sessionManager.getSession(sessionId)?.snapshots || [];
    const meta = snapshots.find((s: FileSnapshotMeta) => s.id === snapshotId);

    if (!meta) {
      logger.error('快照.删除失败.快照不存在', { snapshotId }, LogCategory.RECOVERY);
      return false;
    }

    // 原子性删除快照
    const snapshotDir = this.getSnapshotDir(sessionId);
    const snapshotFile = path.join(snapshotDir, `${snapshotId}.snapshot`);

    const result = this.atomicOps.atomicDeleteSnapshot(
      sessionId,
      snapshotId,
      snapshotFile,
      meta.filePath,
      (file) => this.cache.invalidateSnapshotCache(file)
    );

    if (!result.success) {
      logger.error('快照.删除失败', { snapshotId, error: result.error }, LogCategory.RECOVERY);
      return false;
    }

    // 发送事件 (删除快照不发送事件，或使用 change:reverted)
    globalEventBus.emit({
      type: 'change:reverted',
      timestamp: Date.now(),
      sessionId,
      data: { snapshotId },
    });

    logger.info('快照.删除成功', { snapshotId }, LogCategory.RECOVERY);
    return true;
  }

  /**
   * 比较快照与当前文件
   */
  compareSnapshot(snapshotId: string): PendingChange | null {
    const sessionId = this.sessionManager.getCurrentSessionId();
    if (!sessionId) {
      return null;
    }

    // 查找快照元数据
    const snapshots = this.sessionManager.getSession(sessionId)?.snapshots || [];
    const meta = snapshots.find((s: FileSnapshotMeta) => s.id === snapshotId);

    if (!meta) {
      return null;
    }

    // 读取快照内容
    const snapshotDir = this.getSnapshotDir(sessionId);
    const snapshotFile = path.join(snapshotDir, `${snapshotId}.snapshot`);

    let snapshotContent: string;
    try {
      snapshotContent = this.cache.readSnapshotWithCache(snapshotFile);
    } catch (error) {
      return null;
    }

    // 读取当前文件内容
    const fullPath = path.resolve(this.workspaceRoot, meta.filePath);
    let currentContent: string;

    try {
      currentContent = this.cache.readFileWithCache(fullPath);
    } catch (error) {
      // 文件可能已被删除
      currentContent = '';
    }

    // 计算变更
    const changes = this.countChanges(snapshotContent, currentContent);

    return {
      filePath: meta.filePath,
      snapshotId,
      missionId: meta.missionId,
      assignmentId: meta.assignmentId,
      todoId: meta.todoId,
      workerId: meta.workerId,
      additions: changes.additions,
      deletions: changes.deletions,
      status: 'pending' as const,
    };
  }

  /**
   * 获取快照列表
   */
  getSnapshots(sessionId?: string): FileSnapshotMeta[] {
    const sid = sessionId || this.sessionManager.getCurrentSessionId();
    if (!sid) {
      return [];
    }

    return this.sessionManager.getSession(sid)?.snapshots || [];
  }

  /**
   * 清理快照
   */
  cleanup(sessionId: string, options: CleanupOptions = {}): CleanupResult {
    return this.cleaner.cleanupSession(sessionId, options);
  }

  /**
   * 验证快照
   */
  validate(snapshot: FileSnapshot): ValidationResult {
    return this.validator.validateSnapshot(snapshot);
  }

  /**
   * 获取快照目录
   */
  private getSnapshotDir(sessionId: string): string {
    return this.sessionManager.getSnapshotsDir(sessionId);
  }

  /**
   * 确保快照目录存在
   */
  private ensureSnapshotDir(sessionId: string): void {
    const snapshotDir = this.getSnapshotDir(sessionId);
    if (!fs.existsSync(snapshotDir)) {
      fs.mkdirSync(snapshotDir, { recursive: true });
    }
  }

  /**
   * 计算变更行数
   */
  private countChanges(
    original: string,
    current: string
  ): { additions: number; deletions: number } {
    const originalLines = original.split('\n');
    const currentLines = current.split('\n');

    let additions = 0;
    let deletions = 0;

    // 简单的行级比较
    const maxLen = Math.max(originalLines.length, currentLines.length);

    for (let i = 0; i < maxLen; i++) {
      const origLine = originalLines[i];
      const currLine = currentLines[i];

      if (origLine === undefined) {
        additions++;
      } else if (currLine === undefined) {
        deletions++;
      } else if (origLine !== currLine) {
        additions++;
        deletions++;
      }
    }

    return { additions, deletions };
  }

  /**
   * 获取缓存统计
   */
  getCacheStats() {
    return this.cache.getStats();
  }

  /**
   * 清空缓存
   */
  clearCache(): void {
    this.cache.clearAll();
  }
}
