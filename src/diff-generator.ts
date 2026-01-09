/**
 * Diff 生成器
 * 本地 Diff 生成，对比快照与当前文件
 */

import * as fs from 'fs';
import * as path from 'path';
import { DiffHunk, CLIType } from './types';
import { SessionManager } from './session-manager';

/**
 * Diff 结果
 */
export interface DiffResult {
  filePath: string;
  hunks: DiffHunk[];
  additions: number;
  deletions: number;
  source: CLIType;
}

/**
 * Diff 生成器
 */
export class DiffGenerator {
  private sessionManager: SessionManager;
  private workspaceRoot: string;

  constructor(sessionManager: SessionManager, workspaceRoot: string) {
    this.sessionManager = sessionManager;
    this.workspaceRoot = workspaceRoot;
  }

  /** 生成文件 Diff */
  generateDiff(filePath: string): DiffResult | null {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return null;

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) return null;

    const absolutePath = path.join(this.workspaceRoot, relativePath);
    const currentContent = fs.existsSync(absolutePath)
      ? fs.readFileSync(absolutePath, 'utf-8')
      : '';

    const hunks = this.computeHunks(
      snapshot.originalContent,
      currentContent,
      relativePath,
      snapshot.lastModifiedBy
    );

    let additions = 0;
    let deletions = 0;
    for (const hunk of hunks) {
      const lines = hunk.content.split('\n');
      for (const line of lines) {
        if (line.startsWith('+') && !line.startsWith('+++')) additions++;
        if (line.startsWith('-') && !line.startsWith('---')) deletions++;
      }
    }

    return {
      filePath: relativePath,
      hunks,
      additions,
      deletions,
      source: snapshot.lastModifiedBy,
    };
  }

  /** 计算 Diff Hunks */
  private computeHunks(
    original: string,
    current: string,
    filePath: string,
    source: CLIType
  ): DiffHunk[] {
    const originalLines = original.split('\n');
    const currentLines = current.split('\n');
    const hunks: DiffHunk[] = [];

    // 使用简单的 LCS 算法计算差异
    const diff = this.simpleDiff(originalLines, currentLines);
    
    if (diff.length === 0) return hunks;

    // 将连续的差异合并为 hunk
    let currentHunk: { oldStart: number; oldLines: string[]; newStart: number; newLines: string[] } | null = null;
    let oldLineNum = 1;
    let newLineNum = 1;

    for (const item of diff) {
      if (item.type === 'equal') {
        if (currentHunk) {
          // 结束当前 hunk
          hunks.push(this.createHunk(currentHunk, filePath, source));
          currentHunk = null;
        }
        oldLineNum++;
        newLineNum++;
      } else {
        if (!currentHunk) {
          currentHunk = {
            oldStart: oldLineNum,
            oldLines: [],
            newStart: newLineNum,
            newLines: [],
          };
        }
        if (item.type === 'delete') {
          currentHunk.oldLines.push(item.line);
          oldLineNum++;
        } else if (item.type === 'insert') {
          currentHunk.newLines.push(item.line);
          newLineNum++;
        }
      }
    }

    // 处理最后一个 hunk
    if (currentHunk) {
      hunks.push(this.createHunk(currentHunk, filePath, source));
    }

    return hunks;
  }

  /** 创建 DiffHunk */
  private createHunk(
    data: { oldStart: number; oldLines: string[]; newStart: number; newLines: string[] },
    filePath: string,
    source: CLIType
  ): DiffHunk {
    const content = [
      ...data.oldLines.map(l => `-${l}`),
      ...data.newLines.map(l => `+${l}`),
    ].join('\n');

    return {
      filePath,
      oldStart: data.oldStart,
      oldLines: data.oldLines.length,
      newStart: data.newStart,
      newLines: data.newLines.length,
      content,
      source,
    };
  }

  /** 简单 Diff 算法 */
  private simpleDiff(
    oldLines: string[],
    newLines: string[]
  ): Array<{ type: 'equal' | 'delete' | 'insert'; line: string }> {
    const result: Array<{ type: 'equal' | 'delete' | 'insert'; line: string }> = [];
    
    // 使用简单的逐行比较（可以后续优化为 Myers 算法）
    let i = 0, j = 0;
    
    while (i < oldLines.length || j < newLines.length) {
      if (i >= oldLines.length) {
        result.push({ type: 'insert', line: newLines[j] });
        j++;
      } else if (j >= newLines.length) {
        result.push({ type: 'delete', line: oldLines[i] });
        i++;
      } else if (oldLines[i] === newLines[j]) {
        result.push({ type: 'equal', line: oldLines[i] });
        i++;
        j++;
      } else {
        // 查找最近的匹配
        const oldMatch = newLines.indexOf(oldLines[i], j);
        const newMatch = oldLines.indexOf(newLines[j], i);
        
        if (oldMatch === -1 && newMatch === -1) {
          result.push({ type: 'delete', line: oldLines[i] });
          result.push({ type: 'insert', line: newLines[j] });
          i++;
          j++;
        } else if (oldMatch !== -1 && (newMatch === -1 || oldMatch - j <= newMatch - i)) {
          while (j < oldMatch) {
            result.push({ type: 'insert', line: newLines[j] });
            j++;
          }
        } else {
          while (i < newMatch) {
            result.push({ type: 'delete', line: oldLines[i] });
            i++;
          }
        }
      }
    }
    
    return result;
  }

  /** 生成所有待处理文件的 Diff */
  generateAllDiffs(): DiffResult[] {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return [];

    const results: DiffResult[] = [];
    for (const snapshot of session.snapshots) {
      const diff = this.generateDiff(snapshot.filePath);
      if (diff && (diff.additions > 0 || diff.deletions > 0)) {
        results.push(diff);
      }
    }
    return results;
  }

  /** 格式化 Diff 为统一格式字符串 */
  formatDiff(diff: DiffResult): string {
    const lines: string[] = [
      `--- a/${diff.filePath}`,
      `+++ b/${diff.filePath}`,
    ];

    for (const hunk of diff.hunks) {
      lines.push(`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`);
      lines.push(hunk.content);
    }

    return lines.join('\n');
  }
}

