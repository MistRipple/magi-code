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
import { PermissionMatrix, StrategyConfig, SubTask, WorkerSlot, InteractionMode, INTERACTION_MODE_CONFIGS, InteractionModeConfig } from '../../types';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { CategoryResolver } from '../profile/category-resolver';
import { PlanRecord, PlanStorage } from '../plan-storage';
import { ExecutionPlan, OrchestratorState, QuestionCallback, RequirementAnalysis } from '../protocols/types';
import { IntentGate, IntentHandlerMode } from '../intent-gate';
import { VerificationRunner, VerificationConfig } from '../verification-runner';
import { MissionOrchestrator, ExecutionProgress } from './mission-orchestrator';
import {
  Mission,
  MissionStatus,
  Assignment,
  MissionStorageManager,
  FileBasedMissionStorage,
  MissionStateMapper,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { MessageHub, type SubTaskView as MessageHubSubTaskView } from './message-hub';
import {
  MessageType,
  createStandardMessage,
  MessageCategory,
  MessageLifecycle,
  type ContentBlock
} from '../../protocol/message-protocol';
import type { WorkerReport, OrchestratorResponse, WorkerEvidence, FileChangeRecord } from '../protocols/worker-report';
import { createAdjustResponse } from '../protocols/worker-report';
import { WisdomManager, type WisdomStorage } from '../wisdom';
import { buildIntentClassificationPrompt } from '../prompts/intent-classification';
import { buildWorkerNeedDecisionPrompt, buildRequirementAnalysisPrompt } from '../prompts/orchestrator-prompts';

/**
 * 用户确认回调类型
 */
export type ConfirmationCallback = (plan: ExecutionPlan, formattedPlan: string) => Promise<boolean>;
export type RecoveryConfirmationCallback = (
  failedTask: SubTask,
  error: string,
  options: { retry: boolean; rollback: boolean }
) => Promise<'retry' | 'rollback' | 'continue'>;
export type ClarificationCallback = (
  questions: string[],
  context: string,
  ambiguityScore: number,
  originalPrompt: string
) => Promise<{ answers: Record<string, string>; additionalInfo?: string } | null>;
export type WorkerQuestionCallback = (
  workerId: string,
  question: string,
  context: string,
  options?: string[]
) => Promise<string | null>;

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
 * 执行上下文
 */
export interface MissionDrivenContext {
  plan: ExecutionPlan | null;
  mission: Mission | null;
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
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private categoryResolver = new CategoryResolver();

  // UI 状态展示组件
  private planStorage: PlanStorage;
  private intentGate?: IntentGate;
  private verificationRunner?: VerificationRunner;
  private missionStateMapper = new MissionStateMapper();

  // 项目知识库
  private projectKnowledgeBase?: import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase;
  private wisdomManager: WisdomManager;

  // 状态
  private _state: OrchestratorState = 'idle';
  private _context: MissionDrivenContext = { plan: null, mission: null };
  private lastTaskAnalysis: {
    suggestedMode?: 'sequential' | 'parallel';
    explicitWorkers?: WorkerSlot[];
    wantsParallel?: boolean;
  } | null = null;
  private currentTaskId: string | null = null;
  private lastMissionId: string | null = null;
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

  // 回调
  private confirmationCallback?: ConfirmationCallback;
  private questionCallback?: QuestionCallback;
  private clarificationCallback?: ClarificationCallback;
  private workerQuestionCallback?: WorkerQuestionCallback;
  private recoveryConfirmationCallback?: RecoveryConfirmationCallback;
  private planConfirmationPolicy?: (risk: string) => boolean;

  // Token 统计
  private orchestratorTokens = { inputTokens: 0, outputTokens: 0 };

  private lastExecutionErrors: string[] = [];
  private lastExecutionSuccess = true;

  // 缓存需求分析结果（避免 DIRECT -> TASK 转换时重复调用）
  private _cachedRequirementAnalysis: RequirementAnalysis | null = null;

  // 执行统计
  private executionStats: ExecutionStats;

  // 统一消息出口
  private messageHub: MessageHub;
  private currentSessionId?: string;
  private contextSessionId: string | null = null;

  // 交互模式
  private interactionMode: InteractionMode = 'auto';
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.auto;
  // 运行状态
  private isRunning = false;
  private executionQueue: Promise<void> = Promise.resolve();

  // P0-3: 补充指令队列 - 存储执行中用户发送的补充指令
  private supplementaryInstructions: Array<{
    id: string;
    index: number;
    content: string;
    timestamp: number;
    source: 'user';
  }> = [];
  private supplementaryInstructionIndex = 0;
  private supplementaryInstructionCursors: Map<string, number> = new Map();

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
    this.planStorage = new PlanStorage(workspaceRoot);
    this.executionStats = new ExecutionStats();
    this.wisdomManager = new WisdomManager();

    // 初始化 Mission 存储（使用 .multicli/sessions 目录，按 session 分组存储）
    const sessionsDir = path.join(workspaceRoot, '.multicli', 'sessions');
    const fileStorage = new FileBasedMissionStorage(sessionsDir);
    this.missionStorage = new MissionStorageManager(fileStorage);

    // 初始化 Mission 编排器
    this.missionOrchestrator = new MissionOrchestrator(
      this.profileLoader,
      this.guidanceInjector,
      this.missionStorage,
      workspaceRoot
    );
    this.missionOrchestrator.setSnapshotManager(snapshotManager);
    this.missionOrchestrator.setContextManager(this.contextManager);
    this.missionOrchestrator.setAdapterFactory(adapterFactory);

    // MissionExecutor 已合并到 MissionOrchestrator，无需单独创建

    // 初始化统一消息出口
    this.messageHub = new MessageHub();

    this.configureWisdomStorage();

    this.setupEventForwarding();
  }

  /**
   * 配置 Wisdom 存储
   */
  private configureWisdomStorage(): void {
    const storage: WisdomStorage = {
      storeLearning: (learning: string, sourceAssignmentId: string) => {
        this.contextManager?.addImportantContext(`[Learning:${sourceAssignmentId}] ${learning}`);
      },
      storeDecision: (decision: string, sourceAssignmentId: string) => {
        const decisionId = `decision-${sourceAssignmentId}-${Date.now().toString(36)}`;
        this.contextManager?.addDecision(decisionId, decision, `来源 Assignment ${sourceAssignmentId}`);
      },
      storeWarning: (warning: string, sourceAssignmentId: string) => {
        this.contextManager?.addPendingIssue(`[${sourceAssignmentId}] ${warning}`);
      },
      storeSignificantLearning: (learning: string, context: string) => {
        if (this.projectKnowledgeBase && typeof (this.projectKnowledgeBase as any).addLearning === 'function') {
          (this.projectKnowledgeBase as any).addLearning(learning, context);
          return;
        }
        this.contextManager?.addImportantContext(`[Knowledge] ${learning} (${context})`);
      },
    };

    this.wisdomManager.setStorage(storage);
  }

  /**
   * 设置事件转发
   */
  private setupEventForwarding(): void {
    // Mission Phase 仍可用于内部统计，但不驱动编排者状态机

    // Mission 执行进度（现在从 MissionOrchestrator 获取）
    this.missionOrchestrator.on('progress', (progress: ExecutionProgress) => {
      this.emit('progress', progress);
    });

    // Worker 输出（现在从 MissionOrchestrator 获取）
    // 🔧 修复：将 Worker 输出发送到 MessageHub，确保前端能够显示
    this.missionOrchestrator.on('workerOutput', ({ workerId, output }) => {
      this.emit('workerOutput', { workerId, output });
      // 🔧 关键修复：发送到 MessageHub，路由到 Worker Tab
      this.messageHub.workerOutput(workerId, output);
    });

    // 🔧 P2: 转发分析结果到 UI (ORCHESTRATOR_PLAN)
    // 确保用户能看到编排者的思考和规划过程
    this.missionOrchestrator.on('analysisComplete', ({ strategy }) => {
      if (strategy && strategy.analysisSummary) {
        this.messageHub.orchestratorMessage(strategy.analysisSummary, {
          type: MessageType.PLAN, // 使用 PLAN 类型，前端会渲染为规划卡片
          metadata: {
            phase: 'planning',
            extra: {
              strategy: strategy
            }
          }
        });
      }
    });

    // 🔧 P2: 发送完整的执行计划卡片 (Plan Card)
    this.missionOrchestrator.on('missionPlanned', ({ mission }) => {
      const planBlock: ContentBlock = {
        type: 'plan',
        goal: mission.goal,
        analysis: mission.analysis,
        constraints: mission.constraints.map((c: any) => c.description),
        acceptanceCriteria: mission.acceptanceCriteria.map((c: any) => c.description),
        riskLevel: mission.riskLevel,
        riskFactors: mission.riskFactors
      };

      const message = createStandardMessage({
        traceId: this.messageHub.getTraceId(),
        category: MessageCategory.CONTENT,
        type: MessageType.PLAN,
        source: 'orchestrator',
        agent: 'orchestrator',
        lifecycle: MessageLifecycle.COMPLETED,
        blocks: [planBlock],
        metadata: {
          missionId: mission.id,
          phase: 'planning_complete'
        }
      });
      
      this.messageHub.sendMessage(message);
    });

    // 🔧 P1: 监听任务开始，发送 Running 状态卡片
    // 确保 UI 能够即时显示 Worker 状态为"执行中"
    this.missionOrchestrator.on('assignmentStarted', ({ assignmentId }) => {
      const mission = this._context.mission;
      const assignment = mission?.assignments.find(a => a.id === assignmentId);
      if (assignment && mission) {
        const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);
        
        // 依赖链序号
        const assignmentIndex = mission.assignments.findIndex(a => a.id === assignment.id);
        const totalAssignments = mission.assignments.length;
        const prefix = totalAssignments > 1 ? `[${assignmentIndex + 1}/${totalAssignments}] ` : '';

        this.messageHub.subTaskCard({
          id: mapped.id,
          title: prefix + mapped.title,
          status: 'running',
          worker: mapped.worker,
          summary: '执行中...'
        });
      }
    });

    // 🔧 P3: 监听 Todo 开始，实时更新卡片摘要
    // 让用户知道 Worker 具体在做什么
    this.missionOrchestrator.on('todoStarted', ({ assignmentId, content }) => {
      const mission = this._context.mission;
      const assignment = mission?.assignments.find(a => a.id === assignmentId);
      if (assignment && mission) {
        const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);

        // 依赖链序号
        const assignmentIndex = mission.assignments.findIndex(a => a.id === assignment.id);
        const totalAssignments = mission.assignments.length;
        const prefix = totalAssignments > 1 ? `[${assignmentIndex + 1}/${totalAssignments}] ` : '';

        this.messageHub.subTaskCard({
          id: mapped.id,
          title: prefix + mapped.title,
          status: 'running',
          worker: mapped.worker,
          summary: `正在执行: ${content}`
        });
      }
    });
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

  private enqueueExecution<T>(runner: () => Promise<T>): Promise<T> {
    const next = this.executionQueue.then(runner, runner);
    this.executionQueue = next.then(
      () => undefined,
      () => undefined
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
   * 获取当前上下文
   */
  get context(): MissionDrivenContext {
    return this._context;
  }

  /**
   * 获取当前阶段（state 的别名）
   */
  get phase(): OrchestratorState {
    return this._state;
  }

  /**
   * 获取当前执行计划
   */
  get plan(): ExecutionPlan | null {
    return this._context?.plan || null;
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

  // ============ P0-3: 补充指令机制 ============

  /**
   * 注入补充指令（执行中用户发送的追加消息）
   *
   * 规范要求：
   * - 补充指令不中断当前任务
   * - 编排者接收并暂存
   * - 在下一决策点（工具调用前/步骤边界/思考完成/等待确认）生效
   *
   * @param content 用户输入的补充内容
   * @returns 是否成功注入
   */
  injectSupplementaryInstruction(content: string): boolean {
    if (!this.isRunning) {
      logger.warn('引擎.补充指令.拒绝', { reason: '没有正在执行的任务' }, LogCategory.ORCHESTRATOR);
      return false;
    }

    const instruction = {
      id: `supp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      index: ++this.supplementaryInstructionIndex,
      content: content.trim(),
      timestamp: Date.now(),
      source: 'user' as const,
    };

    this.supplementaryInstructions.push(instruction);

    logger.info('引擎.补充指令.已暂存', {
      id: instruction.id,
      preview: content.substring(0, 50),
      queueSize: this.supplementaryInstructions.length,
    }, LogCategory.ORCHESTRATOR);

    // 触发事件通知 UI 指令已接收
    this.emit('supplementaryInstructionReceived', {
      id: instruction.id,
      count: this.supplementaryInstructions.length,
    });

    return true;
  }

  /**
   * 获取并清空待处理的补充指令
   * 在决策点调用此方法获取上下文
   *
   * @returns 待处理的补充指令内容数组
   */
  consumeSupplementaryInstructions(workerId?: WorkerSlot): string[] {
    if (this.supplementaryInstructions.length === 0) {
      return [];
    }

    // 兼容旧接口：未提供 workerId 时，直接消费全部
    if (!workerId) {
      const instructions = this.supplementaryInstructions.map(i => i.content);
      const count = instructions.length;
      this.supplementaryInstructions = [];
      this.supplementaryInstructionCursors.clear();
      logger.info('引擎.补充指令.已消费', { count }, LogCategory.ORCHESTRATOR);
      return instructions;
    }

    const lastIndex = this.supplementaryInstructionCursors.get(workerId) || 0;
    const pending = this.supplementaryInstructions.filter(i => i.index > lastIndex);
    if (pending.length === 0) {
      return [];
    }

    const latestIndex = pending[pending.length - 1].index;
    this.supplementaryInstructionCursors.set(workerId, latestIndex);
    this.pruneSupplementaryInstructions();

    logger.info('引擎.补充指令.已消费', {
      workerId,
      count: pending.length,
      latestIndex,
    }, LogCategory.ORCHESTRATOR);

    return pending.map(i => i.content);
  }

  /**
   * 查看当前待处理的补充指令数量（不消费）
   */
  getPendingInstructionCount(): number {
    return this.supplementaryInstructions.length;
  }

  /**
   * 清理已被所有已知 Worker 消费的补充指令
   */
  private pruneSupplementaryInstructions(): void {
    if (this.supplementaryInstructionCursors.size === 0) {
      return;
    }
    const minIndex = Math.min(...this.supplementaryInstructionCursors.values());
    if (!Number.isFinite(minIndex)) {
      return;
    }
    this.supplementaryInstructions = this.supplementaryInstructions.filter(i => i.index > minIndex);
  }

  /**
   * 重置补充指令状态（开始新任务前）
   */
  private resetSupplementaryInstructions(): void {
    this.supplementaryInstructions = [];
    this.supplementaryInstructionIndex = 0;
    this.supplementaryInstructionCursors.clear();
  }

  /**
   * 在决策点构建补充指令调整响应
   */
  private buildSupplementaryAdjustment(workerId: WorkerSlot): OrchestratorResponse | null {
    const instructions = this.consumeSupplementaryInstructions(workerId);
    if (instructions.length === 0) {
      return null;
    }

    const formatted = instructions.map(i => `- ${i}`).join('\n');
    return createAdjustResponse({
      newInstructions: `[System] 用户补充指令：\n${formatted}`,
    });
  }

  /**
   * 将待处理补充指令应用到 Mission（用于等待确认后的统一注入）
   */
  private applySupplementaryInstructionsToMission(mission: Mission): void {
    const instructions = this.consumeSupplementaryInstructions();
    if (instructions.length === 0) {
      return;
    }
    const formatted = instructions.map(i => `- ${i}`).join('\n');
    const content = `[System] 用户补充指令：\n${formatted}`;
    for (const assignment of mission.assignments) {
      assignment.guidancePrompt = assignment.guidancePrompt
        ? `${assignment.guidancePrompt}\n\n${content}`
        : content;
    }
  }

  /**
   * 发送任务分配说明到对应 Worker Tab
   *
   * 使用 delegationBriefing 作为详细任务说明
   * 如果没有，则使用用户原始需求作为任务描述
   */
  private sendWorkerDispatchMessage(mission: Mission, assignment: Assignment): void {
    // 使用 delegationBriefing 作为详细任务说明
    const content = assignment.delegationBriefing || mission.userPrompt || mission.goal;

    // 使用新的 workerInstruction API 发送到 Worker Tab
    this.messageHub.workerInstruction(assignment.workerId, content, {
      assignmentId: assignment.id,
      missionId: mission.id,
    });
  }

  /**
   * 发送最终总结消息到主对话区
   */
  private sendSummaryMessage(content: string, metadata?: Record<string, unknown>): void {
    // 使用 MessageHub 发送（统一消息出口）
    this.messageHub.result(content, {
      success: true,
      metadata: {
        phase: 'summary',
        extra: {
          isSummary: true,
          ...metadata,
        },
      },
    });
  }

  /**
   * Worker 汇报回调（统一由编排者处理）
   */
  private async handleWorkerReport(report: WorkerReport): Promise<OrchestratorResponse> {
    const timestamp = Date.now();
    const baseResponse: OrchestratorResponse = { action: 'continue', timestamp };

    if (!report) {
      return baseResponse;
    }

    if ((report.type === 'completed' || report.type === 'failed') && report.result) {
      if (!report.result.evidence) {
        const evidence = this.buildWorkerEvidence(report);
        if (evidence) {
          report.result.evidence = evidence;
        }
      }

      if (!report.result.wisdomExtraction) {
        const wisdom = this.wisdomManager.processReport(report, report.assignmentId);
        report.result.wisdomExtraction = {
          learnings: wisdom.learnings,
          decisions: wisdom.decisions,
          warnings: wisdom.warnings,
          significantLearning: wisdom.significantLearning,
        };
      } else {
        this.wisdomManager.processReport(report, report.assignmentId);
      }

      if (this.contextManager?.getMemoryDocument()?.isDirty()) {
        await this.contextManager.saveMemory();
      }
    }

    // 进度汇报 → 仅发送到对应 Worker Tab
    if (report.type === 'progress' && report.progress) {
      const progress = report.progress;
      const content = `${progress.currentStep} (进度 ${progress.percentage}%)`;
      this.messageHub.workerOutput(report.workerId, content, {
        metadata: {
          assignmentId: report.assignmentId,
          todoId: progress.currentTodoId,
          percentage: progress.percentage,
        },
      });
      return this.buildSupplementaryAdjustment(report.workerId) || baseResponse;
    }

    // Worker 提问 → 交给 UI 回答（阻塞则需用户决策）
    if (report.type === 'question' && report.question) {
      const question = report.question;
      const answer = this.workerQuestionCallback
        ? await this.workerQuestionCallback(report.workerId, question.content, '', question.options)
        : null;

      if (answer && answer.trim()) {
        return {
          action: 'answer',
          timestamp,
          answer: answer.trim(),
        };
      }

      if (question.blocking) {
        return {
          action: 'abort',
          timestamp,
          abortReason: '用户未提供必要回答，任务被阻塞终止',
        };
      }

      return baseResponse;
    }

    // 完成汇报 → 发送到 Worker Tab
    if (report.type === 'completed' && report.result) {
      const summary = report.result.summary || '任务已完成';
      // 🔧 使用 workerSummary 发送，确保前端识别为 WORKER_SUMMARY 类型
      this.messageHub.workerSummary(report.workerId, summary, {
        metadata: {
          assignmentId: report.assignmentId,
          modifiedFiles: report.result.modifiedFiles,
          createdFiles: report.result.createdFiles,
        },
      });
      this.emitSubTaskCard(report, 'completed');
      return baseResponse;
    }

    // 失败汇报 → 发送到 Worker Tab
    if (report.type === 'failed') {
      const error = report.error || report.result?.summary || '执行失败';
      this.messageHub.workerError(report.workerId, `执行遇到问题：${error}`, {
        metadata: {
          assignmentId: report.assignmentId,
        },
      });
      this.emitSubTaskCard(report, 'failed');
      return baseResponse;
    }

    return baseResponse;
  }

  private buildWorkerEvidence(report: WorkerReport): WorkerEvidence | undefined {
    if (!this.snapshotManager) {
      return undefined;
    }

    const assignmentId = report.assignmentId;
    if (!assignmentId) {
      return undefined;
    }

    const pendingChanges = this.snapshotManager.getPendingChanges();
    const matchedChanges = pendingChanges.filter(change => change.assignmentId === assignmentId);

    const fileChanges: FileChangeRecord[] = [];
    for (const change of matchedChanges) {
      const action = change.additions > 0 || change.deletions > 0 ? 'modify' : 'modify';
      fileChanges.push({
        path: change.filePath,
        action,
        linesAdded: change.additions,
        linesRemoved: change.deletions,
      });
    }

    if (report.result?.createdFiles?.length) {
      for (const createdFile of report.result.createdFiles) {
        if (!fileChanges.find(change => change.path === createdFile)) {
          fileChanges.push({
            path: createdFile,
            action: 'create',
          });
        }
      }
    }

    if (report.result?.modifiedFiles?.length) {
      for (const modifiedFile of report.result.modifiedFiles) {
        if (!fileChanges.find(change => change.path === modifiedFile)) {
          fileChanges.push({
            path: modifiedFile,
            action: 'modify',
          });
        }
      }
    }

    if (fileChanges.length === 0) {
      return undefined;
    }

    return {
      fileChanges,
      verifiedAt: Date.now(),
      verificationStatus: 'pending',
    };
  }

  /**
   * 发送子任务卡片（主对话区）
   */
  private emitSubTaskCard(report: WorkerReport, statusOverride: 'completed' | 'failed'): void {
    const mission = this._context.mission;
    if (!mission || !report.assignmentId) {
      return;
    }

    const assignment = mission.assignments.find(a => a.id === report.assignmentId);
    if (!assignment) {
      return;
    }

    const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);
    
    // 🔧 P2: 依赖链序号显示 [1/3]
    const assignmentIndex = mission.assignments.findIndex(a => a.id === assignment.id);
    const totalAssignments = mission.assignments.length;
    const prefix = totalAssignments > 1 ? `[${assignmentIndex + 1}/${totalAssignments}] ` : '';

    const subTask: MessageHubSubTaskView = {
      id: mapped.id,
      title: prefix + mapped.title,
      worker: mapped.worker,
      status: statusOverride === 'completed' ? 'completed' : 'failed',
      summary: report.result?.summary || report.error || mapped.summary,
      modifiedFiles: report.result?.modifiedFiles || mapped.modifiedFiles,
      createdFiles: report.result?.createdFiles || mapped.createdFiles,
      duration: mapped.duration,
    };

    this.messageHub.subTaskCard(subTask);
  }

  /**
   * 初始化引擎
   */
  async initialize(): Promise<void> {
    // 加载画像配置
    await this.profileLoader.load();

    // 初始化 IntentGate（使用适配器进行意图决策）
    const decider = async (prompt: string) => {
      const attempts = [
        buildIntentClassificationPrompt(prompt),
        `${buildIntentClassificationPrompt(prompt)}\n\n请严格只输出 JSON，不要包含多余文字。`
      ];

      for (const classificationPrompt of attempts) {
        const response = await this.adapterFactory.sendMessage(
          'orchestrator',
          classificationPrompt,
          undefined,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            streamToUI: false,  // 🔧 意图分类是内部决策，不应输出到 UI
            // 使用独立会话和空 System Prompt，确保意图分类不受编排器默认上下文影响
            isolatedSession: true,
            systemPrompt: '你是一个意图分析助手。请严格按照用户提供的指令格式输出 JSON。',
          }
        );
        this.recordOrchestratorTokens(response.tokenUsage);

        // 详细日志：捕获 LLM 原始响应
        logger.info('编排器.意图分类.LLM原始响应', {
          promptPreview: prompt.substring(0, 30),
          responseContent: response.content?.substring(0, 200),
        }, LogCategory.ORCHESTRATOR);

        try {
          const jsonMatch = response.content?.match(/\{[\s\S]*\}/);
          if (jsonMatch) {
            const parsed = JSON.parse(jsonMatch[0]);

            // 详细日志：捕获解析结果
            logger.info('编排器.意图分类.解析结果', {
              prompt: prompt.substring(0, 30),
              parsedIntent: parsed.intent,
              parsedMode: parsed.recommendedMode,
              parsedConfidence: parsed.confidence,
              parsedReason: parsed.reason,
            }, LogCategory.ORCHESTRATOR);

            const result = {
              intent: parsed.intent || 'task',
              recommendedMode: this.mapToHandlerMode(parsed.recommendedMode),
              confidence: parsed.confidence || 0.8,
              needsClarification: Boolean(parsed.needsClarification),
              clarificationQuestions: parsed.clarificationQuestions || [],
              reason: parsed.reason || '',
            };

            // 详细日志：最终结果
            logger.info('编排器.意图分类.最终结果', {
              prompt: prompt.substring(0, 30),
              intent: result.intent,
              recommendedMode: result.recommendedMode,
              confidence: result.confidence,
            }, LogCategory.ORCHESTRATOR);

            return result;
          }
        } catch (e) {
          logger.warn('意图分类解析失败，准备重试', { error: e }, LogCategory.ORCHESTRATOR);
        }
      }

      throw new Error('意图分类解析失败');
    };
    this.intentGate = new IntentGate(decider);
    this.missionOrchestrator.setIntentGate(decider);

    // 初始化 VerificationRunner
    if (this.config.verification && this.config.strategy?.enableVerification) {
      this.verificationRunner = new VerificationRunner(
        this.workspaceRoot,
        this.config.verification
      );
    }

    await this.configureContextCompression();

    logger.info('编排器.任务引擎.初始化.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 映射到 IntentHandlerMode
   */
  private mapToHandlerMode(mode: string): IntentHandlerMode {
    const modeMap: Record<string, IntentHandlerMode> = {
      ask: IntentHandlerMode.ASK,
      direct: IntentHandlerMode.DIRECT,
      explore: IntentHandlerMode.EXPLORE,
      task: IntentHandlerMode.TASK,
      demo: IntentHandlerMode.DEMO,
      clarify: IntentHandlerMode.CLARIFY,
    };
    return modeMap[mode] || IntentHandlerMode.TASK;
  }

  /**
   * 重新加载画像
   */
  async reloadProfiles(): Promise<void> {
    // ProfileLoader 使用静态配置，无需重载
  }

  /**
   * 设置项目知识库
   */
  setKnowledgeBase(knowledgeBase: import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase): void {
    this.projectKnowledgeBase = knowledgeBase;
    // 同时注入到 MissionOrchestrator
    this.missionOrchestrator.setKnowledgeBase(knowledgeBase);
    // 注入到 ContextManager（确保 Worker 上下文包含项目知识）
    this.contextManager.setProjectKnowledgeBase(knowledgeBase);
    this.configureWisdomStorage();
    logger.info('任务引擎.知识库.已设置', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 设置确认回调
   */
  setConfirmationCallback(callback: ConfirmationCallback): void {
    this.confirmationCallback = callback;
  }

  /**
   * 设置问题回调
   */
  setQuestionCallback(callback: QuestionCallback): void {
    this.questionCallback = callback;
  }

  /**
   * 设置澄清回调
   */
  setClarificationCallback(callback: ClarificationCallback): void {
    this.clarificationCallback = callback;
  }

  /**
   * 设置 Worker 问题回调
   */
  setWorkerQuestionCallback(callback: WorkerQuestionCallback): void {
    this.workerQuestionCallback = callback;
  }

  /**
   * 设置计划确认策略
   */
  setPlanConfirmationPolicy(policy: (risk: string) => boolean): void {
    this.planConfirmationPolicy = policy;
  }

  /**
   * 设置恢复确认回调
   */
  setRecoveryConfirmationCallback(callback: RecoveryConfirmationCallback): void {
    this.recoveryConfirmationCallback = callback;
  }

  /**
   * 执行任务 - 主入口
   */
  async execute(userPrompt: string, taskId: string, sessionId?: string): Promise<string> {
    return this.enqueueExecution(async () => {
      const trimmedPrompt = userPrompt?.trim() || '';
      if (!trimmedPrompt) {
        return '请输入你的需求或问题。';
      }

      this.isRunning = true;
      this.currentTaskId = taskId || null;
      this.lastMissionId = null;
      this.setState('running');
      this.lastTaskAnalysis = null;
      this.lastRoutingDecision = null;
      this.resetSupplementaryInstructions();

      // 保存 sessionId 用于消息发送
      this.currentSessionId = sessionId;

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        this.currentSessionId = resolvedSessionId;
        await this.ensureContextReady(resolvedSessionId);

      // 1. 意图分析（允许在同一次执行中完成澄清回合）
      let activePrompt = trimmedPrompt;
      let intentResult = await this.missionOrchestrator.processRequest(
        activePrompt,
        resolvedSessionId
      );
      logger.info('编排器.意图分析.结果', {
        mode: intentResult.mode,
        skipMission: intentResult.skipMission,
        promptPreview: activePrompt.substring(0, 50),
      }, LogCategory.ORCHESTRATOR);
      while (intentResult.skipMission && intentResult.mode === IntentHandlerMode.CLARIFY && intentResult.clarificationQuestions) {
        if (!this.clarificationCallback) {
          this.currentTaskId = null;
          return '任务已取消。';
        }
        const answers = await this.clarificationCallback(
          intentResult.clarificationQuestions,
          '',
          0.5,
          activePrompt
        );
        if (!answers) {
          this.currentTaskId = null;
          return '任务已取消。';
        }
        activePrompt = `${activePrompt}\n\n补充信息：${JSON.stringify(answers)}`;
        intentResult = await this.missionOrchestrator.processRequest(
          activePrompt,
          resolvedSessionId
        );
      }

      // 2. 处理非任务模式
      if (intentResult.skipMission) {
        this._context.mission = null;
        if (intentResult.mode === IntentHandlerMode.CLARIFY) {
          this.setState('idle');
          this.currentTaskId = null;
          return '任务已取消。';
        }

        if (intentResult.mode === IntentHandlerMode.ASK) {
          // **ASK 模式**: 编排者直接回答，不调用 Worker
          logger.info('编排器.执行.ASK模式', { prompt: activePrompt.substring(0, 50) }, LogCategory.ORCHESTRATOR);
          const result = await this.executeAskMode(
            activePrompt,
            taskId,
            resolvedSessionId
          );
          this.setState('idle');
          this.currentTaskId = null;
          return result;
        }

        if (intentResult.mode === IntentHandlerMode.DIRECT) {
          // **DIRECT 模式**: Phase 2 需求分析，决定是否需要 Worker
          logger.info('编排器.执行.DIRECT模式', { prompt: activePrompt.substring(0, 50) }, LogCategory.ORCHESTRATOR);

          // 使用统一的需求分析方法
          const requirementAnalysis = await this.analyzeRequirement(
            activePrompt,
            IntentHandlerMode.DIRECT
          );

          logger.info('编排器.DIRECT需求分析结果', {
            needsWorker: requirementAnalysis.needsWorker,
            hasDirectResponse: !!requirementAnalysis.directResponse?.trim(),
            directResponseLength: requirementAnalysis.directResponse?.length || 0,
            goal: requirementAnalysis.goal?.substring(0, 50),
            reason: requirementAnalysis.reason,
          }, LogCategory.ORCHESTRATOR);

          if (!requirementAnalysis.needsWorker) {
            if (requirementAnalysis.directResponse?.trim()) {
              logger.info('编排器.DIRECT模式.发送响应', {
                contentLength: requirementAnalysis.directResponse.length,
                contentPreview: requirementAnalysis.directResponse.substring(0, 100),
              }, LogCategory.ORCHESTRATOR);
              this.messageHub.result(requirementAnalysis.directResponse, {
                metadata: { intent: 'ask', decision: 'requirement_analysis' },
              });
              this.setState('idle');
              this.currentTaskId = null;
              return requirementAnalysis.directResponse;
            }
            logger.info('编排器.DIRECT模式.无直接响应.转ASK', undefined, LogCategory.ORCHESTRATOR);
            const result = await this.executeAskMode(
              activePrompt,
              taskId,
              resolvedSessionId
            );
            this.setState('idle');
            this.currentTaskId = null;
            return result;
          }

          // 需要 Worker：走完整 Mission 流程，已有需求分析结果
          this.lastRoutingDecision = {
            needsWorker: requirementAnalysis.needsWorker,
            category: requirementAnalysis.categories?.[0],
            categories: requirementAnalysis.categories,
            delegationBriefings: requirementAnalysis.delegationBriefings,
            needsTooling: requirementAnalysis.needsTooling,
            requiresModification: requirementAnalysis.requiresModification,
            executionMode: requirementAnalysis.executionMode,
            directResponse: requirementAnalysis.directResponse,
            reason: requirementAnalysis.reason,
          };

          // 缓存需求分析结果，避免重复调用
          this._cachedRequirementAnalysis = requirementAnalysis;

          intentResult = await this.missionOrchestrator.processRequest(
            activePrompt,
            resolvedSessionId,
            { forceMode: IntentHandlerMode.TASK }
          );
          if (!intentResult.mission) {
            this.setState('idle');
            this.currentTaskId = null;
            return '任务已取消。';
          }
          intentResult.skipMission = false;
          intentResult.mode = IntentHandlerMode.TASK;
        }

        if (intentResult.mode === IntentHandlerMode.DEMO) {
          // **DEMO 模式**: Phase 2 需求分析，然后进入任务流程
          logger.info('编排器.执行.DEMO模式', { prompt: activePrompt.substring(0, 50) }, LogCategory.ORCHESTRATOR);

          // 使用统一的需求分析方法
          const requirementAnalysis = await this.analyzeRequirement(
            activePrompt,
            IntentHandlerMode.DEMO
          );

          logger.info('编排器.DEMO需求分析结果', {
            needsWorker: requirementAnalysis.needsWorker,
            goal: requirementAnalysis.goal?.substring(0, 50),
            categories: requirementAnalysis.categories,
            reason: requirementAnalysis.reason,
          }, LogCategory.ORCHESTRATOR);

          // DEMO 模式强制需要 Worker
          const categories = requirementAnalysis.categories?.length
            ? requirementAnalysis.categories
            : this.categoryResolver.resolveAllFromText(activePrompt);

          this.lastRoutingDecision = {
            needsWorker: true,
            category: categories[0] || 'general',
            categories: categories.length ? categories : ['general'],
            delegationBriefings: requirementAnalysis.delegationBriefings ||
              ['这是一个演示/测试请求，请选择一个合适的测试场景来展示系统能力。'],
            needsTooling: requirementAnalysis.needsTooling,
            requiresModification: true,
            executionMode: requirementAnalysis.executionMode,
            reason: requirementAnalysis.reason || 'demo 模式自动转换为 task 模式',
          };

          // 缓存需求分析结果
          this._cachedRequirementAnalysis = requirementAnalysis;

          intentResult = await this.missionOrchestrator.processRequest(
            activePrompt,
            resolvedSessionId,
            { forceMode: IntentHandlerMode.TASK }
          );
          if (!intentResult.mission) {
            this.setState('idle');
            this.currentTaskId = null;
            return '任务已取消。';
          }
          intentResult.skipMission = false;
          intentResult.mode = IntentHandlerMode.TASK;
        }

        if (intentResult.mode === IntentHandlerMode.EXPLORE) {
          // **EXPLORE 模式**: Phase 2 需求分析，决定是否需要 Worker
          logger.info('编排器.执行.EXPLORE模式', { prompt: activePrompt.substring(0, 50) }, LogCategory.ORCHESTRATOR);

          // 使用统一的需求分析方法
          const requirementAnalysis = await this.analyzeRequirement(
            activePrompt,
            IntentHandlerMode.EXPLORE
          );

          logger.info('编排器.EXPLORE需求分析结果', {
            needsWorker: requirementAnalysis.needsWorker,
            hasDirectResponse: !!requirementAnalysis.directResponse?.trim(),
            directResponseLength: requirementAnalysis.directResponse?.length || 0,
            goal: requirementAnalysis.goal?.substring(0, 50),
            reason: requirementAnalysis.reason,
          }, LogCategory.ORCHESTRATOR);

          if (!requirementAnalysis.needsWorker) {
            if (requirementAnalysis.directResponse?.trim()) {
              logger.info('编排器.EXPLORE模式.发送响应', {
                contentLength: requirementAnalysis.directResponse.length,
                contentPreview: requirementAnalysis.directResponse.substring(0, 100),
              }, LogCategory.ORCHESTRATOR);
              this.messageHub.result(requirementAnalysis.directResponse, {
                metadata: { intent: 'explore', decision: 'requirement_analysis' },
              });
              this.setState('idle');
              this.currentTaskId = null;
              return requirementAnalysis.directResponse;
            }
            logger.info('编排器.EXPLORE模式.无直接响应.转ASK', undefined, LogCategory.ORCHESTRATOR);
            const result = await this.executeAskMode(
              activePrompt,
              taskId,
              resolvedSessionId
            );
            this.setState('idle');
            this.currentTaskId = null;
            return result;
          }

          // 需要 Worker：走完整 Mission 流程，已有需求分析结果
          this.lastRoutingDecision = {
            needsWorker: requirementAnalysis.needsWorker,
            category: requirementAnalysis.categories?.[0],
            categories: requirementAnalysis.categories,
            delegationBriefings: requirementAnalysis.delegationBriefings,
            needsTooling: requirementAnalysis.needsTooling,
            requiresModification: requirementAnalysis.requiresModification,
            executionMode: requirementAnalysis.executionMode,
            directResponse: requirementAnalysis.directResponse,
            reason: requirementAnalysis.reason,
          };

          // 缓存需求分析结果，避免重复调用
          this._cachedRequirementAnalysis = requirementAnalysis;

          intentResult = await this.missionOrchestrator.processRequest(
            activePrompt,
            resolvedSessionId,
            { forceMode: IntentHandlerMode.TASK }
          );
          if (!intentResult.mission) {
            this.setState('idle');
            this.currentTaskId = null;
            return '任务已取消。';
          }
          intentResult.skipMission = false;
          intentResult.mode = IntentHandlerMode.TASK;
        }

        if (intentResult.skipMission) {
          // 其他非任务模式
          this.setState('idle');
          this.currentTaskId = null;
          return intentResult.suggestion;
        }
      }

      // 3. 创建并执行 Mission
      const mission = intentResult.mission!;
      const missionTaskId = taskId || mission.id;
      this.currentTaskId = missionTaskId;
      this.lastMissionId = mission.id;
      this._context.mission = mission;

      // Phase 2: 需求分析（合并目标理解 + 路由决策）
      // 检查是否有缓存的需求分析结果（来自 DIRECT 模式转换）
      if (this._cachedRequirementAnalysis) {
        const cachedAnalysis = this._cachedRequirementAnalysis;
        this._cachedRequirementAnalysis = null; // 清除缓存

        // 应用缓存的目标理解到 Mission
        await this.missionOrchestrator.understandGoal(mission, {
          goal: cachedAnalysis.goal,
          analysis: cachedAnalysis.analysis,
          constraints: cachedAnalysis.constraints,
          acceptanceCriteria: cachedAnalysis.acceptanceCriteria,
          riskLevel: cachedAnalysis.riskLevel,
          riskFactors: cachedAnalysis.riskFactors,
        });
      } else if (!this.lastRoutingDecision) {
        const requirementAnalysis = await this.analyzeRequirement(
          activePrompt,
          IntentHandlerMode.TASK
        );

        // 验证：TASK 模式必须需要 Worker
        if (!requirementAnalysis.needsWorker) {
          throw new Error('需求分析结果无效：TASK 模式必须 needsWorker=true');
        }
        if (!requirementAnalysis.categories || requirementAnalysis.categories.length === 0) {
          throw new Error('需求分析结果无效：TASK 模式必须解析分类');
        }

        // 保存路由决策（兼容旧接口）
        this.lastRoutingDecision = {
          needsWorker: requirementAnalysis.needsWorker,
          category: requirementAnalysis.categories[0],
          categories: requirementAnalysis.categories,
          delegationBriefings: requirementAnalysis.delegationBriefings,
          needsTooling: requirementAnalysis.needsTooling,
          requiresModification: requirementAnalysis.requiresModification,
          executionMode: requirementAnalysis.executionMode,
          directResponse: requirementAnalysis.directResponse,
          reason: requirementAnalysis.reason,
        };

        // 应用目标理解到 Mission
        await this.missionOrchestrator.understandGoal(mission, {
          goal: requirementAnalysis.goal,
          analysis: requirementAnalysis.analysis,
          constraints: requirementAnalysis.constraints,
          acceptanceCriteria: requirementAnalysis.acceptanceCriteria,
          riskLevel: requirementAnalysis.riskLevel,
          riskFactors: requirementAnalysis.riskFactors,
        });
      } else {
        // 防御性分支：如果已有路由决策但没有缓存（理论上不应该发生）
        // 所有模式（DIRECT/EXPLORE/DEMO）现在都应该设置缓存
        logger.warn('编排器.需求分析.回退路径', {
          hasRoutingDecision: !!this.lastRoutingDecision,
          hasCachedAnalysis: !!this._cachedRequirementAnalysis,
        }, LogCategory.ORCHESTRATOR);
        await this.understandGoalWithLLM(mission, activePrompt, resolvedSessionId);
      }

      // Phase 3: 协作规划
      await this.planCollaborationWithLLM(mission, resolvedSessionId);

      // 发送任务分配宣告到主对话区
      if (mission.assignments.length > 0) {
        this.messageHub.taskAssignment(
          mission.assignments.map(a => ({
            worker: a.workerId,
            shortTitle: a.shortTitle || a.responsibility,
          })),
          { reason: this.lastRoutingDecision?.reason }
        );
      }

      // 向各 Worker 发送任务分配说明（展示在对应 Worker Tab）
      if (mission.assignments.length > 0) {
        mission.assignments.forEach((assignment) => {
          this.sendWorkerDispatchMessage(mission, assignment);
        });
      }

      // 6. 用户确认（如果需要）
      if (this.planConfirmationPolicy?.('medium')) {
        const plan = this.missionToPlan(mission);
        const formatted = this.formatPlanForUser(mission);

        if (this.confirmationCallback) {
          const confirmed = await this.confirmationCallback(plan, formatted);
          if (!confirmed) {
            await this.missionOrchestrator.cancelMission(mission.id, '用户取消');
            this.currentTaskId = null;
            return '任务已取消。';
          }
        }
      }

      // 等待确认后的决策点：注入补充指令
      this.applySupplementaryInstructionsToMission(mission);

      // 7. 批准并执行
      await this.missionOrchestrator.approveMission(mission.id);

      // 8. 执行 Mission

      const analysis = this.lastTaskAnalysis as unknown as {
        wantsParallel?: boolean;
        explicitWorkers?: WorkerSlot[];
        suggestedMode?: 'sequential' | 'parallel';
      } | null;
      let wantsParallel = Boolean(
        analysis?.wantsParallel
        || (analysis?.explicitWorkers?.length || 0) > 1
        || analysis?.suggestedMode === 'parallel'
      );
      let useWaveExecution = false;
      const executionMode = this.lastRoutingDecision?.executionMode;
      if (executionMode === 'parallel') {
        wantsParallel = true;
      } else if (executionMode === 'sequential' || executionMode === 'direct') {
        wantsParallel = false;
      } else if (executionMode === 'dependency_chain') {
        wantsParallel = false;
        useWaveExecution = true;
      }

      // 使用 MissionOrchestrator.execute()（MissionExecutor 已合并）
      const executionResult = await this.missionOrchestrator.execute(mission, {
        workingDirectory: this.workspaceRoot,
        timeout: this.config.timeout,
        parallel: wantsParallel,
        useWaveExecution,
        onProgress: (progress) => {
          this.emit('progress', progress);
        },
        onOutput: (workerId, output) => {
          this.emit('workerOutput', { workerId, output });
        },
        onReport: (report) => this.handleWorkerReport(report),
        reportTimeout: 5000,
        getSupplementaryInstructions: (workerId) => this.consumeSupplementaryInstructions(workerId),
      });

      // 记录执行统计（按 Assignment 记录）
      for (const [assignmentId, assignmentResult] of executionResult.assignmentResults) {
        const assignment = mission.assignments.find(a => a.id === assignmentId);
        if (assignment) {
          this.executionStats.recordExecution({
            worker: assignment.workerId,
            taskId: missionTaskId,
            subTaskId: assignmentId,
            success: assignmentResult.success,
            duration: assignmentResult.totalDuration,
            error: assignmentResult.errors?.join('; '),
            inputTokens: assignmentResult.tokenUsage?.inputTokens,
            outputTokens: assignmentResult.tokenUsage?.outputTokens,
            phase: 'execution',
          });
        }
      }

      // 9. 执行失败时的恢复流程（Worker 失败或任务失败）
      if (!executionResult.success && this.recoveryConfirmationCallback) {
        const failedSubTask: SubTask = {
          id: mission.id,
          taskId: missionTaskId,
          description: mission.goal,
          assignedWorker: 'claude',
          status: 'failed',
          priority: 5,
          retryCount: 0,
          maxRetries: 3,
          output: [],
          targetFiles: [],
          dependencies: [],
          progress: 0,
        };
        const errorMsg = executionResult.errors?.join('; ') || '执行失败';
        const hasSnapshots = this.snapshotManager.hasSnapshots();

        const decision = await this.recoveryConfirmationCallback(
          failedSubTask,
          errorMsg,
          { retry: true, rollback: hasSnapshots }
        );

        if (decision === 'rollback' && hasSnapshots) {
          const rollbackCount = this.snapshotManager.revertAllChanges();
          logger.info('引擎.恢复.执行失败回滚', { rollbackCount }, LogCategory.ORCHESTRATOR);
          return `执行失败，已回滚 ${rollbackCount} 个文件的更改。`;
        } else if (decision === 'retry') {
          return this.execute(userPrompt, taskId, sessionId);
        }
        // decision === 'continue': 继续进入验证与总结
      }

      // 9. 验证结果
      // 不再发送空洞的过渡消息，验证结果会在最终总结中统一呈现

      const verificationResult = await this.missionOrchestrator.verifyMission(mission.id);
      this.lastExecutionSuccess = executionResult.success && verificationResult.passed;
      this.lastExecutionErrors = [
        ...(executionResult.errors || []),
        ...(verificationResult.passed ? [] : [verificationResult.summary || '验证未通过']),
      ];

      // 10. 验证失败时的恢复流程
      if (!verificationResult.passed && this.recoveryConfirmationCallback) {
        const failedSubTask: SubTask = {
          id: mission.id,
          taskId: missionTaskId,
          description: mission.goal,
          assignedWorker: 'claude',
          status: 'failed',
          priority: 5,
          retryCount: 0,
          maxRetries: 3,
          output: [],
          targetFiles: [],
          dependencies: [],
          progress: 0,
        };
        const errorMsg = `验证失败: ${verificationResult.summary || '未通过验收标准'}`;
        const hasSnapshots = this.snapshotManager.hasSnapshots();

        const decision = await this.recoveryConfirmationCallback(
          failedSubTask,
          errorMsg,
          { retry: true, rollback: hasSnapshots }
        );

        if (decision === 'rollback' && hasSnapshots) {
          const rollbackCount = this.snapshotManager.revertAllChanges();
          logger.info('引擎.恢复.回滚', { rollbackCount }, LogCategory.ORCHESTRATOR);
          return `验证失败，已回滚 ${rollbackCount} 个文件的更改。`;
        } else if (decision === 'retry') {
          // 重新执行 Mission
          return this.execute(userPrompt, taskId, sessionId);
        }
        // decision === 'continue': 继续生成总结
      }

      // 11. 生成总结
      // 不再发送空洞的过渡消息，总结内容会直接呈现

      const summary = await this.missionOrchestrator.summarizeMission(mission.id);

      // 12. 清理 Worker 适配器历史以控制 token 消耗
      this.clearWorkerHistoriesAfterMission();

      this.setState('idle');

      const formatted = this.formatSummary(summary, this.lastExecutionSuccess, this.lastExecutionErrors);

      // 发送最终总结消息到主对话区
      this.sendSummaryMessage(formatted, {
        success: this.lastExecutionSuccess,
        completedTodos: summary.completedTodos,
        failedTodos: summary.failedTodos,
        modifiedFiles: summary.modifiedFiles?.length || 0,
      });

      this.currentTaskId = null;
      return formatted;

    } catch (error) {
      // 执行过程中发生错误时的恢复流程
      const errorMessage = error instanceof Error ? error.message : String(error);
      this.lastExecutionSuccess = false;
      this.lastExecutionErrors = [errorMessage];

      if (this.recoveryConfirmationCallback) {
        if (!this.lastMissionId) {
          logger.error('引擎.恢复.缺失_missionId', { taskId }, LogCategory.ORCHESTRATOR);
          throw error;
        }
        const failedSubTask: SubTask = {
          id: this.lastMissionId,
          taskId: this.lastMissionId,
          description: userPrompt.substring(0, 100),
          assignedWorker: 'claude',
          status: 'failed',
          priority: 5,
          retryCount: 0,
          maxRetries: 3,
          output: [],
          targetFiles: [],
          dependencies: [],
          progress: 0,
        };
        const hasSnapshots = this.snapshotManager.hasSnapshots();

        try {
          const decision = await this.recoveryConfirmationCallback(
            failedSubTask,
            errorMessage,
            { retry: true, rollback: hasSnapshots }
          );

          if (decision === 'rollback' && hasSnapshots) {
            const rollbackCount = this.snapshotManager.revertAllChanges();
            logger.info('引擎.恢复.错误_回滚', { rollbackCount, error: errorMessage }, LogCategory.ORCHESTRATOR);
            this.setState('idle');
            this.currentTaskId = null;
            return `执行出错，已回滚 ${rollbackCount} 个文件的更改。\n\n错误: ${errorMessage}`;
          } else if (decision === 'retry') {
            // 重试执行
            return this.execute(userPrompt, taskId, sessionId);
          }
          // decision === 'continue': 继续抛出错误
        } catch (callbackError) {
          // 回调本身出错，继续原来的错误处理
          logger.warn('引擎.恢复.回调_失败', { error: String(callbackError) }, LogCategory.ORCHESTRATOR);
        }
      }

        this.setState('idle');
        this.currentTaskId = null;
        throw error;
      } finally {
        this.isRunning = false;
      }
    });
  }

  getLastExecutionStatus(): { success: boolean; errors: string[] } {
    return {
      success: this.lastExecutionSuccess,
      errors: [...this.lastExecutionErrors],
    };
  }

  /**
   * 带任务上下文执行
   * 自动判断是否需要创建任务，返回任务 ID 和结果
   */
  async executeWithTaskContext(
    userPrompt: string,
    sessionId?: string
  ): Promise<{ taskId: string; result: string }> {
    const result = await this.execute(userPrompt, '', sessionId);
    return { taskId: this.lastMissionId || '', result };
  }

  /**
   * 仅创建计划（不执行）
   */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
    await this.ensureContextReady(resolvedSessionId);

    // 创建 Mission
    const { mission } = await this.missionOrchestrator.processRequest(
      userPrompt,
      resolvedSessionId,
      { forceMode: IntentHandlerMode.TASK }
    );

    if (!mission) {
      throw new Error('无法创建 Mission');
    }

    // Phase 2: 需求分析（合并目标理解 + 路由决策）
    const requirementAnalysis = await this.analyzeRequirement(
      userPrompt,
      IntentHandlerMode.TASK
    );

    // 验证：TASK 模式必须需要 Worker
    if (!requirementAnalysis.needsWorker) {
      throw new Error('需求分析结果无效：createPlan 必须 needsWorker=true');
    }
    if (!requirementAnalysis.categories || requirementAnalysis.categories.length === 0) {
      throw new Error('需求分析结果无效：createPlan 必须解析分类');
    }

    // 保存路由决策
    this.lastRoutingDecision = {
      needsWorker: requirementAnalysis.needsWorker,
      category: requirementAnalysis.categories[0],
      categories: requirementAnalysis.categories,
      delegationBriefings: requirementAnalysis.delegationBriefings,
      needsTooling: requirementAnalysis.needsTooling,
      requiresModification: requirementAnalysis.requiresModification,
      directResponse: requirementAnalysis.directResponse,
      reason: requirementAnalysis.reason,
    };

    // 应用目标理解到 Mission
    await this.missionOrchestrator.understandGoal(mission, {
      goal: requirementAnalysis.goal,
      analysis: requirementAnalysis.analysis,
      constraints: requirementAnalysis.constraints,
      acceptanceCriteria: requirementAnalysis.acceptanceCriteria,
      riskLevel: requirementAnalysis.riskLevel,
      riskFactors: requirementAnalysis.riskFactors,
    });

    // Phase 3: 协作规划
    await this.planCollaborationWithLLM(mission, resolvedSessionId);

    // 转换为 PlanRecord（用于 UI 状态展示）
    const plan = this.missionToPlan(mission);
    const formattedPlan = this.formatPlanForUser(mission);
    const record: PlanRecord = {
      id: mission.id,
      taskId,
      sessionId: resolvedSessionId,
      prompt: userPrompt,
      plan,
      formattedPlan,
      createdAt: mission.createdAt,
      updatedAt: mission.updatedAt,
    };

    // 保存到 PlanStorage（用于 UI 状态展示）
    this.planStorage.savePlan(record);

    return record;
  }

  /**
   * 执行已有计划
   */
  async executePlan(
    plan: ExecutionPlan,
    taskId: string,
    sessionId?: string,
    _userPrompt?: string
  ): Promise<string> {
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
    await this.ensureContextReady(resolvedSessionId);

    // 尝试从存储加载对应的 Mission
    if (!plan.id || !plan.id.trim()) {
      throw new Error('ExecutionPlan missing id');
    }
    let mission = await this.missionStorage.load(plan.id);

    if (!mission) {
      // 如果没有 Mission，从 Plan 创建一个
      mission = this.planToMission(plan, taskId, resolvedSessionId);
      await this.missionStorage.save(mission);
    }

    // 执行 Mission（使用 MissionOrchestrator）
    this.applySupplementaryInstructionsToMission(mission);
    const executionResult = await this.missionOrchestrator.execute(mission, {
      workingDirectory: this.workspaceRoot,
      timeout: this.config.timeout,
      parallel: plan.executionMode === 'parallel',
      getSupplementaryInstructions: (workerId) => this.consumeSupplementaryInstructions(workerId),
    });

    // 验证和总结
    const verification = await this.missionOrchestrator.verifyMission(mission.id);
    this.lastExecutionSuccess = executionResult.success && verification.passed;
    this.lastExecutionErrors = [
      ...(executionResult.errors || []),
      ...(verification.passed ? [] : [verification.summary || '验证未通过']),
    ];
    const summary = await this.missionOrchestrator.summarizeMission(mission.id);

    return this.formatSummary(summary, this.lastExecutionSuccess, this.lastExecutionErrors);
  }

  /**
   * 恢复 Mission 执行（例如审批通过后）
   */
  async resumeMission(missionId: string, sessionId?: string): Promise<string> {
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || 'default';
    await this.ensureContextReady(resolvedSessionId);

    const mission = await this.missionStorage.load(missionId);
    if (!mission) {
      throw new Error(`Mission not found: ${missionId}`);
    }

    if (mission.status !== 'pending_approval' && mission.status !== 'paused') {
      logger.warn('尝试恢复非暂停状态的 Mission', { missionId, status: mission.status }, LogCategory.ORCHESTRATOR);
      // 继续尝试执行，可能是状态同步问题
    }

    // 更新状态为执行中
    mission.status = 'executing';
    await this.missionStorage.save(mission);

    this.isRunning = true;
    this.currentTaskId = mission.externalTaskId || null;
    this.lastMissionId = mission.id;
    this.setState('running');

    try {
      // 执行 Mission
      this.applySupplementaryInstructionsToMission(mission);
      const executionResult = await this.missionOrchestrator.execute(mission, {
        workingDirectory: this.workspaceRoot,
        timeout: this.config.timeout,
        getSupplementaryInstructions: (workerId) => this.consumeSupplementaryInstructions(workerId),
      });

      // 验证和总结
      let summaryContent = '';
      
      // 如果再次暂停（例如还有其他审批），则不进行验证/总结
      if (executionResult.hasPendingApprovals) {
        summaryContent = '任务部分完成，等待进一步审批。';
        this.messageHub.orchestratorMessage(summaryContent, {
          metadata: { phase: 'pending_approval' }
        });
      } else {
        const verification = await this.missionOrchestrator.verifyMission(mission.id);
        this.lastExecutionSuccess = executionResult.success && verification.passed;
        this.lastExecutionErrors = [
          ...(executionResult.errors || []),
          ...(verification.passed ? [] : [verification.summary || '验证未通过']),
        ];
        const summary = await this.missionOrchestrator.summarizeMission(mission.id);
        summaryContent = this.formatSummary(summary, this.lastExecutionSuccess, this.lastExecutionErrors);
      }

      this.setState('idle');
      this.currentTaskId = null;
      return summaryContent;
    } catch (error) {
      this.setState('idle');
      this.currentTaskId = null;
      throw error;
    }
  }

  /**
   * 执行已有计划（从 PlanRecord）
   */
  async executePlanRecord(
    record: PlanRecord,
    taskId?: string,
    sessionId?: string
  ): Promise<string> {
    const finalTaskId = taskId || record.taskId;
    const finalSessionId = sessionId || record.sessionId;
    return this.executePlan(record.plan, finalTaskId, finalSessionId, record.prompt);
  }

  /**
   * 获取活跃计划
   */
  getActivePlanForSession(sessionId: string): PlanRecord | null {
    // PlanStorage 没有 getActivePlan，使用 getLatestPlanForSession
    return this.planStorage.getLatestPlanForSession(sessionId);
  }

  /**
   * 获取最新计划
   */
  getLatestPlanForSession(sessionId: string): PlanRecord | null {
    return this.planStorage.getLatestPlanForSession(sessionId);
  }

  /**
   * 根据 ID 获取计划
   */
  getPlanById(planId: string, sessionId: string): PlanRecord | null {
    return this.planStorage.getPlan(planId, sessionId);
  }

  /**
   * 准备上下文
   */
  async prepareContext(_sessionId: string, _userPrompt: string): Promise<string> {
    const sessionId = _sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (sessionId) {
      await this.ensureContextReady(sessionId);
    }
    return this.contextManager.getContext(8000);
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

  /**
   * 记录助手消息
   */
  async recordAssistantMessage(content: string): Promise<void> {
    // 可以在这里记录对话历史
    logger.debug('编排器.任务引擎.消息.已记录', { length: content.length }, LogCategory.ORCHESTRATOR);
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
      const isKeyInstruction = this.isKeyInstruction(content);
      this.contextManager.addUserMessage(content, isKeyInstruction);

      // 如果是首条用户消息，尝试提取核心意图
      const memory = this.contextManager.getMemoryDocument();
      if (memory && !memory.getContent().primaryIntent) {
        const intent = this.extractPrimaryIntent(content);
        if (intent) {
          this.contextManager.setPrimaryIntent(intent);
        }
      }
    }
  }

  /**
   * 检测消息是否为关键指令
   */
  private isKeyInstruction(content: string): boolean {
    const keyPatterns = [
      /不要|不能|必须|一定要|禁止|严禁/,      // 约束性指令
      /确认|同意|拒绝|取消|放弃/,              // 决策性指令
      /使用|采用|选择|决定/,                   // 选择性指令
      /优先|首先|最重要/,                      // 优先级指令
    ];
    return keyPatterns.some(pattern => pattern.test(content));
  }

  /**
   * 从用户消息中提取核心意图
   */
  private extractPrimaryIntent(content: string): string {
    // 简单策略：取前 100 个字符作为意图摘要
    const trimmed = content.trim();
    if (trimmed.length <= 100) {
      return trimmed;
    }
    // 尝试在句号或换行处截断
    const breakPoint = trimmed.substring(0, 100).lastIndexOf('。');
    if (breakPoint > 30) {
      return trimmed.substring(0, breakPoint + 1);
    }
    return trimmed.substring(0, 100) + '...';
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
  }

  /**
   * 取消执行
   */
  async cancel(): Promise<void> {
    if (this._context.mission) {
      await this.missionOrchestrator.cancelMission(this._context.mission.id, '用户取消');
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
  getOrchestratorTokenUsage(): { inputTokens: number; outputTokens: number } {
    return { ...this.orchestratorTokens };
  }

  /**
   * 重置编排器 Token 使用
   */
  resetOrchestratorTokenUsage(): void {
    this.orchestratorTokens = { inputTokens: 0, outputTokens: 0 };
  }

  async reloadCompressionAdapter(): Promise<void> {
    await this.configureContextCompression();
  }

  /**
   * 设置扩展上下文
   */
  setExtensionContext(_context: import('vscode').ExtensionContext): void {
    this.executionStats.setContext(_context);
  }

  /**
   * 获取执行统计
   */
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

  /**
   * 获取会话的所有任务视图
   * 替代 UnifiedTaskManager.getAllTasks()
   */
  async listTaskViews(sessionId: string): Promise<import('../../task/task-view-adapter').TaskView[]> {
    const { missionToTaskView } = await import('../../task/task-view-adapter');
    const { TodoManager } = await import('../../todo');

    const missions = await this.missionStorage.listBySession(sessionId);
    const taskViews = [];

    // 性能优化：只创建一个 TodoManager 实例，批量获取所有 mission 的 todos
    const todosByMission = new Map<string, import('../../todo').UnifiedTodo[]>();

    try {
      const todoManager = new TodoManager(this.workspaceRoot);
      await todoManager.initialize();

      // 批量获取所有 mission 的 todos
      for (const mission of missions) {
        const todos = await todoManager.getByMission(mission.id);
        todosByMission.set(mission.id, todos);
      }
    } catch {
      // TodoManager 不可用时，使用空映射
    }

    for (const mission of missions) {
      const todos = todosByMission.get(mission.id) || [];
      taskViews.push(missionToTaskView(mission, todos));
    }

    return taskViews;
  }

  /**
   * 创建任务（Mission）
   * 替代 UnifiedTaskManager.createTask()
   */
  async createTaskFromPrompt(sessionId: string, prompt: string): Promise<import('../../task/task-view-adapter').TaskView> {
    const { missionToTaskView } = await import('../../task/task-view-adapter');

    const mission = await this.missionStorage.createMission({
      sessionId,
      userPrompt: prompt,
      context: '',
    });

    return missionToTaskView(mission, []);
  }

  /**
   * 取消任务
   * 替代 UnifiedTaskManager.cancelTask()
   */
  async cancelTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'cancelled';
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }

  /**
   * 删除任务
   * 替代 UnifiedTaskManager.deleteTask()
   */
  async deleteTaskById(taskId: string): Promise<void> {
    await this.missionStorage.delete(taskId);
  }

  /**
   * 标记任务失败
   * 替代 UnifiedTaskManager.failTask()
   */
  async failTaskById(taskId: string, _error: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'failed';
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }

  /**
   * 标记任务完成
   * 替代 UnifiedTaskManager.completeTask()
   */
  async completeTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'completed';
      mission.completedAt = Date.now();
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }

  /**
   * 启动任务
   * 替代 UnifiedTaskManager.startTask()
   */
  async startTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'executing';
      mission.startedAt = Date.now();
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }

  /**
   * 销毁引擎
   */
  dispose(): void {
    this.removeAllListeners();
    logger.info('编排器.任务引擎.销毁.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  // ============================================================================
  // 私有方法
  // ============================================================================

  /**
   * 使用 LLM 理解目标
   *
   * @deprecated 请使用 analyzeRequirement()，它合并了目标理解和路由决策
   */
  private async understandGoalWithLLM(
    mission: Mission,
    userPrompt: string,
    _sessionId: string
  ): Promise<void> {
    // 使用 Claude 分析用户请求
    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      `分析以下用户请求，提取关键信息并用自然语言向用户解释你的理解。

用户请求：${userPrompt}

## 输出格式

**重要：为了让用户理解你的分析过程，请先用自然语言解释你的理解，然后输出 JSON。**

格式如下：

### 需求理解
[用 2-3 句话向用户解释你对这个需求的理解，包括目标、关键点和可能的风险]

### 分析结果
\`\`\`json
{
  "goal": "用户想要达成什么",
  "analysis": "任务的复杂度和关键点",
  "constraints": ["任何限制条件"],
  "acceptanceCriteria": ["如何判断任务完成"],
  "riskLevel": "low|medium|high",
  "riskFactors": ["可能的风险因素"]
}
\`\`\``,
      undefined,
      { source: 'orchestrator', adapterRole: 'orchestrator', streamToUI: true }
    );

    this.recordOrchestratorTokens(response.tokenUsage);

    try {
      const jsonMatch = response.content?.match(/\{[\s\S]*\}/);
      if (jsonMatch) {
        const analysis = JSON.parse(jsonMatch[0]);
        await this.missionOrchestrator.understandGoal(mission, analysis);
      }
    } catch (error) {
      logger.warn('编排器.任务引擎.目标_分析.解析_失败', { error }, LogCategory.ORCHESTRATOR);
      // 使用默认值
      await this.missionOrchestrator.understandGoal(mission, {
        goal: userPrompt,
        analysis: '用户请求',
        constraints: [],
        acceptanceCriteria: ['任务完成'],
        riskLevel: 'low',
        riskFactors: [],
      });
    }
  }

  /**
   * 使用 LLM 规划协作
   */
  private async planCollaborationWithLLM(mission: Mission, _sessionId: string): Promise<void> {
    if (!this.lastRoutingDecision?.category && !this.lastRoutingDecision?.categories?.length) {
      throw new Error('编排器路由决策缺失：未解析分类');
    }
    this.lastTaskAnalysis = {
      explicitWorkers: [],
    };

    // 选择参与者（使用多分类以支持多 Worker 协作）
    const categories = this.lastRoutingDecision.categories ||
      (this.lastRoutingDecision.category ? [this.lastRoutingDecision.category] : []);

    const participants = await this.missionOrchestrator.selectParticipants(mission, {
      categories,
    });
    this.lastTaskAnalysis.explicitWorkers = participants;

    // 为每个 Worker 构建其分类映射
    const routingCategories: Record<string, string> = {};
    for (const category of categories) {
      const worker = this.profileLoader.getWorkerForCategory(category);
      if (worker && !routingCategories[worker]) {
        routingCategories[worker] = category;
      }
    }

    // 定义契约
    await this.missionOrchestrator.defineContracts(mission, participants);

    // 分配职责（传递 AI 生成的委托说明和每个 Worker 的分类）
    await this.missionOrchestrator.assignResponsibilities(mission, participants, {
      routingCategories,
      routingReason: this.lastRoutingDecision?.reason,
      requiresModification: this.lastRoutingDecision?.requiresModification,
      delegationBriefings: this.lastRoutingDecision?.delegationBriefings,
    });
  }

  private async configureContextCompression(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const compressorConfig = LLMConfigLoader.loadCompressorConfig();
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();

      const compressorReady = compressorConfig.enabled
        && Boolean(compressorConfig.baseUrl && compressorConfig.model)
        && LLMConfigLoader.validateConfig(compressorConfig, 'compressor');

      if (!compressorReady) {
        logger.warn('编排器.上下文.压缩模型.不可用_降级编排模型', {
          enabled: compressorConfig.enabled,
          hasBaseUrl: Boolean(compressorConfig.baseUrl),
          hasModel: Boolean(compressorConfig.model),
        }, LogCategory.ORCHESTRATOR);
      }

      const retryDelays = [10000, 20000, 30000];
      const recordCompression = (
        success: boolean,
        duration: number,
        usage?: { inputTokens?: number; outputTokens?: number },
        error?: string
      ) => {
        this.executionStats.recordExecution({
          worker: 'compressor',
          taskId: 'memory',
          subTaskId: 'compress',
          success,
          duration,
          error,
          inputTokens: usage?.inputTokens,
          outputTokens: usage?.outputTokens,
          phase: 'integration',
        });
      };

      const sendWithClient = async (client: any, label: string, payload: string): Promise<string> => {
        const startAt = Date.now();
        try {
          const response = await client.sendMessage({
            messages: [{ role: 'user', content: payload }],
            maxTokens: 2000,
            temperature: 0.3,
          });
          const duration = Date.now() - startAt;
          recordCompression(true, duration, {
            inputTokens: response.usage?.inputTokens,
            outputTokens: response.usage?.outputTokens,
          });
          return response.content || '';
        } catch (error: any) {
          const duration = Date.now() - startAt;
          recordCompression(false, duration, undefined, error?.message);
          logger.warn('编排器.上下文.压缩模型.调用失败', {
            model: label,
            error: this.normalizeErrorMessage(error),
          }, LogCategory.ORCHESTRATOR);
          throw error;
        }
      };

      const sendWithRetry = async (client: any, label: string, payload: string): Promise<string> => {
        for (let attempt = 0; attempt <= retryDelays.length; attempt++) {
          try {
            return await sendWithClient(client, label, payload);
          } catch (error: any) {
            if (this.isAuthOrQuotaError(error)) {
              throw error;
            }
            if (!this.isConnectionError(error) || attempt === retryDelays.length) {
              throw error;
            }
            const delay = retryDelays[attempt];
            logger.warn('编排器.上下文.压缩模型.连接失败_重试', {
              attempt: attempt + 1,
              delayMs: delay,
              error: this.normalizeErrorMessage(error),
              model: label,
            }, LogCategory.ORCHESTRATOR);
            await this.sleep(delay);
          }
        }
        throw new Error('Compression retry failed.');
      };

      const adapter = {
        sendMessage: async (message: string) => {
          try {
            if (!compressorReady) {
              throw new Error('compressor_unavailable');
            }
            const client = createLLMClient(compressorConfig);
            return await sendWithRetry(client, 'compressor', message);
          } catch (error: any) {
            const shouldFallback = !compressorReady
              || this.isAuthOrQuotaError(error)
              || this.isConnectionError(error)
              || this.isModelError(error)
              || this.isConfigError(error);
            if (!shouldFallback) {
              throw error;
            }
            logger.warn('编排器.上下文.压缩模型.降级_使用编排模型', {
              reason: !compressorReady ? 'not_available'
                : this.isAuthOrQuotaError(error) ? 'auth_or_quota'
                : this.isConnectionError(error) ? 'connection'
                : this.isModelError(error) ? 'model'
                : 'config',
              error: this.normalizeErrorMessage(error),
            }, LogCategory.ORCHESTRATOR);
            const fallbackClient = createLLMClient(orchestratorConfig);
            return await sendWithRetry(fallbackClient, 'orchestrator', message);
          }
        },
      };

      this.contextManager.setCompressorAdapter(adapter);
      const activeConfig = compressorReady ? compressorConfig : orchestratorConfig;
      logger.info('编排器.上下文.压缩模型.已设置', {
        model: activeConfig.model,
        provider: activeConfig.provider,
        fallbackToOrchestrator: !compressorReady,
      }, LogCategory.ORCHESTRATOR);
    } catch (error) {
      logger.error('编排器.上下文.压缩模型.设置失败', error, LogCategory.ORCHESTRATOR);
    }
  }

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

  /**
   * 由编排者 LLM 决策是否需要 Worker
   *
   * @deprecated 请使用 analyzeRequirement()，它合并了目标理解和路由决策
   */
  private async decideWorkerNeedWithLLM(
    userPrompt: string,
    mode: IntentHandlerMode
  ): Promise<{
    needsWorker: boolean;
    category?: string;
    categories?: string[];
    delegationBriefings?: string[];
    needsTooling?: boolean;
    requiresModification?: boolean;
    directResponse?: string;
    reason?: string;
  }> {
    const categoryHints = Array.from(this.profileLoader.getAllCategories().entries())
      .map(([name, config]) => `- ${name}: ${config.description}`)
      .join('\n');

    const prompts = [
      buildWorkerNeedDecisionPrompt(userPrompt, mode, categoryHints),
      `${buildWorkerNeedDecisionPrompt(userPrompt, mode, categoryHints)}\n\n再次强调：必须严格输出 JSON，且 needsWorker/directResponse 字段必须自洽。`,
    ];

    for (const prompt of prompts) {
      const response = await this.adapterFactory.sendMessage(
        'orchestrator',
        prompt,
        undefined,
        {
          source: 'orchestrator',
          streamToUI: false,  // 🔧 路由决策是内部决策，不应输出到 UI
          adapterRole: 'orchestrator',
          messageMeta: { intent: 'routing', mode },
        }
      );

      this.recordOrchestratorTokens(response.tokenUsage);

      if (response.error) {
        logger.warn('编排器.路由决策.失败', { error: response.error }, LogCategory.ORCHESTRATOR);
        continue;
      }

      try {
        const jsonMatch = response.content?.match(/\{[\s\S]*\}/);
        if (jsonMatch) {
          const parsed = JSON.parse(jsonMatch[0]) as {
            needsWorker?: boolean;
            delegationBriefings?: string[];
            delegationBriefing?: string;
            needsTooling?: boolean;
            requiresModification?: boolean;
            directResponse?: string;
            reason?: string;
          };
          const needsWorker = Boolean(parsed.needsWorker);
          const requiresModification = Boolean(parsed.requiresModification);
          const directResponse = typeof parsed.directResponse === 'string' ? parsed.directResponse.trim() : '';

          // 解析 delegationBriefings（单个或数组均可）
          const rawBriefings = Array.isArray(parsed.delegationBriefings)
            ? parsed.delegationBriefings
            : typeof parsed.delegationBriefing === 'string'
              ? [parsed.delegationBriefing]
              : [];
          const delegationBriefings = rawBriefings
            .map(b => typeof b === 'string' ? b.trim() : '')
            .filter(Boolean);

          if (!needsWorker && directResponse.length === 0) {
            logger.warn('编排器.路由决策.无效_直答缺失', { mode }, LogCategory.ORCHESTRATOR);
            continue;
          }

          // 使用多分类解析以支持多 Worker 协作
          const resolvedCategories = needsWorker
            ? this.categoryResolver.resolveAllFromText(userPrompt)
            : undefined;
          const resolvedCategory = resolvedCategories?.[0];

          return {
            needsWorker,
            category: resolvedCategory,
            categories: resolvedCategories,
            delegationBriefings: delegationBriefings.length > 0 ? delegationBriefings : undefined,
            needsTooling: Boolean(parsed.needsTooling),
            requiresModification,
            directResponse: directResponse || undefined,
            reason: parsed.reason,
          };
        }
      } catch (error) {
        logger.warn('编排器.路由决策.解析失败', { error }, LogCategory.ORCHESTRATOR);
      }
    }

    throw new Error('编排器.路由决策.解析失败');
  }

  // ============================================================================
  // Phase 2: 需求分析（合并目标理解 + 路由决策）
  // ============================================================================

  /**
   * Phase 2: 需求分析
   * 一次 LLM 调用，同时输出目标理解和路由决策
   *
   * @see docs/workflow-design.md - 5 阶段工作流
   */
  private async analyzeRequirement(
    userPrompt: string,
    mode: IntentHandlerMode
  ): Promise<RequirementAnalysis> {
    const categoryHints = Array.from(this.profileLoader.getAllCategories().entries())
      .map(([name, config]) => `- ${name}: ${config.description}`)
      .join('\n');

    const prompt = buildRequirementAnalysisPrompt(userPrompt, mode, categoryHints);

    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,  // Phase 2 输出用户可见
        adapterRole: 'orchestrator',
        messageMeta: { intent: 'requirement_analysis', mode },
      }
    );

    this.recordOrchestratorTokens(response.tokenUsage);

    if (response.error) {
      throw new Error(`需求分析失败: ${response.error}`);
    }

    try {
      const jsonMatch = response.content?.match(/\{[\s\S]*\}/);
      if (jsonMatch) {
        const parsed = JSON.parse(jsonMatch[0]) as {
          goal?: string;
          analysis?: string;
          constraints?: string[];
          acceptanceCriteria?: string[];
          riskLevel?: 'low' | 'medium' | 'high';
          riskFactors?: string[];
          needsWorker?: boolean;
          directResponse?: string;
          delegationBriefings?: string[];
          delegationBriefing?: string;
          executionMode?: 'direct' | 'sequential' | 'parallel' | 'dependency_chain';
          needsTooling?: boolean;
          requiresModification?: boolean;
          reason?: string;
        };

        const needsWorker = Boolean(parsed.needsWorker);
        const directResponse = typeof parsed.directResponse === 'string' ? parsed.directResponse.trim() : '';

        // 解析 delegationBriefings（单个或数组均可）
        const rawBriefings = Array.isArray(parsed.delegationBriefings)
          ? parsed.delegationBriefings
          : typeof parsed.delegationBriefing === 'string'
            ? [parsed.delegationBriefing]
            : [];
        const delegationBriefings = rawBriefings
          .map(b => typeof b === 'string' ? b.trim() : '')
          .filter(Boolean);

        // 验证：needsWorker=false 时必须有 directResponse
        if (!needsWorker && directResponse.length === 0) {
          throw new Error('需求分析结果无效：needsWorker=false 但缺少 directResponse');
        }

        // 使用多分类解析以支持多 Worker 协作
        const resolvedCategories = needsWorker
          ? this.categoryResolver.resolveAllFromText(userPrompt)
          : undefined;

        return {
          goal: parsed.goal || userPrompt,
          analysis: parsed.analysis || '用户请求',
          constraints: parsed.constraints,
          acceptanceCriteria: parsed.acceptanceCriteria,
          riskLevel: parsed.riskLevel,
          riskFactors: parsed.riskFactors,
          needsWorker,
          directResponse: directResponse || undefined,
          executionMode: parsed.executionMode,
          categories: resolvedCategories,
          delegationBriefings: delegationBriefings.length > 0 ? delegationBriefings : undefined,
          needsTooling: Boolean(parsed.needsTooling),
          requiresModification: Boolean(parsed.requiresModification),
          reason: parsed.reason || '需求分析完成',
        };
      }
    } catch (error) {
      logger.warn('编排器.需求分析.解析失败', { error }, LogCategory.ORCHESTRATOR);
    }

    throw new Error('需求分析解析失败');
  }

  /**
   * 执行 Ask 模式
   */
  private async executeAskMode(
    userPrompt: string,
    taskId: string,
    sessionId: string
  ): Promise<string> {
    logger.info('编排器.ASK模式.开始', {
      promptLength: userPrompt.length,
      promptPreview: userPrompt.substring(0, 50),
      taskId,
      sessionId,
    }, LogCategory.ORCHESTRATOR);

    const context = await this.prepareContext(sessionId, userPrompt);
    const prompt = context
      ? `请结合以下会话上下文回答用户问题。\n\n${context}\n\n## 用户问题\n${userPrompt}`
      : userPrompt;

    logger.info('编排器.ASK模式.发送LLM请求', {
      streamToUI: true,
      hasContext: !!context,
    }, LogCategory.ORCHESTRATOR);

    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: { taskId, intent: 'ask' },
      }
    );

    logger.info('编排器.ASK模式.LLM响应', {
      hasContent: !!response.content?.trim(),
      contentLength: response.content?.length || 0,
      hasError: !!response.error,
    }, LogCategory.ORCHESTRATOR);

    this.recordOrchestratorTokens(response.tokenUsage);

    if (response.error) {
      throw new Error(response.error);
    }

    // 🔧 简化逻辑：adapter 层已保证空内容会抛出错误，这里直接返回
    return response.content || '';
  }

  /**
   * Mission 转换为 ExecutionPlan（用于 UI 状态展示）
   */
  private missionToPlan(mission: Mission): ExecutionPlan {
    const requestedMode = this.lastRoutingDecision?.executionMode;
    const executionMode: ExecutionPlan['executionMode'] = requestedMode === 'parallel' ? 'parallel' : 'sequential';
    return {
      id: mission.id,
      analysis: mission.analysis,
      needsCollaboration: mission.assignments.length > 1,
      subTasks: mission.assignments.map((assignment) => ({
        id: assignment.id,
        taskId: mission.id,
        description: assignment.responsibility,
        assignedWorker: assignment.workerId,
        targetFiles: [],
        dependencies: [],
        priority: 3,
        status: 'pending' as const,
        progress: 0,
        retryCount: 0,
        maxRetries: 3,
        output: [],
      })),
      executionMode,
      summary: mission.goal,
      featureContract: '',
      acceptanceCriteria: mission.acceptanceCriteria.map(c => c.description),
      createdAt: mission.createdAt,
      riskLevel: mission.riskLevel,
    };
  }

  /**
   * ExecutionPlan 转换为 Mission（用于恢复执行）
   */
  private planToMission(plan: ExecutionPlan, taskId: string, sessionId: string): Mission {
    const now = Date.now();
    // ExecutionPlan 使用 summary 字段代替 goal
    const goal = plan.summary || plan.analysis;
    // 处理 riskLevel - Mission 的 RiskLevel 不包含 'critical'
    const riskLevel = (plan.riskLevel === 'critical' ? 'high' : (plan.riskLevel || 'low')) as 'low' | 'medium' | 'high';
    if (!plan.id || !plan.id.trim()) {
      throw new Error('ExecutionPlan missing id');
    }
    return {
      id: plan.id,
      sessionId,
      userPrompt: goal,
      goal,
      analysis: plan.analysis || '',
      context: '',
      constraints: [],
      acceptanceCriteria: (plan.acceptanceCriteria || []).map((desc, index) => ({
        id: `ac_${index}`,
        description: desc,
        verifiable: true,
        status: 'pending' as const,
      })),
      contracts: [],
      assignments: plan.subTasks.map((subTask) => {
        const category = this.categoryResolver.resolveFromText(
          `${subTask.title || ''} ${subTask.description || ''}`.trim()
        );
        return {
        id: subTask.id,
        missionId: plan.id,
        workerId: subTask.assignedWorker as WorkerSlot,  // ✅ Type assertion: assignments are only for workers
        assignmentReason: {
          profileMatch: { category, score: 0.8, matchedKeywords: [] },
          contractRole: 'none' as const,
          explanation: 'From ExecutionPlan',
          alternatives: [],
        },
        shortTitle: subTask.title || subTask.description.substring(0, 20),
        responsibility: subTask.description,
        scope: { includes: [], excludes: [] },
        guidancePrompt: '',
        producerContracts: [],
        consumerContracts: [],
        todos: [],
        planningStatus: 'pending' as const,
        status: 'pending' as const,
        progress: 0,
        createdAt: now,
      };
      }),
      riskLevel,
      riskFactors: [],
      executionPath: 'standard',
      status: 'pending_approval' as MissionStatus,
      phase: 'plan_review',
      createdAt: plan.createdAt || now,
      updatedAt: now,
    };
  }

  /**
   * 格式化计划供用户查看
   */
  private formatPlanForUser(mission: Mission): string {
    let output = `## 任务计划\n\n`;
    output += `**目标**: ${mission.goal}\n\n`;
    output += `**分析**: ${mission.analysis}\n\n`;
    output += `**风险等级**: ${mission.riskLevel}\n\n`;

    if (mission.assignments.length > 0) {
      output += `### 执行步骤\n\n`;
      mission.assignments.forEach((assignment, index) => {
        output += `${index + 1}. **${assignment.workerId}**: ${assignment.responsibility}\n`;
      });
    }

    if (mission.contracts.length > 0) {
      output += `\n### 契约\n\n`;
      mission.contracts.forEach((contract) => {
        output += `- ${contract.name}: ${contract.description}\n`;
      });
    }

    return output;
  }

  /**
   * 格式化总结
   */
  private formatSummary(
    summary: import('./mission-orchestrator').MissionSummary,
    passed: boolean,
    errors: string[] = []
  ): string {
    const totalTodos = summary.completedTodos + summary.failedTodos + summary.skippedTodos;

    // 使用自然语言格式的总结
    let output = `任务已完成。\n\n`;
    output += `目标：${summary.goal}\n\n`;

    if (passed) {
      output += `完成了 ${summary.completedTodos} 个子任务（共 ${totalTodos} 个）`;
    } else {
      output += `执行了 ${summary.completedTodos}/${totalTodos} 个子任务，部分需要检查`;
    }
    output += `\n\n`;

    if (summary.modifiedFiles.length > 0) {
      output += `涉及的文件：\n`;
      summary.modifiedFiles.forEach((file) => {
        output += `- ${file}\n`;
      });
    }

    if (!passed && errors.length > 0) {
      output += `\n需要关注的问题：\n`;
      errors.forEach((err) => {
        output += `- ${err}\n`;
      });
    }

    return output;
  }

  private isAuthOrQuotaError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (status === 401 || status === 403 || status === 429) return true;
    const message = this.normalizeErrorMessage(error).toLowerCase();
    return /unauthorized|forbidden|invalid api key|api key|auth|permission|quota|insufficient|billing|payment|exceeded|rate limit|limit|blocked|suspended|disabled|account/i.test(message);
  }

  private isConnectionError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (status === 408 || status === 502 || status === 503 || status === 504) return true;
    const code = typeof error?.code === 'string' ? error.code : '';
    if (['ETIMEDOUT', 'ECONNRESET', 'ECONNREFUSED', 'ENOTFOUND', 'EAI_AGAIN'].includes(code)) {
      return true;
    }
    const message = this.normalizeErrorMessage(error).toLowerCase();
    return /timeout|timed out|network|connection|fetch failed|socket hang up|tls|certificate|econnreset|econnrefused|enotfound|eai_again/.test(message);
  }

  private isModelError(error: any): boolean {
    const message = this.normalizeErrorMessage(error).toLowerCase();
    return /model|not found|unknown model|invalid model|unsupported model|no such model/.test(message);
  }

  private isConfigError(error: any): boolean {
    const message = this.normalizeErrorMessage(error).toLowerCase();
    return /disabled in config|invalid configuration|missing|not configured|config/.test(message);
  }

  private normalizeErrorMessage(error: any): string {
    if (!error) return 'Unknown error';
    if (typeof error === 'string') return error;
    if (error instanceof Error && error.message) return error.message;
    if (error?.message) return String(error.message);
    return String(error);
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  /**
   * 任务完成后清理 Worker 适配器历史
   * 以控制 token 消耗，避免历史无限增长
   */
  private clearWorkerHistoriesAfterMission(): void {
    if (this.adapterFactory.clearAllAdapterHistories) {
      this.adapterFactory.clearAllAdapterHistories();
      logger.debug('引擎.历史清理.完成', undefined, LogCategory.ORCHESTRATOR);
    }
  }
}
