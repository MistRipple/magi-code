/**
 * Autonomous Worker - 自主 Worker
 *
 * 核心功能：
 * - 执行 Assignment 而非 SubTask
 * - 支持动态 Todo 添加
 * - 自动生成自检和互检引导
 * - 通过 IAdapterFactory 执行实际命令
 * - **汇报机制**: 每完成一个 Todo 向编排者汇报
 */

import { EventEmitter } from 'events';
import { WorkerSlot, SubTask } from '../../types';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { AdapterOutputScope } from '../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { TodoManager, UnifiedTodo, TodoPlanningContext, TodoPlanningResult, TodoOutput } from '../../todo';
import {
  ProfileAwareRecoveryHandler,
  RecoveryDecision,
  FailureAnalysis,
} from '../recovery/profile-aware-recovery-handler';
import {
  Assignment,
} from '../mission/types';
import {
  WorkerReport,
  OrchestratorResponse,
  OrchestratorAdjustment,
  ReportOptions,
  ReportCallback,
  createProgressReport,
  createCompletedReport,
  createFailedReport,
  createQuestionReport,
  WorkerProgress,
  WorkerResult,
} from '../protocols/worker-report';
import {
  WorkerSessionManager,
  WorkerSession,
  ConversationMessage,
} from './worker-session';
import { logger, LogCategory } from '../../logging';

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
  /** 汇报回调（Worker → Orchestrator） */
  onReport?: ReportCallback;
  /** 汇报超时(ms)，默认 5000 */
  reportTimeout?: number;
  /** Session ID（用于恢复执行） - 提案 4.1 */
  sessionId?: string;
  /** 恢复时的额外指令 - 提案 4.1 */
  resumePrompt?: string;
  /** Session 管理器（可选，不提供则创建临时管理器） - 提案 4.1 */
  sessionManager?: WorkerSessionManager;
}

/**
 * 自主执行结果
 */
export interface AutonomousExecutionResult {
  assignment: Assignment;
  success: boolean;
  completedTodos: UnifiedTodo[];
  failedTodos: UnifiedTodo[];
  skippedTodos: UnifiedTodo[];
  dynamicTodos: UnifiedTodo[];
  recoveredTodos: UnifiedTodo[];
  totalDuration: number;
  errors: string[];
  recoveryAttempts: number;
  /** Token 使用统计 */
  tokenUsage?: TokenUsage;
  /** Session ID（用于后续恢复） - 提案 4.1 */
  sessionId?: string;
}

/**
 * AutonomousWorker - 自主 Worker
 */
export class AutonomousWorker extends EventEmitter {
  private todoManager: TodoManager;
  private recoveryHandler: ProfileAwareRecoveryHandler;
  private retryCountMap: Map<string, number> = new Map();
  /** Session 管理器 - 提案 4.1 */
  private sessionManager: WorkerSessionManager;
  /** 当前活跃的 Session */
  private currentSession: WorkerSession | null = null;

  constructor(
    private workerType: WorkerSlot,
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector,
    todoManager: TodoManager,
    sessionManager?: WorkerSessionManager
  ) {
    super();
    this.todoManager = todoManager;
    this.recoveryHandler = new ProfileAwareRecoveryHandler(profileLoader);
    this.sessionManager = sessionManager || new WorkerSessionManager({ autoCleanup: true });
  }

  /**
   * 获取 Worker 类型
   */
  getWorkerType(): WorkerSlot {
    return this.workerType;
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
  ): Promise<TodoPlanningResult> {
    const context: TodoPlanningContext = {
      missionId: assignment.missionId,
      assignmentId: assignment.id,
      responsibility: assignment.responsibility,
      scope: assignment.scope,
      availableContracts: [
        ...assignment.producerContracts,
        ...assignment.consumerContracts,
      ],
      workerId: assignment.workerId,
      projectContext: options?.projectContext,
    };

    const result = await this.todoManager.planTodos(context);

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
    const completedTodos: UnifiedTodo[] = [];
    const failedTodos: UnifiedTodo[] = [];
    const skippedTodos: UnifiedTodo[] = [];
    const dynamicTodos: UnifiedTodo[] = [];
    const errors: string[] = [];
    // 聚合 Token 使用统计
    let totalTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };
    // 是否被编排者中止
    let aborted = false;
    let abortReason: string | undefined;

    // Session 管理 - 提案 4.1
    const sessionMgr = options.sessionManager || this.sessionManager;
    let session: WorkerSession | null = null;
    let isResuming = false;

    // 尝试恢复或创建 Session
    if (options.sessionId) {
      session = sessionMgr.get(options.sessionId);
      if (session) {
        isResuming = true;
        sessionMgr.markAsResumed(options.sessionId, options.resumePrompt);
        logger.info('Worker.Session.恢复', {
          sessionId: options.sessionId,
          assignmentId: assignment.id,
          completedTodos: session.completedTodos.length,
        }, LogCategory.ORCHESTRATOR);

        // 恢复已完成的 Todo 状态
        for (const todoId of session.completedTodos) {
          const todo = assignment.todos.find(t => t.id === todoId);
          if (todo && todo.status === 'pending') {
            todo.status = 'completed';
            completedTodos.push(todo);
          }
        }

        this.emit('sessionResumed', {
          sessionId: session.id,
          assignmentId: assignment.id,
          completedTodos: session.completedTodos.length,
        });
      } else {
        logger.warn('Worker.Session.恢复失败.不存在', {
          sessionId: options.sessionId,
          assignmentId: assignment.id,
        }, LogCategory.ORCHESTRATOR);
      }
    }

    // 如果没有恢复到 Session，创建新的
    if (!session) {
      session = sessionMgr.create({
        assignmentId: assignment.id,
        workerId: this.workerType,
        initialContext: options.projectContext,
      });
      logger.info('Worker.Session.创建', {
        sessionId: session.id,
        assignmentId: assignment.id,
      }, LogCategory.ORCHESTRATOR);

      this.emit('sessionCreated', {
        sessionId: session.id,
        assignmentId: assignment.id,
      });
    }

    this.currentSession = session;

    this.emit('assignmentStarted', {
      assignmentId: assignment.id,
      sessionId: session.id,
      isResuming,
    });
    logger.info('Worker.Assignment.开始', {
      assignmentId: assignment.id,
      workerId: this.workerType,
      sessionId: session.id,
      isResuming,
    }, LogCategory.ORCHESTRATOR);

    // 执行每个 Todo
    let currentTodo = this.getNextExecutableTodo(assignment);

    while (currentTodo && !aborted) {
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

          // 更新 Session - 提案 4.1
          if (session) {
            sessionMgr.update(session.id, {
              completeTodo: result.todo.id,
              stateSnapshot: {
                currentTodoIndex: completedTodos.length,
                lastExecutionAt: Date.now(),
              },
            });
          }

          // **汇报机制**: 每完成一个 Todo 向编排者汇报
          if (options.onReport) {
            const orchestratorResponse = await this.reportProgress(
              assignment,
              currentTodo,
              completedTodos,
              options
            );

            // 处理编排者响应
            if (orchestratorResponse.action === 'abort') {
              aborted = true;
              abortReason = orchestratorResponse.abortReason || '编排者终止执行';
              logger.info('Worker.Assignment.被编排者终止', { assignmentId: assignment.id, reason: abortReason }, LogCategory.ORCHESTRATOR);
              break;
            } else if (orchestratorResponse.action === 'adjust' && orchestratorResponse.adjustment) {
              // 处理调整指令
              await this.handleAdjustment(assignment, orchestratorResponse.adjustment);
            }
          }

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

          // 更新 Session 失败状态 - 提案 4.1
          if (session) {
            sessionMgr.update(session.id, {
              stateSnapshot: {
                lastError: result.error,
                lastExecutionAt: Date.now(),
              },
            });
          }

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
        logger.error('Worker.Todo.执行异常', { todoId: currentTodo.id, error: errorMessage }, LogCategory.ORCHESTRATOR);
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

    // 如果被中止，将剩余的 pending Todo 标记为 skipped
    if (aborted) {
      for (const todo of assignment.todos) {
        if (todo.status === 'pending' && !skippedTodos.includes(todo)) {
          await this.todoManager.skip(todo.id);
          todo.status = 'skipped';
          todo.blockedReason = abortReason || '被编排者终止';
          skippedTodos.push(todo);
        }
      }
    }

    const success = failedTodos.length === 0 && !aborted;
    const result: AutonomousExecutionResult = {
      assignment,
      success,
      completedTodos,
      failedTodos,
      skippedTodos,
      dynamicTodos,
      recoveredTodos: [],
      totalDuration: Date.now() - startTime,
      errors: aborted ? [...errors, abortReason || '被编排者终止'] : errors,
      recoveryAttempts: session?.stateSnapshot.retryCount || 0,
      tokenUsage: totalTokenUsage,
      sessionId: session?.id, // 提案 4.1: 返回 sessionId 以便后续恢复
    };

    // 清理当前 Session 引用
    this.currentSession = null;

    // 如果成功，可选择删除 Session；如果失败，保留以便恢复
    if (success && session) {
      // 成功时可以选择保留 Session 一段时间，或者立即删除
      // 这里保留 Session，让它自然过期
      logger.debug('Worker.Session.保留', { sessionId: session.id }, LogCategory.ORCHESTRATOR);
    } else if (!success && session) {
      logger.info('Worker.Session.失败保留', {
        sessionId: session.id,
        lastError: errors[0],
      }, LogCategory.ORCHESTRATOR);
    }

    // **最终汇报**: 向编排者汇报最终结果
    if (options.onReport) {
      const finalResult: WorkerResult = {
        success,
        modifiedFiles: completedTodos.flatMap(t => t.output?.modifiedFiles || []),
        createdFiles: [],
        summary: success ? `完成 ${completedTodos.length} 个任务` : `失败 ${failedTodos.length} 个任务`,
        totalDuration: result.totalDuration,
        tokenUsage: totalTokenUsage.inputTokens > 0 ? {
          inputTokens: totalTokenUsage.inputTokens,
          outputTokens: totalTokenUsage.outputTokens,
        } : undefined,
      };

      const finalReport = success
        ? createCompletedReport(this.workerType, assignment.id, finalResult)
        : createFailedReport(this.workerType, assignment.id, errors[0] || '执行失败', finalResult);

      try {
        await options.onReport(finalReport);
      } catch (reportError) {
        logger.error('Worker.最终汇报失败', { error: reportError instanceof Error ? reportError.message : String(reportError) }, LogCategory.ORCHESTRATOR);
      }
    }

    this.emit('assignmentCompleted', result);
    logger.info('Worker.Assignment.完成', {
      assignmentId: assignment.id,
      success,
      completedCount: completedTodos.length,
      failedCount: failedTodos.length,
    }, LogCategory.ORCHESTRATOR);

    return result;
  }

  /**
   * 向编排者汇报进度
   * @private
   */
  private async reportProgress(
    assignment: Assignment,
    completedTodo: UnifiedTodo,
    allCompletedTodos: UnifiedTodo[],
    options: TodoExecuteOptions
  ): Promise<OrchestratorResponse> {
    if (!options.onReport) {
      // 如果没有汇报回调，返回默认继续
      return { action: 'continue', timestamp: Date.now() };
    }

    const pendingTodos = assignment.todos.filter(t => t.status === 'pending');
    const completedSteps = allCompletedTodos.map(t => t.content);
    const remainingSteps = pendingTodos.map(t => t.content);
    const totalSteps = assignment.todos.length;
    const percentage = totalSteps > 0 ? Math.round((allCompletedTodos.length / totalSteps) * 100) : 0;

    const progress: WorkerProgress = {
      currentStep: completedTodo.content,
      currentTodoId: completedTodo.id,
      completedSteps,
      remainingSteps,
      percentage,
      stepDuration: completedTodo.output?.duration || 0,
    };

    const report = createProgressReport(
      this.workerType,
      assignment.id,
      progress
    );

    try {
      const response = await Promise.race([
        options.onReport(report),
        new Promise<OrchestratorResponse>((_, reject) =>
          setTimeout(() => reject(new Error('汇报超时')), options.reportTimeout || 5000)
        ),
      ]);
      return response;
    } catch (error) {
      logger.warn('Worker.汇报异常，继续执行', {
        assignmentId: assignment.id,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      // 汇报失败时默认继续执行
      return { action: 'continue', timestamp: Date.now() };
    }
  }

  /**
   * 处理编排者的调整指令
   * @private
   */
  private async handleAdjustment(
    assignment: Assignment,
    adjustment: OrchestratorAdjustment
  ): Promise<void> {
    logger.info('Worker.处理调整指令', {
      assignmentId: assignment.id,
      hasNewInstructions: !!adjustment.newInstructions,
      skipSteps: adjustment.skipSteps?.length || 0,
      addSteps: adjustment.addSteps?.length || 0,
    }, LogCategory.ORCHESTRATOR);

    // 处理跳过步骤
    if (adjustment.skipSteps && adjustment.skipSteps.length > 0) {
      for (const stepContent of adjustment.skipSteps) {
        const todo = assignment.todos.find(t =>
          t.status === 'pending' && t.content.includes(stepContent)
        );
        if (todo) {
          await this.todoManager.skip(todo.id);
          todo.status = 'skipped';
          todo.blockedReason = '编排者跳过';
          logger.debug('Worker.跳过步骤', { todoId: todo.id, content: stepContent }, LogCategory.ORCHESTRATOR);
        }
      }
    }

    // 处理新增步骤
    if (adjustment.addSteps && adjustment.addSteps.length > 0) {
      for (const stepContent of adjustment.addSteps) {
        const todo: UnifiedTodo = {
          id: `adj-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`,
          missionId: assignment.missionId,
          assignmentId: assignment.id,
          workerId: assignment.workerId,
          content: stepContent,
          reasoning: '编排者添加',
          expectedOutput: '完成任务',
          type: 'implementation',
          priority: 3,
          status: 'pending',
          progress: 0,
          dependsOn: [],
          requiredContracts: [],
          producesContracts: [],
          outOfScope: false,
          retryCount: 0,
          maxRetries: 3,
          createdAt: Date.now(),
        };
        assignment.todos.push(todo);
        logger.debug('Worker.添加步骤', { todoId: todo.id, content: stepContent }, LogCategory.ORCHESTRATOR);
      }
    }

    // 处理优先级调整
    if (adjustment.priorityChanges) {
      for (const [todoId, newPriority] of Object.entries(adjustment.priorityChanges)) {
        const todo = assignment.todos.find(t => t.id === todoId);
        if (todo) {
          todo.priority = newPriority;
          logger.debug('Worker.调整优先级', { todoId, newPriority }, LogCategory.ORCHESTRATOR);
        }
      }
      // 根据优先级重新排序 pending 的 todos
      assignment.todos.sort((a, b) => {
        if (a.status !== 'pending' || b.status !== 'pending') return 0;
        return a.priority - b.priority;
      });
    }

    // 处理新指令（记录日志，实际执行由 Worker 自行理解）
    if (adjustment.newInstructions) {
      logger.info('Worker.收到新指令', {
        assignmentId: assignment.id,
        instructions: adjustment.newInstructions.substring(0, 100),
      }, LogCategory.ORCHESTRATOR);
    }

    this.emit('adjustmentApplied', {
      assignmentId: assignment.id,
      adjustment,
    });
  }

  /**
   * 执行单个 Todo
   */
  async executeTodo(
    todo: UnifiedTodo,
    assignment: Assignment,
    options: TodoExecuteOptions
  ): Promise<{
    success: boolean;
    todo: UnifiedTodo;
    error?: string;
    dynamicTodos?: UnifiedTodo[];
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

    // 更新状态 - 使用 TodoManager
    await this.todoManager.prepareForExecution(todo.id);
    await this.todoManager.start(todo.id);
    // 同步本地对象状态
    todo.status = 'running';
    todo.startedAt = Date.now();

    this.emit('todoStarted', {
      assignmentId: assignment.id,
      todoId: todo.id,
      content: todo.content,
    });

    try {
      // 构建执行 prompt
      const profile = this.profileLoader.getProfile(this.workerType);
      const executionPrompt = this.buildExecutionPrompt(todo, assignment, options.projectContext);

      // 生成自检引导
      const selfCheckGuidance = this.guidanceInjector.buildSelfCheckGuidance(
        profile,
        todo.content
      );

      // 执行（通过 executeWithWorker 调用 LLM 适配器）
      const output = await this.executeWithWorker(
        todo,
        assignment,
        executionPrompt,
        selfCheckGuidance,
        options
      );

      // 更新 Todo 状态 - 使用 TodoManager
      const todoOutput: TodoOutput = {
        success: true,
        summary: output.summary,
        modifiedFiles: output.modifiedFiles || [],
        duration: Date.now() - startTime,
      };
      await this.todoManager.complete(todo.id, todoOutput);
      // 同步本地对象状态
      todo.status = 'completed';
      todo.completedAt = Date.now();
      todo.output = todoOutput;

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

      // 使用 TodoManager 标记失败
      await this.todoManager.fail(todo.id, errorMessage);
      // 同步本地对象状态
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
    todo: UnifiedTodo,
    assignment: Assignment,
    projectContext?: string
  ): string {
    const sections: string[] = [];

    // 1. 任务委托说明（优先使用 AI 生成的自然语言委托）
    if (assignment.delegationBriefing) {
      sections.push(`## 任务委托\n${assignment.delegationBriefing}`);
    } else {
      // 兜底：使用结构化的职责描述
      sections.push(`## 职责分配\n${assignment.responsibility}`);
    }

    // 2. 当前 Todo
    sections.push(`## 当前任务\n${todo.content}`);
    sections.push(`**原因**: ${todo.reasoning}`);
    sections.push(`**预期产出**: ${todo.expectedOutput}`);

    // 3. 职责范围提醒
    if (assignment.scope.excludes.length > 0) {
      sections.push(`## 注意：以下内容不在你的职责范围内\n${assignment.scope.excludes.map(e => `- ${e}`).join('\n')}`);
    }

    // 3.1 目标文件（若有）
    if (assignment.scope.targetPaths && assignment.scope.targetPaths.length > 0) {
      const requirement = assignment.scope.requiresModification
        ? '要求：必须使用 text_editor 修改上述文件并保存结果，禁止使用 search_context。'
        : '要求：仅需读取/分析，不要修改文件。';
      sections.push(`## 目标文件\n${assignment.scope.targetPaths.map(p => `- ${p}`).join('\n')}\n\n${requirement}`);
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
   * 使用 LLM 适配器执行任务
   *
   * 通过 AdapterFactory 发送消息到 LLM API
   */
  private async executeWithWorker(
    todo: UnifiedTodo,
    assignment: Assignment,
    executionPrompt: string,
    selfCheckGuidance: string,
    options: TodoExecuteOptions
  ): Promise<{
    summary: string;
    modifiedFiles?: string[];
    dynamicTodos?: UnifiedTodo[];
    tokenUsage?: TokenUsage;
  }> {
    // 必须提供 adapterFactory
    if (!options.adapterFactory) {
      throw new Error('adapterFactory 是必需的，当前项目仅支持 LLM API 模式');
    }

    // 组合执行 prompt 和自检引导
    const fullPrompt = `${executionPrompt}\n\n## 自检要点\n${selfCheckGuidance}`;

    try {
      const response = await options.adapterFactory.sendMessage(
        this.workerType,
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
      const dynamicTodos = this.extractDynamicTodos(response.content || '', assignment);

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
      throw new Error(`LLM 执行失败: ${errorMessage}`);
    }
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
  private extractDynamicTodos(output: string, assignment: Assignment): UnifiedTodo[] {
    // 检测输出中是否有需要动态添加的任务
    // 这是一个简化实现，实际可能需要更复杂的解析
    const todos: UnifiedTodo[] = [];

    // 匹配 "TODO:" 或 "需要额外处理:" 等模式
    const todoPattern = /(?:TODO|需要额外处理|Additional task)[：:]?\s*(.+)/gi;
    let match;

    while ((match = todoPattern.exec(output)) !== null) {
      const content = match[1].trim();
      if (content) {
        // 创建动态 Todo（需要审批）
        todos.push({
          id: `dynamic-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`,
          missionId: assignment.missionId,
          assignmentId: assignment.id,
          workerId: assignment.workerId,
          content,
          reasoning: '执行过程中发现的额外任务',
          expectedOutput: '完成额外任务',
          type: 'implementation',
          priority: 3, // 中等优先级
          status: 'pending',
          progress: 0,
          dependsOn: [],
          requiredContracts: [],
          producesContracts: [],
          outOfScope: true,
          approvalStatus: 'pending',
          retryCount: 0,
          maxRetries: 3,
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
  private getNextExecutableTodo(assignment: Assignment): UnifiedTodo | null {
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
  private getDependentTodos(todo: UnifiedTodo, assignment: Assignment): UnifiedTodo[] {
    return assignment.todos.filter(t =>
      t.dependsOn.includes(todo.id) && t.status === 'pending'
    );
  }

  /**
   * 动态添加 Todo
   */
  async addDynamicTodo(
    assignment: Assignment,
    content: string,
    reasoning: string,
    type: UnifiedTodo['type']
  ): Promise<UnifiedTodo> {
    const todo = await this.todoManager.create({
      missionId: assignment.missionId,
      assignmentId: assignment.id,
      content,
      reasoning,
      type,
      workerId: assignment.workerId,
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
  requestApproval(todo: UnifiedTodo, reason: string): void {
    this.emit('approvalRequested', {
      todoId: todo.id,
      content: todo.content,
      reason,
    });
  }

  /**
   * 批准超范围 Todo
   */
  approveTodo(todo: UnifiedTodo, note?: string): void {
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
  async rejectTodo(todo: UnifiedTodo, reason: string): Promise<void> {
    todo.approvalStatus = 'rejected';
    todo.approvalNote = reason;
    await this.todoManager.skip(todo.id);
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
    todo: UnifiedTodo,
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
    todo: UnifiedTodo,
    assignment: Assignment,
    options: TodoExecuteOptions
  ): Promise<{
    success: boolean;
    recoveredTodo?: UnifiedTodo;
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
    const completedTodos: UnifiedTodo[] = [];
    const failedTodos: UnifiedTodo[] = [];
    const skippedTodos: UnifiedTodo[] = [];
    const dynamicTodos: UnifiedTodo[] = [];
    const recoveredTodos: UnifiedTodo[] = [];
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
    todo: UnifiedTodo,
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

  // ============================================================================
  // Session 管理方法 - 提案 4.1
  // ============================================================================

  /**
   * 获取当前活跃的 Session
   */
  getCurrentSession(): WorkerSession | null {
    return this.currentSession;
  }

  /**
   * 获取 Session 管理器
   */
  getSessionManager(): WorkerSessionManager {
    return this.sessionManager;
  }

  /**
   * 根据 Assignment ID 获取 Session
   */
  getSessionByAssignment(assignmentId: string): WorkerSession | null {
    return this.sessionManager.getByAssignment(assignmentId);
  }

  /**
   * 清理所有 Session
   */
  clearAllSessions(): void {
    this.sessionManager.clear();
    this.currentSession = null;
  }

  /**
   * 销毁 Worker（清理资源）
   */
  dispose(): void {
    this.sessionManager.dispose();
    this.clearAllRetryCounts();
    this.currentSession = null;
    this.removeAllListeners();
  }
}
