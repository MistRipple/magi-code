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
  AcceptanceCriterion,
  Mission,
  MissionStorageManager,
  FileBasedMissionStorage,
  MissionDeliveryStatus,
  MissionContinuationPolicy,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { MessageHub } from './message-hub';
import type { RequestMessageSummary } from './message-pipeline';
import { WisdomManager } from '../wisdom';
import { buildUnifiedSystemPrompt } from '../prompts/orchestrator-prompts';
import { isAbortError } from '../../errors';
import { ConfigManager, type OrchestratorGovernanceThresholdsConfig } from '../../config';
import { SupplementaryInstructionQueue } from './supplementary-instruction-queue';
import { DispatchManager } from './dispatch-manager';
import { runPostDispatchVerification, type DeliveryVerificationOutcome } from './post-dispatch-verifier';
import { configureResilientAuxiliary } from './resilient-auxiliary-adapter';
import {
  FileTerminationMetricsRepository,
  type TerminationMetricsRecord,
  type TerminationMetricsRepository,
} from './termination-metrics-repository';
import {
  resolveTerminationReason,
  type OrchestratorTerminationReason,
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
  decideRecoveryAction,
  deriveReplanGateSignals,
  isGovernanceAutoRecoverReason,
  type ReplanGateSignals,
  type ReplanSource,
} from './recovery-decision-kernel';

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

interface PlanGovernanceAssessment {
  riskScore: number;
  confidence: number;
  affectedFiles: number;
  crossModules: number;
  writeToolRatio: number;
  historicalFailureRate: number;
  sourceCoverage: number;
  signalAgreement: number;
  historicalCalibration: number;
  decision: 'ask' | 'auto';
  reasons: string[];
}

interface RuntimeTerminationSnapshot {
  snapshotId?: string;
  progressVector?: {
    terminalRequiredTodos?: number;
    acceptedCriteria?: number;
    criticalPathResolved?: number;
    unresolvedBlockers?: number;
  };
  reviewState?: {
    accepted?: number;
    total?: number;
  };
  blockerState?: {
    open?: number;
    score?: number;
    externalWaitOpen?: number;
    maxExternalWaitAgeMs?: number;
  };
  budgetState?: {
    elapsedMs?: number;
    tokenUsed?: number;
    errorRate?: number;
  };
  requiredTotal?: number;
  failedRequired?: number;
  runningOrPendingRequired?: number;
  runningRequired?: number;
  sourceEventIds?: string[];
}

interface RuntimeTerminationShadow {
  enabled: boolean;
  reason: string;
  consistent: boolean;
  note?: string;
}

interface RuntimeTerminationDecisionTraceEntry {
  round?: number;
  phase?: 'no_tool' | 'tool' | 'handoff' | 'finalize';
  action?: 'continue' | 'continue_with_prompt' | 'terminate' | 'handoff' | 'fallback';
  requiredTotal?: number;
  reason?: string;
  candidates?: string[];
  gateState?: {
    noProgressStreak?: number;
    budgetBreachStreak?: number;
    externalWaitBreachStreak?: number;
    consecutiveUpstreamModelErrors?: number;
  };
  note?: string;
  timestamp?: number;
}

type ResolvedOrchestratorTerminationReason = Exclude<OrchestratorTerminationReason, 'unknown'>;

interface PlanLedgerMissionScope {
  missionId: string;
  sessionId: string;
  planId: string;
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
  private readonly terminationMetricsRepository: TerminationMetricsRepository;

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
  private pendingDeliveryRepairConfirmation: {
    missionId: string;
    resolve: (decision: 'repair' | 'stop') => void;
  } | null = null;
  /** dispatch 并发场景下的 Mission 创建单飞锁，确保每轮最多创建一个 Mission */
  private ensureMissionPromise: Promise<string> | null = null;
  /** 当前轮次 draft 前唯一结构化需求合同 */
  private currentRequirementAnalysis: RequirementAnalysis | null = null;
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
  private lastExecutionRuntimeReason: ResolvedOrchestratorTerminationReason = 'completed';
  private lastExecutionFinalStatus: 'completed' | 'failed' | 'cancelled' | 'paused' = 'completed';
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
  // 当前执行的用户原始图片路径（Worker dispatch 传递）
  private activeImagePaths?: string[];

  // 交互模式
  private interactionMode: InteractionMode = 'auto';
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.auto;
  private workspaceFileIndexCache?: { builtAt: number; files: string[]; modules: string[] };
  // 运行状态
  private isRunning = false;
  private executionQueue: Promise<void> = Promise.resolve();
  private pendingCount = 0;

  // P0-3: 补充指令队列（独立状态机）
  private supplementaryQueue: SupplementaryInstructionQueue;

  // P1-4: Dispatch 调度管理器（独立子系统）
  private dispatchManager: DispatchManager;
  private readonly configManager: ConfigManager;

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
    this.configManager = ConfigManager.getInstance();

    // 初始化基础组件
    this.profileLoader = ProfileLoader.getInstance();
    this.guidanceInjector = new GuidanceInjector();
    this.contextManager = new ContextManager(workspaceRoot, undefined, sessionManager);
    this.executionStats = new ExecutionStats();
    this.supplementaryQueue = new SupplementaryInstructionQueue(this);
    this.wisdomManager = new WisdomManager();
    this.terminationMetricsRepository = new FileTerminationMetricsRepository(this.workspaceRoot);

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
      getPlanLedger: () => this.planLedger ?? null,
      getCurrentPlanId: () => this.currentPlanId ?? null,
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

  private async evaluatePlanGovernance(sessionId: string, plan: PlanRecord, userPrompt: string): Promise<PlanGovernanceAssessment> {
    const analysisSignal = this.estimateAnalysisSignal(plan, userPrompt);
    const staticSignal = this.estimateStaticSignal(userPrompt);
    const historical = this.estimateHistoricalSignal(sessionId, plan.planId);

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
    const writeToolRatio = this.estimateWriteToolRatio(userPrompt, plan.mode);
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

  private buildGovernanceSummary(assessment: PlanGovernanceAssessment): string {
    return [
      '### 治理评估',
      `- risk_score: ${assessment.riskScore.toFixed(3)}`,
      `- confidence: ${assessment.confidence.toFixed(3)}`,
      `- affected_files: ${assessment.affectedFiles}`,
      `- cross_modules: ${assessment.crossModules}`,
      `- write_tool_ratio: ${assessment.writeToolRatio.toFixed(3)}`,
      `- historical_failure_rate: ${assessment.historicalFailureRate.toFixed(3)}`,
      `- source_coverage: ${assessment.sourceCoverage.toFixed(3)}`,
      `- signal_agreement: ${assessment.signalAgreement.toFixed(3)}`,
      `- historical_calibration: ${assessment.historicalCalibration.toFixed(3)}`,
      `- approval_decision: ${assessment.decision}`,
      `- gate_reasons: ${assessment.reasons.join('; ') || 'none'}`,
    ].join('\n');
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
    if (mode === 'deep') {
      return 0.9;
    }
    const text = (prompt || '').toLowerCase();
    const readOnlyKeywords = ['总结', '分析', '读取', '解释', 'review', 'summarize', 'read only', 'diagnose'];
    if (readOnlyKeywords.some((keyword) => text.includes(keyword))) {
      return 0.2;
    }
    return 0.7;
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

  private estimateStaticSignal(prompt: string): GovernanceSignalEstimate {
    const index = this.getWorkspaceFileIndex();
    const files = new Set<string>();
    const modules = new Set<string>();
    const tokens = this.extractPromptTokens(prompt);
    if (tokens.length === 0) {
      return { files, modules, available: false };
    }

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

  private getWorkspaceFileIndex(): { files: string[]; modules: string[] } {
    const now = Date.now();
    if (this.workspaceFileIndexCache && now - this.workspaceFileIndexCache.builtAt < 60_000) {
      return {
        files: this.workspaceFileIndexCache.files,
        modules: this.workspaceFileIndexCache.modules,
      };
    }

    const files: string[] = [];
    const modules = new Set<string>();
    const excluded = new Set(['.git', 'node_modules', '.magi', 'out', 'dist', 'build', 'coverage']);
    const collect = (currentDir: string, depth: number): void => {
      if (files.length >= 5000 || depth > 12) {
        return;
      }
      let entries: fs.Dirent[] = [];
      try {
        entries = fs.readdirSync(currentDir, { withFileTypes: true });
      } catch {
        return;
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
        const absolutePath = path.join(currentDir, entry.name);
        if (entry.isDirectory()) {
          collect(absolutePath, depth + 1);
          continue;
        }
        const relative = this.normalizeRelativePath(path.relative(this.workspaceRoot, absolutePath));
        if (!relative) {
          continue;
        }
        files.push(relative);
        modules.add(this.inferModuleFromPath(relative));
      }
    };

    collect(this.workspaceRoot, 0);
    const moduleList = Array.from(modules).filter(Boolean);
    this.workspaceFileIndexCache = {
      builtAt: now,
      files,
      modules: moduleList,
    };
    return { files, modules: moduleList };
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

  resolvePlanConfirmation(confirmed: boolean): boolean {
    if (!this.pendingPlanConfirmation) {
      return false;
    }
    const pending = this.pendingPlanConfirmation;
    this.pendingPlanConfirmation = null;
    pending.resolve(confirmed);
    return true;
  }

  resolveDeliveryRepairConfirmation(decision: 'repair' | 'stop'): boolean {
    if (!this.pendingDeliveryRepairConfirmation) {
      return false;
    }
    const pending = this.pendingDeliveryRepairConfirmation;
    this.pendingDeliveryRepairConfirmation = null;
    pending.resolve(decision);
    return true;
  }

  /**
   * 核心层会话锚点同步：
   * 不依赖 UI/宿主适配层，确保 MessageHub trace 与真实 sessionId 对齐。
   */
  private alignMessageHubTrace(sessionId?: string): void {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (!normalizedSessionId) {
      return;
    }
    if (this.messageHub.getTraceId() === normalizedSessionId) {
      return;
    }
    this.messageHub.setTraceId(normalizedSessionId);
    logger.debug('编排器.消息.Trace.同步', {
      traceId: normalizedSessionId,
    }, LogCategory.ORCHESTRATOR);
  }

  private async awaitPlanConfirmation(
    sessionId: string,
    plan: PlanRecord,
    fallbackFormattedPlan: string,
    options?: {
      forceManual?: boolean;
      governanceSummary?: string;
    },
  ): Promise<boolean> {
    this.alignMessageHubTrace(sessionId);
    const awaitingPlan = await this.planLedger.markAwaitingConfirmation(sessionId, plan.planId, fallbackFormattedPlan);
    const displayPlan = awaitingPlan || plan;
    const governanceSummary = typeof options?.governanceSummary === 'string'
      ? options.governanceSummary.trim()
      : '';
    const baseFormattedPlan = displayPlan.formattedPlan || fallbackFormattedPlan;
    const formattedPlan = governanceSummary
      ? `${baseFormattedPlan}\n\n${governanceSummary}`
      : baseFormattedPlan;

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
      forceManual: options?.forceManual === true,
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

  private async awaitDeliveryRepairConfirmation(input: {
    sessionId: string;
    missionId: string;
    summary: string;
    details?: string;
    round: number;
    maxRounds: number;
    requestType?: 'delivery_repair' | 'replan_followup';
  }): Promise<'repair' | 'stop'> {
    this.alignMessageHubTrace(input.sessionId);
    this.messageHub.data('deliveryRepairRequest', {
      sessionId: input.sessionId,
      missionId: input.missionId,
      summary: input.summary,
      details: input.details,
      round: input.round,
      maxRounds: input.maxRounds,
      requestType: input.requestType || 'delivery_repair',
    });
    this.setState('waiting_confirmation');
    return new Promise<'repair' | 'stop'>((resolve) => {
      this.pendingDeliveryRepairConfirmation = {
        missionId: input.missionId,
        resolve,
      };
    });
  }

  private emitPlanLedgerUpdate(sessionId: string, reason: string): void {
    this.alignMessageHubTrace(sessionId);
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
      category: payload.category,
      dependsOn: payload.dependsOn,
      scopeHints: payload.scopeHint,
      targetFiles: payload.files,
      requiresModification: payload.requiresModification,
    });
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
    runner: (scope: PlanLedgerMissionScope) => Promise<void>,
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
  ): Promise<PlanLedgerMissionScope | null> {
    const missionId = typeof rawMissionId === 'string' ? rawMissionId.trim() : '';
    if (!missionId) {
      logger.warn('编排器.计划账本.事件缺少missionId', {
        action,
      }, LogCategory.ORCHESTRATOR);
      return null;
    }

    const currentSessionId = this.currentSessionId?.trim();
    const currentPlanId = this.currentPlanId?.trim();
    if (currentSessionId && currentPlanId) {
      const currentPlan = this.planLedger.getPlan(currentSessionId, currentPlanId);
      if (currentPlan && (currentPlan.missionId || '').trim() === missionId) {
        return {
          missionId,
          sessionId: currentSessionId,
          planId: currentPlanId,
        };
      }
    }

    const mission = await this.missionStorage.load(missionId);
    const sessionId = typeof mission?.sessionId === 'string' ? mission.sessionId.trim() : '';
    if (!sessionId) {
      logger.warn('编排器.计划账本.事件定位失败_缺少mission会话', {
        action,
        missionId,
      }, LogCategory.ORCHESTRATOR);
      return null;
    }

    const plan = this.planLedger.getLatestPlanByMission(sessionId, missionId);
    if (!plan) {
      logger.warn('编排器.计划账本.事件定位失败_缺少活动计划', {
        action,
        missionId,
        sessionId,
      }, LogCategory.ORCHESTRATOR);
      return null;
    }

    return {
      missionId,
      sessionId,
      planId: plan.planId,
    };
  }

  private setupPlanLedgerEventBindings(): void {
    this.planLedger.on('updated', (event: { sessionId: string; reason: string }) => {
      this.emitPlanLedgerUpdate(event.sessionId, event.reason);
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

      this.isRunning = true;
      this.currentTaskId = taskId || null;
      this.lastMissionId = null;
      this.currentPlanId = null;
      this.currentRequirementAnalysis = null;
      this.lastExecutionRuntimeSnapshot = null;
      this.lastExecutionRuntimeDecisionTrace = [];
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
      let currentPlanMode: PlanMode = this.adapterFactory.isDeepTask() ? 'deep' : 'standard';
      const rootRequestId = typeof requestId === 'string' && requestId.trim().length > 0
        ? requestId.trim()
        : `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      let currentRoundRequestId = rootRequestId;

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        this.currentSessionId = resolvedSessionId;
        this.alignMessageHubTrace(resolvedSessionId);
        this.dispatchManager.resetForNewExecutionCycle();
        await this.ensureContextReady(resolvedSessionId);

        const adapterPlanMode: PlanMode = this.adapterFactory.isDeepTask() ? 'deep' : 'standard';
        currentPlanMode = adapterPlanMode;
        const requirementAnalysis = this.buildRequirementAnalysis(trimmedPrompt, adapterPlanMode);
        this.currentRequirementAnalysis = requirementAnalysis;

        let executionPlan: PlanRecord | null = null;
        const resumeMissionIdForExecution = this.resumeMissionId?.trim();
        if (resumeMissionIdForExecution) {
          const resumedPlan = this.planLedger.getLatestPlanByMission(
            resolvedSessionId,
            resumeMissionIdForExecution,
          );
          if (!resumedPlan) {
            throw new Error(`任务 ${resumeMissionIdForExecution} 缺少可恢复计划，已终止恢复执行`);
          }
          currentPlanMode = resumedPlan.mode;
          this.currentPlanId = resumedPlan.planId;
          this.currentRequirementAnalysis = this.mergeRequirementAnalysisWithPlan(requirementAnalysis, resumedPlan);

          const resumedExecuting = await this.planLedger.markExecuting(
            resolvedSessionId,
            resumedPlan.planId,
            {
              expectedRevision: resumedPlan.revision,
              auditReason: 'resume-mission-ledger-recovery',
            },
          );
          executionPlan = this.requirePlanMutation(resumedExecuting, {
            op: 'resume-markExecuting',
            sessionId: resolvedSessionId,
            planId: resumedPlan.planId,
            missionId: resumeMissionIdForExecution,
          });

          logger.info('编排器.PlanLedger.恢复执行计划', {
            sessionId: resolvedSessionId,
            missionId: resumeMissionIdForExecution,
            planId: executionPlan.planId,
            mode: executionPlan.mode,
            revision: executionPlan.revision,
          }, LogCategory.ORCHESTRATOR);
        }

        if (!executionPlan) {
          const draftPlan = await this.planLedger.createDraft({
            sessionId: resolvedSessionId,
            turnId: this.currentTurnId || `turn:${Date.now()}`,
            missionId: this.currentTurnId || undefined,
            mode: adapterPlanMode,
            prompt: trimmedPrompt,
            summary: requirementAnalysis.goal,
            analysis: requirementAnalysis.analysis,
            constraints: requirementAnalysis.constraints,
            acceptanceCriteria: requirementAnalysis.acceptanceCriteria,
            riskLevel: requirementAnalysis.riskLevel,
          });
          this.currentPlanId = draftPlan.planId;

          const fallbackFormattedPlan = draftPlan.formattedPlan || this.planLedger.formatPlanForDisplay(draftPlan);
          let governanceAssessment: PlanGovernanceAssessment;
          try {
            governanceAssessment = await this.evaluatePlanGovernance(resolvedSessionId, draftPlan, trimmedPrompt);
          } catch (error) {
            // 【错误边界】治理评估依赖外部模型调用，异常时降级到安全侧默认值（人工确认）。
            // 这不是"兼容性回退"——治理评估失败不应阻塞整个任务流程，
            // 而应降级到最安全的策略（要求人工确认），确保用户有机会审查计划。
            logger.warn('编排器.治理评估.降级到人工确认', {
              sessionId: resolvedSessionId,
              planId: draftPlan.planId,
              error: error instanceof Error ? error.message : String(error),
            }, LogCategory.ORCHESTRATOR);
            governanceAssessment = this.buildFallbackGovernanceAssessment(error);
          }
          const governanceSummary = this.buildGovernanceSummary(governanceAssessment);

          // 产品约束：计划确认门禁只在 deep + ask 组合下触发。
          // standard 模式下不弹计划确认，避免轻量任务被重型交互打断。
          const shouldAskConfirmation = this.interactionMode === 'ask' && adapterPlanMode === 'deep';
          const forceManual = false;

          if (shouldAskConfirmation) {
            const confirmed = await this.awaitPlanConfirmation(
              resolvedSessionId,
              draftPlan,
              fallbackFormattedPlan,
              {
                forceManual,
                governanceSummary,
              },
            );
            if (!confirmed) {
              const rejectReason = t('engine.plan.userCancelledReason');
              this.requirePlanMutation(
                await this.planLedger.reject(
                  resolvedSessionId,
                  draftPlan.planId,
                  'user',
                  rejectReason,
                  {
                    expectedRevision: draftPlan.revision,
                    auditReason: 'plan-governance:user-reject',
                  },
                ),
                {
                  op: 'plan-reject',
                  sessionId: resolvedSessionId,
                  planId: draftPlan.planId,
                },
              );
              orchestratorRuntimeReason = 'cancelled';
              this.lastExecutionRuntimeReason = orchestratorRuntimeReason;
              this.lastExecutionFinalStatus = 'cancelled';
              this.lastExecutionErrors = [];
              this.setState('idle');
              this.currentTaskId = null;
              return t('engine.plan.userCancelledResult');
            }
            const approvedPlan = this.requirePlanMutation(
              await this.planLedger.approve(
                resolvedSessionId,
                draftPlan.planId,
                'user',
                `governance:manual;risk=${governanceAssessment.riskScore.toFixed(3)};confidence=${governanceAssessment.confidence.toFixed(3)};reasons=${governanceAssessment.reasons.join('|') || 'none'}`,
                {
                  expectedRevision: draftPlan.revision,
                  auditReason: 'plan-governance:user-approve',
                },
              ),
              {
                op: 'plan-approve-manual',
                sessionId: resolvedSessionId,
                planId: draftPlan.planId,
              },
            );
            const executingPlan = await this.planLedger.markExecuting(resolvedSessionId, draftPlan.planId, {
              expectedRevision: approvedPlan.revision,
              auditReason: 'plan-governance:manual-mark-executing',
            });
            executionPlan = this.requirePlanMutation(executingPlan, {
              op: 'plan-mark-executing-manual',
              sessionId: resolvedSessionId,
              planId: draftPlan.planId,
            });
            this.setState('running');
          } else {
            const approvedPlan = this.requirePlanMutation(
              await this.planLedger.approve(
                resolvedSessionId,
                draftPlan.planId,
                'system:auto',
                `governance:auto;decision=${governanceAssessment.decision};risk=${governanceAssessment.riskScore.toFixed(3)};confidence=${governanceAssessment.confidence.toFixed(3)};reasons=${governanceAssessment.reasons.join('|') || 'none'}`,
                {
                  expectedRevision: draftPlan.revision,
                  auditReason: 'plan-governance:auto-approve',
                },
              ),
              {
                op: 'plan-approve-auto',
                sessionId: resolvedSessionId,
                planId: draftPlan.planId,
              },
            );
            const executingPlan = await this.planLedger.markExecuting(resolvedSessionId, draftPlan.planId, {
              expectedRevision: approvedPlan.revision,
              auditReason: 'plan-governance:auto-mark-executing',
            });
            executionPlan = this.requirePlanMutation(executingPlan, {
              op: 'plan-mark-executing-auto',
              sessionId: resolvedSessionId,
              planId: draftPlan.planId,
            });
          }
        }

        orchestratorAttemptTargetId = this.currentTurnId || `orchestrator-${Date.now()}`;
        this.requirePlanMutation(
          await this.planLedger.startAttempt(resolvedSessionId, executionPlan.planId, {
            scope: 'orchestrator',
            targetId: orchestratorAttemptTargetId,
            reason: 'orchestrator-execution-started',
          }, {
            expectedRevision: executionPlan.revision,
            auditReason: 'orchestrator-execution-start',
          }),
          {
            op: 'orchestrator-start-attempt',
            sessionId: resolvedSessionId,
            planId: executionPlan.planId,
          },
        );
        orchestratorAttemptStarted = true;

        // 1. 组装上下文
        const context = await this.prepareContext(resolvedSessionId, trimmedPrompt);

        // 2. 获取项目上下文和 ADR
        const projectContext = this.projectKnowledgeBase
          ? this.projectKnowledgeBase.getProjectContext(600)
          : undefined;

        const knowledgeIndex = this.projectKnowledgeBase
          ? this.projectKnowledgeBase.getKnowledgeIndex(600)
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
          deepTask: this.adapterFactory.isDeepTask(),
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

        const autoRepairMaxRoundsRaw = this.config.delivery?.autoRepairMaxRounds;
        const autoRepairMaxRoundsValue = Number(autoRepairMaxRoundsRaw);
        const autoRepairMaxRounds = Number.isFinite(autoRepairMaxRoundsValue)
          ? Math.max(0, autoRepairMaxRoundsValue)
          : 0;
        const autoRepairMaxRoundsLabel = autoRepairMaxRounds > 0
          ? autoRepairMaxRounds
          : t('common.unlimited');
        const autoRepairStallThreshold = 3;
        const governanceRecoveryDelays = [10000, 20000, 30000, 40000, 50000];
        const governanceRecoveryMaxRounds = governanceRecoveryDelays.length;
        let autoRepairAttempt = 0;
        let autoFollowUpAttempt = 0;
        let autoFollowUpNoExecutionRetry = 0;
        let lastFollowUpSignature = '';
        let lastFollowUpProgressSignature = '';
        let followUpStallStreak = 0;
        let lastAutoRepairSignature = '';
        let autoRepairStallStreak = 0;
        let governanceRecoveryAttempt = 0;
        let promptForRound = trimmedPrompt;

        // 5. 编排执行（支持交付失败后的自动修复闭环）
        while (true) {
          const requestStatsBeforeRound = this.captureRequestMessageSummary(currentRoundRequestId);
          if (currentRoundRequestId !== rootRequestId) {
            this.dispatchManager.resetForNewExecutionCycle();
          }

          deliveryStatusForMission = null;
          deliverySummaryForMission = undefined;
          deliveryDetailsForMission = undefined;
          deliveryWarningsForMission = undefined;
          continuationPolicyForMission = undefined;
          continuationReasonForMission = undefined;

          this.activeUserPrompt = promptForRound;
          orchestratorRuntimeReason = undefined;
          orchestratorRuntimeSnapshot = undefined;
          orchestratorRuntimeShadow = undefined;
          orchestratorRuntimeDecisionTrace = undefined;

          const response = await this.adapterFactory.sendMessage(
            'orchestrator',
            promptForRound,
            imagePaths,
            {
              source: 'orchestrator',
              adapterRole: 'orchestrator',
              systemPrompt,
              includeToolCalls: true,
              requestId: currentRoundRequestId,
            }
          );
          const requestStatsAfterRound = this.captureRequestMessageSummary(currentRoundRequestId);
          orchestratorRuntimeReason = this.normalizeOrchestratorRuntimeReason(response.orchestratorRuntime?.reason);
          orchestratorRuntimeRounds = response.orchestratorRuntime?.rounds || 0;
          orchestratorRuntimeSnapshot = response.orchestratorRuntime?.snapshot as RuntimeTerminationSnapshot | undefined;
          orchestratorRuntimeShadow = response.orchestratorRuntime?.shadow as RuntimeTerminationShadow | undefined;
          orchestratorRuntimeDecisionTrace =
            response.orchestratorRuntime?.decisionTrace as RuntimeTerminationDecisionTraceEntry[] | undefined;
          runtimeTokenUsage = response.tokenUsage;

          this.recordOrchestratorTokens(response.tokenUsage);

          // 等待 dispatch batch 归档（含 Worker 执行 + Phase C 汇总）
          // worker_dispatch 是非阻塞的，sendMessage 返回时 Worker 可能还在后台执行。
          // 必须等待 activeBatch 归档后再推进下一阶段，保证链路完整闭合。
          const currentBatch = this.dispatchManager.getActiveBatch();
          if (currentBatch && currentBatch.status !== 'archived') {
            await currentBatch.waitForArchive(this.dispatchManager.getIdleTimeoutMs());
          }

          const executionWarnings: string[] = [];
          const deliveryNotes: string[] = [];
          const terminationCandidates: TerminationCandidate[] = [];
          const auditOutcome = currentBatch?.getAuditOutcome();
          if (auditOutcome?.level === 'intervention') {
            const blockedMessage = t('engine.phaseC.interventionBlocked');
            deliveryNotes.push(blockedMessage);
            deliveryStatusForMission = 'blocked';
            deliverySummaryForMission = blockedMessage;
            continuationPolicyForMission = 'ask';
            continuationReasonForMission = blockedMessage;
            logger.warn('编排器.PhaseC.审计阻断_已降级', {
              batchId: currentBatch?.id,
              level: auditOutcome.level,
            }, LogCategory.ORCHESTRATOR);
          }

          if (response.error) {
            const modelError = response.error.trim();
            if (modelError) {
              executionWarnings.push(`上游模型异常：${modelError}`);
            } else {
              executionWarnings.push('上游模型异常：未知错误');
            }
            terminationCandidates.push({
              reason: 'upstream_model_error',
              eventId: 'engine:upstream-model-error',
              triggeredAt: Date.now(),
            });
            logger.warn('编排器.统一执行.上游模型异常_已降级', {
              error: response.error,
            }, LogCategory.ORCHESTRATOR);
          }

          if (currentBatch) {
            const planIdForRuntime = this.currentPlanId;
            const planForRuntime = planIdForRuntime
              ? this.planLedger.getPlan(resolvedSessionId, planIdForRuntime)
              : null;
            const reviewRoundForRuntime = Math.max(1, (planForRuntime?.runtime.review.round || 0) + 1);
            const acceptanceCriteriaForRuntime = planForRuntime?.runtime.acceptance.criteria;

            // Phase transition: executing → reviewing
            if (this.lastMissionId) {
              try {
                await this.missionStorage.transitionStatus(this.lastMissionId, 'reviewing');
              } catch (error) {
                logger.warn('编排器.Mission.状态迁移失败', {
                  missionId: this.lastMissionId,
                  from: 'executing',
                  to: 'reviewing',
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            if (planIdForRuntime) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, planIdForRuntime, {
                  review: { state: 'running', round: reviewRoundForRuntime },
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.评审状态推进失败', {
                  sessionId: resolvedSessionId,
                  planId: planIdForRuntime,
                  state: 'running',
                  round: reviewRoundForRuntime,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }

            let outcome: DeliveryVerificationOutcome;
            try {
              outcome = await runPostDispatchVerification(
                currentBatch,
                this.verificationRunner,
                this.messageHub,
                {
                  workspaceRoot: this.workspaceRoot,
                  acceptanceCriteria: acceptanceCriteriaForRuntime,
                },
              );
            } catch (verificationError) {
              const verificationMessage = verificationError instanceof Error
                ? verificationError.message
                : String(verificationError);
              logger.warn('编排器.PhaseC.校验异常_已降级', {
                batchId: currentBatch.id,
                error: verificationMessage,
              }, LogCategory.ORCHESTRATOR);
              outcome = {
                status: 'failed',
                summary: `验收异常：${verificationMessage}`,
              };
            }
            const acceptanceCriteriaWithSpec = this.mergeAcceptanceCriteriaWithSpecResults({
              criteria: acceptanceCriteriaForRuntime,
              specResults: outcome.specResults,
              reviewRound: reviewRoundForRuntime,
              batchId: currentBatch.id,
              workers: currentBatch.getEntries().map((entry) => entry.worker),
            });
            const shouldUpdateAcceptanceCriteria = Array.isArray(outcome.specResults)
              && outcome.specResults.length > 0
              && acceptanceCriteriaWithSpec.length > 0;
            if (planIdForRuntime) {
              try {
                if (outcome.status === 'failed') {
                  await this.planLedger.updateRuntimeState(resolvedSessionId, planIdForRuntime, {
                    review: { state: 'rejected', round: reviewRoundForRuntime },
                    acceptance: {
                      summary: 'failed',
                      criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
                    },
                    replan: { state: 'required', reason: outcome.summary },
                  });
                } else if (outcome.status === 'passed') {
                  await this.planLedger.updateRuntimeState(resolvedSessionId, planIdForRuntime, {
                    review: { state: 'accepted', round: reviewRoundForRuntime },
                    acceptance: {
                      summary: 'passed',
                      criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
                    },
                    replan: { state: 'none' },
                  });
                } else if (outcome.skippedReason === 'execution_failed') {
                  await this.planLedger.updateRuntimeState(resolvedSessionId, planIdForRuntime, {
                    review: { state: 'rejected', round: reviewRoundForRuntime },
                    acceptance: {
                      summary: 'failed',
                      criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
                    },
                    replan: { state: 'required', reason: outcome.summary },
                  });
                } else {
                  await this.planLedger.updateRuntimeState(resolvedSessionId, planIdForRuntime, {
                    review: { state: 'idle', round: reviewRoundForRuntime },
                    acceptance: {
                      criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
                    },
                    replan: { state: 'none' },
                  });
                }
              } catch (error) {
                logger.warn('编排器.PlanRuntime.评审结果回写失败', {
                  sessionId: resolvedSessionId,
                  planId: planIdForRuntime,
                  status: outcome.status,
                  skippedReason: outcome.skippedReason,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }

            if (outcome.status === 'failed') {
              deliveryNotes.push(outcome.summary);
              if (deliveryStatusForMission === 'blocked') {
                if (!deliverySummaryForMission) {
                  deliverySummaryForMission = outcome.summary;
                }
                if (!deliveryDetailsForMission && outcome.details) {
                  deliveryDetailsForMission = outcome.details;
                }
                if (!deliveryWarningsForMission && outcome.warnings && outcome.warnings.length > 0) {
                  deliveryWarningsForMission = outcome.warnings;
                }
                if (!continuationPolicyForMission) {
                  continuationPolicyForMission = this.interactionMode === 'ask' ? 'ask' : 'auto';
                }
                if (!continuationReasonForMission) {
                  continuationReasonForMission = outcome.summary;
                }
              } else {
                deliveryStatusForMission = 'failed';
                deliverySummaryForMission = outcome.summary;
                deliveryDetailsForMission = outcome.details;
                deliveryWarningsForMission = outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined;
                continuationPolicyForMission = this.interactionMode === 'ask' ? 'ask' : 'auto';
                continuationReasonForMission = outcome.summary;
              }
            } else if (outcome.status === 'passed') {
              if (deliveryStatusForMission === 'blocked') {
                if (!deliverySummaryForMission) {
                  deliverySummaryForMission = outcome.summary;
                }
                if (!deliveryDetailsForMission && outcome.details) {
                  deliveryDetailsForMission = outcome.details;
                }
                if (!deliveryWarningsForMission && outcome.warnings && outcome.warnings.length > 0) {
                  deliveryWarningsForMission = outcome.warnings;
                }
                if (!continuationPolicyForMission) {
                  continuationPolicyForMission = 'stop';
                }
                if (!continuationReasonForMission) {
                  continuationReasonForMission = outcome.summary;
                }
              } else {
                deliveryStatusForMission = 'passed';
                deliverySummaryForMission = outcome.summary;
                deliveryDetailsForMission = outcome.details;
                deliveryWarningsForMission = outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined;
                continuationPolicyForMission = 'stop';
                continuationReasonForMission = outcome.summary;
              }
            } else {
              const skippedStatus: MissionDeliveryStatus = outcome.skippedReason === 'execution_failed'
                ? 'blocked'
                : 'skipped';
              if (outcome.skippedReason === 'execution_failed') {
                deliveryNotes.push(outcome.summary);
              }
              if (deliveryStatusForMission === 'blocked') {
                if (!deliverySummaryForMission) {
                  deliverySummaryForMission = outcome.summary;
                }
                if (!deliveryDetailsForMission && outcome.details) {
                  deliveryDetailsForMission = outcome.details;
                }
                if (!deliveryWarningsForMission && outcome.warnings && outcome.warnings.length > 0) {
                  deliveryWarningsForMission = outcome.warnings;
                }
                if (!continuationPolicyForMission) {
                  continuationPolicyForMission = skippedStatus === 'blocked' ? 'ask' : 'stop';
                }
                if (!continuationReasonForMission) {
                  continuationReasonForMission = outcome.summary;
                }
              } else {
                deliveryStatusForMission = skippedStatus;
                deliverySummaryForMission = outcome.summary;
                deliveryDetailsForMission = outcome.details;
                deliveryWarningsForMission = outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined;
                continuationPolicyForMission = skippedStatus === 'blocked' ? 'ask' : 'stop';
                continuationReasonForMission = outcome.summary;
              }
            }
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

          const resolvedRuntimeTermination = this.resolveOrchestratorRuntimeReason({
            runtimeReason: orchestratorRuntimeReason,
            runtimeSnapshot: orchestratorRuntimeSnapshot,
            additionalCandidates: terminationCandidates,
            fallback: terminationCandidates.length > 0 ? 'failed' : 'completed',
          });
          orchestratorRuntimeReason = resolvedRuntimeTermination.reason;
          orchestratorRuntimeSnapshot = resolvedRuntimeTermination.runtimeSnapshot;
          const finalExecutionStatus = this.resolveExecutionFinalStatus(orchestratorRuntimeReason, orchestratorRuntimeSnapshot);
          const normalizedRuntimeReason = this.normalizeOrchestratorRuntimeReason(orchestratorRuntimeReason);
          const isGovernancePaused = finalExecutionStatus === 'paused'
            && normalizedRuntimeReason
            && this.isGovernancePauseReason(normalizedRuntimeReason);
          if (finalExecutionStatus === 'completed' && normalizedRuntimeReason && normalizedRuntimeReason !== 'completed') {
            executionWarnings.push(`终止门禁判定为 ${normalizedRuntimeReason}，但必需 Todo 已完成，按执行完成处理。`);
          }

          if (executionWarnings.length > 0) {
            const warningSection = [
              '[System] 本轮触发门禁降级（会话不中断）：',
              ...executionWarnings.map(item => `- ${item}`),
            ].join('\n');
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${warningSection}`
              : warningSection;
            this.messageHub.result(warningSection, {
              metadata: { phase: 'system_section', extra: { type: 'execution_warning' } },
            });
          }

          if (deliveryNotes.length > 0) {
            const deliverySection = [
              '[System] 交付验收结果：',
              ...deliveryNotes.map(item => `- ${item}`),
            ].join('\n');
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${deliverySection}`
              : deliverySection;
            this.messageHub.result(deliverySection, {
              metadata: { phase: 'system_section', extra: { type: 'delivery_verification' } },
            });
          }

          const autoRepairProgressSignature = [
            deliverySummaryForMission?.trim() ?? '',
            deliveryDetailsForMission?.trim() ?? '',
            this.buildFollowUpProgressSignature(orchestratorRuntimeSnapshot),
          ].filter(Boolean).join('|');
          let autoRepairStalled = false;
          if (deliveryStatusForMission === 'failed') {
            if (autoRepairProgressSignature && autoRepairProgressSignature === lastAutoRepairSignature) {
              autoRepairStallStreak += 1;
            } else if (autoRepairProgressSignature) {
              lastAutoRepairSignature = autoRepairProgressSignature;
              autoRepairStallStreak = 0;
            } else {
              lastAutoRepairSignature = '';
              autoRepairStallStreak = 0;
            }
            autoRepairStalled = autoRepairStallStreak >= autoRepairStallThreshold;
          } else {
            lastAutoRepairSignature = '';
            autoRepairStallStreak = 0;
          }

          const canAutoRepairByRounds = autoRepairMaxRounds === 0 || autoRepairAttempt < autoRepairMaxRounds;
          if (
            deliveryStatusForMission === 'failed'
            && continuationPolicyForMission === 'ask'
            && canAutoRepairByRounds
            && !isGovernancePaused
          ) {
            const replanReason = this.buildReplanGateReason({
              source: 'delivery_failed',
              summary: deliverySummaryForMission,
              details: deliveryDetailsForMission,
            });
            if (this.currentPlanId) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  replan: { state: 'awaiting_confirmation', reason: replanReason },
                }, {
                  auditReason: 'replan-gate:delivery-failed:awaiting-confirmation',
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.replanGate回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  source: 'delivery_failed',
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            const sessionIdForConfirm = resolvedSessionId;
            const missionIdForConfirm = this.lastMissionId || this.currentTurnId || `mission-${Date.now()}`;
            const decision = await this.awaitDeliveryRepairConfirmation({
              sessionId: sessionIdForConfirm,
              missionId: missionIdForConfirm,
              summary: deliverySummaryForMission || '验收未通过',
              details: deliveryDetailsForMission,
              round: autoRepairAttempt + 1,
              maxRounds: autoRepairMaxRounds,
              requestType: 'delivery_repair',
            });
            continuationPolicyForMission = decision === 'repair' ? 'auto' : 'stop';
            continuationReasonForMission = decision === 'repair'
              ? '用户确认继续自动修复'
              : '用户确认停止自动修复';
            if (this.currentPlanId) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  replan: decision === 'repair'
                    ? { state: 'applied', reason: 'user_confirmed_replan' }
                    : { state: 'required', reason: replanReason },
                }, {
                  auditReason: `replan-gate:delivery-failed:${decision}`,
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.replanGate确认结果回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  decision,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
          }

          const replanGateSignals = this.collectReplanGateSignals({
            runtimeReason: normalizedRuntimeReason,
            runtimeSnapshot: orchestratorRuntimeSnapshot,
            auditOutcome,
          });
          const structuredRequiredTotal = this.resolveRequiredTotal(orchestratorRuntimeSnapshot) ?? 0;
          const hasStructuredExecutionContext = structuredRequiredTotal > 0;

          const deliveryRecoveryDecision = decideRecoveryAction({
            currentPlanMode,
            interactionMode: this.interactionMode,
            isGovernancePaused: isGovernancePaused === true,
            governanceReason: normalizedRuntimeReason,
            governanceRecoveryAttempt,
            governanceRecoveryMaxRounds,
            deliveryFailed: deliveryStatusForMission === 'failed',
            continuationPolicy: continuationPolicyForMission,
            canAutoRepairByRounds: canAutoRepairByRounds && hasStructuredExecutionContext,
            autoRepairStalled,
            hasFollowUpPending: false,
            followUpSignatureChanged: false,
            followUpStallStreak: 0,
            blockedFollowUpOnly: false,
            signals: replanGateSignals,
          });

          if (deliveryRecoveryDecision.action === 'auto_repair') {
            if (this.lastMissionId) {
              try {
                await this.missionStorage.transitionStatus(this.lastMissionId, 'executing');
              } catch (error) {
                logger.warn('编排器.Mission.自动修复前状态恢复失败', {
                  missionId: this.lastMissionId,
                  to: 'executing',
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            if (this.currentPlanId) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  review: { state: 'idle' },
                  replan: { state: 'applied', reason: 'auto_repair_triggered' },
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.自动修复状态回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            autoRepairAttempt += 1;
            const autoRepairPrompt = this.buildAutoRepairPrompt({
              originalPrompt: trimmedPrompt,
              goal: requirementAnalysis.goal,
              constraints: requirementAnalysis.constraints ?? [],
              acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
              deliverySummary: deliverySummaryForMission,
              deliveryDetails: deliveryDetailsForMission,
              round: autoRepairAttempt,
              maxRounds: autoRepairMaxRounds,
            });
            this.messageHub.notify(
              t('engine.delivery.autoRepairScheduled', { round: autoRepairAttempt, maxRounds: autoRepairMaxRoundsLabel }),
              'warning',
            );
            currentRoundRequestId = this.beginSyntheticExecutionRound({
              kind: 'auto_repair',
              round: autoRepairAttempt,
              message: t('engine.delivery.autoRepairProgressMessage', {
                round: autoRepairAttempt,
                maxRounds: autoRepairMaxRoundsLabel,
              }),
            });
            promptForRound = autoRepairPrompt;
            continue;
          }

          if (deliveryRecoveryDecision.action === 'auto_repair_stalled_notice') {
            const stalledMessage = t('engine.delivery.autoRepairStalled', {
              streak: autoRepairStallStreak,
              threshold: autoRepairStallThreshold,
            });
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${stalledMessage}`
              : stalledMessage;
            this.messageHub.result(stalledMessage, {
              metadata: { phase: 'system_section', extra: { type: 'auto_repair_stalled' } },
            });
          }

          const governanceRecoveryDecision = decideRecoveryAction({
            currentPlanMode,
            interactionMode: this.interactionMode,
            isGovernancePaused: isGovernancePaused === true,
            governanceReason: normalizedRuntimeReason,
            governanceRecoveryAttempt,
            governanceRecoveryMaxRounds,
            deliveryFailed: false,
            continuationPolicy: continuationPolicyForMission,
            canAutoRepairByRounds,
            autoRepairStalled,
            hasFollowUpPending: false,
            followUpSignatureChanged: false,
            followUpStallStreak: 0,
            blockedFollowUpOnly: false,
            signals: replanGateSignals,
          });
          if (governanceRecoveryDecision.action === 'auto_governance_resume') {
            governanceRecoveryAttempt += 1;
            const delayMs = governanceRecoveryDelays[governanceRecoveryAttempt - 1] ?? 0;
            const reasonLabel = this.formatGovernanceReason(normalizedRuntimeReason);
            const waitSeconds = Math.max(1, Math.round(delayMs / 1000));
            this.messageHub.notify(
              t('engine.governance.autoResumeScheduled', {
                round: governanceRecoveryAttempt,
                maxRounds: governanceRecoveryMaxRounds,
                seconds: waitSeconds,
                reason: reasonLabel,
              }),
              'warning',
            );
            if (delayMs > 0) {
              await new Promise((resolve) => setTimeout(resolve, delayMs));
            }
            currentRoundRequestId = this.beginSyntheticExecutionRound({
              kind: 'auto_governance_resume',
              round: governanceRecoveryAttempt,
              message: t('engine.governance.autoResumeScheduled', {
                round: governanceRecoveryAttempt,
                maxRounds: governanceRecoveryMaxRounds,
                seconds: waitSeconds,
                reason: reasonLabel,
              }),
            });
            promptForRound = this.buildGovernanceRecoveryPrompt({
              originalPrompt: trimmedPrompt,
              goal: requirementAnalysis.goal,
              constraints: requirementAnalysis.constraints ?? [],
              acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
              reason: normalizedRuntimeReason,
              round: governanceRecoveryAttempt,
              maxRounds: governanceRecoveryMaxRounds,
            });
            continue;
          }

          const resolvedFollowUpSteps = this.resolveFollowUpSteps(
            response.orchestratorRuntime?.nextSteps,
          );
          const {
            actionable: followUpSteps,
            blocked: blockedFollowUpSteps,
            nonActionable: nonActionableFollowUpSteps,
          } = this.classifyFollowUpSteps(resolvedFollowUpSteps);
          const currentPlanForFollowUp = this.currentPlanId
            ? this.planLedger.getPlan(resolvedSessionId, this.currentPlanId)
            : null;
          const persistedPhaseSteps = this.resolvePersistedPhaseFollowUpSteps(currentPlanForFollowUp?.runtime.phase);
          const effectiveFollowUpSteps = followUpSteps.length > 0
            ? followUpSteps
            : persistedPhaseSteps;
          const blockedFollowUpOnly = effectiveFollowUpSteps.length === 0 && blockedFollowUpSteps.length > 0;
          const pendingRequiredTodos = this.extractPendingRequiredCount(orchestratorRuntimeSnapshot);
          const requiredTotalTodos = this.resolveRequiredTotal(orchestratorRuntimeSnapshot) ?? 0;
          const hasStructuredRuntimeBacklog = requiredTotalTodos > 0;
          const followUpSignature = [
            `pending:${pendingRequiredTodos}`,
            `steps:${effectiveFollowUpSteps.join('|')}`,
          ].join('|');
          const followUpProgressSignature = this.buildFollowUpProgressSignature(orchestratorRuntimeSnapshot);
          if (followUpProgressSignature && followUpProgressSignature === lastFollowUpProgressSignature) {
            followUpStallStreak += 1;
          } else {
            lastFollowUpProgressSignature = followUpProgressSignature;
            followUpStallStreak = 0;
          }
          if (blockedFollowUpOnly) {
            const blockedNotice = this.buildFollowUpBlockedNotice(blockedFollowUpSteps);
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${blockedNotice}`
              : blockedNotice;
            this.messageHub.result(blockedNotice, {
              metadata: { phase: 'system_section', extra: { type: 'follow_up_blocked' } },
            });
          }

          if (nonActionableFollowUpSteps.length > 0) {
            logger.info('编排器.FollowUp.忽略非任务建议', {
              count: nonActionableFollowUpSteps.length,
              examples: nonActionableFollowUpSteps.slice(0, 3),
            }, LogCategory.ORCHESTRATOR);
          }
          if (effectiveFollowUpSteps.length > 0 && !hasStructuredRuntimeBacklog) {
            logger.info('编排器.FollowUp.忽略无任务上下文建议', {
              count: effectiveFollowUpSteps.length,
              requiredTotalTodos,
              runtimeReason: normalizedRuntimeReason,
            }, LogCategory.ORCHESTRATOR);
          }

          const allowStepDrivenFollowUp = this.shouldAllowStepDrivenFollowUp({
            runtimeReason: normalizedRuntimeReason,
            requiredTotalTodos,
            pendingRequiredTodos,
            hasStructuredRuntimeBacklog,
            followUpSteps: effectiveFollowUpSteps,
            phaseRuntime: currentPlanForFollowUp?.runtime.phase,
          });
          const hasFollowUpPending = pendingRequiredTodos > 0
            || (allowStepDrivenFollowUp && hasStructuredRuntimeBacklog && effectiveFollowUpSteps.length > 0);

          if (this.currentPlanId) {
            const phasePatch = this.buildPhaseRuntimePatch({
              current: currentPlanForFollowUp?.runtime.phase,
              runtimeReason: normalizedRuntimeReason,
              hasStructuredRuntimeBacklog,
              pendingRequiredTodos,
              followUpSteps: effectiveFollowUpSteps,
            });
            if (phasePatch) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  phase: phasePatch,
                }, {
                  auditReason: 'follow-up-phase-runtime-sync',
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.phase回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
          }

          const followUpProducedExecutionActivity = this.didFollowUpRoundProduceExecutionActivity(
            requestStatsBeforeRound,
            requestStatsAfterRound,
          );
          const followUpNoExecutionDetected = autoFollowUpAttempt > 0
            && hasFollowUpPending
            && !blockedFollowUpOnly
            && !followUpProducedExecutionActivity;

          if (followUpProducedExecutionActivity) {
            autoFollowUpNoExecutionRetry = 0;
          } else if (followUpNoExecutionDetected && autoFollowUpNoExecutionRetry < 1) {
            autoFollowUpNoExecutionRetry += 1;
            logger.warn('编排器.FollowUp.检测到口头续跑未实际执行', {
              round: autoFollowUpAttempt,
              retry: autoFollowUpNoExecutionRetry,
              pendingRequiredTodos,
              followUpSteps: effectiveFollowUpSteps.length,
            }, LogCategory.ORCHESTRATOR);
            promptForRound = this.buildAutoFollowUpPrompt({
              originalPrompt: trimmedPrompt,
              goal: requirementAnalysis.goal,
              constraints: requirementAnalysis.constraints ?? [],
              acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
              steps: effectiveFollowUpSteps,
              round: autoFollowUpAttempt,
              requiredTotal: this.resolveRequiredTotal(orchestratorRuntimeSnapshot),
              terminalRequired: this.resolveTerminalRequired(orchestratorRuntimeSnapshot),
              pendingRequired: pendingRequiredTodos,
              enforceExecution: true,
            });
            continue;
          }
          const runtimeRecoveryDecision = decideRecoveryAction({
            currentPlanMode,
            interactionMode: this.interactionMode,
            isGovernancePaused: isGovernancePaused === true,
            governanceReason: normalizedRuntimeReason,
            governanceRecoveryAttempt,
            governanceRecoveryMaxRounds,
            deliveryFailed: false,
            continuationPolicy: continuationPolicyForMission,
            canAutoRepairByRounds,
            autoRepairStalled,
            hasFollowUpPending,
            followUpSignatureChanged: followUpSignature !== lastFollowUpSignature,
            followUpStallStreak,
            blockedFollowUpOnly,
            signals: replanGateSignals,
          });

          if (runtimeRecoveryDecision.action === 'ask_followup_confirmation') {
            const currentPlanForGate = this.currentPlanId
              ? this.planLedger.getPlan(resolvedSessionId, this.currentPlanId)
              : null;
            const replanSource = (runtimeRecoveryDecision.replanSource || (hasFollowUpPending ? 'ask_followup_pending' : 'budget_pressure')) as ReplanSource;
            const followUpReplanReason = this.buildReplanGateReason({
              source: replanSource,
              reviewRound: currentPlanForGate?.runtime.review.round,
              pendingRequiredTodos,
              steps: effectiveFollowUpSteps,
              budgetElapsedMs: orchestratorRuntimeSnapshot?.budgetState?.elapsedMs,
              budgetTokenUsed: orchestratorRuntimeSnapshot?.budgetState?.tokenUsed,
              scopeIssues: replanGateSignals.scopeIssues,
              failedRequiredTodos: replanGateSignals.failedRequiredTodos,
              unresolvedBlockers: replanGateSignals.unresolvedBlockers,
              externalWaitOpen: replanGateSignals.externalWaitOpen,
            });
            if (this.currentPlanId) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  replan: { state: 'awaiting_confirmation', reason: followUpReplanReason },
                }, {
                  auditReason: `replan-gate:${replanSource}:awaiting-confirmation`,
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.ask续跑门禁回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  source: replanSource,
                  pendingRequiredTodos,
                  followUpSteps: effectiveFollowUpSteps.length,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            const followUpSummaryLines: string[] = [];
            if (effectiveFollowUpSteps.length > 0) {
              followUpSummaryLines.push(`检测到 ${effectiveFollowUpSteps.length} 项待继续执行工作`);
            }
            if (pendingRequiredTodos > 0) {
              followUpSummaryLines.push(`仍有 ${pendingRequiredTodos} 项关键任务未完成`);
            }
            if (replanGateSignals.budgetPressure) {
              followUpSummaryLines.push('已触发预算保护阈值');
            }
            if (replanGateSignals.scopeExpansion) {
              followUpSummaryLines.push(`任务范围出现扩张风险（${replanGateSignals.scopeIssues.length} 项）`);
            }
            if (replanGateSignals.acceptanceFailure) {
              followUpSummaryLines.push(`验收项仍有失败（${replanGateSignals.failedRequiredTodos} 项）`);
            }
            if (replanGateSignals.blockerPressure) {
              followUpSummaryLines.push(`存在阻塞风险（阻塞 ${replanGateSignals.unresolvedBlockers}，外部等待 ${replanGateSignals.externalWaitOpen}）`);
            }
            if (replanGateSignals.progressStalled) {
              followUpSummaryLines.push('执行进度出现停滞');
            }
            const followUpSummary = followUpSummaryLines.length > 0
              ? followUpSummaryLines.join('；')
              : '当前执行需要你确认是否继续';
            const followUpDetailsLines: string[] = [];
            if (effectiveFollowUpSteps.length > 0) {
              followUpDetailsLines.push('待继续执行项：');
              followUpDetailsLines.push(...effectiveFollowUpSteps.map((step) => `- ${step}`));
            }
            if (replanGateSignals.scopeIssues.length > 0) {
              followUpDetailsLines.push('范围风险明细：');
              followUpDetailsLines.push(...replanGateSignals.scopeIssues.slice(0, 8).map((detail) => `- ${detail}`));
            }
            if (replanGateSignals.acceptanceFailure) {
              followUpDetailsLines.push('验收风险明细：');
              followUpDetailsLines.push(`- 当前失败关键任务：${replanGateSignals.failedRequiredTodos}`);
            }
            if (replanGateSignals.blockerPressure) {
              followUpDetailsLines.push('阻塞风险明细：');
              followUpDetailsLines.push(`- 未解决阻塞：${replanGateSignals.unresolvedBlockers}`);
              followUpDetailsLines.push(`- 外部等待阻塞：${replanGateSignals.externalWaitOpen}`);
            }
            const followUpDetails = followUpDetailsLines.join('\n');
            const decision = await this.awaitDeliveryRepairConfirmation({
              sessionId: resolvedSessionId,
              missionId: this.lastMissionId || this.currentTurnId || `mission-${Date.now()}`,
              summary: followUpSummary,
              details: followUpDetails || undefined,
              round: (currentPlanForGate?.runtime.review.round || 0) + 1,
              maxRounds: 0,
              requestType: 'replan_followup',
            });
            if (decision === 'repair') {
              continuationPolicyForMission = 'auto';
              continuationReasonForMission = '用户确认继续执行';
              if (this.currentPlanId) {
                try {
                  await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                    replan: { state: 'applied', reason: `user_confirmed_replan:${replanSource}` },
                  }, {
                    auditReason: `replan-gate:${replanSource}:repair`,
                  });
                } catch (error) {
                  logger.warn('编排器.PlanRuntime.ask续跑确认回写失败', {
                    sessionId: resolvedSessionId,
                    planId: this.currentPlanId,
                    source: replanSource,
                    decision,
                    error: error instanceof Error ? error.message : String(error),
                  }, LogCategory.ORCHESTRATOR);
                }
              }
              if (this.lastMissionId) {
                try {
                  await this.missionStorage.transitionStatus(this.lastMissionId, 'executing');
                } catch (error) {
                  logger.warn('编排器.Mission.ask续跑前状态恢复失败', {
                    missionId: this.lastMissionId,
                    to: 'executing',
                    error: error instanceof Error ? error.message : String(error),
                  }, LogCategory.ORCHESTRATOR);
                }
              }
              if (hasFollowUpPending) {
                autoFollowUpAttempt += 1;
                lastFollowUpSignature = followUpSignature;
                await this.markPhaseRuntimeRunning({
                  sessionId: resolvedSessionId,
                  followUpSteps: effectiveFollowUpSteps,
                });
                currentRoundRequestId = this.beginSyntheticExecutionRound({
                  kind: 'auto_followup',
                  round: autoFollowUpAttempt,
                  message: t('engine.followUp.autoContinueMessage', { round: autoFollowUpAttempt }),
                });
                promptForRound = this.buildAutoFollowUpPrompt({
                  originalPrompt: trimmedPrompt,
                  goal: requirementAnalysis.goal,
                  constraints: requirementAnalysis.constraints ?? [],
                  acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
                  steps: effectiveFollowUpSteps,
                  round: autoFollowUpAttempt,
                  requiredTotal: this.resolveRequiredTotal(orchestratorRuntimeSnapshot),
                  terminalRequired: this.resolveTerminalRequired(orchestratorRuntimeSnapshot),
                  pendingRequired: pendingRequiredTodos,
                });
              } else if (isGovernancePaused && isGovernanceAutoRecoverReason(normalizedRuntimeReason)) {
                const resumeRound = governanceRecoveryAttempt + 1;
                governanceRecoveryAttempt = resumeRound;
                currentRoundRequestId = this.beginSyntheticExecutionRound({
                  kind: 'auto_governance_resume',
                  round: resumeRound,
                  message: t('engine.governance.autoResumeScheduled', {
                    round: resumeRound,
                    maxRounds: governanceRecoveryMaxRounds,
                    seconds: 0,
                    reason: this.formatGovernanceReason(normalizedRuntimeReason),
                  }),
                });
                promptForRound = this.buildGovernanceRecoveryPrompt({
                  originalPrompt: trimmedPrompt,
                  goal: requirementAnalysis.goal,
                  constraints: requirementAnalysis.constraints ?? [],
                  acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
                  reason: normalizedRuntimeReason,
                  round: resumeRound,
                  maxRounds: governanceRecoveryMaxRounds,
                });
              } else if (replanGateSignals.scopeExpansion) {
                const scopeGuardPrompt = [
                  trimmedPrompt,
                  '[System] 用户已确认继续执行，请先处理范围风险后再推进：',
                  ...replanGateSignals.scopeIssues.slice(0, 8).map((detail) => `- ${detail}`),
                ].join('\n');
                promptForRound = scopeGuardPrompt;
              } else {
                promptForRound = trimmedPrompt;
              }
              continue;
            }

            continuationPolicyForMission = 'stop';
            continuationReasonForMission = '用户确认停止继续执行';
            if (this.currentPlanId) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  replan: { state: 'required', reason: followUpReplanReason },
                }, {
                  auditReason: `replan-gate:${replanSource}:stop`,
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.ask续跑拒绝回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  source: replanSource,
                  decision,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            const followUpNote = '[System] 已按你的选择暂停继续执行。';
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${followUpNote}`
              : followUpNote;
            this.messageHub.result(followUpNote, {
              metadata: { phase: 'system_section', extra: { type: 'follow_up_pending' } },
            });
          }

          if (runtimeRecoveryDecision.action === 'auto_followup') {
            if (this.lastMissionId) {
              try {
                await this.missionStorage.transitionStatus(this.lastMissionId, 'executing');
              } catch (error) {
                logger.warn('编排器.Mission.自动续跑前状态恢复失败', {
                  missionId: this.lastMissionId,
                  to: 'executing',
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            autoFollowUpAttempt += 1;
            lastFollowUpSignature = followUpSignature;
            await this.markPhaseRuntimeRunning({
              sessionId: resolvedSessionId,
              followUpSteps: effectiveFollowUpSteps,
            });
            currentRoundRequestId = this.beginSyntheticExecutionRound({
              kind: 'auto_followup',
              round: autoFollowUpAttempt,
              message: t('engine.followUp.autoContinueMessage', { round: autoFollowUpAttempt }),
            });
            if (this.currentPlanId) {
              try {
                await this.planLedger.updateRuntimeState(resolvedSessionId, this.currentPlanId, {
                  replan: { state: 'applied', reason: 'auto_followup_triggered' },
                }, {
                  auditReason: 'replan-gate:auto-followup-applied',
                });
              } catch (error) {
                logger.warn('编排器.PlanRuntime.自动续跑状态回写失败', {
                  sessionId: resolvedSessionId,
                  planId: this.currentPlanId,
                  error: error instanceof Error ? error.message : String(error),
                }, LogCategory.ORCHESTRATOR);
              }
            }
            promptForRound = this.buildAutoFollowUpPrompt({
              originalPrompt: trimmedPrompt,
              goal: requirementAnalysis.goal,
              constraints: requirementAnalysis.constraints ?? [],
              acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
              steps: effectiveFollowUpSteps,
              round: autoFollowUpAttempt,
              requiredTotal: this.resolveRequiredTotal(orchestratorRuntimeSnapshot),
              terminalRequired: this.resolveTerminalRequired(orchestratorRuntimeSnapshot),
              pendingRequired: pendingRequiredTodos,
            });
            continue;
          }

          if (runtimeRecoveryDecision.action === 'pause') {
            const pauseReport = this.buildGovernancePauseReport({
              reason: normalizedRuntimeReason,
              snapshot: orchestratorRuntimeSnapshot,
              recoveryAttempted: governanceRecoveryAttempt,
              recoveryMaxRounds: governanceRecoveryMaxRounds,
            });
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${pauseReport}`
              : pauseReport;
            this.messageHub.result(pauseReport, {
              metadata: { phase: 'system_section', extra: { type: 'governance_pause' } },
            });
          }

          if (autoRepairAttempt > 0) {
            const historySection = `[System] 已自动修复 ${autoRepairAttempt} 轮（详细记录见运行态诊断）。`;
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${historySection}`
              : historySection;
            this.messageHub.result(historySection, {
              metadata: { phase: 'system_section', extra: { type: 'auto_repair_history' } },
            });
          }

          if (autoFollowUpAttempt > 0) {
            const historySection = `[System] 已自动续跑 ${autoFollowUpAttempt} 轮（详细记录见运行态诊断）。`;
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${historySection}`
              : historySection;
            this.messageHub.result(historySection, {
              metadata: { phase: 'system_section', extra: { type: 'auto_follow_up_history' } },
            });
          }

          this.lastExecutionRuntimeReason = orchestratorRuntimeReason;
          this.lastExecutionFinalStatus = finalExecutionStatus;
          this.lastExecutionErrors = finalExecutionStatus === 'failed'
            ? this.buildExecutionFailureMessages(orchestratorRuntimeReason, executionWarnings)
            : [];
          this.setState('idle');
          this.currentTaskId = null;
          return finalContent;
        }

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
        logger.error('编排器.统一执行.失败', { error: errorMessage }, LogCategory.ORCHESTRATOR);
        this.setState('idle');
        this.currentTaskId = null;
        // fail-open：执行异常也返回可读结论，避免打断会话链路
        const degradedMessage = `[System] 本轮执行出现异常，已自动降级为不中断返回：${errorMessage}`;
        this.messageHub.result(degradedMessage, {
          metadata: { phase: 'system_section', extra: { type: 'execution_degraded' } },
        });
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
        const pendingConfirmation = this.pendingPlanConfirmation as {
          sessionId: string;
          planId: string;
          resolve: (confirmed: boolean) => void;
        } | null;
        if (pendingConfirmation) {
          pendingConfirmation.resolve(false);
          this.pendingPlanConfirmation = null;
        }
        const pendingDeliveryConfirmation = this.pendingDeliveryRepairConfirmation as {
          missionId: string;
          resolve: (decision: 'repair' | 'stop') => void;
        } | null;
        if (pendingDeliveryConfirmation) {
          pendingDeliveryConfirmation.resolve('stop');
          this.pendingDeliveryRepairConfirmation = null;
        }
        this.isRunning = false;
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

      if (resumeMissionId) {
        const mission = await this.missionStorage.load(resumeMissionId);
        this.resumeMissionId = null;
        if (!mission) {
          throw new Error(t('engine.errors.taskNotFound', { taskId: resumeMissionId }));
        }
        if (mission.status !== 'paused') {
          throw new Error(t('engine.errors.taskNotPaused', { taskId: resumeMissionId }));
        }
        await this.missionStorage.transitionStatus(mission.id, 'executing');
        this.lastMissionId = mission.id;
        this.missionOrchestrator.setCurrentMissionId(mission.id);

        if (this.currentPlanId) {
          const planForBinding = this.planLedger.getPlan(sessionId, this.currentPlanId);
          if (!planForBinding) {
            throw new Error(`恢复任务 ${mission.id} 时找不到计划 ${this.currentPlanId}`);
          }
          this.requirePlanMutation(
            await this.planLedger.bindMission(sessionId, this.currentPlanId, mission.id, {
              expectedRevision: planForBinding.revision,
              auditReason: 'ensure-mission:bind-resume-mission',
            }),
            {
              op: 'bind-resume-mission',
              sessionId,
              planId: this.currentPlanId,
              missionId: mission.id,
            },
          );
        }

        const orchestratorToolManager = this.adapterFactory.getToolManager();
        const orchestratorAssignmentId = `orchestrator-${mission.id}`;
        const normalizedSessionId = sessionId.trim();
        orchestratorToolManager.setSnapshotContext({
          sessionId: normalizedSessionId,
          missionId: mission.id,
          assignmentId: orchestratorAssignmentId,
          todoId: orchestratorAssignmentId,
          workerId: 'orchestrator',
        });

        return mission.id;
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
        continuationPolicy: this.interactionMode === 'ask' ? 'ask' : 'auto',
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
      runtimeReason: this.lastExecutionRuntimeReason,
      finalStatus: this.lastExecutionFinalStatus,
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

  private buildRequirementAnalysis(
    prompt: string,
    mode: PlanMode,
  ): RequirementAnalysis {
    const goal = extractPrimaryIntent(prompt) || prompt.trim();
    const constraints = extractUserConstraints(prompt);
    const acceptanceCriteria = this.extractPreDraftAcceptanceCriteria(prompt, goal, constraints);
    const riskAssessment = this.assessPreDraftRisk(prompt, mode, constraints, acceptanceCriteria);
    const executionIntent = this.analyzeRequirementExecutionIntent(prompt, mode);
    const analysisParts = [
      `围绕“${goal}”建立执行计划`,
      constraints.length > 0 ? `需遵守 ${constraints.length} 条用户约束` : '当前未识别出额外用户约束',
      acceptanceCriteria.length > 0 ? `验收以 ${acceptanceCriteria.length} 条标准为准` : '验收标准待后续调度补充',
      `风险等级为 ${riskAssessment.riskLevel}`,
    ];

    return {
      goal,
      analysis: analysisParts.join('；'),
      constraints,
      acceptanceCriteria,
      riskLevel: riskAssessment.riskLevel,
      riskFactors: riskAssessment.riskFactors,
      needsWorker: executionIntent.needsWorker,
      needsTooling: executionIntent.needsTooling,
      requiresModification: executionIntent.requiresModification,
      reason: executionIntent.reason,
    };
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
    steps?: string[];
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

    if (Array.isArray(input.steps) && input.steps.length > 0) {
      reasons.push(`next_steps=${input.steps.length}`);
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

  private collectReplanGateSignals(input: {
    runtimeReason?: ResolvedOrchestratorTerminationReason;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
    auditOutcome?: {
      issues?: Array<{ dimension?: string; detail?: string; level?: string }>;
    } | null;
  }): ReplanGateSignals {
    return deriveReplanGateSignals({
      runtimeReason: this.normalizeOrchestratorRuntimeReason(input.runtimeReason),
      runtimeSnapshot: input.runtimeSnapshot,
      auditOutcome: input.auditOutcome,
    });
  }

  private mergeAcceptanceCriteriaWithSpecResults(input: {
    criteria?: AcceptanceCriterion[] | null;
    specResults?: Array<{ criterionId: string; passed: boolean; detail: string }>;
    reviewRound: number;
    batchId?: string;
    workers?: WorkerSlot[];
  }): AcceptanceCriterion[] {
    const baseCriteria = Array.isArray(input.criteria) ? input.criteria : [];
    if (baseCriteria.length === 0) {
      return [];
    }

    const specById = new Map<string, { passed: boolean; detail: string }>();
    for (const result of input.specResults || []) {
      const criterionId = typeof result?.criterionId === 'string' ? result.criterionId.trim() : '';
      if (!criterionId) {
        continue;
      }
      specById.set(criterionId, {
        passed: result.passed === true,
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
      const specResult = specById.get(copied.id);
      if (!specResult) {
        return copied;
      }

      copied.status = specResult.passed ? 'passed' : 'failed';
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
      if (specResult.detail) {
        evidence.add(specResult.detail);
      }
      copied.evidence = evidence.size > 0 ? Array.from(evidence) : undefined;

      const historyEntry = {
        status: copied.status,
        reviewer: 'system:spec-verifier',
        detail: specResult.detail || undefined,
        reviewedAt,
        round: input.reviewRound,
        batchId: input.batchId,
        workerId: singleWorker,
      } as const;
      copied.reviewHistory = [...(copied.reviewHistory || []), historyEntry];

      return copied;
    });
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

  private shouldAllowStepDrivenFollowUp(input: {
    runtimeReason?: ResolvedOrchestratorTerminationReason;
    requiredTotalTodos: number;
    pendingRequiredTodos: number;
    hasStructuredRuntimeBacklog: boolean;
    followUpSteps: string[];
    phaseRuntime?: PlanRuntimePhaseState;
  }): boolean {
    if (input.runtimeReason !== 'completed') {
      return true;
    }
    // “completed” 只代表当前阶段已收口，不应直接等同 mission 完成。
    // 只要当前 mission 已建立结构化 backlog，且本轮留下了明确可执行的后续步骤，
    // deep 模式就应允许继续推进下一阶段，而不是把阶段切换错误地暴露成用户确认。
    if (!input.hasStructuredRuntimeBacklog) {
      return input.phaseRuntime?.continuationIntent === 'continue'
        && input.phaseRuntime.remainingPhases.length > 0;
    }
    if (input.followUpSteps.length === 0) {
      return input.phaseRuntime?.continuationIntent === 'continue'
        && input.phaseRuntime.remainingPhases.length > 0;
    }
    if (input.pendingRequiredTodos > 0) {
      return true;
    }
    logger.info('编排器.FollowUp.阶段完成但Mission未完成', {
      requiredTotalTodos: input.requiredTotalTodos,
      followUpSteps: input.followUpSteps.length,
    }, LogCategory.ORCHESTRATOR);
    return true;
  }

  private resolvePersistedPhaseFollowUpSteps(phase?: PlanRuntimePhaseState | null): string[] {
    if (!phase || phase.continuationIntent !== 'continue' || phase.state !== 'awaiting_next_phase') {
      return [];
    }
    return Array.isArray(phase.remainingPhases)
      ? phase.remainingPhases.filter((item) => typeof item === 'string' && item.trim().length > 0)
      : [];
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
    hasStructuredRuntimeBacklog: boolean;
    pendingRequiredTodos: number;
    followUpSteps: string[];
  }): Partial<PlanRuntimePhaseState> | null {
    const current = input.current;
    const steps = input.followUpSteps.filter((item) => item.trim().length > 0);
    if (steps.length > 0 && input.hasStructuredRuntimeBacklog) {
      const descriptors = steps.map((step) => this.extractPhaseDescriptor(step));
      const first = descriptors[0];
      return {
        state: input.pendingRequiredTodos > 0 ? 'running' : 'awaiting_next_phase',
        currentIndex: current?.currentIndex,
        currentTitle: current?.currentTitle,
        nextIndex: first?.index,
        nextTitle: first?.title,
        remainingPhases: steps,
        continuationIntent: 'continue',
      };
    }
    if (input.pendingRequiredTodos > 0) {
      return {
        state: 'running',
        continuationIntent: current?.continuationIntent === 'continue' ? 'continue' : 'stop',
        remainingPhases: current?.continuationIntent === 'continue' ? current.remainingPhases : [],
        nextIndex: current?.continuationIntent === 'continue' ? current.nextIndex : undefined,
        nextTitle: current?.continuationIntent === 'continue' ? current.nextTitle : undefined,
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
    const activeStep = steps[0] || currentPlan?.runtime.phase.nextTitle;
    const descriptor = activeStep ? this.extractPhaseDescriptor(activeStep) : null;
    try {
      await this.planLedger.updateRuntimeState(input.sessionId, this.currentPlanId, {
        phase: {
          state: 'running',
          currentIndex: descriptor?.index ?? currentPlan?.runtime.phase.nextIndex ?? currentPlan?.runtime.phase.currentIndex,
          currentTitle: descriptor?.title ?? currentPlan?.runtime.phase.nextTitle ?? currentPlan?.runtime.phase.currentTitle,
          nextIndex: currentPlan?.runtime.phase.nextIndex,
          nextTitle: currentPlan?.runtime.phase.nextTitle,
          remainingPhases: steps.length > 0 ? steps : (currentPlan?.runtime.phase.remainingPhases || []),
          continuationIntent: (steps.length > 0 || (currentPlan?.runtime.phase.remainingPhases.length || 0) > 0)
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
    enforceExecution?: boolean;
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
    const executionDirectives = input.enforceExecution
      ? [
          '这是执行轮，不是规划轮。',
          '本轮必须直接推进执行：若步骤涉及派发、修复、验证或复审，必须立刻调用对应工具或 worker_dispatch / worker_wait 落地。',
          '禁止只输出“现在启动”“准备派发”“已确认结构”“派发修复：”这类口头承诺；如果无法执行，必须明确写出阻断原因。',
        ].join('\n')
      : [
          '这是执行轮，不是规划轮。',
          '请直接执行上述步骤，必要时调用工具或 worker_dispatch / worker_wait 继续推进，不要重复总结或只复述阶段计划。',
        ].join('\n');

    return [
      '[System] 你上一轮给出了下一步建议，已进入自动续跑。',
      `续跑轮次：${input.round}`,
      `原始目标：${goal}`,
      `用户原始请求：${input.originalPrompt}`,
      `约束：\n${constraints}`,
      `验收标准：\n${acceptance}`,
      requiredSummary ? `必需 Todo 进度：\n${requiredSummary}` : '',
      `下一步建议：\n${steps}`,
      `执行要求：\n${executionDirectives}`,
      '若确实无法执行，请明确说明原因并输出最终结论。',
    ].filter(line => line && line.trim().length > 0).join('\n\n');
  }

  private beginSyntheticExecutionRound(input: {
    kind: 'auto_followup' | 'auto_repair' | 'auto_governance_resume';
    round: number;
    message: string;
  }): string {
    const requestId = `req_${input.kind}_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    this.messageHub.beginSyntheticRound(requestId, input.message, {
      phase: 'synthetic_execution_round',
      extra: {
        type: input.kind,
        round: input.round,
        syntheticRequest: true,
      },
    });
    return requestId;
  }

  private captureRequestMessageSummary(requestId?: string): RequestMessageSummary | undefined {
    if (!requestId || !requestId.trim()) {
      return undefined;
    }
    const summary = this.messageHub.getRequestMessageStats(requestId.trim());
    return summary ? { ...summary } : undefined;
  }

  private didFollowUpRoundProduceExecutionActivity(
    before?: RequestMessageSummary,
    after?: RequestMessageSummary,
  ): boolean {
    const beforeDispatch = before?.assistantDispatchContent || 0;
    const beforeWorker = before?.assistantWorkerContent || 0;
    const afterDispatch = after?.assistantDispatchContent || 0;
    const afterWorker = after?.assistantWorkerContent || 0;
    return afterDispatch > beforeDispatch || afterWorker > beforeWorker;
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

  private extractPreDraftAcceptanceCriteria(
    prompt: string,
    goal: string,
    constraints: string[],
  ): string[] {
    const constraintSet = new Set(constraints);
    const segments = prompt
      .split(/\n+/)
      .flatMap(line => line.split(/[。！？；;]+/))
      .map(segment => segment.trim())
      .filter(segment => segment.length > 0);
    const matched = Array.from(new Set(
      segments
        .filter(segment => /(?:验收|完成标准|成功标准|acceptance|验证|确保|通过|输出|结果)/i.test(segment))
        .filter(segment => !constraintSet.has(segment))
        .map(segment => (segment.length > 120 ? `${segment.substring(0, 120)}...` : segment))
    )).slice(0, 5);

    if (matched.length > 0) {
      return matched;
    }

    return goal ? [`完成目标：${goal}`] : [];
  }

  private analyzeRequirementExecutionIntent(
    prompt: string,
    mode: PlanMode,
  ): {
    hasReadOnlyIntent: boolean;
    hasWriteIntent: boolean;
    hasHighImpactIntent: boolean;
    needsWorker: boolean;
    needsTooling: boolean;
    requiresModification: boolean;
    reason: string;
  } {
    const normalizedPrompt = prompt.toLowerCase();
    const readOnlyKeywords = ['分析', '解释', '总结', '查看', '审查', 'review', 'summarize', 'read only'];
    const writeKeywords = ['修改', '实现', '修复', '新增', '重构', '删除', '更新', '编写', 'patch'];
    const highImpactKeywords = [
      '架构',
      '迁移',
      '并发',
      'schema',
      'ledger',
      '状态机',
      '依赖',
      '数据库',
      '权限',
      '认证',
      '安全',
      'deploy',
      '生产',
    ];
    const hasReadOnlyIntent = readOnlyKeywords.some(keyword => normalizedPrompt.includes(keyword));
    const hasWriteIntent = writeKeywords.some(keyword => normalizedPrompt.includes(keyword));
    const hasHighImpactIntent = highImpactKeywords.some(keyword => normalizedPrompt.includes(keyword));
    const requiresModification = hasWriteIntent || hasHighImpactIntent;
    const needsWorker = mode === 'deep' || requiresModification || !hasReadOnlyIntent;
    const needsTooling = mode === 'deep' || requiresModification;

    return {
      hasReadOnlyIntent,
      hasWriteIntent,
      hasHighImpactIntent,
      needsWorker,
      needsTooling,
      requiresModification,
      reason: needsWorker
        ? '需求已进入结构化编排主链，需要继续完成计划生成与后续调度决策'
        : '需求以只读分析为主，当前可优先保留编排器直接响应路径',
    };
  }

  private assessPreDraftRisk(
    prompt: string,
    mode: PlanMode,
    constraints: string[],
    acceptanceCriteria: string[],
  ): { riskLevel: 'low' | 'medium' | 'high'; riskFactors: string[] } {
    const executionIntent = this.analyzeRequirementExecutionIntent(prompt, mode);
    const riskFactors: string[] = [];
    let score = 0;

    if (mode === 'deep') {
      score += 2;
      riskFactors.push('任务运行在 deep 模式');
    }
    if (executionIntent.hasHighImpactIntent) {
      score += 2;
      riskFactors.push('需求涉及高影响改动');
    } else if (executionIntent.hasWriteIntent) {
      score += 1;
      riskFactors.push('需求包含代码修改');
    }
    if (prompt.length >= 280) {
      score += 1;
      riskFactors.push('需求描述较长');
    }
    if (constraints.length >= 3) {
      score += 1;
      riskFactors.push('用户约束较多');
    }
    if (acceptanceCriteria.length >= 3) {
      score += 1;
      riskFactors.push('验收标准较多');
    }
    if (executionIntent.hasReadOnlyIntent && !executionIntent.hasWriteIntent && !executionIntent.hasHighImpactIntent) {
      score = Math.max(0, score - 2);
    }

    const riskLevel = score >= 4 ? 'high' : score >= 2 ? 'medium' : 'low';
    return {
      riskLevel,
      riskFactors: Array.from(new Set(riskFactors)),
    };
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

      activeBatch.cancelAll(t('engine.cancel.userCancelled'));

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
      throw new Error(t('engine.errors.taskNotFound', { taskId }));
    }
    if (!mission.userPrompt?.trim()) {
      throw new Error(t('engine.errors.taskMissingPrompt', { taskId }));
    }
    const { userPrompt, sessionId } = mission;
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
