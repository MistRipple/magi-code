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

  // 快照管理（支持文件回滚）
  private snapshotManager: SnapshotManager | null = null;
  private taskManager: TaskManager | null = null;

  // 🆕 执行统计（支持 CLI 降级决策）
  private executionStats: ExecutionStats;

  private _state: OrchestratorState = 'idle';
  private currentContext: TaskContext | null = null;
  private confirmationCallback: ConfirmationCallback | null = null;
  private abortController: AbortController | null = null;
  private unsubscribers: Array<() => void> = [];

  // 任务执行状态
  private pendingTasks: Map<string, SubTask> = new Map();
  private completedResults: ExecutionResult[] = [];
  private reviewAttempts: Map<string, number> = new Map();
  private finalizationPromises: Map<string, Promise<ExecutionResult | null>> = new Map();
  private warnedReviewSkipForDependencies = false;

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

    // 创建 Worker Pool，集成执行统计
    this.workerPool = new WorkerPool({
      cliFactory,
      messageBus: this.messageBus,
      orchestratorId: this.id,
      executionStats: this.executionStats,
      enableFallback: true,
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
  async execute(userPrompt: string, taskId: string): Promise<string> {
    if (this._state !== 'idle') {
      throw new Error(`编排者当前状态为 ${this._state}，无法接受新任务`);
    }

    // 初始化任务上下文
    this.currentContext = {
      taskId,
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

    // 初始化上下文管理器
    if (this.contextManager) {
      await this.contextManager.initialize(taskId, `task-${taskId}`);
      this.contextManager.addMessage({ role: 'user', content: userPrompt });
    }

    try {
      // Phase 1: 任务分析
      this.setState('analyzing');
      const plan = await this.analyzeTask(userPrompt);

      if (!plan) {
        throw new Error('任务分析失败');
      }

      this.currentContext.plan = plan;
      this.checkAborted();

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

  /** 检查是否被中断 */
  private checkAborted(): void {
    if (this.abortController?.signal.aborted) {
      throw new Error('任务已被用户取消');
    }
  }

  /** 取消当前任务 */
  async cancel(): Promise<void> {
    console.log('[OrchestratorAgent] 取消任务');
    this.abortController?.abort();
    await this.workerPool.cancelAllTasks();
    this.setState('idle');
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
    const analysisPrompt = buildOrchestratorAnalysisPrompt(userPrompt, availableWorkers);

    try {
      // 使用 Claude 进行分析（编排者专用会话）
      const response = await this.cliFactory.sendMessage(
        'claude',
        analysisPrompt,
        undefined,
        { source: 'orchestrator', streamToUI: false }
      );

      if (response.error) {
        console.error('[OrchestratorAgent] 分析失败:', response.error);
        return null;
      }

      const plan = this.parseExecutionPlan(response.content);

      if (plan) {
        this.emitUIMessage('plan_ready', formatPlanForUser(plan), { plan });
        globalEventBus.emitEvent('orchestrator:plan_ready', {
          taskId: this.currentContext?.taskId,
          data: { plan },
        });
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
      const jsonMatch = content.match(/```json\s*([\s\S]*?)\s*```/);
      const jsonStr = jsonMatch ? jsonMatch[1] : content;
      const parsed = JSON.parse(jsonStr);

      return {
        id: `plan_${Date.now()}`,
        analysis: parsed.analysis || '',
        isSimpleTask: parsed.isSimpleTask || false,
        skipReason: parsed.skipReason,
        needsCollaboration: parsed.needsCollaboration ?? true,
        subTasks: (parsed.subTasks || []).map((t: any, i: number) => ({
          id: t.id || String(i + 1),
          taskId: this.currentContext?.taskId || '',
          description: t.description || '',
          assignedWorker: t.assignedWorker || t.assignedCli || 'claude',
          reason: t.reason || '',
          targetFiles: t.targetFiles || [],
          dependencies: t.dependencies || [],
          prompt: t.prompt || '',
          priority: t.priority,
          status: 'pending',
          output: [],
        })),
        executionMode: parsed.executionMode || 'sequential',
        summary: parsed.summary || '',
        createdAt: Date.now(),
      };
    } catch (error) {
      console.error('[OrchestratorAgent] 解析执行计划失败:', error);
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
      await this.dispatchWithDependencyGraph(plan.subTasks);
    } else if (plan.executionMode === 'parallel') {
      await this.dispatchParallel(plan.subTasks);
    } else {
      await this.dispatchSequential(plan.subTasks);
    }

  }

  /** 🆕 基于依赖图分发任务 */
  private async dispatchWithDependencyGraph(subTasks: SubTask[]): Promise<void> {
    this.emitUIMessage('progress_update', '正在分析任务依赖关系...');

    try {
      const results = await this.workerPool.executeWithDependencyGraph(
        this.currentContext!.taskId,
        subTasks,
        this.currentContext?.userPrompt
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
    if (!this.snapshotManager) {
      console.log('[OrchestratorAgent] 未配置 SnapshotManager，跳过快照创建');
      return;
    }

    const targetFiles = new Set<string>();
    for (const subTask of plan.subTasks) {
      if (subTask.targetFiles) {
        subTask.targetFiles.forEach(f => targetFiles.add(f));
      }
    }

    if (targetFiles.size === 0) {
      console.log('[OrchestratorAgent] 没有目标文件，跳过快照创建');
      return;
    }

    console.log(`[OrchestratorAgent] 为 ${targetFiles.size} 个文件创建快照...`);
    this.emitUIMessage('progress_update', `正在为 ${targetFiles.size} 个文件创建快照...`);

    for (const filePath of targetFiles) {
      try {
        this.snapshotManager.createSnapshot(
          filePath,
          'claude', // 默认使用 claude 作为修改者
          this.currentContext?.taskId || 'unknown'
        );
      } catch (error) {
        console.warn(`[OrchestratorAgent] 创建快照失败: ${filePath}`, error);
      }
    }
  }

  /** 将执行计划同步到 TaskManager */
  private syncPlanToTaskManager(plan: ExecutionPlan): void {
    if (!this.taskManager || !this.currentContext) return;

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
  private async dispatchParallel(subTasks: SubTask[]): Promise<void> {
    const taskId = this.currentContext!.taskId;
    for (const subTask of subTasks) {
      this.emitUIMessage('progress_update',
        `分发任务给 ${subTask.assignedWorker}: ${subTask.description}`,
        { subTaskId: subTask.id, workerType: subTask.assignedWorker }
      );

      void this.workerPool.dispatchTaskWithRetry(
        subTask.assignedWorker,
        taskId,
        subTask
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
  private async dispatchSequential(subTasks: SubTask[]): Promise<void> {
    const taskId = this.currentContext!.taskId;
    for (const subTask of subTasks) {
      this.checkAborted();

      this.emitUIMessage('progress_update',
        `分发任务给 ${subTask.assignedWorker}: ${subTask.description}`,
        { subTaskId: subTask.id, workerType: subTask.assignedWorker }
      );

      try {
        const result = await this.workerPool.dispatchTaskWithRetry(
          subTask.assignedWorker, taskId, subTask
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

  private selectPeerReviewer(subTask: SubTask): WorkerType {
    const candidates: WorkerType[] = ['claude', 'codex', 'gemini'];
    const filtered = candidates.filter(cli => cli !== subTask.assignedWorker);
    return filtered[0] ?? subTask.assignedWorker;
  }

  private buildSelfCheckPrompt(subTask: SubTask, _result: ExecutionResult): string {
    const files = (subTask.targetFiles || []).join(', ') || '未声明';
    return [
      '你刚完成一个子任务，请进行快速自检。',
      `子任务: ${subTask.id}`,
      `描述: ${subTask.description}`,
      `目标文件: ${files}`,
      '请检查是否满足任务要求、是否遗漏或引入错误。',
      '输出 JSON: {"status":"passed|rejected","issues":[...],"summary":"..."}',
      '只输出 JSON。',
    ].join('\n');
  }

  private buildPeerReviewPrompt(subTask: SubTask, _result: ExecutionResult): string {
    const files = (subTask.targetFiles || []).join(', ') || '未声明';
    return [
      '你是代码审查者，请对另一个 CLI 完成的子任务进行快速审查。',
      `子任务: ${subTask.id}`,
      `描述: ${subTask.description}`,
      `目标文件: ${files}`,
      '请检查是否满足要求、是否存在逻辑/质量问题。',
      '输出 JSON: {"status":"passed|rejected","issues":[...],"summary":"..."}',
      '只输出 JSON。',
    ].join('\n');
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
      const response = await this.cliFactory.sendMessage(
        subTask.assignedWorker,
        prompt,
        undefined,
        { source: 'orchestrator', streamToUI: false }
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
    const peerResponse = await this.cliFactory.sendMessage(
      reviewer,
      peerPrompt,
      undefined,
      { source: 'orchestrator', streamToUI: false }
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

    try {
      const response = await this.cliFactory.sendMessage(
        'claude',
        summaryPrompt,
        undefined,
        { source: 'orchestrator', streamToUI: false }
      );

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
        data: result.success ? { success: true } : { error: result.error || '未知错误' },
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

    if (canRetry) {
      this.emitUIMessage(
        'progress_update',
        `子任务失败，正在重试: ${error}`,
        { subTaskId }
      );
      return;
    }

    console.warn(`[OrchestratorAgent] 子任务失败: ${error}`);
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
