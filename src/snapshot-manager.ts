/**
 * 快照管理器
 * 文件快照创建、存储、还原
 *
 * 存储路径：.multicli/sessions/{sessionId}/snapshots/
 */

import * as fs from 'fs';
import * as path from 'path';
import { FileSnapshot, CLIType, PendingChange } from './types';
import { UnifiedSessionManager, FileSnapshotMeta } from './session';
import { globalEventBus } from './events';

/** 生成唯一 ID */
function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * 快照管理器
 */
export class SnapshotManager {
  private sessionManager: UnifiedSessionManager;
  private workspaceRoot: string;

  constructor(sessionManager: UnifiedSessionManager, workspaceRoot: string) {
    this.sessionManager = sessionManager;
    this.workspaceRoot = workspaceRoot;
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

  /** 创建文件快照 */
  createSnapshot(
    filePath: string,
    modifiedBy: CLIType,
    subTaskId: string
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
      console.error(`[SnapshotManager] Path traversal detected: ${filePath}`);
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
      const originalContent = fs.existsSync(snapshotFile)
        ? fs.readFileSync(snapshotFile, 'utf-8')
        : '';

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
      // 警告：多个 SubTask 修改同一文件，可能导致冲突
      console.warn(
        `[SnapshotManager] Multiple SubTasks modifying same file: ${relativePath}\n` +
        `  Previous: ${otherSnapshot.subTaskId}\n` +
        `  Current: ${subTaskId}\n` +
        `  Consider using file locking to prevent conflicts.`
      );
      // 强制复用已有快照，避免覆盖原始快照内容
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${otherSnapshot.id}.snapshot`);
      const originalContent = fs.existsSync(snapshotFile)
        ? fs.readFileSync(snapshotFile, 'utf-8')
        : '';
      return {
        ...otherSnapshot,
        sessionId: session.id,
        originalContent,
      };
    }

    // 读取原始文件内容
    let originalContent = '';
    if (fs.existsSync(absolutePath)) {
      originalContent = fs.readFileSync(absolutePath, 'utf-8');
    }

    const snapshotId = generateId();
    const snapshotMeta: FileSnapshotMeta = {
      id: snapshotId,
      filePath: relativePath,
      lastModifiedBy: modifiedBy,
      lastModifiedAt: Date.now(),
      subTaskId,
    };

    // 保存快照内容到文件
    this.ensureSnapshotDir(session.id);
    const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshotId}.snapshot`);
    fs.writeFileSync(snapshotFile, originalContent, 'utf-8');

    // 添加元数据到 Session
    this.sessionManager.addSnapshot(session.id, snapshotMeta);

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
      console.error(`[SnapshotManager] Path traversal detected: ${filePath}`);
      return false;
    }

    // 读取快照内容
    const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
    let content = '';

    if (fs.existsSync(snapshotFile)) {
      content = fs.readFileSync(snapshotFile, 'utf-8');
    }

    // 还原文件
    if (content === '' && fs.existsSync(absolutePath)) {
      // 原本不存在的文件，删除它
      fs.unlinkSync(absolutePath);
    } else {
      // 确保目录存在
      const dir = path.dirname(absolutePath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      fs.writeFileSync(absolutePath, content, 'utf-8');
    }

    globalEventBus.emitEvent('snapshot:reverted', {
      sessionId: session.id,
      data: { filePath: relativePath },
    });

    return true;
  }

  /** 获取待处理变更列表 */
  getPendingChanges(): PendingChange[] {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return [];

    const changes: PendingChange[] = [];

    for (const snapshot of session.snapshots) {
      const absolutePath = path.join(this.workspaceRoot, snapshot.filePath);
      const currentContent = fs.existsSync(absolutePath)
        ? fs.readFileSync(absolutePath, 'utf-8')
        : '';

      // 读取原始内容
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const originalContent = fs.existsSync(snapshotFile)
        ? fs.readFileSync(snapshotFile, 'utf-8')
        : '';

      // 计算变更行数
      const { additions, deletions } = this.countChanges(originalContent, currentContent);

      if (additions > 0 || deletions > 0) {
        changes.push({
          filePath: snapshot.filePath,
          snapshotId: snapshot.id,
          lastModifiedBy: snapshot.lastModifiedBy,
          additions,
          deletions,
          status: 'pending',
          subTaskId: snapshot.subTaskId,
        });
      }
    }

    return changes;
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
      const currentContent = fs.existsSync(absolutePath)
        ? fs.readFileSync(absolutePath, 'utf-8')
        : '';

      // 读取原始内容
      const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
      const originalContent = fs.existsSync(snapshotFile)
        ? fs.readFileSync(snapshotFile, 'utf-8')
        : '';

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

  /** 接受变更（删除快照，保留当前文件） */
  acceptChange(filePath: string): boolean {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return false;

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) return false;

    // 删除快照文件
    const snapshotFile = path.join(this.getSnapshotDir(session.id), `${snapshot.id}.snapshot`);
    if (fs.existsSync(snapshotFile)) {
      fs.unlinkSync(snapshotFile);
    }

    // 从 session 中移除快照
    this.sessionManager.removeSnapshot(session.id, relativePath);

    globalEventBus.emitEvent('snapshot:accepted', {
      sessionId: session.id,
      data: { filePath: relativePath },
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

  /** 为多个文件创建快照 */
  createSnapshots(filePaths: string[], modifiedBy: CLIType, subTaskId: string): FileSnapshot[] {
    const snapshots: FileSnapshot[] = [];
    for (const filePath of filePaths) {
      const snapshot = this.createSnapshot(filePath, modifiedBy, subTaskId);
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
