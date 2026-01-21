/**
 * Todo Planner - Worker 自主规划器
 *
 * 核心功能：
 * - 将职责（Assignment）转化为具体的 Todo 列表
 * - 支持 Worker 自主规划工作步骤
 * - 检测超范围任务并标记
 * - 支持规划修订和批准流程
 */

import { CLIType } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import {
  Assignment,
  WorkerTodo,
  TodoType,
  TodoStatus,
  CreateTodoParams,
} from '../mission/types';

/**
 * Todo 规划上下文
 */
export interface PlanningContext {
  /** 职责描述 */
  responsibility: string;
  /** 职责范围 */
  scope: {
    includes: string[];
    excludes: string[];
  };
  /** 可用契约 */
  availableContracts: string[];
  /** 项目上下文（静态） */
  projectContext?: string;
  /** 会话上下文快照（动态，来自 ContextManager） */
  contextSnapshot?: string;
}

/**
 * 规划结果
 */
export interface PlanningResult {
  todos: WorkerTodo[];
  outOfScopeTodos: WorkerTodo[];
  warnings: string[];
}

/**
 * 规划审查反馈
 */
export interface PlanReviewFeedback {
  /** 审查状态 */
  status: 'approved' | 'needs_revision' | 'rejected';
  /** 需要添加的 Todo */
  todosToAdd?: CreateTodoParams[];
  /** 需要移除的 Todo ID */
  todosToRemove?: string[];
  /** 需要修改的 Todo */
  todosToModify?: Array<{
    todoId: string;
    updates: Partial<Pick<WorkerTodo, 'content' | 'reasoning' | 'priority' | 'dependsOn'>>;
  }>;
  /** 审查意见 */
  comments?: string;
  /** 拒绝原因（如果被拒绝） */
  rejectionReason?: string;
}

/**
 * 规划修订结果
 */
export interface PlanRevisionResult {
  /** 修订后的规划 */
  revisedPlan: PlanningResult;
  /** 添加的 Todo 数量 */
  todosAdded: number;
  /** 移除的 Todo 数量 */
  todosRemoved: number;
  /** 修改的 Todo 数量 */
  todosModified: number;
  /** 修订说明 */
  revisionNotes: string[];
}

/**
 * TodoPlanner - Worker 自主规划器
 */
export class TodoPlanner {
  constructor(
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector
  ) {}

  /**
   * 为 Assignment 生成 Todo 规划
   * 这是让 Worker 自主规划的入口
   */
  async planTodos(
    assignment: Assignment,
    context: PlanningContext
  ): Promise<PlanningResult> {
    const profile = this.profileLoader.getProfile(assignment.workerId);
    const todos: WorkerTodo[] = [];
    const outOfScopeTodos: WorkerTodo[] = [];
    const warnings: string[] = [];

    // 1. 分析职责，生成基础 Todo 结构
    const baseTodos = this.analyzeResponsibility(assignment, context);

    // 2. 检查每个 Todo 是否在范围内
    for (const todo of baseTodos) {
      const scopeCheck = this.checkScope(todo, context.scope);

      if (scopeCheck.outOfScope) {
        todo.outOfScope = true;
        todo.approvalStatus = 'pending';
        todo.approvalNote = scopeCheck.reason;
        outOfScopeTodos.push(todo);
        warnings.push(`Todo "${todo.content}" 超出职责范围: ${scopeCheck.reason}`);
      }

      todos.push(todo);
    }

    // 3. 建立依赖关系
    this.establishDependencies(todos);

    // 4. 检查契约依赖
    this.checkContractDependencies(todos, context.availableContracts, warnings);

    return {
      todos,
      outOfScopeTodos,
      warnings,
    };
  }

  /**
   * 创建单个 Todo
   */
  createTodo(params: CreateTodoParams): WorkerTodo {
    const now = Date.now();
    return {
      id: `todo_${now}_${Math.random().toString(36).substr(2, 9)}`,
      assignmentId: params.assignmentId,
      content: params.content,
      reasoning: params.reasoning,
      expectedOutput: params.expectedOutput || '',
      type: params.type,
      priority: params.priority || 3,
      outOfScope: false,
      dependsOn: params.dependsOn || [],
      requiredContracts: params.requiredContracts || [],
      producesContracts: params.producesContracts || [],
      status: 'pending',
      createdAt: now,
    };
  }

  /**
   * 分析职责，生成基础 Todo 结构
   */
  private analyzeResponsibility(
    assignment: Assignment,
    context: PlanningContext
  ): WorkerTodo[] {
    const todos: WorkerTodo[] = [];
    const responsibility = context.responsibility.toLowerCase();

    // 1. 探索/调研 Todo（如果需要）
    if (this.needsDiscovery(responsibility)) {
      todos.push(
        this.createTodo({
          assignmentId: assignment.id,
          content: '探索和了解相关代码结构',
          reasoning: '在实现前需要了解现有代码',
          type: 'discovery',
          priority: 1,
        })
      );
    }

    // 2. 设计 Todo（如果是新功能）
    if (this.needsDesign(responsibility)) {
      todos.push(
        this.createTodo({
          assignmentId: assignment.id,
          content: '设计实现方案',
          reasoning: '需要先规划实现路径',
          type: 'design',
          priority: 2,
        })
      );
    }

    // 3. 实现 Todo（核心）
    todos.push(
      this.createTodo({
        assignmentId: assignment.id,
        content: `执行职责任务：${assignment.responsibility}`,
        reasoning: '完成职责的主要工作',
        expectedOutput: assignment.responsibility,
        type: 'implementation',
        priority: 3,
      })
    );

    // 4. 验证 Todo
    todos.push(
      this.createTodo({
        assignmentId: assignment.id,
        content: `验证任务结果是否满足目标：${assignment.responsibility}`,
        reasoning: '确保实现正确',
        type: 'verification',
        priority: 4,
      })
    );

    // 5. 集成 Todo（如果有契约）
    if (assignment.producerContracts.length > 0 || assignment.consumerContracts.length > 0) {
      todos.push(
        this.createTodo({
          assignmentId: assignment.id,
          content: '集成契约接口',
          reasoning: '与其他 Worker 的工作对接',
          type: 'integration',
          priority: 5,
          requiredContracts: [
            ...assignment.producerContracts,
            ...assignment.consumerContracts,
          ],
        })
      );
    }

    return todos;
  }

  /**
   * 检查是否需要探索阶段
   */
  private needsDiscovery(responsibility: string): boolean {
    const keywords = ['理解', '了解', '分析', '探索', 'understand', 'analyze', 'explore'];
    return keywords.some(k => responsibility.includes(k));
  }

  /**
   * 检查是否需要设计阶段
   */
  private needsDesign(responsibility: string): boolean {
    const keywords = ['新增', '创建', '设计', '实现', 'new', 'create', 'design', 'implement'];
    return keywords.some(k => responsibility.includes(k));
  }

  /**
   * 检查 Todo 是否在职责范围内
   */
  private checkScope(
    todo: WorkerTodo,
    scope: { includes: string[]; excludes: string[] }
  ): { outOfScope: boolean; reason?: string } {
    const content = todo.content.toLowerCase();

    // 检查是否在排除列表中
    for (const exclude of scope.excludes) {
      const excludeLower = exclude.toLowerCase();
      if (content.includes(excludeLower)) {
        return {
          outOfScope: true,
          reason: `涉及排除项: "${exclude}"`,
        };
      }
    }

    // 检查是否在包含列表中（如果有明确的包含列表）
    if (scope.includes.length > 0) {
      const isIncluded = scope.includes.some(include =>
        content.includes(include.toLowerCase())
      );
      if (!isIncluded) {
        // 不强制要求必须在包含列表中，只是一个参考
        // return { outOfScope: true, reason: '不在明确的职责范围内' };
      }
    }

    return { outOfScope: false };
  }

  /**
   * 建立 Todo 之间的依赖关系
   */
  private establishDependencies(todos: WorkerTodo[]): void {
    // 按类型排序建立依赖
    const typeOrder: TodoType[] = [
      'discovery',
      'design',
      'implementation',
      'verification',
      'integration',
    ];

    const todosByType = new Map<TodoType, WorkerTodo[]>();
    for (const todo of todos) {
      if (!todosByType.has(todo.type)) {
        todosByType.set(todo.type, []);
      }
      todosByType.get(todo.type)!.push(todo);
    }

    // 建立类型间的依赖
    for (let i = 1; i < typeOrder.length; i++) {
      const currentType = typeOrder[i];
      const previousType = typeOrder[i - 1];

      const currentTodos = todosByType.get(currentType) || [];
      const previousTodos = todosByType.get(previousType) || [];

      for (const current of currentTodos) {
        for (const previous of previousTodos) {
          if (!current.dependsOn.includes(previous.id)) {
            current.dependsOn.push(previous.id);
          }
        }
      }
    }
  }

  /**
   * 检查契约依赖
   */
  private checkContractDependencies(
    todos: WorkerTodo[],
    availableContracts: string[],
    warnings: string[]
  ): void {
    for (const todo of todos) {
      for (const requiredContract of todo.requiredContracts) {
        if (!availableContracts.includes(requiredContract)) {
          todo.status = 'blocked';
          todo.blockedReason = `等待契约: ${requiredContract}`;
          warnings.push(`Todo "${todo.content}" 被阻塞，等待契约: ${requiredContract}`);
        }
      }
    }
  }

  /**
   * 动态添加 Todo
   * 在执行过程中发现需要额外工作时调用
   */
  addDynamicTodo(
    assignment: Assignment,
    params: CreateTodoParams
  ): WorkerTodo {
    const todo = this.createTodo(params);

    // 检查范围
    const scopeCheck = this.checkScope(todo, assignment.scope);
    if (scopeCheck.outOfScope) {
      todo.outOfScope = true;
      todo.approvalStatus = 'pending';
      todo.approvalNote = scopeCheck.reason;
    }

    return todo;
  }

  /**
   * 更新 Todo 状态
   */
  updateTodoStatus(
    todo: WorkerTodo,
    newStatus: TodoStatus,
    output?: WorkerTodo['output']
  ): WorkerTodo {
    const validTransitions: Record<TodoStatus, TodoStatus[]> = {
      pending: ['in_progress', 'blocked', 'skipped'],
      blocked: ['pending', 'in_progress', 'skipped'],
      in_progress: ['completed', 'failed', 'blocked'],
      completed: [],
      failed: ['pending', 'in_progress'],
      skipped: [],
    };

    if (!validTransitions[todo.status].includes(newStatus)) {
      throw new Error(
        `Invalid todo status transition: ${todo.status} -> ${newStatus}`
      );
    }

    return {
      ...todo,
      status: newStatus,
      startedAt: newStatus === 'in_progress' ? Date.now() : todo.startedAt,
      completedAt: newStatus === 'completed' || newStatus === 'failed' ? Date.now() : todo.completedAt,
      output: output || todo.output,
    };
  }

  // ============= 规划修订流程 =============

  /**
   * 应用规划修订
   *
   * 根据审查反馈修订现有规划
   */
  revisePlan(
    currentPlan: PlanningResult,
    feedback: PlanReviewFeedback,
    assignment: Assignment
  ): PlanRevisionResult {
    const revisionNotes: string[] = [];
    let todosAdded = 0;
    let todosRemoved = 0;
    let todosModified = 0;

    // 复制当前 todos
    let todos = [...currentPlan.todos];
    let outOfScopeTodos = [...currentPlan.outOfScopeTodos];
    const warnings = [...currentPlan.warnings];

    // 1. 移除指定的 Todo
    if (feedback.todosToRemove && feedback.todosToRemove.length > 0) {
      const removeSet = new Set(feedback.todosToRemove);
      const originalCount = todos.length;

      todos = todos.filter(t => !removeSet.has(t.id));
      outOfScopeTodos = outOfScopeTodos.filter(t => !removeSet.has(t.id));

      todosRemoved = originalCount - todos.length;
      revisionNotes.push(`移除了 ${todosRemoved} 个 Todo`);

      // 更新依赖关系（移除对已删除 Todo 的依赖）
      for (const todo of todos) {
        todo.dependsOn = todo.dependsOn.filter(id => !removeSet.has(id));
      }
    }

    // 2. 修改指定的 Todo
    if (feedback.todosToModify && feedback.todosToModify.length > 0) {
      for (const modification of feedback.todosToModify) {
        const todoIndex = todos.findIndex(t => t.id === modification.todoId);
        if (todoIndex !== -1) {
          todos[todoIndex] = {
            ...todos[todoIndex],
            ...modification.updates,
          };
          todosModified++;
        }
      }
      revisionNotes.push(`修改了 ${todosModified} 个 Todo`);
    }

    // 3. 添加新的 Todo
    if (feedback.todosToAdd && feedback.todosToAdd.length > 0) {
      for (const params of feedback.todosToAdd) {
        const newTodo = this.createTodo({
          ...params,
          assignmentId: assignment.id,
        });

        // 检查范围
        const scopeCheck = this.checkScope(newTodo, assignment.scope);
        if (scopeCheck.outOfScope) {
          newTodo.outOfScope = true;
          newTodo.approvalStatus = 'pending';
          newTodo.approvalNote = scopeCheck.reason;
          outOfScopeTodos.push(newTodo);
          warnings.push(`新增 Todo "${newTodo.content}" 超出职责范围: ${scopeCheck.reason}`);
        }

        todos.push(newTodo);
        todosAdded++;
      }
      revisionNotes.push(`添加了 ${todosAdded} 个 Todo`);
    }

    // 4. 重新建立依赖关系
    this.establishDependencies(todos);

    // 5. 添加审查意见
    if (feedback.comments) {
      revisionNotes.push(`审查意见: ${feedback.comments}`);
    }

    return {
      revisedPlan: {
        todos,
        outOfScopeTodos,
        warnings,
      },
      todosAdded,
      todosRemoved,
      todosModified,
      revisionNotes,
    };
  }

  /**
   * 批准规划
   *
   * 标记所有 Todo 为已批准状态
   */
  approvePlan(plan: PlanningResult): PlanningResult {
    const approvedTodos = plan.todos.map(todo => ({
      ...todo,
      approvalStatus: 'approved' as const,
    }));

    return {
      ...plan,
      todos: approvedTodos,
    };
  }

  /**
   * 拒绝规划
   *
   * 标记规划为已拒绝，返回拒绝原因
   */
  rejectPlan(plan: PlanningResult, reason: string): PlanningResult {
    const rejectedTodos = plan.todos.map(todo => ({
      ...todo,
      approvalStatus: 'rejected' as const,
      approvalNote: reason,
      status: 'skipped' as TodoStatus,
    }));

    return {
      ...plan,
      todos: rejectedTodos,
      warnings: [...plan.warnings, `规划被拒绝: ${reason}`],
    };
  }

  /**
   * 验证规划完整性
   *
   * 检查规划是否符合基本要求
   */
  validatePlan(plan: PlanningResult, assignment: Assignment): {
    valid: boolean;
    issues: string[];
  } {
    const issues: string[] = [];

    // 1. 检查是否有 Todo
    if (plan.todos.length === 0) {
      issues.push('规划中没有任何 Todo');
    }

    // 2. 检查是否覆盖核心职责
    const hasImplementation = plan.todos.some(t => t.type === 'implementation');
    if (!hasImplementation) {
      issues.push('规划中没有实现类型的 Todo');
    }

    // 3. 检查是否有验证步骤
    const hasVerification = plan.todos.some(t => t.type === 'verification');
    if (!hasVerification) {
      issues.push('规划中没有验证类型的 Todo');
    }

    // 4. 检查依赖关系是否有效
    const todoIds = new Set(plan.todos.map(t => t.id));
    for (const todo of plan.todos) {
      for (const depId of todo.dependsOn) {
        if (!todoIds.has(depId)) {
          issues.push(`Todo "${todo.content}" 依赖了不存在的 Todo: ${depId}`);
        }
      }
    }

    // 5. 检查循环依赖
    const cyclicDeps = this.detectCyclicDependencies(plan.todos);
    if (cyclicDeps.length > 0) {
      issues.push(`存在循环依赖: ${cyclicDeps.join(' -> ')}`);
    }

    // 6. 检查契约覆盖
    if (assignment.producerContracts.length > 0) {
      const producedContracts = new Set(
        plan.todos.flatMap(t => t.producesContracts)
      );
      for (const contract of assignment.producerContracts) {
        if (!producedContracts.has(contract)) {
          issues.push(`需要生产的契约 "${contract}" 未被任何 Todo 覆盖`);
        }
      }
    }

    return {
      valid: issues.length === 0,
      issues,
    };
  }

  /**
   * 检测循环依赖
   */
  private detectCyclicDependencies(todos: WorkerTodo[]): string[] {
    const todoMap = new Map(todos.map(t => [t.id, t]));
    const visited = new Set<string>();
    const recursionStack = new Set<string>();
    const cyclePath: string[] = [];

    const dfs = (todoId: string): boolean => {
      visited.add(todoId);
      recursionStack.add(todoId);

      const todo = todoMap.get(todoId);
      if (!todo) return false;

      for (const depId of todo.dependsOn) {
        if (!visited.has(depId)) {
          if (dfs(depId)) {
            cyclePath.unshift(todo.content);
            return true;
          }
        } else if (recursionStack.has(depId)) {
          const depTodo = todoMap.get(depId);
          cyclePath.unshift(todo.content);
          cyclePath.unshift(depTodo?.content || depId);
          return true;
        }
      }

      recursionStack.delete(todoId);
      return false;
    };

    for (const todo of todos) {
      if (!visited.has(todo.id)) {
        if (dfs(todo.id)) {
          return cyclePath;
        }
      }
    }

    return [];
  }
}
