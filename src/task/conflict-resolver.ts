/**
 * ConflictResolver - Worker 选择冲突解决器（单一数据源）
 *
 * 仅允许根据分类归属进行选择，不支持多源兜底或替代分支。
 */

import { WorkerSlot } from '../types';
import { AssignmentResolver } from '../orchestrator/profile/assignment-resolver';
import { WorkerAssignmentLoader } from '../orchestrator/profile/worker-assignments';

/** 冲突解决输入 */
export interface ConflictResolutionInput {
  /** 用户手动选择的 Worker */
  userPreference?: WorkerSlot;
  /** 任务分类 */
  category?: string;
  /** 可用的 Worker 列表 */
  availableWorkers: WorkerSlot[];
}

/** 冲突解决结果 */
export interface ConflictResolutionResult {
  /** 最终选择的 Worker */
  worker: WorkerSlot;
  /** 选择原因 */
  reason: string;
  /** 使用的决策层级 */
  level: 'user' | 'profile';
  /** 是否发生了切换 */
  switched: boolean;
  /** 置信度 (0-1) */
  confidence: number;
}

/**
 * Worker 选择冲突解决器
 */
export class ConflictResolver {
  private assignmentResolver = new AssignmentResolver(new WorkerAssignmentLoader());

  /**
   * 解决 Worker 选择冲突
   * - 必须提供 category
   * - category → AssignmentResolver 归属 Worker
   * - 归属 Worker 不可用时直接失败
   */
  resolve(input: ConflictResolutionInput): ConflictResolutionResult {
    const { category, availableWorkers, userPreference } = input;

    if (!category) {
      throw new Error('缺少任务分类，无法解析 Worker');
    }

    const worker = this.assignmentResolver.resolveWorker(category);

    if (userPreference && userPreference !== worker) {
      throw new Error(`用户指定 ${userPreference} 与分类归属 ${worker} 冲突`);
    }

    if (!availableWorkers.includes(worker)) {
      throw new Error(`分类 "${category}" 归属 ${worker}，但当前不可用`);
    }

    return {
      worker,
      reason: `分类 "${category}" 归属 ${worker}`,
      level: userPreference ? 'user' : 'profile',
      switched: false,
      confidence: 1.0,
    };
  }
}
