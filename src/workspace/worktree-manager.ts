/**
 * WorktreeManager — Git Worktree 沙盒隔离管理器
 *
 * 为每个需要写操作的 Worker 任务创建独立的 git worktree，
 * 实现文件系统级别的物理隔离，从根源消除多 Worker 并行写冲突。
 *
 * 生命周期：
 *   1. acquire(taskId) → 创建 worktree + 分支，返回隔离路径
 *   2. Worker 在隔离路径下执行所有文件操作
 *   3. merge(taskId)  → 将分支 merge 回主分支
 *   4. release(taskId) → 清理 worktree 目录和临时分支
 *
 * 设计约束：
 * - 仅在 git 仓库内可用（非 git 项目回退到共享模式）
 * - worktree 路径：{workspaceRoot}/.magi/worktrees/task-{id}
 * - 分支名称：magi/worker/{taskId}
 */

import * as fs from 'fs';
import * as path from 'path';
import { execSync } from 'child_process';
import { logger, LogCategory } from '../logging';

/** Worktree 分配结果 */
export interface WorktreeAllocation {
  /** worktree 的绝对路径（Worker 应在此目录下操作文件） */
  worktreePath: string;
  /** 分支名称 */
  branchName: string;
  /** 基准分支（创建时的 HEAD） */
  baseBranch: string;
}

/** Merge 结果 */
export interface WorktreeMergeResult {
  success: boolean;
  /** merge 是否有冲突 */
  hasConflicts: boolean;
  /** 冲突文件列表 */
  conflictFiles: string[];
  /** 错误信息（如果失败） */
  error?: string;
  /** 冲突摘要（面向用户） */
  conflictSummary?: string;
  /** 冲突修复建议（面向用户） */
  conflictHints?: string[];
}

/** 孤儿 worktree 对账结果 */
export interface WorktreeReconcileResult {
  removedWorktrees: string[];
  removedBranches: string[];
  errors: string[];
}

export class WorktreeManager {
  private readonly workspaceRoot: string;
  private readonly worktreeBaseDir: string;
  /** 已分配的 worktree 映射：taskId → allocation */
  private readonly allocations = new Map<string, WorktreeAllocation>();
  /** 是否为 git 仓库 */
  private isGitRepo: boolean | null = null;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.worktreeBaseDir = path.join(workspaceRoot, '.magi', 'worktrees');
  }

  /**
   * 检测当前 workspace 是否为 git 仓库
   */
  isGitRepository(): boolean {
    if (this.isGitRepo !== null) {
      return this.isGitRepo;
    }
    try {
      this.git('rev-parse', '--is-inside-work-tree');
      this.isGitRepo = true;
    } catch {
      this.isGitRepo = false;
    }
    return this.isGitRepo;
  }

  /**
   * 为任务分配一个隔离的 worktree
   *
   * @param taskId  任务唯一标识
   * @returns WorktreeAllocation，或 null（非 git 仓库时）
   */
  acquire(taskId: string): WorktreeAllocation | null {
    if (!this.isGitRepository()) {
      logger.info('WorktreeManager.acquire.跳过', {
        taskId,
        reason: '非 git 仓库',
      }, LogCategory.ORCHESTRATOR);
      return null;
    }

    // 幂等：如果已分配则直接返回
    const existing = this.allocations.get(taskId);
    if (existing) {
      return existing;
    }

    const sanitizedId = this.sanitizeTaskId(taskId);
    const branchName = `magi/worker/${sanitizedId}`;
    const worktreePath = path.join(this.worktreeBaseDir, `task-${sanitizedId}`);

    try {
      const reconcileResult = this.reconcileOrphanedWorktrees();
      if (reconcileResult.removedWorktrees.length > 0 || reconcileResult.removedBranches.length > 0) {
        logger.info('WorktreeManager.acquire.孤儿清理完成', {
          taskId,
          removedWorktrees: reconcileResult.removedWorktrees.length,
          removedBranches: reconcileResult.removedBranches.length,
        }, LogCategory.ORCHESTRATOR);
      }

      // 获取当前 HEAD 分支名（作为 baseBranch）
      const baseBranch = this.getCurrentBranch();

      // 确保 worktree 目录的父级存在
      fs.mkdirSync(this.worktreeBaseDir, { recursive: true });

      // 清理可能残留的旧 worktree（崩溃恢复）
      this.cleanupStaleWorktree(worktreePath, branchName);

      // 创建 worktree + 新分支（基于当前 HEAD）
      this.git('worktree', 'add', '-b', branchName, worktreePath, 'HEAD');

      const allocation: WorktreeAllocation = {
        worktreePath,
        branchName,
        baseBranch,
      };
      this.allocations.set(taskId, allocation);

      logger.info('WorktreeManager.acquire.成功', {
        taskId,
        worktreePath,
        branchName,
        baseBranch,
      }, LogCategory.ORCHESTRATOR);

      return allocation;
    } catch (error: any) {
      logger.error('WorktreeManager.acquire.失败', {
        taskId,
        worktreePath,
        error: error.message,
      }, LogCategory.ORCHESTRATOR);
      // 失败时清理残留
      this.cleanupStaleWorktree(worktreePath, branchName);
      return null;
    }
  }

  /**
   * 将 worktree 分支的改动 merge 回主分支
   *
   * @param taskId  任务唯一标识
   * @returns merge 结果
   */
  merge(taskId: string): WorktreeMergeResult {
    const allocation = this.allocations.get(taskId);
    if (!allocation) {
      return {
        success: false,
        hasConflicts: false,
        conflictFiles: [],
        error: `未找到任务 ${taskId} 的 worktree 分配`,
      };
    }

    try {
      // 在 worktree 中 commit 所有未提交的改动
      this.commitWorktreeChanges(allocation);

      // 回到主 workspace，merge worktree 分支
      this.git('merge', '--no-ff', '--no-edit', allocation.branchName);

      logger.info('WorktreeManager.merge.成功', {
        taskId,
        branchName: allocation.branchName,
      }, LogCategory.ORCHESTRATOR);

      return { success: true, hasConflicts: false, conflictFiles: [] };
    } catch (error: any) {
      const errorMsg = error.message || String(error);
      const stderrText = typeof error?.stderr === 'string' ? error.stderr : '';
      const conflictFiles = this.getConflictFiles();
      const conflictDetected = conflictFiles.length > 0
        || errorMsg.includes('CONFLICT')
        || errorMsg.includes('Merge conflict')
        || stderrText.includes('CONFLICT')
        || stderrText.includes('Merge conflict');

      // 检测 merge 冲突
      if (conflictDetected) {
        logger.warn('WorktreeManager.merge.冲突', {
          taskId,
          branchName: allocation.branchName,
          conflictFiles,
        }, LogCategory.ORCHESTRATOR);
        const guidance = this.buildMergeConflictGuidance(conflictFiles);

        // 放弃 merge，保留现场供后续处理
        try {
          this.git('merge', '--abort');
        } catch {
          // merge --abort 失败不致命
        }

        return {
          success: false,
          hasConflicts: true,
          conflictFiles,
          error: `合并冲突：${conflictFiles.join(', ')}`,
          conflictSummary: guidance.summary,
          conflictHints: guidance.hints,
        };
      }

      logger.error('WorktreeManager.merge.失败', {
        taskId,
        error: errorMsg,
      }, LogCategory.ORCHESTRATOR);

      return {
        success: false,
        hasConflicts: false,
        conflictFiles: [],
        error: errorMsg,
      };
    }
  }

  /**
   * 释放 worktree（清理目录和分支）
   */
  release(taskId: string): void {
    const allocation = this.allocations.get(taskId);
    if (!allocation) {
      return;
    }

    try {
      // 移除 worktree
      try {
        this.git('worktree', 'remove', '--force', allocation.worktreePath);
      } catch {
        // worktree 目录可能已被手动删除
        this.git('worktree', 'prune');
      }

      // 删除临时分支
      try {
        this.git('branch', '-D', allocation.branchName);
      } catch {
        // 分支可能不存在（已被 merge 删除等）
      }

      this.allocations.delete(taskId);

      logger.info('WorktreeManager.release.成功', {
        taskId,
        branchName: allocation.branchName,
      }, LogCategory.ORCHESTRATOR);
    } catch (error: any) {
      logger.warn('WorktreeManager.release.清理异常', {
        taskId,
        error: error.message,
      }, LogCategory.ORCHESTRATOR);
      // 无论如何从 allocations 中移除
      this.allocations.delete(taskId);
    }
  }

  /**
   * 批量释放所有 worktree（用于会话切换/中断清理）
   */
  releaseAll(): void {
    const taskIds = [...this.allocations.keys()];
    for (const taskId of taskIds) {
      this.release(taskId);
    }
    const reconcileResult = this.reconcileOrphanedWorktrees();
    if (reconcileResult.removedWorktrees.length > 0 || reconcileResult.removedBranches.length > 0) {
      logger.info('WorktreeManager.releaseAll.孤儿清理完成', {
        removedWorktrees: reconcileResult.removedWorktrees.length,
        removedBranches: reconcileResult.removedBranches.length,
      }, LogCategory.ORCHESTRATOR);
    }
  }

  /**
   * 对账并清理孤儿 worktree/分支（进程中断恢复）
   */
  reconcileOrphanedWorktrees(): WorktreeReconcileResult {
    const result: WorktreeReconcileResult = {
      removedWorktrees: [],
      removedBranches: [],
      errors: [],
    };
    if (!this.isGitRepository()) {
      return result;
    }
    fs.mkdirSync(this.worktreeBaseDir, { recursive: true });

    const trackedEntries = this.listTrackedWorktreeEntries();
    const trackedPaths = new Set<string>(trackedEntries.map((entry) => path.resolve(entry.path)));
    const trackedBranches = new Set<string>(
      trackedEntries
        .map((entry) => entry.branch)
        .filter((branch): branch is string => typeof branch === 'string' && branch.length > 0),
    );

    let localEntries: fs.Dirent[] = [];
    try {
      localEntries = fs.readdirSync(this.worktreeBaseDir, { withFileTypes: true });
    } catch (error: any) {
      result.errors.push(`扫描 worktree 目录失败: ${error?.message || String(error)}`);
      return result;
    }

    for (const entry of localEntries) {
      if (!entry.isDirectory()) {
        continue;
      }
      const absolutePath = path.resolve(path.join(this.worktreeBaseDir, entry.name));
      if (trackedPaths.has(absolutePath)) {
        continue;
      }
      try {
        this.git('worktree', 'remove', '--force', absolutePath);
      } catch {
        try {
          fs.rmSync(absolutePath, { recursive: true, force: true });
        } catch (error: any) {
          result.errors.push(`移除孤儿 worktree 目录失败(${absolutePath}): ${error?.message || String(error)}`);
        }
      }
      result.removedWorktrees.push(absolutePath);

      const branchName = this.deriveBranchNameFromWorktreePath(absolutePath);
      if (branchName && !trackedBranches.has(branchName)) {
        try {
          this.git('branch', '-D', branchName);
          result.removedBranches.push(branchName);
        } catch {
          // 分支不存在或已删除，忽略
        }
      }
    }

    const activeBranches = new Set<string>(
      Array.from(this.allocations.values()).map((allocation) => allocation.branchName),
    );
    try {
      const workerBranchesRaw = this.git('branch', '--list', 'magi/worker/*', '--format', '%(refname:short)');
      const workerBranches = workerBranchesRaw
        .split('\n')
        .map((item) => item.trim())
        .filter(Boolean);
      for (const branchName of workerBranches) {
        if (trackedBranches.has(branchName) || activeBranches.has(branchName)) {
          continue;
        }
        const taskId = branchName.replace(/^magi\/worker\//, '');
        const expectedPath = path.resolve(path.join(this.worktreeBaseDir, `task-${taskId}`));
        if (fs.existsSync(expectedPath)) {
          continue;
        }
        try {
          this.git('branch', '-D', branchName);
          result.removedBranches.push(branchName);
        } catch {
          // 分支删除失败不阻塞主流程
        }
      }
    } catch (error: any) {
      result.errors.push(`扫描孤儿分支失败: ${error?.message || String(error)}`);
    }

    try {
      this.git('worktree', 'prune');
    } catch (error: any) {
      result.errors.push(`worktree prune 失败: ${error?.message || String(error)}`);
    }

    return result;
  }

  /**
   * 获取指定任务的 worktree 分配信息
   */
  getAllocation(taskId: string): WorktreeAllocation | undefined {
    return this.allocations.get(taskId);
  }

  /**
   * 当前活跃的 worktree 数量
   */
  get activeCount(): number {
    return this.allocations.size;
  }

  // ============================================================================
  // 内部方法
  // ============================================================================

  /** 执行 git 命令（在主 workspace 目录下） */
  private git(...args: string[]): string {
    const cmd = `git ${args.map(a => this.shellEscape(a)).join(' ')}`;
    return execSync(cmd, {
      cwd: this.workspaceRoot,
      encoding: 'utf-8',
      timeout: 30_000,
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
  }

  /** 在 worktree 目录下执行 git 命令 */
  private gitInWorktree(worktreePath: string, ...args: string[]): string {
    const cmd = `git ${args.map(a => this.shellEscape(a)).join(' ')}`;
    return execSync(cmd, {
      cwd: worktreePath,
      encoding: 'utf-8',
      timeout: 30_000,
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
  }

  /** 获取当前分支名 */
  private getCurrentBranch(): string {
    try {
      return this.git('rev-parse', '--abbrev-ref', 'HEAD');
    } catch {
      // detached HEAD 等场景
      return this.git('rev-parse', '--short', 'HEAD');
    }
  }

  /** 在 worktree 中提交所有改动 */
  private commitWorktreeChanges(allocation: WorktreeAllocation): void {
    try {
      // 检查是否有未暂存的改动
      const status = this.gitInWorktree(allocation.worktreePath, 'status', '--porcelain');
      if (!status.trim()) {
        return; // 无改动，跳过 commit
      }

      // 暂存所有改动
      this.gitInWorktree(allocation.worktreePath, 'add', '-A');

      // 提交
      this.gitInWorktree(
        allocation.worktreePath,
        'commit', '-m', `[magi] Worker task: ${allocation.branchName}`,
        '--no-verify' // 跳过 pre-commit hooks
      );
    } catch (error: any) {
      // commit 失败不致命（可能无改动）
      logger.debug('WorktreeManager.commitWorktreeChanges.跳过', {
        branchName: allocation.branchName,
        reason: error.message,
      }, LogCategory.ORCHESTRATOR);
    }
  }

  /** 获取冲突文件列表 */
  private getConflictFiles(): string[] {
    try {
      const output = this.git('diff', '--name-only', '--diff-filter=U');
      return output.split('\n').filter(Boolean);
    } catch {
      return [];
    }
  }

  /** 清理残留的 worktree（崩溃恢复） */
  private cleanupStaleWorktree(worktreePath: string, branchName: string): void {
    // 如果 worktree 目录存在，先移除
    if (fs.existsSync(worktreePath)) {
      try {
        this.git('worktree', 'remove', '--force', worktreePath);
      } catch {
        // 强制清理目录
        try {
          fs.rmSync(worktreePath, { recursive: true, force: true });
        } catch {
          // 忽略
        }
        try {
          this.git('worktree', 'prune');
        } catch {
          // 忽略
        }
      }
    }

    // 如果分支存在，先删除
    try {
      this.git('branch', '-D', branchName);
    } catch {
      // 分支不存在，正常
    }
  }

  private listTrackedWorktreeEntries(): Array<{ path: string; branch?: string }> {
    let output = '';
    try {
      output = this.git('worktree', 'list', '--porcelain');
    } catch {
      return [];
    }
    const lines = output.split('\n');
    const entries: Array<{ path: string; branch?: string }> = [];
    let current: { path?: string; branch?: string } = {};
    for (const raw of lines) {
      const line = raw.trim();
      if (!line) {
        if (current.path) {
          entries.push({ path: current.path, branch: current.branch });
        }
        current = {};
        continue;
      }
      if (line.startsWith('worktree ')) {
        current.path = line.slice('worktree '.length).trim();
        continue;
      }
      if (line.startsWith('branch ')) {
        const ref = line.slice('branch '.length).trim();
        current.branch = ref.startsWith('refs/heads/') ? ref.slice('refs/heads/'.length) : ref;
      }
    }
    if (current.path) {
      entries.push({ path: current.path, branch: current.branch });
    }
    return entries;
  }

  private deriveBranchNameFromWorktreePath(worktreePath: string): string | undefined {
    const baseName = path.basename(worktreePath);
    if (!baseName.startsWith('task-')) {
      return undefined;
    }
    const taskId = baseName.slice('task-'.length).trim();
    if (!taskId) {
      return undefined;
    }
    return `magi/worker/${taskId}`;
  }

  private buildMergeConflictGuidance(conflictFiles: string[]): { summary: string; hints: string[] } {
    const files = conflictFiles.filter(Boolean);
    const preview = files.slice(0, 6).join(', ');
    const summary = files.length > 0
      ? `Worktree 合并冲突：${files.length} 个文件存在并行修改（${preview}${files.length > 6 ? ', ...' : ''}）`
      : 'Worktree 合并冲突：存在并行修改但未能解析具体文件';
    const hints = [
      '先在主工作区执行 `git status` 与 `git diff --name-only --diff-filter=U`，确认冲突范围。',
      '按冲突文件逐个完成冲突消解，确保保留当前任务与主分支两侧关键改动。',
      '冲突消解后执行 `git add -A`，再继续当前任务流或重新触发该 assignment。',
    ];
    return { summary, hints };
  }

  /** 清理 taskId 中的特殊字符，生成安全的分支/目录名 */
  private sanitizeTaskId(taskId: string): string {
    return taskId.replace(/[^a-zA-Z0-9_-]/g, '_').substring(0, 64);
  }

  /** Shell 转义 */
  private shellEscape(arg: string): string {
    // 对于简单的参数直接返回
    if (/^[a-zA-Z0-9_./-]+$/.test(arg)) {
      return arg;
    }
    // 使用单引号包裹，内部单引号转义
    return `'${arg.replace(/'/g, "'\\''")}'`;
  }
}
