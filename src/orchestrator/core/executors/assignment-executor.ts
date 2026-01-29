/**
 * Assignment Executor - 任务分配执行器
 *
 * 职责：
 * - 执行单个 Assignment
 * - 管理 Todo 执行
 * - 处理快照创建
 * - 同步 SubTask 状态
 */

import { WorkerSlot } from '../../../types';
import { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../../types/agent-types';
import { AutonomousWorker, AutonomousExecutionResult } from '../../worker';
import { Mission, Assignment, WorkerTodo } from '../../mission';
import { SnapshotManager } from '../../../snapshot-manager';
import { UnifiedTaskManager } from '../../../task/unified-task-manager';
import { logger, LogCategory } from '../../../logging';

export interface AssignmentExecutionOptions {
  workingDirectory: string;
  projectContext?: string;
  timeout?: number;
  taskId?: string;
  contextManager?: import('../../../context/context-manager').ContextManager | null;
  onOutput?: (workerId: WorkerSlot, output: string) => void;
}

export interface AssignmentExecutionResult {
  success: boolean;
  completedTodos: WorkerTodo[];
  dynamicTodos: WorkerTodo[];
  errors: string[];
  tokenUsage?: TokenUsage;
}

export class AssignmentExecutor {
  constructor(
    private workers: Map<WorkerSlot, AutonomousWorker>,
    private adapterFactory: IAdapterFactory,
    private snapshotManager: SnapshotManager | null,
    private taskManager: UnifiedTaskManager | null
  ) {}

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

    logger.info(
      LogCategory.ORCHESTRATOR,
      `Worker ${assignment.workerId} 开始执行: ${assignment.responsibility}`
    );

    // 创建快照
    await this.createSnapshots(mission, assignment);

    // 获取上下文快照
    const contextSnapshot = options.contextManager?.getContext(6000);

    // 收集目标文件
    const targetFiles = this.collectTargetFiles(assignment);

    // 执行 Assignment
    const result = await worker.executeAssignment(assignment, {
      workingDirectory: options.workingDirectory,
      projectContext: options.projectContext,
      timeout: options.timeout,
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

    // 更新 ContextManager
    await this.updateContextManager(assignment, result, options.contextManager);

    // 同步 SubTask 状态
    if (options.taskId) {
      await this.syncSubTaskStatus(assignment, result, options.taskId);
    }

    logger.info(
      LogCategory.ORCHESTRATOR,
      `Worker ${assignment.workerId} 执行完成: ${result.success ? '成功' : '失败'}`
    );

    return {
      success: result.success,
      completedTodos: result.completedTodos,
      dynamicTodos: result.dynamicTodos,
      errors: result.errors,
      tokenUsage: result.tokenUsage,
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
      assignment.scope.targetPaths.forEach(p => files.add(p));
    }

    // 从 todos 收集
    if (assignment.todos) {
      assignment.todos.forEach(todo => {
        // WorkerTodo doesn't have targetFiles, collect from output if available
        if (todo.output?.modifiedFiles) {
          todo.output.modifiedFiles.forEach((f: string) => files.add(f));
        }
      });
    }

    return Array.from(files);
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
      // 更新任务状态
      contextManager.updateTaskStatus(
        assignment.id,
        'completed',
        `完成 ${result.completedTodos.length} 个 Todo`
      );

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
    }

    await contextManager.saveMemory();
  }

  /**
   * 同步 SubTask 状态
   */
  private async syncSubTaskStatus(
    assignment: Assignment,
    result: AutonomousExecutionResult,
    taskId: string
  ): Promise<void> {
    if (!this.taskManager) {
      return;
    }

    try {
      const subTask = await this.taskManager.getSubTaskByAssignmentId(taskId, assignment.id);
      if (!subTask) {
        logger.warn(
          LogCategory.ORCHESTRATOR,
          `未找到 Assignment ${assignment.id} 对应的 SubTask`
        );
        return;
      }

      if (result.success) {
        await this.taskManager.updateSubTask(taskId, {
          id: subTask.id,
          status: 'completed'
        });
      } else {
        await this.taskManager.updateSubTask(taskId, {
          id: subTask.id,
          status: 'failed'
        });
      }
    } catch (error: any) {
      logger.warn(
        LogCategory.ORCHESTRATOR,
        `同步 SubTask 状态失败: ${error.message}`
      );
    }
  }
}
