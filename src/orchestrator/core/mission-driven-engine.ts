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
import fs from 'fs';
import path from 'path';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { t } from '../../i18n';
import { UnifiedSessionManager } from '../../session/unified-session-manager';
import { SnapshotManager } from '../../snapshot-manager';
import { ContextManager } from '../../context/context-manager';
import { GovernedKnowledgeContextService } from '../../knowledge/governed-knowledge-context-service';
import { logger, LogCategory } from '../../logging';
import { PermissionMatrix, StrategyConfig, WorkerSlot, InteractionMode, INTERACTION_MODE_CONFIGS, InteractionModeConfig } from '../../types';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { OrchestratorState, RequirementAnalysis } from '../protocols/types';
import type { WorkerReport } from '../protocols/worker-report';
import { VerificationRunner, VerificationConfig } from '../verification-runner';
import { MissionOrchestrator } from './mission-orchestrator';
import {
  AcceptanceCriterion,
  AcceptanceExecutionReport,
  Mission,
  MissionStorageManager,
  FileBasedMissionStorage,
  MissionDeliveryStatus,
  MissionContinuationPolicy,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { MessageHub } from './message-hub';
import { WisdomManager } from '../wisdom';
import { buildAnalysisSystemPrompt, buildDirectResponseSystemPrompt, buildUnifiedSystemPrompt } from '../prompts/orchestrator-prompts';
import { isAbortError } from '../../errors';
import { ConfigManager, type OrchestratorGovernanceThresholdsConfig } from '../../config';
import { SupplementaryInstructionQueue } from './supplementary-instruction-queue';
import { DispatchManager } from './dispatch-manager';
import { configureResilientAuxiliary } from './resilient-auxiliary-adapter';
import {
  FileTerminationMetricsRepository,
  type TerminationMetricsRecord,
  type TerminationMetricsRepository,
} from './termination-metrics-repository';
import {
  FileRequestClassificationCalibrationStore,
  buildRequestClassificationDecisionRecord,
  type RequestClassificationCalibrationStore,
} from './request-classification-calibration';
import {
  resolveTerminationReason,
  type TerminationCandidate,
} from '../../llm/adapters/orchestrator-termination';
import { TaskViewService } from '../../services/task-view-service';
import { PlanLedgerService, type PlanMode, type PlanRecord, type PlanRuntimePhaseState } from '../plan-ledger';
import {
  createWisdomStorage,
  extractPrimaryIntent,
  extractUserConstraints,
  isKeyInstruction,
  resolveOrchestratorContextPolicy,
} from './mission-driven-engine-helpers';
import { normalizeNextSteps } from '../../utils/content-parser';
import {
  isGovernanceAutoRecoverReason,
  type ReplanSource,
} from './recovery-decision-kernel';
import { resolveEffectiveMode, type EffectiveModeResolution } from './effective-mode-resolver';
import { classifyRequest } from './request-classifier';
import {
  OrchestrationReadModelService,
  OrchestrationRuntimeDiagnosticsService,
  type OrchestrationRuntimeDiagnosticsQuery,
  type OrchestrationRuntimeDiagnosticsSnapshot,
  OrchestrationTimelineStore,
  type MissionPlanScope,
  type OrchestrationStateDiff,
  ExecutionChainStore,
  ExecutionChainQueryService,
  ResumeSnapshotStore,
  ResumeSnapshotBuilder,
} from '../runtime';
import type { ExecutionChainSessionSnapshot } from '../runtime/execution-chain-store';
import type { ResumeSnapshot } from '../runtime/resume-snapshot-types';
import { resolveOrchestrationEntry } from './orchestration-entry-router';
import {
  OrchestrationPlanController,
  type OrchestrationPlanControllerDependencies,
} from './orchestration-plan-controller';
import {
  OrchestrationRuntimeLoopController,
  type OrchestrationRuntimeLoopControllerDependencies,
} from './orchestration-runtime-loop-controller';
import type {
  PlanGovernanceAssessment,
  ResolvedOrchestratorTerminationReason,
  RuntimeTerminationDecisionTraceEntry,
  RuntimeTerminationShadow,
  RuntimeTerminationSnapshot,
} from './orchestration-control-plane-types';
import {
  mergeOrchestrationTraceLinks,
  type OrchestrationTraceLinks,
} from '../trace/types';
import type { RuntimeHostContext } from '../../host';

/**
 * 引擎配置
 */
export interface MissionDrivenEngineConfig {
  timeout: number;
  maxRetries: number;
  delivery?: {
    autoRepairMaxRounds?: number;
  };
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

interface GovernanceSignalEstimate {
  files: Set<string>;
  modules: Set<string>;
  available: boolean;
}

/**
 * MissionDrivenEngine - 基于 Mission-Driven Architecture 的编排引擎
 */
export class MissionDrivenEngine extends EventEmitter {
  private static readonly GOVERNANCE_THRESHOLDS_DEFAULT: OrchestratorGovernanceThresholdsConfig = {
    C_min: 0.55,
    C_ok: 0.75,
    R_low: 0.35,
    R_high: 0.70,
  };
  private static readonly REPLAN_GATE_REVIEW_ROUND_THRESHOLD = 3;
  private static readonly REPLAN_GATE_PENDING_REQUIRED_THRESHOLD = 3;

  private adapterFactory: IAdapterFactory;
  private readonly host: RuntimeHostContext;
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
  private readonly readModelService: OrchestrationReadModelService;
  private readonly timelineStore: OrchestrationTimelineStore;
  private readonly runtimeDiagnosticsService: OrchestrationRuntimeDiagnosticsService;
  private readonly planTimelineStateCache = new Map<string, Record<string, unknown>>();
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private readonly terminationMetricsRepository: TerminationMetricsRepository;
  private readonly requestClassificationCalibrationStore: RequestClassificationCalibrationStore;

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
  /** dispatch 并发场景下的 Mission 创建单飞锁，确保每轮最多创建一个 Mission */
  private ensureMissionPromise: Promise<string> | null = null;
  /** 当前轮次 draft 前唯一结构化需求合同 */
  private currentRequirementAnalysis: RequirementAnalysis | null = null;
  private lastRoutingDecision: {
    category?: string;
    categories?: string[];
    delegationBriefings?: string[];
    requiresModification?: boolean;
    executionMode?: RequirementAnalysis['executionMode'];
    entryPath?: RequirementAnalysis['entryPath'];
    historyMode?: RequirementAnalysis['historyMode'];
    includeThinking?: boolean;
    includeToolCalls?: boolean;
    decisionFactors?: string[];
    reason?: string;
  } | null = null;

  // Token 统计
  private orchestratorTokens = {
    inputTokens: 0,
    outputTokens: 0,
  };

  private lastExecutionErrors: string[] = [];
  // 初始值为 undefined，表示尚未执行过；避免首次查询诊断时误报"已完成"
  private lastExecutionRuntimeReason: ResolvedOrchestratorTerminationReason | undefined = undefined;
  private lastExecutionFinalStatus: 'completed' | 'failed' | 'cancelled' | 'paused' | undefined = undefined;
  private lastExecutionRuntimeSnapshot: RuntimeTerminationSnapshot | null = null;
  private lastExecutionRuntimeDecisionTrace: RuntimeTerminationDecisionTraceEntry[] = [];
  private resumeMissionId: string | null = null;

  // 执行统计
  private executionStats: ExecutionStats;

  // 统一消息出口
  private messageHub: MessageHub;
  private currentSessionId?: string;
  private contextSessionId: string | null = null;

  // 当前执行的用户原始请求（Phase C 汇总引用）
  private activeUserPrompt: string = '';
  // 当前编排轮次 requestId（用于 task_card 与工具链路统一时序锚点）
  private activeRoundRequestId: string | null = null;
  // 当前执行的用户原始图片路径（Worker dispatch 传递）
  private activeImagePaths?: string[];

  // 交互模式
  private interactionMode: InteractionMode = 'auto';
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.auto;
  private workspaceFileIndexCache?: { builtAt: number; files: string[]; modules: string[] };
  private workspaceFileIndexPromise?: Promise<{ files: string[]; modules: string[] }>;
  // 运行状态
  private isRunning = false;
  private executionQueue: Promise<void> = Promise.resolve();
  private pendingCount = 0;

  // P0-3: 补充指令队列（独立状态机）
  private supplementaryQueue: SupplementaryInstructionQueue;

  // P1-4: Dispatch 调度管理器（独立子系统）
  private dispatchManager: DispatchManager;
  private readonly planController: OrchestrationPlanController;
  private readonly runtimeLoopController: OrchestrationRuntimeLoopController;
  private readonly configManager: ConfigManager;

  // 执行链真相源
  private readonly executionChainStore: ExecutionChainStore;
  private readonly executionChainQuery: ExecutionChainQueryService;
  private readonly resumeSnapshotStore: ResumeSnapshotStore;
  private readonly resumeSnapshotBuilder: ResumeSnapshotBuilder;
  /** 当前执行的执行链 ID */
  private activeChainId: string | null = null;

  constructor(
    adapterFactory: IAdapterFactory,
    config: MissionDrivenEngineConfig,
    host: RuntimeHostContext,
  ) {
    super();
    this.adapterFactory = adapterFactory;
    this.config = config;
    this.host = host;
    this.workspaceRoot = host.workspaceRoot;
    this.snapshotManager = host.snapshotManager;
    this.sessionManager = host.sessionManager;
    this.configManager = ConfigManager.getInstance();

    // 初始化基础组件
    this.profileLoader = ProfileLoader.getInstance();
    this.guidanceInjector = new GuidanceInjector();
    this.contextManager = new ContextManager(this.workspaceRoot, undefined, this.sessionManager);
    this.executionStats = new ExecutionStats({
      storagePath: path.join(this.workspaceRoot, '.magi', 'runtime', 'execution-stats.json'),
    });
    this.supplementaryQueue = new SupplementaryInstructionQueue(this);
    this.wisdomManager = new WisdomManager();
    this.terminationMetricsRepository = new FileTerminationMetricsRepository(this.workspaceRoot);
    this.requestClassificationCalibrationStore = new FileRequestClassificationCalibrationStore(this.workspaceRoot);

    // 初始化 Mission 存储（使用 .magi/sessions 目录，按 session 分组存储）
    const sessionsDir = path.join(this.workspaceRoot, '.magi', 'sessions');
    const fileStorage = new FileBasedMissionStorage(sessionsDir);
    this.missionStorage = new MissionStorageManager(fileStorage);
    this.planLedger = new PlanLedgerService(this.sessionManager);
    this.readModelService = new OrchestrationReadModelService(this.missionStorage, this.planLedger);
    this.timelineStore = new OrchestrationTimelineStore(this.workspaceRoot);

    // 初始化执行链真相源
    this.executionChainStore = new ExecutionChainStore();
    this.executionChainQuery = new ExecutionChainQueryService(this.executionChainStore);
    this.resumeSnapshotStore = new ResumeSnapshotStore();
    this.resumeSnapshotBuilder = new ResumeSnapshotBuilder(this.resumeSnapshotStore);

    // 注册 session 保存前回调：在持久化前注入执行链数据
    this.sessionManager.setBeforeSaveHook((session) => {
      const snapshot = this.executionChainStore.exportSession(session.id);
      if (snapshot) {
        session.executionChains = snapshot;
        const chainIds = snapshot.chains.map(c => c.id);
        const resumeSnapshots = this.resumeSnapshotStore.exportByChainIds(chainIds);
        if (resumeSnapshots.length > 0) {
          session.resumeSnapshots = resumeSnapshots;
        }
      }
    });

    // 注册 session 加载后回调：从持久化数据恢复执行链
    this.sessionManager.setAfterLoadHook((session) => {
      if (session.executionChains) {
        try {
          const snapshot = session.executionChains as ExecutionChainSessionSnapshot;
          if (snapshot.chains && Array.isArray(snapshot.chains)) {
            this.executionChainStore.importSession(session.id, snapshot);
            // 启动收敛：将 running/resuming 的孤链降级
            this.executionChainStore.convergeOnStartup(session.id, (chainId) => {
              return this.resumeSnapshotStore.getLatest(chainId) !== null;
            });
          }
        } catch (error) {
          logger.warn('编排器.执行链.恢复失败', {
            sessionId: session.id,
            error: error instanceof Error ? error.message : String(error),
          }, LogCategory.ORCHESTRATOR);
        }
      }
      if (session.resumeSnapshots) {
        try {
          const snapshots = session.resumeSnapshots as ResumeSnapshot[];
          if (Array.isArray(snapshots)) {
            this.resumeSnapshotStore.importSnapshots(snapshots);
          }
        } catch (error) {
          logger.warn('编排器.恢复快照.恢复失败', {
            sessionId: session.id,
            error: error instanceof Error ? error.message : String(error),
          }, LogCategory.ORCHESTRATOR);
        }
      }
    });

    this.runtimeDiagnosticsService = new OrchestrationRuntimeDiagnosticsService(
      this.missionStorage,
      this.planLedger,
      this.readModelService,
      this.timelineStore,
      this.workspaceRoot,
    );
    this.taskViewService = new TaskViewService(
      this.missionStorage,
      this.workspaceRoot,
      this.readModelService,
      () => this.missionOrchestrator.getTodoManager() ?? null,
    );

    // 初始化 Mission 编排器
    this.missionOrchestrator = new MissionOrchestrator(
      this.profileLoader,
      this.guidanceInjector,
      adapterFactory,
      this.contextManager,
      this.missionStorage,
      this.workspaceRoot,
      this.snapshotManager,
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
      gitHost: this.host.capabilities.git,
      getActiveUserPrompt: () => this.activeUserPrompt,
      getActiveRoundRequestId: () => this.activeRoundRequestId || undefined,
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
      getPlanLedger: () => this.planLedger ?? null,
      getCurrentPlanId: () => this.currentPlanId ?? null,
      onDispatchTaskRegistered: (payload) => this.handleDispatchTaskRegistered(payload),
      onDispatchBatchCreated: (payload) => this.handleDispatchBatchCreated(payload),
    });

    const planControllerDeps: OrchestrationPlanControllerDependencies = {
      planLedger: this.planLedger,
      resolveEffectiveMode: (planningMode) => this.resolveCurrentEffectiveMode(planningMode),
      mergeRequirementAnalysisWithPlan: (base, plan) => this.mergeRequirementAnalysisWithPlan(base, plan),
      loadRecoveryPlanRecord: (input) => this.loadRecoveryPlanRecord(input),
      requirePlanMutation: (record, context) => this.requirePlanMutation(record, context),
      evaluatePlanGovernance: (sessionId, plan, userPrompt) => this.evaluatePlanGovernance(sessionId, plan, userPrompt),
      buildFallbackGovernanceAssessment: (error) => this.buildFallbackGovernanceAssessment(error),
    };
    this.planController = new OrchestrationPlanController(planControllerDeps);

    const runtimeLoopControllerDeps: OrchestrationRuntimeLoopControllerDependencies = {
      adapterFactory: this.adapterFactory,
      dispatchManager: this.dispatchManager,
      messageHub: this.messageHub,
      missionStorage: this.missionStorage,
      planLedger: this.planLedger,
      workspaceRoot: this.workspaceRoot,
      getVerificationRunner: () => this.verificationRunner,
      getAutoRepairMaxRounds: () => this.config.delivery?.autoRepairMaxRounds,
      onVerificationCompleted: (payload) => this.handleVerificationCompleted(payload),
      helpers: {
        getCurrentPlanId: () => this.currentPlanId,
        getLastMissionId: () => this.lastMissionId,
        setActiveRoundRequestId: (requestId) => {
          this.activeRoundRequestId = requestId;
        },
        setActiveUserPrompt: (prompt) => {
          this.activeUserPrompt = prompt;
        },
        recordOrchestratorTokens: (usage) => this.recordOrchestratorTokens(usage),
        normalizeOrchestratorRuntimeReason: (runtimeReason) => this.normalizeOrchestratorRuntimeReason(runtimeReason),
        resolveOrchestratorRuntimeReason: (input) => this.resolveOrchestratorRuntimeReason(input),
        resolveExecutionFinalStatus: (runtimeReason, runtimeSnapshot) => this.resolveExecutionFinalStatus(runtimeReason, runtimeSnapshot),
        isGovernancePauseReason: (reason) => this.isGovernancePauseReason(reason),
        resolveRequiredTotal: (snapshot) => this.resolveRequiredTotal(snapshot),
        resolveTerminalRequired: (snapshot) => this.resolveTerminalRequired(snapshot),
        extractPendingRequiredCount: (snapshot) => this.extractPendingRequiredCount(snapshot),
        buildFollowUpProgressSignature: (snapshot) => this.buildFollowUpProgressSignature(snapshot),
        mergeAcceptanceCriteriaWithExecutionReport: (input) => this.mergeAcceptanceCriteriaWithExecutionReport(input),
        buildAutoRepairPrompt: (input) => this.buildAutoRepairPrompt(input),
        buildGovernanceRecoveryPrompt: (input) => this.buildGovernanceRecoveryPrompt(input),
        resolveFollowUpSteps: (runtimeSteps) => this.resolveFollowUpSteps(runtimeSteps),
        extractStructuredContinuationStepsFromContent: (content) => this.extractStructuredContinuationStepsFromContent(content),
        classifyFollowUpSteps: (steps) => this.classifyFollowUpSteps(steps),
        buildFollowUpBlockedNotice: (steps) => this.buildFollowUpBlockedNotice(steps),
        buildPhaseRuntimePatch: (input) => this.buildPhaseRuntimePatch(input),
        resolvePhaseRuntimeForDecision: (current, patch) => this.resolvePhaseRuntimeForDecision(current, patch),
        stripNonActionableFollowUpSection: (content) => this.stripNonActionableFollowUpSection(content),
        markPhaseRuntimeRunning: (input) => this.markPhaseRuntimeRunning(input),
        beginSyntheticExecutionRound: (input) => this.beginSyntheticExecutionRound(input),
        buildAutoFollowUpPrompt: (input) => this.buildAutoFollowUpPrompt(input),
        buildGovernancePauseReport: (input) => this.buildGovernancePauseReport(input),
        formatGovernanceReason: (reason) => this.formatGovernanceReason(reason),
        buildExecutionFailureMessages: (runtimeReason, executionErrors) => this.buildExecutionFailureMessages(runtimeReason, executionErrors),
      },
    };
    this.runtimeLoopController = new OrchestrationRuntimeLoopController(runtimeLoopControllerDeps);

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

  private async evaluatePlanGovernance(sessionId: string, plan: PlanRecord, userPrompt: string): Promise<PlanGovernanceAssessment> {
    const analysisSignal = this.estimateAnalysisSignal(plan, userPrompt);
    const staticSignal = await this.estimateStaticSignal(userPrompt);
    const historical = this.estimateHistoricalSignal(sessionId, plan.planId);
    const effectivePlanningMode = this.resolveCurrentEffectiveMode(plan.mode).planningMode;

    const mergedFiles = new Set<string>([
      ...analysisSignal.files,
      ...staticSignal.files,
      ...historical.files,
    ]);
    const mergedModules = new Set<string>([
      ...analysisSignal.modules,
      ...staticSignal.modules,
      ...historical.modules,
    ]);

    const affectedFiles = Math.max(1, mergedFiles.size);
    const crossModules = Math.max(1, mergedModules.size);
    const writeToolRatio = this.estimateWriteToolRatio(userPrompt, effectivePlanningMode);
    const historicalFailureRate = historical.failureRate;

    const normalizeFiles = Math.min(affectedFiles / 40, 1);
    const normalizeModules = Math.min(crossModules / 8, 1);
    const riskScore = Math.min(
      1,
      0.35 * normalizeFiles
        + 0.25 * normalizeModules
        + 0.20 * writeToolRatio
        + 0.20 * historicalFailureRate,
    );

    const sourceCoverage = [analysisSignal.available, staticSignal.available, historical.available]
      .filter(Boolean).length / 3;
    const agreementPairs: number[] = [];
    if (analysisSignal.available && staticSignal.available) {
      agreementPairs.push(this.computeJaccard(analysisSignal.files, staticSignal.files));
    }
    if (analysisSignal.available && historical.available) {
      agreementPairs.push(this.computeJaccard(analysisSignal.modules, historical.modules));
    }
    if (staticSignal.available && historical.available) {
      agreementPairs.push(this.computeJaccard(staticSignal.modules, historical.modules));
    }
    const signalAgreement = agreementPairs.length > 0
      ? agreementPairs.reduce((sum, item) => sum + item, 0) / agreementPairs.length
      : 0;
    const historicalCalibration = historical.calibration;
    const confidence = Math.min(
      1,
      0.4 * sourceCoverage + 0.4 * signalAgreement + 0.2 * historicalCalibration,
    );

    const thresholds = this.getGovernanceThresholds();
    const reasons: string[] = [];
    let decision: 'ask' | 'auto' = 'ask';
    if (sourceCoverage < 2 / 3) {
      reasons.push(`coverage<2/3(${sourceCoverage.toFixed(2)})`);
    }
    if (confidence < thresholds.C_min) {
      reasons.push(`confidence<C_min(${confidence.toFixed(2)}<${thresholds.C_min})`);
    }
    if (riskScore >= thresholds.R_high) {
      reasons.push(`risk>=R_high(${riskScore.toFixed(2)}>=${thresholds.R_high})`);
    }
    if (
      reasons.length === 0
      && riskScore <= thresholds.R_low
      && confidence >= thresholds.C_ok
    ) {
      decision = 'auto';
    } else if (reasons.length === 0) {
      reasons.push(`gray_zone(risk=${riskScore.toFixed(2)},confidence=${confidence.toFixed(2)})`);
    }

    return {
      riskScore,
      confidence,
      affectedFiles,
      crossModules,
      writeToolRatio,
      historicalFailureRate,
      sourceCoverage,
      signalAgreement,
      historicalCalibration,
      decision,
      reasons,
    };
  }

  private getGovernanceThresholds(): OrchestratorGovernanceThresholdsConfig {
    const defaults = MissionDrivenEngine.GOVERNANCE_THRESHOLDS_DEFAULT;
    const manager = this.configManager ?? ConfigManager.getInstance();
    const configured = manager.get('orchestrator')?.governanceThresholds;
    const thresholds: OrchestratorGovernanceThresholdsConfig = {
      C_min: this.normalizeThresholdValue(configured?.C_min, defaults.C_min),
      C_ok: this.normalizeThresholdValue(configured?.C_ok, defaults.C_ok),
      R_low: this.normalizeThresholdValue(configured?.R_low, defaults.R_low),
      R_high: this.normalizeThresholdValue(configured?.R_high, defaults.R_high),
    };

    if (thresholds.C_min > thresholds.C_ok) {
      logger.warn('治理阈值无效，已回退默认值（C_min > C_ok）', {
        configured: thresholds,
        defaults,
      }, LogCategory.ORCHESTRATOR);
      thresholds.C_min = defaults.C_min;
      thresholds.C_ok = defaults.C_ok;
    }

    if (thresholds.R_low > thresholds.R_high) {
      logger.warn('治理阈值无效，已回退默认值（R_low > R_high）', {
        configured: thresholds,
        defaults,
      }, LogCategory.ORCHESTRATOR);
      thresholds.R_low = defaults.R_low;
      thresholds.R_high = defaults.R_high;
    }

    return thresholds;
  }

  private normalizeThresholdValue(raw: unknown, fallback: number): number {
    const value = Number(raw);
    if (!Number.isFinite(value) || value < 0 || value > 1) {
      return fallback;
    }
    return value;
  }

  private buildFallbackGovernanceAssessment(error: unknown): PlanGovernanceAssessment {
    const errorMessage = error instanceof Error ? error.message : String(error);
    const clipped = errorMessage.replace(/\s+/g, ' ').trim().slice(0, 160);
    return {
      riskScore: 1,
      confidence: 0,
      affectedFiles: 1,
      crossModules: 1,
      writeToolRatio: 1,
      historicalFailureRate: 0.5,
      sourceCoverage: 0,
      signalAgreement: 0,
      historicalCalibration: 0,
      decision: 'ask',
      reasons: [`assessment_error(${clipped || 'unknown'})`],
    };
  }

  private estimateWriteToolRatio(prompt: string, mode: PlanMode): number {
    const classification = classifyRequest(prompt, mode);
    if (classification.entryPolicy.entryPath !== 'task_execution') {
      return 0.15;
    }
    if (classification.requiresModification) {
      return mode === 'deep' ? 0.9 : 0.7;
    }
    const text = (prompt || '').toLowerCase();
    const readOnlyKeywords = ['总结', '分析', '读取', '解释', 'review', 'summarize', 'read only', 'diagnose'];
    if (readOnlyKeywords.some((keyword) => text.includes(keyword))) {
      return 0.2;
    }
    return mode === 'deep' ? 0.55 : 0.45;
  }

  private resolveCurrentEffectiveMode(planningMode: PlanMode) {
    return resolveEffectiveMode({
      interactionMode: this.interactionMode,
      planningMode,
      modelCapability: this.adapterFactory.getOrchestratorModelCapability?.(),
    });
  }

  private resolveRequestedPlanningMode(): PlanMode {
    return this.adapterFactory.getRequestedPlanningMode();
  }

  private estimateAnalysisSignal(plan: PlanRecord, prompt: string): GovernanceSignalEstimate {
    const files = new Set<string>();
    const modules = new Set<string>();
    for (const item of plan.items) {
      for (const file of item.targetFiles || []) {
        const normalized = this.normalizeRelativePath(file);
        if (!normalized) {
          continue;
        }
        files.add(normalized);
        modules.add(this.inferModuleFromPath(normalized));
      }
    }
    for (const candidate of this.extractPathLikeCandidates(prompt)) {
      const normalized = this.normalizeRelativePath(candidate);
      if (!normalized) {
        continue;
      }
      files.add(normalized);
      modules.add(this.inferModuleFromPath(normalized));
    }
    return {
      files,
      modules,
      available: files.size > 0 || modules.size > 0,
    };
  }

  private async estimateStaticSignal(prompt: string): Promise<GovernanceSignalEstimate> {
    const files = new Set<string>();
    const modules = new Set<string>();
    const tokens = this.extractPromptTokens(prompt);
    if (tokens.length === 0) {
      return { files, modules, available: false };
    }
    const index = await this.getWorkspaceFileIndex();

    for (const file of index.files) {
      const lower = file.toLowerCase();
      if (tokens.some((token) => lower.includes(token))) {
        files.add(file);
        modules.add(this.inferModuleFromPath(file));
        if (files.size >= 60) {
          break;
        }
      }
    }

    if (files.size === 0) {
      for (const moduleName of index.modules) {
        const lower = moduleName.toLowerCase();
        if (tokens.some((token) => lower.includes(token) || token.includes(lower))) {
          modules.add(moduleName);
        }
      }
    }

    return {
      files,
      modules,
      available: files.size > 0 || modules.size > 0,
    };
  }

  private estimateHistoricalSignal(sessionId: string, currentPlanId: string): {
    files: Set<string>;
    modules: Set<string>;
    failureRate: number;
    calibration: number;
    available: boolean;
  } {
    const files = new Set<string>();
    const modules = new Set<string>();
    const history = this.planLedger
      .listPlans(sessionId, 30)
      .filter((item) => item.planId !== currentPlanId);
    if (history.length === 0) {
      return {
        files,
        modules,
        failureRate: 0.15,
        calibration: 0.2,
        available: false,
      };
    }

    let failedCount = 0;
    for (const plan of history) {
      if (plan.status === 'failed' || plan.status === 'cancelled' || plan.status === 'partially_completed') {
        failedCount += 1;
      }
      for (const item of plan.items) {
        for (const file of item.targetFiles || []) {
          const normalized = this.normalizeRelativePath(file);
          if (!normalized) {
            continue;
          }
          files.add(normalized);
          modules.add(this.inferModuleFromPath(normalized));
          if (files.size >= 60) {
            break;
          }
        }
      }
    }

    const sampleCount = history.length;
    const failureRate = sampleCount > 0 ? Math.min(1, failedCount / sampleCount) : 0.15;
    const calibration = sampleCount >= 12
      ? 0.9
      : sampleCount >= 8
        ? 0.75
        : sampleCount >= 4
          ? 0.55
          : 0.35;

    return {
      files,
      modules,
      failureRate,
      calibration,
      available: sampleCount >= 3,
    };
  }

  private computeJaccard(left: Set<string>, right: Set<string>): number {
    if (left.size === 0 && right.size === 0) {
      return 1;
    }
    if (left.size === 0 || right.size === 0) {
      return 0;
    }
    let intersection = 0;
    for (const item of left) {
      if (right.has(item)) {
        intersection += 1;
      }
    }
    const union = left.size + right.size - intersection;
    if (union <= 0) {
      return 0;
    }
    return intersection / union;
  }

  private extractPathLikeCandidates(prompt: string): string[] {
    if (!prompt.trim()) {
      return [];
    }
    const matches = prompt.match(/[A-Za-z0-9._/-]+\.[A-Za-z0-9]+/g) || [];
    return Array.from(new Set(matches.map((item) => item.trim()).filter(Boolean)));
  }

  private extractPromptTokens(prompt: string): string[] {
    const normalized = (prompt || '').toLowerCase();
    const tokens = normalized.match(/[a-z0-9_]{3,}/g) || [];
    return Array.from(new Set(tokens)).slice(0, 24);
  }

  private async getWorkspaceFileIndex(): Promise<{ files: string[]; modules: string[] }> {
    const now = Date.now();
    if (this.workspaceFileIndexCache && now - this.workspaceFileIndexCache.builtAt < 60_000) {
      return {
        files: this.workspaceFileIndexCache.files,
        modules: this.workspaceFileIndexCache.modules,
      };
    }
    if (this.workspaceFileIndexPromise) {
      return await this.workspaceFileIndexPromise;
    }

    this.workspaceFileIndexPromise = (async () => {
      const files: string[] = [];
      const modules = new Set<string>();
      const excluded = new Set(['.git', 'node_modules', '.magi', 'out', 'dist', 'build', 'coverage']);
      const stack: Array<{ dir: string; depth: number }> = [{ dir: this.workspaceRoot, depth: 0 }];
      let visitedDirs = 0;

      while (stack.length > 0 && files.length < 5000) {
        const current = stack.pop();
        if (!current || current.depth > 12) {
          continue;
        }

        let entries: fs.Dirent[] = [];
        try {
          entries = await fs.promises.readdir(current.dir, { withFileTypes: true });
        } catch {
          continue;
        }

        visitedDirs += 1;
        if (visitedDirs % 8 === 0) {
          await new Promise<void>((resolve) => {
            setImmediate(resolve);
          });
        }

        for (const entry of entries) {
          if (files.length >= 5000) {
            break;
          }
          if (entry.name.startsWith('.') && entry.name !== '.env.example') {
            if (entry.name !== '.github' && entry.name !== '.vscode') {
              continue;
            }
          }
          if (excluded.has(entry.name)) {
            continue;
          }
          const absolutePath = path.join(current.dir, entry.name);
          if (entry.isDirectory()) {
            stack.push({ dir: absolutePath, depth: current.depth + 1 });
            continue;
          }
          const relative = this.normalizeRelativePath(path.relative(this.workspaceRoot, absolutePath));
          if (!relative) {
            continue;
          }
          files.push(relative);
          modules.add(this.inferModuleFromPath(relative));
        }
      }

      const moduleList = Array.from(modules).filter(Boolean);
      this.workspaceFileIndexCache = {
        builtAt: Date.now(),
        files,
        modules: moduleList,
      };
      return { files, modules: moduleList };
    })().finally(() => {
      this.workspaceFileIndexPromise = undefined;
    });

    return await this.workspaceFileIndexPromise;
  }

  private normalizeRelativePath(input: string): string {
    const normalized = input.replace(/\\/g, '/').replace(/^\.\//, '').trim();
    return normalized;
  }

  private inferModuleFromPath(file: string): string {
    const normalized = this.normalizeRelativePath(file);
    if (!normalized) {
      return '';
    }
    const first = normalized.split('/')[0] || '';
    return first || normalized;
  }

  private persistTerminationMetrics(input: {
    sessionId?: string;
    planId?: string | null;
    turnId?: string | null;
    mode: PlanMode;
    finalPlanStatus: 'completed' | 'failed' | 'cancelled' | 'paused' | null;
    runtimeReason?: string;
    runtimeRounds?: number;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
    runtimeShadow?: RuntimeTerminationShadow;
    runtimeDecisionTrace?: RuntimeTerminationDecisionTraceEntry[];
    tokenUsage?: TokenUsage;
    startedAt: number;
  }): void {
    const sessionId = input.sessionId?.trim();
    if (!sessionId) {
      return;
    }

    const snapshot = input.runtimeSnapshot;
    const durationMsFromBudget = snapshot?.budgetState?.elapsedMs;
    const tokenUsedFromBudget = snapshot?.budgetState?.tokenUsed;
    const durationMs = Number.isFinite(durationMsFromBudget)
      ? Number(durationMsFromBudget)
      : Math.max(0, Date.now() - input.startedAt);
    const tokenUsed = Number.isFinite(tokenUsedFromBudget)
      ? Number(tokenUsedFromBudget)
      : (input.tokenUsage?.inputTokens || 0) + (input.tokenUsage?.outputTokens || 0);

    const record: TerminationMetricsRecord = {
      timestamp: new Date().toISOString(),
      session_id: sessionId,
      plan_id: input.planId || null,
      turn_id: input.turnId || null,
      mode: input.mode,
      final_status: input.finalPlanStatus || 'unknown',
      reason: input.runtimeReason || 'unknown',
      rounds: Math.max(0, input.runtimeRounds || 0),
      duration_ms: durationMs,
      token_used: tokenUsed,
      evidence_ids: Array.isArray(snapshot?.sourceEventIds) ? snapshot?.sourceEventIds : [],
      progress_vector: snapshot?.progressVector || null,
      review_state: snapshot?.reviewState || null,
      blocker_state: snapshot?.blockerState || null,
      budget_state: snapshot?.budgetState || null,
      required_total: snapshot?.requiredTotal ?? null,
      failed_required: snapshot?.failedRequired ?? null,
      running_or_pending_required: snapshot?.runningOrPendingRequired ?? null,
      shadow: input.runtimeShadow || null,
      decision_trace: Array.isArray(input.runtimeDecisionTrace) ? input.runtimeDecisionTrace : null,
    };
    this.terminationMetricsRepository.append(record);
  }

  /**
   * 核心层会话上下文同步：
   * 不依赖 UI/宿主适配层，确保 MessageHub 的 sessionId 与当前会话一致，
   * 同时将默认 trace 对齐到当前活动会话的启动上下文。
   */
  private syncMessageHubSessionContext(sessionId?: string): void {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (!normalizedSessionId) {
      return;
    }
    const traceAligned = this.messageHub.getTraceId() === normalizedSessionId;
    const sessionAligned = this.messageHub.getSessionId() === normalizedSessionId;
    if (traceAligned && sessionAligned) {
      return;
    }
    if (!sessionAligned) {
      this.messageHub.setSessionId(normalizedSessionId);
    }
    if (!traceAligned) {
      this.messageHub.setTraceId(normalizedSessionId);
    }
    logger.debug('编排器.消息.会话上下文同步', {
      sessionId: normalizedSessionId,
      traceId: this.messageHub.getTraceId(),
    }, LogCategory.ORCHESTRATOR);
  }

  private appendTimelineEvent(input: {
    type: string;
    summary: string;
    trace?: Partial<OrchestrationTraceLinks> | null;
    payload?: Record<string, unknown>;
    diffs?: OrchestrationStateDiff[];
    timestamp?: number;
  }): void {
    try {
      this.timelineStore.append({
        type: input.type,
        summary: input.summary,
        trace: input.trace,
        payload: input.payload,
        diffs: input.diffs,
        timestamp: input.timestamp,
      });
    } catch (error) {
      logger.warn('编排器.timeline.事件写入失败', {
        type: input.type,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private snapshotPlanTimelineState(record: PlanRecord): Record<string, unknown> {
    return {
      status: record.status,
      reviewState: record.runtime.review.state,
      reviewRound: record.runtime.review.round,
      acceptanceSummary: record.runtime.acceptance.summary,
      replanState: record.runtime.replan.state,
      waitState: record.runtime.wait.state,
      phaseState: record.runtime.phase.state,
      phaseTitle: record.runtime.phase.currentTitle,
      terminationReason: record.runtime.termination.reason,
      itemTotal: record.items.length,
      completedItems: record.items.filter((item) => item.status === 'completed' || item.status === 'skipped').length,
      failedItems: record.items.filter((item) => item.status === 'failed' || item.status === 'cancelled').length,
      attemptTotal: record.attempts.length,
      inflightAttempts: record.attempts.filter((attempt) => attempt.status === 'created' || attempt.status === 'inflight').length,
    };
  }

  private appendPlanLedgerTimelineEvent(event: {
    sessionId: string;
    reason: string;
    record: PlanRecord;
  }): void {
    const planKey = `${event.sessionId}:${event.record.planId}`;
    const previousState = this.planTimelineStateCache.get(planKey);
    const nextState = this.snapshotPlanTimelineState(event.record);
    this.planTimelineStateCache.set(planKey, nextState);
    const stateChanged = JSON.stringify(previousState || null) !== JSON.stringify(nextState);
    this.appendTimelineEvent({
      type: 'plan.runtime.updated',
      summary: `计划运行态更新：${event.reason}`,
      trace: mergeOrchestrationTraceLinks({
        sessionId: event.sessionId,
        planId: event.record.planId,
        missionId: event.record.missionId,
        turnId: event.record.turnId,
      }, this.currentSessionId === event.sessionId
        ? { requestId: this.activeRoundRequestId || undefined }
        : undefined),
      payload: {
        reason: event.reason,
        revision: event.record.revision,
        version: event.record.version,
        status: event.record.status,
      },
      diffs: stateChanged
        ? [{
          entityType: 'plan',
          entityId: event.record.planId,
          before: previousState,
          after: nextState,
        }]
        : undefined,
    });
  }

  private handleDispatchBatchCreated(payload: {
    trace: OrchestrationTraceLinks;
    userPrompt: string;
  }): void {
    // 在执行链中创建 AssignmentGroup
    if (this.activeChainId) {
      const group = this.executionChainStore.createAssignmentGroup({
        chainId: this.activeChainId,
        dispatchBatchId: payload.trace.batchId,
      });
      logger.info('编排器.执行链.AssignmentGroup已创建', {
        chainId: this.activeChainId,
        groupId: group.id,
        batchId: payload.trace.batchId,
      }, LogCategory.ORCHESTRATOR);
    }

    this.appendTimelineEvent({
      type: 'dispatch.batch.created',
      summary: '派发批次已创建',
      trace: payload.trace,
      payload: {
        userPrompt: payload.userPrompt,
      },
      diffs: [{
        entityType: 'batch',
        entityId: payload.trace.batchId || 'unknown-batch',
        before: undefined,
        after: {
          phase: 'active',
          requestId: payload.trace.requestId,
          missionId: payload.trace.missionId,
        },
      }],
    });
  }

  private handleVerificationCompleted(payload: {
    sessionId: string;
    planId?: string;
    batchId: string;
    trace?: OrchestrationTraceLinks;
    outcome: AcceptanceExecutionReport;
  }): void {
    this.appendTimelineEvent({
      type: 'delivery.verification.completed',
      summary: `交付验收已结束：${payload.outcome.status}`,
      trace: mergeOrchestrationTraceLinks(payload.trace, {
        sessionId: payload.sessionId,
        planId: payload.planId,
        batchId: payload.batchId,
      }),
      payload: {
        status: payload.outcome.status,
        summary: payload.outcome.summary,
        skippedReason: payload.outcome.skippedReason,
        warningCount: payload.outcome.warnings?.length || 0,
      },
      diffs: [{
        entityType: 'verification',
        entityId: payload.outcome.trace?.verificationId || `verification:${payload.batchId}`,
        before: undefined,
        after: {
          status: payload.outcome.status,
          summary: payload.outcome.summary,
          skippedReason: payload.outcome.skippedReason,
        },
      }],
    });
  }

  private emitPlanLedgerUpdate(sessionId: string, reason: string): void {
    this.syncMessageHubSessionContext(sessionId);
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
    ownership: string;
    mode: string;
    dependsOn?: string[];
    scopeHint?: string[];
    files?: string[];
    requiresModification: boolean;
    missionTitle?: string;
  }): Promise<void> {
    if (!this.currentPlanId) {
      return;
    }

    // 如果编排者提供了 mission_title，回写为计划摘要（替换初始的用户消息截取）
    if (payload.missionTitle) {
      await this.planLedger.updateSummary(payload.sessionId, this.currentPlanId, payload.missionTitle);
      // 同步回写 Mission.title（语义化任务名称的唯一真相源）
      await this.missionStorage.updateTitle(payload.missionId, payload.missionTitle);
    }

    await this.planLedger.upsertDispatchItem(payload.sessionId, this.currentPlanId, {
      itemId: payload.taskId,
      title: payload.title,
      worker: payload.worker,
      category: payload.ownership,
      dependsOn: payload.dependsOn,
      scopeHints: payload.scopeHint,
      targetFiles: payload.files,
      requiresModification: payload.requiresModification,
    });

    // 在执行链中创建 Worker 分支
    if (this.activeChainId) {
      const chain = this.executionChainStore.getChain(this.activeChainId);
      const mainlineBranch = this.executionChainQuery.getMainlineBranch(this.activeChainId);
      const activeGroupId = chain?.activeAssignmentGroupId;
      const branch = this.executionChainStore.createBranch({
        chainId: this.activeChainId,
        kind: 'worker',
        parentBranchId: mainlineBranch?.id,
        workerSlot: payload.worker,
        assignmentGroupId: activeGroupId,
      });
      if (activeGroupId) {
        this.executionChainStore.addBranchToAssignmentGroup(activeGroupId, branch.id);
      }
    }
  }

  private classifyAttemptTerminalStatus(success: boolean, message?: string): 'succeeded' | 'failed' | 'timeout' | 'cancelled' {
    if (success) {
      return 'succeeded';
    }

    const normalized = (message || '').toLowerCase();
    if (/timeout|timed out|deadline|超时|budget_exceeded|external_wait_timeout|stalled/.test(normalized)) {
      return 'timeout';
    }
    if (/cancel|cancelled|canceled|abort|aborted|interrupted|用户中断|外部中断/.test(normalized)) {
      return 'cancelled';
    }
    return 'failed';
  }

  private mapAttemptToAssignmentStatus(
    status: 'succeeded' | 'failed' | 'timeout' | 'cancelled',
  ): 'completed' | 'failed' | 'cancelled' {
    if (status === 'succeeded') {
      return 'completed';
    }
    if (status === 'cancelled') {
      return 'cancelled';
    }
    return 'failed';
  }

  private mapAttemptToTodoStatus(
    status: 'succeeded' | 'failed' | 'timeout' | 'cancelled',
  ): 'completed' | 'failed' | 'cancelled' {
    if (status === 'succeeded') {
      return 'completed';
    }
    if (status === 'cancelled') {
      return 'cancelled';
    }
    return 'failed';
  }

  private extractAssignmentFailureMessage(data: unknown): string | undefined {
    const payload = data as {
      error?: unknown;
      summary?: unknown;
      result?: { errors?: unknown };
    };

    if (typeof payload?.error === 'string' && payload.error.trim()) {
      return payload.error.trim();
    }
    if (Array.isArray(payload?.result?.errors)) {
      const merged = payload.result.errors
        .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
        .map((item) => item.trim())
        .join(' | ');
      if (merged) {
        return merged;
      }
    }
    if (typeof payload?.summary === 'string' && payload.summary.trim()) {
      return payload.summary.trim();
    }
    return undefined;
  }

  private mapOrchestratorRuntimeReasonToAttemptStatus(
    runtimeReason?: string,
  ): 'succeeded' | 'failed' | 'timeout' | 'cancelled' {
    switch (runtimeReason) {
      case 'completed':
        return 'succeeded';
      case 'cancelled':
      case 'external_abort':
      case 'interrupted':
        return 'cancelled';
      case 'budget_exceeded':
      case 'external_wait_timeout':
      case 'stalled':
        return 'timeout';
      default:
        return 'failed';
    }
  }

  private normalizeOrchestratorRuntimeReason(
    runtimeReason?: string,
  ): ResolvedOrchestratorTerminationReason | undefined {
    switch (runtimeReason) {
      case 'completed':
      case 'failed':
      case 'cancelled':
      case 'stalled':
      case 'budget_exceeded':
      case 'external_wait_timeout':
      case 'external_abort':
      case 'upstream_model_error':
      case 'interrupted':
        return runtimeReason;
      default:
        return undefined;
    }
  }

  private resolveOrchestratorRuntimeReason(input: {
    runtimeReason?: string;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
    additionalCandidates?: TerminationCandidate[];
    fallback?: ResolvedOrchestratorTerminationReason;
  }): {
    reason: ResolvedOrchestratorTerminationReason;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
  } {
    const normalizedRuntimeReason = this.normalizeOrchestratorRuntimeReason(input.runtimeReason);
    const snapshotEvidenceIds = Array.isArray(input.runtimeSnapshot?.sourceEventIds)
      ? input.runtimeSnapshot.sourceEventIds.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
      : [];
    const candidates: TerminationCandidate[] = [];

    if (normalizedRuntimeReason) {
      if (snapshotEvidenceIds.length > 0) {
        candidates.push(...snapshotEvidenceIds.map((eventId, index) => ({
          reason: normalizedRuntimeReason,
          eventId,
          triggeredAt: index,
        })));
      } else {
        candidates.push({
          reason: normalizedRuntimeReason,
          eventId: `runtime:${normalizedRuntimeReason}`,
          triggeredAt: 0,
        });
      }
    }

    if (Array.isArray(input.additionalCandidates) && input.additionalCandidates.length > 0) {
      candidates.push(...input.additionalCandidates);
    }

    const resolved = resolveTerminationReason(
      candidates,
      input.fallback || normalizedRuntimeReason || 'failed',
    );

    return {
      reason: resolved.reason,
      runtimeSnapshot: input.runtimeSnapshot
        ? {
          ...input.runtimeSnapshot,
          sourceEventIds: resolved.evidenceIds,
        }
        : resolved.evidenceIds.length > 0
          ? { sourceEventIds: resolved.evidenceIds }
          : undefined,
    };
  }

  private resolveExecutionFinalStatus(
    runtimeReason?: ResolvedOrchestratorTerminationReason,
    runtimeSnapshot?: RuntimeTerminationSnapshot,
  ): 'completed' | 'failed' | 'cancelled' | 'paused' {
    const normalized = this.normalizeOrchestratorRuntimeReason(runtimeReason);
    if (normalized === 'cancelled' || normalized === 'external_abort' || normalized === 'interrupted') {
      return 'cancelled';
    }
    if (normalized === 'completed') {
      return 'completed';
    }

    const requiredTotal = this.resolveRequiredTotal(runtimeSnapshot);
    const terminalRequired = this.resolveTerminalRequired(runtimeSnapshot);
    const pendingRequired = this.extractPendingRequiredCount(runtimeSnapshot);
    const failedRequired = typeof runtimeSnapshot?.failedRequired === 'number' && Number.isFinite(runtimeSnapshot.failedRequired)
      ? runtimeSnapshot.failedRequired
      : 0;
    const hasRequired = typeof requiredTotal === 'number' && requiredTotal > 0;
    const resolvedAllRequired = hasRequired
      && typeof terminalRequired === 'number'
      && terminalRequired >= requiredTotal
      && pendingRequired === 0
      && failedRequired === 0;

    if (resolvedAllRequired) {
      return 'completed';
    }

    if (normalized && this.isGovernancePauseReason(normalized)) {
      return 'paused';
    }

    return 'failed';
  }

  private isGovernancePauseReason(reason: ResolvedOrchestratorTerminationReason): boolean {
    return reason === 'budget_exceeded'
      || reason === 'external_wait_timeout'
      || reason === 'stalled'
      || reason === 'upstream_model_error';
  }

  private resolveRuntimeAcceptanceSummary(
    runtimeReason: ResolvedOrchestratorTerminationReason,
    runtimeSnapshot?: RuntimeTerminationSnapshot,
    finalExecutionStatus?: 'completed' | 'failed' | 'cancelled' | 'paused',
  ): 'pending' | 'partial' | 'passed' | 'failed' {
    if (finalExecutionStatus === 'completed' || runtimeReason === 'completed') {
      return 'passed';
    }
    const failedRequired = typeof runtimeSnapshot?.failedRequired === 'number' && Number.isFinite(runtimeSnapshot.failedRequired)
      ? runtimeSnapshot.failedRequired
      : 0;
    if (failedRequired > 0 || finalExecutionStatus === 'failed' || runtimeReason === 'failed') {
      return 'failed';
    }
    const pendingRequired = this.extractPendingRequiredCount(runtimeSnapshot);
    const requiredTotal = this.resolveRequiredTotal(runtimeSnapshot) || 0;
    if (requiredTotal > 0 && pendingRequired > 0) {
      return 'partial';
    }
    return 'pending';
  }

  private buildRuntimeReplanReason(input: {
    runtimeReason: ResolvedOrchestratorTerminationReason;
    acceptanceSummary: 'pending' | 'partial' | 'passed' | 'failed';
    runtimeSnapshot?: RuntimeTerminationSnapshot;
  }): string {
    const parts = [`runtime=${input.runtimeReason}`, `acceptance=${input.acceptanceSummary}`];
    const pendingRequired = this.extractPendingRequiredCount(input.runtimeSnapshot);
    if (pendingRequired > 0) {
      parts.push(`pending_required=${pendingRequired}`);
    }
    const failedRequired = typeof input.runtimeSnapshot?.failedRequired === 'number' && Number.isFinite(input.runtimeSnapshot.failedRequired)
      ? input.runtimeSnapshot.failedRequired
      : 0;
    if (failedRequired > 0) {
      parts.push(`failed_required=${failedRequired}`);
    }
    const blockerState = input.runtimeSnapshot?.blockerState;
    const unresolvedBlockers = typeof blockerState?.open === 'number' && Number.isFinite(blockerState.open)
      ? Math.max(0, Math.floor(blockerState.open))
      : 0;
    if (unresolvedBlockers > 0) {
      parts.push(`unresolved_blockers=${unresolvedBlockers}`);
    }
    const externalWaitOpen = typeof blockerState?.externalWaitOpen === 'number' && Number.isFinite(blockerState.externalWaitOpen)
      ? Math.max(0, Math.floor(blockerState.externalWaitOpen))
      : 0;
    if (externalWaitOpen > 0) {
      parts.push(`external_wait_open=${externalWaitOpen}`);
    }
    return parts.join(';');
  }

  private async syncPlanRuntimeGovernanceState(input: {
    sessionId: string;
    planId: string;
    runtimeReason: ResolvedOrchestratorTerminationReason;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
    finalExecutionStatus: 'completed' | 'failed' | 'cancelled' | 'paused';
  }): Promise<void> {
    const acceptanceSummary = this.resolveRuntimeAcceptanceSummary(
      input.runtimeReason,
      input.runtimeSnapshot,
      input.finalExecutionStatus,
    );
    const externalWaitOpen = typeof input.runtimeSnapshot?.blockerState?.externalWaitOpen === 'number'
      && Number.isFinite(input.runtimeSnapshot.blockerState.externalWaitOpen)
      ? Math.max(0, Math.floor(input.runtimeSnapshot.blockerState.externalWaitOpen))
      : 0;
    const waitReasonCode = input.runtimeReason === 'external_wait_timeout'
      ? 'external_wait_timeout'
      : externalWaitOpen > 0
        ? 'external_wait_open'
        : undefined;
    const shouldRequireReplan = input.finalExecutionStatus !== 'completed'
      && (
        acceptanceSummary === 'failed'
        || acceptanceSummary === 'partial'
        || this.isGovernancePauseReason(input.runtimeReason)
      );
    const replanReason = shouldRequireReplan
      ? this.buildRuntimeReplanReason({
          runtimeReason: input.runtimeReason,
          acceptanceSummary,
          runtimeSnapshot: input.runtimeSnapshot,
        })
      : undefined;
    await this.planLedger.updateRuntimeState(input.sessionId, input.planId, {
      acceptance: { summary: acceptanceSummary },
      wait: waitReasonCode
        ? { state: 'external_waiting', reasonCode: waitReasonCode }
        : { state: 'none' },
      replan: shouldRequireReplan
        ? { state: 'required', reason: replanReason }
        : { state: 'none' },
      termination: {
        reason: input.runtimeReason,
        snapshotId: input.runtimeSnapshot?.snapshotId,
      },
    }, {
      auditReason: `runtime-governance:final-sync:${input.runtimeReason}`,
    });
  }

  private buildExecutionFailureMessages(
    runtimeReason: ResolvedOrchestratorTerminationReason,
    executionErrors: string[],
  ): string[] {
    const normalizedErrors = executionErrors
      .map(item => item.trim())
      .filter(item => item.length > 0);
    if (normalizedErrors.length > 0) {
      return normalizedErrors;
    }

    switch (runtimeReason) {
      case 'budget_exceeded':
        return ['执行达到预算上限'];
      case 'external_wait_timeout':
        return ['执行等待外部条件超时'];
      case 'stalled':
        return ['执行停滞，未取得有效进展'];
      case 'upstream_model_error':
        return ['执行遭遇上游模型错误'];
      case 'failed':
      default:
        return [t('provider.executionFailed')];
    }
  }

  private reportPlanLedgerAsyncError(action: string, error: unknown): void {
    logger.warn('编排器.计划账本.异步回写失败', {
      action,
      error: error instanceof Error ? error.message : String(error),
    }, LogCategory.ORCHESTRATOR);
  }

  private async withPlanLedgerMissionScope(
    action: string,
    rawMissionId: unknown,
    runner: (scope: MissionPlanScope) => Promise<void>,
  ): Promise<void> {
    const scope = await this.resolvePlanLedgerMissionScope(action, rawMissionId);
    if (!scope) {
      return;
    }
    await runner(scope);
  }

  private async resolvePlanLedgerMissionScope(
    action: string,
    rawMissionId: unknown,
  ): Promise<MissionPlanScope | null> {
    const missionId = typeof rawMissionId === 'string' ? rawMissionId.trim() : '';
    if (!missionId) {
      logger.warn('编排器.计划账本.事件缺少missionId', {
        action,
      }, LogCategory.ORCHESTRATOR);
      return null;
    }

    const resolution = await this.readModelService.resolveMissionPlanScopeDetailed({
      missionId,
      preferredSessionId: this.currentSessionId || undefined,
      preferredPlanId: this.currentPlanId || undefined,
    });
    if (!resolution.scope) {
      const logPayload: Record<string, unknown> = {
        action,
        missionId,
        reason: resolution.reason,
      };
      if (resolution.reason === 'missing_plan') {
        const sessionId = (await this.missionStorage.load(missionId))?.sessionId;
        if (sessionId) {
          logPayload.sessionId = sessionId;
        }
      }
      logger.warn('编排器.计划账本.事件定位失败', logPayload, LogCategory.ORCHESTRATOR);
      return null;
    }

    return resolution.scope;
  }

  private setupPlanLedgerEventBindings(): void {
    this.planLedger.on('updated', (event: { sessionId: string; reason: string; record: PlanRecord }) => {
      this.emitPlanLedgerUpdate(event.sessionId, event.reason);
      this.appendPlanLedgerTimelineEvent(event);
    });

    this.missionOrchestrator.on('assignmentPlanned', (data) => {
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      if (!assignmentId) {
        logger.warn('编排器.计划账本.assignmentPlanned.缺少assignmentId', {
          dataKeys: data && typeof data === 'object' ? Object.keys(data as Record<string, unknown>) : typeof data,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void this.withPlanLedgerMissionScope('assignmentPlanned', data.missionId, async (scope) => {
        await this.planLedger.bindAssignmentTodos(scope.sessionId, scope.planId, assignmentId, data.todos);
        this.appendTimelineEvent({
          type: 'assignment.planned',
          summary: `任务已规划：${assignmentId}`,
          trace: mergeOrchestrationTraceLinks(data.trace, {
            sessionId: scope.sessionId,
            planId: scope.planId,
            missionId: scope.missionId,
            assignmentId,
          }),
          payload: {
            todoIds: data.todos.map((todo) => todo.id),
            todoCount: data.todos.length,
            warnings: data.warnings,
          },
          diffs: [{
            entityType: 'assignment',
            entityId: assignmentId,
            before: {
              status: 'pending',
              todoCount: 0,
            },
            after: {
              status: 'ready',
              todoCount: data.todos.length,
              todoIds: data.todos.map((todo) => todo.id),
            },
          }],
        });
      })
        .catch((error) => this.reportPlanLedgerAsyncError('assignmentPlanned', error));
    });

    this.missionOrchestrator.on('assignmentStarted', (data) => {
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      if (!assignmentId) {
        logger.warn('编排器.计划账本.assignmentStarted.缺少assignmentId', {
          dataKeys: data && typeof data === 'object' ? Object.keys(data as Record<string, unknown>) : typeof data,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void this.withPlanLedgerMissionScope('assignmentStarted', data.missionId, async (scope) => {
        await this.planLedger.startAttempt(scope.sessionId, scope.planId, {
          scope: 'assignment',
          targetId: assignmentId,
          assignmentId,
          reason: 'assignment-started',
        });
        await this.planLedger.updateAssignmentStatus(scope.sessionId, scope.planId, assignmentId, 'running');
        this.appendTimelineEvent({
          type: 'assignment.started',
          summary: `任务开始执行：${assignmentId}`,
          trace: mergeOrchestrationTraceLinks(data.trace, {
            sessionId: scope.sessionId,
            planId: scope.planId,
            missionId: scope.missionId,
            assignmentId,
          }),
          payload: {
            workerId: data.workerId,
          },
          diffs: [{
            entityType: 'assignment',
            entityId: assignmentId,
            before: { status: 'ready' },
            after: { status: 'running' },
          }],
        });
      }).catch((error) => this.reportPlanLedgerAsyncError('assignmentStarted', error));
    });

    this.missionOrchestrator.on('assignmentCompleted', (data) => {
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      if (!assignmentId) {
        logger.warn('编排器.计划账本.assignmentCompleted.缺少assignmentId', {
          dataKeys: data && typeof data === 'object' ? Object.keys(data as Record<string, unknown>) : typeof data,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      const success = Boolean((data as { success?: unknown }).success);
      const failureMessage = success ? undefined : this.extractAssignmentFailureMessage(data);
      const attemptStatus = this.classifyAttemptTerminalStatus(success, failureMessage);
      const assignmentStatus = this.mapAttemptToAssignmentStatus(attemptStatus);
      void this.withPlanLedgerMissionScope('assignmentCompleted', data.missionId, async (scope) => {
        await this.planLedger.completeLatestAttempt(scope.sessionId, scope.planId, {
          scope: 'assignment',
          targetId: assignmentId,
          assignmentId,
          status: attemptStatus,
          reason: success ? 'assignment-completed' : 'assignment-failed',
          error: failureMessage,
        });
        await this.planLedger.updateAssignmentStatus(
          scope.sessionId,
          scope.planId,
          assignmentId,
          assignmentStatus,
        );
        this.appendTimelineEvent({
          type: 'assignment.completed',
          summary: `任务执行已结束：${assignmentId}`,
          trace: mergeOrchestrationTraceLinks(data.trace, {
            sessionId: scope.sessionId,
            planId: scope.planId,
            missionId: scope.missionId,
            assignmentId,
          }),
          payload: {
            success,
            summary: data.summary,
            status: assignmentStatus,
          },
          diffs: [{
            entityType: 'assignment',
            entityId: assignmentId,
            before: { status: 'running' },
            after: { status: assignmentStatus },
          }],
        });
      }).catch((error) => this.reportPlanLedgerAsyncError('assignmentCompleted', error));
    });

    this.missionOrchestrator.on('todoStarted', (data) => {
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      const todoId = typeof data.todoId === 'string' ? data.todoId.trim() : '';
      if (!assignmentId || !todoId) {
        logger.warn('编排器.计划账本.todoStarted.缺少关键字段', {
          assignmentId: data.assignmentId,
          todoId: data.todoId,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void this.withPlanLedgerMissionScope('todoStarted', data.missionId, async (scope) => {
        await this.planLedger.startAttempt(scope.sessionId, scope.planId, {
          scope: 'todo',
          targetId: todoId,
          assignmentId,
          todoId,
          reason: 'todo-started',
        });
        await this.planLedger.updateTodoStatus(
          scope.sessionId,
          scope.planId,
          assignmentId,
          todoId,
          'running',
        );
        this.appendTimelineEvent({
          type: 'todo.started',
          summary: `Todo 开始执行：${todoId}`,
          trace: mergeOrchestrationTraceLinks(data.trace, {
            sessionId: scope.sessionId,
            planId: scope.planId,
            missionId: scope.missionId,
            assignmentId,
            todoId,
          }),
          payload: {
            content: data.content,
          },
          diffs: [{
            entityType: 'todo',
            entityId: todoId,
            before: { status: 'ready' },
            after: { status: 'running' },
          }],
        });
      }).catch((error) => this.reportPlanLedgerAsyncError('todoStarted', error));
    });

    this.missionOrchestrator.on('todoCompleted', (data) => {
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      const todoId = typeof data.todoId === 'string' ? data.todoId.trim() : '';
      if (!assignmentId || !todoId) {
        logger.warn('编排器.计划账本.todoCompleted.缺少关键字段', {
          assignmentId: data.assignmentId,
          todoId: data.todoId,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void this.withPlanLedgerMissionScope('todoCompleted', data.missionId, async (scope) => {
        await this.planLedger.completeLatestAttempt(scope.sessionId, scope.planId, {
          scope: 'todo',
          targetId: todoId,
          assignmentId,
          todoId,
          status: 'succeeded',
          reason: 'todo-completed',
        });
        await this.planLedger.updateTodoStatus(
          scope.sessionId,
          scope.planId,
          assignmentId,
          todoId,
          'completed',
        );
        this.appendTimelineEvent({
          type: 'todo.completed',
          summary: `Todo 已完成：${todoId}`,
          trace: mergeOrchestrationTraceLinks(data.trace, {
            sessionId: scope.sessionId,
            planId: scope.planId,
            missionId: scope.missionId,
            assignmentId,
            todoId,
          }),
          payload: {
            content: data.content,
          },
          diffs: [{
            entityType: 'todo',
            entityId: todoId,
            before: { status: 'running' },
            after: { status: 'completed' },
          }],
        });
      }).catch((error) => this.reportPlanLedgerAsyncError('todoCompleted', error));
    });

    this.missionOrchestrator.on('todoFailed', (data) => {
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      const todoId = typeof data.todoId === 'string' ? data.todoId.trim() : '';
      const errorMessage = typeof data.error === 'string' ? data.error.trim() : '';
      if (!assignmentId || !todoId) {
        logger.warn('编排器.计划账本.todoFailed.缺少关键字段', {
          assignmentId: data.assignmentId,
          todoId: data.todoId,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      const attemptStatus = this.classifyAttemptTerminalStatus(false, errorMessage || undefined);
      const todoStatus = this.mapAttemptToTodoStatus(attemptStatus);
      void this.withPlanLedgerMissionScope('todoFailed', data.missionId, async (scope) => {
        await this.planLedger.completeLatestAttempt(scope.sessionId, scope.planId, {
          scope: 'todo',
          targetId: todoId,
          assignmentId,
          todoId,
          status: attemptStatus,
          reason: 'todo-failed',
          error: errorMessage || undefined,
        });
        await this.planLedger.updateTodoStatus(
          scope.sessionId,
          scope.planId,
          assignmentId,
          todoId,
          todoStatus,
        );
        this.appendTimelineEvent({
          type: 'todo.failed',
          summary: `Todo 执行失败：${todoId}`,
          trace: mergeOrchestrationTraceLinks(data.trace, {
            sessionId: scope.sessionId,
            planId: scope.planId,
            missionId: scope.missionId,
            assignmentId,
            todoId,
          }),
          payload: {
            content: data.content,
            error: errorMessage || undefined,
            status: todoStatus,
          },
          diffs: [{
            entityType: 'todo',
            entityId: todoId,
            before: { status: 'running' },
            after: { status: todoStatus },
          }],
        });
      }).catch((error) => this.reportPlanLedgerAsyncError('todoFailed', error));
    });

  }

  private enqueueExecution<T>(runner: () => Promise<T>): Promise<T> {
    const queueDepth = this.pendingCount++;
    if (queueDepth > 0) {
      this.messageHub.notify(t('engine.queue.waiting', { queueDepth }));
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

  prepareResumeMission(missionId: string): void {
    this.resumeMissionId = missionId?.trim() || null;
  }

  /**
   * 恢复被中断的执行链
   *
   * 查找指定 session 中最近可恢复的链，将其状态从 interrupted → resuming，
   * 并设置 resumeMissionId 以复用原 Mission。
   *
   * @returns 可恢复链的信息，或 null（无可恢复链）
   */
  prepareChainResume(sessionId: string): { chainId: string; missionId?: string } | null {
    const recoverableChain = this.executionChainQuery.findLatestRecoverableChain(sessionId);
    if (!recoverableChain) {
      return null;
    }

    try {
      this.executionChainStore.transitionChainStatus(recoverableChain.id, 'resuming');
      this.activeChainId = recoverableChain.id;

      // 复用原 Mission（如果有的话）
      if (recoverableChain.currentMissionId) {
        this.resumeMissionId = recoverableChain.currentMissionId;
      }

      logger.info('编排器.执行链.恢复准备', {
        chainId: recoverableChain.id,
        missionId: recoverableChain.currentMissionId,
        attempt: recoverableChain.attempt,
      }, LogCategory.ORCHESTRATOR);

      return {
        chainId: recoverableChain.id,
        missionId: recoverableChain.currentMissionId,
      };
    } catch (error) {
      logger.warn('编排器.执行链.恢复准备失败', {
        chainId: recoverableChain.id,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  /**
   * 放弃执行链（不可恢复的取消）
   *
   * 将指定链标记为 cancelled + 不可恢复。
   */
  abandonChain(chainId: string): boolean {
    const chain = this.executionChainStore.getChain(chainId);
    if (!chain) return false;
    if (chain.status === 'completed' || chain.status === 'cancelled' || chain.status === 'failed') {
      return false; // 已在终态
    }
    try {
      // 如果是 running/paused，先中断
      if (chain.status === 'running' || chain.status === 'paused') {
        this.executionChainStore.transitionChainStatus(chainId, 'interrupted', {
          interruptedReason: 'user_stop',
          recoverable: false,
        });
      }
      this.executionChainStore.transitionChainStatus(chainId, 'cancelled', {
        recoverable: false,
      });
      logger.info('编排器.执行链.已放弃', { chainId }, LogCategory.ORCHESTRATOR);
      return true;
    } catch (error) {
      logger.warn('编排器.执行链.放弃失败', {
        chainId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return false;
    }
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
        this.host.capabilities.diagnostics,
        this.config.verification,
        this.host.workspaceFolders,
      );
    }

    await configureResilientAuxiliary(this.contextManager, this.executionStats);

    // 提前初始化 TodoManager，避免首次调用 todo_list/todo_update 时命中“未初始化”
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
    // 注入到 ToolManager，供 project_knowledge_query 工具按需拉取
    this.adapterFactory.getToolManager().setProjectKnowledgeBase(() => this.projectKnowledgeBase);
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
    turnIdHint?: string,
    requestId?: string
  ): Promise<string> {
    return this.enqueueExecution(async () => {
      const trimmedPrompt = userPrompt?.trim() || '';
      if (!trimmedPrompt) {
        return t('engine.input.emptyPrompt');
      }

      const previousSessionId = typeof this.currentSessionId === 'string'
        ? this.currentSessionId.trim()
        : '';
      this.isRunning = true;
      this.currentTaskId = taskId || null;
      this.lastMissionId = null;
      this.currentPlanId = null;
      this.currentRequirementAnalysis = null;
      // 新请求开始时重置所有上次执行状态，防止诊断面板显示过期的"已完成"
      this.lastExecutionRuntimeReason = undefined;
      this.lastExecutionFinalStatus = undefined;
      this.lastExecutionErrors = [];
      this.lastExecutionRuntimeSnapshot = null;
      this.lastExecutionRuntimeDecisionTrace = [];
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
      const executeStartedAt = Date.now();
      let orchestratorAttemptStarted = false;
      let orchestratorAttemptTargetId: string | null = null;
      let orchestratorRuntimeReason: ResolvedOrchestratorTerminationReason | undefined;
      let orchestratorRuntimeRounds = 0;
      let orchestratorRuntimeSnapshot: RuntimeTerminationSnapshot | undefined;
      let orchestratorRuntimeShadow: RuntimeTerminationShadow | undefined;
      let orchestratorRuntimeDecisionTrace: RuntimeTerminationDecisionTraceEntry[] | undefined;
      let runtimeTokenUsage: TokenUsage | undefined;
      let deliveryStatusForMission: MissionDeliveryStatus | null = null;
      let deliverySummaryForMission: string | undefined;
      let deliveryDetailsForMission: string | undefined;
      let deliveryWarningsForMission: string[] | undefined;
      let continuationPolicyForMission: MissionContinuationPolicy | undefined;
      let continuationReasonForMission: string | undefined;
      let currentPlanMode: PlanMode = this.resolveRequestedPlanningMode();
      let effectiveMode = this.resolveCurrentEffectiveMode(currentPlanMode);
      const rootRequestId = typeof requestId === 'string' && requestId.trim().length > 0
        ? requestId.trim()
        : `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      this.activeRoundRequestId = rootRequestId;

      // 创建执行链记录
      const resolvedSession = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
      const chain = this.executionChainStore.createChain({
        sessionId: resolvedSession,
        userMessageId: this.currentTurnId,
        requestId: rootRequestId,
      });
      this.activeChainId = chain.id;
      // 创建主线分支
      this.executionChainStore.createBranch({
        chainId: chain.id,
        kind: 'mainline',
      });

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        const normalizedResolvedSessionId = resolvedSessionId.trim();
        if (previousSessionId && normalizedResolvedSessionId && previousSessionId !== normalizedResolvedSessionId) {
          this.adapterFactory.clearAllAdapterHistories?.();
          logger.info('编排器.会话切换.已清空适配器历史', {
            fromSessionId: previousSessionId,
            toSessionId: normalizedResolvedSessionId,
          }, LogCategory.ORCHESTRATOR);
        }
        this.currentSessionId = resolvedSessionId;
        this.appendTimelineEvent({
          type: 'request.started',
          summary: '请求开始执行',
          trace: mergeOrchestrationTraceLinks({
            sessionId: resolvedSessionId,
            turnId: this.currentTurnId || undefined,
            requestId: rootRequestId,
          }, {
            planId: this.currentPlanId || undefined,
            missionId: this.lastMissionId || undefined,
          }),
          payload: {
            prompt: trimmedPrompt,
          },
          diffs: [{
            entityType: 'request',
            entityId: rootRequestId,
            before: { status: 'idle' },
            after: { status: 'running' },
          }],
        });
        this.syncMessageHubSessionContext(resolvedSessionId);
        this.dispatchManager.resetForNewExecutionCycle();
        await this.ensureContextReady(resolvedSessionId);

        const adapterPlanMode: PlanMode = this.resolveRequestedPlanningMode();
        const entryResolution = resolveOrchestrationEntry({
          prompt: trimmedPrompt,
          requestedPlanningMode: adapterPlanMode,
          interactionMode: this.interactionMode,
          modelCapability: this.adapterFactory.getOrchestratorModelCapability?.(),
        });
        currentPlanMode = entryResolution.requestedPlanningMode;
        effectiveMode = entryResolution.effectiveMode;
        let requirementAnalysis = entryResolution.requirementAnalysis;
        this.currentRequirementAnalysis = requirementAnalysis;
        this.lastRoutingDecision = entryResolution.routingDecision;
        this.recordRequestClassificationDecision({
          sessionId: resolvedSessionId,
          requestId: rootRequestId,
          requestedPlanningMode: adapterPlanMode,
          effectiveMode: entryResolution.effectiveMode,
          prompt: trimmedPrompt,
          classification: entryResolution.classification,
        });

        const resumeMissionIdForExecution = this.resumeMissionId?.trim();
        if (requirementAnalysis.entryPath === 'direct_response') {
          logger.info('编排器.执行入口.命中轻量直答分流', {
            sessionId: resolvedSessionId,
            requestId: rootRequestId,
            goal: requirementAnalysis.goal,
            reason: requirementAnalysis.reason,
            bypassResumeMissionId: resumeMissionIdForExecution || undefined,
          }, LogCategory.ORCHESTRATOR);
          const directContent = await this.executeNonTaskTurn({
            prompt: trimmedPrompt,
            images: imagePaths,
            requestId: rootRequestId,
            requirementAnalysis,
          });
          this.lastExecutionRuntimeReason = 'completed';
          this.lastExecutionFinalStatus = 'completed';
          this.lastExecutionErrors = [];
          this.lastExecutionRuntimeSnapshot = null;
          this.finalizeChainStatus('completed');
          return directContent;
        }

        if (requirementAnalysis.entryPath === 'lightweight_analysis') {
          logger.info('编排器.执行入口.命中轻量分析分流', {
            sessionId: resolvedSessionId,
            requestId: rootRequestId,
            goal: requirementAnalysis.goal,
            reason: requirementAnalysis.reason,
            bypassResumeMissionId: resumeMissionIdForExecution || undefined,
          }, LogCategory.ORCHESTRATOR);
          const analysisContent = await this.executeNonTaskTurn({
            prompt: trimmedPrompt,
            images: imagePaths,
            requestId: rootRequestId,
            requirementAnalysis,
          });
          this.lastExecutionRuntimeReason = 'completed';
          this.lastExecutionFinalStatus = 'completed';
          this.lastExecutionErrors = [];
          this.lastExecutionRuntimeSnapshot = null;
          this.finalizeChainStatus('completed');
          return analysisContent;
        }

        const planResolution = await this.planController.resolveExecutionPlan({
          sessionId: resolvedSessionId,
          turnId: this.currentTurnId || `turn:${Date.now()}`,
          prompt: trimmedPrompt,
          adapterPlanMode,
          requirementAnalysis,
          resumeMissionId: resumeMissionIdForExecution || undefined,
        });
        const executionPlan = planResolution.executionPlan;
        requirementAnalysis = planResolution.requirementAnalysis;
        currentPlanMode = planResolution.currentPlanMode;
        effectiveMode = planResolution.effectiveMode;
        this.currentPlanId = planResolution.currentPlanId;
        this.currentRequirementAnalysis = requirementAnalysis;

        // 绑定 plan 到执行链
        if (this.activeChainId && this.currentPlanId) {
          this.executionChainStore.updateChainBindings(this.activeChainId, {
            currentPlanId: this.currentPlanId,
          });
        }

        orchestratorAttemptTargetId = await this.planController.startOrchestratorAttempt({
          sessionId: resolvedSessionId,
          plan: executionPlan,
          turnId: this.currentTurnId || `orchestrator-${Date.now()}`,
        });
        orchestratorAttemptStarted = true;

        // 1. 组装上下文
        const context = await this.prepareContext(resolvedSessionId, trimmedPrompt, rootRequestId);

        // 2. 获取项目上下文和 ADR
        const governedKnowledge = this.projectKnowledgeBase
          ? new GovernedKnowledgeContextService(this.projectKnowledgeBase)
          : null;
        const projectContext = governedKnowledge
          ? governedKnowledge.buildProjectContext(600, {
            purpose: 'project_context',
            consumer: 'mission_driven_engine',
            sessionId: resolvedSessionId,
            requestId: rootRequestId,
            missionId: executionPlan.missionId,
            agentId: 'orchestrator',
          }).content
          : undefined;

        const knowledgeIndex = governedKnowledge
          ? governedKnowledge.buildKnowledgeIndex(600, {
            purpose: 'knowledge_index',
            consumer: 'mission_driven_engine',
            sessionId: resolvedSessionId,
            requestId: rootRequestId,
            missionId: executionPlan.missionId,
            agentId: 'orchestrator',
          }).content
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
              const fullSummary = allTodos.map(todo => {
                const isDone = todo.status === 'completed';
                const statusFlag = isDone
                  ? t('engine.todos.completedGuard')
                  : todo.status.toUpperCase();
                return t('engine.todos.itemLine', {
                  status: statusFlag,
                  id: todo.id,
                  worker: todo.workerId || 'unassigned',
                  content: todo.content,
                });
              }).join('\n');
              // Token 截断机制：限制最大长度约 1000 字符，防止上下文超载
              activeTodosSummary = fullSummary.length > 1000
                ? `${fullSummary.substring(0, 1000)}\n${t('engine.todos.truncated')}`
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
          knowledgeIndex,
          availableToolsSummary,
          categoryDefinitions,
          deepTask: effectiveMode.planningMode === 'deep',
        });

        // 追加用户规则（buildUnifiedSystemPrompt 不含用户规则，需显式注入）
        const userRulesPrompt = this.adapterFactory.getUserRulesPrompt();
        if (userRulesPrompt) {
          systemPrompt = `${systemPrompt}\n\n${userRulesPrompt}`;
        }

        // 4. 设置编排者快照上下文
        // Mission 尚未创建前，临时使用 turnId 作为 mission 作用域占位；
        // 一旦进入 dispatch 流并拿到真实 mission.id，会在 ensureMissionForDispatch 中覆盖。
        const orchestratorToolManager = this.adapterFactory.getToolManager();
        const orchestratorAssignmentId = `orchestrator-${this.currentTurnId}`;
        const normalizedSessionId = resolvedSessionId.trim();
        orchestratorToolManager.setSnapshotContext({
          sessionId: normalizedSessionId,
          missionId: this.lastMissionId || this.currentTurnId!,
          requestId: orchestratorAttemptTargetId || this.currentTurnId!,
          assignmentId: orchestratorAssignmentId,
          todoId: orchestratorAssignmentId,
          workerId: 'orchestrator',
        });

        // 编排者跨轮会话记忆保留在 adapter history 中，不再每轮清空。
        // SystemPrompt 侧通过 prepareContext 动态裁剪 recent_turns，避免双重注入和 token 膨胀。

        const runtimeLoopResult = await this.runtimeLoopController.run({
          sessionId: resolvedSessionId,
          prompt: trimmedPrompt,
          imagePaths,
          rootRequestId,
          systemPrompt,
          requirementAnalysis,
          effectiveMode,
        });
        orchestratorRuntimeReason = runtimeLoopResult.runtimeReason;
        orchestratorRuntimeRounds = runtimeLoopResult.runtimeRounds;
        orchestratorRuntimeSnapshot = runtimeLoopResult.runtimeSnapshot;
        orchestratorRuntimeShadow = runtimeLoopResult.runtimeShadow;
        orchestratorRuntimeDecisionTrace = runtimeLoopResult.runtimeDecisionTrace;
        runtimeTokenUsage = runtimeLoopResult.runtimeTokenUsage;
        deliveryStatusForMission = runtimeLoopResult.deliveryStatusForMission;
        deliverySummaryForMission = runtimeLoopResult.deliverySummaryForMission;
        deliveryDetailsForMission = runtimeLoopResult.deliveryDetailsForMission;
        deliveryWarningsForMission = runtimeLoopResult.deliveryWarningsForMission;
        continuationPolicyForMission = runtimeLoopResult.continuationPolicyForMission;
        continuationReasonForMission = runtimeLoopResult.continuationReasonForMission;
        this.lastExecutionRuntimeReason = runtimeLoopResult.runtimeReason;
        this.lastExecutionFinalStatus = runtimeLoopResult.finalExecutionStatus;
        this.lastExecutionErrors = runtimeLoopResult.executionErrors;
        this.finalizeChainStatus(runtimeLoopResult.finalExecutionStatus);
        this.setState('idle');
        this.currentTaskId = null;
        return runtimeLoopResult.finalContent;

      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        // 中断导致的 abort 不视为执行失败，静默处理
        if (isAbortError(error)) {
          const resolvedRuntimeTermination = this.resolveOrchestratorRuntimeReason({
            runtimeReason: orchestratorRuntimeReason,
            runtimeSnapshot: orchestratorRuntimeSnapshot,
            additionalCandidates: [{
              reason: 'interrupted',
              eventId: 'engine:execute-aborted',
              triggeredAt: Date.now(),
            }],
          });
          orchestratorRuntimeReason = resolvedRuntimeTermination.reason;
          orchestratorRuntimeSnapshot = resolvedRuntimeTermination.runtimeSnapshot;
          logger.info('编排器.统一执行.中断', { error: errorMessage }, LogCategory.ORCHESTRATOR);
          this.lastExecutionRuntimeReason = orchestratorRuntimeReason;
          this.lastExecutionFinalStatus = 'cancelled';
          this.lastExecutionErrors = [];
          this.finalizeChainStatus('cancelled');
          this.setState('idle');
          this.currentTaskId = null;
          return '';
        }
        const resolvedRuntimeTermination = this.resolveOrchestratorRuntimeReason({
          runtimeReason: orchestratorRuntimeReason,
          runtimeSnapshot: orchestratorRuntimeSnapshot,
          additionalCandidates: [{
            reason: 'failed',
            eventId: 'engine:execute-error',
            triggeredAt: Date.now(),
          }],
        });
        orchestratorRuntimeReason = resolvedRuntimeTermination.reason;
        orchestratorRuntimeSnapshot = resolvedRuntimeTermination.runtimeSnapshot;
        this.lastExecutionRuntimeReason = orchestratorRuntimeReason;
        this.lastExecutionFinalStatus = 'failed';
        this.lastExecutionErrors = [errorMessage];
        this.finalizeChainStatus('failed');
        logger.error('编排器.统一执行.失败', { error: errorMessage }, LogCategory.ORCHESTRATOR);
        this.setState('idle');
        this.currentTaskId = null;
        // fail-open：执行异常也返回可读结论，避免打断会话链路
        const degradedMessage = `本轮执行出现异常，系统已自动降级为不中断返回：${errorMessage}`;
        return degradedMessage;
      } finally {
        const finalSessionId = this.currentSessionId;
        const finalPlanId = this.currentPlanId;
        const finalRuntimeTermination = finalPlanId
          ? this.resolveOrchestratorRuntimeReason({
            runtimeReason: orchestratorRuntimeReason,
            runtimeSnapshot: orchestratorRuntimeSnapshot,
          })
          : null;
        const finalRuntimeReason = finalRuntimeTermination?.reason;
        const finalRuntimeSnapshot = finalRuntimeTermination?.runtimeSnapshot || orchestratorRuntimeSnapshot;
        const finalExecutionStatus = finalRuntimeReason
          ? this.resolveExecutionFinalStatus(finalRuntimeReason, finalRuntimeSnapshot)
          : null;
        this.lastExecutionRuntimeSnapshot = finalRuntimeSnapshot
          ? { ...finalRuntimeSnapshot }
          : null;
        this.lastExecutionRuntimeDecisionTrace = Array.isArray(orchestratorRuntimeDecisionTrace)
          ? orchestratorRuntimeDecisionTrace.map((entry) => ({
            ...entry,
            candidates: Array.isArray(entry.candidates) ? [...entry.candidates] : undefined,
            gateState: entry.gateState ? { ...entry.gateState } : undefined,
          }))
          : [];
        if (finalRuntimeReason) {
          this.lastExecutionRuntimeReason = finalRuntimeReason;
          if (finalExecutionStatus) {
            this.lastExecutionFinalStatus = finalExecutionStatus;
          }
          if (finalExecutionStatus === 'failed') {
            this.lastExecutionErrors = this.buildExecutionFailureMessages(finalRuntimeReason, this.lastExecutionErrors);
          }
        }
        if (finalSessionId && finalPlanId && orchestratorAttemptStarted && orchestratorAttemptTargetId && finalRuntimeReason) {
          const attemptStatus = this.mapOrchestratorRuntimeReasonToAttemptStatus(finalRuntimeReason);
          try {
            await this.planLedger.completeLatestAttempt(finalSessionId, finalPlanId, {
              scope: 'orchestrator',
              targetId: orchestratorAttemptTargetId,
              status: attemptStatus,
              reason: finalRuntimeReason,
              error: attemptStatus === 'succeeded' ? undefined : this.lastExecutionErrors[0],
            });
          } catch (attemptError) {
            logger.warn('编排器.计划账本.Attempt.终态更新失败', {
              sessionId: finalSessionId,
              planId: finalPlanId,
              targetId: orchestratorAttemptTargetId,
              error: attemptError instanceof Error ? attemptError.message : String(attemptError),
            }, LogCategory.ORCHESTRATOR);
          }
        }
        if (finalSessionId && finalPlanId && finalRuntimeReason && finalExecutionStatus) {
          try {
            await this.syncPlanRuntimeGovernanceState({
              sessionId: finalSessionId,
              planId: finalPlanId,
              runtimeReason: finalRuntimeReason,
              runtimeSnapshot: finalRuntimeSnapshot,
              finalExecutionStatus,
            });
          } catch (runtimeError) {
            logger.warn('编排器.计划账本.运行态治理同步失败', {
              sessionId: finalSessionId,
              planId: finalPlanId,
              error: runtimeError instanceof Error ? runtimeError.message : String(runtimeError),
            }, LogCategory.ORCHESTRATOR);
          }
        }
        if (finalSessionId && finalPlanId && finalExecutionStatus && finalExecutionStatus !== 'paused') {
          try {
            await this.planLedger.finalize(finalSessionId, finalPlanId, finalExecutionStatus);
          } catch (planError) {
            logger.warn('编排器.计划账本.终态更新失败', {
              sessionId: finalSessionId,
              planId: finalPlanId,
              error: planError instanceof Error ? planError.message : String(planError),
            }, LogCategory.ORCHESTRATOR);
          }
        }
        this.persistTerminationMetrics({
          sessionId: finalSessionId,
          planId: finalPlanId,
          turnId: this.currentTurnId,
          mode: currentPlanMode,
          finalPlanStatus: finalExecutionStatus,
          runtimeReason: finalRuntimeReason,
          runtimeRounds: orchestratorRuntimeRounds,
          runtimeSnapshot: finalRuntimeSnapshot,
          runtimeShadow: orchestratorRuntimeShadow,
          runtimeDecisionTrace: orchestratorRuntimeDecisionTrace,
          tokenUsage: runtimeTokenUsage,
          startedAt: executeStartedAt,
        });
        this.appendTimelineEvent({
          type: 'request.completed',
          summary: `请求执行结束：${this.lastExecutionFinalStatus}`,
          trace: mergeOrchestrationTraceLinks({
            sessionId: finalSessionId || undefined,
            turnId: this.currentTurnId || undefined,
            planId: finalPlanId || undefined,
            missionId: this.lastMissionId || undefined,
            requestId: rootRequestId,
          }),
          payload: {
            runtimeReason: this.lastExecutionRuntimeReason,
            finalStatus: this.lastExecutionFinalStatus,
            errorCount: this.lastExecutionErrors.length,
            deliveryStatus: deliveryStatusForMission || undefined,
          },
          diffs: [{
            entityType: 'request',
            entityId: rootRequestId,
            before: { status: 'running' },
            after: {
              status: this.lastExecutionFinalStatus,
              runtimeReason: this.lastExecutionRuntimeReason,
              deliveryStatus: deliveryStatusForMission || undefined,
            },
          }],
        });
        this.isRunning = false;
        this.activeRoundRequestId = null;
        this.resumeMissionId = null;
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
              // 未调用 worker_dispatch → 不属于任务维度，删除空 Mission
              await this.missionStorage.delete(this.lastMissionId);
            } else {
              // 更新 Mission 终态
              if (finalExecutionStatus === 'completed') {
                await this.taskViewService.completeTaskById(this.lastMissionId);
              } else if (finalExecutionStatus === 'failed') {
                await this.taskViewService.failTaskById(this.lastMissionId, this.lastExecutionErrors[0]);
              } else if (finalExecutionStatus === 'cancelled') {
                await this.taskViewService.cancelTaskById(this.lastMissionId);
              } else if (finalExecutionStatus === 'paused') {
                await this.taskViewService.pauseTaskById(this.lastMissionId, this.lastExecutionRuntimeReason);
              }

              if (deliveryStatusForMission) {
                await this.missionStorage.updateDelivery(this.lastMissionId, {
                  status: deliveryStatusForMission,
                  summary: deliverySummaryForMission,
                  details: deliveryDetailsForMission,
                  warnings: deliveryWarningsForMission,
                  continuationPolicy: continuationPolicyForMission,
                  continuationReason: continuationReasonForMission,
                });
              }
            }
          } catch (missionUpdateError) {
            logger.warn('编排器.Mission.终态更新失败', {
              missionId: this.lastMissionId,
              finalPlanStatus: finalExecutionStatus,
              error: missionUpdateError instanceof Error ? missionUpdateError.message : String(missionUpdateError),
            }, LogCategory.ORCHESTRATOR);
          }
        }
        try {
          await this.contextManager.flushMemorySave();
        } catch (memoryError) {
          logger.warn('编排器.上下文.保存失败', { error: memoryError }, LogCategory.ORCHESTRATOR);
        }
        this.currentPlanId = null;
        this.currentRequirementAnalysis = null;
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
    const resumeMissionId = this.resumeMissionId;
    const turnIdAtCall = this.currentTurnId;
    const pending = (async (): Promise<string> => {
      // 双重检查：并发请求在等待期内可能已有 mission 产生
      if (this.lastMissionId) {
        return this.lastMissionId;
      }

      const sessionId = this.currentSessionId || this.sessionManager.getCurrentSession()?.id || '';
      if (!sessionId) {
        throw new Error(t('engine.errors.missingSessionId'));
      }
      const prompt = this.activeUserPrompt?.trim() || '';
      if (!prompt) {
        throw new Error(t('engine.errors.missingUserPrompt'));
      }

      const requirementAnalysis = this.currentRequirementAnalysis;
      if (!requirementAnalysis) {
        throw new Error('requirement analysis missing before mission creation');
      }
      const riskLevel = requirementAnalysis.riskLevel;
      if (!riskLevel) {
        throw new Error('requirement analysis riskLevel missing before mission creation');
      }
      const currentPlanMode: PlanMode = this.currentPlanId
        ? (this.planLedger.getPlan(sessionId, this.currentPlanId)?.mode ?? this.resolveRequestedPlanningMode())
        : this.resolveRequestedPlanningMode();
      const effectiveMode = this.resolveCurrentEffectiveMode(currentPlanMode);

      if (resumeMissionId) {
        const missionProjection = await this.readModelService.getMissionProjection(resumeMissionId);
        this.resumeMissionId = null;
        if (!missionProjection) {
          throw new Error(t('engine.errors.taskNotFound', { taskId: resumeMissionId }));
        }
        if (missionProjection.status !== 'paused') {
          throw new Error(t('engine.errors.taskNotPaused', { taskId: resumeMissionId }));
        }
        await this.missionStorage.transitionStatus(missionProjection.missionId, 'executing');
        this.lastMissionId = missionProjection.missionId;
        this.missionOrchestrator.setCurrentMissionId(missionProjection.missionId);
        // 绑定 mission 到执行链
        if (this.activeChainId) {
          this.executionChainStore.updateChainBindings(this.activeChainId, {
            currentMissionId: missionProjection.missionId,
          });
        }

        if (this.currentPlanId) {
          const planForBinding = await this.loadRecoveryPlanRecord({
            missionId: missionProjection.missionId,
            preferredSessionId: sessionId,
            preferredPlanId: this.currentPlanId,
          });
          if (!planForBinding) {
            throw new Error(`恢复任务 ${missionProjection.missionId} 时找不到计划 ${this.currentPlanId}`);
          }
          this.requirePlanMutation(
            await this.planLedger.bindMission(sessionId, this.currentPlanId, missionProjection.missionId, {
              expectedRevision: planForBinding.revision,
              auditReason: 'ensure-mission:bind-resume-mission',
            }),
            {
              op: 'bind-resume-mission',
              sessionId,
              planId: this.currentPlanId,
              missionId: missionProjection.missionId,
            },
          );
        }

        const orchestratorToolManager = this.adapterFactory.getToolManager();
        const orchestratorAssignmentId = `orchestrator-${missionProjection.missionId}`;
        const normalizedSessionId = sessionId.trim();
        orchestratorToolManager.setSnapshotContext({
          sessionId: normalizedSessionId,
          missionId: missionProjection.missionId,
          assignmentId: orchestratorAssignmentId,
          todoId: orchestratorAssignmentId,
          workerId: 'orchestrator',
        });

        return missionProjection.missionId;
      }

      const mission = await this.missionStorage.createMission({
        sessionId,
        userPrompt: prompt,
        context: '',
        goal: requirementAnalysis.goal,
        analysis: requirementAnalysis.analysis,
        constraints: requirementAnalysis.constraints,
        acceptanceCriteria: requirementAnalysis.acceptanceCriteria,
        riskLevel,
        riskFactors: requirementAnalysis.riskFactors,
        executionPath: this.mapRiskLevelToExecutionPath(riskLevel),
        continuationPolicy: effectiveMode.allowDeepContinuation ? 'auto' : 'stop',
      });

      // 若 Mission 创建期间执行轮次已切换，回收该 Mission，避免生成孤儿任务
      if (turnIdAtCall && this.currentTurnId !== turnIdAtCall) {
        try {
          await this.missionStorage.delete(mission.id);
        } catch {
          // 回收失败不阻塞主流程，后续由任务清理流程处理
        }
        throw new Error(t('engine.errors.turnSwitchedMissionInvalid'));
      }

      await this.missionStorage.transitionStatus(mission.id, 'planning');
      await this.missionStorage.transitionStatus(mission.id, 'executing');

      this.lastMissionId = mission.id;
      this.missionOrchestrator.setCurrentMissionId(mission.id);

      // 绑定 mission 到执行链
      if (this.activeChainId) {
        this.executionChainStore.updateChainBindings(this.activeChainId, {
          currentMissionId: mission.id,
        });
      }

      if (this.currentPlanId) {
        await this.planLedger.bindMission(sessionId, this.currentPlanId, mission.id);
      }

      const orchestratorToolManager = this.adapterFactory.getToolManager();
      const orchestratorAssignmentId = `orchestrator-${mission.id}`;
      const normalizedSessionId = sessionId.trim();
      orchestratorToolManager.setSnapshotContext({
        sessionId: normalizedSessionId,
        // 终止快照/todo_list 必须与 Todo 的真实 missionId 对齐，避免误读历史或空作用域。
        missionId: mission.id,
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

  getLastExecutionStatus(): {
    success: boolean;
    errors: string[];
    runtimeReason: ResolvedOrchestratorTerminationReason;
    finalStatus: 'completed' | 'failed' | 'cancelled' | 'paused';
    runtimeSnapshot: RuntimeTerminationSnapshot | null;
    runtimeDecisionTrace: RuntimeTerminationDecisionTraceEntry[];
  } {
    return {
      success: this.lastExecutionFinalStatus === 'completed' || this.lastExecutionFinalStatus === 'paused',
      errors: [...this.lastExecutionErrors],
      runtimeReason: this.lastExecutionRuntimeReason ?? 'completed',
      finalStatus: this.lastExecutionFinalStatus ?? 'completed',
      runtimeSnapshot: this.lastExecutionRuntimeSnapshot
        ? { ...this.lastExecutionRuntimeSnapshot }
        : null,
      runtimeDecisionTrace: this.lastExecutionRuntimeDecisionTrace.map((entry) => ({
        ...entry,
        candidates: Array.isArray(entry.candidates) ? [...entry.candidates] : undefined,
        gateState: entry.gateState ? { ...entry.gateState } : undefined,
      })),
    };
  }

  async queryRuntimeDiagnostics(
    input: Omit<OrchestrationRuntimeDiagnosticsQuery, 'liveRuntimeReason' | 'liveFinalStatus' | 'liveFailureReason' | 'liveErrors' | 'liveRuntimeSnapshot' | 'liveRuntimeDecisionTrace'>,
  ): Promise<OrchestrationRuntimeDiagnosticsSnapshot | null> {
    const sessionId = typeof input.sessionId === 'string' ? input.sessionId.trim() : '';
    if (!sessionId) {
      return null;
    }
    const shouldAttachLiveExecution = sessionId === (this.currentSessionId?.trim() || '');
    return this.runtimeDiagnosticsService.query({
      ...input,
      liveRuntimeReason: shouldAttachLiveExecution ? this.lastExecutionRuntimeReason : undefined,
      liveFinalStatus: shouldAttachLiveExecution ? this.lastExecutionFinalStatus : undefined,
      liveFailureReason: shouldAttachLiveExecution ? this.lastExecutionErrors[0] : undefined,
      liveErrors: shouldAttachLiveExecution ? this.lastExecutionErrors : undefined,
      liveRuntimeSnapshot: shouldAttachLiveExecution ? this.lastExecutionRuntimeSnapshot : undefined,
      liveRuntimeDecisionTrace: shouldAttachLiveExecution ? this.lastExecutionRuntimeDecisionTrace : undefined,
      updatedAt: Date.now(),
    });
  }

  /**
   * 带任务上下文执行
   */
  async executeWithTaskContext(
    userPrompt: string,
    sessionId?: string,
    imagePaths?: string[],
    turnIdHint?: string,
    requestId?: string
  ): Promise<{ taskId: string; result: string }> {
    const result = await this.execute(userPrompt, '', sessionId, imagePaths, turnIdHint, requestId);
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
  async prepareContext(_sessionId: string, _userPrompt: string, requestId?: string): Promise<string> {
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
    assembledOptions.knowledgeAudit = {
      ...(assembledOptions.knowledgeAudit || {}),
      consumer: 'mission_driven_engine',
      requestId: requestId?.trim() || undefined,
      missionId,
      agentId: 'orchestrator',
    };

    if (policy.includeRecentTurns) {
      return this.contextManager.getAssembledContextText(assembledOptions);
    }

    return this.contextManager.getAssembledContextText(assembledOptions, {
      excludePartTypes: ['recent_turns'],
    });
  }

  private buildAutoRepairPrompt(input: {
    originalPrompt: string;
    goal: string;
    constraints: string[];
    acceptanceCriteria: string[];
    deliverySummary?: string;
    deliveryDetails?: string;
    round: number;
    maxRounds: number;
  }): string {
    const goal = input.goal || input.originalPrompt;
    const maxRoundsLabel = input.maxRounds > 0 ? input.maxRounds : t('common.unlimited');
    const constraints = input.constraints.length > 0
      ? input.constraints.map(item => `- ${item}`).join('\n')
      : '- 无';
    const acceptance = input.acceptanceCriteria.length > 0
      ? input.acceptanceCriteria.map(item => `- ${item}`).join('\n')
      : '- 无';
    const summary = input.deliverySummary?.trim() || '未提供';
    const details = input.deliveryDetails?.trim() || '';
    const detailSection = details ? `验收失败详情：\n${details}` : '';

    return [
      '[System] 交付验收未通过，进入自动修复。',
      `修复轮次：${input.round}/${maxRoundsLabel}`,
      `原始目标：${goal}`,
      `用户原始请求：${input.originalPrompt}`,
      `约束：\n${constraints}`,
      `验收标准：\n${acceptance}`,
      `验收失败摘要：${summary}`,
      detailSection,
      '请根据失败信息修复问题，必要时调整实现并重新运行相关验收。',
      '输出最终交付总结，并明确哪些验收已通过、哪些仍未通过。',
    ].filter(line => line && line.trim().length > 0).join('\n\n');
  }

  private buildReplanGateReason(input: {
    source: ReplanSource;
    summary?: string;
    details?: string;
    reviewRound?: number;
    pendingRequiredTodos?: number;
    phaseRuntime?: PlanRuntimePhaseState | null;
    budgetElapsedMs?: number;
    budgetTokenUsed?: number;
    scopeIssues?: string[];
    failedRequiredTodos?: number;
    unresolvedBlockers?: number;
    externalWaitOpen?: number;
  }): string {
    const reasons: string[] = [`source=${input.source}`];

    if (typeof input.reviewRound === 'number'
      && input.reviewRound >= MissionDrivenEngine.REPLAN_GATE_REVIEW_ROUND_THRESHOLD) {
      reasons.push(`review_round>=${MissionDrivenEngine.REPLAN_GATE_REVIEW_ROUND_THRESHOLD}(${input.reviewRound})`);
    }

    if (typeof input.pendingRequiredTodos === 'number'
      && input.pendingRequiredTodos >= MissionDrivenEngine.REPLAN_GATE_PENDING_REQUIRED_THRESHOLD) {
      reasons.push(`pending_required>=${MissionDrivenEngine.REPLAN_GATE_PENDING_REQUIRED_THRESHOLD}(${input.pendingRequiredTodos})`);
    }

    if (input.phaseRuntime) {
      if (input.phaseRuntime.state === 'awaiting_next_phase') {
        reasons.push('phase_state=awaiting_next_phase');
      }
      if (input.phaseRuntime.continuationIntent === 'continue') {
        reasons.push('continuation_intent=continue');
      }
      if (Array.isArray(input.phaseRuntime.remainingPhases) && input.phaseRuntime.remainingPhases.length > 0) {
        reasons.push(`remaining_phases=${input.phaseRuntime.remainingPhases.length}`);
      }
    }

    if (typeof input.budgetElapsedMs === 'number' && Number.isFinite(input.budgetElapsedMs) && input.budgetElapsedMs > 0) {
      reasons.push(`budget_elapsed_ms=${Math.max(0, Math.floor(input.budgetElapsedMs))}`);
    }

    if (typeof input.budgetTokenUsed === 'number' && Number.isFinite(input.budgetTokenUsed) && input.budgetTokenUsed > 0) {
      reasons.push(`budget_tokens=${Math.max(0, Math.floor(input.budgetTokenUsed))}`);
    }

    if (Array.isArray(input.scopeIssues) && input.scopeIssues.length > 0) {
      reasons.push(`scope_issues=${input.scopeIssues.length}`);
    }

    if (typeof input.failedRequiredTodos === 'number' && Number.isFinite(input.failedRequiredTodos) && input.failedRequiredTodos > 0) {
      reasons.push(`failed_required=${Math.max(0, Math.floor(input.failedRequiredTodos))}`);
    }

    if (typeof input.unresolvedBlockers === 'number' && Number.isFinite(input.unresolvedBlockers) && input.unresolvedBlockers > 0) {
      reasons.push(`unresolved_blockers=${Math.max(0, Math.floor(input.unresolvedBlockers))}`);
    }

    if (typeof input.externalWaitOpen === 'number' && Number.isFinite(input.externalWaitOpen) && input.externalWaitOpen > 0) {
      reasons.push(`external_wait_open=${Math.max(0, Math.floor(input.externalWaitOpen))}`);
    }

    if (input.summary?.trim()) {
      reasons.push(`summary=${input.summary.trim().slice(0, 120)}`);
    }

    if (input.details?.trim()) {
      reasons.push(`details=${input.details.trim().slice(0, 160)}`);
    }

    return reasons.join(';');
  }

  private mergeAcceptanceCriteriaWithExecutionReport(input: {
    criteria?: AcceptanceCriterion[] | null;
    report?: AcceptanceExecutionReport;
    reviewRound: number;
    batchId?: string;
    workers?: WorkerSlot[];
  }): AcceptanceCriterion[] {
    const baseCriteria = Array.isArray(input.criteria) ? input.criteria : [];
    if (baseCriteria.length === 0) {
      return [];
    }

    const criteriaResultById = new Map<string, { status: 'passed' | 'failed'; detail: string }>();
    for (const result of input.report?.criteriaResults || []) {
      const criterionId = typeof result?.criterionId === 'string' ? result.criterionId.trim() : '';
      if (!criterionId) {
        continue;
      }
      criteriaResultById.set(criterionId, {
        status: result.status === 'passed' ? 'passed' : 'failed',
        detail: typeof result.detail === 'string' ? result.detail.trim() : '',
      });
    }

    const uniqueWorkers = Array.from(new Set((input.workers || []).map((worker) => worker.trim()).filter(Boolean)));
    const singleWorker = uniqueWorkers.length === 1 ? (uniqueWorkers[0] as WorkerSlot) : undefined;
    const reviewedAt = Date.now();

    return baseCriteria.map((criterion) => {
      const copied: AcceptanceCriterion = {
        ...criterion,
        evidence: Array.isArray(criterion.evidence) ? [...criterion.evidence] : undefined,
        verificationSpec: criterion.verificationSpec ? { ...criterion.verificationSpec } : undefined,
        reviewHistory: Array.isArray(criterion.reviewHistory)
          ? criterion.reviewHistory.map((entry) => ({ ...entry }))
          : undefined,
      };
      const criteriaResult = criteriaResultById.get(copied.id);
      if (!criteriaResult) {
        return copied;
      }

      copied.status = criteriaResult.status;
      copied.scope = copied.scope || 'batch_verification';
      if (!copied.owner && singleWorker) {
        copied.owner = singleWorker;
      }
      if (input.batchId) {
        copied.lastBatchId = input.batchId;
      }
      if (singleWorker) {
        copied.lastWorkerId = singleWorker;
      }

      const evidence = new Set<string>(copied.evidence || []);
      if (criteriaResult.detail) {
        evidence.add(criteriaResult.detail);
      }
      copied.evidence = evidence.size > 0 ? Array.from(evidence) : undefined;

      const historyEntry = {
        status: copied.status,
        reviewer: 'system:spec-verifier',
        detail: criteriaResult.detail || undefined,
        reviewedAt,
        round: input.reviewRound,
        batchId: input.batchId,
        workerId: singleWorker,
      } as const;
      copied.reviewHistory = [...(copied.reviewHistory || []), historyEntry];

      return copied;
    });
  }

  private recordRequestClassificationDecision(input: {
    sessionId: string;
    requestId: string;
    requestedPlanningMode: PlanMode;
    effectiveMode: EffectiveModeResolution;
    prompt: string;
    classification: import('./request-classifier').RequestClassification;
  }): void {
    try {
      const record = buildRequestClassificationDecisionRecord({
        sessionId: input.sessionId,
        turnId: this.currentTurnId,
        requestId: input.requestId,
        prompt: input.prompt,
        requestedPlanningMode: input.requestedPlanningMode,
        effectivePlanningMode: input.effectiveMode.planningMode,
        requestedInteractionMode: input.effectiveMode.requestedInteractionMode,
        effectiveInteractionMode: input.effectiveMode.interactionMode,
        modelCapability: input.effectiveMode.modelCapability,
        classification: input.classification,
      });
      this.requestClassificationCalibrationStore.appendDecision(record);
    } catch (error) {
      logger.warn('编排器.请求分类.校准记录失败', {
        sessionId: input.sessionId,
        requestId: input.requestId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private formatGovernanceReason(
    reason?: ResolvedOrchestratorTerminationReason,
  ): string {
    switch (reason) {
      case 'budget_exceeded':
        return t('engine.governance.reason.budget_exceeded');
      case 'external_wait_timeout':
        return t('engine.governance.reason.external_wait_timeout');
      case 'stalled':
        return t('engine.governance.reason.stalled');
      case 'upstream_model_error':
        return t('engine.governance.reason.upstream_model_error');
      default:
        return t('engine.governance.reason.unknown');
    }
  }

  private buildGovernanceRecoveryPrompt(input: {
    originalPrompt: string;
    goal: string;
    constraints: string[];
    acceptanceCriteria: string[];
    reason?: ResolvedOrchestratorTerminationReason;
    round: number;
    maxRounds: number;
  }): string {
    const goal = input.goal || input.originalPrompt;
    const reasonLabel = this.formatGovernanceReason(input.reason);
    const constraints = input.constraints.length > 0
      ? input.constraints.map(item => `- ${item}`).join('\n')
      : '- 无';
    const acceptance = input.acceptanceCriteria.length > 0
      ? input.acceptanceCriteria.map(item => `- ${item}`).join('\n')
      : '- 无';

    return [
      '[System] 上一轮触发治理暂停，已进入自动恢复。',
      `暂停原因：${reasonLabel}`,
      `恢复轮次：${input.round}/${input.maxRounds}`,
      `原始目标：${goal}`,
      `用户原始请求：${input.originalPrompt}`,
      `约束：\n${constraints}`,
      `验收标准：\n${acceptance}`,
      '请继续推进未完成任务，必要时重试上游调用或等待外部依赖恢复。',
    ].filter(line => line && line.trim().length > 0).join('\n\n');
  }

  private buildGovernancePauseReport(input: {
    reason?: ResolvedOrchestratorTerminationReason;
    snapshot?: RuntimeTerminationSnapshot;
    recoveryAttempted: number;
    recoveryMaxRounds: number;
  }): string {
    const reasonLabel = this.formatGovernanceReason(input.reason);
    const snapshot = input.snapshot;
    const lines: string[] = [`[System] ${t('engine.governance.pauseTitle')}`];
    lines.push(`- ${t('engine.governance.pauseReason', { reason: reasonLabel })}`);

    if (snapshot) {
      const requiredTotal = this.resolveRequiredTotal(snapshot);
      const terminalRequired = this.resolveTerminalRequired(snapshot);
      const pendingRequired = this.extractPendingRequiredCount(snapshot);
      const failedRequired = typeof snapshot.failedRequired === 'number' && Number.isFinite(snapshot.failedRequired)
        ? snapshot.failedRequired
        : 0;
      if (typeof requiredTotal === 'number' && typeof terminalRequired === 'number') {
        lines.push(`- ${t('engine.governance.pauseMetric.required', {
          terminal: terminalRequired,
          total: requiredTotal,
          failed: failedRequired,
        })}`);
      }
      if (pendingRequired > 0) {
        lines.push(`- ${t('engine.governance.pauseMetric.pending', { pending: pendingRequired })}`);
      }
      if (snapshot.blockerState?.maxExternalWaitAgeMs) {
        lines.push(`- ${t('engine.governance.pauseMetric.externalWait', {
          seconds: Math.ceil(snapshot.blockerState.maxExternalWaitAgeMs / 1000),
        })}`);
      }
      const budgetState = snapshot.budgetState;
      if (budgetState && typeof budgetState.elapsedMs === 'number' && typeof budgetState.tokenUsed === 'number' && typeof budgetState.errorRate === 'number') {
        lines.push(`- ${t('engine.governance.pauseMetric.budget', {
          elapsed: Math.ceil(budgetState.elapsedMs / 1000),
          tokens: budgetState.tokenUsed,
          errorRate: budgetState.errorRate.toFixed(3),
        })}`);
      }
    }

    const advice = this.resolveGovernancePauseAdvice(input.reason);
    if (advice) {
      lines.push(`- ${t('engine.governance.pauseAdvice', { advice })}`);
    }
    if (input.recoveryAttempted >= input.recoveryMaxRounds && input.recoveryMaxRounds > 0) {
      lines.push(`- ${t('engine.governance.autoResumeExhausted', {
        attempt: input.recoveryAttempted,
        maxRounds: input.recoveryMaxRounds,
      })}`);
    }

    return lines.join('\n');
  }

  private resolveGovernancePauseAdvice(reason?: ResolvedOrchestratorTerminationReason): string {
    switch (reason) {
      case 'budget_exceeded':
        return t('engine.governance.advice.budget_exceeded');
      case 'external_wait_timeout':
        return t('engine.governance.advice.external_wait_timeout');
      case 'stalled':
        return t('engine.governance.advice.stalled');
      case 'upstream_model_error':
        return t('engine.governance.advice.upstream_model_error');
      default:
        return t('engine.governance.advice.unknown');
    }
  }

  private resolveFollowUpSteps(runtimeSteps?: string[]): string[] {
    if (!Array.isArray(runtimeSteps)) {
      return [];
    }
    return normalizeNextSteps(runtimeSteps);
  }

  private classifyFollowUpSteps(steps: string[]): {
    actionable: string[];
    blocked: string[];
    nonActionable: string[];
  } {
    const actionable: string[] = [];
    const blocked: string[] = [];
    const nonActionable: string[] = [];
    for (const step of steps) {
      if (this.isBlockedFollowUpStep(step)) {
        blocked.push(step);
      } else if (this.isNonActionableFollowUpStep(step)) {
        nonActionable.push(step);
      } else {
        actionable.push(step);
      }
    }
    return { actionable, blocked, nonActionable };
  }

  private isBlockedFollowUpStep(step: string): boolean {
    const text = step.trim();
    if (!text) {
      return false;
    }
    const lower = text.toLowerCase();
    const patterns: RegExp[] = [
      /(系统|平台|策略).*(拦截|阻断|拒绝)/,
      /(已被|被).*(拦截|阻止|拒绝)/,
      /(无法|不能|未能|无法确认|无法获取|无法判断).*(输出|结果|日志|命令|编译|测试|诊断|tsc)/,
      /(无输出|暂无输出)/,
      /(需要|请).*(用户|你|人工).*(提供|授权|允许|确认)/,
      /(需要|请).*(授权|权限|许可)/,
      /重复.*(阻止|拦截|拒绝)/,
      /command rejected|tool blocked|permission denied|unauthorized|forbidden|access denied|no output/i,
    ];
    if (patterns.some((pattern) => pattern.test(text) || pattern.test(lower))) {
      return true;
    }
    return false;
  }

  private isNonActionableFollowUpStep(step: string): boolean {
    const text = step.trim();
    if (!text) {
      return true;
    }
    const lower = text.toLowerCase();
    const directPatterns: RegExp[] = [
      /(如有|如果有|欢迎|请随时|随时).*(需求|问题|告诉|提出|联系|再说)/,
      /(我可以|可以帮你|能够帮你|支持).*(功能开发|bug ?修复|架构设计|代码审查|项目分析|代码重构)/i,
      /(请输入|请提供).*(具体|详细)?.*(需求|描述|信息)/,
      /^第\d+轮[:：]/,
    ];
    if (directPatterns.some((pattern) => pattern.test(text) || pattern.test(lower))) {
      return true;
    }

    const capabilityKeywords = [
      '功能开发',
      'bug修复',
      'bug 修复',
      '架构设计',
      '代码审查',
      '项目分析',
      '代码重构',
      'worker 协作',
      'multi-worker',
      '能力描述',
    ];
    const hitCount = capabilityKeywords.reduce((count, keyword) => (
      lower.includes(keyword.toLowerCase()) ? count + 1 : count
    ), 0);
    return hitCount >= 2;
  }

  private buildFollowUpBlockedNotice(blocked: string[]): string {
    const items = blocked.map(item => `- ${item}`).join('\n');
    return t('engine.followUp.blockedNotice', { items });
  }

  private stripNonActionableFollowUpSection(content: string): string {
    const trimmed = content.trim();
    if (!trimmed) {
      return trimmed;
    }
    const headingMatch = /(Next Steps:|下一步建议：?)/i.exec(trimmed);
    if (!headingMatch || typeof headingMatch.index !== 'number') {
      return trimmed;
    }
    const prefix = trimmed.slice(0, headingMatch.index).trimEnd();
    const tail = trimmed.slice(headingMatch.index + headingMatch[0].length).trim();
    const tailLines = tail.split(/\r?\n/).map((line) => line.trim()).filter((line) => line.length > 0);
    const tailLooksLikeFollowUpList = tailLines.length > 0 && tailLines.every((line) => /^[-*•]\s+/.test(line));
    if (!tailLooksLikeFollowUpList) {
      return trimmed;
    }
    return prefix.trim();
  }

  private isAssistantMetaPrompt(prompt: string): boolean {
    return /(?:你是谁|你是什么|你能做什么|你可以做什么|介绍.*你自己|自我介绍|你的(?:能力|职责)|怎么用|如何使用|模式区别|magi(?:\s+|是|是什么)|who are you|what are you|what can you do|how to use|your role|capabilities)/i
      .test(prompt.trim());
  }

  private sanitizeDirectResponseContent(prompt: string, content: string): string {
    const strippedFollowUp = this.stripNonActionableFollowUpSection(content).trim();
    if (!strippedFollowUp) {
      return strippedFollowUp;
    }

    const internalLeakPatterns = [
      /worker_dispatch/i,
      /worker_wait/i,
      /context_compact/i,
      /todo_(?:split|list|update|claim_next)/i,
      /runtime governance/i,
      /无需创建任务/,
      /无需派发\s*worker/i,
      /不需要创建任务/,
      /不需要派发\s*worker/i,
      /当前已就绪/,
      /有什么开发任务需要我帮你完成吗/i,
      /请问您需要我做什么/i,
    ];
    const projectLeakPatterns = [
      /当前项目/,
      /\d+\s*个源文件/,
      /\bADR\b/i,
      /\bFAQ\b/i,
      /技术选型方案/,
      /Worker 分工/i,
      /我的职责/,
      /我的约束/,
      /我的能力/,
    ];

    const rawLines = strippedFollowUp
      .split(/\r?\n+/)
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
    const dedupedLines = Array.from(new Set(rawLines));
    const shouldDropLine = (line: string): boolean => (
      internalLeakPatterns.some((pattern) => pattern.test(line))
      || projectLeakPatterns.some((pattern) => pattern.test(line))
    );
    const filteredLines = dedupedLines.filter((line) => !shouldDropLine(line));

    const assistantMetaPrompt = this.isAssistantMetaPrompt(prompt);
    const fallbackLine = dedupedLines.find((line) => {
      if (shouldDropLine(line)) {
        return false;
      }
      if (/^(我的能力|当前项目|我的职责|我的约束)$/i.test(line)) {
        return false;
      }
      return true;
    }) || dedupedLines[0] || strippedFollowUp;

    const cleaned = filteredLines.join(' ').replace(/\s+/g, ' ').trim();
    if (!cleaned) {
      return assistantMetaPrompt ? fallbackLine : strippedFollowUp;
    }

    if (!assistantMetaPrompt) {
      return cleaned;
    }

    const segments = cleaned
      .split(/(?<=[。！？!?])\s+/)
      .map((segment) => segment.trim())
      .filter((segment) => segment.length > 0);
    const concise = segments.slice(0, 2).join(' ');
    if (!concise) {
      return cleaned;
    }
    if (concise.length <= 220) {
      return concise;
    }
    return `${concise.slice(0, 220).trim()}...`;
  }

  private sanitizeAnalysisResponseContent(content: string): string {
    const strippedFollowUp = this.stripNonActionableFollowUpSection(content).trim();
    if (!strippedFollowUp) {
      return strippedFollowUp;
    }

    const lines = strippedFollowUp
      .split(/\r?\n+/)
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
    const dedupedLines = Array.from(new Set(lines));
    const dropPatterns = [
      /worker_dispatch/i,
      /worker_wait/i,
      /context_compact/i,
      /runtime governance/i,
      /当前已就绪/,
      /有什么开发任务需要我帮你完成吗/i,
      /请问您需要我做什么/i,
      /请问您接下来(?:需要|还需要|想要)/i,
      /接下来(?:可以|可继续|可选)/i,
      /^\d+[.、:：)]/,
      /如有新的开发需求，请随时提出/i,
      /如需进一步(?:操作|分析|处理|修改|执行)/i,
      /如果你需要，我可以继续/i,
      /我可以帮你做(?:功能开发|bug 修复|架构设计|代码审查)/i,
      /无需创建任务/,
      /无需派发\s*worker/i,
      /不需要创建任务/,
      /不需要派发\s*worker/i,
      /(?:项目分析|项目理解|项目梳理|分析|梳理)(?:任务)?已完成/i,
      /(?:任务完成|分析完成|梳理完成)[。！!]?$/i,
      /以上是对.+(?:完整分析|完整理解)/i,
      /等待您的下一步指示/i,
    ];

    const filtered = dedupedLines.filter((line) => !dropPatterns.some((pattern) => pattern.test(line)));
    const sentenceFragments = filtered
      .flatMap((line) => line.split(/(?<=[。！？!?])\s*/))
      .map((segment) => segment.trim())
      .filter((segment) => segment.length > 0);
    const dedupedSentences: string[] = [];
    for (const sentence of sentenceFragments) {
      if (!dedupedSentences.includes(sentence)) {
        dedupedSentences.push(sentence);
      }
    }
    const cleaned = dedupedSentences.join('\n').trim();
    if (cleaned) {
      return cleaned;
    }

    const fallback = dedupedLines.find((line) => !dropPatterns.some((pattern) => pattern.test(line)));
    return fallback?.trim() || '';
  }

  private extractStructuredContinuationStepsFromContent(content: string): string[] {
    const trimmed = content.trim();
    if (!trimmed) {
      return [];
    }
    const sectionSteps = this.extractSectionListItems(trimmed, /(Next Steps:|下一步建议：?)/i);
    if (sectionSteps.length > 0) {
      return normalizeNextSteps(sectionSteps);
    }

    const phaseSectionSteps = this.extractSectionListItems(
      trimmed,
      /(Phases:|Phase Plan:|阶段列表：?|阶段：?)/i,
      { requirePhaseMarker: true },
    );
    if (phaseSectionSteps.length > 0) {
      return normalizeNextSteps(phaseSectionSteps);
    }

    const inlinePhaseSteps = this.extractInlinePhaseListItems(trimmed);
    return normalizeNextSteps(inlinePhaseSteps);
  }

  private extractSectionListItems(
    content: string,
    headingPattern: RegExp,
    options: { requirePhaseMarker?: boolean } = {},
  ): string[] {
    const headingMatch = headingPattern.exec(content);
    if (!headingMatch || typeof headingMatch.index !== 'number') {
      return [];
    }
    const tail = content.slice(headingMatch.index + headingMatch[0].length);
    const steps: string[] = [];
    for (const line of tail.split(/\r?\n/)) {
      const candidate = line.trim();
      if (!candidate) {
        if (steps.length > 0) {
          break;
        }
        continue;
      }
      const listMatch = candidate.match(/^(?:[-*•]\s+|(?:\d+)[.)、:：]\s+)(.+)$/);
      if (!listMatch) {
        if (steps.length > 0) {
          break;
        }
        continue;
      }
      const item = listMatch[1].trim();
      if (options.requirePhaseMarker && !/(?:^|[\s(（])(?:phase\s*\d+|阶段\s*\d+)/i.test(item)) {
        continue;
      }
      steps.push(item);
    }
    return steps;
  }

  private extractInlinePhaseListItems(content: string): string[] {
    const steps = content
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => /^(?:\d+)[.)、:：]\s+/.test(line))
      .map((line) => line.replace(/^(?:\d+)[.)、:：]\s+/, '').trim())
      .filter((line) => /(?:^|[\s(（])(?:phase\s*\d+|阶段\s*\d+)/i.test(line));
    return steps.length >= 2 ? steps : [];
  }

  private resolvePhaseRuntimeForDecision(
    current?: PlanRuntimePhaseState | null,
    patch?: Partial<PlanRuntimePhaseState> | null,
  ): PlanRuntimePhaseState | null {
    const base = current ?? null;
    if (!base && !patch) {
      return null;
    }
    const merged = {
      state: patch?.state ?? base?.state ?? 'idle',
      currentIndex: patch?.currentIndex ?? base?.currentIndex,
      currentTitle: patch?.currentTitle ?? base?.currentTitle,
      nextIndex: patch?.nextIndex ?? base?.nextIndex,
      nextTitle: patch?.nextTitle ?? base?.nextTitle,
      remainingPhases: Array.isArray(patch?.remainingPhases)
        ? patch.remainingPhases
        : Array.isArray(base?.remainingPhases)
          ? base.remainingPhases
          : [],
      continuationIntent: patch?.continuationIntent ?? base?.continuationIntent ?? 'stop',
    } satisfies PlanRuntimePhaseState;
    return merged;
  }

  private extractPhaseDescriptor(step: string): { index?: number; title: string } {
    const trimmed = step.trim();
    if (!trimmed) {
      return { title: '' };
    }
    const explicitPhase = trimmed.match(/(?:^|[\s(（])phase\s*([0-9]+)(?:\b|[:：\-\s])/i)
      || trimmed.match(/(?:^|[\s(（])阶段\s*([0-9]+)(?:\b|[:：\-\s])/i);
    const phaseIndex = explicitPhase ? Number(explicitPhase[1]) : undefined;
    if (phaseIndex && Number.isFinite(phaseIndex)) {
      return { index: phaseIndex, title: `Phase ${phaseIndex}` };
    }
    const beforeColon = trimmed.split(/[:：]/)[0]?.trim();
    return { title: beforeColon || trimmed };
  }

  private buildPhaseRuntimePatch(input: {
    current?: PlanRuntimePhaseState | null;
    runtimeReason?: ResolvedOrchestratorTerminationReason;
    pendingRequiredTodos: number;
    followUpSteps: string[];
  }): Partial<PlanRuntimePhaseState> | null {
    const steps = input.followUpSteps.filter((item) => item.trim().length > 0);

    if (steps.length > 0) {
      const descriptors = steps.map((step) => this.extractPhaseDescriptor(step));
      const first = descriptors[0];
      return {
        state: 'awaiting_next_phase',
        currentIndex: input.current?.currentIndex,
        currentTitle: input.current?.currentTitle,
        nextIndex: first?.index,
        nextTitle: first?.title,
        remainingPhases: steps,
        continuationIntent: 'continue',
      };
    }
    if (input.pendingRequiredTodos > 0) {
      return {
        state: 'running',
        continuationIntent: 'stop',
        remainingPhases: [],
        nextIndex: undefined,
        nextTitle: undefined,
      };
    }
    if (input.runtimeReason === 'completed') {
      return {
        state: 'completed',
        continuationIntent: 'stop',
        remainingPhases: [],
      };
    }
    return null;
  }

  private async markPhaseRuntimeRunning(input: {
    sessionId: string;
    followUpSteps: string[];
  }): Promise<void> {
    if (!this.currentPlanId) {
      return;
    }
    const currentPlan = this.planLedger.getPlan(input.sessionId, this.currentPlanId);
    const steps = input.followUpSteps.filter((item) => item.trim().length > 0);
    const currentPhase = currentPlan?.runtime.phase;
    const queue = steps.length > 0
      ? steps
      : (currentPhase?.remainingPhases || []);
    const activeStep = queue[0] || currentPhase?.nextTitle || currentPhase?.currentTitle;
    const descriptor = activeStep ? this.extractPhaseDescriptor(activeStep) : null;
    const nextRemaining = queue.slice(1);
    const nextDescriptor = nextRemaining.length > 0
      ? this.extractPhaseDescriptor(nextRemaining[0])
      : null;
    try {
      await this.planLedger.updateRuntimeState(input.sessionId, this.currentPlanId, {
        phase: {
          state: 'running',
          currentIndex: descriptor?.index ?? currentPhase?.nextIndex ?? currentPhase?.currentIndex,
          currentTitle: descriptor?.title ?? currentPhase?.nextTitle ?? currentPhase?.currentTitle,
          nextIndex: nextDescriptor?.index,
          nextTitle: nextDescriptor?.title,
          remainingPhases: nextRemaining,
          continuationIntent: nextRemaining.length > 0
            ? 'continue'
            : 'stop',
        },
      }, {
        auditReason: 'follow-up-phase-runtime-running',
      });
    } catch (error) {
      logger.warn('编排器.PlanRuntime.phase运行态推进失败', {
        sessionId: input.sessionId,
        planId: this.currentPlanId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private buildAutoFollowUpPrompt(input: {
    originalPrompt: string;
    goal: string;
    constraints: string[];
    acceptanceCriteria: string[];
    steps: string[];
    round: number;
    requiredTotal?: number;
    terminalRequired?: number;
    pendingRequired?: number;
  }): string {
    const goal = input.goal || input.originalPrompt;
    const constraints = input.constraints.length > 0
      ? input.constraints.map(item => `- ${item}`).join('\n')
      : '- 无';
    const acceptance = input.acceptanceCriteria.length > 0
      ? input.acceptanceCriteria.map(item => `- ${item}`).join('\n')
      : '- 无';
    const steps = input.steps.length > 0
      ? input.steps.map(item => `- ${item}`).join('\n')
      : '- 无';
    const totalRequired = typeof input.requiredTotal === 'number' ? input.requiredTotal : undefined;
    const terminalRequired = typeof input.terminalRequired === 'number' ? input.terminalRequired : undefined;
    const pendingRequired = typeof input.pendingRequired === 'number' ? input.pendingRequired : undefined;
    const requiredSummary = (typeof totalRequired === 'number' && typeof terminalRequired === 'number')
      ? [
          `- 必需 Todo 总数: ${totalRequired}`,
          `- 已终态必需 Todo: ${terminalRequired}`,
          `- 剩余必需 Todo: ${Math.max(0, totalRequired - terminalRequired)}`,
          typeof pendingRequired === 'number' ? `- 运行或待处理必需 Todo: ${pendingRequired}` : '',
        ].filter(line => line.length > 0).join('\n')
      : '';
    const executionDirectives = [
      '这是执行轮，不是规划轮。',
      '请直接执行上述步骤，必要时调用工具或 worker_dispatch / worker_wait 继续推进，不要重复总结或只复述阶段计划。',
    ].join('\n');

    return [
      '[System] 你上一轮给出了结构化续航信号（下一步建议或阶段列表），已进入自动续跑。',
      `续跑轮次：${input.round}`,
      `原始目标：${goal}`,
      `用户原始请求：${input.originalPrompt}`,
      `约束：\n${constraints}`,
      `验收标准：\n${acceptance}`,
      requiredSummary ? `必需 Todo 进度：\n${requiredSummary}` : '',
      `结构化续航步骤：\n${steps}`,
      `执行要求：\n${executionDirectives}`,
      '若确实无法执行，请明确说明原因并输出最终结论。',
    ].filter(line => line && line.trim().length > 0).join('\n\n');
  }

  private beginSyntheticExecutionRound(input: {
    kind: 'auto_continuation' | 'auto_repair' | 'auto_governance_resume';
    round: number;
    message: string;
  }): string {
    const requestId = `req_${input.kind}_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    const phaseRuntime = this.getCurrentPlanPhaseRuntime(this.currentSessionId);
    this.messageHub.beginSyntheticRound(requestId, input.message, {
      phase: 'synthetic_execution_round',
      extra: {
        type: input.kind,
        round: input.round,
        syntheticRequest: true,
        currentPhaseIndex: phaseRuntime?.currentIndex,
        currentPhaseTitle: phaseRuntime?.currentTitle,
        nextPhaseIndex: phaseRuntime?.nextIndex,
        nextPhaseTitle: phaseRuntime?.nextTitle,
        continuationIntent: phaseRuntime?.continuationIntent,
      },
    });
    return requestId;
  }

  private getCurrentPlanPhaseRuntime(sessionId?: string): PlanRuntimePhaseState | undefined {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    const normalizedPlanId = typeof this.currentPlanId === 'string' ? this.currentPlanId.trim() : '';
    if (!normalizedSessionId || !normalizedPlanId) {
      return undefined;
    }
    return this.planLedger.getPlan(normalizedSessionId, normalizedPlanId)?.runtime.phase;
  }

  private resolveRequiredTotal(snapshot?: RuntimeTerminationSnapshot): number | undefined {
    if (!snapshot) {
      return undefined;
    }
    const raw = snapshot.requiredTotal;
    return typeof raw === 'number' && Number.isFinite(raw) ? raw : undefined;
  }

  private resolveTerminalRequired(snapshot?: RuntimeTerminationSnapshot): number | undefined {
    if (!snapshot) {
      return undefined;
    }
    const raw = snapshot.progressVector?.terminalRequiredTodos;
    return typeof raw === 'number' && Number.isFinite(raw) ? raw : undefined;
  }

  private extractPendingRequiredCount(snapshot?: RuntimeTerminationSnapshot): number {
    if (!snapshot) {
      return 0;
    }
    const raw = snapshot.runningOrPendingRequired;
    if (typeof raw === 'number' && Number.isFinite(raw)) {
      return Math.max(0, raw);
    }
    const total = this.resolveRequiredTotal(snapshot);
    const terminal = this.resolveTerminalRequired(snapshot);
    if (typeof total === 'number' && typeof terminal === 'number') {
      return Math.max(0, total - terminal);
    }
    return 0;
  }

  private buildFollowUpProgressSignature(snapshot?: RuntimeTerminationSnapshot): string {
    if (!snapshot) {
      return '';
    }
    const total = this.resolveRequiredTotal(snapshot);
    const terminal = this.resolveTerminalRequired(snapshot);
    const pending = this.extractPendingRequiredCount(snapshot);
    const failed = typeof snapshot.failedRequired === 'number' && Number.isFinite(snapshot.failedRequired)
      ? snapshot.failedRequired
      : 0;
    if (typeof total !== 'number' && typeof terminal !== 'number' && pending === 0 && failed === 0) {
      return '';
    }
    return `total:${total ?? 'na'}|terminal:${terminal ?? 'na'}|pending:${pending}|failed:${failed}`;
  }

  private requirePlanMutation(
    record: PlanRecord | null,
    context: {
      op: string;
      sessionId: string;
      planId: string;
      missionId?: string;
    },
  ): PlanRecord {
    if (record) {
      return record;
    }
    const errorPayload = {
      ...context,
      reason: 'ledger_write_rejected_or_conflicted',
    };
    logger.error('编排器.PlanLedger.关键写入失败', errorPayload, LogCategory.ORCHESTRATOR);
    const missionSegment = context.missionId ? `, missionId=${context.missionId}` : '';
    throw new Error(`PlanLedger 写入失败: op=${context.op}, sessionId=${context.sessionId}, planId=${context.planId}${missionSegment}`);
  }

  private mergeRequirementAnalysisWithPlan(
    base: RequirementAnalysis,
    plan: PlanRecord,
  ): RequirementAnalysis {
    const planAcceptance = plan.runtime.acceptance.criteria
      .map((item) => item.description?.trim())
      .filter((item): item is string => Boolean(item))
      .slice(0, 12);
    return {
      ...base,
      goal: plan.summary?.trim() || base.goal,
      analysis: plan.analysis?.trim() || base.analysis,
      constraints: plan.constraints.length > 0 ? [...plan.constraints] : base.constraints,
      acceptanceCriteria: planAcceptance.length > 0 ? planAcceptance : base.acceptanceCriteria,
    };
  }

  private async loadRecoveryPlanRecord(input: {
    missionId: string;
    preferredSessionId?: string;
    preferredPlanId?: string;
  }): Promise<PlanRecord | null> {
    const recoveryProjection = await this.readModelService.getRecoveryProjectionByMission({
      missionId: input.missionId,
      preferredSessionId: input.preferredSessionId,
      preferredPlanId: input.preferredPlanId,
    });
    if (!recoveryProjection) {
      return null;
    }
    return this.planLedger.getPlan(recoveryProjection.sessionId, recoveryProjection.planId);
  }

  private async executeNonTaskTurn(input: {
    prompt: string;
    images?: string[];
    requestId: string;
    requirementAnalysis: RequirementAnalysis;
  }): Promise<string> {
    const systemPrompt = input.requirementAnalysis.entryPath === 'lightweight_analysis'
      ? buildAnalysisSystemPrompt({
          workspaceRoot: this.workspaceRoot,
        })
      : buildDirectResponseSystemPrompt({
          workspaceRoot: this.workspaceRoot,
        });
    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      input.prompt,
      input.images,
      {
        planningMode: 'standard',
        source: 'orchestrator',
        adapterRole: 'orchestrator',
        requestId: input.requestId,
        includeThinking: input.requirementAnalysis.includeThinking ?? false,
        includeToolCalls: input.requirementAnalysis.includeToolCalls ?? false,
        allowedToolNames: input.requirementAnalysis.allowedToolNames,
        historyMode: input.requirementAnalysis.historyMode ?? 'isolated',
        systemPrompt,
        messageMetadata: { sessionId: this.currentSessionId },
      },
    );
    this.recordOrchestratorTokens(response.tokenUsage);
    if (response.error) {
      throw new Error(response.error);
    }
    const content = response.content?.trim();
    if (!content) {
      throw new Error(input.requirementAnalysis.entryPath === 'lightweight_analysis'
        ? '轻量分析路径未返回有效内容'
        : '直接回答路径未返回有效内容');
    }
    if (input.requirementAnalysis.entryPath === 'direct_response') {
      return this.sanitizeDirectResponseContent(input.prompt, content);
    }
    if (input.requirementAnalysis.entryPath === 'lightweight_analysis') {
      return this.sanitizeAnalysisResponseContent(content);
    }
    return content;
  }

  private mapRiskLevelToExecutionPath(
    riskLevel: 'low' | 'medium' | 'high',
  ): 'light' | 'standard' | 'full' {
    if (riskLevel === 'low') {
      return 'light';
    }
    if (riskLevel === 'high') {
      return 'full';
    }
    return 'standard';
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
   * 中断执行
   *
   * 语义变更：原来的 cancel() 直接终结执行链，现在改为将执行链标记为
   * interrupted + recoverable，保留恢复资格。
   * 真正的取消（不可恢复）由上层显式调用 abandonChain() 完成。
   */
  async cancel(): Promise<void> {
    // C-09: 取消活跃的 DispatchBatch，信号链传递到所有 Worker
    const activeBatch = this.dispatchManager.getActiveBatch();
    if (activeBatch && activeBatch.status === 'active') {
      const runningWorkers = activeBatch.getEntries()
        .filter(e => e.status === 'running')
        .map(e => e.worker);

      activeBatch.cancelAll(t('engine.cancel.userCancelled'));

      // 中断所有正在执行的 Worker LLM 请求
      for (const worker of runningWorkers) {
        try {
          await this.adapterFactory.interrupt(worker);
        } catch { /* 中断失败不阻塞 */ }
      }
    }

    // 将执行链标记为 interrupted（可恢复）
    if (this.activeChainId) {
      try {
        this.executionChainStore.transitionChainStatus(this.activeChainId, 'interrupted', {
          interruptedReason: 'user_stop',
          recoverable: true,
        });
      } catch (error) {
        logger.warn('编排器.执行链.中断状态转换失败', {
          chainId: this.activeChainId,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
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
   * 将执行链终态同步到 ExecutionChainStore
   */
  private finalizeChainStatus(finalStatus: 'completed' | 'failed' | 'cancelled' | 'paused' | undefined): void {
    if (!this.activeChainId || !finalStatus) return;
    const chain = this.executionChainStore.getChain(this.activeChainId);
    if (!chain) return;
    // 如果已经被 cancel() 标记为 interrupted，不覆盖为 cancelled/completed
    // （cancel 走 interrupted 路径，execute 方法的 isAbortError catch 会设 cancelled，
    //  但 interrupted 状态保留恢复资格，优先级更高）
    if (chain.status === 'interrupted') return;
    const targetStatus = finalStatus === 'paused' ? 'paused' as const : finalStatus;
    try {
      this.executionChainStore.transitionChainStatus(this.activeChainId, targetStatus);
    } catch (error) {
      logger.warn('编排器.执行链.终态同步失败', {
        chainId: this.activeChainId,
        targetStatus,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  // ===== 执行链查询暴露 =====

  /** 获取执行链查询服务 */
  getExecutionChainQuery(): ExecutionChainQueryService {
    return this.executionChainQuery;
  }

  /** 获取执行链存储（仅限内部子系统使用） */
  getExecutionChainStore(): ExecutionChainStore {
    return this.executionChainStore;
  }

  /** 获取恢复快照存储 */
  getResumeSnapshotStore(): ResumeSnapshotStore {
    return this.resumeSnapshotStore;
  }

  /** 获取恢复快照构建器 */
  getResumeSnapshotBuilder(): ResumeSnapshotBuilder {
    return this.resumeSnapshotBuilder;
  }

  /** 获取当前活跃的执行链 ID */
  getActiveChainId(): string | null {
    return this.activeChainId;
  }

  /**
   * 构建执行链前端摘要（供 bootstrap 发送）
   */
  buildExecutionChainSummary(sessionId: string): {
    hasRecoverableChain: boolean;
    recoverableChainId?: string;
    recoverableChainTitle?: string;
    lastChainStatus?: string;
  } {
    const recoverableChain = this.executionChainQuery.findLatestRecoverableChain(sessionId);
    const allChains = this.executionChainStore.getChainsBySession(sessionId);
    const lastChain = allChains.length > 0
      ? allChains.reduce((latest, chain) => chain.updatedAt > latest.updatedAt ? chain : latest)
      : null;

    return {
      hasRecoverableChain: recoverableChain !== null,
      recoverableChainId: recoverableChain?.id,
      recoverableChainTitle: recoverableChain?.currentMissionId,
      lastChainStatus: lastChain?.status,
    };
  }

  /**
   * 获取统计摘要
   */
  getStatsSummary(): string {
    const { inputTokens, outputTokens } = this.orchestratorTokens;
    return t('engine.stats.summary', { inputTokens, outputTokens });
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
    const missionProjection = await this.readModelService.getMissionProjection(taskId);
    if (!missionProjection) {
      throw new Error(t('engine.errors.taskNotFound', { taskId }));
    }
    if (!missionProjection.prompt?.trim()) {
      throw new Error(t('engine.errors.taskMissingPrompt', { taskId }));
    }
    const { prompt: userPrompt, sessionId } = missionProjection;
    // 触发统一执行链路（执行成功后再迁移原 draft 状态，避免先删后跑导致任务丢失）
    await this.execute(userPrompt, taskId, sessionId);
    try {
      // 状态迁移：将 draft 标记为已取消（被执行链路替代），保留审计记录
      const draftMission = await this.missionStorage.load(taskId);
      if (draftMission) {
        await this.missionStorage.transitionStatus(taskId, 'cancelled');
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
