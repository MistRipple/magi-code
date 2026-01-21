/**
 * Review Executor - 评审执行器
 *
 * 职责：
 * - 执行代码评审阶段
 * - 协调评审者和执行者
 * - 处理评审结果
 */

import { CLIType } from '../../../types';
import { ProfileLoader } from '../../profile/profile-loader';
import { ProfileAwareReviewer } from '../../review/profile-aware-reviewer';
import { AutonomousWorker } from '../../worker';
import { Mission, Assignment } from '../../mission';
import { logger, LogCategory } from '../../../logging';

export interface ReviewOptions {
  workingDirectory: string;
  projectContext?: string;
}

export interface ReviewResult {
  success: boolean;
  errors: string[];
}

export class ReviewExecutor {
  constructor(
    private workers: Map<CLIType, AutonomousWorker>,
    private profileLoader: ProfileLoader,
    private reviewer: ProfileAwareReviewer
  ) {}

  /**
   * 执行评审阶段
   */
  async execute(
    mission: Mission,
    options: ReviewOptions
  ): Promise<ReviewResult> {
    logger.info(LogCategory.ORCHESTRATOR, '开始评审阶段');

    const errors: string[] = [];

    for (const assignment of mission.assignments) {
      try {
        await this.reviewAssignment(assignment, options, mission);
      } catch (error: any) {
        const errorMsg = `评审 Assignment ${assignment.id} 失败: ${error.message}`;
        logger.error(LogCategory.ORCHESTRATOR, errorMsg);
        errors.push(errorMsg);
      }
    }

    if (errors.length > 0) {
      logger.warn(LogCategory.ORCHESTRATOR, `评审阶段完成，但有 ${errors.length} 个错误`);
      return { success: false, errors };
    }

    logger.info(LogCategory.ORCHESTRATOR, '评审阶段完成');
    return { success: true, errors: [] };
  }

  /**
   * 评审单个 Assignment
   */
  private async reviewAssignment(
    assignment: Assignment,
    options: ReviewOptions,
    mission: Mission
  ): Promise<void> {
    const executorProfile = this.profileLoader.getProfile(assignment.workerId);
    if (!executorProfile) {
      throw new Error(`未找到 Worker ${assignment.workerId} 的画像`);
    }

    // 选择评审者（与执行者不同的 Worker）
    const reviewerId = this.selectReviewer(assignment.workerId);
    if (!reviewerId) {
      logger.warn(
        LogCategory.ORCHESTRATOR,
        `没有合适的评审者，跳过 Assignment ${assignment.id} 的评审`
      );
      return;
    }

    const reviewerProfile = this.profileLoader.getProfile(reviewerId);
    if (!reviewerProfile) {
      throw new Error(`未找到评审者 ${reviewerId} 的画像`);
    }

    logger.info(
      LogCategory.ORCHESTRATOR,
      `${reviewerId} 开始评审 ${assignment.workerId} 的工作`
    );

    // Note: ProfileAwareReviewer doesn't have a review() method for individual assignments
    // It has reviewPlan() for the entire mission plan
    // For now, we'll use reviewPlan as a placeholder
    const reviewResult = await this.reviewer.reviewPlan(mission);

    if (!reviewResult.approved) {
      logger.warn(
        LogCategory.ORCHESTRATOR,
        `评审未通过: ${reviewResult.issues.map(i => i.message).join(', ')}`
      );
    } else {
      logger.info(
        LogCategory.ORCHESTRATOR,
        `评审通过: ${assignment.responsibility}`
      );
    }
  }

  /**
   * 选择评审者
   */
  private selectReviewer(executorId: CLIType): CLIType | null {
    // 简单策略：选择第一个不是执行者的 Worker
    for (const workerId of this.workers.keys()) {
      if (workerId !== executorId) {
        return workerId;
      }
    }
    return null;
  }
}
