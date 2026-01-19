/**
 * Profile Aware Recovery Handler - 画像感知恢复处理器
 *
 * 核心功能：
 * - 基于 Worker 画像智能选择恢复策略
 * - 弱项相关失败 → 换 Worker 重试
 * - 非弱项失败 → 原有恢复逻辑
 */

import { CLIType } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { WorkerProfile } from '../profile/types';
import {
  Assignment,
  WorkerTodo,
  TodoOutput,
} from '../mission/types';

/**
 * 恢复策略类型
 */
export type RecoveryStrategyType =
  | 'retry_same_worker'      // 同 Worker 重试
  | 'switch_worker'          // 换 Worker 重试
  | 'simplify_task'          // 简化任务
  | 'request_human_help'     // 请求人工介入
  | 'skip_task';             // 跳过任务

/**
 * 恢复决策
 */
export interface RecoveryDecision {
  strategy: RecoveryStrategyType;
  reason: string;
  alternativeWorker?: CLIType;
  simplifiedTask?: string;
  confidence: number;
}

/**
 * 失败分析结果
 */
export interface FailureAnalysis {
  /** 是否与 Worker 弱项相关 */
  relatedToWeakness: boolean;
  /** 匹配的弱项 */
  matchedWeaknesses: string[];
  /** 错误类型 */
  errorType: 'timeout' | 'logic_error' | 'capability_mismatch' | 'external' | 'unknown';
  /** 是否可恢复 */
  recoverable: boolean;
}

/**
 * ProfileAwareRecoveryHandler - 画像感知恢复处理器
 */
export class ProfileAwareRecoveryHandler {
  constructor(private profileLoader: ProfileLoader) {}

  /**
   * 分析失败原因
   */
  analyzeFailure(
    todo: WorkerTodo,
    assignment: Assignment,
    output: TodoOutput
  ): FailureAnalysis {
    const profile = this.profileLoader.getProfile(assignment.workerId);
    const errorMessage = output.error?.toLowerCase() || '';
    const taskContent = todo.content.toLowerCase();

    // 检查是否与弱项相关
    const matchedWeaknesses = this.findWeaknessMatches(
      `${errorMessage} ${taskContent}`,
      profile
    );

    // 判断错误类型
    const errorType = this.classifyError(errorMessage, output);

    // 判断是否可恢复
    const recoverable = this.isRecoverable(errorType, matchedWeaknesses.length > 0);

    return {
      relatedToWeakness: matchedWeaknesses.length > 0,
      matchedWeaknesses,
      errorType,
      recoverable,
    };
  }

  /**
   * 决定恢复策略
   */
  decideRecoveryStrategy(
    todo: WorkerTodo,
    assignment: Assignment,
    failureAnalysis: FailureAnalysis,
    retryCount: number
  ): RecoveryDecision {
    // 1. 如果与弱项相关，优先换 Worker
    if (failureAnalysis.relatedToWeakness) {
      const alternativeWorker = this.findAlternativeWorker(
        assignment,
        failureAnalysis.matchedWeaknesses
      );

      if (alternativeWorker) {
        return {
          strategy: 'switch_worker',
          reason: `任务涉及 ${assignment.workerId} 的弱项 (${failureAnalysis.matchedWeaknesses.join(', ')})，建议换 ${alternativeWorker} 重试`,
          alternativeWorker,
          confidence: 0.8,
        };
      }
    }

    // 2. 根据错误类型和重试次数决定策略
    if (retryCount === 0) {
      // 第一次失败，同 Worker 重试
      return {
        strategy: 'retry_same_worker',
        reason: '首次失败，尝试同 Worker 重试',
        confidence: 0.6,
      };
    }

    if (retryCount === 1) {
      // 第二次失败，尝试简化任务
      if (this.canSimplifyTask(todo)) {
        return {
          strategy: 'simplify_task',
          reason: '二次失败，尝试简化任务后重试',
          simplifiedTask: this.simplifyTask(todo),
          confidence: 0.5,
        };
      }

      // 无法简化，换 Worker
      const alternativeWorker = this.findAlternativeWorker(assignment, []);
      if (alternativeWorker) {
        return {
          strategy: 'switch_worker',
          reason: '二次失败，尝试换 Worker',
          alternativeWorker,
          confidence: 0.5,
        };
      }
    }

    // 3. 多次失败或不可恢复，请求人工介入或跳过
    if (!failureAnalysis.recoverable) {
      return {
        strategy: 'skip_task',
        reason: '错误不可恢复，跳过任务',
        confidence: 0.9,
      };
    }

    return {
      strategy: 'request_human_help',
      reason: `多次重试失败 (${retryCount + 1} 次)，需要人工介入`,
      confidence: 0.9,
    };
  }

  /**
   * 查找弱项匹配
   */
  private findWeaknessMatches(text: string, profile: WorkerProfile): string[] {
    const textLower = text.toLowerCase();
    return profile.profile.weaknesses.filter(w =>
      textLower.includes(w.toLowerCase())
    );
  }

  /**
   * 分类错误类型
   */
  private classifyError(
    errorMessage: string,
    output: TodoOutput
  ): FailureAnalysis['errorType'] {
    if (errorMessage.includes('timeout') || errorMessage.includes('超时')) {
      return 'timeout';
    }

    if (errorMessage.includes('cannot') || errorMessage.includes('unable') ||
        errorMessage.includes('不能') || errorMessage.includes('无法')) {
      return 'capability_mismatch';
    }

    if (errorMessage.includes('network') || errorMessage.includes('connection') ||
        errorMessage.includes('api') || errorMessage.includes('external')) {
      return 'external';
    }

    if (errorMessage.includes('error') || errorMessage.includes('failed') ||
        errorMessage.includes('错误') || errorMessage.includes('失败')) {
      return 'logic_error';
    }

    return 'unknown';
  }

  /**
   * 判断是否可恢复
   */
  private isRecoverable(
    errorType: FailureAnalysis['errorType'],
    isWeaknessRelated: boolean
  ): boolean {
    // 外部错误可能不可恢复
    if (errorType === 'external') {
      return false;
    }

    // 弱项相关的错误通过换 Worker 可恢复
    if (isWeaknessRelated) {
      return true;
    }

    // 超时和逻辑错误通常可恢复
    if (errorType === 'timeout' || errorType === 'logic_error') {
      return true;
    }

    return true;
  }

  /**
   * 查找替代 Worker
   */
  private findAlternativeWorker(
    assignment: Assignment,
    weaknesses: string[]
  ): CLIType | undefined {
    const allProfiles = this.profileLoader.getAllProfiles();
    const currentWorker = assignment.workerId;

    // 查找擅长当前任务且不在弱项的 Worker
    for (const [cli, profile] of allProfiles.entries()) {
      if (cli === currentWorker) continue;

      // 检查这个 Worker 是否擅长处理当前弱项相关的任务
      const hasStrengthForWeakness = weaknesses.some(weakness =>
        profile.profile.strengths.some(s =>
          s.toLowerCase().includes(weakness.toLowerCase())
        )
      );

      if (hasStrengthForWeakness) {
        return cli as CLIType;
      }
    }

    // 没有特别匹配的，返回第一个不同的 Worker
    for (const [cli] of allProfiles.entries()) {
      if (cli !== currentWorker) {
        return cli as CLIType;
      }
    }

    return undefined;
  }

  /**
   * 判断任务是否可以简化
   */
  private canSimplifyTask(todo: WorkerTodo): boolean {
    // 如果任务描述较长，可能可以简化
    return todo.content.length > 50 || todo.content.includes('和') || todo.content.includes('并');
  }

  /**
   * 简化任务
   */
  private simplifyTask(todo: WorkerTodo): string {
    const content = todo.content;

    // 尝试拆分复合任务
    if (content.includes('和')) {
      return content.split('和')[0].trim();
    }

    if (content.includes('并')) {
      return content.split('并')[0].trim();
    }

    // 截取前半部分
    const midpoint = Math.floor(content.length / 2);
    const lastSpace = content.lastIndexOf(' ', midpoint);
    if (lastSpace > 0) {
      return content.substring(0, lastSpace).trim();
    }

    return content.substring(0, midpoint).trim();
  }

  /**
   * 执行恢复策略
   */
  async executeRecovery(
    decision: RecoveryDecision,
    todo: WorkerTodo,
    assignment: Assignment
  ): Promise<{
    success: boolean;
    newAssignment?: Assignment;
    newTodo?: WorkerTodo;
  }> {
    switch (decision.strategy) {
      case 'switch_worker':
        if (decision.alternativeWorker) {
          // 创建新的 Assignment 给替代 Worker
          const newAssignment: Assignment = {
            ...assignment,
            id: `assignment_recovery_${Date.now()}`,
            workerId: decision.alternativeWorker,
            status: 'pending',
            progress: 0,
          };
          return { success: true, newAssignment };
        }
        return { success: false };

      case 'simplify_task':
        if (decision.simplifiedTask) {
          // 创建简化的 Todo
          const newTodo: WorkerTodo = {
            ...todo,
            id: `todo_simplified_${Date.now()}`,
            content: decision.simplifiedTask,
            status: 'pending',
          };
          return { success: true, newTodo };
        }
        return { success: false };

      case 'retry_same_worker':
        // 重置 Todo 状态
        const retryTodo: WorkerTodo = {
          ...todo,
          status: 'pending',
          output: undefined,
          startedAt: undefined,
          completedAt: undefined,
        };
        return { success: true, newTodo: retryTodo };

      case 'skip_task':
        // 标记为跳过
        const skippedTodo: WorkerTodo = {
          ...todo,
          status: 'skipped',
          blockedReason: decision.reason,
        };
        return { success: true, newTodo: skippedTodo };

      case 'request_human_help':
        // 需要人工介入，返回失败
        return { success: false };

      default:
        return { success: false };
    }
  }
}
