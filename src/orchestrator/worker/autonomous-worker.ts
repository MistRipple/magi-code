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
import * as fs from 'fs/promises';
import * as crypto from 'crypto';
import * as path from 'path';
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
} from './worker-session';
import { logger, LogCategory } from '../../logging';
// 共享上下文与文件摘要缓存模块
import {
  ContextAssembler,
  ISharedContextPool,
  IFileSummaryCache,
  FileSummary,
  ContextSource,
  AssembledContext,
  createSharedContextEntry,
} from '../../context';

/**
 * 文件读取结果
 * 用于 readFileWithCache 方法返回值，标识读取类型和来源
 */
export interface FileReadResult {
  /** 返回类型: 'summary' 表示返回摘要, 'full' 表示返回完整文件内容 */
  type: 'summary' | 'full';
  /** 内容（摘要格式化后的字符串或原始文件内容） */
  content: string;
  /** 是否来自缓存 */
  fromCache: boolean;
}

/**
 * Worker 洞察（用于写入 SharedContextPool）
 */
export interface WorkerInsight {
  /** 洞察内容 */
  content: string;
  /** 标签（用于订阅筛选） */
  tags: string[];
  /** 重要性级别 */
  importance: 'critical' | 'high' | 'medium' | 'low';
}

/**
 * 共享上下文依赖（强制注入）
 */
export interface SharedContextDependencies {
  /** 上下文组装器 - 负责按预算分配组装上下文 */
  contextAssembler: ContextAssembler;
  /** 文件摘要缓存 - 减少重复读取同一文件 */
  fileSummaryCache: IFileSummaryCache;
  /** 共享上下文池 - 跨 Worker 知识共享 */
  sharedContextPool: ISharedContextPool;
}

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
  /** 获取补充指令（在决策点注入） */
  getSupplementaryInstructions?: () => string[];
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
  /** 是否有等待审批的 Todo */
  hasPendingApprovals?: boolean;
  /** 是否为直接执行模式（简单任务无 Todo） */
  directExecution?: boolean;
  /** 直接执行模式的输出 */
  directOutput?: DirectExecutionOutput;
}

/**
 * 直接执行模式的输出（简单任务）
 */
export interface DirectExecutionOutput {
  summary: string;
  modifiedFiles?: string[];
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

  // ============================================================================
  // 共享上下文依赖 - 提案 9.2（强制注入）
  // ============================================================================

  /** 上下文组装器 - 按预算分配组装上下文 */
  private contextAssembler: ContextAssembler;
  /** 文件摘要缓存 - 减少重复读取同一文件 */
  private fileSummaryCache: IFileSummaryCache;
  /** 共享上下文池 - 跨 Worker 知识共享 */
  private sharedContextPool: ISharedContextPool;
  /** 当前 Mission ID（用于写入共享上下文） */
  private currentMissionId?: string;
  /** 当前 Assignment 的缓存读取统计（用于质量门禁） */
  private cacheReadStats: {
    lookups: number;
    cacheHits: number;
    cacheMisses: number;
  } = {
    lookups: 0,
    cacheHits: 0,
    cacheMisses: 0,
  };

  constructor(
    private workerType: WorkerSlot,
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector,
    todoManager: TodoManager,
    sharedContextDeps: SharedContextDependencies,
    sessionManager?: WorkerSessionManager
  ) {
    super();

    // 运行时验证：强制依赖参数（TypeScript 类型在运行时不存在）
    if (!todoManager) {
      throw new Error(`创建 Worker[${workerType}] 失败: todoManager 为必需依赖`);
    }
    if (!sharedContextDeps) {
      throw new Error(`创建 Worker[${workerType}] 失败: sharedContextDeps 为必需依赖`);
    }
    if (!sharedContextDeps.contextAssembler) {
      throw new Error(`创建 Worker[${workerType}] 失败: 缺少 contextAssembler`);
    }
    if (!sharedContextDeps.fileSummaryCache) {
      throw new Error(`创建 Worker[${workerType}] 失败: 缺少 fileSummaryCache`);
    }
    if (!sharedContextDeps.sharedContextPool) {
      throw new Error(`创建 Worker[${workerType}] 失败: 缺少 sharedContextPool`);
    }

    this.todoManager = todoManager;
    this.recoveryHandler = new ProfileAwareRecoveryHandler(profileLoader);
    this.sessionManager = sessionManager || new WorkerSessionManager({ autoCleanup: true });

    // 强制依赖：共享上下文组件（运行时已验证非空）
    this.contextAssembler = sharedContextDeps.contextAssembler;
    this.fileSummaryCache = sharedContextDeps.fileSummaryCache;
    this.sharedContextPool = sharedContextDeps.sharedContextPool;
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

    // 设置当前 Mission ID（用于共享上下文写入） - 提案 9.2
    this.currentMissionId = assignment.missionId;
    this.resetCacheReadStats();

    // 组装共享上下文 - 提案 9.2（强制依赖已保证可用）
    // 从 scope.includes 提取标签，如果没有则使用空数组
    const tags = assignment.scope.includes || [];
    const sharedContext = await this.assembleSharedContext(
      assignment.missionId,
      tags
    );

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

    // ========== 双模式执行：直接执行 vs Todo 循环 ==========
    // 由编排者 LLM 语义理解决定：
    //   - 简单任务：assignment.todos 为空，直接执行
    //   - 复杂任务：assignment.todos 非空，按 Todo 循环执行

    if (!assignment.todos || assignment.todos.length === 0) {
      // ========== 直接执行模式（简单任务） ==========
      logger.info('Worker.直接执行模式', {
        assignmentId: assignment.id,
        workerId: this.workerType,
        responsibility: assignment.responsibility.substring(0, 50),
      }, LogCategory.ORCHESTRATOR);

      const directResult = await this.executeDirectly(assignment, options, sharedContext);

      // 构建结果
      const result: AutonomousExecutionResult = {
        assignment,
        success: directResult.success,
        completedTodos: [],
        failedTodos: [],
        skippedTodos: [],
        dynamicTodos: [],
        recoveredTodos: [],
        totalDuration: Date.now() - startTime,
        errors: directResult.error ? [directResult.error] : [],
        recoveryAttempts: 0,
        tokenUsage: directResult.tokenUsage || totalTokenUsage,
        sessionId: session?.id,
        hasPendingApprovals: false,
        directExecution: true, // 标记为直接执行模式
        directOutput: directResult.output, // 直接执行的输出
      };
      const qualityCheckedResult = this.applyQualityGate(assignment, result, sharedContext, startTime);

      // 清理 Session
      this.currentSession = null;
      this.currentMissionId = undefined;
      this.resetCacheReadStats();
      if (qualityCheckedResult.success && session) {
        logger.debug('Worker.Session.保留', { sessionId: session.id }, LogCategory.ORCHESTRATOR);
      } else if (!qualityCheckedResult.success && session) {
        logger.info('Worker.Session.失败保留', { sessionId: session.id, lastError: qualityCheckedResult.errors[0] }, LogCategory.ORCHESTRATOR);
      }

      this.emit('assignmentCompleted', qualityCheckedResult);

      return qualityCheckedResult;
    }

    // ========== Todo 循环模式（复杂任务） ==========
    // 执行每个 Todo
    let currentTodo = this.getNextExecutableTodo(assignment);

    while (currentTodo && !aborted) {
      try {
        // 传递共享上下文到 Todo 执行 - 提案 9.2
        const result = await this.executeTodo(currentTodo, assignment, options, sharedContext);

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

    // 检查是否有等待审批的 Todo
    const hasPendingApprovals = assignment.todos.some(t => t.status === 'pending' && t.approvalStatus === 'pending');

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
      sessionId: session?.id,
      hasPendingApprovals, // 返回 pendingApproval 状态
    };
    const qualityCheckedResult = this.applyQualityGate(assignment, result, sharedContext, startTime);

    // 清理当前 Session 引用
    this.currentSession = null;
    this.currentMissionId = undefined;
    this.resetCacheReadStats();

    // 如果成功，可选择删除 Session；如果失败，保留以便恢复
    if (qualityCheckedResult.success && session) {
      // 成功时可以选择保留 Session 一段时间，或者立即删除
      // 这里保留 Session，让它自然过期
      logger.debug('Worker.Session.保留', { sessionId: session.id }, LogCategory.ORCHESTRATOR);
    } else if (!qualityCheckedResult.success && session) {
      logger.info('Worker.Session.失败保留', {
        sessionId: session.id,
        lastError: qualityCheckedResult.errors[0],
      }, LogCategory.ORCHESTRATOR);
    }

    // **最终汇报**: 向编排者汇报最终结果
    if (options.onReport) {
      const finalResult: WorkerResult = {
        success: qualityCheckedResult.success,
        modifiedFiles: completedTodos.flatMap(t => t.output?.modifiedFiles || []),
        createdFiles: [],
        summary: qualityCheckedResult.success ? `完成 ${completedTodos.length} 个任务` : `失败 ${failedTodos.length} 个任务`,
        totalDuration: qualityCheckedResult.totalDuration,
        tokenUsage: totalTokenUsage.inputTokens > 0 ? {
          inputTokens: totalTokenUsage.inputTokens,
          outputTokens: totalTokenUsage.outputTokens,
        } : undefined,
      };

      const finalReport = qualityCheckedResult.success
        ? createCompletedReport(this.workerType, assignment.id, finalResult)
        : createFailedReport(this.workerType, assignment.id, qualityCheckedResult.errors[0] || '执行失败', finalResult);

      try {
        await options.onReport(finalReport);
      } catch (reportError) {
        logger.error('Worker.最终汇报失败', { error: reportError instanceof Error ? reportError.message : String(reportError) }, LogCategory.ORCHESTRATOR);
      }
    }

    this.emit('assignmentCompleted', qualityCheckedResult);
    logger.info('Worker.Assignment.完成', {
      assignmentId: assignment.id,
      success: qualityCheckedResult.success,
      completedCount: completedTodos.length,
      failedCount: failedTodos.length,
    }, LogCategory.ORCHESTRATOR);

    return qualityCheckedResult;
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
      this.appendSupplementaryInstructions(assignment, [adjustment.newInstructions]);
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
   * 追加补充指令到 Assignment 引导
   */
  private appendSupplementaryInstructions(assignment: Assignment, instructions: string[]): void {
    const normalized = instructions
      .map(i => i.trim())
      .filter(Boolean);
    if (normalized.length === 0) {
      return;
    }
    const content = `[System] 用户补充指令：\n${normalized.map(i => `- ${i}`).join('\n')}`;
    assignment.guidancePrompt = assignment.guidancePrompt
      ? `${assignment.guidancePrompt}\n\n${content}`
      : content;
  }

  /**
   * 执行单个 Todo
   */
  async executeTodo(
    todo: UnifiedTodo,
    assignment: Assignment,
    options: TodoExecuteOptions,
    sharedContext?: AssembledContext | null
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
      // 构建执行 prompt（注入共享上下文） - 提案 9.2
      const profile = this.profileLoader.getProfile(this.workerType);
      const extraTargets = this.extractTargetFiles(
        `${todo.content}\n${todo.reasoning || ''}\n${todo.expectedOutput || ''}`
      );
      const targetFileContext = await this.buildTargetFileContext(
        assignment,
        options.workingDirectory,
        extraTargets
      );
      const executionPrompt = this.buildExecutionPrompt(
        todo,
        assignment,
        options.projectContext,
        sharedContext,
        targetFileContext
      );

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
      await this.writeInsight(this.buildSuccessInsight(assignment, output.summary, output.modifiedFiles || [], todo));

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
      await this.writeInsight(this.buildFailureInsight(assignment, errorMessage, todo));

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
    projectContext?: string,
    sharedContext?: AssembledContext | null,
    targetFileContext?: string | null
  ): string {
    const sections: string[] = [];

    // 0. 共享上下文注入（如果可用） - 提案 9.2
    const sharedKnowledge = this.buildSharedKnowledgeSection(sharedContext);
    if (sharedKnowledge) {
      sections.push(sharedKnowledge);
    }

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
        ? '任务性质：需要对上述文件进行实际修改并保存。'
        : '任务性质：仅需读取/分析，无需修改文件。';
      sections.push(`## 目标文件\n${assignment.scope.targetPaths.map(p => `- ${p}`).join('\n')}\n\n${requirement}`);
    }

    // 3.2 目标文件摘要（强制缓存前置读取）
    if (targetFileContext) {
      sections.push(`## 目标文件摘要\n${targetFileContext}`);
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
      const decisionHook = options.getSupplementaryInstructions
        ? () => {
            const instructions = options.getSupplementaryInstructions?.() || [];
            if (instructions.length > 0) {
              this.appendSupplementaryInstructions(assignment, instructions);
            }
            return instructions;
          }
        : undefined;

      const response = await options.adapterFactory.sendMessage(
        this.workerType,
        fullPrompt,
        undefined, // 无图片
        {
          source: 'worker',
          adapterRole: 'worker',
          ...options.adapterScope,
          decisionHook,
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
   * 直接执行模式（简单任务，无 Todo）
   *
   * 由编排者 LLM 语义理解决定使用此模式，Worker 直接执行 assignment.responsibility
   */
  private async executeDirectly(
    assignment: Assignment,
    options: TodoExecuteOptions,
    sharedContext?: AssembledContext | null
  ): Promise<{
    success: boolean;
    output?: DirectExecutionOutput;
    error?: string;
    tokenUsage?: TokenUsage;
  }> {
    const startTime = Date.now();

    try {
      // 构建执行 prompt（复用现有逻辑，但不依赖 Todo）
      const directTargets = this.extractTargetFiles(
        `${assignment.responsibility}\n${assignment.delegationBriefing || ''}`
      );
      const targetFileContext = await this.buildTargetFileContext(
        assignment,
        options.workingDirectory,
        directTargets
      );
      const executionPrompt = this.buildDirectExecutionPrompt(
        assignment,
        options.projectContext,
        sharedContext,
        targetFileContext
      );

      // 生成自检引导
      const profile = this.profileLoader.getProfile(this.workerType);
      const selfCheckGuidance = this.guidanceInjector.buildSelfCheckGuidance(
        profile,
        assignment.responsibility
      );

      // 必须提供 adapterFactory
      if (!options.adapterFactory) {
        throw new Error('adapterFactory 是必需的，当前项目仅支持 LLM API 模式');
      }

      // 组合执行 prompt 和自检引导
      const fullPrompt = `${executionPrompt}\n\n## 自检要点\n${selfCheckGuidance}`;

      logger.debug('Worker.直接执行.Prompt', {
        assignmentId: assignment.id,
        promptLength: fullPrompt.length,
      }, LogCategory.ORCHESTRATOR);

      const decisionHook = options.getSupplementaryInstructions
        ? () => {
            const instructions = options.getSupplementaryInstructions?.() || [];
            if (instructions.length > 0) {
              this.appendSupplementaryInstructions(assignment, instructions);
            }
            return instructions;
          }
        : undefined;

      const response = await options.adapterFactory.sendMessage(
        this.workerType,
        fullPrompt,
        undefined, // 无图片
        {
          source: 'worker',
          adapterRole: 'worker',
          ...options.adapterScope,
          decisionHook,
        }
      );

      if (response.error) {
        throw new Error(response.error);
      }

      // 解析响应
      const summary = response.content || '执行完成';
      const modifiedFiles = this.extractModifiedFiles(response.content || '');
      await this.writeInsight(this.buildSuccessInsight(assignment, summary, modifiedFiles));

      // 调用输出回调
      if (options.onOutput && response.content) {
        options.onOutput(response.content);
      }

      logger.info('Worker.直接执行.完成', {
        assignmentId: assignment.id,
        duration: Date.now() - startTime,
        modifiedFilesCount: modifiedFiles.length,
      }, LogCategory.ORCHESTRATOR);

      return {
        success: true,
        output: {
          summary,
          modifiedFiles,
        },
        tokenUsage: response.tokenUsage,
      };
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);

      logger.error('Worker.直接执行.失败', {
        assignmentId: assignment.id,
        error: errorMessage,
        duration: Date.now() - startTime,
      }, LogCategory.ORCHESTRATOR);
      await this.writeInsight(this.buildFailureInsight(assignment, errorMessage));

      return {
        success: false,
        error: errorMessage,
      };
    }
  }

  /**
   * 构建直接执行 Prompt（简单任务，无 Todo）
   */
  private buildDirectExecutionPrompt(
    assignment: Assignment,
    projectContext?: string,
    sharedContext?: AssembledContext | null,
    targetFileContext?: string | null
  ): string {
    const sections: string[] = [];

    const sharedKnowledge = this.buildSharedKnowledgeSection(sharedContext);
    if (sharedKnowledge) {
      sections.push(sharedKnowledge);
    }

    // 1. 任务委托说明（优先使用 AI 生成的自然语言委托）
    if (assignment.delegationBriefing) {
      sections.push(`## 任务\n${assignment.delegationBriefing}`);
    } else {
      // 兜底：使用结构化的职责描述
      sections.push(`## 任务\n${assignment.responsibility}`);
    }

    // 2. 职责范围提醒
    if (assignment.scope.excludes.length > 0) {
      sections.push(`## 注意：以下内容不在你的职责范围内\n${assignment.scope.excludes.map(e => `- ${e}`).join('\n')}`);
    }

    // 3. 目标文件（若有）
    if (assignment.scope.targetPaths && assignment.scope.targetPaths.length > 0) {
      const requirement = assignment.scope.requiresModification
        ? '任务性质：需要对上述文件进行实际修改并保存。'
        : '任务性质：仅需读取/分析，无需修改文件。';
      sections.push(`## 目标文件\n${assignment.scope.targetPaths.map(p => `- ${p}`).join('\n')}\n\n${requirement}`);
    }

    if (targetFileContext) {
      sections.push(`## 目标文件摘要\n${targetFileContext}`);
    }

    // 4. 引导 Prompt
    if (assignment.guidancePrompt) {
      sections.push(`## 角色引导\n${assignment.guidancePrompt}`);
    }

    // 5. 项目上下文
    if (projectContext) {
      sections.push(`## 项目上下文\n${projectContext}`);
    }

    return sections.join('\n\n');
  }

  /**
   * 构建共享知识片段
   */
  private buildSharedKnowledgeSection(sharedContext?: AssembledContext | null): string | null {
    if (!sharedContext || sharedContext.parts.length === 0) {
      return null;
    }

    const sharedSections: string[] = [];

    for (const part of sharedContext.parts) {
      switch (part.type) {
        case 'project_knowledge':
          sharedSections.push(`### 项目知识\n${part.content}`);
          break;
        case 'shared_context':
          sharedSections.push(`### 共享上下文\n${part.content}`);
          break;
        case 'contracts':
          sharedSections.push(`### 任务契约\n${part.content}`);
          break;
        case 'long_term_memory':
          sharedSections.push(`### 历史记忆\n${part.content}`);
          break;
        default:
          break;
      }
    }

    if (sharedSections.length === 0) {
      return null;
    }

    return `## 共享知识\n\n${sharedSections.join('\n\n')}`;
  }

  /**
   * 构建目标文件上下文（强制经缓存读取）
   */
  private async buildTargetFileContext(
    assignment: Assignment,
    workingDirectory: string,
    extraTargets: string[] = []
  ): Promise<string | null> {
    const targetPaths = Array.from(new Set([...(assignment.scope.targetPaths || []), ...extraTargets]));
    if (targetPaths.length === 0) {
      return null;
    }

    const maxFiles = 5;
    const selectedPaths = targetPaths.slice(0, maxFiles);
    const sections: string[] = [];

    for (const targetPath of selectedPaths) {
      const absolutePath = path.isAbsolute(targetPath)
        ? targetPath
        : path.resolve(workingDirectory, targetPath);

      try {
        const fileResult = await this.readFileWithCache(absolutePath);
        const content = fileResult.type === 'summary'
          ? fileResult.content
          : this.formatSummary(this.generateFileSummaryFromContent(fileResult.content, absolutePath));
        const sourceLabel = fileResult.fromCache ? '缓存摘要' : '实时摘要';
        sections.push(`### ${targetPath}\n来源: ${sourceLabel}\n${content}`);
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        logger.warn('Worker.目标文件.读取失败', {
          assignmentId: assignment.id,
          workerId: this.workerType,
          targetPath,
          error: errorMessage,
        }, LogCategory.ORCHESTRATOR);
        sections.push(`### ${targetPath}\n来源: 读取失败\n${errorMessage}`);
      }
    }

    if (targetPaths.length > maxFiles) {
      sections.push(`已按预算裁剪，额外省略 ${targetPaths.length - maxFiles} 个目标文件。`);
    }

    return sections.join('\n\n');
  }

  /**
   * 构建成功洞察
   */
  private buildSuccessInsight(
    assignment: Assignment,
    summary: string,
    modifiedFiles: string[] = [],
    todo?: UnifiedTodo
  ): WorkerInsight {
    const scopeText = todo ? todo.content : assignment.responsibility;
    const summaryText = this.trimInsightContent(summary, 600);
    const fileText = modifiedFiles.length > 0
      ? `涉及文件: ${modifiedFiles.slice(0, 6).join(', ')}`
      : '未检测到明确文件变更。';

    return {
      content: `任务成功: ${scopeText}\n结论: ${summaryText}\n${fileText}`,
      tags: this.buildInsightTags(assignment, modifiedFiles),
      importance: modifiedFiles.length > 0 ? 'high' : 'medium',
    };
  }

  /**
   * 构建失败洞察
   */
  private buildFailureInsight(
    assignment: Assignment,
    errorMessage: string,
    todo?: UnifiedTodo
  ): WorkerInsight {
    const scopeText = todo ? todo.content : assignment.responsibility;
    const errorText = this.trimInsightContent(errorMessage, 400);
    return {
      content: `任务失败: ${scopeText}\n错误: ${errorText}`,
      tags: this.buildInsightTags(assignment),
      importance: 'high',
    };
  }

  /**
   * 生成洞察标签
   */
  private buildInsightTags(assignment: Assignment, files: string[] = []): string[] {
    const tagSet = new Set<string>();

    for (const includeTag of assignment.scope.includes || []) {
      if (includeTag && includeTag.trim()) {
        tagSet.add(includeTag.trim());
      }
    }

    const fileCandidates = [
      ...(assignment.scope.targetPaths || []),
      ...files,
    ];
    for (const filePath of fileCandidates) {
      const tags = this.extractTagsFromFilePath(filePath);
      for (const tag of tags) {
        tagSet.add(tag);
      }
    }

    tagSet.add(this.workerType);
    return Array.from(tagSet).slice(0, 8);
  }

  /**
   * 限制洞察内容长度，避免写入超限
   */
  private trimInsightContent(content: string, maxLength: number): string {
    const normalized = content.replace(/\s+/g, ' ').trim();
    if (normalized.length <= maxLength) {
      return normalized;
    }
    return `${normalized.slice(0, maxLength)}...`;
  }

  /**
   * 重置缓存读取统计
   */
  private resetCacheReadStats(): void {
    this.cacheReadStats = {
      lookups: 0,
      cacheHits: 0,
      cacheMisses: 0,
    };
  }

  /**
   * 执行结果质量门禁
   */
  private applyQualityGate(
    assignment: Assignment,
    result: AutonomousExecutionResult,
    sharedContext?: AssembledContext | null,
    startedAt?: number
  ): AutonomousExecutionResult {
    const gateErrors: string[] = [];

    if (sharedContext && sharedContext.budgetUsage > 1) {
      gateErrors.push('质量门禁失败: 共享上下文预算超限。');
    }

    if (result.success) {
      if (!sharedContext) {
        gateErrors.push('质量门禁失败: 未注入共享上下文。');
      }

      if (!this.hasWorkerSharedFacts(assignment.missionId, startedAt)) {
        gateErrors.push('质量门禁失败: 未写入可复用共享事实。');
      }

      const hasTargetFiles = (assignment.scope.targetPaths?.length || 0) > 0;
      if (hasTargetFiles && this.cacheReadStats.lookups === 0) {
        gateErrors.push('质量门禁失败: 目标文件未经过缓存前置读取。');
      }

      const unknownRequiredContracts = this.collectUnknownRequiredContracts(assignment);
      if (unknownRequiredContracts.length > 0) {
        gateErrors.push(`质量门禁失败: 发现未声明的契约依赖 ${unknownRequiredContracts.join(', ')}`);
      }
    }

    if (gateErrors.length === 0) {
      return result;
    }

    logger.warn('Worker.质量门禁.失败', {
      assignmentId: assignment.id,
      workerId: this.workerType,
      gateErrors,
      cacheReadStats: this.cacheReadStats,
    }, LogCategory.ORCHESTRATOR);

    return {
      ...result,
      success: false,
      errors: [...result.errors, ...gateErrors],
    };
  }

  /**
   * 检查 Worker 是否写入可复用共享事实
   */
  private hasWorkerSharedFacts(missionId: string, startedAt?: number): boolean {
    const source = this.mapWorkerTypeToContextSource();
    const entries = this.sharedContextPool.getByMission(missionId);
    return entries.some(entry => {
      if (startedAt && entry.createdAt < startedAt) {
        return false;
      }
      if (entry.source === source) {
        return true;
      }
      return entry.sources?.includes(source) || false;
    });
  }

  /**
   * 收集未声明的契约依赖
   */
  private collectUnknownRequiredContracts(assignment: Assignment): string[] {
    const declaredContracts = new Set<string>([
      ...assignment.producerContracts,
      ...assignment.consumerContracts,
    ]);
    const unknownContracts = new Set<string>();

    for (const todo of assignment.todos) {
      for (const requiredContract of todo.requiredContracts || []) {
        if (!declaredContracts.has(requiredContract)) {
          unknownContracts.add(requiredContract);
        }
      }
    }

    return Array.from(unknownContracts);
  }

  /**
   * 获取下一个可执行的 Todo
   */
  private getNextExecutableTodo(assignment: Assignment): UnifiedTodo | null {
    for (const todo of assignment.todos) {
      // pending 或 ready 状态的 Todo 都可以执行
      // (TodoManager.create 会根据依赖自动将 pending 转为 ready)
      if (todo.status !== 'pending' && todo.status !== 'ready') continue;

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

        // 如果策略是重新执行，尝试执行（恢复路径同样注入共享上下文）
        if (decision.strategy === 'retry_same_worker' || decision.strategy === 'simplify_task') {
          const recoveryTags = targetAssignment.scope.includes || [];
          const recoverySharedContext = await this.assembleSharedContext(
            targetAssignment.missionId,
            recoveryTags
          );
          const executeResult = await this.executeTodo(
            targetTodo,
            targetAssignment,
            options,
            recoverySharedContext
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

    // 设置当前 Mission ID - 提案 9.2
    this.currentMissionId = assignment.missionId;
    this.resetCacheReadStats();

    // 组装共享上下文（强制依赖已保证可用）
    // 从 scope.includes 提取标签，如果没有则使用空数组
    const tags = assignment.scope.includes || [];
    const sharedContextForRecovery = await this.assembleSharedContext(
      assignment.missionId,
      tags
    );

    this.emit('assignmentStarted', { assignmentId: assignment.id });

    let currentTodo = this.getNextExecutableTodo(assignment);

    while (currentTodo) {
      // 传递共享上下文 - 提案 9.2
      const result = await this.executeTodo(currentTodo, assignment, options, sharedContextForRecovery);

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
    const qualityCheckedResult = this.applyQualityGate(
      assignment,
      finalResult,
      sharedContextForRecovery,
      startTime
    );

    this.currentMissionId = undefined;
    this.resetCacheReadStats();
    this.emit('assignmentCompleted', qualityCheckedResult);

    return qualityCheckedResult;
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
    this.currentMissionId = undefined;
    this.removeAllListeners();
  }

  // ============================================================================
  // 共享上下文与文件摘要缓存方法 - 提案 9.2
  // ============================================================================

  /**
   * 读取文件（优先查缓存）
   *
   * 按照 docs/context/unified-memory-plan.md 9.2 节规范实现：
   * 1. 计算当前文件 hash
   * 2. 查询 FileSummaryCache
   * 3. 缓存命中则返回摘要
   * 4. 缓存未命中则读取原文件，并异步生成摘要
   *
   * @param filePath - 文件绝对路径
   * @returns 文件读取结果
   */
  async readFileWithCache(filePath: string): Promise<FileReadResult> {
    try {
      this.cacheReadStats.lookups++;

      // 1. 读取文件并计算当前 hash（避免重复 I/O）
      const content = await fs.readFile(filePath, 'utf-8');
      const currentHash = this.computeContentHash(content);

      // 2. 查询缓存（强制依赖已保证 fileSummaryCache 可用）
      const cachedSummary = this.fileSummaryCache.get(filePath, currentHash);

      if (cachedSummary) {
        // 缓存命中，返回格式化后的摘要
        this.cacheReadStats.cacheHits++;
        logger.debug('Worker.文件缓存.命中', {
          filePath,
          workerId: this.workerType,
        }, LogCategory.ORCHESTRATOR);

        return {
          type: 'summary',
          content: this.formatSummary(cachedSummary),
          fromCache: true,
        };
      }

      // 3. 缓存未命中，使用已读取的原文件内容
      this.cacheReadStats.cacheMisses++;
      logger.debug('Worker.文件缓存.未命中', {
        filePath,
        workerId: this.workerType,
      }, LogCategory.ORCHESTRATOR);

      // 4. 异步生成并缓存摘要（不阻塞主流程）
      this.generateAndCacheSummary(filePath, currentHash, content).catch(error => {
        logger.warn('Worker.文件摘要.生成失败', {
          filePath,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      });

      return {
        type: 'full',
        content,
        fromCache: false,
      };
    } catch (error) {
      this.cacheReadStats.cacheMisses++;
      const errorMessage = error instanceof Error ? error.message : String(error);
      throw new Error(`读取文件失败: ${filePath}, 错误: ${errorMessage}`);
    }
  }

  /**
   * 生成并缓存摘要
   *
   * 生成结构化文件摘要并写入 FileSummaryCache 和 SharedContextPool
   *
   * @param filePath - 文件路径
   * @param fileHash - 文件内容 hash
   * @param content - 文件内容
   */
  async generateAndCacheSummary(
    filePath: string,
    fileHash: string,
    content: string
  ): Promise<void> {
    try {
      // 生成结构化摘要（强制依赖已保证 fileSummaryCache 可用）
      const summary = this.generateFileSummaryFromContent(content, filePath);
      const source = this.mapWorkerTypeToContextSource();

      // 写入文件摘要缓存
      this.fileSummaryCache.set(filePath, fileHash, summary, source);

      logger.debug('Worker.文件摘要.已缓存', {
        filePath,
        lineCount: summary.lineCount,
        workerId: this.workerType,
      }, LogCategory.ORCHESTRATOR);

      // 如果有当前 Mission，同时写入共享上下文（sharedContextPool 强制依赖已保证可用）
      if (this.currentMissionId) {
        const entry = createSharedContextEntry({
          missionId: this.currentMissionId,
          source,
          type: 'file_summary',
          content: this.formatSummary(summary),
          tags: this.extractTagsFromFilePath(filePath),
          fileRefs: [{ path: filePath, hash: fileHash }],
          importance: 'medium',
        });

        const result = this.sharedContextPool.add(entry);
        logger.debug('Worker.文件摘要.已共享', {
          filePath,
          action: result.action,
          missionId: this.currentMissionId,
        }, LogCategory.ORCHESTRATOR);
      }
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      logger.warn('Worker.文件摘要.生成失败', {
        filePath,
        error: errorMessage,
      }, LogCategory.ORCHESTRATOR);
      throw error;
    }
  }

  /**
   * 组装共享上下文（如果依赖可用）
   *
   * 在任务执行前调用，按预算分配组装上下文注入到 prompt
   *
   * @param missionId - Mission ID
   * @param tags - 订阅标签
   * @returns 组装后的上下文（如果依赖可用）
   */
  async assembleSharedContext(
    missionId: string,
    tags: string[] = []
  ): Promise<AssembledContext | null> {
    try {
      // 强制依赖已保证 contextAssembler 可用
      const context = await this.contextAssembler.assemble({
        missionId,
        subscription: {
          agentId: this.workerType,
          subscribedTags: tags,
        },
        budget: {
          total: 8000,
          projectKnowledgeRatio: 0.10,
          sharedContextRatio: 0.25,
          contractsRatio: 0.15,
          localWindowRatio: 0.40,
          longTermMemoryRatio: 0.10,
        },
      });

      logger.info('Worker.共享上下文.组装完成', {
        missionId,
        workerId: this.workerType,
        totalTokens: context.totalTokens,
        budgetUsage: `${(context.budgetUsage * 100).toFixed(1)}%`,
        partsCount: context.parts.length,
      }, LogCategory.ORCHESTRATOR);

      return context;
    } catch (error) {
      logger.warn('Worker.共享上下文.组装失败', {
        missionId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  /**
   * 写入洞察到共享上下文池
   *
   * 将 Worker 执行过程中产生的关键结论写入共享上下文池
   *
   * @param insight - 洞察内容
   */
  async writeInsight(insight: WorkerInsight): Promise<void> {
    // 仅检查业务状态：currentMissionId（sharedContextPool 强制依赖已保证可用）
    if (!this.currentMissionId) {
      logger.debug('Worker.洞察.跳过写入', {
        reason: '无当前 Mission',
      }, LogCategory.ORCHESTRATOR);
      return;
    }

    const entry = createSharedContextEntry({
      missionId: this.currentMissionId,
      source: this.mapWorkerTypeToContextSource(),
      type: 'insight',
      content: insight.content,
      tags: insight.tags,
      importance: insight.importance,
    });

    const result = this.sharedContextPool.add(entry);
    logger.info('Worker.洞察.已写入', {
      missionId: this.currentMissionId,
      workerId: this.workerType,
      action: result.action,
      importance: insight.importance,
    }, LogCategory.ORCHESTRATOR);
  }

  // ============================================================================
  // 共享上下文私有辅助方法
  // ============================================================================

  /**
   * 计算内容 hash（取前 16 位短 hash）
   */
  private computeContentHash(content: string): string {
    const hash = crypto.createHash('sha256').update(content).digest('hex');
    return hash.substring(0, 16);
  }

  /**
   * 从文件内容生成结构化摘要
   *
   * 简化实现：基于代码结构提取摘要
   * 完整实现应调用 LLM 生成更精确的摘要
   *
   * @param content - 文件内容
   * @param filePath - 文件路径
   * @returns 结构化摘要
   */
  private generateFileSummaryFromContent(content: string, filePath: string): FileSummary {
    const lines = content.split('\n');
    const lineCount = lines.length;

    // 提取文件目的（从顶部注释或文件名推断）
    const purpose = this.extractPurposeFromContent(content, filePath);

    // 提取核心逻辑概述
    const coreLogic = this.extractCoreLogic(content);

    // 提取关键导出
    const keyExports = this.extractKeyExports(content);

    // 提取依赖
    const dependencies = this.extractDependencies(content);

    // 检测敏感逻辑
    const hasSensitiveLogic = this.detectSensitiveLogic(content);

    return {
      purpose,
      coreLogic,
      keyExports: keyExports.length > 0 ? keyExports : undefined,
      dependencies: dependencies.length > 0 ? dependencies : undefined,
      lineCount,
      hasSensitiveLogic,
    };
  }

  /**
   * 从内容中提取文件目的
   */
  private extractPurposeFromContent(content: string, filePath: string): string {
    // 尝试从文件顶部注释提取
    const commentMatch = content.match(/^(?:\/\*\*[\s\S]*?\*\/|\/\/[^\n]*)/);
    if (commentMatch) {
      const comment = commentMatch[0]
        .replace(/\/\*\*|\*\/|\/\/|\*/g, '')
        .trim()
        .split('\n')[0]
        .trim();
      if (comment.length > 10 && comment.length < 200) {
        return comment;
      }
    }

    // 备用：从文件名推断
    const fileName = filePath.split('/').pop() || filePath;
    return `${fileName} 模块`;
  }

  /**
   * 提取核心逻辑概述
   */
  private extractCoreLogic(content: string): string {
    // 统计类和函数数量
    const classMatches = content.match(/\bclass\s+\w+/g) || [];
    const functionMatches = content.match(/(?:function\s+\w+|(?:async\s+)?(?:get|set)?\s*\w+\s*\([^)]*\)\s*(?::\s*\w+)?\s*\{)/g) || [];
    const interfaceMatches = content.match(/\binterface\s+\w+/g) || [];

    const parts: string[] = [];
    if (classMatches.length > 0) {
      parts.push(`定义了 ${classMatches.length} 个类`);
    }
    if (functionMatches.length > 0) {
      parts.push(`包含 ${functionMatches.length} 个函数/方法`);
    }
    if (interfaceMatches.length > 0) {
      parts.push(`声明了 ${interfaceMatches.length} 个接口`);
    }

    return parts.length > 0 ? parts.join('，') + '。' : '代码逻辑待分析。';
  }

  /**
   * 提取关键导出
   */
  private extractKeyExports(content: string): string[] {
    const exports: string[] = [];

    // 匹配 export class/function/interface/const
    const exportMatches = content.matchAll(/export\s+(?:default\s+)?(?:class|function|interface|const|let|var|type|enum)\s+(\w+)/g);
    for (const match of exportMatches) {
      if (match[1] && exports.length < 5) {
        exports.push(match[1]);
      }
    }

    return exports;
  }

  /**
   * 提取依赖
   */
  private extractDependencies(content: string): string[] {
    const deps: string[] = [];

    // 匹配 import from 语句
    const importMatches = content.matchAll(/import\s+.*?from\s+['"]([^'"]+)['"]/g);
    for (const match of importMatches) {
      if (match[1] && deps.length < 3) {
        // 只保留外部依赖（非相对路径）
        if (!match[1].startsWith('.') && !match[1].startsWith('/')) {
          deps.push(match[1]);
        }
      }
    }

    return deps;
  }

  /**
   * 检测敏感逻辑
   */
  private detectSensitiveLogic(content: string): boolean {
    const sensitivePatterns = [
      /password/i,
      /secret/i,
      /api[_-]?key/i,
      /credential/i,
      /token/i,
      /private[_-]?key/i,
    ];

    return sensitivePatterns.some(pattern => pattern.test(content));
  }

  /**
   * 格式化摘要为可读字符串
   *
   * @param summary - 结构化摘要
   * @returns 格式化后的摘要字符串
   */
  private formatSummary(summary: FileSummary): string {
    const lines: string[] = [];

    lines.push(`**目的**: ${summary.purpose}`);
    lines.push(`**核心逻辑**: ${summary.coreLogic}`);

    if (summary.keyExports && summary.keyExports.length > 0) {
      lines.push(`**关键导出**: ${summary.keyExports.join(', ')}`);
    }

    if (summary.dependencies && summary.dependencies.length > 0) {
      lines.push(`**依赖**: ${summary.dependencies.join(', ')}`);
    }

    lines.push(`**代码行数**: ${summary.lineCount}`);

    if (summary.hasSensitiveLogic) {
      lines.push(`**注意**: 包含敏感逻辑`);
    }

    return lines.join('\n');
  }

  /**
   * 从文件路径提取标签
   *
   * @param filePath - 文件路径
   * @returns 标签数组
   */
  private extractTagsFromFilePath(filePath: string): string[] {
    const tags: string[] = [];
    const pathParts = filePath.split('/');

    // 从路径中提取模块名称作为标签
    for (const part of pathParts) {
      if (part === 'src' || part === 'test' || part === 'tests') continue;
      if (part.endsWith('.ts') || part.endsWith('.js')) continue;
      if (part.length > 2 && part.length < 30) {
        tags.push(part);
      }
    }

    // 限制标签数量
    return tags.slice(-3);
  }

  /**
   * 将 WorkerSlot 映射为 ContextSource
   */
  private mapWorkerTypeToContextSource(): ContextSource {
    switch (this.workerType) {
      case 'claude':
        return 'claude';
      case 'codex':
        return 'codex';
      case 'gemini':
        return 'gemini';
      default:
        return 'orchestrator';
    }
  }
}
