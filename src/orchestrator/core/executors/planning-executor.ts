/**
 * Planning Executor - 规划执行器
 *
 * 职责：
 * - 一级 Todo 的唯一创建入口
 * - 1 个 Assignment 对应 1 个一级 Todo
 * - 支持 macro（直接创建）和 plan（LLM 拆分，降级为 macro）两种模式
 *
 * 设计原则：
 * - 一级 Todo 由编排层创建，无 parentId
 * - Worker 执行过程中通过 addDynamicTodo 创建二级 Todo（parentId 指向一级）
 */

import { Mission, Assignment } from '../../mission';
import { TodoManager } from '../../../todo';
import { logger, LogCategory } from '../../../logging';

export interface PlanningOptions {
  projectContext?: string;
  parallel?: boolean;
  contextManager?: import('../../../context/context-manager').ContextManager | null;
  mode: 'macro' | 'plan';
}

export interface PlanningResult {
  success: boolean;
  errors: string[];
}

export class PlanningExecutor {
  constructor(
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

    logger.info(LogCategory.ORCHESTRATOR, `开始规划阶段 (mode=${options.mode}, ${parallel ? '并行' : '顺序'})`);

    try {
      const createFn = options.mode === 'plan'
        ? (a: Assignment) => this.planWithLLM(mission.id, a)
        : (a: Assignment) => this.createMacroTodo(mission.id, a);

      if (parallel) {
        await Promise.all(mission.assignments.map(createFn));
      } else {
        for (const assignment of mission.assignments) {
          await createFn(assignment);
        }
      }

      logger.info(LogCategory.ORCHESTRATOR, '规划阶段完成');
      return { success: true, errors: [] };
    } catch (error: any) {
      logger.error(LogCategory.ORCHESTRATOR, `规划阶段失败: ${error.message}`);
      return { success: false, errors: [error.message] };
    }
  }

  /**
   * 创建一级 Todo（1 个 Assignment = 1 个一级 Todo）
   * 编排层唯一的 Todo 创建入口
   */
  async createMacroTodo(missionId: string, assignment: Assignment): Promise<void> {
    logger.info(
      LogCategory.ORCHESTRATOR,
      `为 ${assignment.workerId} 创建一级 Todo: ${assignment.responsibility}`
    );

    const content = this.buildTodoContent(assignment);
    const todo = await this.todoManager.create({
      missionId,
      assignmentId: assignment.id,
      content,
      reasoning: assignment.delegationBriefing || assignment.responsibility,
      type: 'implementation',
      workerId: assignment.workerId,
      targetFiles: assignment.scope?.targetPaths,
    });

    this.applyTodoToAssignment(assignment, [todo]);

    logger.info(
      LogCategory.ORCHESTRATOR,
      `${assignment.workerId} 一级 Todo 已创建: ${todo.id}`
    );
  }

  /**
   * 规划模式：LLM 拆分为多步骤一级 Todo
   * 当前降级为 createMacroTodo
   */
  private async planWithLLM(missionId: string, assignment: Assignment): Promise<void> {
    // 降级为 macro 模式
    await this.createMacroTodo(missionId, assignment);
  }

  private buildTodoContent(assignment: Assignment): string {
    const targetPaths = assignment.scope?.targetPaths?.length
      ? assignment.scope.requiresModification
        ? `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。必须使用工具直接编辑并保存。`
        : `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。只需读取/分析，不要修改文件。`
      : '';
    return `${assignment.responsibility}${targetPaths}`;
  }

  private applyTodoToAssignment(assignment: Assignment, todos: import('../../../todo/types').UnifiedTodo[]): void {
    assignment.todos = todos;
    assignment.planningStatus = 'planned';
    if (assignment.status === 'pending') {
      assignment.status = 'ready';
    }
  }
}
