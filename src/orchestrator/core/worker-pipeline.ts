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
import type { WorkerSlot } from '../../types';
import type { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import type { AutonomousWorker, AutonomousExecutionResult } from '../worker';
import type { Assignment } from '../mission';
import type { SnapshotManager } from '../../snapshot-manager';
import type { ReportCallback } from '../protocols/worker-report';
import type { CancellationToken } from './dispatch-batch';
import { LspEnforcer } from '../lsp/lsp-enforcer';
import { logger, LogCategory } from '../../logging';
import type { AssembledContext } from '../../context/context-assembler';

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
  cancellationToken?: CancellationToken;
  imagePaths?: string[];
  missionId?: string;

  // 治理开关（由 DispatchManager 根据 governance 参数计算）
  enableSnapshot: boolean;
  enableLSP: boolean;
  enableTargetEnforce: boolean;
  enableContextUpdate: boolean;

  // 外部依赖（可选注入）
  snapshotManager?: SnapshotManager | null;
  contextManager?: import('../../context/context-manager').ContextManager | null;
  todoManager?: import('../../todo').TodoManager | null;

  // 反应式编排：补充指令回调（由 DispatchManager 从 SupplementaryInstructionQueue 注入）
  getSupplementaryInstructions?: () => string[];
}

export interface PipelineResult {
  executionResult: AutonomousExecutionResult;
  lspNewErrors: string[];
  targetChangeDetected: boolean;
}

// ============================================================================
// WorkerPipeline
// ============================================================================

export class WorkerPipeline {
  private lspEnforcer: LspEnforcer | null = null;

  async execute(config: PipelineConfig): Promise<PipelineResult> {
    const {
      assignment, workerInstance, adapterFactory, workspaceRoot,
      projectContext, onReport, cancellationToken, imagePaths,
      enableSnapshot, enableLSP, enableTargetEnforce, enableContextUpdate,
      snapshotManager, contextManager, todoManager,
      getSupplementaryInstructions,
    } = config;
    const missionId = config.missionId || 'dispatch';

    logger.info(
      'WorkerPipeline.开始',
      {
        assignmentId: assignment.id,
        worker: assignment.workerId,
        governance: { enableSnapshot, enableLSP, enableTargetEnforce, enableContextUpdate },
      },
      LogCategory.ORCHESTRATOR
    );

    // ========== 1. [可选] 快照创建 ==========
    if (enableSnapshot && snapshotManager) {
      this.createSnapshots(snapshotManager, missionId, assignment);
    }

    // ========== 2. 设置工具级快照上下文 ==========
    const toolManager = adapterFactory.getToolManager();
    toolManager.setSnapshotContext({
      missionId,
      assignmentId: assignment.id,
      todoId: assignment.id,
      workerId: assignment.workerId,
    });

    // ========== 3/4/5. 上下文快照 + 目标文件收集 + LSP 预检（并行执行） ==========
    // 这三个步骤无互依赖，并行执行减少总耗时
    const targetFiles = this.collectTargetFiles(assignment);
    const normalizedTargets = this.normalizeTargetFiles(targetFiles, workspaceRoot);
    let preExecutionContents: Map<string, string> | null = null;
    if (enableTargetEnforce && normalizedTargets.length > 0) {
      preExecutionContents = this.captureTargetContents(normalizedTargets, workspaceRoot);
    }

    const assembledContextPromise = enableContextUpdate && contextManager
      ? this.generateAssembledContext(missionId, assignment.workerId, contextManager)
      : Promise.resolve(undefined);

    let preflightDiagnostics: string[] = [];
    const lspPromise = enableLSP
      ? (async () => {
          if (!this.lspEnforcer) {
            this.lspEnforcer = new LspEnforcer(workspaceRoot);
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

    // 将结构化 AssembledContext 格式化为文本（供 adapterScope）
    const contextSnapshotText = assembledContext && contextManager
      ? contextManager.formatAssembledContext(assembledContext)
      : undefined;

    // ========== 6. Worker 执行 ==========
    let result: AutonomousExecutionResult;
    const lspNewErrors: string[] = [];
    let targetChangeDetected = true;

    try {
      result = await workerInstance.executeAssignment(assignment, {
        workingDirectory: workspaceRoot,
        adapterFactory,
        projectContext,
        onReport,
        cancellationToken,
        imagePaths,
        getSupplementaryInstructions,
        preAssembledContext: assembledContext,
        adapterScope: contextSnapshotText ? {
          messageMeta: {
            contextSnapshot: contextSnapshotText,
            taskContext: { goal: assignment.responsibility, targetFiles },
          },
        } : undefined,
      });

      // ========== 7. [可选] 目标变更检测 + 强制重试 ==========
      if (enableTargetEnforce && preExecutionContents && normalizedTargets.length > 0
          && assignment.scope?.requiresModification) {
        const hasChanges = this.hasContentChanges(normalizedTargets, preExecutionContents, workspaceRoot)
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
            workingDirectory: workspaceRoot,
            adapterFactory,
            projectContext,
            onReport,
            cancellationToken,
            imagePaths,
            getSupplementaryInstructions,
            adapterScope: contextSnapshotText ? {
              messageMeta: {
                contextSnapshot: contextSnapshotText,
                taskContext: { goal: assignment.responsibility, targetFiles },
              },
            } : undefined,
          });

          assignment.guidancePrompt = originalGuidance;

          const retryHasChanges = this.hasContentChanges(normalizedTargets, preExecutionContents, workspaceRoot)
            || (snapshotManager ? this.hasAssignmentChanges(snapshotManager, assignment.id, normalizedTargets) : false);

          if (!retryHasChanges) {
            if (!result.errors) { result.errors = []; }
            result.errors.push('未检测到对目标文件的修改');
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
            result.errors.push(`LSP 后检发现 ${postResult.newErrors.length} 个新增编译错误：${postResult.newErrors.join('；')}`);
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

    return { executionResult: result, lspNewErrors, targetChangeDetected };
  }

  // ===========================================================================
  // 私有方法（从 AssignmentExecutor 提取）
  // ===========================================================================

  private createSnapshots(
    snapshotManager: SnapshotManager,
    missionId: string,
    assignment: Assignment,
  ): void {
    const targetFiles = this.collectTargetFiles(assignment);
    if (targetFiles.length === 0) return;

    try {
      for (const filePath of targetFiles) {
        snapshotManager.createSnapshotForMission(
          filePath, missionId, assignment.id,
          'assignment-init', assignment.workerId,
          `Assignment 执行前快照: ${assignment.responsibility}`,
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
    contextManager: import('../../context/context-manager').ContextManager,
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
    todoManager?: import('../../todo').TodoManager | null,
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
      ? `必须修改以下文件之一并保存：${targetFiles.join(', ')}。`
      : '必须对目标文件进行实际修改并保存。';
    return [
      '【强制要求】',
      '你之前没有对目标文件产生实际修改。',
      files,
      '禁止仅给出说明或计划，必须通过工具编辑文件并保存后再输出结果。',
    ].join('\n');
  }

  private async updateContextManager(
    assignment: Assignment,
    result: AutonomousExecutionResult,
    contextManager: import('../../context/context-manager').ContextManager,
  ): Promise<void> {
    if (result.success) {
      if (result.hasPendingApprovals) {
        contextManager.updateTaskStatus(
          assignment.id, 'in_progress',
          `等待审批: 完成 ${result.completedTodos.length} 个 Todo`,
        );
      } else {
        contextManager.updateTaskStatus(
          assignment.id, 'completed',
          `完成 ${result.completedTodos.length} 个 Todo`,
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
          `${assignment.workerId} 完成: ${assignment.responsibility}`,
        );
      }

      if (result.dynamicTodos.length > 0) {
        contextManager.addImportantContext(
          `${assignment.workerId} 动态添加了 ${result.dynamicTodos.length} 个 Todo`,
        );
      }

      if (result.completedTodos.length > 0) {
        const lastTodo = result.completedTodos[result.completedTodos.length - 1];
        if (lastTodo.output?.summary) {
          contextManager.addNextStep(
            `验证 ${assignment.workerId} 的输出: ${lastTodo.output.summary.substring(0, 50)}...`,
          );
        }
      }

      contextManager.setCurrentWork(`${assignment.workerId} 已完成: ${assignment.responsibility}`);
    } else {
      contextManager.updateTaskStatus(assignment.id, 'failed', result.errors.join('; '));
      if (result.errors.length > 0) {
        contextManager.addPendingIssue(`${assignment.workerId} 执行失败: ${result.errors[0]}`);
      }
      contextManager.setCurrentWork(
        `${assignment.workerId} 执行失败，需要排查: ${result.errors[0]?.substring(0, 50) || '未知错误'}`,
      );
    }

    await contextManager.saveMemory();
  }
}
