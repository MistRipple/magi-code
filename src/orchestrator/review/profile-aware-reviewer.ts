/**
 * Profile Aware Reviewer - 画像感知评审器
 *
 * 让评审决策基于 Worker 画像，包括：
 * - 检查任务分配是否符合 Worker 能力
 * - 基于 strengths 智能选择互检评审者
 * - 基于风险 + 弱项决定评审严格度
 */

import { WorkerSlot } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { WorkerProfile } from '../profile/types';
import {
  Mission,
  Assignment,
  WorkerTodo,
  PlanIssue,
  PlanReviewResult,
  ReviewLevel,
} from '../mission/types';

/**
 * 规划评审结果
 */
export interface PlanEvaluationResult {
  approved: boolean;
  issues: PlanIssue[];
  suggestions: string[];
}

/**
 * ProfileAwareReviewer - 画像感知评审器
 */
export class ProfileAwareReviewer {
  constructor(private profileLoader: ProfileLoader) {}

  /**
   * 计划评审：检查任务分配是否符合 Worker 能力
   */
  async reviewPlan(mission: Mission): Promise<PlanEvaluationResult> {
    const issues: PlanIssue[] = [];
    const suggestions: string[] = [];

    for (const assignment of mission.assignments) {
      const profile = this.profileLoader.getProfile(assignment.workerId);
      const category = this.inferCategory(assignment);

      // 1. 检查是否分配给了擅长该分类的 Worker
      if (!profile.preferences.preferredCategories.includes(category)) {
        const betterWorker = this.findBetterWorker(category, assignment.workerId);
        issues.push({
          type: 'suboptimal_assignment',
          taskId: assignment.id,
          message: `任务分类 "${category}" 不在 ${assignment.workerId} 的擅长领域`,
          suggestion: betterWorker
            ? `建议分配给 ${betterWorker}`
            : undefined,
        });
      }

      // 2. 检查任务是否涉及 Worker 的弱项
      const weaknessHits = this.findWeaknessMatches(
        assignment.responsibility,
        profile
      );
      for (const weakness of weaknessHits) {
        issues.push({
          type: 'weakness_match',
          taskId: assignment.id,
          message: `任务涉及 ${assignment.workerId} 的弱项: "${weakness}"`,
          reviewLevel: 'strict',
        });
        suggestions.push(
          `建议对 ${assignment.workerId} 的输出进行更严格的评审，特别关注 "${weakness}" 相关内容`
        );
      }

      // 3. 检查 Todo 列表
      for (const todo of assignment.todos) {
        const todoIssues = this.reviewTodo(todo, assignment, profile);
        issues.push(...todoIssues);
      }
    }

    return {
      approved: issues.filter(i => i.type === 'critical').length === 0,
      issues,
      suggestions,
    };
  }

  /**
   * 评审单个 Todo
   */
  private reviewTodo(
    todo: WorkerTodo,
    assignment: Assignment,
    profile: WorkerProfile
  ): PlanIssue[] {
    const issues: PlanIssue[] = [];

    // 检查是否超出职责范围
    if (todo.outOfScope && todo.approvalStatus !== 'approved') {
      issues.push({
        type: 'scope_violation',
        taskId: todo.id,
        message: `Todo "${todo.content}" 超出职责范围，需要审批`,
      });
    }

    // 检查是否涉及 Worker 弱项
    const weaknessHits = this.findWeaknessMatches(todo.content, profile);
    for (const weakness of weaknessHits) {
      issues.push({
        type: 'weakness_match',
        taskId: todo.id,
        message: `Todo 涉及 ${assignment.workerId} 的弱项: "${weakness}"`,
        reviewLevel: 'strict',
      });
    }

    // 检查依赖是否合理
    for (const depId of todo.dependsOn) {
      const depTodo = assignment.todos.find(t => t.id === depId);
      if (!depTodo) {
        issues.push({
          type: 'missing_dependency',
          taskId: todo.id,
          message: `Todo "${todo.content}" 依赖的 Todo ${depId} 不存在`,
        });
      }
    }

    return issues;
  }

  /**
   * 互检评审者选择：基于能力画像匹配
   */
  selectPeerReviewer(assignment: Assignment, executor: WorkerSlot): WorkerSlot {
    const category = this.inferCategory(assignment);
    const allProfiles = this.profileLoader.getAllProfiles();

    // 选择擅长该分类且不是执行者的 Worker
    const candidates = Array.from(allProfiles.entries())
      .filter(([worker]) => worker !== executor)
      .filter(([_, profile]) =>
        profile.preferences.preferredCategories.includes(category)
      )
      .sort((a, b) => {
        // 优先选择该分类是第一优先的 Worker
        const aIndex = a[1].preferences.preferredCategories.indexOf(category);
        const bIndex = b[1].preferences.preferredCategories.indexOf(category);
        return aIndex - bIndex;
      });

    if (candidates.length > 0) {
      return candidates[0][0] as WorkerSlot;
    }

    // 没有找到合适的评审者，返回默认
    return executor === 'claude' ? 'codex' : 'claude';
  }

  /**
   * 评审严格度：基于分类风险 + Worker 弱项
   */
  determineReviewLevel(assignment: Assignment, executor: WorkerSlot): ReviewLevel {
    const category = this.inferCategory(assignment);
    const categoryConfig = this.profileLoader.getCategory(category);
    const profile = this.profileLoader.getProfile(executor);

    // 基础严格度来自分类风险
    let level: ReviewLevel =
      categoryConfig?.riskLevel === 'high'
        ? 'strict'
        : categoryConfig?.riskLevel === 'medium'
          ? 'standard'
          : 'light';

    // 如果任务涉及 Worker 弱项，提升严格度
    const involvesWeakness = profile.profile.weaknesses.some(w =>
      assignment.responsibility.toLowerCase().includes(w.toLowerCase())
    );

    if (involvesWeakness && level !== 'strict') {
      level = level === 'light' ? 'standard' : 'strict';
    }

    return level;
  }

  /**
   * 评审 Todo 执行结果
   */
  async reviewTodoOutput(
    todo: WorkerTodo,
    assignment: Assignment
  ): Promise<PlanReviewResult> {
    const executor = assignment.workerId;
    const profile = this.profileLoader.getProfile(executor);
    const reviewLevel = this.determineReviewLevel(assignment, executor);

    const issues: PlanIssue[] = [];

    // 检查执行结果
    if (!todo.output) {
      return {
        status: 'needs_revision',
        feedback: 'Todo 未执行或未产生输出',
        issues: [
          {
            type: 'critical',
            taskId: todo.id,
            message: 'Todo 未执行',
          },
        ],
        reviewedAt: Date.now(),
      };
    }

    if (!todo.output.success) {
      return {
        status: 'needs_revision',
        feedback: `执行失败: ${todo.output.error}`,
        issues: [
          {
            type: 'critical',
            taskId: todo.id,
            message: todo.output.error || '执行失败',
          },
        ],
        reviewedAt: Date.now(),
      };
    }

    // 基于评审级别检查
    if (reviewLevel === 'strict') {
      // 严格评审：检查是否涉及 Worker 弱项
      const weaknessHits = this.findWeaknessMatches(
        todo.output.summary,
        profile
      );
      for (const weakness of weaknessHits) {
        issues.push({
          type: 'weakness_match',
          taskId: todo.id,
          message: `输出可能涉及 ${executor} 的弱项: "${weakness}"`,
          reviewLevel: 'strict',
        });
      }
    }

    if (issues.length > 0) {
      return {
        status: 'needs_revision',
        feedback: `发现 ${issues.length} 个问题需要修复`,
        issues,
        reviewedAt: Date.now(),
      };
    }

    return {
      status: 'approved',
      feedback: '评审通过',
      reviewedAt: Date.now(),
    };
  }

  /**
   * 推断任务分类
   */
  private inferCategory(assignment: Assignment): string {
    const text = assignment.responsibility.toLowerCase();
    const categories = this.profileLoader.getAllCategories();

    for (const [categoryId, config] of Object.entries(categories)) {
      for (const keywordPattern of config.keywords) {
        const keywords = keywordPattern.split('|');
        if (keywords.some((k: string) => text.includes(k.toLowerCase()))) {
          return categoryId;
        }
      }
    }

    return 'general';
  }

  /**
   * 查找更合适的 Worker
   */
  private findBetterWorker(
    category: string,
    currentWorker: WorkerSlot
  ): WorkerSlot | null {
    const allProfiles = this.profileLoader.getAllProfiles();

    for (const [worker, profile] of allProfiles.entries()) {
      if (worker === currentWorker) continue;
      if (profile.preferences.preferredCategories.includes(category)) {
        return worker as WorkerSlot;
      }
    }

    return null;
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
}
