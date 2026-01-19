/**
 * Assignment Manager - 职责分配管理器
 *
 * 负责职责分配的创建、更新和管理
 */

import { CLIType } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import {
  Mission,
  Contract,
  Assignment,
  AssignmentStatus,
  AssignmentScope,
  AssignmentReason,
  CreateAssignmentParams,
  WorkerTodo,
} from './types';

/**
 * AssignmentManager - 职责分配管理器
 */
export class AssignmentManager {
  constructor(
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector
  ) {}

  /**
   * 创建职责分配
   * 基于 Mission 和参与者，生成职责分配
   */
  async createAssignments(
    mission: Mission,
    participants: CLIType[],
    contracts: Contract[]
  ): Promise<Assignment[]> {
    const assignments: Assignment[] = [];

    for (const participant of participants) {
      const assignment = await this.createAssignmentForWorker(
        mission,
        participant,
        contracts
      );
      assignments.push(assignment);
    }

    return assignments;
  }

  /**
   * 为单个 Worker 创建职责分配
   */
  private async createAssignmentForWorker(
    mission: Mission,
    workerId: CLIType,
    contracts: Contract[]
  ): Promise<Assignment> {
    const profile = this.profileLoader.getProfile(workerId);

    // 确定职责范围
    const scope = this.determineScope(mission, workerId, contracts);

    // 生成分配原因
    const assignmentReason = this.generateAssignmentReason(
      mission,
      workerId,
      profile
    );

    // 确定契约关系
    const producerContracts = contracts
      .filter(c => c.producer === workerId)
      .map(c => c.id);
    const consumerContracts = contracts
      .filter(c => c.consumers.includes(workerId))
      .map(c => c.id);

    // 生成引导 Prompt
    const guidancePrompt = this.guidanceInjector.buildWorkerPrompt(profile, {
      taskDescription: scope.includes.join('; '),
      category: assignmentReason.profileMatch.category,
      collaborators: contracts.length > 0 ? this.getCollaborators(contracts, workerId) : undefined,
      featureContract: this.formatContracts(contracts, workerId),
    });

    const now = Date.now();
    return {
      id: `assignment_${now}_${Math.random().toString(36).substr(2, 9)}`,
      missionId: mission.id,
      workerId,
      assignmentReason,
      responsibility: this.generateResponsibility(mission, workerId, scope),
      scope,
      guidancePrompt,
      producerContracts,
      consumerContracts,
      todos: [],
      planningStatus: 'pending',
      status: 'pending',
      progress: 0,
      createdAt: now,
    };
  }

  /**
   * 确定职责范围
   */
  private determineScope(
    mission: Mission,
    workerId: CLIType,
    contracts: Contract[]
  ): AssignmentScope {
    const profile = this.profileLoader.getProfile(workerId);
    const includes: string[] = [];
    const excludes: string[] = [];

    // 基于画像偏好确定职责
    for (const category of profile.preferences.preferredCategories) {
      includes.push(`${category} 相关任务`);
    }

    // 基于契约确定职责
    const producerContracts = contracts.filter(c => c.producer === workerId);
    for (const contract of producerContracts) {
      includes.push(`定义并实现 ${contract.name}`);
    }

    const consumerContracts = contracts.filter(c => c.consumers.includes(workerId));
    for (const contract of consumerContracts) {
      includes.push(`使用 ${contract.name}`);
    }

    // 基于画像弱项确定排除项
    for (const weakness of profile.profile.weaknesses) {
      excludes.push(`涉及 ${weakness} 的任务（如必须，需额外审查）`);
    }

    return {
      includes,
      excludes,
      targetPaths: [],
    };
  }

  /**
   * 生成分配原因
   */
  private generateAssignmentReason(
    mission: Mission,
    workerId: CLIType,
    profile: ReturnType<ProfileLoader['getProfile']>
  ): AssignmentReason {
    const category = this.inferCategory(mission, profile);
    const score = this.calculateMatchScore(mission, profile);

    return {
      profileMatch: {
        category,
        score,
        matchedKeywords: this.extractMatchedKeywords(mission, profile),
      },
      contractRole: 'none', // 由调用方更新
      explanation: `${workerId} 在 ${category} 类任务上有优势，匹配度 ${score}%`,
      alternatives: [],
    };
  }

  /**
   * 推断任务分类
   */
  private inferCategory(
    mission: Mission,
    profile: ReturnType<ProfileLoader['getProfile']>
  ): string {
    const text = `${mission.goal} ${mission.analysis}`.toLowerCase();

    for (const category of profile.preferences.preferredCategories) {
      if (text.includes(category)) {
        return category;
      }
    }

    return profile.preferences.preferredCategories[0] || 'general';
  }

  /**
   * 计算匹配分数
   */
  private calculateMatchScore(
    mission: Mission,
    profile: ReturnType<ProfileLoader['getProfile']>
  ): number {
    const text = `${mission.goal} ${mission.analysis}`.toLowerCase();
    let score = 50; // 基础分

    // 优势匹配加分
    for (const strength of profile.profile.strengths) {
      if (text.includes(strength.toLowerCase())) {
        score += 10;
      }
    }

    // 弱项匹配减分
    for (const weakness of profile.profile.weaknesses) {
      if (text.includes(weakness.toLowerCase())) {
        score -= 15;
      }
    }

    // 偏好分类匹配加分
    for (const category of profile.preferences.preferredCategories) {
      if (text.includes(category)) {
        score += 15;
      }
    }

    return Math.max(0, Math.min(100, score));
  }

  /**
   * 提取匹配的关键词
   */
  private extractMatchedKeywords(
    mission: Mission,
    profile: ReturnType<ProfileLoader['getProfile']>
  ): string[] {
    const text = `${mission.goal} ${mission.analysis}`.toLowerCase();
    const matched: string[] = [];

    for (const strength of profile.profile.strengths) {
      if (text.includes(strength.toLowerCase())) {
        matched.push(strength);
      }
    }

    for (const category of profile.preferences.preferredCategories) {
      if (text.includes(category)) {
        matched.push(category);
      }
    }

    return matched;
  }

  /**
   * 生成职责描述
   */
  private generateResponsibility(
    mission: Mission,
    workerId: CLIType,
    scope: AssignmentScope
  ): string {
    const profile = this.profileLoader.getProfile(workerId);
    const primaryCategory = profile.preferences.preferredCategories[0] || 'general';

    return `作为 ${profile.displayName}，负责 ${primaryCategory} 相关工作：${scope.includes.slice(0, 3).join('、')}`;
  }

  /**
   * 获取协作者列表
   */
  private getCollaborators(contracts: Contract[], workerId: CLIType): CLIType[] {
    const collaborators = new Set<CLIType>();

    for (const contract of contracts) {
      if (contract.producer === workerId) {
        contract.consumers.forEach(c => collaborators.add(c));
      }
      if (contract.consumers.includes(workerId)) {
        collaborators.add(contract.producer);
      }
    }

    collaborators.delete(workerId);
    return Array.from(collaborators);
  }

  /**
   * 格式化契约信息
   */
  private formatContracts(contracts: Contract[], workerId: CLIType): string {
    const relevant = contracts.filter(
      c => c.producer === workerId || c.consumers.includes(workerId)
    );

    if (relevant.length === 0) return '';

    const lines = relevant.map(c => {
      const role = c.producer === workerId ? '提供方' : '消费方';
      return `- ${c.name}（${role}）: ${c.description}`;
    });

    return lines.join('\n');
  }

  /**
   * 更新 Assignment 状态
   */
  updateAssignmentStatus(
    assignment: Assignment,
    newStatus: AssignmentStatus
  ): Assignment {
    const validTransitions: Record<AssignmentStatus, AssignmentStatus[]> = {
      pending: ['planning'],
      planning: ['ready', 'pending'],
      ready: ['executing', 'blocked'],
      executing: ['completed', 'failed', 'blocked'],
      blocked: ['executing', 'failed'],
      completed: [],
      failed: ['pending'],
    };

    if (!validTransitions[assignment.status].includes(newStatus)) {
      throw new Error(
        `Invalid assignment status transition: ${assignment.status} -> ${newStatus}`
      );
    }

    return {
      ...assignment,
      status: newStatus,
      startedAt: newStatus === 'executing' ? Date.now() : assignment.startedAt,
      completedAt: newStatus === 'completed' ? Date.now() : assignment.completedAt,
    };
  }

  /**
   * 添加 Todo 到 Assignment
   */
  addTodo(assignment: Assignment, todo: WorkerTodo): Assignment {
    return {
      ...assignment,
      todos: [...assignment.todos, todo],
    };
  }

  /**
   * 更新 Assignment 中的 Todo
   */
  updateTodo(assignment: Assignment, todo: WorkerTodo): Assignment {
    const todoIndex = assignment.todos.findIndex(t => t.id === todo.id);
    if (todoIndex === -1) {
      throw new Error(`Todo not found: ${todo.id}`);
    }

    const newTodos = [...assignment.todos];
    newTodos[todoIndex] = todo;

    // 重新计算进度
    const completedCount = newTodos.filter(
      t => t.status === 'completed' || t.status === 'skipped'
    ).length;
    const progress = newTodos.length > 0
      ? Math.round((completedCount / newTodos.length) * 100)
      : 0;

    return {
      ...assignment,
      todos: newTodos,
      progress,
    };
  }

  /**
   * 检查 Assignment 是否完成
   */
  isAssignmentComplete(assignment: Assignment): boolean {
    if (assignment.todos.length === 0) return false;

    return assignment.todos.every(
      t => t.status === 'completed' || t.status === 'skipped'
    );
  }

  /**
   * 获取下一个可执行的 Todo
   */
  getNextExecutableTodo(assignment: Assignment): WorkerTodo | null {
    for (const todo of assignment.todos) {
      if (todo.status !== 'pending') continue;

      // 检查超范围审批
      if (todo.outOfScope && todo.approvalStatus !== 'approved') continue;

      // 检查依赖
      const dependenciesMet = todo.dependsOn.every(depId => {
        const depTodo = assignment.todos.find(t => t.id === depId);
        return depTodo && depTodo.status === 'completed';
      });

      if (dependenciesMet) {
        return todo;
      }
    }

    return null;
  }
}
