/**
 * Profile Aware Reviewer - 画像感知评审器
 *
 * 让评审决策基于 Worker 画像，包括：
 * - 检查任务分配是否符合 Worker 能力
 * - 基于 strengths 智能选择互检评审者
 * - 基于风险 + 弱项决定评审严格度
 */

import { WorkerSlot } from '../../types';
import type { UnifiedTodo } from '../../todo/types';
import { ProfileLoader } from '../profile/profile-loader';
import { AssignmentResolver } from '../profile/assignment-resolver';
import { WorkerProfile } from '../profile/types';
import {
  Mission,
  Assignment,
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
  private assignmentResolver: AssignmentResolver;

  constructor(private profileLoader: ProfileLoader) {
    this.assignmentResolver = new AssignmentResolver(profileLoader.getAssignmentLoader());
  }

  /**
   * 计划评审：检查任务分配是否符合 Worker 能力
   */
  async reviewPlan(mission: Mission): Promise<PlanEvaluationResult> {
    const issues: PlanIssue[] = [];
    const suggestions: string[] = [];

    for (const assignment of mission.assignments) {
      const profile = this.profileLoader.getProfile(assignment.workerId);
      const category = this.getAssignmentCategory(assignment);

      const resolvedWorker = this.assignmentResolver.resolveWorker(category);
      if (resolvedWorker !== assignment.workerId) {
        issues.push({
          type: 'critical',
          taskId: assignment.id,
          message: `分类 "${category}" 归属 ${resolvedWorker}，但当前分配为 ${assignment.workerId}`,
          suggestion: `请修正 worker-assignments.json 归属或重新分配该任务`,
        });
      }

      // 1. 检查任务是否涉及 Worker 的弱项
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

      // 2. 检查 Todo 列表
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
    todo: UnifiedTodo,
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
    const category = this.getAssignmentCategory(assignment);
    const allProfiles = this.profileLoader.getAllProfiles();

    const categoryDef = this.profileLoader.getCategory(category);
    const candidates = Array.from(allProfiles.entries())
      .filter(([worker]) => worker !== executor)
      .sort((a, b) => {
        if (!categoryDef) return 0;
        const aScore = this.scorePersonaAgainstCategory(a[1], categoryDef);
        const bScore = this.scorePersonaAgainstCategory(b[1], categoryDef);
        return bScore - aScore;
      });

    if (candidates.length > 0) {
      return candidates[0][0] as WorkerSlot;
    }

    const otherWorkers = Array.from(allProfiles.keys()).filter(w => w !== executor);
    if (otherWorkers.length === 0) {
      throw new Error(`无法选择评审者，当前仅有执行者 ${executor}`);
    }
    return otherWorkers[0];
  }

  /**
   * 评审严格度：基于分类风险 + Worker 弱项
   */
  determineReviewLevel(assignment: Assignment, executor: WorkerSlot): ReviewLevel {
    const category = this.getAssignmentCategory(assignment);
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
    const involvesWeakness = profile.persona.weaknesses.some(w =>
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
    todo: UnifiedTodo,
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
  private getAssignmentCategory(assignment: Assignment): string {
    const category = assignment.assignmentReason?.profileMatch?.category;
    if (!category) {
      throw new Error(`Assignment ${assignment.id} 缺少分类信息`);
    }
    return category;
  }

  private scorePersonaAgainstCategory(profile: WorkerProfile, categoryDef: { guidance: { focus: string[] } }): number {
    const focus = categoryDef.guidance?.focus || [];
    if (focus.length === 0) return 0;
    const strengths = profile.persona.strengths.map(s => s.toLowerCase());
    return focus.reduce((score, item) => {
      const normalized = item.toLowerCase();
      return strengths.some(s => normalized.includes(s) || s.includes(normalized)) ? score + 1 : score;
    }, 0);
  }

  /**
   * 查找弱项匹配
   */
  private findWeaknessMatches(text: string, profile: WorkerProfile): string[] {
    const textLower = text.toLowerCase();
    return profile.persona.weaknesses.filter(w =>
      textLower.includes(w.toLowerCase())
    );
  }
}
