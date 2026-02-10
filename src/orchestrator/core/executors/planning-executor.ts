/**
 * Planning Executor - 规划执行器
 *
 * 职责：
 * - 编排层为每个 Assignment 创建宏观 Todo
 * - 支持并行和顺序规划
 * - 生成和传递上下文快照
 *
 * 设计原则：
 * - Todo 创建权归编排层，Worker 只负责执行
 * - Worker 执行过程中如果任务过大，可通过 addDynamicTodo 自行拆分
 */

import { WorkerSlot } from '../../../types';
import { AutonomousWorker } from '../../worker';
import { Mission, Assignment } from '../../mission';
import { TodoManager } from '../../../todo';
import { logger, LogCategory } from '../../../logging';

export interface PlanningOptions {
  projectContext?: string;
  parallel?: boolean;
  contextManager?: import('../../../context/context-manager').ContextManager | null;
}

export interface PlanningResult {
  success: boolean;
  errors: string[];
}

export class PlanningExecutor {
  constructor(
    private workers: Map<WorkerSlot, AutonomousWorker>,
    private todoManager: TodoManager
  ) {}

  /**
   * 执行规划阶段
   */
  async execute(
    mission: Mission,
    options: PlanningOptions
  ): Promise<PlanningResult> {
    const parallel = options.parallel !== false; // 默认并行

    logger.info(LogCategory.ORCHESTRATOR, `开始规划阶段 (${parallel ? '并行' : '顺序'})`);

    try {
      if (parallel) {
        await this.planParallel(mission, options);
      } else {
        await this.planSequential(mission, options);
      }

      logger.info(LogCategory.ORCHESTRATOR, '规划阶段完成');
      return { success: true, errors: [] };
    } catch (error: any) {
      logger.error(LogCategory.ORCHESTRATOR, `规划阶段失败: ${error.message}`);
      return { success: false, errors: [error.message] };
    }
  }

  /**
   * 并行规划
   */
  private async planParallel(
    mission: Mission,
    options: PlanningOptions
  ): Promise<void> {
    const planningPromises = mission.assignments.map(async (assignment) => {
      await this.createTodoForAssignment(mission, assignment);
    });

    await Promise.all(planningPromises);
  }

  /**
   * 顺序规划
   */
  private async planSequential(
    mission: Mission,
    options: PlanningOptions
  ): Promise<void> {
    for (const assignment of mission.assignments) {
      await this.createTodoForAssignment(mission, assignment);
    }
  }

  /**
   * 为 Assignment 创建宏观 Todo（编排层职责）
   *
   * 编排者创建 1 个 implementation Todo 代表整个 Assignment 的职责。
   * Worker 执行过程中如果发现任务过大，可通过 addDynamicTodo 自行拆分。
   */
  private async createTodoForAssignment(
    mission: Mission,
    assignment: Assignment
  ): Promise<void> {
    logger.info(
      LogCategory.ORCHESTRATOR,
      `为 ${assignment.workerId} 创建宏观 Todo: ${assignment.responsibility}`
    );

    const targetPaths = assignment.scope?.targetPaths?.length
      ? assignment.scope.requiresModification
        ? `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。必须使用工具直接编辑并保存。`
        : `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。只需读取/分析，不要修改文件。`
      : '';

    const todo = await this.todoManager.create({
      missionId: mission.id,
      assignmentId: assignment.id,
      content: `${assignment.responsibility}${targetPaths}`,
      reasoning: assignment.delegationBriefing || assignment.responsibility,
      type: 'implementation',
      workerId: assignment.workerId,
      targetFiles: assignment.scope?.targetPaths,
    });

    assignment.todos = [todo];
    assignment.planningStatus = 'planned';
    if (assignment.status === 'pending') {
      assignment.status = 'ready';
    }

    logger.info(
      LogCategory.ORCHESTRATOR,
      `${assignment.workerId} 宏观 Todo 已创建: ${todo.id}`
    );
  }
}
