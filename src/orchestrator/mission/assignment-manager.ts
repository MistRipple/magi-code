/**
 * Assignment Manager - 职责分配管理器
 *
 * 负责职责分配的创建、更新和管理
 */

import { WorkerSlot } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector, TaskStructuredInfo } from '../profile/guidance-injector';
import { CategoryResolver } from '../profile/category-resolver';
import { AssignmentResolver } from '../profile/assignment-resolver';
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
  private categoryResolver = new CategoryResolver();
  private assignmentResolver: AssignmentResolver;

  constructor(
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector
  ) {
    this.assignmentResolver = new AssignmentResolver(profileLoader.getAssignmentLoader());
  }

  /**
   * 创建职责分配
   * 基于 Mission 和参与者，生成职责分配
   */
  async createAssignments(
    mission: Mission,
    participants: WorkerSlot[],
    contracts: Contract[],
    options?: {
      taskInfo?: TaskStructuredInfo;
      additionalContext?: string;
      routingCategory?: string;
      routingCategories?: Record<string, string>;
      routingReason?: string;
      requiresModification?: boolean;
      /** AI 生成的委托说明（按 worker 索引对应） */
      delegationBriefings?: string[];
    }
  ): Promise<Assignment[]> {
    const assignments: Assignment[] = [];

    for (let i = 0; i < participants.length; i++) {
      const participant = participants[i];
      const delegationBriefing = options?.delegationBriefings?.[i];
      // 使用 per-worker 分类，如果有的话
      const workerCategory = options?.routingCategories?.[participant] || options?.routingCategory;
      const assignment = await this.createAssignmentForWorker(
        mission,
        participant,
        contracts,
        { ...options, delegationBriefing, routingCategory: workerCategory }
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
    workerId: WorkerSlot,
    contracts: Contract[],
    options?: {
      taskInfo?: TaskStructuredInfo;
      additionalContext?: string;
      routingCategory?: string;
      routingReason?: string;
      requiresModification?: boolean;
      /** AI 生成的委托说明 */
      delegationBriefing?: string;
    }
  ): Promise<Assignment> {
    const profile = this.profileLoader.getProfile(workerId);

    // 确定职责范围
    const scope = this.determineScope(mission, workerId, contracts, options?.requiresModification);

    // 生成分配原因
    const assignmentReason = this.generateAssignmentReason(
      mission,
      workerId,
      profile,
      options?.routingCategory,
      options?.routingReason
    );

    // 确定契约关系
    const producerContracts = contracts
      .filter(c => c.producer === workerId)
      .map(c => c.id);
    const consumerContracts = contracts
      .filter(c => c.consumers.includes(workerId))
      .map(c => c.id);

    // 生成引导 Prompt
    const guidancePrompt = this.guidanceInjector.buildFullTaskPrompt(
      profile,
      {
        taskDescription: scope.includes.join('; '),
        category: assignmentReason.profileMatch.category,
        collaborators: contracts.length > 0 ? this.getCollaborators(contracts, workerId) : undefined,
        featureContract: this.formatContracts(contracts, workerId),
      },
      options?.additionalContext || mission.context,
      options?.taskInfo
    );

    const now = Date.now();
    return {
      id: `assignment_${now}_${Math.random().toString(36).substr(2, 9)}`,
      missionId: mission.id,
      workerId,
      assignmentReason,
      responsibility: this.generateResponsibility(
        mission,
        workerId,
        scope,
        assignmentReason.profileMatch.category
      ),
      delegationBriefing: options?.delegationBriefing,
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
    workerId: WorkerSlot,
    contracts: Contract[],
    requiresModification?: boolean
  ): AssignmentScope {
    const profile = this.profileLoader.getProfile(workerId);
    const includes: string[] = [];
    const excludes: string[] = [];

    // 基于归属分类确定职责
    for (const category of profile.assignedCategories) {
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
    for (const weakness of profile.persona.weaknesses) {
      excludes.push(`涉及 ${weakness} 的任务（如必须，需额外审查）`);
    }

    const targetPaths = this.extractTargetPaths(mission.userPrompt);

    return {
      includes,
      excludes,
      targetPaths,
      requiresModification,
    };
  }

  private extractTargetPaths(prompt: string): string[] {
    const filePattern = /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|scss|html|json|md|yaml|yml|txt)/gi;
    const matches = prompt.match(filePattern);
    return matches ? [...new Set(matches)] : [];
  }

  /**
   * 生成分配原因
   */
  private generateAssignmentReason(
    mission: Mission,
    workerId: WorkerSlot,
    profile: ReturnType<ProfileLoader['getProfile']>,
    routingCategory?: string,
    routingReason?: string
  ): AssignmentReason {
    if (routingCategory) {
      const resolvedWorker = this.assignmentResolver.resolveWorker(routingCategory);
      if (resolvedWorker !== workerId) {
        throw new Error(`分类 "${routingCategory}" 归属 ${resolvedWorker}，不可分配给 ${workerId}`);
      }
      return {
        profileMatch: {
          category: routingCategory,
          score: 100,
          matchedKeywords: [],
        },
        contractRole: 'none',
        explanation: routingReason || `编排者路由指定 ${workerId} 执行 ${routingCategory} 类任务`,
        alternatives: [],
      };
    }

    const category = this.resolveCategory(mission);
    const resolvedWorker = this.assignmentResolver.resolveWorker(category);
    if (resolvedWorker !== workerId) {
      throw new Error(`分类 "${category}" 归属 ${resolvedWorker}，不可分配给 ${workerId}`);
    }
    const score = this.calculateMatchScore(mission, profile);

    return {
      profileMatch: {
        category,
        score,
        matchedKeywords: this.extractMatchedKeywords(mission, category, profile),
      },
      contractRole: 'none', // 由调用方更新
      explanation: `${workerId} 在 ${category} 类任务上有优势，匹配度 ${score}%`,
      alternatives: [],
    };
  }

  /**
   * 解析任务分类（唯一规则）
   */
  private resolveCategory(mission: Mission): string {
    const text = `${mission.goal} ${mission.analysis}`.toLowerCase();
    return this.categoryResolver.resolveFromText(text);
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
    for (const strength of profile.persona.strengths) {
      if (text.includes(strength.toLowerCase())) {
        score += 10;
      }
    }

    // 弱项匹配减分
    for (const weakness of profile.persona.weaknesses) {
      if (text.includes(weakness.toLowerCase())) {
        score -= 15;
      }
    }

    return Math.max(0, Math.min(100, score));
  }

  /**
   * 提取匹配的关键词
   */
  private extractMatchedKeywords(
    mission: Mission,
    category: string,
    profile: ReturnType<ProfileLoader['getProfile']>
  ): string[] {
    const text = `${mission.goal} ${mission.analysis}`.toLowerCase();
    const matched: string[] = [];

    for (const strength of profile.persona.strengths) {
      if (text.includes(strength.toLowerCase())) {
        matched.push(strength);
      }
    }

    const categoryDef = this.profileLoader.getCategory(category);
    if (categoryDef) {
      for (const keyword of categoryDef.keywords) {
        try {
          const regex = new RegExp(keyword, 'i');
          if (regex.test(text)) {
            matched.push(keyword);
          }
        } catch {
          if (text.includes(keyword.toLowerCase())) {
            matched.push(keyword);
          }
        }
      }
    }

    return matched;
  }

  /**
   * 生成职责描述
   */
  private generateResponsibility(
    mission: Mission,
    workerId: WorkerSlot,
    scope: AssignmentScope,
    category: string
  ): string {
    const profile = this.profileLoader.getProfile(workerId);
    const primaryCategory = category || profile.assignedCategories[0] || 'general';
    const parts: string[] = [
      `作为 ${profile.persona.displayName}，负责 ${primaryCategory} 相关工作。`,
    ];

    const goal = mission.goal || mission.userPrompt;
    if (goal) {
      parts.push(`目标: ${goal}`);
    }

    if (mission.userPrompt && mission.userPrompt !== goal) {
      parts.push(`原始需求: ${mission.userPrompt}`);
    }

    if (scope.targetPaths && scope.targetPaths.length > 0) {
      parts.push(`目标路径: ${scope.targetPaths.join(', ')}`);
    }

    if (scope.includes.length > 0) {
      parts.push(`职责范围: ${scope.includes.slice(0, 5).join('、')}`);
    }

    return parts.join('\n');
  }

  /**
   * 获取协作者列表
   */
  private getCollaborators(contracts: Contract[], workerId: WorkerSlot): WorkerSlot[] {
    const collaborators = new Set<WorkerSlot>();

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
  private formatContracts(contracts: Contract[], workerId: WorkerSlot): string {
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
