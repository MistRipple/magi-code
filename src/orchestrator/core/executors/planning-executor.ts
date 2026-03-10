/**
 * Planning Executor - 规划执行器
 *
 * 职责：
 * - 一级 Todo 的唯一创建入口
 * - 1 个 Assignment 对应 1 个一级 Todo
 *
 * 设计原则：
 * - 一级 Todo 由编排层创建，无 parentId
 * - Worker 执行过程中通过 addDynamicTodo 创建二级 Todo（parentId 指向一级）
 */

import { Assignment } from '../../mission';
import { TodoManager, TodoType } from '../../../todo';
import { logger, LogCategory } from '../../../logging';

export class PlanningExecutor {
  constructor(
    private todoManager: TodoManager
  ) {}

  /**
   * 创建一级 Todo（1 个 Assignment = 1 个一级 Todo）
   * 编排层唯一的 Todo 创建入口
   */
  async createMacroTodo(
    missionId: string,
    sessionId: string,
    assignment: Assignment,
  ): Promise<void> {
    logger.info(
      `为 ${assignment.workerId} 创建一级 Todo: ${assignment.responsibility}`,
      undefined,
      LogCategory.ORCHESTRATOR
    );

    const content = this.buildTodoContent(assignment);
    const macroTodoType = this.resolveMacroTodoType(assignment);
    const todo = await this.todoManager.create({
      sessionId,
      missionId,
      assignmentId: assignment.id,
      content,
      reasoning: assignment.delegationBriefing || assignment.responsibility,
      type: macroTodoType,
      workerId: assignment.workerId,
      targetFiles: assignment.scope?.targetPaths,
    });

    assignment.todos = [todo];
    assignment.planningStatus = 'planned';
    if (assignment.status === 'pending') {
      assignment.status = 'ready';
    }

    logger.info(
      `${assignment.workerId} 一级 Todo 已创建: ${todo.id}`,
      undefined,
      LogCategory.ORCHESTRATOR
    );
  }

  private buildTodoContent(assignment: Assignment): string {
    const scopeHints = assignment.scope?.scopeHints?.length
      ? `\n范围线索（非硬约束）: ${assignment.scope.scopeHints.join(', ')}。可按需扩展到相关文件/模块。`
      : '';
    const targetPaths = assignment.scope?.targetPaths?.length
      ? assignment.scope.requiresModification
        ? `\n严格目标文件: ${assignment.scope.targetPaths.join(', ')}。需要对这些目标文件产出实际改动。`
        : `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。只需读取/分析，不要修改文件。`
      : '';
    return `${assignment.responsibility}${scopeHints}${targetPaths}`;
  }

  private resolveMacroTodoType(assignment: Assignment): TodoType {
    return assignment.scope?.requiresModification === true
      ? 'implementation'
      : 'discovery';
  }
}
