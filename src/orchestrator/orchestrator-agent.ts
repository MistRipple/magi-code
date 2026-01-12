/**
 * Orchestrator Agent - 独立编排者 Claude
 *
 * 核心职责：
 * - 专职编排，不执行任何编码任务
 * - 实现事件循环，实时监控所有 Worker
 * - 响应用户交互和 Worker 反馈
 * - 动态调度和错误处理
 * - 🆕 CLI 降级和执行统计
 *
 * 架构理念：
 * - 编排者是"永远在线"的协调者
 * - 100% 时间用于监控和协调
 * - 可以立即响应任何事件
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
} from './protocols/types';
import {
  buildOrchestratorAnalysisPrompt,
  buildOrchestratorSummaryPrompt,
  formatPlanForUser,
  buildProgressMessage,
} from './prompts/orchestrator-prompts';
import { TokenUsage } from '../cli/types';

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
};

/** 用户确认回调类型 */
export type ConfirmationCallback = (plan: ExecutionPlan, formattedPlan: string) => Promise<boolean>;

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
 * 🆕 集成 CLI 降级和执行统计
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

  // 🆕 执行统计（支持 CLI 降级决策）
  private executionStats: ExecutionStats;

  private _state: OrchestratorState = 'idle';
  private currentContext: TaskContext | null = null;
  private confirmationCallback: ConfirmationCallback | null = null;
  private questionCallback: QuestionCallback | null = null;
  private abortController: AbortController | null = null;
  private unsubscribers: Array<() => void> = [];

  // 任务执行状态
  private pendingTasks: Map<string, SubTask> = new Map();
  private completedResults: ExecutionResult[] = [];
  private lastIntegrationSummary: string | null = null;
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

    // 🆕 创建执行统计实例
    this.executionStats = new ExecutionStats();

    // 创建 Worker Pool，集成执行统计和快照管理
    this.workerPool = new WorkerPool({
      cliFactory,
      messageBus: this.messageBus,
      orchestratorId: this.id,
      executionStats: this.executionStats,
      enableFallback: true,
      snapshotManager: this.snapshotManager || undefined,  // 🆕 传递快照管理器
    });

    // 初始化验证组件
    if (this.workspaceRoot && this.config.verification) {
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

  /** 🆕 设置扩展上下文（用于持久化执行统计） */
  setExtensionContext(context: import('vscode').ExtensionContext): void {
    this.executionStats.setContext(context);
  }

  /** 🆕 获取执行统计实例 */
  getExecutionStats(): ExecutionStats {
    return this.executionStats;
  }

  /** 🆕 获取执行统计摘要（用于 UI 显示） */
  getStatsSummary(): string {
    return this.executionStats.getSummary();
  }

  /** 初始化 */
  async initialize(): Promise<void> {
    await this.workerPool.initialize();
    console.log('[OrchestratorAgent] 初始化完成');
    console.log(`[OrchestratorAgent] 执行统计: ${this.getStatsSummary()}`);
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
      this.handleTaskFailed(msg as TaskFailedMessage);
    });
    this.unsubscribers.push(unsubFailed);

    // 监听进度汇报消息
    const unsubProgress = this.messageBus.subscribe('progress_report', (msg) => {
      this.handleProgressReport(msg as ProgressReportMessage);
    });
    this.unsubscribers.push(unsubProgress);
  }

  /** 设置 Worker Pool 事件处理 */
  private setupWorkerPoolHandlers(): void {
    this.workerPool.on('workerOutput', ({ workerId, workerType, chunk }) => {
      this.emitUIMessage('worker_output', chunk, { workerId, workerType });
    });

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
    this.reviewAttempts.clear();
    this.finalizationPromises.clear();
    this.warnedReviewSkipForDependencies = false;
    this.lastIntegrationSummary = null;

    // 初始化上下文管理器
    await this.ensureContext(contextSessionId, userPrompt);

    try {
      // Phase 1: 任务分析（支持补充提问）
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
            return '任务已取消。';
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
      this.checkAborted();

      // 🆕 如果不需要 Worker，直接返回编排者的回复
      if (plan.needsWorker === false && plan.directResponse) {
        console.log('[OrchestratorAgent] 不需要 Worker，编排者直接回答');
        this.emitUIMessage('direct_response', plan.directResponse);
        await this.saveAndCompressMemory(plan.directResponse);
        this.setState('completed');
        this.currentContext.endTime = Date.now();
        return plan.directResponse;
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
      }

      // Phase 2: 等待用户确认
      this.setState('waiting_confirmation');
      const confirmed = await this.waitForConfirmation(plan);

      if (!confirmed) {
        this.setState('idle');
        return '任务已取消。';
      }
      this.checkAborted();

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
      if (this.verificationRunner) {
        this.setState('verifying');
        verificationResult = await this.runVerification(taskId);

        // 如果验证失败，记录错误但继续汇总
        if (!verificationResult.success) {
          this.emitUIMessage('error', `验证失败: ${verificationResult.summary}`);
        }
      }
      this.checkAborted();

      // Phase 6: 汇总结果
      this.setState('summarizing');
      const summary = await this.summarizeResults(userPrompt, this.completedResults, verificationResult);

      // 保存 Memory 并检查是否需要压缩
      await this.saveAndCompressMemory(summary);

      this.setState('completed');
      this.currentContext.endTime = Date.now();

      return summary;

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);

      if (this.abortController?.signal.aborted) {
        this.setState('idle');
        return '任务已被取消。';
      }

      this.setState('failed');
      this.emitUIMessage('error', `任务执行失败: ${errorMsg}`);
      throw error;

    } finally {
      this.cleanup();
    }
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
  // Phase 1: 任务分析
  // =========================================================================

  /**
   * 分析任务，生成执行计划
   */
  private async analyzeTask(userPrompt: string): Promise<ExecutionPlan | null> {
    console.log('[OrchestratorAgent] Phase 1: 任务分析...');

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

      const plan = this.parseExecutionPlan(response.content);
      if (plan) {
        this.ensureArchitectureTask(plan, userPrompt);
        this.normalizeExecutionPlan(plan);
      }

      if (plan) {
        if (plan.analysis) {
          this.emitUIMessage('progress_update', `需求分析: ${plan.analysis}`);
        }
        if (!plan.needsUserInput) {
          this.emitUIMessage('plan_ready', formatPlanForUser(plan), { plan });
          globalEventBus.emitEvent('orchestrator:plan_ready', {
            taskId: this.currentContext?.taskId,
            data: { plan },
          });
        }
      }

      return plan;
    } catch (error) {
      console.error('[OrchestratorAgent] 分析异常:', error);
      return null;
    }
  }

  /**
   * 解析执行计划 JSON
   */
  private parseExecutionPlan(content: string): ExecutionPlan | null {
    try {
      const jsonCandidates = this.extractPlanJsonCandidates(content);
      const parsed = this.parsePlanJson(jsonCandidates);
      const rawSubTasks = this.extractSubTasks(parsed);

      // 🆕 处理不需要 Worker 的情况（编排者直接回答）
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

      // 🆕 修复：不再强制要求功能契约和验收清单
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
        subTasks: rawSubTasks.map((t: any, i: number) => ({
          id: t.id || String(i + 1),
          taskId: this.currentContext?.taskId || '',
          description: t.description || '',
          assignedWorker: t.assignedWorker || t.assignedCli || 'claude',
          reason: t.reason || '',
          targetFiles: t.targetFiles || [],
          dependencies: t.dependencies || [],
          prompt: t.prompt || '',
          priority: t.priority,
          kind: t.kind || 'implementation',
          featureId: t.featureId || `feature_${this.currentContext?.taskId || 'unknown'}`,
          status: 'pending',
          output: [],
        })),
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

  private normalizeExecutionPlan(plan: ExecutionPlan): void {
    if (!plan.subTasks || plan.subTasks.length === 0) {
      return;
    }

    this.normalizeArchitectureKinds(plan);
    this.pruneClaudeImplementationForFullStack(plan);
    this.pruneMissingDependencies(plan);

    const hasDependencies = plan.subTasks.some(task => task.dependencies && task.dependencies.length > 0);
    const hasFileConflicts = this.hasFileConflicts(plan.subTasks);

    if (!hasDependencies && !hasFileConflicts && plan.executionMode === 'sequential') {
      plan.executionMode = 'parallel';
      this.emitUIMessage('progress_update', '执行模式已调整为并行（无依赖且无文件冲突）');
    }
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

    throw new Error(`无法解析执行计划 JSON: ${errors.join(' | ')}`);
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

  /** 分发任务给 Worker */
  private async dispatchTasks(plan: ExecutionPlan): Promise<void> {
    console.log('[OrchestratorAgent] Phase 3: 分发任务...');

    this.syncPlanToTaskManager(plan);

    // 在执行前创建文件快照（支持回滚）
    await this.createSnapshotsForPlan(plan);

    for (const subTask of plan.subTasks) {
      this.pendingTasks.set(subTask.id, subTask);
    }

    // 🆕 检查是否有任务依赖关系，决定执行策略
    const hasDependencies = plan.subTasks.some(
      t => t.dependencies && t.dependencies.length > 0
    );

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
      await this.dispatchWithDependencyGraph(plan);
    } else if (plan.executionMode === 'parallel') {
      await this.dispatchParallel(plan.subTasks, plan);
    } else {
      await this.dispatchSequential(plan.subTasks, plan);
    }

  }

  /** 🆕 基于依赖图分发任务 */
  private async dispatchWithDependencyGraph(plan: ExecutionPlan): Promise<void> {
    this.emitUIMessage('progress_update', '正在分析任务依赖关系...');

    try {
      const results = await this.workerPool.executeWithDependencyGraph(
        this.currentContext!.taskId,
        plan.subTasks,
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
    const maxTokens = config.workerMaxTokens + (this.isHighRiskSubTask(subTask) ? config.workerHighRiskExtraTokens : 0);

    const contextSlice = this.contextManager?.getContextSlice({
      maxTokens,
      memoryRatio: config.workerMemoryRatio,
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

    return [sharedContext, taskHint, contextSlice].filter(Boolean).join('\n\n');
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
    if (plan.subTasks.length === 0) {
      return;
    }

    const featureId = plan.subTasks[0]?.featureId || `feature_${this.currentContext.taskId}`;
    const taskId = this.currentContext.taskId;
    const maxRounds = Math.max(1, integrationConfig.maxRounds);
    const sharedContext = this.buildSharedContext(plan);
    let dependencyIds = plan.subTasks.map(task => task.id);

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
        prompt: this.buildIntegrationPrompt(plan),
        priority: 1,
        status: 'pending',
        output: [],
        kind: 'integration',
        featureId,
      };

      this.pendingTasks.set(integrationSubTask.id, integrationSubTask);
      this.taskManager?.addExistingSubTask(taskId, integrationSubTask);
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
        this.emitUIMessage('progress_update', `✅ 联调通过: ${report.summary}`);
        return;
      }

      this.emitUIMessage('error', `❌ 联调未通过: ${report.summary}`);

      const repairTasks: SubTask[] = report.issues.map((issue, index) => {
        const worker = this.pickWorkerForIssue(issue);
        return {
          id: `repair_${taskId}_${round}_${index + 1}`,
          taskId,
          description: `修复联调问题: ${issue.title || issue.area || '问题'}`,
          assignedWorker: worker,
          reason: '联调修复',
          targetFiles: issue.targetFiles || [],
          dependencies: [...dependencyIds],
          prompt: issue.fixPrompt || this.buildRepairPrompt(plan, issue),
          priority: 2,
          status: 'pending',
          output: [],
          kind: 'repair',
          featureId,
        };
      });

      if (repairTasks.length === 0) {
        throw new Error('联调未通过但未生成修复任务');
      }

      await this.createSnapshotsForSubTasks(repairTasks);
      for (const repairTask of repairTasks) {
        this.pendingTasks.set(repairTask.id, repairTask);
        this.taskManager?.addExistingSubTask(taskId, repairTask);
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
      const subTask = this.pendingTasks.get(subTaskId);
      if (!subTask) {
        return null;
      }

      if (subTask.kind === 'integration') {
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

    if (!this.verificationRunner) {
      return { success: true, summary: '跳过验证（未配置）' };
    }

    this.emitUIMessage('progress_update', '正在执行验证检查...');

    // 收集所有修改的文件
    const modifiedFiles = this.completedResults
      .flatMap(r => r.modifiedFiles || [])
      .filter((f, i, arr) => arr.indexOf(f) === i); // 去重

    try {
      const result = await this.verificationRunner.runVerification(taskId, modifiedFiles);

      if (result.success) {
        this.emitUIMessage('progress_update', `✅ 验证通过: ${result.summary}`);
      } else {
        this.emitUIMessage('error', `❌ 验证失败: ${result.summary}`);
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

    // 构建包含验证结果的汇总 prompt
    let summaryPrompt = buildOrchestratorSummaryPrompt(userPrompt, results);

    if (verificationResult) {
      summaryPrompt += `\n\n## 验证结果\n${verificationResult.summary}`;
    }
    if (this.lastIntegrationSummary) {
      summaryPrompt += `\n\n## 集成联调\n${this.lastIntegrationSummary}`;
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
        const summary = `任务执行完成，但汇总失败: ${response.error}`;
        this.emitUIMessage('summary', summary);
        return summary;
      }

      this.emitUIMessage('summary', response.content);
      return response.content;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      const summary = `任务执行完成，但汇总失败: ${errorMsg}`;
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

  private recordResult(result: ExecutionResult): boolean {
    if (!this.pendingTasks.has(result.subTaskId)) {
      return false;
    }

    this.completedResults.push(result);
    this.pendingTasks.delete(result.subTaskId);

    const total = this.currentContext?.plan?.subTasks.length || 0;
    const completed = this.completedResults.length;

    if (this.contextManager) {
      this.contextManager.updateTaskStatus(
        result.subTaskId,
        result.success ? 'completed' : 'failed',
        result.success ? '执行成功' : result.error
      );
    }

    if (this.taskManager) {
      this.taskManager.updateSubTaskStatus(
        result.taskId,
        result.subTaskId,
        result.success ? 'completed' : 'failed'
      );
      globalEventBus.emitEvent(result.success ? 'subtask:completed' : 'subtask:failed', {
        taskId: result.taskId,
        subTaskId: result.subTaskId,
        data: result.success
          ? { success: true, cli: result.workerType }
          : { error: result.error || '未知错误', cli: result.workerType },  // 🆕 传递 CLI 信息
      });
    }

    this.emitUIMessage(
      'progress_update',
      buildProgressMessage(completed, total, result.workerType),
      { progress: Math.round((completed / total) * 100), result }
    );

    if (!result.success) {
      this.emitUIMessage('error', `子任务失败: ${result.error || '未知错误'}`, { subTaskId: result.subTaskId });
    }

    return true;
  }

  /** 处理任务完成消息 */
  private handleTaskCompleted(message: TaskCompletedMessage): void {
    const { result } = message.payload;

    void this.finalizeResult(result).catch(error => {
      console.warn('[OrchestratorAgent] 任务收尾失败:', error);
    });
  }

  /** 处理任务失败消息 */
  private handleTaskFailed(message: TaskFailedMessage): void {
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
  }

  /** 处理进度汇报消息 */
  private handleProgressReport(message: ProgressReportMessage): void {
    const { taskId, subTaskId, status, progress, message: msg, output } = message.payload;

    if (output) {
      this.emitUIMessage('worker_output', output, { subTaskId });
    }

    if (status === 'started' || status === 'in_progress') {
      this.taskManager?.updateSubTaskStatus(taskId, subTaskId, 'running');

      if (status === 'started') {
        const subTask = this.pendingTasks.get(subTaskId)
          ?? this.currentContext?.plan?.subTasks.find(task => task.id === subTaskId);
        globalEventBus.emitEvent('subtask:started', {
          taskId,
          subTaskId,
          data: {
            cli: subTask?.assignedWorker,
            description: subTask?.description,
          },
        });
      }
    }

    if (msg) {
      this.emitUIMessage('progress_update', msg, { subTaskId, progress });
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
