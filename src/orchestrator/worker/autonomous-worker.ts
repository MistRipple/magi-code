/**
 * Autonomous Worker - 自主 Worker
 *
 * 核心功能：
 * - 包装 BaseWorker，增加自主规划能力
 * - 执行 Assignment 而非 SubTask
 * - 支持动态 Todo 添加
 * - 自动生成自检和互检引导
 * - 通过 IAdapterFactory 执行实际命令
 */

import { EventEmitter } from 'events';
import { CLIType, SubTask } from '../../types';
import { BaseWorker } from '../../workers/base-worker';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { AdapterOutputScope } from '../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { TodoPlanner, PlanningContext, PlanningResult } from './todo-planner';
import {
  ProfileAwareRecoveryHandler,
  RecoveryDecision,
  FailureAnalysis,
} from '../recovery/profile-aware-recovery-handler';
import {
  Assignment,
  WorkerTodo,
  TodoOutput,
  TodoStatus,
} from '../mission/types';

/**
 * Todo 执行选项
 */
export interface TodoExecuteOptions {
  /** 工作目录 */
  workingDirectory: string;
  /** 超时时间 */
  timeout?: number;
  /** 输出回调 */
  onOutput?: (output: string) => void;
  /** 项目上下文 */
  projectContext?: string;
  /** 适配器工厂（可选，用于实际执行） */
  adapterFactory?: IAdapterFactory;
  /** 适配器输出范围选项 */
  adapterScope?: AdapterOutputScope;
}

/**
 * 自主执行结果
 */
export interface AutonomousExecutionResult {
  assignment: Assignment;
  success: boolean;
  completedTodos: WorkerTodo[];
  failedTodos: WorkerTodo[];
  skippedTodos: WorkerTodo[];
  dynamicTodos: WorkerTodo[];
  recoveredTodos: WorkerTodo[];
  totalDuration: number;
  errors: string[];
  recoveryAttempts: number;
  /** Token 使用统计 */
  tokenUsage?: TokenUsage;
}

/**
 * AutonomousWorker - 自主 Worker
 */
export class AutonomousWorker extends EventEmitter {
  private todoPlanner: TodoPlanner;
  private recoveryHandler: ProfileAwareRecoveryHandler;
  private retryCountMap: Map<string, number> = new Map();

  constructor(
    private cliType: CLIType,
    private baseWorker: BaseWorker,
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector
  ) {
    super();
    this.todoPlanner = new TodoPlanner(profileLoader, guidanceInjector);
    this.recoveryHandler = new ProfileAwareRecoveryHandler(profileLoader);
  }

  /**
   * 获取 CLI 类型
   */
  getCliType(): CLIType {
    return this.cliType;
  }

  /**
   * 规划 Assignment 的 Todo 列表
   */
  async planAssignment(
    assignment: Assignment,
    options?: {
      projectContext?: string;
      contextSnapshot?: string;
    }
  ): Promise<PlanningResult> {
    const context: PlanningContext = {
      responsibility: assignment.responsibility,
      scope: assignment.scope,
      availableContracts: [
        ...assignment.producerContracts,
        ...assignment.consumerContracts,
      ],
      projectContext: options?.projectContext,
      contextSnapshot: options?.contextSnapshot,
    };

    const result = await this.todoPlanner.planTodos(assignment, context);

    this.emit('planningCompleted', {
      assignmentId: assignment.id,
      todos: result.todos,
      outOfScopeTodos: result.outOfScopeTodos,
      warnings: result.warnings,
    });

    return result;
  }

  /**
   * 执行整个 Assignment
   */
  async executeAssignment(
    assignment: Assignment,
    options: TodoExecuteOptions
  ): Promise<AutonomousExecutionResult> {
    const startTime = Date.now();
    const completedTodos: WorkerTodo[] = [];
    const failedTodos: WorkerTodo[] = [];
    const skippedTodos: WorkerTodo[] = [];
    const dynamicTodos: WorkerTodo[] = [];
    const errors: string[] = [];
    // 聚合 Token 使用统计
    let totalTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };

    this.emit('assignmentStarted', { assignmentId: assignment.id });

    // 执行每个 Todo
    let currentTodo = this.getNextExecutableTodo(assignment);

    while (currentTodo) {
      try {
        const result = await this.executeTodo(currentTodo, assignment, options);

        // 聚合 Token 统计
        if (result.tokenUsage) {
          totalTokenUsage.inputTokens += result.tokenUsage.inputTokens || 0;
          totalTokenUsage.outputTokens += result.tokenUsage.outputTokens || 0;
          if (result.tokenUsage.cacheReadTokens) {
            totalTokenUsage.cacheReadTokens = (totalTokenUsage.cacheReadTokens || 0) + result.tokenUsage.cacheReadTokens;
          }
          if (result.tokenUsage.cacheWriteTokens) {
            totalTokenUsage.cacheWriteTokens = (totalTokenUsage.cacheWriteTokens || 0) + result.tokenUsage.cacheWriteTokens;
          }
        }

        if (result.success) {
          completedTodos.push(result.todo);

          // 检查是否需要动态添加 Todo
          if (result.dynamicTodos && result.dynamicTodos.length > 0) {
            dynamicTodos.push(...result.dynamicTodos);
            // 将动态 Todo 添加到 assignment
            for (const todo of result.dynamicTodos) {
              assignment.todos.push(todo);
            }
          }
        } else {
          failedTodos.push(result.todo);
          errors.push(result.error || 'Unknown error');

          // 检查是否应该跳过依赖的 Todo
          const dependentTodos = this.getDependentTodos(currentTodo, assignment);
          for (const depTodo of dependentTodos) {
            depTodo.status = 'skipped';
            depTodo.blockedReason = `依赖的 Todo "${currentTodo.content}" 失败`;
            skippedTodos.push(depTodo);
          }
        }
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        errors.push(errorMessage);
        currentTodo.status = 'failed';
        currentTodo.output = {
          success: false,
          summary: '',
          modifiedFiles: [],
          error: errorMessage,
          duration: 0,
        };
        failedTodos.push(currentTodo);
      }

      // 获取下一个可执行的 Todo
      currentTodo = this.getNextExecutableTodo(assignment);
    }

    // 收集被跳过的 Todo
    for (const todo of assignment.todos) {
      if (todo.status === 'skipped' && !skippedTodos.includes(todo)) {
        skippedTodos.push(todo);
      }
    }

    const result: AutonomousExecutionResult = {
      assignment,
      success: failedTodos.length === 0,
      completedTodos,
      failedTodos,
      skippedTodos,
      dynamicTodos,
      recoveredTodos: [],
      totalDuration: Date.now() - startTime,
      errors,
      recoveryAttempts: 0,
      tokenUsage: totalTokenUsage,
    };

    this.emit('assignmentCompleted', result);

    return result;
  }

  /**
   * 执行单个 Todo
   */
  async executeTodo(
    todo: WorkerTodo,
    assignment: Assignment,
    options: TodoExecuteOptions
  ): Promise<{
    success: boolean;
    todo: WorkerTodo;
    error?: string;
    dynamicTodos?: WorkerTodo[];
    tokenUsage?: TokenUsage;
  }> {
    const startTime = Date.now();

    // 检查超范围审批
    if (todo.outOfScope && todo.approvalStatus !== 'approved') {
      return {
        success: false,
        todo,
        error: '超范围 Todo 未获得审批',
      };
    }

    // 更新状态
    todo.status = 'in_progress';
    todo.startedAt = Date.now();

    this.emit('todoStarted', {
      assignmentId: assignment.id,
      todoId: todo.id,
      content: todo.content,
    });

    try {
      // 构建执行 prompt
      const profile = this.profileLoader.getProfile(this.cliType);
      const executionPrompt = this.buildExecutionPrompt(todo, assignment, options.projectContext);

      // 生成自检引导
      const selfCheckGuidance = this.guidanceInjector.buildSelfCheckGuidance(
        profile,
        todo.content
      );

      // 执行（通过 executeWithWorker 调用 CLI 适配器）
      const output = await this.executeWithWorker(
        todo,
        assignment,
        executionPrompt,
        selfCheckGuidance,
        options
      );

      // 更新 Todo 状态
      todo.status = 'completed';
      todo.completedAt = Date.now();
      todo.output = {
        success: true,
        summary: output.summary,
        modifiedFiles: output.modifiedFiles || [],
        duration: Date.now() - startTime,
      };

      this.emit('todoCompleted', {
        assignmentId: assignment.id,
        todoId: todo.id,
        output: todo.output,
      });

      return {
        success: true,
        todo,
        dynamicTodos: output.dynamicTodos,
        tokenUsage: output.tokenUsage,
      };
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);

      todo.status = 'failed';
      todo.completedAt = Date.now();
      todo.output = {
        success: false,
        summary: '',
        modifiedFiles: [],
        error: errorMessage,
        duration: Date.now() - startTime,
      };

      this.emit('todoFailed', {
        assignmentId: assignment.id,
        todoId: todo.id,
        error: errorMessage,
      });

      return {
        success: false,
        todo,
        error: errorMessage,
      };
    }
  }

  /**
   * 构建执行 Prompt
   */
  private buildExecutionPrompt(
    todo: WorkerTodo,
    assignment: Assignment,
    projectContext?: string
  ): string {
    const sections: string[] = [];

    // 1. 职责上下文
    sections.push(`## 职责分配\n${assignment.responsibility}`);

    // 2. 当前 Todo
    sections.push(`## 当前任务\n${todo.content}`);
    sections.push(`**原因**: ${todo.reasoning}`);
    sections.push(`**预期产出**: ${todo.expectedOutput}`);

    // 3. 职责范围提醒
    if (assignment.scope.excludes.length > 0) {
      sections.push(`## 注意：以下内容不在你的职责范围内\n${assignment.scope.excludes.map(e => `- ${e}`).join('\n')}`);
    }

    // 4. 契约信息
    if (todo.requiredContracts.length > 0) {
      sections.push(`## 依赖的契约\n${todo.requiredContracts.map(c => `- ${c}`).join('\n')}`);
    }

    // 5. 引导 Prompt
    if (assignment.guidancePrompt) {
      sections.push(`## 角色引导\n${assignment.guidancePrompt}`);
    }

    // 6. 项目上下文
    if (projectContext) {
      sections.push(`## 项目上下文\n${projectContext}`);
    }

    return sections.join('\n\n');
  }

  /**
   * 使用底层 Worker 执行
   *
   * 支持两种执行模式：
   * 1. 通过 CLIAdapterFactory（推荐）：使用适配器工厂发送消息
   * 2. 通过 BaseWorker（传统）：使用底层 Worker 执行
   */
  private async executeWithWorker(
    todo: WorkerTodo,
    assignment: Assignment,
    executionPrompt: string,
    selfCheckGuidance: string,
    options: TodoExecuteOptions
  ): Promise<{
    summary: string;
    modifiedFiles?: string[];
    dynamicTodos?: WorkerTodo[];
    tokenUsage?: TokenUsage;
  }> {
    // 组合执行 prompt 和自检引导
    const fullPrompt = `${executionPrompt}\n\n## 自检要点\n${selfCheckGuidance}`;

    // 模式 1: 通过 CLIAdapterFactory 执行
    if (options.adapterFactory) {
      try {
        const response = await options.adapterFactory.sendMessage(
          this.cliType,
          fullPrompt,
          undefined, // 无图片
          {
            source: 'worker',
            streamToUI: true,
            adapterRole: 'worker',
            ...options.adapterScope,
          }
        );

        if (response.error) {
          throw new Error(response.error);
        }

        // 解析响应
        const summary = response.content || response.error || '执行完成';
        const modifiedFiles = this.extractModifiedFiles(response.content || '');
        const dynamicTodos = this.extractDynamicTodos(response.content || '');

        // 调用输出回调
        if (options.onOutput && response.content) {
          options.onOutput(response.content);
        }

        return {
          summary,
          modifiedFiles,
          dynamicTodos,
          tokenUsage: response.tokenUsage,
        };
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        throw new Error(`CLI 执行失败: ${errorMessage}`);
      }
    }

    // 模式 2: 使用 BaseWorker 执行（真实 CLI）
    const inferredTargets = this.extractTargetFiles(fullPrompt);
    const targetFiles = [...new Set([...(assignment.scope?.targetPaths || []), ...inferredTargets])];
    const subTask: SubTask = {
      id: todo.id,
      taskId: assignment.missionId,
      description: fullPrompt,
      assignmentId: assignment.id,
      assignedWorker: this.cliType,
      reason: todo.reasoning,
      prompt: fullPrompt,
      targetFiles,
      dependencies: todo.dependsOn || [],
      priority: todo.priority || 5,
      kind: this.mapTodoKind(todo.type),
      status: 'pending',
      progress: 0,
      retryCount: 0,
      maxRetries: 3,
      output: [],
    };

    const result = await this.baseWorker.execute({
      subTask,
      workingDirectory: options.workingDirectory,
      timeout: options.timeout,
      onOutput: options.onOutput,
    });

    if (!result.success) {
      throw new Error(result.error || 'CLI 执行失败');
    }

    const outputText = result.output || '';
    const modifiedFiles = result.modifiedFiles || this.extractModifiedFiles(outputText);
    const dynamicTodos = this.extractDynamicTodos(outputText);

    return {
      summary: outputText || result.error || '执行完成',
      modifiedFiles,
      dynamicTodos,
    };
  }

  /**
   * 从输出中提取修改的文件列表
   */
  private extractModifiedFiles(output: string): string[] {
    const files: string[] = [];

    // 匹配常见的文件修改模式
    const patterns = [
      /(?:Created|Modified|Updated|Wrote|Edited):\s*([^\n]+)/gi,
      /(?:创建|修改|更新|写入|编辑)[了]?[：:]\s*([^\n]+)/gi,
      /✓\s+([^\s]+\.[a-z]+)/gi,
    ];

    for (const pattern of patterns) {
      let match;
      while ((match = pattern.exec(output)) !== null) {
        const file = match[1].trim();
        if (file && !files.includes(file)) {
          files.push(file);
        }
      }
    }

    return files;
  }

  private extractTargetFiles(text: string): string[] {
    const filePattern = /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|scss|html|json|md|yaml|yml|txt)/gi;
    const matches = text.match(filePattern);
    return matches ? [...new Set(matches)] : [];
  }

  /**
   * 从输出中提取动态 Todo
   */
  private extractDynamicTodos(output: string): WorkerTodo[] {
    // 检测输出中是否有需要动态添加的任务
    // 这是一个简化实现，实际可能需要更复杂的解析
    const todos: WorkerTodo[] = [];

    // 匹配 "TODO:" 或 "需要额外处理:" 等模式
    const todoPattern = /(?:TODO|需要额外处理|Additional task)[：:]?\s*(.+)/gi;
    let match;

    while ((match = todoPattern.exec(output)) !== null) {
      const content = match[1].trim();
      if (content) {
        // 创建动态 Todo（需要审批）
        todos.push({
          id: `dynamic-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`,
          assignmentId: '',
          content,
          reasoning: '执行过程中发现的额外任务',
          expectedOutput: '完成额外任务',
          type: 'implementation',
          priority: 3, // 中等优先级
          status: 'pending',
          dependsOn: [],
          requiredContracts: [],
          producesContracts: [],
          outOfScope: true,
          approvalStatus: 'pending',
          createdAt: Date.now(),
        });
      }
    }

    return todos;
  }

  private mapTodoKind(todoType: string): SubTask['kind'] {
    switch (todoType) {
      case 'integration':
        return 'integration';
      case 'repair':
        return 'repair';
      case 'architecture':
        return 'architecture';
      case 'batch':
        return 'batch';
      case 'background':
        return 'background';
      default:
        return 'implementation';
    }
  }

  /**
   * 获取下一个可执行的 Todo
   */
  private getNextExecutableTodo(assignment: Assignment): WorkerTodo | null {
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

  /**
   * 获取依赖于指定 Todo 的所有 Todo
   */
  private getDependentTodos(todo: WorkerTodo, assignment: Assignment): WorkerTodo[] {
    return assignment.todos.filter(t =>
      t.dependsOn.includes(todo.id) && t.status === 'pending'
    );
  }

  /**
   * 动态添加 Todo
   */
  addDynamicTodo(
    assignment: Assignment,
    content: string,
    reasoning: string,
    type: WorkerTodo['type']
  ): WorkerTodo {
    const todo = this.todoPlanner.addDynamicTodo(assignment, {
      assignmentId: assignment.id,
      content,
      reasoning,
      type,
    });

    this.emit('dynamicTodoAdded', {
      assignmentId: assignment.id,
      todo,
    });

    return todo;
  }

  /**
   * 请求审批超范围 Todo
   */
  requestApproval(todo: WorkerTodo, reason: string): void {
    this.emit('approvalRequested', {
      todoId: todo.id,
      content: todo.content,
      reason,
    });
  }

  /**
   * 批准超范围 Todo
   */
  approveTodo(todo: WorkerTodo, note?: string): void {
    todo.approvalStatus = 'approved';
    todo.approvalNote = note;

    this.emit('todoApproved', {
      todoId: todo.id,
      note,
    });
  }

  /**
   * 拒绝超范围 Todo
   */
  rejectTodo(todo: WorkerTodo, reason: string): void {
    todo.approvalStatus = 'rejected';
    todo.approvalNote = reason;
    todo.status = 'skipped';

    this.emit('todoRejected', {
      todoId: todo.id,
      reason,
    });
  }

  /**
   * 规划失败恢复策略
   *
   * 基于 ProfileAwareRecoveryHandler 分析失败原因并决定恢复策略
   */
  async planRecovery(
    todo: WorkerTodo,
    assignment: Assignment,
    output: TodoOutput
  ): Promise<RecoveryDecision> {
    // 获取重试计数
    const retryCount = this.retryCountMap.get(todo.id) || 0;

    // 分析失败原因
    const failureAnalysis = this.recoveryHandler.analyzeFailure(todo, assignment, output);

    this.emit('failureAnalyzed', {
      todoId: todo.id,
      assignmentId: assignment.id,
      analysis: failureAnalysis,
    });

    // 决定恢复策略
    const decision = this.recoveryHandler.decideRecoveryStrategy(
      todo,
      assignment,
      failureAnalysis,
      retryCount
    );

    this.emit('recoveryDecided', {
      todoId: todo.id,
      assignmentId: assignment.id,
      decision,
    });

    return decision;
  }

  /**
   * 执行恢复策略
   *
   * 根据 RecoveryDecision 执行对应的恢复操作
   */
  async executeRecovery(
    decision: RecoveryDecision,
    todo: WorkerTodo,
    assignment: Assignment,
    options: TodoExecuteOptions
  ): Promise<{
    success: boolean;
    recoveredTodo?: WorkerTodo;
    newAssignment?: Assignment;
  }> {
    this.emit('recoveryStarted', {
      todoId: todo.id,
      strategy: decision.strategy,
    });

    // 更新重试计数
    const currentCount = this.retryCountMap.get(todo.id) || 0;
    this.retryCountMap.set(todo.id, currentCount + 1);

    try {
      const recoveryResult = await this.recoveryHandler.executeRecovery(
        decision,
        todo,
        assignment
      );

      if (recoveryResult.success) {
        // 恢复成功，重新执行
        let targetTodo = todo;
        let targetAssignment = assignment;

        if (recoveryResult.newAssignment) {
          targetAssignment = recoveryResult.newAssignment;
        }
        if (recoveryResult.newTodo) {
          targetTodo = recoveryResult.newTodo;
        }

        // 重置 Todo 状态
        targetTodo.status = 'pending';
        targetTodo.output = undefined;

        // 如果策略是重新执行，尝试执行
        if (decision.strategy === 'retry_same_worker' || decision.strategy === 'simplify_task') {
          const executeResult = await this.executeTodo(
            targetTodo,
            targetAssignment,
            options
          );

          if (executeResult.success) {
            this.emit('recoverySucceeded', {
              todoId: todo.id,
              strategy: decision.strategy,
            });

            return {
              success: true,
              recoveredTodo: executeResult.todo,
              newAssignment: recoveryResult.newAssignment,
            };
          }
        }

        // 如果策略是换 Worker，返回新的 Assignment
        if (decision.strategy === 'switch_worker') {
          this.emit('recoveryNeedsReassignment', {
            todoId: todo.id,
            suggestedWorker: decision.alternativeWorker,
            newAssignment: recoveryResult.newAssignment,
          });

          return {
            success: true,
            newAssignment: recoveryResult.newAssignment,
          };
        }
      }

      // 恢复失败
      this.emit('recoveryFailed', {
        todoId: todo.id,
        strategy: decision.strategy,
        reason: 'Recovery execution failed',
      });

      return { success: false };
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);

      this.emit('recoveryFailed', {
        todoId: todo.id,
        strategy: decision.strategy,
        reason: errorMessage,
      });

      return { success: false };
    }
  }

  /**
   * 执行带恢复的 Assignment
   *
   * 与 executeAssignment 类似，但会自动尝试恢复失败的 Todo
   */
  async executeAssignmentWithRecovery(
    assignment: Assignment,
    options: TodoExecuteOptions,
    maxRecoveryAttempts: number = 3
  ): Promise<AutonomousExecutionResult> {
    const startTime = Date.now();
    const completedTodos: WorkerTodo[] = [];
    const failedTodos: WorkerTodo[] = [];
    const skippedTodos: WorkerTodo[] = [];
    const dynamicTodos: WorkerTodo[] = [];
    const recoveredTodos: WorkerTodo[] = [];
    const errors: string[] = [];
    let recoveryAttempts = 0;

    this.emit('assignmentStarted', { assignmentId: assignment.id });

    let currentTodo = this.getNextExecutableTodo(assignment);

    while (currentTodo) {
      const result = await this.executeTodo(currentTodo, assignment, options);

      if (result.success) {
        completedTodos.push(result.todo);

        if (result.dynamicTodos && result.dynamicTodos.length > 0) {
          dynamicTodos.push(...result.dynamicTodos);
          for (const todo of result.dynamicTodos) {
            assignment.todos.push(todo);
          }
        }
      } else {
        // 尝试恢复
        const todoRetryCount = this.retryCountMap.get(currentTodo.id) || 0;

        if (todoRetryCount < maxRecoveryAttempts && result.todo.output) {
          const decision = await this.planRecovery(
            result.todo,
            assignment,
            result.todo.output
          );

          if (decision.strategy !== 'skip_task' && decision.strategy !== 'request_human_help') {
            recoveryAttempts++;
            const recoveryResult = await this.executeRecovery(
              decision,
              currentTodo,
              assignment,
              options
            );

            if (recoveryResult.success && recoveryResult.recoveredTodo) {
              recoveredTodos.push(recoveryResult.recoveredTodo);
              completedTodos.push(recoveryResult.recoveredTodo);
              currentTodo = this.getNextExecutableTodo(assignment);
              continue;
            }
          }
        }

        // 恢复失败或达到最大重试次数
        failedTodos.push(result.todo);
        errors.push(result.error || 'Unknown error');

        // 跳过依赖的 Todo
        const dependentTodos = this.getDependentTodos(currentTodo, assignment);
        for (const depTodo of dependentTodos) {
          depTodo.status = 'skipped';
          depTodo.blockedReason = `依赖的 Todo "${currentTodo.content}" 失败`;
          skippedTodos.push(depTodo);
        }
      }

      currentTodo = this.getNextExecutableTodo(assignment);
    }

    // 收集被跳过的 Todo
    for (const todo of assignment.todos) {
      if (todo.status === 'skipped' && !skippedTodos.includes(todo)) {
        skippedTodos.push(todo);
      }
    }

    const finalResult: AutonomousExecutionResult = {
      assignment,
      success: failedTodos.length === 0,
      completedTodos,
      failedTodos,
      skippedTodos,
      dynamicTodos,
      recoveredTodos,
      totalDuration: Date.now() - startTime,
      errors,
      recoveryAttempts,
    };

    this.emit('assignmentCompleted', finalResult);

    return finalResult;
  }

  /**
   * 获取失败分析
   */
  getFailureAnalysis(
    todo: WorkerTodo,
    assignment: Assignment,
    output: TodoOutput
  ): FailureAnalysis {
    return this.recoveryHandler.analyzeFailure(todo, assignment, output);
  }

  /**
   * 清除 Todo 的重试计数
   */
  clearRetryCount(todoId: string): void {
    this.retryCountMap.delete(todoId);
  }

  /**
   * 清除所有重试计数
   */
  clearAllRetryCounts(): void {
    this.retryCountMap.clear();
  }
}
