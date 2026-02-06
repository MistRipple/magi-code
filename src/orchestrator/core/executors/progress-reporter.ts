/**
 * Progress Reporter - 进度报告器
 *
 * 职责：
 * - 跟踪执行进度
 * - 生成进度报告
 * - 触发进度回调
 */

import { EventEmitter } from 'events';
import { WorkerSlot } from '../../../types';
import { TokenUsage } from '../../../types/agent-types';
import { Mission, Assignment } from '../../mission';
import type { UnifiedTodo } from '../../../todo/types';

/**
 * 执行进度
 */
export interface ExecutionProgress {
  /** 当前阶段 */
  phase: 'planning' | 'execution' | 'review' | 'verification' | 'completed';
  /** 总 Assignment 数 */
  totalAssignments: number;
  /** 已完成 Assignment 数 */
  completedAssignments: number;
  /** 总 Todo 数 */
  totalTodos: number;
  /** 已完成 Todo 数 */
  completedTodos: number;
  /** 当前执行的 Worker */
  currentWorker?: WorkerSlot;
  /** 当前执行的 Assignment */
  currentAssignment?: string;
  /** Token 使用统计 */
  tokenUsage?: TokenUsage;
  /** 进度百分比 (0-100) */
  percentage: number;
}

export class ProgressReporter extends EventEmitter {
  private currentPhase: ExecutionProgress['phase'] = 'planning';
  private completedAssignments = 0;
  private completedTodos = 0;
  private totalTokenUsage: TokenUsage = {
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
  };

  constructor(private mission: Mission) {
    super();
  }

  /**
   * 设置当前阶段
   */
  setPhase(phase: ExecutionProgress['phase']): void {
    this.currentPhase = phase;
    this.emitProgress();
  }

  /**
   * 报告 Assignment 开始
   */
  reportAssignmentStart(assignment: Assignment): void {
    this.emitProgress({
      currentWorker: assignment.workerId,
      currentAssignment: assignment.responsibility,
    });
  }

  /**
   * 报告 Assignment 完成
   */
  reportAssignmentComplete(
    assignment: Assignment,
    completedTodos: UnifiedTodo[],
    tokenUsage?: TokenUsage
  ): void {
    this.completedAssignments++;
    this.completedTodos += completedTodos.length;

    if (tokenUsage) {
      this.aggregateTokenUsage(tokenUsage);
    }

    this.emitProgress();
  }

  /**
   * 报告 Todo 完成
   */
  reportTodoComplete(todo: UnifiedTodo, tokenUsage?: TokenUsage): void {
    this.completedTodos++;

    if (tokenUsage) {
      this.aggregateTokenUsage(tokenUsage);
    }

    this.emitProgress();
  }

  /**
   * 获取当前进度
   */
  getProgress(): ExecutionProgress {
    const totalAssignments = this.mission.assignments.length;
    const totalTodos = this.mission.assignments.reduce(
      (sum, a) => sum + (a.todos?.length || 0),
      0
    );

    let percentage = 0;
    if (this.currentPhase === 'completed') {
      percentage = 100;
    } else if (totalTodos > 0) {
      percentage = Math.round((this.completedTodos / totalTodos) * 100);
    } else if (totalAssignments > 0) {
      percentage = Math.round((this.completedAssignments / totalAssignments) * 100);
    }

    return {
      phase: this.currentPhase,
      totalAssignments,
      completedAssignments: this.completedAssignments,
      totalTodos,
      completedTodos: this.completedTodos,
      tokenUsage: this.totalTokenUsage,
      percentage,
    };
  }

  /**
   * 触发进度事件
   */
  private emitProgress(extra?: Partial<ExecutionProgress>): void {
    const progress = { ...this.getProgress(), ...extra };
    this.emit('progress', progress);
  }

  /**
   * 聚合 Token 使用
   */
  private aggregateTokenUsage(tokenUsage: TokenUsage): void {
    this.totalTokenUsage.inputTokens += tokenUsage.inputTokens || 0;
    this.totalTokenUsage.outputTokens += tokenUsage.outputTokens || 0;

    if (tokenUsage.cacheReadTokens) {
      this.totalTokenUsage.cacheReadTokens =
        (this.totalTokenUsage.cacheReadTokens || 0) + tokenUsage.cacheReadTokens;
    }

    if (tokenUsage.cacheWriteTokens) {
      this.totalTokenUsage.cacheWriteTokens =
        (this.totalTokenUsage.cacheWriteTokens || 0) + tokenUsage.cacheWriteTokens;
    }
  }

  /**
   * 重置进度
   */
  reset(): void {
    this.currentPhase = 'planning';
    this.completedAssignments = 0;
    this.completedTodos = 0;
    this.totalTokenUsage = {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
    };
  }
}
