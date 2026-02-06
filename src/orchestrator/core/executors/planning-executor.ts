/**
 * Planning Executor - 规划执行器
 *
 * 职责：
 * - 协调 Worker 规划 Todo
 * - 支持并行和顺序规划
 * - 生成和传递上下文快照
 */

import { WorkerSlot } from '../../../types';
import { AutonomousWorker } from '../../worker';
import { Mission, Assignment } from '../../mission';
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
    private workers: Map<WorkerSlot, AutonomousWorker>
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
      const worker = this.workers.get(assignment.workerId);
      if (!worker) {
        throw new Error(`Worker ${assignment.workerId} not found`);
      }

      const contextSnapshot = await this.generateContextSnapshot(
        mission.id,
        assignment.workerId,
        options.contextManager
      );

      logger.info(
        LogCategory.ORCHESTRATOR,
        `Worker ${assignment.workerId} 开始规划: ${assignment.responsibility}`
      );

      const planResult = await worker.planAssignment(assignment, {
        projectContext: options.projectContext,
        contextSnapshot,
      });

      assignment.todos = planResult.todos || [];
      assignment.planningStatus = 'planned';
      if (assignment.status === 'pending') {
        assignment.status = 'ready';
      }

      // PlanningResult returns todos directly, check warnings
      if (planResult.warnings.length > 0) {
        logger.warn(
          LogCategory.ORCHESTRATOR,
          `Worker ${assignment.workerId} 规划警告: ${planResult.warnings.join(', ')}`
        );
      }

      logger.info(
        LogCategory.ORCHESTRATOR,
        `Worker ${assignment.workerId} 规划完成，生成 ${planResult.todos.length} 个 Todo`
      );
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
      const worker = this.workers.get(assignment.workerId);
      if (!worker) {
        throw new Error(`Worker ${assignment.workerId} not found`);
      }

      // 每次规划前生成最新的上下文快照
      const contextSnapshot = await this.generateContextSnapshot(
        mission.id,
        assignment.workerId,
        options.contextManager
      );

      logger.info(
        LogCategory.ORCHESTRATOR,
        `Worker ${assignment.workerId} 开始规划: ${assignment.responsibility}`
      );

      const planResult = await worker.planAssignment(assignment, {
        projectContext: options.projectContext,
        contextSnapshot,
      });

      assignment.todos = planResult.todos || [];
      assignment.planningStatus = 'planned';
      if (assignment.status === 'pending') {
        assignment.status = 'ready';
      }

      // PlanningResult returns todos directly, check warnings
      if (planResult.warnings.length > 0) {
        logger.warn(
          LogCategory.ORCHESTRATOR,
          `Worker ${assignment.workerId} 规划警告: ${planResult.warnings.join(', ')}`
        );
      }

      logger.info(
        LogCategory.ORCHESTRATOR,
        `Worker ${assignment.workerId} 规划完成，生成 ${planResult.todos.length} 个 Todo`
      );
    }
  }

  /**
   * 生成上下文快照
   */
  private async generateContextSnapshot(
    missionId: string,
    workerId: WorkerSlot,
    contextManager?: import('../../../context/context-manager').ContextManager | null
  ): Promise<string | undefined> {
    if (!contextManager) {
      return undefined;
    }

    return contextManager.getAssembledContextText(
      contextManager.buildAssemblyOptions(missionId, workerId, 4000)
    );
  }
}
