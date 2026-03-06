/**
 * Mission-Driven Engine - 核心编排引擎
 *
 * 职责：
 * - 任务分析与意图识别
 * - Mission 规划与执行协调
 * - Worker 调度与进度管理
 * - 验证与总结
 */
import { EventEmitter } from 'events';
import path from 'path';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { UnifiedSessionManager } from '../../session/unified-session-manager';
import { SnapshotManager } from '../../snapshot-manager';
import { ContextManager } from '../../context/context-manager';
import { logger, LogCategory } from '../../logging';
import { PermissionMatrix, StrategyConfig, WorkerSlot, InteractionMode, INTERACTION_MODE_CONFIGS, InteractionModeConfig } from '../../types';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { CategoryResolver } from '../profile/category-resolver';
import { OrchestratorState, RequirementAnalysis } from '../protocols/types';
import type { WorkerReport } from '../protocols/worker-report';
import { VerificationRunner, VerificationConfig } from '../verification-runner';
import { MissionOrchestrator } from './mission-orchestrator';
import {
  Mission,
  MissionStorageManager,
  FileBasedMissionStorage,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { MessageHub } from './message-hub';
import { WisdomManager } from '../wisdom';
import { buildUnifiedSystemPrompt } from '../prompts/orchestrator-prompts';
import { isAbortError } from '../../errors';
import { SupplementaryInstructionQueue } from './supplementary-instruction-queue';
import { DispatchManager } from './dispatch-manager';
import { runPostDispatchVerification } from './post-dispatch-verifier';
import { configureResilientAuxiliary } from './resilient-auxiliary-adapter';
import { TaskViewService } from '../../services/task-view-service';
import { PlanLedgerService, type PlanMode, type PlanRecord } from '../plan-ledger';
import {
  createWisdomStorage,
  extractPrimaryIntent,
  extractUserConstraints,
  isKeyInstruction,
  resolveOrchestratorContextPolicy,
} from './mission-driven-engine-helpers';

/**
 * 引擎配置
 */
export interface MissionDrivenEngineConfig {
  timeout: number;
  maxRetries: number;
  review?: {
    selfCheck?: boolean;
    peerReview?: 'auto' | 'always' | 'never';
    maxRounds?: number;
    highRiskExtensions?: string[];
    highRiskKeywords?: string[];
  };
  planReview?: {
    enabled?: boolean;
    reviewer?: WorkerSlot;
  };
  verification?: Partial<VerificationConfig>;
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: WorkerSlot;
  };
  permissions?: PermissionMatrix;
  strategy?: StrategyConfig;
}

/**
 * MissionDrivenEngine - 基于 Mission-Driven Architecture 的编排引擎
 */
export class MissionDrivenEngine extends EventEmitter {
  private adapterFactory: IAdapterFactory;
  private sessionManager: UnifiedSessionManager;
  private snapshotManager: SnapshotManager;
  private contextManager: ContextManager;
  private workspaceRoot: string;
  private config: MissionDrivenEngineConfig;

  // Mission-Driven 核心组件
  private missionOrchestrator: MissionOrchestrator;
  // MissionExecutor 已合并到 MissionOrchestrator
  private missionStorage: MissionStorageManager;
  private taskViewService: TaskViewService;
  private readonly planLedger: PlanLedgerService;
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private categoryResolver = new CategoryResolver();

  private verificationRunner?: VerificationRunner;

  // 项目知识库
  private projectKnowledgeBase?: import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase;
  private wisdomManager: WisdomManager;

  // 状态
  private _state: OrchestratorState = 'idle';
  private lastTaskAnalysis: {
    suggestedMode?: 'sequential' | 'parallel';
    explicitWorkers?: WorkerSlot[];
    wantsParallel?: boolean;
  } | null = null;
  private currentTaskId: string | null = null;
  private lastMissionId: string | null = null;
  /** 当前对话轮次唯一标识（每次 execute() 入口生成，贯穿整轮快照） */
  private currentTurnId: string | null = null;
  /** 当前对话轮次对应计划 ID（Plan Ledger） */
  private currentPlanId: string | null = null;
  /** 等待确认中的计划请求 */
  private pendingPlanConfirmation: {
    sessionId: string;
    planId: string;
    resolve: (confirmed: boolean) => void;
  } | null = null;
  /** dispatch 并发场景下的 Mission 创建单飞锁，确保每轮最多创建一个 Mission */
  private ensureMissionPromise: Promise<string> | null = null;
  private lastRoutingDecision: {
    needsWorker: boolean;
    category?: string;
    categories?: string[];
    delegationBriefings?: string[];
    needsTooling?: boolean;
    requiresModification?: boolean;
    executionMode?: RequirementAnalysis['executionMode'];
    directResponse?: string;
    reason?: string;
  } | null = null;

  // Token 统计
  private orchestratorTokens = {
    inputTokens: 0,
    outputTokens: 0,
  };

  private lastExecutionErrors: string[] = [];
  private lastExecutionSuccess = true;

  // 执行统计
  private executionStats: ExecutionStats;

  // 统一消息出口
  private messageHub: MessageHub;
  private currentSessionId?: string;
  private contextSessionId: string | null = null;

  // 当前执行的用户原始请求（Phase C 汇总引用）
  private activeUserPrompt: string = '';
  // 当前执行的用户原始图片路径（Worker dispatch 传递）
  private activeImagePaths?: string[];

  // 交互模式
  private interactionMode: InteractionMode = 'auto';
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.auto;
  // 运行状态
  private isRunning = false;
  private executionQueue: Promise<void> = Promise.resolve();
  private pendingCount = 0;

  // P0-3: 补充指令队列（独立状态机）
  private supplementaryQueue: SupplementaryInstructionQueue;

  // P1-4: Dispatch 调度管理器（独立子系统）
  private dispatchManager: DispatchManager;

  constructor(
    adapterFactory: IAdapterFactory,
    config: MissionDrivenEngineConfig,
    workspaceRoot: string,
    snapshotManager: SnapshotManager,
    sessionManager: UnifiedSessionManager
  ) {
    super();
    this.adapterFactory = adapterFactory;
    this.config = config;
    this.workspaceRoot = workspaceRoot;
    this.snapshotManager = snapshotManager;
    this.sessionManager = sessionManager;

    // 初始化基础组件
    this.profileLoader = ProfileLoader.getInstance();
    this.guidanceInjector = new GuidanceInjector();
    this.contextManager = new ContextManager(workspaceRoot, undefined, sessionManager);
    this.executionStats = new ExecutionStats();
    this.supplementaryQueue = new SupplementaryInstructionQueue(this);
    this.wisdomManager = new WisdomManager();

    // 初始化 Mission 存储（使用 .magi/sessions 目录，按 session 分组存储）
    const sessionsDir = path.join(workspaceRoot, '.magi', 'sessions');
    const fileStorage = new FileBasedMissionStorage(sessionsDir);
    this.missionStorage = new MissionStorageManager(fileStorage);
    this.taskViewService = new TaskViewService(this.missionStorage, this.workspaceRoot);
    this.planLedger = new PlanLedgerService(this.sessionManager);

    // 初始化 Mission 编排器
    this.missionOrchestrator = new MissionOrchestrator(
      this.profileLoader,
      this.guidanceInjector,
      adapterFactory,
      this.contextManager,
      this.missionStorage,
      workspaceRoot,
      snapshotManager,
    );

    // MissionExecutor 已合并到 MissionOrchestrator，无需单独创建

    // 初始化统一消息出口
    this.messageHub = new MessageHub();

    this.configureWisdomStorage();

    // 初始化 Dispatch 调度管理器
    this.dispatchManager = new DispatchManager({
      adapterFactory: this.adapterFactory,
      profileLoader: this.profileLoader,
      messageHub: this.messageHub,
      missionOrchestrator: this.missionOrchestrator,
      workspaceRoot: this.workspaceRoot,
      getActiveUserPrompt: () => this.activeUserPrompt,
      getActiveImagePaths: () => this.activeImagePaths,
      getCurrentSessionId: () => this.currentSessionId,
      getMissionIdsBySession: async (sessionId: string) => {
        const missions = await this.missionStorage.listBySession(sessionId);
        return missions.map(mission => mission.id);
      },
      ensureMissionForDispatch: async () => this.ensureMissionForDispatch(),
      getCurrentTurnId: () => this.currentTurnId,
      getProjectKnowledgeBase: () => this.projectKnowledgeBase,
      processWorkerWisdom: (report) => this.processWorkerWisdom(report),
      recordOrchestratorTokens: (usage, phase) => this.recordOrchestratorTokens(usage, phase),
      recordWorkerTokenUsage: (results) => this.recordWorkerTokenUsage(results),
      getSnapshotManager: () => this.snapshotManager ?? null,
      getContextManager: () => this.contextManager ?? null,
      getTodoManager: () => this.missionOrchestrator.getTodoManager() ?? null,
      getSupplementaryQueue: () => this.supplementaryQueue,
      onDispatchTaskRegistered: (payload) => this.handleDispatchTaskRegistered(payload),
    });

    // 构造阶段先注入一次编排工具 handler，避免初始化空窗触发 "handler not configured"
    this.dispatchManager.setupOrchestrationToolHandlers();
    this.setupPlanLedgerEventBindings();
  }

  /**
   * 配置 Wisdom 存储
   */
  private configureWisdomStorage(): void {
    this.wisdomManager.setStorage(
      createWisdomStorage(this.contextManager, () => this.projectKnowledgeBase),
    );
  }

  /**
   * 处理 Worker 终态报告中的 Wisdom，并持久化到上下文/知识库。
   * 仅处理 completed/failed 且带 result 的报告，避免 progress/question 噪声。
   */
  private processWorkerWisdom(report: WorkerReport): void {
    if ((report.type !== 'completed' && report.type !== 'failed') || !report.result) {
      return;
    }

    try {
      const extraction = this.wisdomManager.processReport(report, report.assignmentId);
      if (
        extraction.learnings.length > 0
        || extraction.decisions.length > 0
        || extraction.warnings.length > 0
        || Boolean(extraction.significantLearning)
      ) {
        logger.info('任务引擎.Wisdom.已提取', {
          assignmentId: report.assignmentId,
          worker: report.workerId,
          learnings: extraction.learnings.length,
          decisions: extraction.decisions.length,
          warnings: extraction.warnings.length,
          hasSignificant: Boolean(extraction.significantLearning),
        }, LogCategory.ORCHESTRATOR);
      }
    } catch (error) {
      logger.warn('任务引擎.Wisdom.提取失败', {
        assignmentId: report.assignmentId,
        worker: report.workerId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  /**
   * 获取当前状态
   */
  get state(): OrchestratorState {
    return this._state;
  }

  /**
   * 是否正在运行
   */
  get running(): boolean {
    return this.isRunning;
  }

  /**
   * 设置交互模式
   */
  setInteractionMode(mode: InteractionMode): void {
    this.interactionMode = mode;
    this.modeConfig = INTERACTION_MODE_CONFIGS[mode];
    logger.info('引擎.交互_模式.变更', { mode }, LogCategory.ORCHESTRATOR);
  }

  /**
   * 获取当前交互模式
   */
  getInteractionMode(): InteractionMode {
    return this.interactionMode;
  }

  /**
   * 获取交互模式配置
   */
  getModeConfig(): InteractionModeConfig {
    return this.modeConfig;
  }

  getPlanLedgerSnapshot(sessionId: string) {
    return this.planLedger.getSnapshot(sessionId);
  }

  getActivePlanState(sessionId: string) {
    return this.planLedger.buildActivePlanState(sessionId);
  }

  async reconcilePlanLedgerForSession(sessionId: string): Promise<void> {
    const normalizedSessionId = sessionId?.trim();
    if (!normalizedSessionId) {
      return;
    }

    try {
      const missions = await this.missionStorage.listBySession(normalizedSessionId);
      await this.planLedger.reconcileByMissions(
        normalizedSessionId,
        missions.map((mission) => ({
          id: mission.id,
          status: mission.status,
        })),
      );
    } catch (error) {
      logger.warn('编排器.计划账本.会话对账失败', {
        sessionId: normalizedSessionId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  resolvePlanConfirmation(confirmed: boolean): boolean {
    if (!this.pendingPlanConfirmation) {
      return false;
    }
    const pending = this.pendingPlanConfirmation;
    this.pendingPlanConfirmation = null;
    pending.resolve(confirmed);
    return true;
  }

  private async awaitPlanConfirmation(
    sessionId: string,
    plan: PlanRecord,
    fallbackFormattedPlan: string,
  ): Promise<boolean> {
    const awaitingPlan = await this.planLedger.markAwaitingConfirmation(sessionId, plan.planId, fallbackFormattedPlan);
    const displayPlan = awaitingPlan || plan;
    const formattedPlan = displayPlan.formattedPlan || fallbackFormattedPlan;

    this.messageHub.data('confirmationRequest', {
      sessionId,
      plan: {
        planId: displayPlan.planId,
        status: displayPlan.status,
        summary: displayPlan.summary,
        items: displayPlan.items.map(item => ({
          itemId: item.itemId,
          title: item.title,
          owner: item.owner,
          status: item.status,
        })),
      },
      formattedPlan,
    });

    this.setState('waiting_confirmation');
    return new Promise<boolean>((resolve) => {
      this.pendingPlanConfirmation = {
        sessionId,
        planId: displayPlan.planId,
        resolve,
      };
    });
  }

  private emitPlanLedgerUpdate(sessionId: string, reason: string): void {
    const snapshot = this.planLedger.getSnapshot(sessionId);
    const activePlan = snapshot.activePlan;
    this.messageHub.data('planLedgerUpdated', {
      sessionId,
      reason,
      activePlan,
      plans: snapshot.plans,
    });
  }

  private async handleDispatchTaskRegistered(payload: {
    sessionId: string;
    missionId: string;
    taskId: string;
    worker: WorkerSlot;
    title: string;
    category: string;
    dependsOn?: string[];
    scopeHint?: string[];
    files?: string[];
    requiresModification: boolean;
  }): Promise<void> {
    if (!this.currentPlanId) {
      return;
    }
    await this.planLedger.upsertDispatchItem(payload.sessionId, this.currentPlanId, {
      itemId: payload.taskId,
      title: payload.title,
      worker: payload.worker,
      category: payload.category,
      dependsOn: payload.dependsOn,
      scopeHints: payload.scopeHint,
      targetFiles: payload.files,
      requiresModification: payload.requiresModification,
    });
  }

  private mapMissionStatusToTerminalPlanStatus(status: string): 'completed' | 'failed' | 'cancelled' | null {
    if (status === 'completed') return 'completed';
    if (status === 'failed') return 'failed';
    if (status === 'cancelled') return 'cancelled';
    return null;
  }

  private reportPlanLedgerAsyncError(action: string, error: unknown): void {
    logger.warn('编排器.计划账本.异步回写失败', {
      action,
      error: error instanceof Error ? error.message : String(error),
    }, LogCategory.ORCHESTRATOR);
  }

  private setupPlanLedgerEventBindings(): void {
    this.planLedger.on('updated', (event: { sessionId: string; reason: string }) => {
      this.emitPlanLedgerUpdate(event.sessionId, event.reason);
    });

    this.missionOrchestrator.on('assignmentPlanned', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      void this.planLedger
        .bindAssignmentTodos(this.currentSessionId, this.currentPlanId, data.assignmentId, data.todos)
        .catch((error) => this.reportPlanLedgerAsyncError('assignmentPlanned', error));
    });

    this.missionOrchestrator.on('assignmentStarted', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      if (!assignmentId) {
        logger.warn('编排器.计划账本.assignmentStarted.缺少assignmentId', {
          dataKeys: data && typeof data === 'object' ? Object.keys(data as Record<string, unknown>) : typeof data,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void this.planLedger
        .updateAssignmentStatus(this.currentSessionId, this.currentPlanId, assignmentId, 'running')
        .catch((error) => this.reportPlanLedgerAsyncError('assignmentStarted', error));
    });

    this.missionOrchestrator.on('assignmentCompleted', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      if (!assignmentId) {
        logger.warn('编排器.计划账本.assignmentCompleted.缺少assignmentId', {
          dataKeys: data && typeof data === 'object' ? Object.keys(data as Record<string, unknown>) : typeof data,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void this.planLedger
        .updateAssignmentStatus(
          this.currentSessionId,
          this.currentPlanId,
          assignmentId,
          data.success ? 'completed' : 'failed',
        )
        .catch((error) => this.reportPlanLedgerAsyncError('assignmentCompleted', error));
    });

    this.missionOrchestrator.on('todoStarted', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      void this.planLedger
        .updateTodoStatus(
          this.currentSessionId,
          this.currentPlanId,
          data.assignmentId,
          data.todoId,
          'running',
        )
        .catch((error) => this.reportPlanLedgerAsyncError('todoStarted', error));
    });

    this.missionOrchestrator.on('todoCompleted', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      void this.planLedger
        .updateTodoStatus(
          this.currentSessionId,
          this.currentPlanId,
          data.assignmentId,
          data.todoId,
          'completed',
        )
        .catch((error) => this.reportPlanLedgerAsyncError('todoCompleted', error));
    });

    this.missionOrchestrator.on('todoFailed', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      void this.planLedger
        .updateTodoStatus(
          this.currentSessionId,
          this.currentPlanId,
          data.assignmentId,
          data.todoId,
          'failed',
        )
        .catch((error) => this.reportPlanLedgerAsyncError('todoFailed', error));
    });

    this.missionOrchestrator.on('missionStatusChanged', (data) => {
      const terminalStatus = this.mapMissionStatusToTerminalPlanStatus(data.newStatus);
      if (!terminalStatus) {
        return;
      }
      const sessionId = data.mission.sessionId;
      void this.planLedger
        .finalizeByMissionStatus(sessionId, data.mission.id, terminalStatus)
        .catch((error) => this.reportPlanLedgerAsyncError('missionStatusChanged', error));
    });
  }

  private enqueueExecution<T>(runner: () => Promise<T>): Promise<T> {
    const queueDepth = this.pendingCount++;
    if (queueDepth > 0) {
      this.messageHub.notify(`当前有 ${queueDepth} 个任务排队中，请稍候...`);
    }
    const next = this.executionQueue.then(runner, runner);
    this.executionQueue = next.then(
      () => { this.pendingCount--; },
      () => { this.pendingCount--; }
    );
    return next;
  }

  private setState(next: OrchestratorState): void {
    if (this._state === next) {
      return;
    }
    this._state = next;
    this.emit('stateChange', this._state);
    const isRunning = next !== 'idle' && next !== 'completed' && next !== 'failed';
    this.messageHub.phaseChange(next, isRunning, this.currentTaskId || undefined);
  }

  /**
   * 获取当前阶段（state 的别名）
   */
  get phase(): OrchestratorState {
    return this._state;
  }

  /**
   * 获取 MessageHub 实例
   * 外部可以订阅 MessageHub 事件来接收消息
   */
  getMessageHub(): MessageHub {
    return this.messageHub;
  }

  /**
   * 获取 ContextManager 实例
   * 外部可以使用 ContextManager 记录上下文信息
   */
  getContextManager(): ContextManager {
    return this.contextManager;
  }

  // ============ P0-3: 补充指令机制（委托 SupplementaryInstructionQueue） ============

  injectSupplementaryInstruction(content: string): boolean {
    return this.supplementaryQueue.inject(content, this.isRunning);
  }

  consumeSupplementaryInstructions(workerId?: WorkerSlot): string[] {
    return this.supplementaryQueue.consume(workerId);
  }

  getPendingInstructionCount(): number {
    return this.supplementaryQueue.getPendingCount();
  }

  activateWorkerSessionResume(sourceMissionId: string, resumePrompt?: string): boolean {
    return this.dispatchManager.activateResumeContext(sourceMissionId, resumePrompt);
  }

  clearWorkerSessionResume(): void {
    this.dispatchManager.clearResumeContext();
  }

  /**
   * 初始化引擎
   */
  async initialize(): Promise<void> {
    // 加载画像配置
    await this.profileLoader.load();
    this.applyToolPermissions();

    // 初始化 VerificationRunner
    if (this.config.strategy?.enableVerification) {
      this.verificationRunner = new VerificationRunner(
        this.workspaceRoot,
        this.config.verification
      );
    }

    await configureResilientAuxiliary(this.contextManager, this.executionStats);

    // 提前初始化 TodoManager，避免首次调用 get_todos/update_todo 时命中“未初始化”
    await this.missionOrchestrator.ensureTodoManagerInitialized();

    // 注入编排工具的回调处理器
    this.dispatchManager.setupOrchestrationToolHandlers();

    logger.info('编排器.任务引擎.初始化.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 将引擎权限配置同步到 ToolManager（默认全开）
   */
  private applyToolPermissions(): void {
    const permissions: PermissionMatrix = {
      allowEdit: this.config.permissions?.allowEdit ?? true,
      allowBash: this.config.permissions?.allowBash ?? true,
      allowWeb: this.config.permissions?.allowWeb ?? true,
    };

    this.adapterFactory.getToolManager().setPermissions(permissions);
    logger.info('编排器.工具权限.已同步', permissions, LogCategory.ORCHESTRATOR);
  }

  /**
   * 重新加载画像
   */
  async reloadProfiles(): Promise<void> {
    await this.profileLoader.reload();
    // 画像/配置变化后，立即重建编排工具可用 Worker 枚举，避免 schema 与运行时配置不一致
    this.dispatchManager.setupOrchestrationToolHandlers();
    logger.info('画像配置已重载', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 设置项目知识库
   */
  setKnowledgeBase(knowledgeBase: import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase): void {
    this.projectKnowledgeBase = knowledgeBase;
    // 注入到 ContextManager（确保 Worker 上下文包含项目知识）
    this.contextManager.setProjectKnowledgeBase(knowledgeBase);
    this.configureWisdomStorage();
    logger.info('任务引擎.知识库.已设置', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 统一执行入口 - ReAct 模式
   *
   * 单次 LLM 调用 + 工具循环。
   * LLM 在统一系统提示词下自主决策：直接回答 / 工具操作 / 分配 Worker。
   */
  async execute(
    userPrompt: string,
    taskId: string,
    sessionId?: string,
    imagePaths?: string[],
    turnIdHint?: string
  ): Promise<string> {
    return this.enqueueExecution(async () => {
      const trimmedPrompt = userPrompt?.trim() || '';
      if (!trimmedPrompt) {
        return '请输入你的需求或问题。';
      }

      this.isRunning = true;
      this.currentTaskId = taskId || null;
      this.lastMissionId = null;
      this.currentPlanId = null;
      this.pendingPlanConfirmation = null;
      this.ensureMissionPromise = null;
      // 每轮对话生成唯一 turnId，作为本轮所有快照的 missionId。
      // 若上游已生成（用于 UI 点击历史计划精确回溯），则复用同一 turnId。
      const normalizedTurnIdHint = typeof turnIdHint === 'string' ? turnIdHint.trim() : '';
      this.currentTurnId = normalizedTurnIdHint || `turn:${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
      this.setState('running');
      this.lastTaskAnalysis = null;
      this.lastRoutingDecision = null;
      this.supplementaryQueue.reset();
      this.currentSessionId = sessionId;
      this.activeUserPrompt = trimmedPrompt;
      this.activeImagePaths = imagePaths;
      let planFinalStatus: 'completed' | 'failed' | 'cancelled' | null = null;

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        this.currentSessionId = resolvedSessionId;
        this.dispatchManager.resetForNewExecutionCycle();
        await this.ensureContextReady(resolvedSessionId);

        const planMode: PlanMode = this.adapterFactory.isDeepTask() ? 'deep' : 'standard';
        const draftPlan = await this.planLedger.createDraft({
          sessionId: resolvedSessionId,
          turnId: this.currentTurnId || `turn:${Date.now()}`,
          missionId: this.currentTurnId || undefined,
          mode: planMode,
          prompt: trimmedPrompt,
          summary: trimmedPrompt,
        });
        this.currentPlanId = draftPlan.planId;

        const fallbackFormattedPlan = draftPlan.formattedPlan || this.planLedger.formatPlanForDisplay(draftPlan);
        if (this.interactionMode === 'ask') {
          const confirmed = await this.awaitPlanConfirmation(resolvedSessionId, draftPlan, fallbackFormattedPlan);
          if (!confirmed) {
            await this.planLedger.reject(resolvedSessionId, draftPlan.planId, 'user', '用户取消执行计划');
            this.lastExecutionSuccess = false;
            this.lastExecutionErrors = ['用户取消执行计划'];
            planFinalStatus = 'cancelled';
            this.setState('idle');
            this.currentTaskId = null;
            return '已取消执行计划。';
          }
          await this.planLedger.approve(resolvedSessionId, draftPlan.planId, 'user');
          this.setState('running');
        } else {
          await this.planLedger.approve(resolvedSessionId, draftPlan.planId, 'system:auto');
        }
        await this.planLedger.markExecuting(resolvedSessionId, draftPlan.planId);

        // 1. 组装上下文
        const context = await this.prepareContext(resolvedSessionId, trimmedPrompt);

        // 2. 获取项目上下文和 ADR
        const projectContext = this.projectKnowledgeBase
          ? this.projectKnowledgeBase.getProjectContext(600)
          : undefined;

        const relevantADRs = this.projectKnowledgeBase
          ? this.projectKnowledgeBase.getADRs({ status: 'accepted' })
              .map(adr => `### ${adr.title}\n${adr.decision}`)
              .join('\n\n') || undefined
          : undefined;

        // 3. 构建统一系统提示词（Worker 列表从 ProfileLoader 动态获取，工具列表从 ToolManager 动态加载）
        const enabledProfiles = this.profileLoader.getEnabledProfiles();
        const availability = this.dispatchManager.getWorkerAvailability();
        const availableWorkers = availability.availableWorkers;
        const workerProfiles = availableWorkers
          .map(worker => enabledProfiles.get(worker))
          .filter((profile): profile is NonNullable<typeof profile> => Boolean(profile))
          .map(p => ({
            worker: p.worker,
            displayName: p.persona.displayName,
            strengths: p.persona.strengths,
            assignedCategories: p.assignedCategories,
          }));
        const availableToolsSummary = await this.getAvailableToolsSummary();

        // 获取分类定义（用于系统提示词的分工映射表）
        const allCategories = this.profileLoader.getAllCategories();
        const categoryDefinitions = new Map<string, { displayName: string; description: string }>();
        for (const [name, def] of allCategories) {
          categoryDefinitions.set(name, { displayName: def.displayName, description: def.description });
        }

        // 获取系统当前的活动 Todo 列表并转为字符串以注入到上下文
        let activeTodosSummary = '';
        try {
          const todoManager = this.missionOrchestrator.getTodoManager();
          if (todoManager) {
            // 获取所有相关的 Todo，包括已完成的，以告知编排者真实进度
            const allTodos = await todoManager.query({ sessionId: resolvedSessionId });
            if (allTodos.length > 0) {
              const fullSummary = allTodos.map(t => {
                const isDone = t.status === 'completed';
                const statusFlag = isDone ? 'COMPLETED (代码已被真实修改，请勿重复执行)' : t.status.toUpperCase();
                return `- [${statusFlag}] ID: ${t.id} | Worker: ${t.workerId || 'unassigned'} | 任务: ${t.content}`;
              }).join('\n');
              // Token 截断机制：限制最大长度约 1000 字符，防止上下文超载
              activeTodosSummary = fullSummary.length > 1000
                ? fullSummary.substring(0, 1000) + '\n... (部分任务已截断)'
                : fullSummary;
            }
          }
        } catch (e) {
          logger.warn('获取 active Todos 失败', { error: String(e) }, LogCategory.ORCHESTRATOR);
        }

        let systemPrompt = buildUnifiedSystemPrompt({
          workspaceRoot: this.workspaceRoot,
          availableWorkers,
          workerProfiles,
          projectContext,
          sessionSummary: context || undefined,
          activeTodosSummary,
          relevantADRs,
          availableToolsSummary,
          categoryDefinitions,
          deepTask: this.adapterFactory.isDeepTask(),
        });

        // 追加用户规则（buildUnifiedSystemPrompt 不含用户规则，需显式注入）
        const userRulesPrompt = this.adapterFactory.getUserRulesPrompt();
        if (userRulesPrompt) {
          systemPrompt = `${systemPrompt}\n\n${userRulesPrompt}`;
        }

        // 4. 设置编排者快照上下文
        // 使用本轮唯一 turnId 作为 missionId，确保每轮对话的快照可独立分组。
        // 若后续进入 dispatch 流，worker 的快照也将使用同一 turnId。
        const orchestratorToolManager = this.adapterFactory.getToolManager();
        const orchestratorAssignmentId = `orchestrator-${this.currentTurnId}`;
        const normalizedSessionId = resolvedSessionId.trim();
        orchestratorToolManager.setSnapshotContext({
          sessionId: normalizedSessionId,
          missionId: this.currentTurnId!,
          assignmentId: orchestratorAssignmentId,
          todoId: orchestratorAssignmentId,
          workerId: 'orchestrator',
        });

        // 编排者跨轮会话记忆保留在 adapter history 中，不再每轮清空。
        // SystemPrompt 侧通过 prepareContext 动态裁剪 recent_turns，避免双重注入和 token 膨胀。

        // 5. 编排执行
        const response = await this.adapterFactory.sendMessage(
          'orchestrator',
          trimmedPrompt,
          imagePaths,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            systemPrompt,
            includeToolCalls: true,
          }
        );

        this.recordOrchestratorTokens(response.tokenUsage);

        // 等待 dispatch batch 归档（含 Worker 执行 + Phase C 汇总）
        // dispatch_task 是非阻塞的，sendMessage 返回时 Worker 可能还在后台执行。
        // 必须等待 activeBatch 归档后再推进下一阶段，保证链路完整闭合。
        const currentBatch = this.dispatchManager.getActiveBatch();
        if (currentBatch && currentBatch.status !== 'archived') {
          await currentBatch.waitForArchive();
        }

        const auditOutcome = currentBatch?.getAuditOutcome();
        if (auditOutcome?.level === 'intervention') {
          throw new Error('Phase C 审计发现需干预项，自动交付已阻断，请按审计建议追加修复任务后重试');
        }

        if (response.error) {
          throw new Error(response.error);
        }

        if (currentBatch) {
          await runPostDispatchVerification(currentBatch, this.verificationRunner, this.messageHub);
        }

        // 反应式编排兜底：若 Batch 处于”等待最终汇总”且编排者未输出正文，
        // 则由系统生成确定性总结并发送到主对话区，避免用户只看到子任务卡片没有结论。
        let finalContent = response.content || '';
        if (currentBatch && this.dispatchManager.isReactiveBatchAwaitingSummary(currentBatch.id)) {
          if (finalContent.trim()) {
            this.dispatchManager.markReactiveBatchSummarized(currentBatch.id);
          } else {
            finalContent = this.dispatchManager.buildReactiveBatchFallbackSummary(currentBatch);
            this.messageHub.result(finalContent, {
              metadata: {
                phase: 'reactive_fallback_summary',
                extra: {
                  batchId: currentBatch.id,
                },
              },
            });
            this.dispatchManager.markReactiveBatchSummarized(currentBatch.id);
          }
        }

        this.lastExecutionSuccess = true;
        this.lastExecutionErrors = [];
        planFinalStatus = 'completed';
        this.setState('idle');
        this.currentTaskId = null;
        return finalContent;

      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        // 中断导致的 abort 不视为执行失败，静默处理
        if (isAbortError(error)) {
          logger.info('编排器.统一执行.中断', { error: errorMessage }, LogCategory.ORCHESTRATOR);
          this.lastExecutionSuccess = false;
          this.lastExecutionErrors = [];
          planFinalStatus = 'cancelled';
          this.setState('idle');
          this.currentTaskId = null;
          return '';
        }
        this.lastExecutionSuccess = false;
        this.lastExecutionErrors = [errorMessage];
        planFinalStatus = 'failed';
        logger.error('编排器.统一执行.失败', { error: errorMessage }, LogCategory.ORCHESTRATOR);
        this.setState('idle');
        this.currentTaskId = null;
        throw error;
      } finally {
        const finalSessionId = this.currentSessionId;
        const finalPlanId = this.currentPlanId;
        if (finalSessionId && finalPlanId && planFinalStatus) {
          try {
            await this.planLedger.finalize(finalSessionId, finalPlanId, planFinalStatus);
          } catch (planError) {
            logger.warn('编排器.计划账本.终态更新失败', {
              sessionId: finalSessionId,
              planId: finalPlanId,
              error: planError instanceof Error ? planError.message : String(planError),
            }, LogCategory.ORCHESTRATOR);
          }
        }
        const pendingConfirmation = this.pendingPlanConfirmation as {
          sessionId: string;
          planId: string;
          resolve: (confirmed: boolean) => void;
        } | null;
        if (pendingConfirmation) {
          pendingConfirmation.resolve(false);
          this.pendingPlanConfirmation = null;
        }
        this.isRunning = false;
        this.ensureMissionPromise = null;
        // 清除编排者快照上下文
        this.adapterFactory.getToolManager().clearSnapshotContext('orchestrator');
        // 清除 MissionOrchestrator 的 Mission ID 关联
        this.missionOrchestrator.setCurrentMissionId(null);
        // 更新 Mission 生命周期
        // 任务采用懒创建：只有进入 dispatch 流才会创建 Mission。
        // 因此 this.lastMissionId 存在即代表当前轮属于任务执行流。
        if (this.lastMissionId) {
          try {
            const batch = this.dispatchManager.getActiveBatch();
            if (batch?.status === 'archived') {
              this.dispatchManager.markReactiveBatchSummarized(batch.id);
            }
            const hadDispatch = !!batch && batch.getEntries().length > 0;

            if (!hadDispatch) {
              // 未调用 dispatch_task → 不属于任务维度，删除空 Mission
              await this.missionStorage.delete(this.lastMissionId);
            } else {
              // 有 dispatch：用 batch entries 填充 Mission.goal（替代用户消息原文）
              const mission = await this.missionStorage.load(this.lastMissionId);
              if (mission && !mission.goal) {
                const entries = batch.getEntries();
                mission.goal = entries.length === 1
                  ? entries[0].task.substring(0, 80)
                  : entries.map(e => e.task.substring(0, 40)).join('；');
                await this.missionStorage.update(mission);
              }
              // 更新 Mission 终态
              if (this.lastExecutionSuccess) {
                await this.taskViewService.completeTaskById(this.lastMissionId);
              } else if (this.lastExecutionErrors.length > 0) {
                await this.taskViewService.failTaskById(this.lastMissionId, this.lastExecutionErrors[0]);
              } else {
                await this.taskViewService.cancelTaskById(this.lastMissionId);
              }
            }
          } catch {
            // Mission 状态更新失败不影响主流程
          }
        }
        try {
          await this.contextManager.flushMemorySave();
        } catch (memoryError) {
          logger.warn('编排器.上下文.保存失败', { error: memoryError }, LogCategory.ORCHESTRATOR);
        }
        this.currentPlanId = null;
      }
    });
  }

  private async ensureMissionForDispatch(): Promise<string> {
    if (this.lastMissionId) {
      return this.lastMissionId;
    }
    if (this.ensureMissionPromise) {
      return this.ensureMissionPromise;
    }
    const turnIdAtCall = this.currentTurnId;
    const pending = (async (): Promise<string> => {
      // 双重检查：并发请求在等待期内可能已有 mission 产生
      if (this.lastMissionId) {
        return this.lastMissionId;
      }

      const sessionId = this.currentSessionId || this.sessionManager.getCurrentSession()?.id || '';
      if (!sessionId) {
        throw new Error('缺少会话 ID');
      }
      const prompt = this.activeUserPrompt?.trim() || '';
      if (!prompt) {
        throw new Error('缺少用户请求');
      }

      const mission = await this.missionStorage.createMission({
        sessionId,
        userPrompt: prompt,
        context: '',
      });

      // 若 Mission 创建期间执行轮次已切换，回收该 Mission，避免生成孤儿任务
      if (turnIdAtCall && this.currentTurnId !== turnIdAtCall) {
        try {
          await this.missionStorage.delete(mission.id);
        } catch {
          // 回收失败不阻塞主流程，后续由任务清理流程处理
        }
        throw new Error('执行轮次已切换，Mission 创建结果失效');
      }

      mission.status = 'executing';
      mission.failureReason = undefined;
      mission.startedAt = Date.now();
      await this.missionStorage.update(mission);

      this.lastMissionId = mission.id;
      this.missionOrchestrator.setCurrentMissionId(mission.id);

      if (this.currentPlanId) {
        await this.planLedger.bindMission(sessionId, this.currentPlanId, mission.id);
      }

      const orchestratorToolManager = this.adapterFactory.getToolManager();
      const orchestratorAssignmentId = `orchestrator-${mission.id}`;
      const normalizedSessionId = sessionId.trim();
      orchestratorToolManager.setSnapshotContext({
        sessionId: normalizedSessionId,
        missionId: this.currentTurnId || mission.id,
        assignmentId: orchestratorAssignmentId,
        todoId: orchestratorAssignmentId,
        workerId: 'orchestrator',
      });

      return mission.id;
    })();

    this.ensureMissionPromise = pending;

    try {
      return await pending;
    } finally {
      if (this.ensureMissionPromise === pending) {
        this.ensureMissionPromise = null;
      }
    }
  }

  getLastExecutionStatus(): { success: boolean; errors: string[] } {
    return {
      success: this.lastExecutionSuccess,
      errors: [...this.lastExecutionErrors],
    };
  }

  /**
   * 带任务上下文执行
   */
  async executeWithTaskContext(
    userPrompt: string,
    sessionId?: string,
    imagePaths?: string[],
    turnIdHint?: string
  ): Promise<{ taskId: string; result: string }> {
    const result = await this.execute(userPrompt, '', sessionId, imagePaths, turnIdHint);
    return { taskId: this.lastMissionId || '', result };
  }

  /**
   * 获取可用工具摘要（供统一系统提示词使用）
   * 委托 ToolManager.buildToolsSummary() 生成，保持单一 source of truth
   */
  private async getAvailableToolsSummary(): Promise<string> {
    try {
      const toolManager = this.adapterFactory.getToolManager();
      return await toolManager.buildToolsSummary({ role: 'orchestrator' });
    } catch (error) {
      logger.warn('获取工具摘要失败', { error }, LogCategory.ORCHESTRATOR);
      return '';
    }
  }
  /**
   * 准备上下文
   */
  async prepareContext(_sessionId: string, _userPrompt: string): Promise<string> {
    const sessionId = _sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (sessionId) {
      await this.ensureContextReady(sessionId);
    }

    const policy = resolveOrchestratorContextPolicy(
      this.adapterFactory.getAdapterHistoryInfo?.('orchestrator') ?? undefined,
    );
    const missionId = this.lastMissionId || (sessionId ? `session:${sessionId}` : 'session:default');
    const assembledOptions = this.contextManager.buildAssemblyOptions(
      missionId,
      'orchestrator',
      policy.totalTokens,
      [],
      'medium',
      _userPrompt
    );
    assembledOptions.localTurns = policy.localTurns;

    if (policy.includeRecentTurns) {
      return this.contextManager.getAssembledContextText(assembledOptions);
    }

    return this.contextManager.getAssembledContextText(assembledOptions, {
      excludePartTypes: ['recent_turns'],
    });
  }

  /**
   * 记录 Worker 执行的 Token 使用（按 Assignment 维度）
   * 从 ExecutionResult.assignmentResults 中遍历每个 assignment，
   * 将其 tokenUsage 写入 executionStats
   */
  private recordWorkerTokenUsage(
    assignmentResults: Map<string, import('../worker').AutonomousExecutionResult>
  ): void {
    for (const [assignmentId, assignmentResult] of assignmentResults) {
      const tokenUsage = assignmentResult.tokenUsage;
      if (!tokenUsage || (tokenUsage.inputTokens === 0 && tokenUsage.outputTokens === 0)) {
        continue;
      }

      this.executionStats.recordExecution({
        worker: assignmentResult.assignment.workerId,
        taskId: assignmentId,
        subTaskId: 'assignment',
        success: assignmentResult.success,
        duration: assignmentResult.totalDuration,
        inputTokens: tokenUsage.inputTokens,
        outputTokens: tokenUsage.outputTokens,
        phase: 'execution',
      });
    }
  }

  /**
   * 记录编排器 Token 使用
   */
  recordOrchestratorTokens(usage?: TokenUsage, phase: 'planning' | 'verification' = 'planning'): void {
    if (usage) {
      this.orchestratorTokens.inputTokens += usage.inputTokens || 0;
      this.orchestratorTokens.outputTokens += usage.outputTokens || 0;

      // 同时记录到 ExecutionStats（编排器使用 claude）
      this.executionStats.recordExecution({
        worker: 'orchestrator',
        taskId: 'orchestrator',
        subTaskId: phase,
        success: true,
        duration: 0,
        inputTokens: usage.inputTokens,
        outputTokens: usage.outputTokens,
        phase,
      });
    }
  }

  async recordContextMessage(
    role: 'user' | 'assistant' | 'system',
    content: string,
    sessionId?: string
  ): Promise<void> {
    if (!content) {
      return;
    }
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (!resolvedSessionId) {
      return;
    }
    await this.ensureContextReady(resolvedSessionId);
    this.contextManager.addMessage({ role, content });

    // 【新增】用户消息时，记录到 Memory 的 userMessages 字段
    if (role === 'user') {
      // 检测是否为关键指令（包含决策性关键词）
      const keyInstruction = isKeyInstruction(content);
      this.contextManager.addUserMessage(content, keyInstruction);
      for (const constraint of extractUserConstraints(content)) {
        this.contextManager.addUserConstraint(constraint);
      }

      // 如果是首条用户消息，尝试提取核心意图
      const memory = this.contextManager.getMemoryDocument();
      if (memory && !memory.getContent().primaryIntent) {
        const intent = extractPrimaryIntent(content);
        if (intent) {
          this.contextManager.setPrimaryIntent(intent);
        }
      }
    }

    this.contextManager.scheduleMemorySave();
  }

  async recordStreamingMessage(
    messageId: string,
    role: 'user' | 'assistant' | 'system',
    content: string,
    sessionId?: string
  ): Promise<void> {
    if (!messageId || !content) {
      return;
    }
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (!resolvedSessionId) {
      return;
    }
    await this.ensureContextReady(resolvedSessionId);
    this.contextManager.updateStreamingMessage(messageId, { role, content });
  }

  clearStreamingMessage(messageId: string): void {
    if (!messageId) {
      return;
    }
    this.contextManager.clearStreamingMessage(messageId);
  }

  async recordToolOutput(toolName: string, output: string, sessionId?: string): Promise<void> {
    if (!toolName || !output) {
      return;
    }
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (!resolvedSessionId) {
      return;
    }
    await this.ensureContextReady(resolvedSessionId);
    this.contextManager.addToolOutput(toolName, output);
    this.contextManager.scheduleMemorySave();
  }

  /**
   * 取消执行
   */
  async cancel(): Promise<void> {
    const pendingConfirmation = this.pendingPlanConfirmation as {
      sessionId: string;
      planId: string;
      resolve: (confirmed: boolean) => void;
    } | null;
    if (pendingConfirmation) {
      pendingConfirmation.resolve(false);
      this.pendingPlanConfirmation = null;
    }

    // C-09: 取消活跃的 DispatchBatch，信号链传递到所有 Worker
    const activeBatch = this.dispatchManager.getActiveBatch();
    if (activeBatch && activeBatch.status === 'active') {
      const runningWorkers = activeBatch.getEntries()
        .filter(e => e.status === 'running')
        .map(e => e.worker);

      activeBatch.cancelAll('用户取消');

      // 中断所有正在执行的 Worker LLM 请求
      for (const worker of runningWorkers) {
        try {
          await this.adapterFactory.interrupt(worker);
        } catch { /* 中断失败不阻塞 */ }
      }
    }

    this.isRunning = false;
    this.setState('idle');
    this.currentTaskId = null;
  }

  /**
   * 中断当前任务（别名为 cancel）
   */
  async interrupt(): Promise<void> {
    await this.cancel();
  }

  /**
   * 获取统计摘要
   */
  getStatsSummary(): string {
    const { inputTokens, outputTokens } = this.orchestratorTokens;
    return `编排器 Token 使用: 输入 ${inputTokens}, 输出 ${outputTokens}`;
  }

  /**
   * 获取编排器 Token 使用
   */
  getOrchestratorTokenUsage(): {
    inputTokens: number;
    outputTokens: number;
  } {
    return { ...this.orchestratorTokens };
  }

  /**
   * 重置编排器 Token 使用
   */
  resetOrchestratorTokenUsage(): void {
    this.orchestratorTokens = {
      inputTokens: 0,
      outputTokens: 0,
    };
  }

  async reloadCompressionAdapter(): Promise<void> {
    await configureResilientAuxiliary(this.contextManager, this.executionStats);
  }

  /**
   * 设置扩展上下文
   */
  setExtensionContext(_context: import('vscode').ExtensionContext): void {
    this.executionStats.setContext(_context);
  }

  /**
   * 获取 MissionOrchestrator
   */
  getMissionOrchestrator(): MissionOrchestrator {
    return this.missionOrchestrator;
  }

  getExecutionStats(): ExecutionStats {
    return this.executionStats;
  }

  // ============================================================================
  // 任务视图方法（统一 Todo 系统 - 替代 UnifiedTaskManager）
  // ============================================================================

  getTaskViewService(): TaskViewService {
    return this.taskViewService;
  }

  // 委托方法 — 保持公共 API 兼容，调用方逐步迁移到 TaskViewService
  async listTaskViews(sessionId: string) { return this.taskViewService.listTaskViews(sessionId); }
  async createTaskFromPrompt(sessionId: string, prompt: string) { return this.taskViewService.createTaskFromPrompt(sessionId, prompt); }
  async cancelTaskById(taskId: string) { return this.taskViewService.cancelTaskById(taskId); }
  async deleteTaskById(taskId: string) { return this.taskViewService.deleteTaskById(taskId); }
  async failTaskById(taskId: string, error: string) { return this.taskViewService.failTaskById(taskId, error); }
  async completeTaskById(taskId: string) { return this.taskViewService.completeTaskById(taskId); }
  async markTaskExecuting(taskId: string) { return this.taskViewService.markTaskExecuting(taskId); }

  /**
   * 启动任务：加载已有 draft mission 并触发统一执行链路
   */
  async startTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (!mission) {
      throw new Error(`任务不存在: ${taskId}`);
    }
    if (!mission.userPrompt?.trim()) {
      throw new Error(`任务缺少执行内容: ${taskId}`);
    }
    const { userPrompt, sessionId } = mission;
    // 触发统一执行链路（执行成功后再迁移原 draft 状态，避免先删后跑导致任务丢失）
    await this.execute(userPrompt, taskId, sessionId);
    try {
      // 状态迁移：将 draft 标记为已取消（被执行链路替代），保留审计记录
      const draftMission = await this.missionStorage.load(taskId);
      if (draftMission) {
        draftMission.status = 'cancelled';
        draftMission.updatedAt = Date.now();
        await this.missionStorage.update(draftMission);
      }
    } catch (error) {
      logger.warn('编排器.任务.草稿状态迁移失败', {
        taskId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  /**
   * 销毁引擎
   */
  dispose(): void {
    this.dispatchManager.dispose();
    this.messageHub.dispose();
    this.missionOrchestrator.dispose();
    this.removeAllListeners();
    logger.info('编排器.任务引擎.销毁.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  // ============================================================================
  // 私有方法
  // ============================================================================
  private async ensureContextReady(sessionId: string): Promise<void> {
    if (!sessionId) {
      return;
    }
    this.contextManager.setSessionManager(this.sessionManager);
    this.contextManager.setCurrentSessionId(sessionId);
    if (this.contextSessionId !== sessionId) {
      const session = this.sessionManager.getSession(sessionId) || this.sessionManager.getCurrentSession();
      const sessionName = session?.name || session?.id || sessionId;
      await this.contextManager.initialize(sessionId, sessionName);
      this.contextSessionId = sessionId;
      logger.info('编排器.上下文.已初始化', { sessionId, sessionName }, LogCategory.ORCHESTRATOR);
    }
  }

}
