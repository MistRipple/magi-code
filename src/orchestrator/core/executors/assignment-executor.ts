/**
 * Assignment Executor - 任务分配执行器
 *
 * 职责：
 * - 执行单个 Assignment
 * - 管理 Todo 执行
 * - 处理快照创建
 */

import { WorkerSlot } from '../../../types';
import { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../../types/agent-types';
import { AutonomousWorker, AutonomousExecutionResult } from '../../worker';
import { Mission, Assignment, WorkerTodo } from '../../mission';
import { SnapshotManager } from '../../../snapshot-manager';
import fs from 'fs';
import path from 'path';
import { logger, LogCategory } from '../../../logging';
import { LspEnforcer } from '../../lsp/lsp-enforcer';
import type { ReportCallback } from '../../protocols/worker-report';

export interface AssignmentExecutionOptions {
  workingDirectory: string;
  projectContext?: string;
  timeout?: number;
  contextManager?: import('../../../context/context-manager').ContextManager | null;
  onOutput?: (workerId: WorkerSlot, output: string) => void;
  onReport?: ReportCallback;
  reportTimeout?: number;
  getSupplementaryInstructions?: () => string[];
}

export interface AssignmentExecutionResult {
  success: boolean;
  completedTodos: WorkerTodo[];
  dynamicTodos: WorkerTodo[];
  errors: string[];
  tokenUsage?: TokenUsage;
  /** 完整的 Worker 执行结果（用于统计） */
  fullResult?: AutonomousExecutionResult;
  /** 是否有等待审批的 Todo */
  hasPendingApprovals?: boolean;
}

export class AssignmentExecutor {
  private lspEnforcer: LspEnforcer | null = null;

  constructor(
    private workers: Map<WorkerSlot, AutonomousWorker>,
    private adapterFactory: IAdapterFactory,
    private snapshotManager: SnapshotManager | null,
    private workspaceRoot: string
  ) {
    this.lspEnforcer = new LspEnforcer(workspaceRoot);
  }

  /**
   * 执行单个 Assignment
   */
  async execute(
    mission: Mission,
    assignment: Assignment,
    options: AssignmentExecutionOptions
  ): Promise<AssignmentExecutionResult> {
    const worker = this.workers.get(assignment.workerId);
    if (!worker) {
      return {
        success: false,
        completedTodos: [],
        dynamicTodos: [],
        errors: [`Worker ${assignment.workerId} not found`],
      };
    }

    // 日志显示任务描述（优先使用 AI 生成的自然语言描述）
    const taskDescription = assignment.delegationBriefing || assignment.responsibility;
    logger.info(
      LogCategory.ORCHESTRATOR,
      `Worker ${assignment.workerId} 开始执行: ${taskDescription}`
    );

    // 创建快照
    await this.createSnapshots(mission, assignment);

    // 获取上下文快照
    const contextSnapshot = options.contextManager?.getContext(6000);

    // 收集目标文件
    const targetFiles = this.collectTargetFiles(assignment);
    const normalizedTargets = this.normalizeTargetFiles(targetFiles);
    const preExecutionContents = this.captureTargetContents(normalizedTargets);

    if (this.lspEnforcer) {
      try {
        await this.lspEnforcer.applyIfNeeded(assignment);
      } catch (error: any) {
        logger.warn('LSP 预检失败，继续执行', {
          assignmentId: assignment.id,
          error: error?.message
        }, LogCategory.ORCHESTRATOR);
      }
    }

    // 执行 Assignment
    let result = await worker.executeAssignment(assignment, {
      workingDirectory: options.workingDirectory,
      projectContext: options.projectContext,
      timeout: options.timeout,
      onReport: options.onReport,
      reportTimeout: options.reportTimeout,
      getSupplementaryInstructions: options.getSupplementaryInstructions,
      adapterFactory: this.adapterFactory,
      adapterScope: {
        messageMeta: {
          contextSnapshot,
          taskContext: {
            goal: assignment.responsibility,
            targetFiles,
          },
        },
      },
    });

    if (this.shouldEnforceTargetChanges(assignment, targetFiles)) {
      const hasChanges = this.hasAssignmentChanges(assignment.id, normalizedTargets)
        || this.hasContentChanges(normalizedTargets, preExecutionContents);
      if (!hasChanges) {
        logger.warn(
          `Worker ${assignment.workerId} 未产生目标文件变更，触发一次强制重试`,
          { assignmentId: assignment.id, targetFiles: normalizedTargets },
          LogCategory.ORCHESTRATOR
        );

        const originalGuidance = assignment.guidancePrompt;
        assignment.guidancePrompt = `${originalGuidance}\n\n${this.buildForceChangeGuidance(normalizedTargets)}`;

        result = await worker.executeAssignment(assignment, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
          timeout: options.timeout,
          onReport: options.onReport,
          reportTimeout: options.reportTimeout,
          getSupplementaryInstructions: options.getSupplementaryInstructions,
          adapterFactory: this.adapterFactory,
          adapterScope: {
            messageMeta: {
              contextSnapshot,
              taskContext: {
                goal: assignment.responsibility,
                targetFiles,
              },
            },
          },
        });

        assignment.guidancePrompt = originalGuidance;

        const retryHasChanges = this.hasAssignmentChanges(assignment.id, normalizedTargets)
          || this.hasContentChanges(normalizedTargets, preExecutionContents);
        if (!retryHasChanges) {
          if (!result.errors) {
            result.errors = [];
          }
          result.errors.push('未检测到对目标文件的修改');
          result.success = false;
          logger.error(
            `Worker ${assignment.workerId} 重试后仍未产生目标文件变更`,
            { assignmentId: assignment.id, targetFiles: normalizedTargets },
            LogCategory.ORCHESTRATOR
          );
        }
      }
    }

    // 更新 ContextManager
    await this.updateContextManager(assignment, result, options.contextManager);

    logger.info(
      LogCategory.ORCHESTRATOR,
      `Worker ${assignment.workerId} 执行完成: ${result.success ? '成功' : '失败'}${result.hasPendingApprovals ? ' (等待审批)' : ''}`
    );

    return {
      success: result.success,
      completedTodos: result.completedTodos,
      dynamicTodos: result.dynamicTodos,
      errors: result.errors,
      tokenUsage: result.tokenUsage,
      fullResult: result,
      hasPendingApprovals: result.hasPendingApprovals,
    };
  }

  /**
   * 创建快照
   */
  private async createSnapshots(
    mission: Mission,
    assignment: Assignment
  ): Promise<void> {
    if (!this.snapshotManager) {
      return;
    }

    const targetFiles = this.collectTargetFiles(assignment);
    if (targetFiles.length === 0) {
      return;
    }

    try {
      for (const filePath of targetFiles) {
        this.snapshotManager.createSnapshotForMission(
          filePath,
          mission.id,
          assignment.id,
          'assignment-init',
          assignment.workerId,
          `Assignment 执行前快照: ${assignment.responsibility}`
        );
      }

      logger.info(
        LogCategory.ORCHESTRATOR,
        `为 Assignment ${assignment.id} 创建快照，包含 ${targetFiles.length} 个文件`
      );
    } catch (error: any) {
      logger.warn(
        LogCategory.ORCHESTRATOR,
        `创建快照失败: ${error.message}`
      );
    }
  }

  /**
   * 收集目标文件
   */
  private collectTargetFiles(assignment: Assignment): string[] {
    const files = new Set<string>();

    // 从 scope 收集
    if (assignment.scope?.targetPaths) {
      assignment.scope.targetPaths
        .filter((p): p is string => typeof p === 'string' && p.trim().length > 0)
        .forEach(p => files.add(p.trim()));
    }

    // 从 todos 收集
    if (assignment.todos) {
      assignment.todos.forEach(todo => {
        // WorkerTodo doesn't have targetFiles, collect from output if available
        if (todo.output?.modifiedFiles) {
          todo.output.modifiedFiles
            .filter((f: unknown): f is string => typeof f === 'string' && f.trim().length > 0)
            .forEach((f: string) => files.add(f.trim()));
        }
      });
    }

    return Array.from(files);
  }

  private normalizeTargetFiles(files: string[]): string[] {
    const normalized = new Set<string>();
    for (const filePath of files) {
      const trimmed = filePath.trim();
      if (!trimmed) continue;
      const relative = path.isAbsolute(trimmed)
        ? path.relative(this.workspaceRoot, trimmed)
        : trimmed;
      normalized.add(path.normalize(relative));
    }
    return Array.from(normalized);
  }

  private captureTargetContents(targetFiles: string[]): Map<string, string> {
    const contents = new Map<string, string>();
    for (const filePath of targetFiles) {
      const absolute = this.getAbsolutePath(filePath);
      let content = '';
      if (fs.existsSync(absolute)) {
        content = fs.readFileSync(absolute, 'utf-8');
      }
      contents.set(filePath, content);
    }
    return contents;
  }

  private hasContentChanges(targetFiles: string[], before: Map<string, string>): boolean {
    for (const filePath of targetFiles) {
      const absolute = this.getAbsolutePath(filePath);
      const previous = before.get(filePath);
      if (previous === undefined) continue;
      if (!fs.existsSync(absolute)) {
        if (previous !== '') return true;
        continue;
      }
      const current = fs.readFileSync(absolute, 'utf-8');
      if (current !== previous) {
        return true;
      }
    }
    return false;
  }

  private shouldEnforceTargetChanges(assignment: Assignment, targetFiles: string[]): boolean {
    return Boolean(
      this.snapshotManager &&
      targetFiles.length > 0 &&
      assignment.scope?.requiresModification
    );
  }

  private hasAssignmentChanges(assignmentId: string, targetFiles: string[]): boolean {
    if (!this.snapshotManager) return false;
    if (targetFiles.length === 0) return false;
    const targetSet = new Set(targetFiles.map(file => path.normalize(file)));
    const pending = this.snapshotManager.getPendingChanges();
    return pending.some(change => {
      if (change.assignmentId !== assignmentId) return false;
      const normalized = path.normalize(change.filePath);
      return targetSet.has(normalized);
    });
  }

  private getAbsolutePath(filePath: string): string {
    return path.isAbsolute(filePath)
      ? filePath
      : path.join(this.workspaceRoot, filePath);
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

  /**
   * 更新 ContextManager
   */
  private async updateContextManager(
    assignment: Assignment,
    result: AutonomousExecutionResult,
    contextManager?: import('../../../context/context-manager').ContextManager | null
  ): Promise<void> {
    if (!contextManager) {
      return;
    }

    if (result.success) {
      if (result.hasPendingApprovals) {
        // 等待审批时，状态设为进行中 (in_progress) 或 paused
        contextManager.updateTaskStatus(
          assignment.id,
          'in_progress',
          `等待审批: 完成 ${result.completedTodos.length} 个 Todo`
        );
      } else {
        // 更新任务状态
        contextManager.updateTaskStatus(
          assignment.id,
          'completed',
          `完成 ${result.completedTodos.length} 个 Todo`
        );
      }

      // 添加代码变更记录
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
          file,
          'modify',
          `${assignment.workerId} 完成: ${assignment.responsibility}`
        );
      }

      // 记录动态 Todo
      if (result.dynamicTodos.length > 0) {
        contextManager.addImportantContext(
          `${assignment.workerId} 动态添加了 ${result.dynamicTodos.length} 个 Todo`
        );
      }

      // 【新增】任务成功完成后，添加下一步建议
      if (result.completedTodos.length > 0) {
        const lastTodo = result.completedTodos[result.completedTodos.length - 1];
        if (lastTodo.output?.summary) {
          contextManager.addNextStep(`验证 ${assignment.workerId} 的输出: ${lastTodo.output.summary.substring(0, 50)}...`);
        }
      }

      // 【新增】更新当前工作状态
      contextManager.setCurrentWork(`${assignment.workerId} 已完成: ${assignment.responsibility}`);
    } else {
      // 失败时更新状态和添加待解决问题
      contextManager.updateTaskStatus(
        assignment.id,
        'failed',
        result.errors.join('; ')
      );

      if (result.errors.length > 0) {
        contextManager.addPendingIssue(
          `${assignment.workerId} 执行失败: ${result.errors[0]}`
        );
      }

      // 【新增】更新当前工作状态为失败
      contextManager.setCurrentWork(`${assignment.workerId} 执行失败，需要排查: ${result.errors[0]?.substring(0, 50) || '未知错误'}`);
    }

    await contextManager.saveMemory();
  }

}
