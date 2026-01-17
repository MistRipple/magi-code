/**
 * Orchestrator Agent - 独立编排者 Claude
 *
 * 核心职责：
 * - 专职编排，不执行任何编码任务
 * - 实现事件循环，实时监控所有 Worker
 * - 响应用户交互和 Worker 反馈
 * - 动态调度和错误处理
 * - CLI 降级和执行统计
 *
 * 架构理念：
 * - 编排者是"永远在线"的协调者
 * - 100% 时间用于监控和协调
 * - 可以立即响应任何事件
 *
 * Intent Gate 架构（v0.6.0）：
 * - Phase 0: 意图门控，在任务分析前先判断用户意图
 * - 核心原则：NEVER START IMPLEMENTING, UNLESS USER WANTS YOU TO IMPLEMENT SOMETHING EXPLICITLY
 */

import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { globalEventBus } from '../events';
import { MessageBus, globalMessageBus } from './message-bus';
import { WorkerPool } from './worker-pool';
import { ExecutionStats } from './execution-stats';
import { VerificationRunner, VerificationResult, VerificationConfig } from './verification-runner';
import { ContextManager, ContextCompressor } from '../context';
import { SnapshotManager } from '../snapshot-manager';
import { TaskManager } from '../task-manager';
import { RiskPolicy, RiskAssessment } from './risk-policy';
import { PolicyEngine, policyEngine, ConflictDetectionResult } from './policy-engine';
import { TaskStateManager, TaskState } from './task-state-manager';
import { PlanStorage, PlanRecord, PlanReview } from './plan-storage';
import { PlanTodoManager } from './plan-todo';
import { ExecutionStateManager, ExecutionStateStatus } from './execution-state';
import { IntentGate, IntentHandlerMode, IntentType } from './intent-gate';
import {
  WorkerType,
  OrchestratorState,
  OrchestratorConfig,
  OrchestratorEvents,
  ExecutionPlan,
  ExecutionResult,
  SubTask,
  TaskContext,
  OrchestratorUIMessage,
  QuestionCallback,
  BusMessage,
  TaskCompletedMessage,
  TaskFailedMessage,
  ProgressReportMessage,
  WorkerQuestionMessage,
} from './protocols/types';
import {
  buildOrchestratorAnalysisPrompt,
  buildOrchestratorSummaryPrompt,
  formatPlanForUser,
  buildProgressMessage,
} from './prompts/orchestrator-prompts';
import { TokenUsage } from '../cli/types';
import { PermissionMatrix, StrategyConfig, SubTaskStatus } from '../types';
import { TaskAnalyzer, TaskAnalysis } from '../task/task-analyzer';
import { TaskSplitter, SplitResult, SubTaskDef } from '../task/task-splitter';
import { CLISelector, CLISkillsConfig } from '../task/cli-selector';
import { AITaskDecomposer } from '../task/ai-task-decomposer';
import { ResultAggregator } from '../task/result-aggregator';
import { RecoveryHandler } from './recovery-handler';
import { ProfileLoader } from './profile/profile-loader';

/** 子任务自检/互检默认配置 */
const DEFAULT_REVIEW_CONFIG = {
  selfCheck: true,
  peerReview: 'auto' as const,
  maxRounds: 1,
  highRiskExtensions: ['.ts', '.tsx', '.js', '.jsx', '.json', '.yml', '.yaml'],
  highRiskKeywords: ['refactor', '重构', '迁移', '删除', 'remove', 'schema', '接口', 'config'],
};

/** 默认配置 */
const DEFAULT_CONFIG: OrchestratorConfig = {
  timeout: 300000, // 5 分钟
  maxRetries: 3,
  review: DEFAULT_REVIEW_CONFIG,
  planReview: {
    enabled: true,
    reviewer: 'claude',
  },
  verification: {
    compileCheck: true,
    lintCheck: true,
    testCheck: false,
  },
  integration: {
    enabled: true,
    maxRounds: 2,
    worker: 'claude',
  },
  permissions: {
    allowEdit: true,
    allowBash: true,
    allowWeb: true,
  },
  strategy: {
    enableVerification: true,
    enableRecovery: true,
    autoRollbackOnFailure: false,
  },
  cliSelection: {
    enabled: true,
    healthThreshold: 0.7,
  },
};

/** 用户确认回调类型 */
export type ConfirmationCallback = (plan: ExecutionPlan, formattedPlan: string) => Promise<boolean>;
export type RecoveryConfirmationCallback = (
  failedTask: TaskState,
  error: string,
  options: { retry: boolean; rollback: boolean }
) => Promise<'retry' | 'rollback' | 'continue'>;

/** 🆕 需求澄清回调类型 */
export type ClarificationCallback = (
  questions: string[],
  context: string,
  ambiguityScore: number,
  originalPrompt: string
) => Promise<{ answers: Record<string, string>; additionalInfo?: string } | null>;

/** 🆕 Worker 疑问回调类型 */
export type WorkerQuestionCallback = (
  workerId: string,
  question: string,
  context: string,
  options?: string[]
) => Promise<string | null>;

/** 🆕 需求模糊度评估结果 */
export interface AmbiguityAssessment {
  score: number;              // 0-100，越高越模糊
  isAmbiguous: boolean;       // 是否需要澄清
  questions: string[];        // 需要澄清的问题
  missingDimensions: string[]; // 缺失的维度
  context: string;            // 评估上下文
}

type ReviewDecisionStatus = 'passed' | 'rejected' | 'skipped';

interface ReviewDecision {
  status: ReviewDecisionStatus;
  reviewer?: WorkerType;
  issues?: string[];
  summary?: string;
  reason?: string;
}

interface ReviewConfigResolved {
  selfCheck: boolean;
  peerReview: 'auto' | 'always' | 'never';
  maxRounds: number;
  highRiskExtensions: string[];
  highRiskKeywords: string[];
}

type IntegrationStatus = 'passed' | 'failed';

interface IntegrationIssue {
  title?: string;
  detail?: string;
  area?: string;
  targetFiles?: string[];
  suggestedWorker?: WorkerType;
  fixPrompt?: string;
}

interface IntegrationReport {
  status: IntegrationStatus;
  summary: string;
  issues: IntegrationIssue[];
}

/**
 * Orchestrator Agent
 * 独立编排者 Claude 的核心实现
 * 集成 CLI 降级和执行统计
 */
export class OrchestratorAgent extends EventEmitter {
  readonly id: string = 'orchestrator';

  private cliFactory: CLIAdapterFactory;
  private messageBus: MessageBus;
  private workerPool: WorkerPool;
  private config: OrchestratorConfig;

  // 验证组件
  private verificationRunner: VerificationRunner | null = null;
  private workspaceRoot: string = '';

  // 上下文管理
  private contextManager: ContextManager | null = null;
  private contextCompressor: ContextCompressor | null = null;
  private contextSessionId: string | null = null;

  // 快照管理（支持文件回滚）
  private snapshotManager: SnapshotManager | null = null;
  private taskManager: TaskManager | null = null;
  private taskStateManager: TaskStateManager | null = null;
  private recoveryHandler: RecoveryHandler | null = null;
  private planStorage: PlanStorage | null = null;
  private executionStateManager: ExecutionStateManager | null = null;
  private planTodoManager: PlanTodoManager | null = null;


  private executionStats: ExecutionStats;
  private riskPolicy: RiskPolicy;
  private taskAnalyzer: TaskAnalyzer;
  private cliSelector: CLISelector;
  private taskSplitter: TaskSplitter;
  private aiTaskDecomposer: AITaskDecomposer;
  private strategyConfig: StrategyConfig;
  private permissions: PermissionMatrix;

  // 🆕 Worker 画像系统
  private profileLoader: ProfileLoader | null = null;
  // 🆕 统一策略引擎
  private policyEngine: PolicyEngine | null = null;

  // 🆕 Intent Gate - 意图门控
  private intentGate: IntentGate;

  private _state: OrchestratorState = 'idle';
  private currentContext: TaskContext | null = null;
  private confirmationCallback: ConfirmationCallback | null = null;
  private questionCallback: QuestionCallback | null = null;
  private recoveryConfirmationCallback: RecoveryConfirmationCallback | null = null;
  private clarificationCallback: ClarificationCallback | null = null;  // 🆕 需求澄清回调
  private workerQuestionCallback: WorkerQuestionCallback | null = null; // 🆕 Worker 疑问回调
  private pendingWorkerQuestions: Map<string, { resolve: (answer: string) => void; reject: (error: Error) => void }> = new Map(); // 🆕 待回答的 Worker 问题
  private planConfirmationPolicy: ((risk: TaskContext['risk'] | null) => boolean) | null = null;
  private abortController: AbortController | null = null;
  private unsubscribers: Array<() => void> = [];

  // 任务执行状态
  private pendingTasks: Map<string, SubTask> = new Map();
  private backgroundTasks: Map<string, SubTask> = new Map();
  private backgroundFinalizations: Map<string, Promise<ExecutionResult | null>> = new Map();
  private completedResults: ExecutionResult[] = [];
  private lastIntegrationSummary: string | null = null;
  private batchTasks: Map<string, SubTask[]> = new Map();
  private reviewAttempts: Map<string, number> = new Map();
  private finalizationPromises: Map<string, Promise<ExecutionResult | null>> = new Map();
  private warnedReviewSkipForDependencies = false;
  private orchestratorTokenUsage = { inputTokens: 0, outputTokens: 0 };

  constructor(
    cliFactory: CLIAdapterFactory,
    config?: Partial<OrchestratorConfig>,
    workspaceRoot?: string,
    snapshotManager?: SnapshotManager,
    taskManager?: TaskManager
  ) {
    super();
    this.cliFactory = cliFactory;
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.messageBus = globalMessageBus;
    this.workspaceRoot = workspaceRoot || '';
    this.snapshotManager = snapshotManager || null;
    this.taskManager = taskManager || null;
    if (this.workspaceRoot) {
      this.planStorage = new PlanStorage(this.workspaceRoot);
      this.executionStateManager = new ExecutionStateManager(this.workspaceRoot);
      this.planTodoManager = new PlanTodoManager(this.workspaceRoot);
    }


    this.executionStats = new ExecutionStats();
    if (this.config.cliSelection?.healthThreshold !== undefined) {
      this.executionStats.configure({ healthThreshold: this.config.cliSelection.healthThreshold });
    }
    this.riskPolicy = new RiskPolicy();
    this.taskAnalyzer = new TaskAnalyzer();
    this.cliSelector = new CLISelector();
    this.cliSelector.setExecutionStats(this.executionStats);
    if (this.config.cliSelection) {
      this.cliSelector.configureSmartSelection({
        enabled: this.config.cliSelection.enabled,
        healthThreshold: this.config.cliSelection.healthThreshold,
      });
    }
    this.taskSplitter = new TaskSplitter(this.cliSelector);
    this.aiTaskDecomposer = new AITaskDecomposer(this.cliFactory, this.cliSelector);
    this.strategyConfig = this.resolveStrategyConfig();
    this.permissions = this.resolvePermissions();

    // 🆕 初始化 Worker 画像系统
    if (this.workspaceRoot) {
      this.profileLoader = new ProfileLoader(this.workspaceRoot);
      // 异步加载画像，不阻塞构造函数
      this.profileLoader.load().catch(err => {
        console.warn('[OrchestratorAgent] ProfileLoader 加载失败:', err);
      });

      // 🆕 创建 PolicyEngine 实例并注入 ProfileLoader
      this.policyEngine = new PolicyEngine(this.profileLoader);

      // 将 ProfileLoader 注入到 CLISelector
      this.cliSelector.setProfileLoader(this.profileLoader);
    }

    // 🆕 初始化 Intent Gate - 意图门控
    this.intentGate = new IntentGate();

    // 创建 Worker Pool，集成执行统计和快照管理
    this.workerPool = new WorkerPool({
      cliFactory,
      messageBus: this.messageBus,
      orchestratorId: this.id,
      executionStats: this.executionStats,
      enableFallback: true,
      snapshotManager: this.snapshotManager || undefined,
      permissions: this.permissions,
      workspacePath: this.workspaceRoot, // 🆕 传递工作区路径，用于加载 Worker 画像
    });

    // 初始化验证组件
    if (this.workspaceRoot && this.config.verification && this.strategyConfig.enableVerification) {
      this.verificationRunner = new VerificationRunner(this.workspaceRoot, {
        compileCheck: this.config.verification.compileCheck ?? true,
        lintCheck: this.config.verification.lintCheck ?? false,
        testCheck: this.config.verification.testCheck ?? false,
      } as Partial<VerificationConfig>);
    }

    // 初始化上下文管理
    if (this.workspaceRoot) {
      this.contextManager = new ContextManager(this.workspaceRoot);
      this.contextCompressor = new ContextCompressor();
    }

    this.setupMessageHandlers();
    this.setupWorkerPoolHandlers();
  }

  /** 获取当前状态 */
  get state(): OrchestratorState {
    return this._state;
  }

  /** 获取当前任务上下文 */
  get context(): TaskContext | null {
    return this.currentContext;
  }

  /** 设置状态 */
  private setState(state: OrchestratorState): void {
    if (this._state !== state) {
      const oldState = this._state;
      this._state = state;
      this.emit('stateChange', state);
      console.log(`[OrchestratorAgent] 状态变更: ${oldState} -> ${state}`);
    }
  }

  /** 设置确认回调 */
  setConfirmationCallback(callback: ConfirmationCallback): void {
    this.confirmationCallback = callback;
  }

  /** 设置用户补充信息回调 */
  setQuestionCallback(callback: QuestionCallback): void {
    this.questionCallback = callback;
  }

  /** 设置失败恢复确认回调 */
  setRecoveryConfirmationCallback(callback: RecoveryConfirmationCallback): void {
    this.recoveryConfirmationCallback = callback;
  }

  /** 🆕 设置需求澄清回调 */
  setClarificationCallback(callback: ClarificationCallback): void {
    this.clarificationCallback = callback;
  }

  /** 🆕 设置 Worker 疑问回调 */
  setWorkerQuestionCallback(callback: WorkerQuestionCallback): void {
    this.workerQuestionCallback = callback;
  }

  /** 设置执行计划确认策略 */
  setPlanConfirmationPolicy(policy: (risk: TaskContext['risk'] | null) => boolean): void {
    this.planConfirmationPolicy = policy;
  }

  /** 更新 CLI 技能配置 */
  setCliSkills(skills: Partial<CLISkillsConfig>): void {
    this.cliSelector.updateSkills(skills);
  }

  /** 设置扩展上下文（用于持久化执行统计） */
  setExtensionContext(context: import('vscode').ExtensionContext): void {
    this.executionStats.setContext(context);
  }

  /** 获取执行统计实例 */
  getExecutionStats(): ExecutionStats {
    return this.executionStats;
  }

  /** 获取执行统计摘要（用于 UI 显示） */
  getStatsSummary(): string {
    return this.executionStats.getSummary();
  }

  /** 初始化 */
  async initialize(): Promise<void> {
    await this.workerPool.initialize();

    // 🆕 将 ProfileLoader 设置给 CLISelector 和 TaskAnalyzer，实现画像驱动的任务分配
    const profileLoader = this.workerPool.getProfileLoader();
    if (profileLoader) {
      this.cliSelector.setProfileLoader(profileLoader);
      this.taskAnalyzer.setProfileLoader(profileLoader);
      console.log('[OrchestratorAgent] CLISelector 和 TaskAnalyzer 已集成 Worker 画像');
    }

    console.log('[OrchestratorAgent] 初始化完成');
    console.log(`[OrchestratorAgent] 执行统计: ${this.getStatsSummary()}`);
  }

  /** 获取指定计划记录 */
  getPlanById(planId: string, sessionId: string): PlanRecord | null {
    return this.planStorage?.getPlan(planId, sessionId) ?? null;
  }

  /** 获取会话最新计划记录 */
  getLatestPlanForSession(sessionId: string): PlanRecord | null {
    return this.planStorage?.getLatestPlanForSession(sessionId) ?? null;
  }

  /** 获取会话当前激活计划记录 */
  getActivePlanForSession(sessionId: string): PlanRecord | null {
    if (!this.executionStateManager) {
      return this.getLatestPlanForSession(sessionId);
    }
    const state = this.executionStateManager.loadState(sessionId);
    if (!state) {
      return this.getLatestPlanForSession(sessionId);
    }
    const record = this.planStorage?.getPlan(state.activePlanId, sessionId) ?? null;
    return record ?? this.getLatestPlanForSession(sessionId);
  }

  /** 生成执行计划但不执行 */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    if (this._state !== 'idle') {
      if (this._state === 'failed' || this._state === 'completed') {
        this.setState('idle');
      } else {
        throw new Error(`编排者当前状态为 ${this._state}，无法生成计划`);
      }
    }

    const contextSessionId = sessionId || taskId;
    this.currentContext = {
      taskId,
      sessionId: contextSessionId,
      userPrompt,
      results: [],
      startTime: Date.now(),
    };
    this.abortController = new AbortController();
    this.completedResults = [];
    this.pendingTasks.clear();
    this.backgroundTasks.clear();
    this.backgroundFinalizations.clear();
    this.reviewAttempts.clear();
    this.finalizationPromises.clear();
    this.warnedReviewSkipForDependencies = false;
    this.lastIntegrationSummary = null;
    this.batchTasks.clear();

    await this.ensureContext(contextSessionId, userPrompt);

    try {
      let plan: ExecutionPlan | null = null;
      let analysisPrompt = userPrompt;
      for (let round = 0; round < 3; round += 1) {
        this.setState('analyzing');
        this.currentContext.userPrompt = analysisPrompt;
        plan = await this.analyzeTask(analysisPrompt);

        if (!plan) {
          throw new Error('任务分析失败');
        }

        if (plan.needsUserInput && plan.questions && plan.questions.length > 0) {
          this.setState('waiting_questions');
          const answer = await this.waitForUserInput(plan);
          if (!answer) {
            this.setState('idle');
            throw new Error('计划已取消');
          }
          analysisPrompt = `${userPrompt}\n\n## 用户补充信息\n${answer}`;
          continue;
        }

        break;
      }

      if (!plan) {
        throw new Error('任务分析失败');
      }

      if (plan.needsUserInput && plan.questions && plan.questions.length > 0) {
        throw new Error('用户补充信息不足，无法生成执行计划');
      }

      this.currentContext.plan = plan;
      this.currentContext.risk = this.riskPolicy.evaluate(userPrompt, plan);
      this.checkAborted();

      const formattedPlan = formatPlanForUser(plan);
      const review = await this.reviewPlan(plan, formattedPlan);
      const record = this.persistPlan(plan, formattedPlan, review);
      if (!record) {
        throw new Error('执行计划持久化失败');
      }
      if (review.status === 'rejected') {
        this.updateTaskPlanStatus('failed');
        this.emitUIMessage('progress_update', `计划评审未通过: ${review.summary}`);
      }

      // 🔧 修复：在 createPlan 完成后发送 plan_ready（用于 /plan 命令）
      this.emitUIMessage('plan_ready', formattedPlan, {
        plan,
        planId: record.id,
        formattedPlan,
        review: { status: review.status, summary: review.summary }
      });
      globalEventBus.emitEvent('orchestrator:plan_ready', {
        taskId: this.currentContext?.taskId,
        data: { plan, planId: record.id, formattedPlan },
      });

      this.setState('idle');
      this.currentContext.endTime = Date.now();
      return record;
    } catch (error) {
      this.setState('failed');
      throw error;
    } finally {
      this.cleanup();
    }
  }

  /** 使用已有执行计划开始执行 */
  async executePlan(plan: ExecutionPlan, taskId: string, sessionId?: string, userPrompt?: string): Promise<string> {
    if (this._state !== 'idle') {
      if (this._state === 'failed' || this._state === 'completed') {
        this.setState('idle');
      } else {
        throw new Error(`编排者当前状态为 ${this._state}，无法执行计划`);
      }
    }

    const prompt = userPrompt || plan.summary || '执行已保存计划';
    const contextSessionId = sessionId || taskId;
    this.currentContext = {
      taskId,
      sessionId: contextSessionId,
      userPrompt: prompt,
      results: [],
      startTime: Date.now(),
    };
    this.abortController = new AbortController();
    this.completedResults = [];
    this.pendingTasks.clear();
    this.backgroundTasks.clear();
    this.backgroundFinalizations.clear();
    this.reviewAttempts.clear();
    this.finalizationPromises.clear();
    this.warnedReviewSkipForDependencies = false;
    this.lastIntegrationSummary = null;
    this.batchTasks.clear();

    await this.ensureContext(contextSessionId, prompt);

    try {
      this.normalizeExecutionPlan(plan);
      this.currentContext.plan = plan;
      this.currentContext.risk = this.riskPolicy.evaluate(prompt, plan);
      this.checkAborted();

      return await this.runPlanExecution(prompt, plan, taskId);
    } catch (error) {
      if (this.abortController?.signal.aborted) {
        this.setState('idle');
        return '任务已被取消。';
      }
      this.setState('failed');
      this.emitUIMessage('error', `任务执行失败: ${error instanceof Error ? error.message : String(error)}`);
      this.updateExecutionState('failed', plan.id);
      this.updateTaskPlanStatus('failed');
      throw error;
    } finally {
      this.cleanup();
    }
  }

  /** 设置消息处理器 */
  private setupMessageHandlers(): void {
    // 监听任务完成消息
    const unsubCompleted = this.messageBus.subscribe('task_completed', (msg) => {
      this.handleTaskCompleted(msg as TaskCompletedMessage);
    });
    this.unsubscribers.push(unsubCompleted);

    // 监听任务失败消息
    const unsubFailed = this.messageBus.subscribe('task_failed', (msg) => {
      void this.handleTaskFailed(msg as TaskFailedMessage);
    });
    this.unsubscribers.push(unsubFailed);

    // 监听进度汇报消息
    const unsubProgress = this.messageBus.subscribe('progress_report', (msg) => {
      this.handleProgressReport(msg as ProgressReportMessage);
    });
    this.unsubscribers.push(unsubProgress);

    // 🆕 监听 Worker 问题消息
    const unsubWorkerQuestion = this.messageBus.subscribe('worker_question', (msg) => {
      void this.handleWorkerQuestion(msg as WorkerQuestionMessage);
    });
    this.unsubscribers.push(unsubWorkerQuestion);
  }

  /**
   * 🆕 处理 Worker 问题
   * 将问题转发给用户，并将回答返回给 Worker
   */
  private async handleWorkerQuestion(message: WorkerQuestionMessage): Promise<void> {
    const { taskId, subTaskId, workerId, question, context, options, questionId } = message.payload;

    console.log(`[OrchestratorAgent] 收到 Worker 问题: ${question} (from ${workerId})`);

    // 通知 UI 有 Worker 提问
    this.emitUIMessage('progress_update', `Worker ${workerId} 提问: ${question}`, {
      subTaskId,
      workerQuestion: { questionId, question, context, options }
    } as any);

    // 如果设置了 Worker 问题回调，转发给用户
    if (this.workerQuestionCallback) {
      try {
        const previousState = this._state;
        this.setState('waiting_worker_answer');

        const answer = await this.workerQuestionCallback(workerId, question, context, options);

        if (answer) {
          // 将回答发送给 Worker
          this.messageBus.sendWorkerAnswer(
            this.id,
            workerId,
            taskId,
            subTaskId,
            questionId,
            answer,
            'user'
          );
          console.log(`[OrchestratorAgent] 已回答 Worker 问题: ${questionId}`);
        } else {
          // 用户取消回答，使用默认回答
          this.messageBus.sendWorkerAnswer(
            this.id,
            workerId,
            taskId,
            subTaskId,
            questionId,
            '请自行决定最佳方案',
            'orchestrator'
          );
          console.log(`[OrchestratorAgent] 用户未回答，使用默认回答: ${questionId}`);
        }

        // 恢复之前的状态
        this.setState(previousState);

      } catch (error) {
        console.error('[OrchestratorAgent] 处理 Worker 问题异常:', error);
        // 发送默认回答
        this.messageBus.sendWorkerAnswer(
          this.id,
          workerId,
          taskId,
          subTaskId,
          questionId,
          '请自行决定最佳方案',
          'orchestrator'
        );
      }
    } else {
      // 没有设置回调，自动回答
      console.log('[OrchestratorAgent] 未设置 Worker 问题回调，自动回答');
      this.messageBus.sendWorkerAnswer(
        this.id,
        workerId,
        taskId,
        subTaskId,
        questionId,
        '请自行决定最佳方案',
        'orchestrator'
      );
    }
  }

  /** 设置 Worker Pool 事件处理 */
  private setupWorkerPoolHandlers(): void {
    this.workerPool.on('taskRetry', ({ subTaskId, attempt, delay }) => {
      this.emitUIMessage(
        'progress_update',
        `子任务重试中 (${attempt}/${this.config.maxRetries})，等待 ${Math.round(delay)}ms`,
        { subTaskId, retryAttempt: attempt, retryDelay: delay }
      );
    });

    this.workerPool.on('cliFallback', ({ original, fallback, reason }) => {
      this.emitUIMessage(
        'progress_update',
        `CLI 降级: ${original} -> ${fallback}，原因: ${reason}`
      );
    });
  }

  // =========================================================================
  // 核心执行流程
  // =========================================================================

  // =========================================================================
  // 🆕 Phase 0: 需求澄清机制
  // =========================================================================

  /**
   * 🆕 评估需求模糊度
   * 通过 Claude 分析用户需求的明确程度
   */
  private async assessAmbiguity(userPrompt: string): Promise<AmbiguityAssessment> {
    console.log('[OrchestratorAgent] Phase 0: 评估需求模糊度...');

    const assessmentPrompt = `你是一个需求分析专家。请评估以下用户需求的模糊程度。

## 用户需求
${userPrompt}

## 评估维度
1. **目标明确性**：是否有明确的功能目标？是否有具体的输入/输出定义？
2. **范围明确性**：是否指定了目标文件/模块？是否有边界条件说明？
3. **技术明确性**：是否指定了技术方案？是否有接口定义？
4. **验收标准**：是否有明确的完成标准？是否有测试用例？

## 输出格式（JSON）
\`\`\`json
{
  "score": 0-100,  // 模糊度评分，0=完全明确，100=完全模糊
  "isAmbiguous": true/false,  // 是否需要澄清（score > 50 时为 true）
  "missingDimensions": ["目标明确性", ...],  // 缺失的维度
  "questions": ["问题1", "问题2"],  // 需要用户回答的问题（最多3个）
  "context": "评估说明"
}
\`\`\`

只输出 JSON，不要其他内容。`;

    try {
      const response = await this.cliFactory.sendMessage(
        'claude',
        assessmentPrompt,
        undefined,
        { source: 'orchestrator', streamToUI: false }
      );

      if (response.error) {
        console.warn('[OrchestratorAgent] 模糊度评估失败，默认为明确需求');
        return {
          score: 0,
          isAmbiguous: false,
          questions: [],
          missingDimensions: [],
          context: '评估失败，默认为明确需求'
        };
      }

      const jsonMatch = response.content.match(/```json\s*([\s\S]*?)\s*```/);
      const jsonStr = jsonMatch ? jsonMatch[1] : response.content;
      const parsed = JSON.parse(jsonStr);

      return {
        score: parsed.score || 0,
        isAmbiguous: parsed.isAmbiguous || parsed.score > 50,
        questions: parsed.questions || [],
        missingDimensions: parsed.missingDimensions || [],
        context: parsed.context || ''
      };
    } catch (error) {
      console.warn('[OrchestratorAgent] 模糊度评估异常:', error);
      return {
        score: 0,
        isAmbiguous: false,
        questions: [],
        missingDimensions: [],
        context: '评估异常，默认为明确需求'
      };
    }
  }

  /**
   * 🆕 执行需求澄清流程
   * 向用户提问并等待回答
   */
  private async clarifyRequirements(
    userPrompt: string,
    assessment: AmbiguityAssessment
  ): Promise<string> {
    if (!this.clarificationCallback) {
      console.log('[OrchestratorAgent] 未设置澄清回调，跳过澄清流程');
      return userPrompt;
    }

    console.log(`[OrchestratorAgent] 需求模糊度: ${assessment.score}%，开始澄清流程`);
    this.setState('clarifying');

    // 发送澄清请求消息
    this.messageBus.requestClarification(
      this.id,
      this.currentContext?.taskId || '',
      assessment.questions,
      assessment.context,
      assessment.score,
      userPrompt
    );

    // 通知 UI
    this.emitUIMessage('progress_update', `检测到需求模糊（${assessment.score}%），正在请求澄清...`);

    try {
      const result = await this.clarificationCallback(
        assessment.questions,
        assessment.context,
        assessment.score,
        userPrompt
      );

      if (!result) {
        console.log('[OrchestratorAgent] 用户取消澄清');
        return userPrompt;
      }

      // 合并用户回答到原始需求
      const answersText = Object.entries(result.answers)
        .map(([q, a]) => `Q: ${q}\nA: ${a}`)
        .join('\n\n');

      const clarifiedPrompt = `${userPrompt}

## 用户补充澄清
${answersText}
${result.additionalInfo ? `\n## 额外信息\n${result.additionalInfo}` : ''}`;

      console.log('[OrchestratorAgent] 需求澄清完成');
      return clarifiedPrompt;

    } catch (error) {
      console.error('[OrchestratorAgent] 澄清流程异常:', error);
      return userPrompt;
    }
  }

  /**
   * 执行任务 - 主入口
   */
  async execute(userPrompt: string, taskId: string, sessionId?: string): Promise<string> {
    if (this._state !== 'idle') {
      if (this._state === 'failed' || this._state === 'completed') {
        this.setState('idle');
      } else {
        throw new Error(`编排者当前状态为 ${this._state}，无法接受新任务`);
      }
    }

    // 初始化任务上下文
    const contextSessionId = sessionId || taskId;
    this.currentContext = {
      taskId,
      sessionId: contextSessionId,
      userPrompt,
      results: [],
      startTime: Date.now(),
    };
    this.abortController = new AbortController();
    this.completedResults = [];
    this.pendingTasks.clear();
    this.backgroundTasks.clear();
    this.backgroundFinalizations.clear();
    this.reviewAttempts.clear();
    this.finalizationPromises.clear();
    this.warnedReviewSkipForDependencies = false;
    this.lastIntegrationSummary = null;
    this.batchTasks.clear();
    this.pendingWorkerQuestions.clear();

    // 初始化上下文管理器
    await this.ensureContext(contextSessionId, userPrompt);

    try {
      // 🆕 Phase 0: Intent Gate - 意图门控
      // 核心原则：NEVER START IMPLEMENTING, UNLESS USER WANTS YOU TO IMPLEMENT SOMETHING EXPLICITLY
      const intentResult = this.intentGate.process(userPrompt);
      console.log(`[OrchestratorAgent] Intent Gate: ${intentResult.classification.type}, ` +
                  `confidence: ${(intentResult.classification.confidence * 100).toFixed(0)}%, ` +
                  `mode: ${intentResult.recommendedMode}`);

      // 根据意图类型路由处理
      if (intentResult.skipTaskAnalysis) {
        const response = await this.handleIntentDirectly(userPrompt, intentResult, taskId);
        if (response !== null) {
          return response;
        }
        // 如果 handleIntentDirectly 返回 null，继续走任务分析流程
      }

      // Phase 0.5: 需求澄清（如果设置了澄清回调且意图门控未处理）
      let clarifiedPrompt = userPrompt;
      if (this.clarificationCallback && !intentResult.needsClarification) {
        const assessment = await this.assessAmbiguity(userPrompt);
        if (assessment.isAmbiguous && assessment.questions.length > 0) {
          clarifiedPrompt = await this.clarifyRequirements(userPrompt, assessment);
          this.currentContext.userPrompt = clarifiedPrompt;
        }
      }
      this.checkAborted();

      // Phase 1: 任务分析（支持补充提问）
      let plan: ExecutionPlan | null = null;
      let analysisPrompt = clarifiedPrompt;
      for (let round = 0; round < 3; round += 1) {
        this.setState('analyzing');
        this.currentContext.userPrompt = analysisPrompt;
        plan = await this.analyzeTask(analysisPrompt);

        if (!plan) {
          throw new Error('任务分析失败');
        }

        if (plan.needsUserInput && plan.questions && plan.questions.length > 0) {
          this.setState('waiting_questions');
          const answer = await this.waitForUserInput(plan);
          if (!answer) {
            this.setState('idle');
            return '任务已取消。';
          }
          analysisPrompt = `${clarifiedPrompt}\n\n## 用户补充信息\n${answer}`;
          continue;
        }

        break;
      }

      if (!plan) {
        throw new Error('任务分析失败');
      }

      if (plan.needsUserInput && plan.questions && plan.questions.length > 0) {
        throw new Error('用户补充信息不足，无法生成执行计划');
      }

      this.currentContext.plan = plan;
      const risk: RiskAssessment = this.riskPolicy.evaluate(userPrompt, plan);
      this.currentContext.risk = risk;
      this.checkAborted();
      return await this.runPlanExecution(userPrompt, plan, taskId);

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);

      if (this.abortController?.signal.aborted) {
        this.setState('idle');
        return '任务已被取消。';
      }

      this.setState('failed');
      this.emitUIMessage('error', `任务执行失败: ${errorMsg}`);
      this.updateExecutionState('failed', this.currentContext?.plan?.id);
      this.updateTaskPlanStatus('failed');
      throw error;

    } finally {
      this.cleanup();
    }
  }

  private persistPlan(plan: ExecutionPlan, formattedPlan: string, review?: PlanReview): PlanRecord | null {
    if (!this.planStorage || !this.currentContext) return null;
    const now = Date.now();
    const planId = plan.id || `plan_${now}`;
    plan.id = planId;
    const sessionId = this.currentContext.sessionId || this.currentContext.taskId;
    const record: PlanRecord = {
      id: planId,
      sessionId,
      taskId: this.currentContext.taskId,
      prompt: this.currentContext.userPrompt,
      createdAt: now,
      updatedAt: now,
      plan,
      formattedPlan,
      review,
    };
    this.planStorage.savePlan(record);
    this.planTodoManager?.ensurePlanFile(record);
    if (this.taskManager) {
      const summary = plan.summary || plan.analysis || plan.featureContract || '执行计划已生成';
      this.taskManager.updateTaskPlan(this.currentContext.taskId, {
        planId,
        planSummary: summary,
        status: 'ready',
      });
    }
    if (this.contextManager) {
      const summary = plan.summary || plan.analysis || plan.featureContract;
      if (summary) {
        this.contextManager.addImportantContext(`计划摘要: ${summary}`);
      }
      if (plan.featureContract) {
        this.contextManager.addDecision(
          `contract-${planId}`,
          '功能契约',
          plan.featureContract
        );
      }
      if (review?.status === 'rejected') {
        this.contextManager.addPendingIssue(`计划评审未通过: ${review.summary}`);
      }
      void this.contextManager.saveMemory();
    }
    this.updateExecutionState('planned', planId);
    return record;
  }

  private updateExecutionState(status: ExecutionStateStatus, planId?: string): void {
    if (!this.executionStateManager || !this.currentContext) return;
    const activePlanId = planId || this.currentContext.plan?.id;
    if (!activePlanId) return;
    const now = Date.now();
    const sessionId = this.currentContext.sessionId || this.currentContext.taskId;
    const existing = this.executionStateManager.loadState(sessionId);
    const state = {
      sessionId,
      activePlanId,
      taskId: this.currentContext.taskId,
      status,
      createdAt: existing?.createdAt ?? now,
      updatedAt: now,
    };
    this.executionStateManager.saveState(state);
  }

  private updateTaskPlanStatus(status: ExecutionStateStatus): void {
    if (!this.taskManager || !this.currentContext) return;
    const mapping: Record<ExecutionStateStatus, 'ready' | 'executing' | 'completed' | 'failed'> = {
      planned: 'ready',
      executing: 'executing',
      completed: 'completed',
      failed: 'failed',
    };
    this.taskManager.updateTaskPlanStatus(this.currentContext.taskId, mapping[status]);
  }

  private shouldReviewPlan(): boolean {
    return this.config.planReview?.enabled !== false;
  }

  private buildPlanReviewPrompt(plan: ExecutionPlan, formattedPlan: string): string {
    return [
      '你是执行计划评审专家，请审查以下计划是否具备可执行性、边界清晰、职责拆分合理。',
      '',
      '## 执行计划',
      formattedPlan,
      '',
      '## 评审标准',
      '1. 目标是否明确、可验证',
      '2. 子任务是否覆盖关键路径，是否有遗漏',
      '3. 依赖关系是否合理',
      '4. 是否存在高风险或歧义点',
      '',
      '## 输出要求（只输出 JSON）',
      '```json',
      '{',
      '  "status": "approved | rejected",',
      '  "summary": "评审结论（必要时指出需修订的点）"',
      '}',
      '```',
    ].join('\n');
  }

  private parsePlanReview(content: string): { status: 'approved' | 'rejected'; summary: string } {
    const match = content.match(/\{[\s\S]*\}/);
    if (!match) {
      return { status: 'approved', summary: '未提供结构化评审，默认通过' };
    }
    try {
      const parsed = JSON.parse(match[0]);
      const status = parsed.status === 'rejected' ? 'rejected' : 'approved';
      const summary = typeof parsed.summary === 'string' ? parsed.summary : '未提供评审摘要';
      return { status, summary };
    } catch (error) {
      return { status: 'approved', summary: '评审解析失败，默认通过' };
    }
  }

  private async reviewPlan(plan: ExecutionPlan, formattedPlan: string): Promise<PlanReview> {
    if (!this.shouldReviewPlan()) {
      return { status: 'skipped', summary: '计划评审已关闭', reviewer: 'system', reviewedAt: Date.now() };
    }
    const reviewer = this.config.planReview?.reviewer ?? 'claude';
    try {
      const response = await this.cliFactory.sendMessage(
        reviewer,
        this.buildPlanReviewPrompt(plan, formattedPlan),
        undefined,
        {
          source: 'orchestrator',
          streamToUI: false,
          adapterRole: 'orchestrator',
          messageMeta: {
            taskId: this.currentContext?.taskId,
            intent: 'plan_review',
          },
        }
      );
      // 记录 token 使用
      this.recordOrchestratorTokens(response.tokenUsage);
      if (response.error) {
        return { status: 'approved', summary: `评审失败(${response.error})，默认通过`, reviewer, reviewedAt: Date.now() };
      }
      const decision = this.parsePlanReview(response.content || '');
      return { status: decision.status, summary: decision.summary, reviewer, reviewedAt: Date.now() };
    } catch (error) {
      return { status: 'approved', summary: `评审异常(${error instanceof Error ? error.message : String(error)})，默认通过`, reviewer, reviewedAt: Date.now() };
    }
  }

  private async runPlanExecution(userPrompt: string, plan: ExecutionPlan, taskId: string): Promise<string> {
    const formattedPlan = formatPlanForUser(plan);
    const sessionId = this.currentContext?.sessionId || this.currentContext?.taskId || taskId;
    const existingRecord = plan.id ? this.planStorage?.getPlan(plan.id, sessionId) : null;
    const review = existingRecord?.review ?? await this.reviewPlan(plan, formattedPlan);
    const record = this.persistPlan(plan, formattedPlan, review);
    if (!record) {
      console.warn('[OrchestratorAgent] 计划持久化失败，继续执行');
    }
    if (review.status === 'rejected') {
      this.updateTaskPlanStatus('failed');
      this.emitUIMessage('error', `计划评审未通过: ${review.summary}`);
      throw new Error('计划评审未通过，请修订后重试。');
    }

   
    // 问答类请求：不需要 Worker，编排者直接回答
    if (plan.needsWorker === false) {
      console.log('[OrchestratorAgent] 不需要 Worker，编排者直接回答');

      let response = plan.directResponse || '';

      // 如果没有预设回答，调用 Claude 生成
      if (!response) {
        const askResponse = await this.cliFactory.sendMessage(
          'claude',
          userPrompt,
          undefined,
          {
            source: 'orchestrator',
            streamToUI: true,
            adapterRole: 'orchestrator',
            messageMeta: {
              taskId: this.currentContext?.taskId,
              intent: 'ask',
              contextSnapshot: this.buildContextSnapshot(),
            },
          }
        );
        this.recordOrchestratorTokens(askResponse.tokenUsage);
        response = askResponse.content || '';
      } else {
        this.emitUIMessage('direct_response', response);
      }

      await this.saveAndCompressMemory(response);
      this.setState('completed');
      this.currentContext!.endTime = Date.now();
      this.updateExecutionState('completed', plan.id);
      this.updateTaskPlanStatus('completed');
      return response;
    }

    // 记录任务到 Memory
    if (this.contextManager && plan.subTasks) {
      plan.subTasks.forEach(task => {
        this.contextManager!.addTask({
          id: task.id,
          description: task.description,
          status: 'pending',
          assignedWorker: task.assignedWorker
        });
      });
      void this.contextManager.saveMemory();
    }

    // Phase 2: 等待用户确认（按策略判定）
    let confirmed = true;
    const shouldConfirm = this.planConfirmationPolicy
      ? this.planConfirmationPolicy(this.currentContext?.risk ?? null)
      : Boolean(this.currentContext?.risk?.hardStop);
    if (shouldConfirm) {
      this.setState('waiting_confirmation');
      confirmed = await this.waitForConfirmation(plan);
    }

    if (!confirmed) {
      this.setState('idle');
      this.updateExecutionState('planned', plan.id);
      this.updateTaskPlanStatus('planned');
      return '任务已取消。';
    }
    this.checkAborted();

    this.updateExecutionState('executing', plan.id);
    this.updateTaskPlanStatus('executing');

    // Phase 3: 分发任务给 Worker
    this.setState('dispatching');
    await this.dispatchTasks(plan);

    // Phase 4: 监控执行
    this.setState('monitoring');
    await this.monitorExecution(plan);
    this.checkAborted();

    // Phase 4.5: 功能集成联调
    await this.runIntegrationStage(plan);
    this.checkAborted();

    // Phase 5: 验证阶段（如果配置了验证）
    let verificationResult: VerificationResult | null = null;
    if (this.verificationRunner && this.currentContext?.risk?.verification !== 'none' && this.strategyConfig.enableVerification) {
      this.setState('verifying');
      verificationResult = await this.runVerification(taskId);

      // 如果验证失败，记录错误但继续汇总
      if (!verificationResult.success) {
        this.emitUIMessage('error', `验证失败: ${verificationResult.summary}`);
      }
    }
    this.checkAborted();

    await this.waitForBackgroundTasks();

    // Phase 6: 汇总结果
    this.setState('summarizing');
    const summary = await this.summarizeResults(userPrompt, this.completedResults, verificationResult);

    // 保存 Memory 并检查是否需要压缩
    await this.saveAndCompressMemory(summary);

    this.setState('completed');
    this.currentContext!.endTime = Date.now();
    this.updateExecutionState('completed', plan.id);
    this.updateTaskPlanStatus('completed');

    return summary;
  }

  /**
   * 保存 Memory 并检查是否需要压缩
   */
  private async saveAndCompressMemory(summary: string): Promise<void> {
    if (!this.contextManager) return;

    // 添加助手响应到即时上下文
    this.contextManager.addMessage({ role: 'assistant', content: summary });

    // 检查是否需要压缩
    if (this.contextManager.needsCompression() && this.contextCompressor) {
      const memory = this.contextManager.getMemoryDocument();
      if (memory) {
        console.log('[OrchestratorAgent] Memory 需要压缩，开始压缩...');
        await this.contextCompressor.compress(memory);
      }
    }

    // 保存 Memory
    await this.contextManager.saveMemory();
  }

  /** 确保上下文已初始化并注入最新对话 */
  private async ensureContext(sessionId: string, userPrompt?: string): Promise<string> {
    if (!this.contextManager) return '';

    if (this.contextSessionId !== sessionId) {
      await this.contextManager.initialize(sessionId, `session-${sessionId}`);
      this.contextManager.clearImmediateContext();
      this.contextSessionId = sessionId;
      if (this.workspaceRoot) {
        this.taskStateManager = new TaskStateManager(sessionId, this.workspaceRoot, true);
        await this.taskStateManager.load();
        this.taskStateManager.onStateChange((taskState) => {
          this.applyTaskStateToTaskManager(taskState);
        });
        this.replayTaskStatesToTaskManager();
        if (this.snapshotManager && this.strategyConfig.enableRecovery) {
          this.recoveryHandler = new RecoveryHandler(
            this.cliFactory,
            this.snapshotManager,
            this.taskStateManager
          );
        }
      }
    }

    if (userPrompt) {
      this.contextManager.addMessage({ role: 'user', content: userPrompt });
    }

    return this.contextManager.getContext();
  }

  private buildContextSnapshot(maxTokens: number = 1200): string {
    if (!this.contextManager) return '';
    const snapshot = this.contextManager.getContextSlice({ maxTokens });
    return snapshot.trim();
  }

  /** ask 模式使用：准备上下文并注入用户输入 */
  async prepareContext(sessionId: string, userPrompt: string): Promise<string> {
    return this.ensureContext(sessionId, userPrompt);
  }

  /** ask 模式使用：记录编排者回复并持久化 Memory */
  async recordAssistantMessage(content: string): Promise<void> {
    if (!this.contextManager) return;
    await this.saveAndCompressMemory(content);
  }

  /** 检查是否被中断 */
  private checkAborted(): void {
    if (this.abortController?.signal.aborted) {
      throw new Error('任务已被用户取消');
    }
  }

  /** 取消当前任务 */
  async cancel(): Promise<void> {
    console.log('[OrchestratorAgent] 取消任务');

    // 1. 触发 AbortController
    this.abortController?.abort();

    // 2. 取消 WorkerPool 中的所有任务
    await this.workerPool.cancelAllTasks();
    this.workerPool.clearExecutionStates();

    // 3. 清理内部状态
    this.cleanup();

    // 4. 设置状态为 idle
    this.setState('idle');

    if (this.taskStateManager) {
      for (const task of this.taskStateManager.getAllTasks()) {
        if (task.status === 'completed' || task.status === 'failed' || task.status === 'cancelled') {
          continue;
        }
        this.taskStateManager.updateStatus(task.id, 'cancelled');
      }
    }

    console.log('[OrchestratorAgent] 任务已取消，状态已清理');
  }

  /** 清理状态 */
  private cleanup(): void {
    this.abortController = null;
    this.pendingTasks.clear();
    this.reviewAttempts.clear();
    this.finalizationPromises.clear();
    this.warnedReviewSkipForDependencies = false;
  }

  // =========================================================================
  // Phase 0: Intent Gate - 意图门控
  // =========================================================================

  /**
   * 直接处理不需要任务分析的意图
   * @returns 处理结果，如果返回 null 则继续走任务分析流程
   */
  private async handleIntentDirectly(
    userPrompt: string,
    intentResult: import('./intent-gate').IntentGateResult,
    taskId: string
  ): Promise<string | null> {
    const { recommendedMode, classification, needsClarification, clarificationQuestions } = intentResult;

    switch (recommendedMode) {
      case IntentHandlerMode.ASK:
        // 问答模式：直接调用编排者 Claude 回答
        console.log('[OrchestratorAgent] Intent Gate: ASK 模式，直接回答');
        return await this.executeAskMode(userPrompt, taskId);

      case IntentHandlerMode.CLARIFY:
        // 澄清模式：请求用户提供更多信息
        console.log('[OrchestratorAgent] Intent Gate: CLARIFY 模式，请求澄清');
        if (this.clarificationCallback && clarificationQuestions && clarificationQuestions.length > 0) {
          // 使用现有的澄清回调接口
          const result = await this.clarificationCallback(
            clarificationQuestions,
            classification.reason,
            Math.round((1 - classification.confidence) * 100), // 转换为模糊度分数
            userPrompt
          );
          if (result && result.additionalInfo) {
            // 用户提供了澄清信息，重新处理
            const clarifiedPrompt = `${userPrompt}\n\n## 用户补充信息\n${result.additionalInfo}`;
            this.currentContext!.userPrompt = clarifiedPrompt;
            return null; // 继续走任务分析流程
          }
        }
        // 没有澄清回调或用户未提供澄清，降级为问答
        return await this.executeAskMode(userPrompt, taskId);

      case IntentHandlerMode.EXPLORE:
        // 探索模式：分析代码库后回答
        console.log('[OrchestratorAgent] Intent Gate: EXPLORE 模式，探索分析');
        return await this.executeExploreMode(userPrompt, taskId);

      case IntentHandlerMode.DIRECT:
        // 直接执行模式：简单操作，无需计划
        console.log('[OrchestratorAgent] Intent Gate: DIRECT 模式，直接执行');
        // 对于简单操作，仍然走任务分析但跳过确认
        return null;

      case IntentHandlerMode.TASK:
        // 任务模式：需要完整的任务分析和执行
        console.log('[OrchestratorAgent] Intent Gate: TASK 模式，进入任务分析');
        return null; // 继续走任务分析流程

      default:
        return null;
    }
  }

  /**
   * 问答模式：编排者 Claude 直接回答
   */
  private async executeAskMode(userPrompt: string, taskId: string): Promise<string> {
    this.setState('analyzing'); // 使用 analyzing 状态表示正在处理
    this.emitUIMessage('progress_update', '正在回答您的问题...');

    const context = this.contextManager?.getContext() || '';
    const prompt = context
      ? `请结合以下项目上下文回答用户问题。\n\n## 项目上下文\n${this.truncateContext(context)}\n\n## 用户问题\n${userPrompt}`
      : userPrompt;

    const response = await this.cliFactory.sendMessage(
      'claude',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: {
          taskId,
          intent: 'ask',
          contextSnapshot: this.buildContextSnapshot(),
        },
      }
    );

    this.recordOrchestratorTokens(response.tokenUsage);
    const content = response.content || '';

    await this.saveAndCompressMemory(content);
    this.setState('completed');
    this.currentContext!.endTime = Date.now();

    return content;
  }

  /**
   * 探索模式：分析代码库后回答
   */
  private async executeExploreMode(userPrompt: string, taskId: string): Promise<string> {
    this.setState('analyzing'); // 使用 analyzing 状态表示正在处理
    this.emitUIMessage('progress_update', '正在分析代码库...');

    const context = this.contextManager?.getContext() || '';
    const prompt = `你是一个代码分析专家。请分析以下项目上下文，然后回答用户的问题。

## 项目上下文
${this.truncateContext(context)}

## 用户问题
${userPrompt}

请提供详细的分析和解答。`;

    const response = await this.cliFactory.sendMessage(
      'claude',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: {
          taskId,
          intent: 'explore',
          contextSnapshot: this.buildContextSnapshot(),
        },
      }
    );

    this.recordOrchestratorTokens(response.tokenUsage);
    const content = response.content || '';

    await this.saveAndCompressMemory(content);
    this.setState('completed');
    this.currentContext!.endTime = Date.now();

    return content;
  }

  /**
   * 截断上下文（避免过长）
   */
  private truncateContext(context: string, maxChars: number = 6000): string {
    const trimmed = context.trim();
    if (trimmed.length <= maxChars) return trimmed;
    return trimmed.slice(0, maxChars) + '\n...（上下文已截断）';
  }

  // =========================================================================
  // Phase 1: 任务分析
  // =========================================================================

  /**
   * 分析任务，生成执行计划
   */
  private async analyzeTask(userPrompt: string): Promise<ExecutionPlan | null> {
    console.log('[OrchestratorAgent] Phase 1: 任务分析...');

    const ruleAnalysis = this.taskAnalyzer.analyze(userPrompt);
    this.syncCliAvailability();

    if (this.shouldUseRuleBasedPlan(ruleAnalysis)) {
      const plan = await this.buildPlanFromAnalysis(userPrompt, ruleAnalysis, false);
      if (plan) {
        if (plan.analysis) {
          this.emitUIMessage('progress_update', `需求分析: ${plan.analysis}`);
        }
        // 🔧 修复：不在 analyzeTask 中发送 plan_ready
        // plan_ready 只在 createPlan() 完成后发送（用于 /plan 命令）
        // 正常执行流程通过 confirmationRequest 请求确认
      }
      return plan;
    }

    const availableWorkers: WorkerType[] = ['claude', 'codex', 'gemini'];
    const projectContext = this.contextManager?.getContext() || '';
    const analysisPrompt = buildOrchestratorAnalysisPrompt(userPrompt, availableWorkers, projectContext || undefined);

    try {
      // 使用 Claude 进行分析（编排者专用会话）
      const response = await this.cliFactory.sendMessage(
        'claude',
        analysisPrompt,
        undefined,
        {
          source: 'orchestrator',
          streamToUI: true,
          adapterRole: 'orchestrator',
          messageMeta: {
            taskId: this.currentContext?.taskId,
            intent: 'orchestrator_analyze',
            contextSnapshot: this.buildContextSnapshot(),
          },
        }
      );

      this.recordOrchestratorTokens(response.tokenUsage);

      if (response.error) {
        console.error('[OrchestratorAgent] 分析失败:', response.error);
        return null;
      }

      const rawContent = response.content || '';
      const preview = rawContent.replace(/\s+/g, ' ').trim().slice(0, 200);
      console.log('[OrchestratorAgent] 计划解析输入预览:', {
        length: rawContent.length,
        preview,
      });

      let plan = this.parseExecutionPlan(response.content);
      if (plan && !this.validateExecutionPlan(plan)) {
        plan = null;
      }
      if (!plan) {
        plan = await this.buildPlanFromAnalysis(userPrompt, ruleAnalysis, true);
      }
      if (plan) {
        this.ensureArchitectureTask(plan, userPrompt);
        this.normalizeExecutionPlan(plan);
      }

      if (plan) {
        if (plan.analysis) {
          this.emitUIMessage('progress_update', `需求分析: ${plan.analysis}`);
        }
        // 🔧 修复：不在 analyzeTask 中发送 plan_ready
        // plan_ready 只在 createPlan() 完成后发送（用于 /plan 命令）
        // 正常执行流程通过 confirmationRequest 请求确认
      }

      return plan;
    } catch (error) {
      console.error('[OrchestratorAgent] 分析异常:', error);
      return null;
    }
  }

  private syncCliAvailability(): void {
    const statuses = this.cliFactory.getAllStatus();
    const idle = statuses.filter(status => status.connected && !status.busy).map(status => status.type);
    const connected = statuses.filter(status => status.connected).map(status => status.type);
    this.cliSelector.setAvailableCLIs(
      idle.length > 0 ? idle : (connected.length > 0 ? connected : ['claude', 'codex', 'gemini'])
    );
  }

  private shouldUseRuleBasedPlan(analysis: TaskAnalysis): boolean {
    // 问答类请求使用规则处理（直接回答）
    if (analysis.isQuestion) return true;
    if (analysis.splittable) return false;
    if (analysis.complexity > 2) return false;
    if (analysis.category === 'architecture') return false;
    return true;
  }

  private async buildPlanFromAnalysis(
    userPrompt: string,
    analysis: TaskAnalysis,
    allowAIDecompose: boolean
  ): Promise<ExecutionPlan> {
    // 问答类请求：不需要 Worker，编排者直接回答
    if (analysis.isQuestion) {
      console.log('[OrchestratorAgent] 检测到问答类请求，编排者将直接回答');
      return {
        id: `plan_${Date.now()}`,
        analysis: `问答类请求：${analysis.category}`,
        isSimpleTask: true,
        needsWorker: false,
        directResponse: '', // 留空，后续由 Claude 生成回答
        needsUserInput: false,
        questions: [],
        skipReason: '问答/咨询类请求，无需执行任务',
        needsCollaboration: false,
        subTasks: [],
        executionMode: 'sequential',
        summary: '问答类请求',
        featureContract: '',
        acceptanceCriteria: [],
        createdAt: Date.now(),
      };
    }

    const splitResult = await this.buildSplitResult(analysis, allowAIDecompose);
    const subTasks = this.mapSplitToSubTasks(splitResult.subTasks, analysis);
    const executionMode = splitResult.executionMode === 'sequential' ? 'sequential' : 'parallel';
    const needsCollaboration = subTasks.length > 1;

    return {
      id: `plan_${Date.now()}`,
      analysis: `规则分析：${analysis.category}，复杂度 ${analysis.complexity}/5`,
      isSimpleTask: subTasks.length <= 1,
      needsWorker: true,
      needsUserInput: false,
      questions: [],
      needsCollaboration,
      subTasks,
      executionMode,
      summary: `规则分析生成 ${subTasks.length} 个子任务`,
      featureContract: userPrompt,
      acceptanceCriteria: ['任务按要求完成'],
      createdAt: Date.now(),
    };
  }

  private async buildSplitResult(
    analysis: TaskAnalysis,
    allowAIDecompose: boolean
  ): Promise<SplitResult> {
    if (allowAIDecompose && this.aiTaskDecomposer.shouldUseAI(analysis)) {
      return await this.aiTaskDecomposer.decompose(analysis);
    }
    return this.taskSplitter.split(analysis);
  }

  private mapSplitToSubTasks(subTasks: SubTaskDef[], analysis: TaskAnalysis): SubTask[] {
    return subTasks.map((task, index) => ({
      id: task.id || String(index + 1),
      taskId: this.currentContext?.taskId || '',
      description: task.description,
      assignedWorker: task.assignedCli,
      assignedCli: task.assignedCli,
      reason: task.cliSelection?.reason || '规则分配',
      targetFiles: task.targetFiles || [],
      dependencies: task.dependencies || [],
      prompt: this.buildRuleBasedPrompt(analysis, task),
      priority: task.priority,
      kind: this.mapCategoryToKind(task.category),
      featureId: `feature_${this.currentContext?.taskId || 'unknown'}`,
      status: 'pending',
      output: [],
    }));
  }

  private buildRuleBasedPrompt(analysis: TaskAnalysis, task: SubTaskDef): string {
    const files = task.targetFiles || [];
    const fileHint = files.length > 0
      ? `目标文件: ${files.join(', ')}`
      : '目标文件: 未指定';

    return [
      `用户需求: ${analysis.prompt}`,
      `子任务: ${task.description}`,
      fileHint,
      '请直接修改文件完成该任务，并简要说明变更。',
    ].join('\n');
  }

  private mapCategoryToKind(category: TaskAnalysis['category']): SubTask['kind'] {
    if (category === 'architecture') return 'architecture';
    if (category === 'bugfix' || category === 'debug' || category === 'refactor') return 'repair';
    return 'implementation';
  }

  private buildAggregatedSummary(results: ExecutionResult[]): string {
    const aggregator = new ResultAggregator();
    const now = Date.now();
    const aggregated = aggregator.aggregate(results.map(result => ({
      subTaskId: result.subTaskId,
      cli: result.workerType,
      status: result.success ? 'completed' : 'failed',
      error: result.error,
      startTime: now - (result.duration || 0),
      endTime: now,
      duration: result.duration,
    })));
    return aggregated.summary;
  }

  private filterResultsForSummary(results: ExecutionResult[]): ExecutionResult[] {
    const unique = new Map<string, ExecutionResult>();
    for (const result of results) {
      unique.set(result.subTaskId, result);
    }

    const task = this.currentContext?.taskId
      ? this.taskManager?.getTask(this.currentContext.taskId)
      : null;

    return Array.from(unique.values()).filter(result => {
      if (result.subTaskId.startsWith('integration_')) {
        return false;
      }
      const kind = task?.subTasks.find(sub => sub.id === result.subTaskId)?.kind;
      return kind !== 'integration';
    });
  }

  private sanitizeSummaryText(content: string): string {
    const withoutFences = content.replace(/```[\s\S]*?```/g, '[代码块已省略]');
    const normalized = withoutFences.replace(/\n{3,}/g, '\n\n').trim();
    const blocks = normalized.split(/\n{2,}/);
    const seen = new Set<string>();
    const deduped: string[] = [];

    for (const block of blocks) {
      const trimmed = block.trim();
      if (!trimmed) continue;
      const key = trimmed
        .replace(/^#{1,6}\s*/gm, '')
        .replace(/\*\*/g, '')
        .replace(/\s+/g, ' ');
      if (seen.has(key)) continue;
      seen.add(key);
      deduped.push(trimmed);
    }

    const result = deduped.join('\n\n');
    const lines = result.split('\n');
    if (lines.length <= 40) {
      return result;
    }
    return `${lines.slice(0, 40).join('\n')}\n...(已省略)`;
  }

  private createBatchSubTasks(
    plan: ExecutionPlan,
    subTasks: SubTask[] = plan.subTasks
  ): { batchTasks: SubTask[]; batchedIds: Set<string> } {
    const batchedIds = new Set<string>();
    const batchTasks: SubTask[] = [];

    if (plan.executionMode !== 'parallel') {
      return { batchTasks, batchedIds };
    }

    const groups = new Map<WorkerType, SubTask[]>();
    for (const task of subTasks) {
      if (task.kind && task.kind !== 'implementation') continue;
      if (task.dependencies && task.dependencies.length > 0) continue;
      const list = groups.get(task.assignedWorker) ?? [];
      list.push(task);
      groups.set(task.assignedWorker, list);
    }

    for (const [worker, tasks] of groups.entries()) {
      if (tasks.length < 2) continue;
      const taskId = this.currentContext?.taskId || 'batch';
      const batchId = `batch_${taskId}_${worker}_${Date.now()}`;
      const targetFiles = Array.from(new Set(tasks.flatMap(t => t.targetFiles || [])));

      const batchSubTask: SubTask = {
        id: batchId,
        taskId: this.currentContext?.taskId || '',
        description: `批量执行 (${worker})`,
        assignedWorker: worker,
        reason: '同一 CLI 多任务批量执行',
        targetFiles,
        dependencies: [],
        conflictDomain: `batch:${worker}`,
        dependencyChain: [],
        prompt: this.buildBatchPrompt(tasks),
        priority: 1,
        status: 'pending',
        output: [],
        kind: 'batch',
        featureId: tasks[0]?.featureId || `feature_${taskId}`,
        batchItems: tasks.map(t => t.id),
      };

      tasks.forEach(t => batchedIds.add(t.id));
      batchTasks.push(batchSubTask);
      this.batchTasks.set(batchId, tasks);
    }

    return { batchTasks, batchedIds };
  }

  private buildBatchPrompt(tasks: SubTask[]): string {
    const lines: string[] = [
      '你需要按顺序完成以下任务：',
    ];

    tasks.forEach((task, index) => {
      lines.push(`${index + 1}. [ ] (${task.id}) ${task.description}`);
      if (task.targetFiles && task.targetFiles.length > 0) {
        lines.push(`   目标文件: ${task.targetFiles.join(', ')}`);
      }
      if (task.prompt) {
        lines.push(`   要求: ${task.prompt}`);
      }
    });

    lines.push('');
    lines.push('每完成一个任务后将 [ ] 改为 [x]，最后输出 "ALL_TASKS_COMPLETED"。');
    lines.push('如果无法完成某个任务，请说明原因并继续列出完成的项。');
    return lines.join('\n');
  }

  private markBatchStarted(tasks: SubTask[]): void {
    const taskId = this.currentContext?.taskId || '';
    for (const task of tasks) {
      this.taskManager?.updateSubTaskStatus(taskId, task.id, 'running');
      this.taskStateManager?.updateStatus(task.id, 'running');
    }
  }

  private parseBatchCompletion(content: string, taskIds: string[]): { completed: Set<string>; allCompleted: boolean } {
    const completed = new Set<string>();
    const regex = /\[\s*x\s*\]\s*\(([^)]+)\)/gi;
    let match = regex.exec(content);
    while (match) {
      completed.add(match[1]);
      match = regex.exec(content);
    }
    const allCompleted = content.includes('ALL_TASKS_COMPLETED') || completed.size === taskIds.length;
    return { completed, allCompleted };
  }

  private async finalizeBatchResult(batchTask: SubTask, result: ExecutionResult): Promise<void> {
    const tasks = this.batchTasks.get(batchTask.id) ?? [];
    this.batchTasks.delete(batchTask.id);

    const taskIds = tasks.map(t => t.id);
    const completion = this.parseBatchCompletion(result.result || '', taskIds);
    const outputSummary = (result.result || '').slice(0, 800);

    for (const task of tasks) {
      const success = result.success && (completion.allCompleted || completion.completed.has(task.id));
      const error = success ? undefined : (result.error || '批量执行未完成');
      const normalized: ExecutionResult = {
        workerId: result.workerId,
        workerType: result.workerType,
        taskId: task.taskId,
        subTaskId: task.id,
        result: success ? `批量执行完成。\n${outputSummary}` : `批量执行失败。\n${outputSummary}`,
        success,
        duration: result.duration,
        modifiedFiles: result.modifiedFiles,
        error,
      };
      this.recordResult(normalized);
    }

    this.pendingTasks.delete(batchTask.id);
  }

  private mapTaskStateStatus(status: TaskState['status']): SubTaskStatus {
    switch (status) {
      case 'running':
      case 'retrying':
        return 'running';
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'cancelled':
        return 'skipped';
      case 'pending':
      default:
        return 'pending';
    }
  }

  private applyTaskStateToTaskManager(taskState: TaskState): void {
    if (!this.taskManager) return;
    const task = this.taskManager.getTask(taskState.parentTaskId);
    if (!task) return;
    const mappedStatus = this.mapTaskStateStatus(taskState.status);
    const existing = task.subTasks.find(st => st.id === taskState.id);
    if (!existing) {
      const subTask: SubTask = {
        id: taskState.id,
        taskId: taskState.parentTaskId,
        description: taskState.description,
        assignedWorker: taskState.assignedCli,
        assignedCli: taskState.assignedCli,
        targetFiles: taskState.modifiedFiles ?? [],
        modifiedFiles: taskState.modifiedFiles ?? [],
        dependencies: [],
        status: mappedStatus,
        output: [],
        kind: 'implementation',
      };
      this.taskManager.addExistingSubTask(taskState.parentTaskId, subTask);
    } else if (taskState.modifiedFiles && taskState.modifiedFiles.length > 0) {
      this.taskManager.updateSubTaskFiles(
        taskState.parentTaskId,
        taskState.id,
        taskState.modifiedFiles
      );
    }
    this.taskManager.updateSubTaskStatus(taskState.parentTaskId, taskState.id, mappedStatus);
  }

  private replayTaskStatesToTaskManager(): void {
    if (!this.taskStateManager) return;
    for (const taskState of this.taskStateManager.getAllTasks()) {
      this.applyTaskStateToTaskManager(taskState);
    }
  }

  /**
   * 解析执行计划 JSON
   */
  private parseExecutionPlan(content: string): ExecutionPlan | null {
    try {
      const jsonCandidates = this.extractPlanJsonCandidates(content);
      const hasCandidate = jsonCandidates.some(candidate => candidate.trim().length > 0);
      if (!hasCandidate) {
        const preview = (content || '')
          .replace(/\s+/g, ' ')
          .trim()
          .slice(0, 200);
        console.warn(`[OrchestratorAgent] 执行计划内容为空或未找到 JSON 候选${preview ? `: ${preview}` : ''}`);
        return null;
      }
      const parsed = this.parsePlanJson(jsonCandidates);
      // parsePlanJson 现在返回 null 而不是抛出错误
      if (!parsed) {
        return null;
      }
      const rawSubTasks = this.extractSubTasks(parsed);

     
      const needsWorker = parsed.needsWorker !== false; // 默认为 true
      const directResponse = parsed.directResponse || '';
      const questions = Array.isArray(parsed.questions)
        ? parsed.questions.filter((q: unknown) => typeof q === 'string').map((q: string) => q.trim()).filter(Boolean)
        : [];
      const needsUserInput = parsed.needsUserInput === true || questions.length > 0;

      // 如果不需要 Worker，返回简化的计划
      if (!needsWorker && directResponse) {
        return {
          id: `plan_${Date.now()}`,
          analysis: parsed.analysis || '',
          isSimpleTask: true,
          needsWorker: false,
          directResponse,
          needsUserInput,
          questions,
          skipReason: parsed.skipReason || '编排者直接回答',
          needsCollaboration: false,
          subTasks: [],
          executionMode: 'sequential',
          summary: parsed.summary || directResponse,
          featureContract: '',
          acceptanceCriteria: [],
          createdAt: Date.now(),
        };
      }

      const featureContract = typeof parsed.featureContract === 'string'
        ? parsed.featureContract.trim()
        : '';
      const acceptanceCriteria = Array.isArray(parsed.acceptanceCriteria)
        ? parsed.acceptanceCriteria
          .filter((item: unknown) => typeof item === 'string')
          .map((item: string) => item.trim())
          .filter(Boolean)
        : [];

     
      // 如果缺少，使用默认值，而不是抛出错误
      const finalFeatureContract = featureContract || parsed.analysis || '完成用户请求的任务';
      const finalAcceptanceCriteria = acceptanceCriteria.length > 0
        ? acceptanceCriteria
        : ['任务按要求完成'];

      if (!featureContract && acceptanceCriteria.length === 0) {
        console.warn('[OrchestratorAgent] 执行计划缺少功能契约或验收清单，使用默认值');
      }

      return {
        id: `plan_${Date.now()}`,
        analysis: parsed.analysis || '',
        isSimpleTask: parsed.isSimpleTask || false,
        needsWorker: true,
        needsUserInput,
        questions,
        skipReason: parsed.skipReason,
        needsCollaboration: parsed.needsCollaboration ?? true,
        subTasks: rawSubTasks.map((t: any, i: number) => {
          const fallbackFiles = this.extractTargetFilesFromText(
            `${t?.description || ''}\n${t?.prompt || ''}`
          );
          const targetFiles: string[] = Array.isArray(t?.targetFiles) && t.targetFiles.length > 0
            ? (t.targetFiles as unknown[]).filter(
                (f: unknown): f is string => typeof f === 'string' && f.trim().length > 0
              )
            : fallbackFiles;
          const normalizedTargets = Array.from(new Set(targetFiles));
          return ({
          id: t.id || String(i + 1),
          taskId: this.currentContext?.taskId || '',
          description: t.description || '',
          assignedWorker: t.assignedWorker || t.assignedCli || 'claude',
          reason: t.reason || '',
          targetFiles: normalizedTargets,
          dependencies: t.dependencies || [],
          conflictDomain: typeof t.conflictDomain === 'string' ? t.conflictDomain : undefined,
          dependencyChain: Array.isArray(t.dependencyChain) ? t.dependencyChain : undefined,
          prompt: t.prompt || '',
          priority: t.priority,
          kind: t.kind || (t.background ? 'background' : 'implementation'),
          featureId: t.featureId || `feature_${this.currentContext?.taskId || 'unknown'}`,
          background: Boolean(t.background) || t.kind === 'background',
          status: 'pending',
          output: [],
        });
        }),
        executionMode: parsed.executionMode || 'parallel',
        summary: parsed.summary || '',
        featureContract: finalFeatureContract,
        acceptanceCriteria: finalAcceptanceCriteria,
        createdAt: Date.now(),
      };
    } catch (error) {
      console.error('[OrchestratorAgent] 解析执行计划失败:', error);
      return null;
    }
  }

  private extractSubTasks(parsed: any): any[] {
    if (!parsed) return [];
    if (Array.isArray(parsed.subTasks)) return parsed.subTasks;
    if (Array.isArray(parsed)) return parsed;
    return [];
  }

  private extractTargetFilesFromText(text: string): string[] {
    if (!text) return [];
    const filePattern = /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|scss|html|json|md|yaml|yml)/gi;
    const matches = text.match(filePattern);
    return matches ? Array.from(new Set(matches)) : [];
  }

  /** 确保全栈任务包含架构/契约任务（Claude） */
  private ensureArchitectureTask(plan: ExecutionPlan, userPrompt: string): void {
    if (!plan.subTasks || plan.subTasks.length === 0) {
      return;
    }

    const hasClaudeTask = plan.subTasks.some(t =>
      t.assignedWorker === 'claude' && (t.kind === 'architecture' || /架构|契约|系统|设计|框架/i.test(t.description))
    );

    const hasFrontend = plan.subTasks.some(t =>
      t.assignedWorker === 'gemini' || /前端|UI|界面|页面|组件/i.test(t.description)
    );

    const hasBackend = plan.subTasks.some(t =>
      t.assignedWorker === 'codex' || /后端|API|接口|服务|鉴权|数据库/i.test(t.description)
    );

    if (hasClaudeTask || !hasFrontend || !hasBackend) {
      return;
    }

    const taskId = this.currentContext?.taskId || '';
    const architectureTaskId = `arch-${Date.now()}`;
    const architecturePrompt = [
      '请先完成系统架构与契约设计，输出明确可执行的方案：',
      '1. 目录结构/模块边界',
      '2. 接口契约（请求/响应字段、状态码、错误码）',
      '3. 前后端对接约束（字段命名、校验规则、鉴权方式）',
      '4. 若缺失框架/基础结构，请先补齐框架骨架',
      '',
      `原始需求：${userPrompt}`
    ].join('\n');

    const architectureTask: SubTask = {
      id: architectureTaskId,
      taskId,
      description: '架构与契约设计（前后端统一框架）',
      assignedWorker: 'claude',
      reason: '需要统一前后端契约与框架，避免联调偏差',
      targetFiles: [],
      dependencies: [],
      prompt: architecturePrompt,
      priority: 0,
      kind: 'architecture',
      featureId: plan.subTasks[0]?.featureId || `feature_${taskId || 'unknown'}`,
      status: 'pending',
      output: [],
    };

    plan.subTasks.unshift(architectureTask);

    plan.subTasks.forEach(task => {
      if (task.id === architectureTaskId) return;
      if (!task.dependencies) {
        task.dependencies = [architectureTaskId];
        return;
      }
      if (!task.dependencies.includes(architectureTaskId)) {
        task.dependencies.push(architectureTaskId);
      }
    });
  }

  private validateExecutionPlan(plan: ExecutionPlan): boolean {
    if (!plan) return false;
    if (plan.needsWorker === false) {
      return typeof plan.directResponse === 'string' && plan.directResponse.trim().length > 0;
    }
    if (!Array.isArray(plan.subTasks) || plan.subTasks.length === 0) return false;
    if (!plan.executionMode) return false;
    return plan.subTasks.every(task => task && typeof task.description === 'string' && task.description.trim().length > 0);
  }

  private normalizeExecutionPlan(plan: ExecutionPlan): void {
    if (!plan.subTasks || plan.subTasks.length === 0) {
      return;
    }

    this.normalizePlanMetadata(plan);
    this.normalizeArchitectureKinds(plan);
    this.pruneClaudeImplementationForFullStack(plan);
    this.mergeLightCrossLayerTasks(plan);
    this.pruneMissingDependencies(plan);
    this.assignConflictDomains(plan);

    const hasDependencies = plan.subTasks.some(task => task.dependencies && task.dependencies.length > 0);
    const hasFileConflicts = this.hasFileConflicts(plan.subTasks);

    if (!hasDependencies && !hasFileConflicts && plan.executionMode === 'sequential') {
      plan.executionMode = 'parallel';
      this.emitUIMessage('progress_update', '执行模式已调整为并行（无依赖且无文件冲突）');
    }
  }

  private normalizePlanMetadata(plan: ExecutionPlan): void {
    const basePrompt = plan.featureContract || this.currentContext?.userPrompt || '';
    const analysis = basePrompt ? this.taskAnalyzer.analyze(basePrompt) : null;

    if (!plan.featureContract) {
      plan.featureContract = basePrompt;
    }
    if (!plan.analysis) {
      plan.analysis = analysis
        ? `规则分析：${analysis.category}，复杂度 ${analysis.complexity}/5`
        : '未提供分析结果';
    }
    if (!Array.isArray(plan.acceptanceCriteria) || plan.acceptanceCriteria.length === 0) {
      plan.acceptanceCriteria = this.deriveAcceptanceCriteria(plan);
    }
    if (!plan.summary) {
      plan.summary = plan.analysis || '任务计划已生成';
    }
    if (!plan.executionMode) {
      plan.executionMode = analysis?.suggestedMode ?? 'sequential';
    }
    if (plan.needsCollaboration === undefined) {
      plan.needsCollaboration = plan.subTasks.length > 1;
    }
  }

  private deriveAcceptanceCriteria(plan: ExecutionPlan): string[] {
    const criteria = new Set<string>();
    if (plan.featureContract) {
      criteria.add('满足功能契约');
    }
    for (const task of plan.subTasks || []) {
      if (task?.description) {
        criteria.add(`完成：${task.description}`);
      }
      if (criteria.size >= 6) break;
    }
    if (criteria.size === 0) {
      criteria.add('按要求完成任务');
    }
    return Array.from(criteria);
  }

  private mergeLightCrossLayerTasks(plan: ExecutionPlan): void {
    const risk = this.currentContext?.risk;
    if (!risk || risk.path !== 'light') return;
    if (!plan.subTasks || plan.subTasks.length !== 2) return;

    const [first, second] = plan.subTasks;
    if (!this.isSmallSubTask(first) || !this.isSmallSubTask(second)) return;

    const isFrontendPair = this.isFrontendTask(first) || this.isFrontendTask(second);
    const isBackendPair = this.isBackendTask(first) || this.isBackendTask(second);
    if (!isFrontendPair || !isBackendPair) return;

    const targetWorker = this.pickPrimaryWorker(first, second);
    if (first.assignedWorker === second.assignedWorker && first.assignedWorker === targetWorker) {
      return;
    }

    const mergedTargetFiles = Array.from(new Set([
      ...(first.targetFiles || []),
      ...(second.targetFiles || []),
    ]));
    const mergedDependencies = Array.from(new Set([
      ...(first.dependencies || []),
      ...(second.dependencies || []),
    ]));

    const mergedTask: SubTask = {
      id: `merge-${Date.now()}`,
      taskId: first.taskId || second.taskId || this.currentContext?.taskId || '',
      description: `轻量跨层合并任务：${first.description} / ${second.description}`,
      assignedWorker: targetWorker,
      reason: '轻量跨层改动合并执行，减少拆分成本',
      targetFiles: mergedTargetFiles,
      dependencies: mergedDependencies,
      prompt: [
        '以下为轻量跨层合并任务，请一次性完成：',
        '',
        `子任务1: ${first.description}`,
        first.prompt ? `要求:\n${first.prompt}` : '',
        '',
        `子任务2: ${second.description}`,
        second.prompt ? `要求:\n${second.prompt}` : '',
      ].filter(Boolean).join('\n'),
      priority: Math.min(first.priority ?? 0, second.priority ?? 0),
      kind: first.kind || second.kind || 'implementation',
      featureId: first.featureId || second.featureId || `feature_${this.currentContext?.taskId || 'unknown'}`,
      status: 'pending',
      output: [],
    };

    plan.subTasks = [mergedTask];
    plan.executionMode = 'sequential';
    this.emitUIMessage('progress_update', '已按轻量跨层规则合并前后端子任务');
  }

  private assignConflictDomains(plan: ExecutionPlan): void {
    for (const task of plan.subTasks) {
      task.dependencies = task.dependencies ?? [];
      task.dependencyChain = Array.from(new Set(task.dependencies));

      if (!task.conflictDomain || !task.conflictDomain.trim()) {
        const domains = this.resolveConflictDomains(task);
        task.conflictDomain = domains.length > 0 ? domains.join('|') : 'unknown';
      }
    }
  }

  private resolveConflictDomains(task: SubTask): string[] {
    const domains = new Set<string>();
    const files = (task.targetFiles || []).filter(Boolean);
    const configFiles = [
      'package.json',
      'package-lock.json',
      'pnpm-lock.yaml',
      'yarn.lock',
      'requirements.txt',
      'pyproject.toml',
      'Pipfile',
      'go.mod',
      'go.sum',
      'Cargo.toml',
      'Cargo.lock',
      'pom.xml',
      'build.gradle',
      'build.gradle.kts',
      'tsconfig.json',
      'vite.config',
      'webpack.config',
      'next.config',
    ];

    for (const file of files) {
      const normalized = file.replace(/\\/g, '/').replace(/^\.\//, '');
      const base = normalized.split('/').pop() || '';
      if (configFiles.some(cfg => base === cfg || base.startsWith(cfg))) {
        domains.add(`config:${base}`);
        continue;
      }

      const moduleMatch = normalized.match(/src\/modules\/([^/]+)/);
      if (moduleMatch?.[1]) {
        domains.add(`module:${moduleMatch[1]}`);
        continue;
      }

      const apiMatch = normalized.match(/src\/(api|routes|controllers?)\/([^/]+)/);
      if (apiMatch?.[2]) {
        domains.add(`api:${apiMatch[2]}`);
        continue;
      }

      if (normalized.startsWith('src/')) {
        const parts = normalized.split('/');
        const dir = parts.length >= 2 ? `${parts[0]}/${parts[1]}` : parts[0];
        domains.add(`dir:${dir}`);
        continue;
      }

      const top = normalized.split('/')[0];
      if (top) {
        domains.add(`dir:${top}`);
      }
    }

    if (domains.size === 0) {
      const text = `${task.description} ${task.prompt || ''}`;
      if (/前端|UI|界面|页面|组件/i.test(text)) {
        domains.add('frontend');
      }
      if (/后端|API|接口|服务|数据库|鉴权|认证/i.test(text)) {
        domains.add('backend');
      }
      if (/架构|契约|系统|设计|框架/i.test(text)) {
        domains.add('architecture');
      }
      if (/配置|依赖|构建|脚本/i.test(text)) {
        domains.add('config');
      }
    }

    return Array.from(domains.values());
  }

  private isSmallSubTask(task: SubTask): boolean {
    const desc = task.description || '';
    const prompt = task.prompt || '';
    const files = task.targetFiles || [];
    return desc.length <= 80 && prompt.length <= 400 && files.length <= 2;
  }

  private isFrontendTask(task: SubTask): boolean {
    return task.assignedWorker === 'gemini' || /前端|UI|界面|页面|组件|样式/i.test(task.description);
  }

  private isBackendTask(task: SubTask): boolean {
    return task.assignedWorker === 'codex' || /后端|API|接口|服务|鉴权|数据库/i.test(task.description);
  }

  private pickPrimaryWorker(a: SubTask, b: SubTask): WorkerType {
    if (this.isBackendTask(a) || this.isBackendTask(b)) {
      return 'codex';
    }
    if (this.isFrontendTask(a) || this.isFrontendTask(b)) {
      return 'gemini';
    }
    return 'claude';
  }

  private normalizeArchitectureKinds(plan: ExecutionPlan): void {
    plan.subTasks.forEach(task => {
      if (task.assignedWorker !== 'claude') return;
      if (task.kind === 'architecture') return;
      if (/^arch-/i.test(task.id) || /架构|契约|系统|设计|框架/i.test(task.description)) {
        task.kind = 'architecture';
      }
    });
  }

  private pruneClaudeImplementationForFullStack(plan: ExecutionPlan): void {
    const hasArchitecture = plan.subTasks.some(t =>
      t.assignedWorker === 'claude' && t.kind === 'architecture'
    );
    const hasFrontend = plan.subTasks.some(t =>
      t.assignedWorker === 'gemini' || /前端|UI|界面|页面|组件/i.test(t.description)
    );
    const hasBackend = plan.subTasks.some(t =>
      t.assignedWorker === 'codex' || /后端|API|接口|服务|鉴权|数据库/i.test(t.description)
    );

    if (!hasArchitecture || !hasFrontend || !hasBackend) return;

    const before = plan.subTasks.length;
    plan.subTasks = plan.subTasks.filter(t => {
      if (t.assignedWorker !== 'claude') return true;
      if (t.kind === 'architecture' || t.kind === 'integration' || t.kind === 'repair') return true;
      if (/架构|契约|系统|设计|框架/i.test(t.description)) return true;
      return false;
    });

    if (plan.subTasks.length !== before) {
      this.emitUIMessage('progress_update', '已移除冗余的 Claude 实现任务，保留架构/联调任务');
    }
  }

  private pruneMissingDependencies(plan: ExecutionPlan): void {
    const ids = new Set(plan.subTasks.map(t => t.id));
    plan.subTasks.forEach(task => {
      if (!task.dependencies) return;
      task.dependencies = task.dependencies.filter(dep => ids.has(dep));
    });
  }

  private hasFileConflicts(subTasks: SubTask[]): boolean {
    const fileToTasks = new Map<string, Set<string>>();
    for (const task of subTasks) {
      const files = this.collectTaskFiles(task);
      for (const file of files) {
        const set = fileToTasks.get(file) ?? new Set<string>();
        set.add(task.id);
        fileToTasks.set(file, set);
        if (set.size > 1) {
          return true;
        }
      }
    }
    return false;
  }

  private collectTaskFiles(subTask: SubTask): string[] {
    const targetFiles = (subTask.targetFiles || []).filter(Boolean);
    if (targetFiles.length > 0) {
      return targetFiles;
    }

    const text = `${subTask.description || ''}\n${subTask.prompt || ''}`;
    const matches = text.match(/[\\w./-]+\\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|html|json|md)/gi);
    if (!matches) {
      return [];
    }
    return [...new Set(matches)];
  }

  /**
   * 从原始内容中提取可能的 JSON 计划文本
   */
  private extractPlanJsonCandidates(content: string): string[] {
    const candidates: string[] = [];
    if (!content) return candidates;

    const fenced = content.match(/```json\s*([\s\S]*?)\s*```/i);
    if (fenced?.[1]) {
      candidates.push(fenced[1].trim());
    }

    const anyFence = content.match(/```\s*([\s\S]*?)\s*```/);
    if (anyFence?.[1]) {
      const trimmed = anyFence[1].trim();
      if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
        candidates.push(trimmed);
      }
    }

    const objectJson = this.extractBalancedJson(content, '{', '}');
    if (objectJson) {
      candidates.push(objectJson);
    }

    const arrayJson = this.extractBalancedJson(content, '[', ']');
    if (arrayJson) {
      candidates.push(arrayJson);
    }

    if (candidates.length === 0) {
      candidates.push(content.trim());
    }

    return candidates;
  }

  /**
   * 从文本中提取第一个平衡的 JSON 结构（支持字符串和转义）
   */
  private extractBalancedJson(content: string, openChar: '{' | '[', closeChar: '}' | ']'): string | null {
    const start = content.indexOf(openChar);
    if (start === -1) return null;

    let depth = 0;
    let inString = false;
    let escaping = false;

    for (let i = start; i < content.length; i += 1) {
      const ch = content[i];

      if (escaping) {
        escaping = false;
        continue;
      }

      if (ch === '\\') {
        if (inString) escaping = true;
        continue;
      }

      if (ch === '"') {
        inString = !inString;
        continue;
      }

      if (inString) continue;

      if (ch === openChar) depth += 1;
      if (ch === closeChar) depth -= 1;

      if (depth === 0) {
        return content.slice(start, i + 1).trim();
      }
    }

    return null;
  }

  /**
   * 解析 JSON（支持去除尾随逗号的容错）
   * 返回 null 而不是抛出错误，让调用方使用 fallback 逻辑
   */
  private parsePlanJson(candidates: string[]): any {
    const errors: string[] = [];
    for (const raw of candidates) {
      const trimmed = raw.trim();
      if (!trimmed) continue;
      try {
        const parsed = JSON.parse(trimmed);
        if (this.isLikelyPlanJson(parsed)) {
          return parsed;
        }
        errors.push('解析结果不包含执行计划结构');
      } catch (error) {
        const cleaned = trimmed.replace(/,\s*([}\]])/g, '$1');
        try {
          const parsed = JSON.parse(cleaned);
          if (this.isLikelyPlanJson(parsed)) {
            return parsed;
          }
          errors.push('解析结果不包含执行计划结构');
        } catch (retryError) {
          const sanitized = this.sanitizePlanJson(cleaned);
          try {
            const parsed = JSON.parse(sanitized);
            if (this.isLikelyPlanJson(parsed)) {
              return parsed;
            }
            errors.push('解析结果不包含执行计划结构');
          } catch {
            const message = error instanceof Error ? error.message : String(error);
            errors.push(message);
          }
          continue;
        }
      }
    }

    // 返回 null 而不是抛出错误，让调用方使用 fallback 逻辑
    console.warn('[OrchestratorAgent] 无法解析执行计划 JSON:', errors.join(' | '));
    return null;
  }

  private isLikelyPlanJson(parsed: any): boolean {
    if (!parsed) return false;
    if (Array.isArray(parsed)) {
      return parsed.length > 0 && typeof parsed[0] === 'object';
    }
    if (typeof parsed !== 'object') return false;
    return (
      'subTasks' in parsed ||
      'analysis' in parsed ||
      'needsWorker' in parsed ||
      'needsUserInput' in parsed ||
      'questions' in parsed ||
      'directResponse' in parsed ||
      'summary' in parsed
    );
  }

  private sanitizePlanJson(raw: string): string {
    let result = '';
    let inString = false;
    let escaped = false;
    for (const char of raw) {
      if (escaped) {
        result += char;
        escaped = false;
        continue;
      }
      if (char === '\\') {
        result += char;
        escaped = true;
        continue;
      }
      if (char === '"') {
        inString = !inString;
        result += char;
        continue;
      }
      if (inString) {
        if (char === '\n') {
          result += '\\n';
          continue;
        }
        if (char === '\r') {
          result += '\\n';
          continue;
        }
        if (char === '\t') {
          result += '\\t';
          continue;
        }
      }
      result += char;
    }
    return result;
  }

  // =========================================================================
  // Phase 1.5: 等待用户补充信息
  // =========================================================================

  private async waitForUserInput(plan: ExecutionPlan): Promise<string | null> {
    const questions = (plan.questions || []).map(q => String(q || '').trim()).filter(Boolean);
    if (questions.length === 0) return null;
    if (!this.questionCallback) {
      console.log('[OrchestratorAgent] 未设置问题回调，无法等待用户补充');
      return null;
    }

    this.emitUIMessage('progress_update', '等待用户补充关键信息...');
    try {
      const answer = await this.questionCallback(questions, plan);
      const normalized = typeof answer === 'string' ? answer.trim() : '';
      return normalized || null;
    } catch (error) {
      console.error('[OrchestratorAgent] 等待用户补充异常:', error);
      return null;
    }
  }

  // =========================================================================
  // Phase 2: 等待用户确认
  // =========================================================================

  /**
   * 等待用户确认执行计划
   */
  private async waitForConfirmation(plan: ExecutionPlan): Promise<boolean> {
    if (!this.confirmationCallback) {
      console.log('[OrchestratorAgent] 未设置确认回调，自动确认');
      return true;
    }

    const formattedPlan = formatPlanForUser(plan);

    globalEventBus.emitEvent('orchestrator:waiting_confirmation', {
      taskId: this.currentContext?.taskId,
      data: { plan, formattedPlan },
    });

    try {
      const confirmed = await this.confirmationCallback(plan, formattedPlan);
      console.log(`[OrchestratorAgent] 用户确认结果: ${confirmed ? 'Y' : 'N'}`);
      return confirmed;
    } catch (error) {
      console.error('[OrchestratorAgent] 等待确认异常:', error);
      return false;
    }
  }


  // =========================================================================
  // Phase 3: 分发任务
  // =========================================================================

  private isBackgroundSubTask(subTask?: SubTask | null): boolean {
    return Boolean(subTask?.background || subTask?.kind === 'background');
  }

  private getForegroundSubTasks(plan: ExecutionPlan): SubTask[] {
    return (plan.subTasks || []).filter(task => !this.isBackgroundSubTask(task));
  }

  private getBackgroundSubTasks(plan: ExecutionPlan): SubTask[] {
    return (plan.subTasks || []).filter(task => this.isBackgroundSubTask(task));
  }

  private dispatchBackgroundTasks(subTasks: SubTask[], plan: ExecutionPlan): void {
    if (!this.currentContext || subTasks.length === 0) {
      return;
    }

    const taskId = this.currentContext.taskId;
    const backgroundPriority = 10;

    for (const subTask of subTasks) {
      this.backgroundTasks.set(subTask.id, subTask);
      this.emitUIMessage(
        'progress_update',
        `后台任务排队: ${subTask.description}`,
        { subTaskId: subTask.id, workerType: subTask.assignedWorker }
      );

      const context = this.buildWorkerContext(plan, subTask);
      const promise = this.workerPool.dispatchTaskWithRetry(
        subTask.assignedWorker,
        taskId,
        subTask,
        context,
        { priority: backgroundPriority }
      ).then(result => this.finalizeResult(result)).catch(error => {
        const failedResult: ExecutionResult = {
          workerId: 'unknown',
          workerType: subTask.assignedWorker,
          taskId,
          subTaskId: subTask.id,
          result: '',
          success: false,
          duration: 0,
          error: error instanceof Error ? error.message : String(error),
        };
        return this.finalizeResult(failedResult);
      }).finally(() => {
        this.backgroundFinalizations.delete(subTask.id);
      });

      this.backgroundFinalizations.set(subTask.id, promise);
    }
  }

  private async waitForBackgroundTasks(timeoutMs = 15000): Promise<void> {
    if (this.abortController?.signal.aborted) {
      return;
    }
    const pending = Array.from(this.backgroundFinalizations.values());
    if (pending.length === 0) {
      return;
    }

    const timeout = Math.max(1000, timeoutMs);
    await Promise.race([
      Promise.allSettled(pending).then(() => undefined),
      new Promise<void>(resolve => setTimeout(resolve, timeout)),
    ]);

    if (this.backgroundFinalizations.size > 0) {
      this.emitUIMessage('progress_update', '后台任务仍在执行，先进入汇总阶段');
    }
  }

  /** 分发任务给 Worker */
  private async dispatchTasks(plan: ExecutionPlan): Promise<void> {
    console.log('[OrchestratorAgent] Phase 3: 分发任务...');

    this.syncPlanToTaskManager(plan);

    // 在执行前创建文件快照（支持回滚）
    await this.createSnapshotsForPlan(plan);

    const foregroundSubTasks = this.getForegroundSubTasks(plan);
    const backgroundSubTasks = this.getBackgroundSubTasks(plan);

    // 🆕 为任务分配冲突域（使用 plan 而非 subTasks）
    this.assignConflictDomains(plan);

    // 🆕 检测文件冲突
    const conflictResult = this.policyEngine
      ? this.policyEngine.detectConflicts(foregroundSubTasks)
      : { hasConflict: false, conflictingFiles: [], conflictingTasks: [] };
    if (conflictResult.hasConflict) {
      console.log('[OrchestratorAgent] 检测到文件冲突:', conflictResult.conflictingFiles);
      this.emitUIMessage('progress_update',
        `检测到 ${conflictResult.conflictingFiles.length} 个文件存在冲突，将串行执行相关任务`
      );
    }

    for (const subTask of foregroundSubTasks) {
      this.pendingTasks.set(subTask.id, subTask);
    }


    const hasDependencies = foregroundSubTasks.some(
      t => t.dependencies && t.dependencies.length > 0
    );

    const { batchTasks, batchedIds } = hasDependencies
      ? { batchTasks: [], batchedIds: new Set<string>() }
      : this.createBatchSubTasks(plan, foregroundSubTasks);

    if (batchTasks.length > 0) {
      for (const batchTask of batchTasks) {
        this.pendingTasks.set(batchTask.id, batchTask);
      }
    }

    const dispatchSubTasks = batchTasks.length > 0
      ? foregroundSubTasks.filter(task => !batchedIds.has(task.id)).concat(batchTasks)
      : foregroundSubTasks;

    this.dispatchBackgroundTasks(backgroundSubTasks, plan);

    if (hasDependencies) {
      // 使用依赖图调度执行
      console.log('[OrchestratorAgent] 检测到任务依赖，使用依赖图调度');
      if (!this.warnedReviewSkipForDependencies && this.shouldEnableReviews()) {
        this.emitUIMessage(
          'progress_update',
          '检测到任务依赖，子任务自检/互检在依赖图模式下暂不启用'
        );
        this.warnedReviewSkipForDependencies = true;
      }
      await this.dispatchWithDependencyGraph(plan, foregroundSubTasks);
    } else if (conflictResult.hasConflict) {
      // 🆕 有冲突时使用智能调度策略
      console.log('[OrchestratorAgent] 检测到文件冲突，使用智能调度策略');
      await this.dispatchWithConflictAwareness(dispatchSubTasks, plan, conflictResult);
    } else if (plan.executionMode === 'parallel') {
      await this.dispatchParallel(dispatchSubTasks, plan);
    } else {
      await this.dispatchSequential(dispatchSubTasks, plan);
    }

  }

  /** 🆕 冲突感知的任务分发 */
  private async dispatchWithConflictAwareness(
    subTasks: SubTask[],
    plan: ExecutionPlan,
    conflictResult: ConflictDetectionResult
  ): Promise<void> {
    const taskId = this.currentContext!.taskId;

    // 使用 PolicyEngine 决定执行策略
    const strategy = this.policyEngine
      ? this.policyEngine.decideExecutionStrategy(subTasks)
      : { parallel: [], serial: [], reason: 'PolicyEngine 未初始化' };

    // 先并行执行无冲突的任务
    if (strategy.parallel.length > 0) {
      const parallelTaskIds = new Set(strategy.parallel.flat());
      const parallelTasks = subTasks.filter(t => parallelTaskIds.has(t.id));

      if (parallelTasks.length > 0) {
        console.log(`[OrchestratorAgent] 并行执行 ${parallelTasks.length} 个无冲突任务`);
        this.emitUIMessage('progress_update',
          `并行执行 ${parallelTasks.length} 个无冲突任务`
        );
        await this.dispatchParallel(parallelTasks, plan);
      }
    }

    // 然后串行执行有冲突的任务
    if (strategy.serial.length > 0) {
      const serialTaskIds = new Set(strategy.serial);
      const serialTasks = subTasks.filter(t => serialTaskIds.has(t.id));

      if (serialTasks.length > 0) {
        console.log(`[OrchestratorAgent] 串行执行 ${serialTasks.length} 个冲突任务`);
        this.emitUIMessage('progress_update',
          `串行执行 ${serialTasks.length} 个存在文件冲突的任务`
        );

        // 按建议顺序排序
        const orderedTasks = conflictResult.suggestedOrder
          ? strategy.serial.map((id: string) => serialTasks.find(t => t.id === id)).filter(Boolean) as SubTask[]
          : serialTasks;

        await this.dispatchSequential(orderedTasks, plan);
      }
    }
  }

  /** 基于依赖图分发任务 */
  private async dispatchWithDependencyGraph(plan: ExecutionPlan, subTasks: SubTask[]): Promise<void> {
    this.emitUIMessage('progress_update', '正在分析任务依赖关系...');

    try {
      const results = await this.workerPool.executeWithDependencyGraph(
        this.currentContext!.taskId,
        subTasks,
        (subTask) => this.buildWorkerContext(plan, subTask)
      );

      // 处理执行结果
      for (const result of results) {
        await this.finalizeResult(result);
      }

      const successCount = results.filter(r => r.success).length;
      const failCount = results.filter(r => !r.success).length;
      console.log(`[OrchestratorAgent] 依赖图执行完成: ${successCount} 成功, ${failCount} 失败`);

    } catch (error) {
      console.error('[OrchestratorAgent] 依赖图执行失败:', error);
      this.emitUIMessage('error', `任务执行失败: ${error instanceof Error ? error.message : String(error)}`);
      throw error;
    }
  }

  /** 为执行计划中的目标文件创建快照 */
  private async createSnapshotsForPlan(plan: ExecutionPlan): Promise<void> {
    await this.createSnapshotsForSubTasks(plan.subTasks);
  }

  /** 为子任务集合创建快照 */
  private async createSnapshotsForSubTasks(subTasks: SubTask[]): Promise<void> {
    if (!this.snapshotManager) {
      console.log('[OrchestratorAgent] 未配置 SnapshotManager，跳过快照创建');
      return;
    }

    // 构建文件到 Worker 的映射，记录每个文件由哪个 Worker 负责
    const fileToWorker = new Map<string, WorkerType>();
    for (const subTask of subTasks) {
      if (subTask.targetFiles) {
        const worker = subTask.assignedWorker || 'claude';
        subTask.targetFiles.forEach(f => {
          // 如果文件已被其他 Worker 关联，保留第一个（或可以选择覆盖）
          if (!fileToWorker.has(f)) {
            fileToWorker.set(f, worker);
          }
        });
      }
    }

    if (fileToWorker.size === 0) {
      console.log('[OrchestratorAgent] 没有目标文件，跳过快照创建');
      return;
    }

    console.log(`[OrchestratorAgent] 为 ${fileToWorker.size} 个文件创建快照...`);
    this.emitUIMessage('progress_update', `正在为 ${fileToWorker.size} 个文件创建快照...`);

    for (const [filePath, worker] of fileToWorker) {
      try {
        this.snapshotManager.createSnapshot(
          filePath,
          worker, // 使用实际分配的 Worker
          this.currentContext?.taskId || 'unknown'
        );
      } catch (error) {
        console.warn(`[OrchestratorAgent] 创建快照失败: ${filePath}`, error);
      }
    }
  }

  /** 构建共享上下文（功能契约 + 验收清单） */
  private buildSharedContext(plan: ExecutionPlan): string {
    const criteria = (plan.acceptanceCriteria || []).map(item => `- ${item}`).join('\n');
    return [
      '功能契约:',
      plan.featureContract,
      '',
      '验收清单:',
      criteria || '- 未提供',
    ].join('\n');
  }

  private resolveContextConfig(): Required<NonNullable<OrchestratorConfig['context']>> {
    const config = this.config.context || {};
    return {
      workerMaxTokens: config.workerMaxTokens ?? 1200,
      workerMemoryRatio: config.workerMemoryRatio ?? 0.35,
      workerHighRiskExtraTokens: config.workerHighRiskExtraTokens ?? 600,
    };
  }

  private resolvePermissions(): PermissionMatrix {
    return {
      allowEdit: this.config.permissions?.allowEdit ?? true,
      allowBash: this.config.permissions?.allowBash ?? true,
      allowWeb: this.config.permissions?.allowWeb ?? true,
    };
  }

  private resolveStrategyConfig(): StrategyConfig {
    return {
      enableVerification: this.config.strategy?.enableVerification ?? true,
      enableRecovery: this.config.strategy?.enableRecovery ?? true,
      autoRollbackOnFailure: this.config.strategy?.autoRollbackOnFailure ?? false,
    };
  }

  private isHighRiskSubTask(subTask: SubTask): boolean {
    const reviewConfig = this.resolveReviewConfig();
    const extensions = (reviewConfig?.highRiskExtensions ?? DEFAULT_REVIEW_CONFIG.highRiskExtensions)
      .map(ext => ext.toLowerCase());
    const keywords = (reviewConfig?.highRiskKeywords ?? DEFAULT_REVIEW_CONFIG.highRiskKeywords)
      .map(keyword => keyword.toLowerCase());

    const text = `${subTask.description} ${subTask.prompt || ''}`.toLowerCase();
    const keywordHit = keywords.some(keyword => keyword && text.includes(keyword));
    if (keywordHit) {
      return true;
    }

    return (subTask.targetFiles || []).some(file => {
      const lower = file.toLowerCase();
      return extensions.some(ext => lower.endsWith(ext));
    });
  }

  private buildWorkerContext(plan: ExecutionPlan, subTask: SubTask): string {
    const sharedContext = this.buildSharedContext(plan);
    const config = this.resolveContextConfig();
    const fileCount = subTask.targetFiles?.length ?? 0;
    const riskBoost = this.isHighRiskSubTask(subTask) ? config.workerHighRiskExtraTokens : 0;
    const fileBoost = Math.min(600, fileCount * 200);
    const maxTokens = config.workerMaxTokens + riskBoost + fileBoost;
    const memoryRatio = fileCount <= 1 ? Math.max(0.2, config.workerMemoryRatio - 0.1) : config.workerMemoryRatio;

    const contextSlice = this.contextManager?.getContextSlice({
      maxTokens,
      memoryRatio,
      memorySummary: {
        includeKeyDecisions: 2,
        includeImportantContext: true,
        includePendingIssues: true,
        includeCompletedTasks: 2,
        includeCodeChanges: 2,
      },
    }) ?? '';

    const taskHint = [
      '任务信息:',
      `子任务: ${subTask.description}`,
      subTask.dependencies?.length ? `依赖: ${subTask.dependencies.join(', ')}` : '',
      subTask.targetFiles?.length ? `目标文件: ${subTask.targetFiles.join(', ')}` : '',
    ].filter(Boolean).join('\n');

    const trimmedContext = contextSlice.trim();
    return [sharedContext, taskHint, trimmedContext].filter(Boolean).join('\n\n');
  }

  private buildIntegrationPrompt(plan: ExecutionPlan): string {
    return [
      '你是集成联调负责人，请基于功能契约与验收清单审查当前实现。',
      '只做分析，不修改任何文件。',
      '',
      '输出 JSON 格式：',
      '{"status":"passed|failed","summary":"整体结论","issues":[{"title":"问题标题","detail":"问题详情","area":"backend|frontend|architecture|api|data|other","targetFiles":["可能涉及的文件"],"suggestedWorker":"claude|codex|gemini","fixPrompt":"修复指令"}]}',
      '只输出 JSON。',
      '',
      '功能契约:',
      plan.featureContract,
      '',
      '验收清单:',
      (plan.acceptanceCriteria || []).map(item => `- ${item}`).join('\n') || '- 未提供',
    ].join('\n');
  }

  private buildIntegrationContext(plan: ExecutionPlan, results: ExecutionResult[], round: number): string {
    const summaries = results.map(result => {
      const files = result.modifiedFiles?.length ? result.modifiedFiles.join(', ') : '无';
      const output = result.result ? result.result.slice(0, 1200) : '';
      return [
        `子任务: ${result.subTaskId} (${result.workerType})`,
        `结果: ${result.success ? '成功' : '失败'}`,
        `修改文件: ${files}`,
        output ? `输出摘要:\n${output}` : ''
      ].filter(Boolean).join('\n');
    }).join('\n\n');

    return [
      `联调轮次: ${round}`,
      this.buildSharedContext(plan),
      '',
      '子任务执行摘要:',
      summaries || '暂无执行结果',
    ].join('\n');
  }

  private parseIntegrationReport(content: string): IntegrationReport {
    const jsonMatch = content.match(/\{[\s\S]*\}/);
    const raw = jsonMatch ? jsonMatch[0] : content;
    try {
      const parsed = JSON.parse(raw);
      const status = parsed.status === 'passed' ? 'passed' : 'failed';
      const summary = typeof parsed.summary === 'string' ? parsed.summary : '未提供总结';
      const issues = Array.isArray(parsed.issues)
        ? parsed.issues.map((issue: any) => ({
          title: typeof issue.title === 'string' ? issue.title : undefined,
          detail: typeof issue.detail === 'string' ? issue.detail : undefined,
          area: typeof issue.area === 'string' ? issue.area : undefined,
          targetFiles: Array.isArray(issue.targetFiles) ? issue.targetFiles.filter((f: any) => typeof f === 'string') : undefined,
          suggestedWorker: ['claude', 'codex', 'gemini'].includes(issue.suggestedWorker) ? issue.suggestedWorker : undefined,
          fixPrompt: typeof issue.fixPrompt === 'string' ? issue.fixPrompt : undefined,
        })) : [];
      return { status, summary, issues };
    } catch (error) {
      return {
        status: 'failed',
        summary: '集成报告解析失败',
        issues: [],
      };
    }
  }

  private pickWorkerForIssue(issue: IntegrationIssue): WorkerType {
    if (issue.suggestedWorker) {
      return issue.suggestedWorker;
    }
    const area = (issue.area || '').toLowerCase();
    if (area === 'frontend') return 'gemini';
    if (area === 'backend' || area === 'api' || area === 'data') return 'codex';
    if (area === 'architecture') return 'claude';
    return 'claude';
  }

  private buildRepairPrompt(plan: ExecutionPlan, issue: IntegrationIssue): string {
    const detail = issue.detail || issue.title || '集成问题';
    return [
      '修复集成问题：',
      detail,
      '',
      '请遵守功能契约与验收清单。',
      '',
      '功能契约:',
      plan.featureContract,
      '',
      '验收清单:',
      (plan.acceptanceCriteria || []).map(item => `- ${item}`).join('\n') || '- 未提供',
    ].join('\n');
  }

  private async runIntegrationStage(plan: ExecutionPlan): Promise<void> {
    const integrationConfig = this.resolveIntegrationConfig();
    if (!integrationConfig.enabled || !this.currentContext) {
      return;
    }
    const foregroundSubTasks = this.getForegroundSubTasks(plan);
    if (foregroundSubTasks.length === 0) {
      return;
    }
    const riskPath = this.currentContext.risk?.path;
    const needsIntegration = riskPath === 'full'
      || (riskPath === 'standard' && (plan.needsCollaboration || foregroundSubTasks.length > 1))
      || (riskPath === undefined && foregroundSubTasks.length > 1);
    if (!needsIntegration) {
      this.emitUIMessage('progress_update', '联调阶段已跳过（风险路径较低或单任务）');
      return;
    }

    const featureId = plan.subTasks[0]?.featureId || `feature_${this.currentContext.taskId}`;
    const taskId = this.currentContext.taskId;
    const maxRounds = Math.max(1, integrationConfig.maxRounds);
    const sharedContext = this.buildSharedContext(plan);
    let dependencyIds = foregroundSubTasks.map(task => task.id);

    for (let round = 1; round <= maxRounds; round += 1) {
      this.setState('integrating');
      this.emitUIMessage('progress_update', `开始第 ${round} 轮联调检查...`);

      const integrationSubTask: SubTask = {
        id: `integration_${taskId}_${round}`,
        taskId,
        description: `功能联调检查（第 ${round} 轮）`,
        assignedWorker: integrationConfig.worker,
        reason: '联调收敛与验收',
        targetFiles: [],
        dependencies: [...dependencyIds],
        conflictDomain: 'integration',
        dependencyChain: [...dependencyIds],
        prompt: this.buildIntegrationPrompt(plan),
        priority: 1,
        status: 'pending',
        output: [],
        kind: 'integration',
        featureId,
      };

      this.pendingTasks.set(integrationSubTask.id, integrationSubTask);
      this.taskManager?.addExistingSubTask(taskId, integrationSubTask);
      if (this.taskStateManager && !this.taskStateManager.getTask(integrationSubTask.id)) {
        this.taskStateManager.createTask({
          id: integrationSubTask.id,
          parentTaskId: taskId,
          description: integrationSubTask.description,
          assignedCli: integrationSubTask.assignedWorker,
          maxAttempts: this.config.maxRetries,
        });
      }
      globalEventBus.emitEvent('task:created', { taskId });

      const integrationContext = this.buildIntegrationContext(plan, this.completedResults, round);
      const integrationResult = await this.workerPool.dispatchTaskWithRetry(
        integrationSubTask.assignedWorker,
        taskId,
        integrationSubTask,
        integrationContext || sharedContext
      );
      const finalIntegrationResult = await this.finalizeResult(integrationResult);
      const report = this.parseIntegrationReport(finalIntegrationResult?.result || integrationResult.result || '');
      this.lastIntegrationSummary = report.summary;

      if (report.status === 'passed') {
        this.emitUIMessage('progress_update', `[通过] 联调通过: ${report.summary}`);
        return;
      }

      this.emitUIMessage('error', `[失败] 联调未通过: ${report.summary}`);

      if (report.issues.length === 0) {
        report.issues.push({
          title: '联调问题待修复',
          detail: report.summary || '联调未通过，请根据联调结论修复问题',
          area: 'other',
          suggestedWorker: integrationConfig.worker,
          fixPrompt: this.buildRepairPrompt(plan, {
            detail: report.summary || '联调未通过，请根据联调结论修复问题',
            area: 'other',
          }),
        });
      }

      const repairTasks: SubTask[] = report.issues.map((issue, index) => {
        const worker = this.pickWorkerForIssue(issue);
        const task: SubTask = {
          id: `repair_${taskId}_${round}_${index + 1}`,
          taskId,
          description: `修复联调问题: ${issue.title || issue.area || '问题'}`,
          assignedWorker: worker,
          reason: '联调修复',
          targetFiles: issue.targetFiles || [],
          dependencies: [...dependencyIds],
          dependencyChain: [...dependencyIds],
          prompt: issue.fixPrompt || this.buildRepairPrompt(plan, issue),
          priority: 2,
          status: 'pending',
          output: [],
          kind: 'repair',
          featureId,
        };
        if (!task.conflictDomain) {
          const domains = this.resolveConflictDomains(task);
          task.conflictDomain = domains.length > 0 ? domains.join('|') : 'unknown';
        }
        return task;
      });

      if (repairTasks.length === 0) {
        throw new Error('联调未通过但未生成修复任务');
      }

      await this.createSnapshotsForSubTasks(repairTasks);
      for (const repairTask of repairTasks) {
        this.pendingTasks.set(repairTask.id, repairTask);
        this.taskManager?.addExistingSubTask(taskId, repairTask);
        if (this.taskStateManager && !this.taskStateManager.getTask(repairTask.id)) {
          this.taskStateManager.createTask({
            id: repairTask.id,
            parentTaskId: taskId,
            description: repairTask.description,
            assignedCli: repairTask.assignedWorker,
            maxAttempts: this.config.maxRetries,
          });
        }
      }
      globalEventBus.emitEvent('task:created', { taskId });

      await this.dispatchSequential(repairTasks, plan, [
        sharedContext,
        '',
        '联调问题摘要:',
        report.summary,
      ].join('\n'));

      dependencyIds = dependencyIds.concat(repairTasks.map(task => task.id));
    }

    throw new Error('联调多轮未通过');
  }

  /** 将执行计划同步到 TaskManager */
  private syncPlanToTaskManager(plan: ExecutionPlan): void {
    if (!this.taskManager || !this.currentContext) return;

    this.taskManager.updateTask(this.currentContext.taskId, {
      featureContract: plan.featureContract,
      acceptanceCriteria: plan.acceptanceCriteria,
    });

    for (const subTask of plan.subTasks) {
      try {
        this.taskManager.addExistingSubTask(this.currentContext.taskId, subTask);
        if (this.taskStateManager && !this.taskStateManager.getTask(subTask.id)) {
          this.taskStateManager.createTask({
            id: subTask.id,
            parentTaskId: this.currentContext.taskId,
            description: subTask.description,
            assignedCli: subTask.assignedWorker,
            maxAttempts: this.config.maxRetries,
          });
        }
      } catch (error) {
        console.warn('[OrchestratorAgent] 同步子任务失败:', error);
      }
    }

    globalEventBus.emitEvent('task:created', { taskId: this.currentContext.taskId });
  }

  /** 并行分发任务 */
  private async dispatchParallel(subTasks: SubTask[], plan: ExecutionPlan, contextOverride?: string): Promise<void> {
    const taskId = this.currentContext!.taskId;
    for (const subTask of subTasks) {
      if (subTask.kind === 'batch' && subTask.batchItems?.length) {
        const tasks = this.batchTasks.get(subTask.id) ?? [];
        this.markBatchStarted(tasks);
      }
      this.emitUIMessage('progress_update',
        `分发任务给 ${subTask.assignedWorker}: ${subTask.description}`,
        { subTaskId: subTask.id, workerType: subTask.assignedWorker }
      );

      const context = contextOverride ?? this.buildWorkerContext(plan, subTask);
      void this.workerPool.dispatchTaskWithRetry(
        subTask.assignedWorker,
        taskId,
        subTask,
        context
      ).then(result => {
        void this.finalizeResult(result);
      }).catch(error => {
        console.error(`[OrchestratorAgent] 并行任务分发失败:`, error);
        const failedResult: ExecutionResult = {
          workerId: 'unknown',
          workerType: subTask.assignedWorker,
          taskId,
          subTaskId: subTask.id,
          result: '',
          success: false,
          duration: 0,
          error: error instanceof Error ? error.message : String(error),
        };
        void this.finalizeResult(failedResult);
      });
    }
  }

  /** 串行分发任务 */
  private async dispatchSequential(subTasks: SubTask[], plan: ExecutionPlan, contextOverride?: string): Promise<void> {
    const taskId = this.currentContext!.taskId;
    for (const subTask of subTasks) {
      this.checkAborted();

      if (subTask.kind === 'batch' && subTask.batchItems?.length) {
        const tasks = this.batchTasks.get(subTask.id) ?? [];
        this.markBatchStarted(tasks);
      }
      this.emitUIMessage('progress_update',
        `分发任务给 ${subTask.assignedWorker}: ${subTask.description}`,
        { subTaskId: subTask.id, workerType: subTask.assignedWorker }
      );

      try {
        const context = contextOverride ?? this.buildWorkerContext(plan, subTask);
        const result = await this.workerPool.dispatchTaskWithRetry(
          subTask.assignedWorker, taskId, subTask, context
        );

        const finalResult = await this.finalizeResult(result);

        if (!finalResult?.success) break;
      } catch (error) {
        const failedResult: ExecutionResult = {
          workerId: 'unknown',
          workerType: subTask.assignedWorker,
          taskId,
          subTaskId: subTask.id,
          result: '',
          success: false,
          duration: 0,
          error: error instanceof Error ? error.message : String(error),
        };

        void this.finalizeResult(failedResult);
        break;
      }
    }
  }

  /** 处理子任务执行结果 */
  private async finalizeResult(result: ExecutionResult): Promise<ExecutionResult | null> {
    const subTaskId = result.subTaskId;
    if (!subTaskId) {
      this.completedResults.push(result);
      return result;
    }

    if (this.finalizationPromises.has(subTaskId)) {
      return this.finalizationPromises.get(subTaskId) ?? null;
    }

    const promise = (async () => {
      const subTask = this.getTrackedSubTask(subTaskId);
      if (!subTask) {
        return null;
      }

      if (subTask.kind === 'batch') {
        await this.finalizeBatchResult(subTask, result);
        return result;
      }

      if (subTask.kind === 'integration') {
        this.recordResult(result);
        return result;
      }

      if (this.isBackgroundSubTask(subTask)) {
        this.recordResult(result);
        return result;
      }

      const reviewConfig = this.resolveReviewConfig();
      if (!result.success || !reviewConfig) {
        this.recordResult(result);
        return result;
      }

      const decision = await this.runSubTaskReviews(subTask, result, reviewConfig);
      if (decision.status === 'passed' || decision.status === 'skipped') {
        this.recordResult(result);
        return result;
      }

      const attempts = this.reviewAttempts.get(subTaskId) ?? 0;
      if (attempts >= reviewConfig.maxRounds) {
        const failedResult: ExecutionResult = {
          ...result,
          success: false,
          error: decision.summary || decision.reason || result.error || '子任务互检失败',
        };
        this.recordResult(failedResult);
        return failedResult;
      }

      this.reviewAttempts.set(subTaskId, attempts + 1);
      this.emitUIMessage(
        'progress_update',
        `子任务 ${subTaskId} 互检未通过，进入第 ${attempts + 1} 轮修复`,
        { subTaskId, review: decision } as any
      );

      const fixMessage = [
        '修复请求:',
        `任务描述: ${subTask.description}`,
        decision.summary ? `问题: ${decision.summary}` : '',
        decision.reason ? `原因: ${decision.reason}` : ''
      ].filter(Boolean).join('\n');
      this.cliFactory.emitOrchestratorMessageToUI(subTask.assignedWorker, fixMessage);

      this.pendingTasks.set(subTaskId, subTask);
      const retryResult = await this.workerPool.dispatchTaskWithRetry(
        subTask.assignedWorker,
        subTask.taskId,
        subTask
      );
      return this.finalizeResult(retryResult);
    })();

    this.finalizationPromises.set(subTaskId, promise);
    try {
      return await promise;
    } finally {
      this.finalizationPromises.delete(subTaskId);
    }
  }

  private resolveReviewConfig(): ReviewConfigResolved | null {
    if (!this.config.review) {
      return null;
    }

    return {
      selfCheck: this.config.review.selfCheck ?? DEFAULT_REVIEW_CONFIG.selfCheck,
      peerReview: this.config.review.peerReview ?? DEFAULT_REVIEW_CONFIG.peerReview,
      maxRounds: this.config.review.maxRounds ?? DEFAULT_REVIEW_CONFIG.maxRounds,
      highRiskExtensions: this.config.review.highRiskExtensions ?? DEFAULT_REVIEW_CONFIG.highRiskExtensions,
      highRiskKeywords: this.config.review.highRiskKeywords ?? DEFAULT_REVIEW_CONFIG.highRiskKeywords,
    };
  }

  private resolveIntegrationConfig(): Required<NonNullable<OrchestratorConfig['integration']>> {
    const config = this.config.integration || {};
    return {
      enabled: config.enabled ?? true,
      maxRounds: config.maxRounds ?? 2,
      worker: config.worker ?? 'claude',
    };
  }

  private shouldEnableReviews(): boolean {
    return !!this.resolveReviewConfig();
  }

  private shouldPeerReview(subTask: SubTask, config: ReviewConfigResolved): boolean {
    if (config.peerReview === 'always') {
      return true;
    }
    if (config.peerReview === 'never') {
      return false;
    }

    const keywords = config.highRiskKeywords.map(keyword => keyword.toLowerCase());
    const text = `${subTask.description} ${subTask.prompt || ''}`.toLowerCase();
    const keywordHit = keywords.some(keyword => keyword && text.includes(keyword));
    if (keywordHit) {
      return true;
    }

    const extensions = config.highRiskExtensions.map(ext => ext.toLowerCase());
    const fileHit = (subTask.targetFiles || []).some(file => {
      const lower = file.toLowerCase();
      return extensions.some(ext => lower.endsWith(ext));
    });

    return fileHit;
  }

  private async waitForCliReady(worker: WorkerType, timeoutMs: number = 60000): Promise<boolean> {
    const start = Date.now();
    while (Date.now() - start < timeoutMs) {
      const adapter = this.cliFactory.getAdapter(worker);
      if (!adapter || !adapter.isBusy) {
        return true;
      }
      await new Promise(resolve => setTimeout(resolve, 200));
    }
    return false;
  }

  private selectPeerReviewer(subTask: SubTask): WorkerType {
    const candidates: WorkerType[] = ['claude', 'codex', 'gemini'];
    const filtered = candidates.filter(cli => cli !== subTask.assignedWorker);
    return filtered[0] ?? subTask.assignedWorker;
  }

  private buildSelfCheckPrompt(subTask: SubTask, _result: ExecutionResult): string {
    const files = (subTask.targetFiles || []).join(', ') || '未声明';
    const plan = this.currentContext?.plan;

    // 构建验收标准列表
    const criteriaList = (plan?.acceptanceCriteria && plan.acceptanceCriteria.length > 0)
      ? plan.acceptanceCriteria.map((c, i) => `${i + 1}. ${c}`).join('\n')
      : '- 无明确验收标准，请根据任务描述自行判断';

    // 功能契约（如有）
    const contractSection = plan?.featureContract
      ? `\n## 功能契约（必须遵守）\n${plan.featureContract}`
      : '';

    return [
      '# 子任务自检',
      '',
      '你刚完成一个子任务，请进行自检。',
      '',
      '## 原始任务要求',
      `- 子任务ID: ${subTask.id}`,
      `- 任务描述: ${subTask.description}`,
      `- 目标文件: ${files}`,
      contractSection,
      '',
      '## 验收标准（必须满足）',
      criteriaList,
      '',
      '## 请回答以下问题',
      '',
      '1. **你实际完成了什么？** 简要描述你的实现方式和结果',
      '2. **逐条检查验收标准**：每条是否满足？如何满足的？',
      '3. **实现差异说明**：如果你的实现方式与原始指令有差异，说明原因（更好的方案是允许的）',
      '4. **潜在问题**：是否有遗漏、边界情况未处理、或潜在bug？',
      '',
      '## 输出格式（只输出JSON）',
      '```json',
      '{',
      '  "actualWork": "实际完成的工作描述",',
      '  "criteriaCheck": [',
      '    {"criteria": "验收标准1", "passed": true, "how": "如何满足的"}',
      '  ],',
      '  "deviations": ["与原始指令的差异及原因（如有）"],',
      '  "status": "passed | rejected",',
      '  "issues": ["发现的问题（如有）"],',
      '  "summary": "总结"',
      '}',
      '```',
    ].filter(Boolean).join('\n');
  }

  private buildPeerReviewPrompt(subTask: SubTask, _result: ExecutionResult): string {
    const files = (subTask.targetFiles || []).join(', ') || '未声明';
    const plan = this.currentContext?.plan;

    // 构建验收标准列表
    const criteriaList = (plan?.acceptanceCriteria && plan.acceptanceCriteria.length > 0)
      ? plan.acceptanceCriteria.map((c, i) => `${i + 1}. ${c}`).join('\n')
      : '- 无明确验收标准，请根据任务描述自行判断';

    // 功能契约（如有）
    const contractSection = plan?.featureContract
      ? `\n## 功能契约（必须遵守）\n${plan.featureContract}`
      : '';

    return [
      '# 代码互检',
      '',
      '你是代码审查者，请对另一个 CLI 完成的子任务进行审查。',
      '',
      '## 原始任务要求',
      `- 子任务ID: ${subTask.id}`,
      `- 任务描述: ${subTask.description}`,
      `- 目标文件: ${files}`,
      `- 执行者: ${subTask.assignedWorker}`,
      contractSection,
      '',
      '## 验收标准（必须满足）',
      criteriaList,
      '',
      '## 审查要点',
      '',
      '1. **实现完整性**：是否完整实现了任务描述的功能？',
      '2. **验收标准检查**：逐条检查是否满足验收标准',
      '3. **代码质量**：可读性、可维护性、是否有明显的代码异味？',
      '4. **潜在问题**：是否有bug、安全漏洞、边界情况未处理？',
      '5. **实现方式评价**：如果实现方式与预期不同，评估是否合理或更优',
      '',
      '## 输出格式（只输出JSON）',
      '```json',
      '{',
      '  "implementationSummary": "对实现的简要描述",',
      '  "criteriaCheck": [',
      '    {"criteria": "验收标准1", "passed": true, "note": "检查说明"}',
      '  ],',
      '  "codeQuality": {"score": "good|acceptable|poor", "notes": ["质量说明"]},',
      '  "status": "passed | rejected",',
      '  "issues": ["发现的问题（如有）"],',
      '  "suggestions": ["改进建议（如有）"],',
      '  "summary": "审查结论"',
      '}',
      '```',
    ].filter(Boolean).join('\n');
  }

  private parseReviewDecision(content: string): ReviewDecision {
    const jsonMatch = content.match(/\{[\s\S]*\}/);
    const raw = jsonMatch ? jsonMatch[0] : content;
    try {
      const parsed = JSON.parse(raw);
      const status = parsed.status === 'rejected' ? 'rejected' : 'passed';
      const issues = Array.isArray(parsed.issues) ? parsed.issues : [];
      const summary = typeof parsed.summary === 'string' ? parsed.summary : '';
      return { status, issues, summary };
    } catch (error) {
      return { status: 'skipped', reason: 'review_parse_failed' };
    }
  }

  private async runSubTaskReviews(
    subTask: SubTask,
    result: ExecutionResult,
    config: ReviewConfigResolved
  ): Promise<ReviewDecision> {
    if (!config.selfCheck && !this.shouldPeerReview(subTask, config)) {
      return { status: 'skipped', reason: 'review_disabled' };
    }

    if (config.selfCheck) {
      const prompt = this.buildSelfCheckPrompt(subTask, result);
      const ready = await this.waitForCliReady(subTask.assignedWorker);
      if (!ready) {
        return { status: 'skipped', reason: 'reviewer_busy' };
      }
      this.cliFactory.emitOrchestratorMessageToUI(subTask.assignedWorker, prompt);
      const response = await this.cliFactory.sendMessage(
        subTask.assignedWorker,
        prompt,
        undefined,
        {
          source: 'worker',
          messageMeta: {
            taskId: subTask.taskId,
            subTaskId: subTask.id,
            intent: 'self_check',
          },
        }
      );
      // 记录 token 使用
      this.recordOrchestratorTokens(response.tokenUsage);

      if (response.error) {
        return { status: 'rejected', reviewer: subTask.assignedWorker, reason: response.error };
      }

      const decision = this.parseReviewDecision(response.content || '');
      if (decision.status === 'rejected') {
        decision.reviewer = subTask.assignedWorker;
        this.emitUIMessage(
          'progress_update',
          `子任务 ${subTask.id} 自检未通过`,
          { subTaskId: subTask.id, review: decision } as any
        );
        return decision;
      }
    }

    if (!this.shouldPeerReview(subTask, config)) {
      return { status: 'passed', reason: 'peer_review_skipped' };
    }

    const reviewer = this.selectPeerReviewer(subTask);
    const peerPrompt = this.buildPeerReviewPrompt(subTask, result);
    const peerReady = await this.waitForCliReady(reviewer);
    if (!peerReady) {
      return { status: 'skipped', reason: 'reviewer_busy' };
    }
    this.cliFactory.emitOrchestratorMessageToUI(reviewer, peerPrompt);
    const peerResponse = await this.cliFactory.sendMessage(
      reviewer,
      peerPrompt,
      undefined,
      {
        source: 'worker',
        messageMeta: {
          taskId: subTask.taskId,
          subTaskId: subTask.id,
          intent: 'peer_review',
        },
      }
    );
    // 记录 token 使用
    this.recordOrchestratorTokens(peerResponse.tokenUsage);

    if (peerResponse.error) {
      return { status: 'rejected', reviewer, reason: peerResponse.error };
    }

    const peerDecision = this.parseReviewDecision(peerResponse.content || '');
    if (peerDecision.status === 'rejected') {
      peerDecision.reviewer = reviewer;
      this.emitUIMessage(
        'progress_update',
        `子任务 ${subTask.id} 互检未通过`,
        { subTaskId: subTask.id, review: peerDecision } as any
      );
      return peerDecision;
    }

    return { status: 'passed', reviewer };
  }

  private async waitForAllFinalized(): Promise<void> {
    const pending = Array.from(this.finalizationPromises.values());
    if (pending.length === 0) {
      return;
    }
    await Promise.allSettled(pending);
  }

  // =========================================================================
  // Phase 4: 监控执行
  // =========================================================================

  /** 监控任务执行（用于并行模式） */
  private async monitorExecution(plan: ExecutionPlan): Promise<void> {
    if (plan.executionMode !== 'parallel') return;

    console.log('[OrchestratorAgent] Phase 4: 监控执行...');

    await new Promise<void>((resolve, reject) => {
      const interval = setInterval(() => {
        if (this.abortController?.signal.aborted) {
          clearInterval(interval);
          reject(new Error('任务已被取消'));
          return;
        }
        if (this.pendingTasks.size === 0) {
          clearInterval(interval);
          resolve();
        }
      }, 1000);

      setTimeout(() => {
        clearInterval(interval);
        if (this.pendingTasks.size > 0) reject(new Error('任务执行超时'));
      }, this.config.timeout);
    });

    await this.waitForAllFinalized();
  }


  // =========================================================================
  // Phase 5: 验证阶段
  // =========================================================================

  /** 执行验证 */
  private async runVerification(taskId: string): Promise<VerificationResult> {
    console.log('[OrchestratorAgent] Phase 5: 验证阶段...');

    if (!this.strategyConfig.enableVerification) {
      return { success: true, summary: '跳过验证（策略关闭）' };
    }

    if (!this.verificationRunner) {
      return { success: true, summary: '跳过验证（未配置）' };
    }

    // 收集所有修改的文件
    const modifiedFiles = this.completedResults
      .flatMap(r => r.modifiedFiles || [])
      .filter((f, i, arr) => arr.indexOf(f) === i); // 去重

    // 🆕 使用 PolicyEngine 决定验证策略
    const riskAssessment = this.currentContext?.risk;
    let verificationDecision;

    if (riskAssessment && this.policyEngine) {
      verificationDecision = this.policyEngine.decideVerification(riskAssessment, modifiedFiles);
      console.log(`[OrchestratorAgent] 验证决策: ${verificationDecision.reason}`);
    } else {
      // 回退到基础验证
      verificationDecision = {
        shouldVerify: true,
        config: {
          compileCheck: true,
          ideCheck: true,
          lintCheck: false,
          testCheck: false,
        },
        reason: '未找到风险评估，使用基础验证',
      };
    }

    if (!verificationDecision.shouldVerify) {
      return { success: true, summary: `跳过验证: ${verificationDecision.reason}` };
    }

    // 应用验证配置
    this.verificationRunner.updateConfig({
      compileCheck: verificationDecision.config.compileCheck ?? true,
      ideCheck: verificationDecision.config.ideCheck ?? true,
      lintCheck: verificationDecision.config.lintCheck ?? false,
      testCheck: verificationDecision.config.testCheck ?? false,
    });

    // 显示验证策略信息
    const verificationItems: string[] = [];
    if (verificationDecision.config.compileCheck) verificationItems.push('编译');
    if (verificationDecision.config.ideCheck) verificationItems.push('IDE诊断');
    if (verificationDecision.config.lintCheck) verificationItems.push('Lint');
    if (verificationDecision.config.testCheck) verificationItems.push('测试');

    this.emitUIMessage('progress_update',
      `正在执行验证检查 [${verificationItems.join(' + ')}]... (${verificationDecision.reason})`
    );

    try {
      const result = await this.verificationRunner.runVerification(taskId, modifiedFiles);

      if (result.success) {
        this.emitUIMessage('progress_update', `[通过] 验证通过: ${result.summary}`);
      } else {
        this.emitUIMessage('error', `[失败] 验证失败: ${result.summary}`);
      }

      return result;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      return { success: false, summary: `验证执行出错: ${errorMsg}` };
    }
  }

  // =========================================================================
  // Phase 6: 汇总结果
  // =========================================================================

  /** 汇总执行结果 */
  private async summarizeResults(
    userPrompt: string,
    results: ExecutionResult[],
    verificationResult?: VerificationResult | null
  ): Promise<string> {
    console.log('[OrchestratorAgent] Phase 6: 汇总结果...');

    if (results.length === 0) {
      const emptySummary = '没有执行任何任务。';
      this.emitUIMessage('summary', emptySummary);
      return emptySummary;
    }

    const summaryResults = this.filterResultsForSummary(results);

    // 构建包含验证结果的汇总 prompt
    let summaryPrompt = buildOrchestratorSummaryPrompt(userPrompt, summaryResults);

    if (verificationResult) {
      summaryPrompt += `\n\n## 验证结果\n${verificationResult.summary}`;
    }
    if (this.lastIntegrationSummary) {
      summaryPrompt += `\n\n## 集成联调\n${this.lastIntegrationSummary}`;
    }
    const aggregatedSummary = this.buildAggregatedSummary(summaryResults);
    if (aggregatedSummary) {
      summaryPrompt += `\n\n## 执行统计\n${aggregatedSummary}`;
    }

    try {
      const response = await this.cliFactory.sendMessage(
        'claude',
        summaryPrompt,
        undefined,
        {
          source: 'orchestrator',
          streamToUI: false,
          adapterRole: 'orchestrator',
          messageMeta: {
            taskId: this.currentContext?.taskId,
            intent: 'summary',
            contextSnapshot: this.buildContextSnapshot(),
          },
        }
      );

      this.recordOrchestratorTokens(response.tokenUsage);

      if (response.error) {
        const summary = this.sanitizeSummaryText([
          aggregatedSummary ? `执行统计:\n${aggregatedSummary}` : '',
          verificationResult?.summary ? `验证结果:\n${verificationResult.summary}` : '',
          this.lastIntegrationSummary ? `集成联调:\n${this.lastIntegrationSummary}` : '',
          `任务执行完成，但汇总失败: ${response.error}`,
        ].filter(Boolean).join('\n\n'));
        this.emitUIMessage('summary', summary);
        return summary;
      }

      const summary = this.sanitizeSummaryText(response.content || '');
      this.emitUIMessage('summary', summary);
      return summary;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      const summary = this.sanitizeSummaryText([
        aggregatedSummary ? `执行统计:\n${aggregatedSummary}` : '',
        verificationResult?.summary ? `验证结果:\n${verificationResult.summary}` : '',
        this.lastIntegrationSummary ? `集成联调:\n${this.lastIntegrationSummary}` : '',
        `任务执行完成，但汇总失败: ${errorMsg}`,
      ].filter(Boolean).join('\n\n'));
      this.emitUIMessage('summary', summary);
      return summary;
    }
  }

  recordOrchestratorTokens(tokenUsage?: TokenUsage): void {
    if (!tokenUsage) return;
    this.orchestratorTokenUsage.inputTokens += tokenUsage.inputTokens || 0;
    this.orchestratorTokenUsage.outputTokens += tokenUsage.outputTokens || 0;
    globalEventBus.emitEvent('execution:stats_updated', {});
  }

  getOrchestratorTokenUsage(): { inputTokens: number; outputTokens: number } {
    return { ...this.orchestratorTokenUsage };
  }

  resetOrchestratorTokenUsage(): void {
    this.orchestratorTokenUsage = { inputTokens: 0, outputTokens: 0 };
    globalEventBus.emitEvent('execution:stats_updated', {});
  }

  // =========================================================================
  // 消息处理
  // =========================================================================

  private getTrackedSubTask(subTaskId: string): SubTask | undefined {
    return this.pendingTasks.get(subTaskId) ?? this.backgroundTasks.get(subTaskId);
  }

  private removeTrackedSubTask(subTaskId: string): SubTask | undefined {
    if (this.pendingTasks.has(subTaskId)) {
      const subTask = this.pendingTasks.get(subTaskId);
      this.pendingTasks.delete(subTaskId);
      return subTask;
    }
    if (this.backgroundTasks.has(subTaskId)) {
      const subTask = this.backgroundTasks.get(subTaskId);
      this.backgroundTasks.delete(subTaskId);
      return subTask;
    }
    return undefined;
  }

  private recordResult(result: ExecutionResult): boolean {
    const subTask = this.getTrackedSubTask(result.subTaskId);
    if (!subTask) {
      return false;
    }

    if ((!result.modifiedFiles || result.modifiedFiles.length === 0) && this.snapshotManager) {
      const changedFiles = this.snapshotManager.getChangedFilesForSubTask(result.subTaskId);
      if (changedFiles.length > 0) {
        result.modifiedFiles = changedFiles;
      }
    }

    this.completedResults.push(result);
    this.removeTrackedSubTask(result.subTaskId);

    if (subTask) {
      this.checkDisciplineViolations(subTask, result);
      const planId = this.currentContext?.plan?.id;
      const sessionId = this.currentContext?.sessionId || this.currentContext?.taskId;
      if (planId && sessionId && (result.success === true || result.success === false)) {
        this.planTodoManager?.updateSubTaskStatus(sessionId, planId, subTask.id, result.success ? 'completed' : 'failed');
      }
    }

    const progressCounts = this.getProgressCounts(result.taskId);
    const total = progressCounts.total;
    const completed = progressCounts.completed;

    if (this.contextManager) {
      this.contextManager.updateTaskStatus(
        result.subTaskId,
        result.success ? 'completed' : 'failed',
        result.success ? '执行成功' : result.error
      );
      if (result.modifiedFiles && result.modifiedFiles.length > 0) {
        const summary = this.buildChangeSummary(result);
        const uniqueFiles = Array.from(new Set(result.modifiedFiles));
        uniqueFiles.forEach(file => {
          this.contextManager!.addCodeChange(file, 'modify', summary);
        });
      }
      void this.contextManager.saveMemory();
    }

    if (this.taskManager) {
      this.taskManager.updateSubTaskStatus(
        result.taskId,
        result.subTaskId,
        result.success ? 'completed' : 'failed'
      );
      if (result.modifiedFiles && result.modifiedFiles.length > 0) {
        this.taskManager.updateSubTaskFiles(result.taskId, result.subTaskId, result.modifiedFiles);
      }
      globalEventBus.emitEvent(result.success ? 'subtask:completed' : 'subtask:failed', {
        taskId: result.taskId,
        subTaskId: result.subTaskId,
        data: result.success
          ? {
            success: true,
            cli: result.workerType,
            description: subTask?.description,
            targetFiles: subTask?.targetFiles,
            modifiedFiles: result.modifiedFiles || [],
            duration: result.duration,
          }
          : {
            error: result.error || '未知错误',
            cli: result.workerType,
            description: subTask?.description,
            targetFiles: subTask?.targetFiles,
            modifiedFiles: result.modifiedFiles || [],
            duration: result.duration,
          }, 
      });
    }
    if (this.taskStateManager) {
      this.taskStateManager.updateStatus(
        result.subTaskId,
        result.success ? 'completed' : 'failed',
        result.error
      );
      if (result.success) {
        this.taskStateManager.setResult(result.subTaskId, result.result, result.modifiedFiles);
      }
    }

    this.emitUIMessage(
      'progress_update',
      buildProgressMessage(completed, total, result.workerType),
      { progress: total > 0 ? Math.round((completed / total) * 100) : 0, result }
    );

    if (!result.success) {
      this.emitUIMessage('error', `子任务失败: ${result.error || '未知错误'}`, { subTaskId: result.subTaskId });
    }

    return true;
  }

  private checkDisciplineViolations(subTask: SubTask, result: ExecutionResult): void {
    const modified = (result.modifiedFiles || []).map(file => file.trim()).filter(Boolean);
    if (modified.length === 0) {
      return;
    }

    const normalizedTargets = (subTask.targetFiles || []).map(f => f.trim()).filter(Boolean);
    const outOfScope = normalizedTargets.length === 0
      ? modified
      : modified.filter(file => !normalizedTargets.includes(file));

    const touchedPlan = modified.filter(file => file.includes('.multicli/plans') || file.includes('.multicli/execution-state'));

    if (touchedPlan.length > 0) {
      this.emitUIMessage(
        'progress_update',
        `编排纪律提示: 子任务 ${subTask.id} 修改了编排状态文件（${touchedPlan.join(', ')}），请避免直接改动计划/状态文件`,
        { subTaskId: subTask.id }
      );
    }

    if (outOfScope.length > 0) {
      const targets = normalizedTargets.length > 0 ? normalizedTargets.join(', ') : '未声明目标文件';
      this.emitUIMessage(
        'progress_update',
        `编排纪律提示: 子任务 ${subTask.id} 修改了计划外文件 (${outOfScope.join(', ')})；目标文件: ${targets}`,
        { subTaskId: subTask.id }
      );
    }
  }

  private buildChangeSummary(result: ExecutionResult): string {
    const raw = (result.result || '').trim();
    if (!raw) {
      return result.success ? '执行成功' : (result.error || '执行失败');
    }
    const text = raw.replace(/\s+/g, ' ').trim();
    return text.length > 200 ? `${text.slice(0, 200)}...` : text;
  }

  private getProgressCounts(taskId: string): { total: number; completed: number } {
    const backgroundIds = new Set(
      (this.currentContext?.plan?.subTasks || [])
        .filter(task => this.isBackgroundSubTask(task))
        .map(task => task.id)
    );
    if (this.taskStateManager) {
      const tasks = this.taskStateManager.getAllTasks().filter(task =>
        task.parentTaskId === taskId && !backgroundIds.has(task.id)
      );
      if (tasks.length > 0) {
        const completed = tasks.filter(task =>
          task.status === 'completed' || task.status === 'failed' || task.status === 'cancelled'
        ).length;
        return { total: tasks.length, completed };
      }
    }

    const total = (this.currentContext?.plan?.subTasks || [])
      .filter(task => !backgroundIds.has(task.id)).length;
    const completed = this.completedResults.filter(
      r => r.taskId === taskId && !backgroundIds.has(r.subTaskId)
    ).length;
    return { total, completed };
  }

  /** 处理任务完成消息 */
  private handleTaskCompleted(message: TaskCompletedMessage): void {
    const { result } = message.payload;

    void this.finalizeResult(result).catch(error => {
      console.warn('[OrchestratorAgent] 任务收尾失败:', error);
    });
  }

  /** 处理任务失败消息 */
  private async handleTaskFailed(message: TaskFailedMessage): Promise<void> {
    const { taskId, subTaskId, error, canRetry } = message.payload;
    const subTask = this.pendingTasks.get(subTaskId);

    if (!subTask) {
      return;
    }

    // task_failed 仅表示“本次尝试失败”，最终状态由调度结果决定
    this.emitUIMessage(
      'progress_update',
      `子任务尝试失败: ${error}`,
      { subTaskId, canRetry }
    );
    console.warn(`[OrchestratorAgent] 子任务尝试失败: ${error}`);
    if (this.taskStateManager) {
      if (canRetry) {
        this.taskStateManager.resetForRetry(subTaskId);
      } else {
        this.taskStateManager.updateStatus(subTaskId, 'failed', error);
      }
    }
    if (!canRetry && this.strategyConfig.enableRecovery && this.recoveryHandler && this.taskStateManager) {
      const failedTask = this.taskStateManager.getTask(subTaskId);
      if (failedTask) {
        const decision = await this.resolveRecoveryDecision(failedTask, error);
        if (decision === 'rollback') {
          await this.performSessionRollback(failedTask);
          this.setState('monitoring');
          return;
        }
        if (decision === 'continue') {
          this.emitUIMessage('progress_update', '已忽略失败，继续执行后续任务', { subTaskId });
          this.setState('monitoring');
          return;
        }
        this.setState('recovering');
        this.emitUIMessage('progress_update', `进入失败恢复流程: ${failedTask.description}`, { subTaskId });
        void this.recoveryHandler
          .recover(taskId, failedTask, { success: false, summary: error }, error)
          .then((result) => {
            this.emitUIMessage('progress_update', `恢复结果: ${result.message}`, { subTaskId });
          })
          .finally(() => {
            this.setState('monitoring');
          });
      }
    }
  }

  private async resolveRecoveryDecision(
    failedTask: TaskState,
    error: string
  ): Promise<'retry' | 'rollback' | 'continue'> {
    const canRetry = this.recoveryHandler?.shouldContinueRecovery(failedTask) ?? false;
    const canRollback = Boolean(this.snapshotManager);
    if (!this.recoveryConfirmationCallback) {
      if (canRetry) return 'retry';
      if (canRollback) return 'rollback';
      return 'continue';
    }
    return this.recoveryConfirmationCallback(failedTask, error, { retry: canRetry, rollback: canRollback });
  }

  private async performSessionRollback(failedTask: TaskState): Promise<void> {
    await this.workerPool.cancelAllTasks();
    this.workerPool.clearExecutionStates();
    const count = this.snapshotManager?.revertAllChanges() ?? 0;
    this.taskStateManager?.updateStatus(failedTask.id, 'cancelled', '已回滚');
    this.emitUIMessage('progress_update', `已回滚 ${count} 个变更`, { subTaskId: failedTask.id });
  }

  /** 处理进度汇报消息 */
  private handleProgressReport(message: ProgressReportMessage): void {
    const { taskId, subTaskId, status, progress, message: msg, output } = message.payload;

    if (status === 'started' || status === 'in_progress') {
      this.taskManager?.updateSubTaskStatus(taskId, subTaskId, 'running');
      this.taskStateManager?.updateStatus(subTaskId, 'running');

      if (status === 'started') {
        const subTask = this.pendingTasks.get(subTaskId)
          ?? this.currentContext?.plan?.subTasks.find(task => task.id === subTaskId);
        globalEventBus.emitEvent('subtask:started', {
          taskId,
          subTaskId,
          data: {
            cli: subTask?.assignedWorker,
            description: subTask?.description,
            targetFiles: subTask?.targetFiles,
            reason: subTask?.reason,
          },
        });
      }
    }
    if (status === 'failed') {
      this.taskStateManager?.updateStatus(subTaskId, 'failed', msg);
    }

    if (msg) {
      this.emitUIMessage('progress_update', msg, { subTaskId, progress });
    }
    if (progress !== undefined) {
      this.taskStateManager?.updateProgress(subTaskId, progress);
    }
  }

  // =========================================================================
  // UI 消息发送
  // =========================================================================

  /** 发送 UI 消息（标识来源为编排者） */
  private emitUIMessage(
    type: OrchestratorUIMessage['type'],
    content: string,
    metadata?: Partial<OrchestratorUIMessage['metadata']>
  ): void {
    const message: OrchestratorUIMessage = {
      type,
      taskId: this.currentContext?.taskId || '',
      timestamp: Date.now(),
      content,
      metadata: { phase: this._state, ...metadata },
    };

    // 发送事件时标识来源为 'orchestrator'
    globalEventBus.emitEvent('orchestrator:ui_message', {
      data: { ...message, source: 'orchestrator' }  // 将 source 放入 data 中
    });
    this.emit('uiMessage', message);
  }

  // =========================================================================
  // 生命周期
  // =========================================================================

  /** 销毁编排者 */
  dispose(): void {
    this.unsubscribers.forEach(unsub => unsub());
    this.unsubscribers = [];
    this.workerPool.dispose();
    this.cleanup();
    this.removeAllListeners();
    console.log('[OrchestratorAgent] 已销毁');
  }
}
