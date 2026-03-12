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
  Mission,
  MissionStorageManager,
  FileBasedMissionStorage,
  MissionDeliveryStatus,
  MissionContinuationPolicy,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { MessageHub } from './message-hub';
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
import { PlanLedgerService, type PlanMode, type PlanRecord } from '../plan-ledger';
import {
  createWisdomStorage,
  extractPrimaryIntent,
  extractUserConstraints,
  isKeyInstruction,
  resolveOrchestratorContextPolicy,
} from './mission-driven-engine-helpers';
import { normalizeNextSteps } from '../../utils/content-parser';

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
  private lastExecutionFinalStatus: 'completed' | 'failed' | 'cancelled' = 'completed';

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
    finalPlanStatus: 'completed' | 'failed' | 'cancelled' | null;
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
  }): Promise<'repair' | 'stop'> {
    this.alignMessageHubTrace(input.sessionId);
    this.messageHub.data('deliveryRepairRequest', {
      sessionId: input.sessionId,
      missionId: input.missionId,
      summary: input.summary,
      details: input.details,
      round: input.round,
      maxRounds: input.maxRounds,
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
  ): 'completed' | 'failed' | 'cancelled' {
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

    return 'failed';
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
      void (async () => {
        await this.planLedger.startAttempt(this.currentSessionId!, this.currentPlanId!, {
          scope: 'assignment',
          targetId: assignmentId,
          assignmentId,
          reason: 'assignment-started',
        });
        await this.planLedger.updateAssignmentStatus(this.currentSessionId!, this.currentPlanId!, assignmentId, 'running');
      })().catch((error) => this.reportPlanLedgerAsyncError('assignmentStarted', error));
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
      const success = Boolean((data as { success?: unknown }).success);
      const failureMessage = success ? undefined : this.extractAssignmentFailureMessage(data);
      const attemptStatus = this.classifyAttemptTerminalStatus(success, failureMessage);
      const assignmentStatus = this.mapAttemptToAssignmentStatus(attemptStatus);
      void (async () => {
        await this.planLedger.completeLatestAttempt(this.currentSessionId!, this.currentPlanId!, {
          scope: 'assignment',
          targetId: assignmentId,
          assignmentId,
          status: attemptStatus,
          reason: success ? 'assignment-completed' : 'assignment-failed',
          error: failureMessage,
        });
        await this.planLedger.updateAssignmentStatus(
          this.currentSessionId!,
          this.currentPlanId!,
          assignmentId,
          assignmentStatus,
        );
      })().catch((error) => this.reportPlanLedgerAsyncError('assignmentCompleted', error));
    });

    this.missionOrchestrator.on('todoStarted', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      const todoId = typeof data.todoId === 'string' ? data.todoId.trim() : '';
      if (!assignmentId || !todoId) {
        logger.warn('编排器.计划账本.todoStarted.缺少关键字段', {
          assignmentId: data.assignmentId,
          todoId: data.todoId,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void (async () => {
        await this.planLedger.startAttempt(this.currentSessionId!, this.currentPlanId!, {
          scope: 'todo',
          targetId: todoId,
          assignmentId,
          todoId,
          reason: 'todo-started',
        });
        await this.planLedger.updateTodoStatus(
          this.currentSessionId!,
          this.currentPlanId!,
          assignmentId,
          todoId,
          'running',
        );
      })().catch((error) => this.reportPlanLedgerAsyncError('todoStarted', error));
    });

    this.missionOrchestrator.on('todoCompleted', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
      const assignmentId = typeof data.assignmentId === 'string' ? data.assignmentId.trim() : '';
      const todoId = typeof data.todoId === 'string' ? data.todoId.trim() : '';
      if (!assignmentId || !todoId) {
        logger.warn('编排器.计划账本.todoCompleted.缺少关键字段', {
          assignmentId: data.assignmentId,
          todoId: data.todoId,
        }, LogCategory.ORCHESTRATOR);
        return;
      }
      void (async () => {
        await this.planLedger.completeLatestAttempt(this.currentSessionId!, this.currentPlanId!, {
          scope: 'todo',
          targetId: todoId,
          assignmentId,
          todoId,
          status: 'succeeded',
          reason: 'todo-completed',
        });
        await this.planLedger.updateTodoStatus(
          this.currentSessionId!,
          this.currentPlanId!,
          assignmentId,
          todoId,
          'completed',
        );
      })().catch((error) => this.reportPlanLedgerAsyncError('todoCompleted', error));
    });

    this.missionOrchestrator.on('todoFailed', (data) => {
      if (!this.currentPlanId || !this.currentSessionId) {
        return;
      }
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
      void (async () => {
        await this.planLedger.completeLatestAttempt(this.currentSessionId!, this.currentPlanId!, {
          scope: 'todo',
          targetId: todoId,
          assignmentId,
          todoId,
          status: attemptStatus,
          reason: 'todo-failed',
          error: errorMessage || undefined,
        });
        await this.planLedger.updateTodoStatus(
          this.currentSessionId!,
          this.currentPlanId!,
          assignmentId,
          todoId,
          todoStatus,
        );
      })().catch((error) => this.reportPlanLedgerAsyncError('todoFailed', error));
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
        return t('engine.input.emptyPrompt');
      }

      this.isRunning = true;
      this.currentTaskId = taskId || null;
      this.lastMissionId = null;
      this.currentPlanId = null;
      this.currentRequirementAnalysis = null;
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

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        this.currentSessionId = resolvedSessionId;
        this.alignMessageHubTrace(resolvedSessionId);
        this.dispatchManager.resetForNewExecutionCycle();
        await this.ensureContextReady(resolvedSessionId);

        const planMode: PlanMode = this.adapterFactory.isDeepTask() ? 'deep' : 'standard';
        currentPlanMode = planMode;
        const requirementAnalysis = this.buildRequirementAnalysis(trimmedPrompt, planMode);
        this.currentRequirementAnalysis = requirementAnalysis;
        const draftPlan = await this.planLedger.createDraft({
          sessionId: resolvedSessionId,
          turnId: this.currentTurnId || `turn:${Date.now()}`,
          missionId: this.currentTurnId || undefined,
          mode: planMode,
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
        const shouldAskConfirmation = this.interactionMode === 'ask' && planMode === 'deep';
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
            await this.planLedger.reject(resolvedSessionId, draftPlan.planId, 'user', rejectReason);
            orchestratorRuntimeReason = 'cancelled';
            this.lastExecutionRuntimeReason = orchestratorRuntimeReason;
            this.lastExecutionFinalStatus = 'cancelled';
            this.lastExecutionErrors = [];
            this.setState('idle');
            this.currentTaskId = null;
            return t('engine.plan.userCancelledResult');
          }
          await this.planLedger.approve(
            resolvedSessionId,
            draftPlan.planId,
            'user',
            `governance:manual;risk=${governanceAssessment.riskScore.toFixed(3)};confidence=${governanceAssessment.confidence.toFixed(3)};reasons=${governanceAssessment.reasons.join('|') || 'none'}`,
          );
          this.setState('running');
        } else {
          await this.planLedger.approve(
            resolvedSessionId,
            draftPlan.planId,
            'system:auto',
            `governance:auto;decision=${governanceAssessment.decision};risk=${governanceAssessment.riskScore.toFixed(3)};confidence=${governanceAssessment.confidence.toFixed(3)};reasons=${governanceAssessment.reasons.join('|') || 'none'}`,
          );
        }
        await this.planLedger.markExecuting(resolvedSessionId, draftPlan.planId);
        orchestratorAttemptTargetId = this.currentTurnId || `orchestrator-${Date.now()}`;
        await this.planLedger.startAttempt(resolvedSessionId, draftPlan.planId, {
          scope: 'orchestrator',
          targetId: orchestratorAttemptTargetId,
          reason: 'orchestrator-execution-started',
        });
        orchestratorAttemptStarted = true;

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
        // Mission 尚未创建前，临时使用 turnId 作为 mission 作用域占位；
        // 一旦进入 dispatch 流并拿到真实 mission.id，会在 ensureMissionForDispatch 中覆盖。
        const orchestratorToolManager = this.adapterFactory.getToolManager();
        const orchestratorAssignmentId = `orchestrator-${this.currentTurnId}`;
        const normalizedSessionId = resolvedSessionId.trim();
        orchestratorToolManager.setSnapshotContext({
          sessionId: normalizedSessionId,
          missionId: this.lastMissionId || this.currentTurnId!,
          assignmentId: orchestratorAssignmentId,
          todoId: orchestratorAssignmentId,
          workerId: 'orchestrator',
        });

        // 编排者跨轮会话记忆保留在 adapter history 中，不再每轮清空。
        // SystemPrompt 侧通过 prepareContext 动态裁剪 recent_turns，避免双重注入和 token 膨胀。

        const autoRepairMaxRounds = Math.max(0, this.config.delivery?.autoRepairMaxRounds ?? 1);
        let autoRepairAttempt = 0;
        let autoFollowUpAttempt = 0;
        let lastFollowUpSignature = '';
        let lastFollowUpProgressSignature = '';
        let followUpStallStreak = 0;
        const autoRepairHistory: string[] = [];
        const autoFollowUpHistory: string[] = [];
        let promptForRound = trimmedPrompt;

        // 5. 编排执行（支持交付失败后的自动修复闭环）
        while (true) {
          const executionRound = autoRepairAttempt + 1;
          if (autoRepairAttempt > 0 || autoFollowUpAttempt > 0) {
            this.dispatchManager.resetForNewExecutionCycle();
            if (autoRepairAttempt > 0) {
              this.messageHub.progress(
                t('engine.delivery.autoRepairProgressTitle'),
                t('engine.delivery.autoRepairProgressMessage', { round: autoRepairAttempt, maxRounds: autoRepairMaxRounds }),
              );
            } else {
              this.messageHub.progress(
                t('engine.followUp.autoContinueTitle'),
                t('engine.followUp.autoContinueMessage', { round: autoFollowUpAttempt }),
              );
            }
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
            }
          );
          orchestratorRuntimeReason = this.normalizeOrchestratorRuntimeReason(response.orchestratorRuntime?.reason);
          orchestratorRuntimeRounds = response.orchestratorRuntime?.rounds || 0;
          orchestratorRuntimeSnapshot = response.orchestratorRuntime?.snapshot as RuntimeTerminationSnapshot | undefined;
          orchestratorRuntimeShadow = response.orchestratorRuntime?.shadow as RuntimeTerminationShadow | undefined;
          orchestratorRuntimeDecisionTrace =
            response.orchestratorRuntime?.decisionTrace as RuntimeTerminationDecisionTraceEntry[] | undefined;
          runtimeTokenUsage = response.tokenUsage;

          this.recordOrchestratorTokens(response.tokenUsage);

          // 等待 dispatch batch 归档（含 Worker 执行 + Phase C 汇总）
          // dispatch_task 是非阻塞的，sendMessage 返回时 Worker 可能还在后台执行。
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
            let outcome: DeliveryVerificationOutcome;
            try {
              outcome = await runPostDispatchVerification(currentBatch, this.verificationRunner, this.messageHub);
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

          if (deliverySummaryForMission) {
            autoRepairHistory.push(`第${executionRound}轮：${deliverySummaryForMission}`);
          }

          if (
            deliveryStatusForMission === 'failed'
            && continuationPolicyForMission === 'ask'
            && autoRepairAttempt < autoRepairMaxRounds
          ) {
            const sessionIdForConfirm = resolvedSessionId;
            const missionIdForConfirm = this.lastMissionId || this.currentTurnId || `mission-${Date.now()}`;
            const decision = await this.awaitDeliveryRepairConfirmation({
              sessionId: sessionIdForConfirm,
              missionId: missionIdForConfirm,
              summary: deliverySummaryForMission || '验收未通过',
              details: deliveryDetailsForMission,
              round: autoRepairAttempt + 1,
              maxRounds: autoRepairMaxRounds,
            });
            continuationPolicyForMission = decision === 'repair' ? 'auto' : 'stop';
            continuationReasonForMission = decision === 'repair'
              ? '用户确认继续自动修复'
              : '用户确认停止自动修复';
          }

          const shouldAutoRepair = deliveryStatusForMission === 'failed'
            && continuationPolicyForMission === 'auto'
            && autoRepairAttempt < autoRepairMaxRounds;

          if (shouldAutoRepair) {
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
              t('engine.delivery.autoRepairScheduled', { round: autoRepairAttempt, maxRounds: autoRepairMaxRounds }),
              'warning',
            );
            promptForRound = autoRepairPrompt;
            continue;
          }

          const followUpSteps = this.resolveFollowUpSteps(response.orchestratorRuntime?.nextSteps);
          const pendingRequiredTodos = this.extractPendingRequiredCount(orchestratorRuntimeSnapshot);
          const followUpSignature = [
            `pending:${pendingRequiredTodos}`,
            `steps:${followUpSteps.join('|')}`,
          ].join('|');
          const followUpProgressSignature = this.buildFollowUpProgressSignature(orchestratorRuntimeSnapshot);
          if (followUpProgressSignature && followUpProgressSignature === lastFollowUpProgressSignature) {
            followUpStallStreak += 1;
          } else {
            lastFollowUpProgressSignature = followUpProgressSignature;
            followUpStallStreak = 0;
          }
          const shouldAutoFollowUp = (followUpSteps.length > 0 || pendingRequiredTodos > 0)
            && currentPlanMode === 'deep'
            && this.interactionMode === 'auto'
            && followUpSignature !== lastFollowUpSignature
            && followUpStallStreak < 2;

          if (!shouldAutoFollowUp
            && currentPlanMode === 'deep'
            && this.interactionMode === 'ask'
            && (followUpSteps.length > 0 || pendingRequiredTodos > 0)) {
            const followUpNote = followUpSteps.length > 0
              ? '[System] 当前为 ask 模式，检测到下一步建议。请确认是否继续执行。'
              : '[System] 当前为 ask 模式，仍有未完成必需 Todo。请确认是否继续执行。';
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${followUpNote}`
              : followUpNote;
            this.messageHub.result(followUpNote, {
              metadata: { phase: 'system_section', extra: { type: 'follow_up_pending' } },
            });
          }

          if (shouldAutoFollowUp) {
            autoFollowUpAttempt += 1;
            lastFollowUpSignature = followUpSignature;
            autoFollowUpHistory.push(`第${autoFollowUpAttempt}轮：${followUpSteps.join('；')}`);
            this.messageHub.notify(
              t('engine.followUp.autoContinueScheduled', { round: autoFollowUpAttempt }),
              'warning',
            );
            promptForRound = this.buildAutoFollowUpPrompt({
              originalPrompt: trimmedPrompt,
              goal: requirementAnalysis.goal,
              constraints: requirementAnalysis.constraints ?? [],
              acceptanceCriteria: requirementAnalysis.acceptanceCriteria ?? [],
              steps: followUpSteps,
              round: autoFollowUpAttempt,
              requiredTotal: this.resolveRequiredTotal(orchestratorRuntimeSnapshot),
              terminalRequired: this.resolveTerminalRequired(orchestratorRuntimeSnapshot),
              pendingRequired: pendingRequiredTodos,
            });
            continue;
          }

          if (autoRepairAttempt > 0 && autoRepairHistory.length > 0) {
            const historySection = [
              '[System] 自动修复轮次记录：',
              ...autoRepairHistory.map(item => `- ${item}`),
            ].join('\n');
            finalContent = finalContent.trim()
              ? `${finalContent}\n\n${historySection}`
              : historySection;
            this.messageHub.result(historySection, {
              metadata: { phase: 'system_section', extra: { type: 'auto_repair_history' } },
            });
          }

          if (autoFollowUpAttempt > 0 && autoFollowUpHistory.length > 0) {
            const historySection = [
              '[System] 自动续跑轮次记录：',
              ...autoFollowUpHistory.map(item => `- ${item}`),
            ].join('\n');
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
        if (finalSessionId && finalPlanId && finalExecutionStatus) {
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
              // 更新 Mission 终态
              if (finalExecutionStatus === 'completed') {
                await this.taskViewService.completeTaskById(this.lastMissionId);
              } else if (finalExecutionStatus === 'failed') {
                await this.taskViewService.failTaskById(this.lastMissionId, this.lastExecutionErrors[0]);
              } else if (finalExecutionStatus === 'cancelled') {
                await this.taskViewService.cancelTaskById(this.lastMissionId);
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
        // 终止快照/get_todos 必须与 Todo 的真实 missionId 对齐，避免误读历史或空作用域。
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
    finalStatus: 'completed' | 'failed' | 'cancelled';
  } {
    return {
      success: this.lastExecutionFinalStatus === 'completed',
      errors: [...this.lastExecutionErrors],
      runtimeReason: this.lastExecutionRuntimeReason,
      finalStatus: this.lastExecutionFinalStatus,
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
      `修复轮次：${input.round}/${input.maxRounds}`,
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

  private resolveFollowUpSteps(runtimeSteps?: string[]): string[] {
    if (!Array.isArray(runtimeSteps)) {
      return [];
    }
    return normalizeNextSteps(runtimeSteps);
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

    return [
      '[System] 你上一轮给出了下一步建议，已进入自动续跑。',
      `续跑轮次：${input.round}`,
      `原始目标：${goal}`,
      `用户原始请求：${input.originalPrompt}`,
      `约束：\n${constraints}`,
      `验收标准：\n${acceptance}`,
      requiredSummary ? `必需 Todo 进度：\n${requiredSummary}` : '',
      `下一步建议：\n${steps}`,
      '请直接执行上述步骤，必要时调用工具或 dispatch_task 继续推进，不要重复总结。',
      '若确实无法执行，请明确说明原因并输出最终结论。',
    ].filter(line => line && line.trim().length > 0).join('\n\n');
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
