/**
 * Snapshot Validator - 快照验证模块
 *
 * 职责：
 * - 验证快照完整性
 * - 检查快照冲突
 * - 验证文件路径
 * - 检查快照状态
 */

import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../logging';
import { FileSnapshot } from '../types';
import { UnifiedSessionManager, FileSnapshotMeta } from '../session';

export interface ValidationResult {
  valid: boolean;
  errors: string[];
  warnings: string[];
}

export class SnapshotValidator {
  constructor(
    private sessionManager: UnifiedSessionManager,
    private workspaceRoot: string
  ) {}

  /**
   * 验证快照完整性
   */
  validateSnapshot(snapshot: FileSnapshot): ValidationResult {
    const errors: string[] = [];
    const warnings: string[] = [];

    // 检查必需字段
    if (!snapshot.id) {
      errors.push('快照 ID 缺失');
    }

    if (!snapshot.filePath) {
      errors.push('文件路径缺失');
    }

    if (!snapshot.originalContent) {
      warnings.push('快照内容为空');
    }

    if (!snapshot.timestamp) {
      errors.push('时间戳缺失');
    }

    if (!snapshot.workerId) {
      warnings.push('Worker 信息缺失');
    }

    // 检查文件路径有效性
    if (snapshot.filePath) {
      const fullPath = path.resolve(this.workspaceRoot, snapshot.filePath);
      if (!fullPath.startsWith(this.workspaceRoot)) {
        errors.push(`文件路径超出工作区范围: ${snapshot.filePath}`);
      }
    }

    return {
      valid: errors.length === 0,
      errors,
      warnings,
    };
  }

  /**
   * 检查快照是否存在
   */
  snapshotExists(sessionId: string, filePath: string): boolean {
    const snapshots = this.sessionManager.getSession(sessionId)?.snapshots || [];
    return snapshots.some((s: FileSnapshotMeta) => s.filePath === filePath);
  }

  /**
   * 检查快照文件是否存在
   */
  snapshotFileExists(snapshotFilePath: string): boolean {
    return fs.existsSync(snapshotFilePath);
  }

  /**
   * 验证快照内容与文件内容是否一致
   */
  validateSnapshotContent(
    snapshotContent: string,
    currentContent: string
  ): ValidationResult {
    const errors: string[] = [];
    const warnings: string[] = [];

    if (snapshotContent === currentContent) {
      warnings.push('快照内容与当前文件内容相同，可能不需要快照');
    }

    if (snapshotContent.length === 0) {
      warnings.push('快照内容为空');
    }

    if (currentContent.length === 0) {
      warnings.push('当前文件内容为空');
    }

    return {
      valid: errors.length === 0,
      errors,
      warnings,
    };
  }

  /**
   * 检查快照冲突
   */
  checkConflicts(
    sessionId: string,
    filePath: string,
    currentContent: string
  ): {
    hasConflict: boolean;
    conflictType?: 'modified' | 'deleted' | 'none';
    message?: string;
  } {
    const snapshots = this.sessionManager.getSession(sessionId)?.snapshots || [];
    const snapshot = snapshots.find(s => s.filePath === filePath);

    if (!snapshot) {
      return { hasConflict: false, conflictType: 'none' };
    }

    const fullPath = path.resolve(this.workspaceRoot, filePath);

    // 检查文件是否被删除
    if (!fs.existsSync(fullPath)) {
      return {
        hasConflict: true,
        conflictType: 'deleted',
        message: `文件已被删除: ${filePath}`,
      };
    }

    // 检查文件是否被修改
    try {
      const fileContent = fs.readFileSync(fullPath, 'utf-8');
      if (fileContent !== currentContent) {
        return {
          hasConflict: true,
          conflictType: 'modified',
          message: `文件已被修改: ${filePath}`,
        };
      }
    } catch (error) {
      logger.error('快照.冲突检查失败', { filePath, error }, LogCategory.RECOVERY);
      return {
        hasConflict: true,
        conflictType: 'modified',
        message: `无法读取文件: ${filePath}`,
      };
    }

    return { hasConflict: false, conflictType: 'none' };
  }

  /**
   * 验证会话目录
   */
  validateSessionDirectory(sessionId: string): ValidationResult {
    const errors: string[] = [];
    const warnings: string[] = [];

    const snapshotDir = this.sessionManager.getSnapshotsDir(sessionId);

    if (!fs.existsSync(snapshotDir)) {
      errors.push(`快照目录不存在: ${snapshotDir}`);
    } else {
      try {
        const stats = fs.statSync(snapshotDir);
        if (!stats.isDirectory()) {
          errors.push(`快照路径不是目录: ${snapshotDir}`);
        }
      } catch (error) {
        errors.push(`无法访问快照目录: ${snapshotDir}`);
      }
    }

    return {
      valid: errors.length === 0,
      errors,
      warnings,
    };
  }
}
