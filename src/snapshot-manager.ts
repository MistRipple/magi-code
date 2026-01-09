/**
 * 快照管理器
 * 文件快照创建、存储、还原
 */

import * as fs from 'fs';
import * as path from 'path';
import { FileSnapshot, CLIType, PendingChange } from './types';
import { SessionManager } from './session-manager';
import { globalEventBus } from './events';

/** 生成唯一 ID */
function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * 快照管理器
 */
export class SnapshotManager {
  private sessionManager: SessionManager;
  private workspaceRoot: string;
  private snapshotDir: string;

  constructor(sessionManager: SessionManager, workspaceRoot: string) {
    this.sessionManager = sessionManager;
    this.workspaceRoot = workspaceRoot;
    this.snapshotDir = path.join(workspaceRoot, '.cli-arranger', 'snapshots');
    this.ensureSnapshotDir();
  }

  /** 确保快照目录存在 */
  private ensureSnapshotDir(): void {
    if (!fs.existsSync(this.snapshotDir)) {
      fs.mkdirSync(this.snapshotDir, { recursive: true });
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
    
    const relativePath = path.relative(this.workspaceRoot, absolutePath);

    // 检查是否已有该文件的快照
    const existingSnapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (existingSnapshot) {
      // 更新修改信息，但保留原始内容
      existingSnapshot.lastModifiedBy = modifiedBy;
      existingSnapshot.lastModifiedAt = Date.now();
      existingSnapshot.subTaskId = subTaskId;
      this.sessionManager.addSnapshot(session.id, existingSnapshot);
      return existingSnapshot;
    }

    // 读取原始文件内容
    let originalContent = '';
    if (fs.existsSync(absolutePath)) {
      originalContent = fs.readFileSync(absolutePath, 'utf-8');
    }

    const snapshot: FileSnapshot = {
      id: generateId(),
      sessionId: session.id,
      filePath: relativePath,
      originalContent,
      lastModifiedBy: modifiedBy,
      lastModifiedAt: Date.now(),
      subTaskId,
    };

    // 保存快照内容到文件
    this.saveSnapshotContent(snapshot);
    
    // 添加到 Session
    this.sessionManager.addSnapshot(session.id, snapshot);
    
    globalEventBus.emitEvent('snapshot:created', {
      sessionId: session.id,
      data: { filePath: relativePath, snapshotId: snapshot.id },
    });

    return snapshot;
  }

  /** 保存快照内容到文件 */
  private saveSnapshotContent(snapshot: FileSnapshot): void {
    const sessionDir = path.join(this.snapshotDir, snapshot.sessionId);
    if (!fs.existsSync(sessionDir)) {
      fs.mkdirSync(sessionDir, { recursive: true });
    }
    
    const snapshotFile = path.join(sessionDir, `${snapshot.id}.snapshot`);
    fs.writeFileSync(snapshotFile, snapshot.originalContent, 'utf-8');
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
    
    // 读取快照内容
    const snapshotFile = path.join(this.snapshotDir, session.id, `${snapshot.id}.snapshot`);
    let content = snapshot.originalContent;
    
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

      // 计算变更行数
      const { additions, deletions } = this.countChanges(
        snapshot.originalContent,
        currentContent
      );

      if (additions > 0 || deletions > 0) {
        changes.push({
          filePath: snapshot.filePath,
          snapshotId: snapshot.id,
          lastModifiedBy: snapshot.lastModifiedBy,
          additions,
          deletions,
          status: 'pending',
        });
      }
    }

    return changes;
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
    const snapshotFile = path.join(this.snapshotDir, session.id, `${snapshot.id}.snapshot`);
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

  /** 清理会话的所有快照 */
  cleanupSession(sessionId: string): void {
    const sessionDir = path.join(this.snapshotDir, sessionId);
    if (fs.existsSync(sessionDir)) {
      fs.rmSync(sessionDir, { recursive: true, force: true });
    }
  }
}

