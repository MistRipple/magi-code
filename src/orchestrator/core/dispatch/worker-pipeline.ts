/**
 * WorkerPipeline - 统一 Worker 执行管道
 *
 * 从 AssignmentExecutor 提取的核心逻辑（L3 统一架构重构）。
 * 职责：围绕 Worker.executeAssignment 提供可配置的治理包装：
 * - [可选] Snapshot 创建
 * - [可选] LSP 预检/后检
 * - [可选] 目标变更检测 + 强制重试
 * - [可选] ContextManager 更新
 *
 * 设计原则：
 * - 不依赖 Mission 对象（使用 missionId 字符串代替）
 * - 所有治理步骤通过 PipelineConfig 的开关控制
 * - 由 DispatchManager.launchDispatchWorker 根据 governance 参数自动计算开关
 */

import fs from 'fs';
import path from 'path';
import type { WorkerSlot } from '../../../types';
import type { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import type { AutonomousWorker, AutonomousExecutionResult } from '../../worker';
import type { Assignment } from '../../mission';
import type { PlanMode } from '../../plan-ledger';
import type { SnapshotManager } from '../../../snapshot-manager';
import type { ReportCallback } from '../../protocols/worker-report';
import type { CancellationToken } from './dispatch-batch';
import { LspEnforcer } from '../../lsp/lsp-enforcer';
import { logger, LogCategory } from '../../../logging';
import type { AssembledContext } from '../../../context/context-assembler';
import { t } from '../../../i18n';
import type { GitHost } from '../../../host';
import type { WorktreeAllocation, WorktreeMergeResult } from '../../../workspace/worktree-manager';

type WorkspaceWriteIsolationMode = 'git_worktree' | 'workspace_serial';

// ============================================================================
// 配置与结果类型
// ============================================================================

export interface PipelineConfig {
  // 基本信息（必选）
  assignment: Assignment;
  workerInstance: AutonomousWorker;
  adapterFactory: IAdapterFactory;
  workspaceRoot: string;

  // 执行选项
  projectContext?: string;
  onReport?: ReportCallback;
  heartbeatIntervalMs?: number;
  cancellationToken?: CancellationToken;
  imagePaths?: string[];
  missionId?: string;
  requestId?: string;
  sessionId?: string;
  resumeSessionId?: string;
  resumePrompt?: string;
  planningMode?: PlanMode;

  // 治理开关（由 DispatchManager 根据 governance 参数计算）
  enableSnapshot: boolean;
  enableLSP: boolean;
  enableTargetEnforce: boolean;
  enableContextUpdate: boolean;

  // 外部依赖（可选注入）
  snapshotManager?: SnapshotManager | null;
  contextManager?: import('../../../context/context-manager').ContextManager | null;
  todoManager?: import('../../../todo').TodoManager | null;
  /** Git 隔离宿主（当任务需要写操作时注入） */
  gitHost?: GitHost | null;

  // 反应式编排：补充指令回调（由 DispatchManager 从 SupplementaryInstructionQueue 注入）
  getSupplementaryInstructions?: () => string[];
}

export interface PipelineResult {
  executionResult: AutonomousExecutionResult;
  lspNewErrors: string[];
  targetChangeDetected: boolean;
  /** Worktree merge 结果（仅在 worktree 隔离模式下有值） */
  worktreeMerge?: WorktreeMergeResult;
}

// ============================================================================
// WorkerPipeline
// ============================================================================

export class WorkerPipeline {
  private lspEnforcer: LspEnforcer | null = null;

  async execute(config: PipelineConfig): Promise<PipelineResult> {
    const {
      assignment, workerInstance, adapterFactory, workspaceRoot,
      projectContext, onReport, heartbeatIntervalMs, cancellationToken, imagePaths,
      sessionId, resumeSessionId, resumePrompt,
      enableSnapshot, enableLSP, enableTargetEnforce, enableContextUpdate,
      snapshotManager, contextManager, todoManager,
      getSupplementaryInstructions,
      gitHost,
    } = config;
    const missionId = config.missionId || 'dispatch';
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    const normalizedRequestId = typeof config.requestId === 'string' && config.requestId.trim().length > 0
      ? config.requestId.trim()
      : (typeof assignment.trace?.requestId === 'string' && assignment.trace.requestId.trim().length > 0
          ? assignment.trace.requestId.trim()
          : undefined);
    const normalizedTurnId = typeof assignment.trace?.turnId === 'string' && assignment.trace.turnId.trim().length > 0
      ? assignment.trace.turnId.trim()
      : undefined;
    const messageSessionId = normalizedSessionId
      || (typeof assignment.trace?.sessionId === 'string' && assignment.trace.sessionId.trim().length > 0
        ? assignment.trace.sessionId.trim()
        : undefined);
    const dispatchWaveId = typeof assignment.trace?.batchId === 'string' && assignment.trace.batchId.trim().length > 0
      ? assignment.trace.batchId.trim()
      : undefined;
    const laneId = dispatchWaveId ? `${dispatchWaveId}:${assignment.workerId}` : undefined;
    const workerCardId = dispatchWaveId
      ? `worker-lane-instruction-${dispatchWaveId}-${assignment.workerId}`
      : undefined;
    const adapterMessageMetadata = {
      assignmentId: assignment.id,
      missionId,
      ...(normalizedRequestId ? { requestId: normalizedRequestId } : {}),
      worker: assignment.workerId,
      ...(messageSessionId ? { sessionId: messageSessionId } : {}),
      ...(normalizedTurnId ? { turnId: normalizedTurnId } : {}),
      ...(dispatchWaveId ? { dispatchWaveId } : {}),
      ...(laneId ? { laneId } : {}),
      ...(workerCardId ? { workerCardId } : {}),
    };

    // ========== 0. [可选] 写隔离 ==========
    // Git 仓库：优先使用 worktree 物理隔离；分配失败或非 Git 工作区：自动降级为主工作区串行写模式。
    const requiresWrite = assignment.scope?.requiresModification ?? false;
    let worktreeAllocation: WorktreeAllocation | null = null;
    let writeIsolationMode: WorkspaceWriteIsolationMode | null = null;
    if (requiresWrite) {
      if (gitHost && gitHost.isGitRepository(workspaceRoot)) {
        worktreeAllocation = gitHost.acquireWorktree({
          workspacePath: workspaceRoot,
          taskId: assignment.id,
        });
        if (worktreeAllocation) {
          writeIsolationMode = 'git_worktree';
        } else {
          // worktree 分配失败——降级到串行写模式继续执行
          writeIsolationMode = 'workspace_serial';
          logger.warn('WorkerPipeline.Worktree.分配失败_降级串行写模式', {
            assignmentId: assignment.id,
            worker: assignment.workerId,
            workspaceRoot,
          }, LogCategory.ORCHESTRATOR);
        }
      } else {
        // 无 Git 环境或非 Git 仓库——使用串行写模式
        writeIsolationMode = 'workspace_serial';
        logger.info('WorkerPipeline.Worktree.降级_非Git工作区串行写模式', {
          assignmentId: assignment.id,
          worker: assignment.workerId,
          requiresWrite,
          workspaceRoot,
          reason: !gitHost ? 'gitHost不可用' : '非Git仓库',
        }, LogCategory.ORCHESTRATOR);
      }
    }
    // Worker 的有效工作目录：worktree 路径 > 原始 workspaceRoot
    const effectiveWorkspaceRoot = worktreeAllocation?.worktreePath ?? workspaceRoot;

    logger.info(
      'WorkerPipeline.开始',
      {
        assignmentId: assignment.id,
        worker: assignment.workerId,
        governance: { enableSnapshot, enableLSP, enableTargetEnforce, enableContextUpdate },
        worktreeIsolated: !!worktreeAllocation,
        writeIsolationMode: writeIsolationMode ?? (requiresWrite ? 'workspace_serial' : 'none'),
        effectiveWorkspaceRoot: worktreeAllocation ? effectiveWorkspaceRoot : undefined,
      },
      LogCategory.ORCHESTRATOR
    );

    // ========== 1. [可选] 快照创建 ==========
    if (enableSnapshot && snapshotManager) {
      await this.createSnapshots(snapshotManager, missionId, assignment, normalizedSessionId);
    }

    // ========== 2. 设置工具级快照上下文 ==========
    const toolManager = adapterFactory.getToolManager();
    if (normalizedSessionId) {
      toolManager.setSnapshotContext({
        sessionId: normalizedSessionId,
        missionId,
        requestId: normalizedRequestId,
        assignmentId: assignment.id,
        todoId: assignment.id,
        workerId: assignment.workerId,
      });
    } else {
      logger.warn('WorkerPipeline.快照上下文.跳过_缺少会话', {
        assignmentId: assignment.id,
        missionId,
      }, LogCategory.ORCHESTRATOR);
    }

    // ========== 3/4/5. 上下文快照 + 目标文件收集 + LSP 预检（并行执行） ==========
    // 这三个步骤无互依赖，并行执行减少总耗时
    const targetFiles = this.collectTargetFiles(assignment);
    const normalizedTargets = this.normalizeTargetFiles(targetFiles, effectiveWorkspaceRoot);
    let preExecutionContents: Map<string, string> | null = null;
    if (enableTargetEnforce && normalizedTargets.length > 0) {
      preExecutionContents = this.captureTargetContents(normalizedTargets, effectiveWorkspaceRoot);
    }

    const assembledContextPromise = enableContextUpdate && contextManager
      ? this.generateAssembledContext(missionId, assignment.workerId, contextManager)
      : Promise.resolve(undefined);

    let preflightDiagnostics: string[] = [];
    const lspPromise = enableLSP
      ? (async () => {
          if (!this.lspEnforcer) {
            this.lspEnforcer = new LspEnforcer(workspaceRoot, toolManager);
          }
          try {
            await this.lspEnforcer.applyIfNeeded(assignment);
            preflightDiagnostics = await this.lspEnforcer.captureDiagnostics(assignment);
          } catch (error: any) {
            logger.warn('WorkerPipeline.LSP预检失败', {
              assignmentId: assignment.id, error: error?.message,
            }, LogCategory.ORCHESTRATOR);
          }
        })()
      : Promise.resolve();

    const [assembledContext] = await Promise.all([assembledContextPromise, lspPromise]);

    // ========== 6. Worker 执行 ==========
    let result: AutonomousExecutionResult;
    const lspNewErrors: string[] = [];
    let targetChangeDetected = true;

    if (cancellationToken) {
      cancellationToken.onCancel((reason) => {
        void adapterFactory.interrupt(assignment.workerId as WorkerSlot).catch((error: any) => {
          logger.warn('WorkerPipeline.取消中断失败', {
            assignmentId: assignment.id,
            worker: assignment.workerId,
            reason,
            error: error?.message || String(error),
          }, LogCategory.ORCHESTRATOR);
        });
      });
    }

    try {
      result = await workerInstance.executeAssignment(assignment, {
        workingDirectory: effectiveWorkspaceRoot,
        adapterFactory,
        adapterScope: {
          ...(config.planningMode ? { planningMode: config.planningMode } : {}),
          messageMetadata: adapterMessageMetadata,
        },
        projectContext,
        onReport,
        heartbeatIntervalMs,
        cancellationToken,
        imagePaths,
        getSupplementaryInstructions,
        sessionId: resumeSessionId,
        resumePrompt,
        preAssembledContext: assembledContext,
      });

      // ========== 7. [可选] 目标变更检测 + 强制重试 ==========
      if (enableTargetEnforce && preExecutionContents && normalizedTargets.length > 0
          && assignment.scope?.requiresModification) {
        const hasChanges = this.hasContentChanges(normalizedTargets, preExecutionContents, effectiveWorkspaceRoot)
          || (snapshotManager ? this.hasAssignmentChanges(snapshotManager, assignment.id, normalizedTargets) : false);

        if (!hasChanges) {
          logger.warn(
            `WorkerPipeline: ${assignment.workerId} 未产生目标文件变更，触发强制重试`,
            { assignmentId: assignment.id, targetFiles: normalizedTargets },
            LogCategory.ORCHESTRATOR
          );

          // 重试前重置 Todo 状态：第一次执行已将 Todo 标记为 completed，
          // 不重置会导致第二次 executeAssignment 空转（无可执行 Todo）
          await this.resetTodosForRetry(assignment, todoManager);

          const originalGuidance = assignment.guidancePrompt;
          assignment.guidancePrompt = `${originalGuidance}\n\n${this.buildForceChangeGuidance(normalizedTargets)}`;

          result = await workerInstance.executeAssignment(assignment, {
            workingDirectory: effectiveWorkspaceRoot,
            adapterFactory,
            adapterScope: {
              ...(config.planningMode ? { planningMode: config.planningMode } : {}),
              messageMetadata: adapterMessageMetadata,
            },
            projectContext,
            onReport,
            cancellationToken,
            imagePaths,
            getSupplementaryInstructions,
            sessionId: resumeSessionId,
            resumePrompt,
          });

          assignment.guidancePrompt = originalGuidance;

          const retryHasChanges = this.hasContentChanges(normalizedTargets, preExecutionContents, effectiveWorkspaceRoot)
            || (snapshotManager ? this.hasAssignmentChanges(snapshotManager, assignment.id, normalizedTargets) : false);

          if (!retryHasChanges) {
            if (!result.errors) { result.errors = []; }
            result.errors.push(t('pipeline.errors.noTargetFileChanges'));
            result.success = false;
            targetChangeDetected = false;
            logger.error(
              `WorkerPipeline: ${assignment.workerId} 重试后仍未产生目标文件变更`,
              { assignmentId: assignment.id, targetFiles: normalizedTargets },
              LogCategory.ORCHESTRATOR
            );
          }
        }
      }

      // ========== 8. [可选] LSP 后检 ==========
      if (enableLSP && this.lspEnforcer) {
        try {
          const postResult = await this.lspEnforcer.postCheck(assignment, preflightDiagnostics);
          if (postResult && postResult.newErrors.length > 0) {
            lspNewErrors.push(...postResult.newErrors);
            if (!result.errors) { result.errors = []; }
            result.errors.push(t('pipeline.errors.lspPostCheckNewErrors', {
              count: postResult.newErrors.length,
              errors: postResult.newErrors.join('；'),
            }));
          }
        } catch (error: any) {
          logger.warn('WorkerPipeline.LSP后检异常', {
            assignmentId: assignment.id, error: error?.message,
          }, LogCategory.ORCHESTRATOR);
        }
      }

      // ========== 9. [可选] Context 更新 ==========
      if (enableContextUpdate && contextManager) {
        await this.updateContextManager(assignment, result, contextManager);
      }
    } finally {
      // 清除快照上下文（无论成功或失败）
      toolManager.clearSnapshotContext(assignment.workerId);
    }

    logger.info(
      'WorkerPipeline.完成',
      {
        assignmentId: assignment.id,
        worker: assignment.workerId,
        success: result.success,
        hasPendingApprovals: result.hasPendingApprovals,
      },
      LogCategory.ORCHESTRATOR
    );

    // ========== 10. [可选] Worktree merge + release ==========
    let worktreeMerge: WorktreeMergeResult | undefined;
    if (worktreeAllocation && gitHost) {
      try {
        if (result.success) {
          worktreeMerge = gitHost.mergeWorktree({
            workspacePath: workspaceRoot,
            taskId: assignment.id,
          });
          if (worktreeMerge.hasConflicts) {
            logger.warn('WorkerPipeline.Worktree.合并冲突', {
              assignmentId: assignment.id,
              conflictFiles: worktreeMerge.conflictFiles,
            }, LogCategory.ORCHESTRATOR);
            // 合并冲突意味着 Worker 的变更无法安全地合入主工作区，
            // 必须将任务标记为失败，否则下游 batch/card 终态会误判为成功
            result.success = false;
            if (!result.errors) { result.errors = []; }
            result.errors.push(t('pipeline.errors.worktreeMergeConflict', {
              files: worktreeMerge.conflictFiles.join(', '),
            }));
            if (worktreeMerge.conflictSummary) {
              result.errors.push(worktreeMerge.conflictSummary);
            }
            if (Array.isArray(worktreeMerge.conflictHints) && worktreeMerge.conflictHints.length > 0) {
              result.errors.push(...worktreeMerge.conflictHints);
            }
            await this.createWorktreeConflictRepairTodo({
              assignment,
              todoManager,
              worktreeMerge,
            });
          }
        } else {
          logger.info('WorkerPipeline.Worktree.跳过合并_任务失败', {
            assignmentId: assignment.id,
          }, LogCategory.ORCHESTRATOR);
        }
      } finally {
        gitHost.releaseWorktree({
          workspacePath: workspaceRoot,
          taskId: assignment.id,
        });
      }
    }

    return { executionResult: result, lspNewErrors, targetChangeDetected, worktreeMerge };
  }

  // ===========================================================================
  // 私有方法（从 AssignmentExecutor 提取）
  // ===========================================================================

  private async createSnapshots(
    snapshotManager: SnapshotManager,
    missionId: string,
    assignment: Assignment,
    sessionId?: string,
  ): Promise<void> {
    if (!sessionId || !sessionId.trim()) {
      logger.warn('WorkerPipeline.快照创建.跳过_缺少会话', {
        assignmentId: assignment.id,
        missionId,
      }, LogCategory.ORCHESTRATOR);
      return;
    }
    const targetFiles = this.collectTargetFiles(assignment);
    if (targetFiles.length === 0) return;

    try {
      for (const filePath of targetFiles) {
        await snapshotManager.createSnapshotForMission(
          filePath, sessionId, missionId, assignment.id,
          'assignment-init', assignment.workerId,
          t('pipeline.snapshot.beforeAssignment', { responsibility: assignment.responsibility }),
        );
      }
      logger.info(
        'WorkerPipeline.快照创建',
        { assignmentId: assignment.id, fileCount: targetFiles.length },
        LogCategory.ORCHESTRATOR
      );
    } catch (error: any) {
      logger.warn('WorkerPipeline.快照创建失败', { error: error.message }, LogCategory.ORCHESTRATOR);
    }
  }

  private async generateAssembledContext(
    missionId: string,
    workerId: WorkerSlot,
    contextManager: import('../../../context/context-manager').ContextManager,
  ): Promise<AssembledContext | undefined> {
    const options = contextManager.buildAssemblyOptions(missionId, workerId, 8000);
    const assembled = await contextManager.getAssembledContext(options);
    return (assembled.parts && assembled.parts.length > 0) ? assembled : undefined;
  }

  private collectTargetFiles(assignment: Assignment): string[] {
    const files = new Set<string>();
    if (assignment.scope?.targetPaths) {
      assignment.scope.targetPaths
        .filter((p): p is string => typeof p === 'string' && p.trim().length > 0)
        .forEach(p => files.add(p.trim()));
    }
    if (assignment.todos) {
      assignment.todos.forEach(todo => {
        if (todo.output?.modifiedFiles) {
          todo.output.modifiedFiles
            .filter((f: unknown): f is string => typeof f === 'string' && (f as string).trim().length > 0)
            .forEach((f: string) => files.add(f.trim()));
        }
      });
    }
    return Array.from(files);
  }

  private normalizeTargetFiles(files: string[], workspaceRoot: string): string[] {
    const normalized = new Set<string>();
    for (const filePath of files) {
      const trimmed = filePath.trim();
      if (!trimmed) continue;
      const relative = path.isAbsolute(trimmed)
        ? path.relative(workspaceRoot, trimmed)
        : trimmed;
      normalized.add(path.normalize(relative));
    }
    return Array.from(normalized);
  }

  private captureTargetContents(targetFiles: string[], workspaceRoot: string): Map<string, string> {
    const contents = new Map<string, string>();
    for (const filePath of targetFiles) {
      const absolute = this.getAbsolutePath(filePath, workspaceRoot);
      let content = '';
      if (fs.existsSync(absolute)) {
        const stat = fs.statSync(absolute);
        // 目录路径不参与内容捕获，避免 EISDIR 错误
        if (!stat.isDirectory()) {
          content = fs.readFileSync(absolute, 'utf-8');
        }
      }
      contents.set(filePath, content);
    }
    return contents;
  }

  private hasContentChanges(targetFiles: string[], before: Map<string, string>, workspaceRoot: string): boolean {
    for (const filePath of targetFiles) {
      const absolute = this.getAbsolutePath(filePath, workspaceRoot);
      const previous = before.get(filePath);
      if (previous === undefined) continue;
      if (!fs.existsSync(absolute)) {
        if (previous !== '') return true;
        continue;
      }
      const stat = fs.statSync(absolute);
      // 目录路径不参与内容变更检测，避免 EISDIR 错误
      if (stat.isDirectory()) continue;
      const current = fs.readFileSync(absolute, 'utf-8');
      if (current !== previous) return true;
    }
    return false;
  }

  private hasAssignmentChanges(snapshotManager: SnapshotManager, assignmentId: string, targetFiles: string[]): boolean {
    if (targetFiles.length === 0) return false;
    const targetSet = new Set(targetFiles.map(file => path.normalize(file)));
    const pending = snapshotManager.getPendingChanges();
    return pending.some(change => {
      if (change.assignmentId !== assignmentId) return false;
      return targetSet.has(path.normalize(change.filePath));
    });
  }

  private getAbsolutePath(filePath: string, workspaceRoot: string): string {
    return path.isAbsolute(filePath) ? filePath : path.join(workspaceRoot, filePath);
  }

  /**
   * 重试前重置 Todo 状态
   *
   * 第一次 executeAssignment 会将 Todo 标记为 completed/failed，
   * 复用同一个 assignment 重试时必须恢复为 pending，否则第二次执行无可执行 Todo，
   * 空循环被判定为 success → 质量门禁误判。
   *
   * 同时同步 TodoManager 持久化状态，确保 prepareForExecution/start 等流程正常工作。
   */
  private async resetTodosForRetry(
    assignment: Assignment,
    todoManager?: import('../../../todo').TodoManager | null,
  ): Promise<void> {
    for (const todo of assignment.todos) {
      if (todo.status === 'completed' || todo.status === 'failed') {
        // 同步 TodoManager 持久化状态
        if (todoManager) {
          await todoManager.resetToPending(todo.id);
        }
        todo.status = 'pending';
        todo.completedAt = undefined;
        todo.output = undefined;
      }
    }
    assignment.planningStatus = 'planned';
    if (assignment.status !== 'pending') {
      assignment.status = 'ready';
    }
  }

  private buildForceChangeGuidance(targetFiles: string[]): string {
    const files = targetFiles.length > 0
      ? t('pipeline.forceChange.modifyAnyTarget', { files: targetFiles.join(', ') })
      : t('pipeline.forceChange.modifyTarget');
    return [
      t('pipeline.forceChange.title'),
      t('pipeline.forceChange.noActualModification'),
      files,
      t('pipeline.forceChange.noPlanOnly'),
    ].join('\n');
  }

  private async updateContextManager(
    assignment: Assignment,
    result: AutonomousExecutionResult,
    contextManager: import('../../../context/context-manager').ContextManager,
  ): Promise<void> {
    if (result.success) {
      if (result.hasPendingApprovals) {
        contextManager.updateTaskStatus(
          assignment.id, 'in_progress',
          t('pipeline.context.awaitingApproval', { count: result.completedTodos.length }),
        );
      } else {
        contextManager.updateTaskStatus(
          assignment.id, 'completed',
          t('pipeline.context.completedTodos', { count: result.completedTodos.length }),
        );
      }

      const modifiedFiles = new Set<string>();
      for (const todo of result.completedTodos) {
        if (todo.output?.modifiedFiles) {
          for (const file of todo.output.modifiedFiles) {
            modifiedFiles.add(file);
          }
        }
      }
      for (const file of modifiedFiles) {
        contextManager.addCodeChange(
          file, 'modify',
          t('pipeline.context.workerCompleted', {
            workerId: assignment.workerId,
            responsibility: assignment.responsibility,
          }),
        );
      }

      if (result.dynamicTodos.length > 0) {
        contextManager.addImportantContext(
          t('pipeline.context.dynamicTodosAdded', {
            workerId: assignment.workerId,
            count: result.dynamicTodos.length,
          }),
        );
      }

      if (result.completedTodos.length > 0) {
        const lastTodo = result.completedTodos[result.completedTodos.length - 1];
        if (lastTodo.output?.summary) {
          contextManager.addNextStep(
            t('pipeline.context.verifyWorkerOutput', {
              workerId: assignment.workerId,
              summary: `${lastTodo.output.summary.substring(0, 50)}...`,
            }),
          );
        }
      }

      contextManager.setCurrentWork(t('pipeline.context.workerFinished', {
        workerId: assignment.workerId,
        responsibility: assignment.responsibility,
      }));
    } else {
      contextManager.updateTaskStatus(assignment.id, 'failed', result.errors.join('; '));
      if (result.errors.length > 0) {
        contextManager.addPendingIssue(t('pipeline.context.workerFailedIssue', {
          workerId: assignment.workerId,
          error: result.errors[0],
        }));
      }
      contextManager.setCurrentWork(
        t('pipeline.context.workerFailedCurrentWork', {
          workerId: assignment.workerId,
          error: result.errors[0]?.substring(0, 50) || t('pipeline.context.unknownError'),
        }),
      );
    }

    await contextManager.saveMemory();
  }

  private async createWorktreeConflictRepairTodo(input: {
    assignment: Assignment;
    todoManager?: import('../../../todo').TodoManager | null;
    worktreeMerge: WorktreeMergeResult;
  }): Promise<void> {
    const { assignment, todoManager, worktreeMerge } = input;
    if (!todoManager || !worktreeMerge.hasConflicts) {
      return;
    }
    const marker = `[worktree-conflict:${assignment.id}]`;
    if (assignment.todos.some((todo) => typeof todo.content === 'string' && todo.content.includes(marker))) {
      return;
    }
    const conflictFiles = Array.isArray(worktreeMerge.conflictFiles)
      ? worktreeMerge.conflictFiles.filter(Boolean)
      : [];
    const filesLabel = conflictFiles.length > 0
      ? conflictFiles.join(', ')
      : '未识别文件';
    try {
      const repairTodo = await todoManager.create({
        missionId: assignment.missionId,
        assignmentId: assignment.id,
        trace: assignment.trace,
        source: 'system_repair',
        content: `${marker} 处理 worktree 合并冲突并完成冲突消解：${filesLabel}`,
        reasoning: `并发变更在 merge 阶段发生冲突，需先完成冲突消解再继续交付。${worktreeMerge.conflictSummary || ''}`.trim(),
        type: 'fix',
        workerId: assignment.workerId,
        targetFiles: conflictFiles.length > 0 ? conflictFiles : undefined,
      });
      assignment.todos.push(repairTodo);
      logger.info('WorkerPipeline.Worktree.冲突修复Todo已创建', {
        assignmentId: assignment.id,
        todoId: repairTodo.id,
        conflictFiles,
      }, LogCategory.ORCHESTRATOR);
    } catch (error: any) {
      logger.warn('WorkerPipeline.Worktree.冲突修复Todo创建失败', {
        assignmentId: assignment.id,
        conflictFiles,
        error: error?.message || String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }
}
