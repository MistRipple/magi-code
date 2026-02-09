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
import { IntentDecision, IntentGate, IntentHandlerMode } from '../intent-gate';
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
import { buildRequirementAnalysisPrompt, buildUnifiedSystemPrompt, buildDispatchSummaryPrompt } from '../prompts/orchestrator-prompts';
import { extractEmbeddedJson } from '../../utils/content-parser';
// AutonomousWorker 和 TodoExecuteOptions 通过 MissionOrchestrator 间接使用，不直接引用
import { DispatchBatch, CancellationError, type DispatchEntry, type DispatchResult, type DispatchStatus } from './dispatch-batch';
import { createSharedContextEntry } from '../../context/shared-context-pool';
import { globalEventBus } from '../../events';

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

  // DispatchBatch 追踪（当前活跃的 Batch）
  private activeBatch: DispatchBatch | null = null;
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

    // Todo 事件监听 — 仅用于 Mission/Todo 循环模式
    // dispatch 模式的进度追踪由 report_progress 工具负责（见下方 progress:reported 监听）
    this.missionOrchestrator.on('todoStarted', ({ assignmentId, content }) => {
      const mission = this._context.mission;
      const assignment = mission?.assignments.find(a => a.id === assignmentId);
      if (!assignment || !mission) return;

      const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);
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
    });

    this.missionOrchestrator.on('todoCompleted', ({ assignmentId, content }) => {
      const mission = this._context.mission;
      const assignment = mission?.assignments.find(a => a.id === assignmentId);
      if (!assignment || !mission) return;

      const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);
      const prefix = this.buildSubTaskTitlePrefix(mission, assignment.id);
      this.messageHub.subTaskCard({
        id: mapped.id,
        title: prefix + mapped.title,
        status: 'running',
        worker: mapped.worker,
        summary: `完成: ${content}`,
        modifiedFiles: mapped.modifiedFiles,
        createdFiles: mapped.createdFiles,
      });
    });

    this.missionOrchestrator.on('todoFailed', ({ assignmentId, content, error }) => {
      const mission = this._context.mission;
      const assignment = mission?.assignments.find(a => a.id === assignmentId);
      if (!assignment || !mission) return;

      const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);
      const prefix = this.buildSubTaskTitlePrefix(mission, assignment.id);
      this.messageHub.subTaskCard({
        id: mapped.id,
        title: prefix + mapped.title,
        status: 'running',
        worker: mapped.worker,
        summary: `失败: ${content} - ${error || '未知错误'}`,
      });
    });

    // Worker Insight 通知：高优先级洞察推送给用户
    this.missionOrchestrator.on('insightGenerated', ({ workerId, type, content, importance }) => {
      const typeLabels: Record<string, string> = {
        decision: '决策', contract: '契约', risk: '风险', constraint: '约束',
      };
      const label = typeLabels[type] || type;
      const level = importance === 'critical' ? 'warning' : 'info';
      this.messageHub.notify(`[${workerId}] ${label}: ${content}`, level);
    });

    // Worker 进度汇报 — 仅用于 dispatch/直接执行模式
    // Mission/Todo 循环模式的进度追踪由上方 todoStarted/todoCompleted/todoFailed 负责
    // 两套系统职责互斥，不做交叉兜底
    const toolManager = this.adapterFactory.getToolManager();
    toolManager.on('progress:reported', ({ contextId, step, percentage }: {
      contextId: string;
      step: string;
      percentage?: number;
      details?: string;
    }) => {
      const entry = this.activeBatch?.getEntry(contextId);
      if (!entry) return;

      const percentStr = typeof percentage === 'number' ? ` (${percentage}%)` : '';
      this.messageHub.subTaskCard({
        id: contextId,
        title: entry.task.substring(0, 40),
        status: 'running',
        worker: entry.worker,
        summary: `${step}${percentStr}`,
      });
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

    // 未提供 workerId 时，消费全部待处理指令
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
   * 构建子任务卡片标题前缀（依赖链序号）
   */
  private buildSubTaskTitlePrefix(mission: Mission, assignmentId: string): string {
    const assignmentIndex = mission.assignments.findIndex(a => a.id === assignmentId);
    const totalAssignments = mission.assignments.length;
    if (assignmentIndex < 0 || totalAssignments <= 1) {
      return '';
    }
    return `[${assignmentIndex + 1}/${totalAssignments}] `;
  }

  /**
   * 发送子任务状态更新卡片（主对话区）
   */
  private emitSubTaskStatusCard(
    report: Pick<WorkerReport, 'assignmentId' | 'workerId'>,
    status: MessageHubSubTaskView['status'],
    summary?: string
  ): boolean {
    const mission = this._context.mission;
    if (!mission) {
      logger.error('编排器.Worker汇报.状态卡更新失败', {
        reason: 'mission_missing',
        workerId: report.workerId,
        assignmentId: report.assignmentId,
        status,
      }, LogCategory.ORCHESTRATOR);
      this.messageHub.systemNotice('任务状态同步失败：任务上下文缺失', {
        phase: 'subtask_status_sync',
        reason: 'mission_missing',
        worker: report.workerId,
        assignmentId: report.assignmentId,
        extra: { status },
      });
      return false;
    }

    if (!report.assignmentId) {
      logger.error('编排器.Worker汇报.状态卡更新失败', {
        reason: 'assignment_id_missing',
        workerId: report.workerId,
        status,
      }, LogCategory.ORCHESTRATOR);
      this.messageHub.systemNotice('任务状态同步失败：缺少任务分配标识', {
        phase: 'subtask_status_sync',
        reason: 'assignment_id_missing',
        worker: report.workerId,
        extra: { status },
      });
      return false;
    }

    const assignment = mission.assignments.find(a => a.id === report.assignmentId);
    if (!assignment) {
      logger.error('编排器.Worker汇报.状态卡更新失败', {
        reason: 'assignment_not_found',
        missionId: mission.id,
        assignmentId: report.assignmentId,
        workerId: report.workerId,
        status,
      }, LogCategory.ORCHESTRATOR);
      this.messageHub.systemNotice('任务状态同步失败：未找到对应任务分配', {
        phase: 'subtask_status_sync',
        reason: 'assignment_not_found',
        missionId: mission.id,
        assignmentId: report.assignmentId,
        worker: report.workerId,
        extra: { status },
      });
      return false;
    }

    const mapped = this.missionStateMapper.mapAssignmentToSubTaskView(assignment);
    const subTask: MessageHubSubTaskView = {
      id: mapped.id,
      title: this.buildSubTaskTitlePrefix(mission, assignment.id) + mapped.title,
      worker: mapped.worker,
      status,
      summary: summary || mapped.summary,
      modifiedFiles: mapped.modifiedFiles,
      createdFiles: mapped.createdFiles,
      duration: mapped.duration,
    };

    this.messageHub.subTaskCard(subTask);
    return true;
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

      // 提问期间主对话区卡片进入 pending（等待确认）
      this.emitSubTaskStatusCard(report, 'pending');

      let answer: string | null = null;
      if (this.workerQuestionCallback) {
        try {
          answer = await this.workerQuestionCallback(report.workerId, question.content, '', question.options);
        } catch (error) {
          if (error instanceof Error) {
            logger.error('编排器.Worker提问.回调异常', {
              workerId: report.workerId,
              assignmentId: report.assignmentId,
              blocking: question.blocking,
              errorName: error.name,
              errorMessage: error.message,
              stack: error.stack,
            }, LogCategory.ORCHESTRATOR);
          } else {
            logger.error('编排器.Worker提问.回调异常_非Error对象', {
              workerId: report.workerId,
              assignmentId: report.assignmentId,
              blocking: question.blocking,
              error: String(error),
            }, LogCategory.ORCHESTRATOR);
          }
          this.emitSubTaskStatusCard(report, 'failed', '提问处理失败：提问回调执行异常');
          return {
            action: 'abort',
            timestamp,
            abortReason: '提问处理失败：提问回调执行异常',
          };
        }
      } else {
        logger.error('编排器.Worker提问.回调缺失', {
          workerId: report.workerId,
          assignmentId: report.assignmentId,
          blocking: question.blocking,
        }, LogCategory.ORCHESTRATOR);
        this.emitSubTaskStatusCard(report, 'failed', '提问处理失败：提问通道未配置');
        return {
          action: 'abort',
          timestamp,
          abortReason: '提问处理失败：提问通道未配置',
        };
      }

      if (answer && answer.trim()) {
        // 收到用户回答后恢复 running
        this.emitSubTaskStatusCard(report, 'running');
        return {
          action: 'answer',
          timestamp,
          answer: answer.trim(),
        };
      }

      if (question.blocking) {
        // 阻塞问题无回答：终止并明确状态
        logger.warn('编排器.Worker提问.阻塞未回答_终止', {
          workerId: report.workerId,
          assignmentId: report.assignmentId,
          questionType: question.questionType,
        }, LogCategory.ORCHESTRATOR);
        this.emitSubTaskStatusCard(report, 'stopped', '等待用户回答超时，任务已终止');
        return {
          action: 'abort',
          timestamp,
          abortReason: '用户未提供必要回答，任务被阻塞终止',
        };
      }

      // 非阻塞问题无回答：继续执行并恢复 running
      logger.warn('编排器.Worker提问.非阻塞未回答_继续', {
        workerId: report.workerId,
        assignmentId: report.assignmentId,
        questionType: question.questionType,
      }, LogCategory.ORCHESTRATOR);
      this.emitSubTaskStatusCard(report, 'running', '未收到用户回答，按默认策略继续执行');
      return baseResponse;
    }

    // 完成汇报 → Worker 的 LLM 输出已通过 normalizer 流式路径到达 Worker Tab，
    // 此处仅更新主对话区的 SubTaskCard 状态
    if (report.type === 'completed' && report.result) {
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
    this.applyToolPermissions();

    // 初始化 IntentGate（使用适配器进行意图决策）
    const decider = async (prompt: string) => {
      const sessionContext = await this.prepareDecisionContext();
      const attempts = [
        buildIntentClassificationPrompt(prompt, sessionContext),
        `${buildIntentClassificationPrompt(prompt, sessionContext)}\n\n请严格只输出 JSON，不要包含多余文字。`
      ];

      for (const classificationPrompt of attempts) {
        const response = await this.adapterFactory.sendMessage(
          'orchestrator',
          classificationPrompt,
          undefined,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            visibility: 'system',  // 🔧 意图分类是内部决策，不应输出到 UI
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
          const parsed = this.extractIntentClassificationPayload(response.content ?? '');
          if (parsed) {

            // 详细日志：捕获解析结果
            logger.info('编排器.意图分类.解析结果', {
              prompt: prompt.substring(0, 30),
              parsedIntent: parsed.intent,
              parsedMode: parsed.recommendedMode,
              parsedConfidence: parsed.confidence,
              parsedReason: parsed.reason,
            }, LogCategory.ORCHESTRATOR);

            const result: IntentDecision = {
              intent: this.normalizeIntent(parsed.intent),
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

    // 注入编排工具的回调处理器
    this.setupOrchestrationToolHandlers();

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
   * 注入编排工具（dispatch_task / plan_mission / send_worker_message）的回调处理器
   */
  private setupOrchestrationToolHandlers(): void {
    const toolManager = this.adapterFactory.getToolManager();
    const orchestrationExecutor = toolManager.getOrchestrationExecutor();

    // 从 ProfileLoader 动态注入 Worker 列表到工具定义
    const allProfiles = this.profileLoader.getAllProfiles();
    orchestrationExecutor.setAvailableWorkers(
      Array.from(allProfiles.values()).map(p => ({
        slot: p.worker,
        description: p.persona.strengths.slice(0, 2).join('/'),
      }))
    );

    orchestrationExecutor.setHandlers({
      dispatch: async (params) => {
        const { worker, task, files, dependsOn } = params;
        logger.info('编排工具.dispatch_task.开始', {
          worker, taskPreview: task.substring(0, 80), dependsOn,
        }, LogCategory.ORCHESTRATOR);

        // 生成唯一 task_id
        const taskId = `dispatch-${Date.now()}-${worker}-${Math.random().toString(36).substring(2, 5)}`;

        // 确保 DispatchBatch 存在（一次 orchestrator LLM 调用共享一个 Batch）
        if (!this.activeBatch || this.activeBatch.status !== 'active') {
          this.activeBatch = new DispatchBatch();
          this.activeBatch.userPrompt = this.activeUserPrompt;
          this.setupBatchEventHandlers(this.activeBatch);
        }

        // 注册到 DispatchBatch
        try {
          this.activeBatch.register({ taskId, worker, task, files, dependsOn });

          // C-12: 环检测 + 深度上限校验
          this.activeBatch.topologicalSort();
          this.activeBatch.validateDepthLimit();

          // C-13: 文件冲突解决 — 冲突的并行任务自动添加依赖转串行
          const serialized = this.activeBatch.resolveFileConflicts();
          if (serialized > 0) {
            logger.info('DispatchBatch.文件冲突.已自动串行化', {
              addedDeps: serialized, taskId,
            }, LogCategory.ORCHESTRATOR);
            // 串行化后重新验证拓扑和深度
            this.activeBatch.topologicalSort();
            this.activeBatch.validateDepthLimit();
          }
        } catch (regError: any) {
          return { task_id: taskId, status: 'failed' as const, worker, error: regError.message };
        }

        // 发送 subTaskCard（状态取决于注册后是否有依赖）
        const entry = this.activeBatch.getEntry(taskId);
        const hasDeps = entry ? entry.status === 'waiting_deps' : (dependsOn && dependsOn.length > 0);
        this.messageHub.subTaskCard({
          id: taskId,
          title: task.substring(0, 40),
          status: hasDeps ? 'pending' : 'running',
          worker: worker,
        });

        // 通过隔离策略决定是否立即启动（约束 5）
        if (!hasDeps) {
          this.dispatchReadyTasksWithIsolation(this.activeBatch);
        }
        // 有依赖的任务由 DispatchBatch 的 task:ready 事件触发

        // 立即返回 task_id（非阻塞）
        return { task_id: taskId, status: 'dispatched' as const, worker };
      },

      plan: async (params) => {
        const { goal, constraints, workers } = params;
        logger.info('编排工具.plan_mission.开始', {
          goalPreview: goal.substring(0, 80),
          constraints,
          workers,
        }, LogCategory.ORCHESTRATOR);

        try {
          // 将 constraints 和 workers 拼入 goal，供 Mission 创建使用
          let enrichedGoal = goal;
          if (constraints && constraints.length > 0) {
            enrichedGoal += `\n\n约束条件:\n${constraints.map(c => `- ${c}`).join('\n')}`;
          }
          if (workers && workers.length > 0) {
            enrichedGoal += `\n\n建议 Worker: ${workers.join(', ')}`;
          }

          // 通过 MissionOrchestrator 的完整流程创建 Mission
          const sessionId = this.currentSessionId || 'plan-session';
          const result = await this.missionOrchestrator.processRequest(enrichedGoal, sessionId, {
            forceMode: IntentHandlerMode.TASK,
          });

          if (!result.mission) {
            return { success: false, summary: '创建 Mission 失败', errors: ['Mission creation failed'] };
          }

          return {
            success: true,
            missionId: result.mission.id,
            summary: `Mission 已创建: ${result.mission.goal}`,
          };
        } catch (error: any) {
          return { success: false, summary: error.message, errors: [error.message] };
        }
      },

      sendMessage: async (params) => {
        const { worker, message } = params;
        logger.info('编排工具.send_worker_message', {
          worker, messagePreview: message.substring(0, 80),
        }, LogCategory.ORCHESTRATOR);

        this.messageHub.workerInstruction(worker, message);
        return { delivered: true };
      },
    });

    logger.info('编排器.编排工具回调.已注入', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 启动 dispatch Worker 执行（非阻塞）
   *
   * 创建 Assignment、获取 Worker 实例、在后台执行。
   * 执行过程中通过 subTaskCard 回传进度，完成后更新 DispatchBatch 状态。
   */
  private launchDispatchWorker(taskId: string, worker: WorkerSlot, task: string, files?: string[]): void {
    const batch = this.activeBatch;

    // 标记开始运行
    batch?.markRunning(taskId);
    this.messageHub.subTaskCard({
      id: taskId,
      title: task.substring(0, 40),
      status: 'running',
      worker,
    });

    // 发送任务指令到 Worker Tab，让用户看到 orchestrator 对 worker 的要求
    this.messageHub.workerInstruction(worker, task, {
      assignmentId: taskId,
      missionId: batch?.id,
    });

    (async () => {
      // 确保 Worker 存在
      const workerInstance = await this.missionOrchestrator.ensureWorkerForDispatch(worker);

      // 构建轻量 Assignment（不走 Mission 系统）
      // missionId 使用 batchId 保证同 batch 内 Worker 共享 SharedContextPool、跨 batch 隔离
      const assignment: Assignment = {
        id: taskId,
        missionId: batch?.id || 'dispatch',
        workerId: worker,
        shortTitle: task.substring(0, 20),
        responsibility: task,
        delegationBriefing: task,
        assignmentReason: {
          profileMatch: { category: 'dispatch', score: 1, matchedKeywords: [] },
          contractRole: 'none' as const,
          explanation: 'dispatch_task 工具直接分配',
          alternatives: [],
        },
        scope: {
          includes: [task],
          excludes: [],
          targetPaths: files || [],
          requiresModification: true,
        },
        guidancePrompt: '',
        producerContracts: [],
        consumerContracts: [],
        todos: [],
        planningStatus: 'planned' as const,
        status: 'pending' as const,
        progress: 0,
        createdAt: Date.now(),
      };

      // 获取项目上下文
      const projectContext = this.projectKnowledgeBase
        ? this.projectKnowledgeBase.getProjectContext(600)
        : undefined;

      // 设置快照上下文（dispatch 模式也需要精确记录文件变更）
      // todoId 直接使用 taskId，与 subTaskCard 的 id 对齐，确保前端能匹配快照
      const toolManager = this.adapterFactory.getToolManager();
      toolManager.setSnapshotContext({
        missionId: batch?.id || 'dispatch',
        assignmentId: taskId,
        todoId: taskId,
        workerId: worker,
      });

      try {
      // C-09: 传递 cancellationToken 到 Worker，建立取消信号链
      // 不设置总时间超时 — Worker 可以运行任意长时间，失败检测由 Worker 内部的连续失败机制处理
      const result = await workerInstance.executeAssignment(assignment, {
        workingDirectory: this.workspaceRoot,
        adapterFactory: this.adapterFactory,
        projectContext,
        onReport: (report) => this.handleDispatchWorkerReport(report, batch),
        cancellationToken: batch?.cancellationToken,
        imagePaths: this.activeImagePaths,
      });

      const summary = result.directOutput?.summary
        || (result.completedTodos.length > 0
          ? `完成 ${result.completedTodos.length} 个任务`
          : (result.success ? '任务完成' : '任务失败'));
      const modifiedFiles = result.directOutput?.modifiedFiles
        || result.completedTodos.flatMap(t => t.output?.modifiedFiles || []);

      // 更新 subTaskCard 最终状态
      this.messageHub.subTaskCard({
        id: taskId,
        title: task.substring(0, 40),
        status: result.success ? 'completed' : 'failed',
        worker,
        summary,
        modifiedFiles,
      });

      // 更新 DispatchBatch 状态（含 tokenUsage 传递，供 archive 日志统计）
      const dispatchResult: DispatchResult = {
        success: result.success, summary, modifiedFiles,
        tokenUsage: result.tokenUsage ? {
          inputTokens: result.tokenUsage.inputTokens || 0,
          outputTokens: result.tokenUsage.outputTokens || 0,
        } : undefined,
      };
      if (result.success) {
        batch?.markCompleted(taskId, dispatchResult);
      } else {
        batch?.markFailed(taskId, dispatchResult);
      }

      // 记录 Worker Token 使用到 executionStats
      const singleResult = new Map<string, import('../worker').AutonomousExecutionResult>();
      singleResult.set(taskId, result);
      this.recordWorkerTokenUsage(singleResult);

      logger.info('编排工具.dispatch_task.Worker完成', {
        worker, taskId, success: result.success, summary,
      }, LogCategory.ORCHESTRATOR);
      } finally {
        // 清除快照上下文（无论成功或失败）
        toolManager.clearSnapshotContext();
      }
    })().catch(async (error: any) => {
      // C-09: 取消异常不按失败处理，cancelAll 已标记 cancelled 状态
      if (error instanceof CancellationError || error?.isCancellation) {
        this.messageHub.subTaskCard({
          id: taskId,
          title: task.substring(0, 40),
          status: 'stopped',
          worker,
          summary: error.message,
        });
        logger.info('编排工具.dispatch_task.Worker取消', {
          worker, taskId, reason: error.message,
        }, LogCategory.ORCHESTRATOR);
        return;
      }

      const errorMsg = error?.message || String(error);

      this.messageHub.subTaskCard({
        id: taskId,
        title: task.substring(0, 40),
        status: 'failed',
        worker,
        summary: errorMsg,
      });

      this.messageHub.workerError(
        worker,
        `任务执行失败: ${errorMsg}`,
      );

      batch?.markFailed(taskId, { success: false, summary: errorMsg, errors: [errorMsg] });

      // C-15: Worker 崩溃后状态清理
      try {
        const workerInstance = this.missionOrchestrator.getWorker(worker);
        workerInstance?.clearAllSessions();
      } catch { /* 清理失败不阻塞 */ }

      logger.error('编排工具.dispatch_task.Worker失败', {
        worker, taskId, error: errorMsg,
      }, LogCategory.ORCHESTRATOR);
    });
  }

  /**
   * 配置 DispatchBatch 事件处理
   */
  private setupBatchEventHandlers(batch: DispatchBatch): void {
    // 依赖就绪 → 通过隔离策略筛选后启动 Worker
    // 约束 5：同类型 Worker 串行，不同类型并行
    batch.on('task:ready', (_taskId: string, _entry: DispatchEntry) => {
      this.dispatchReadyTasksWithIsolation(batch);
    });

    // Worker 完成后重新检查是否有同类型排队任务可启动
    batch.on('task:statusChanged', (_taskId: string, status: DispatchStatus) => {
      if (status === 'completed' || status === 'failed' || status === 'skipped' || status === 'cancelled') {
        // 延迟一个 tick，等 checkDependents 处理完毕再调度
        setImmediate(() => this.dispatchReadyTasksWithIsolation(batch));
      }
    });

    // 全部完成 → Phase C 汇总
    batch.on('batch:allCompleted', (batchId: string, entries: DispatchEntry[]) => {
      const summary = batch.getSummary();
      logger.info('DispatchBatch.全部完成', { batchId, ...summary }, LogCategory.ORCHESTRATOR);
      this.triggerPhaseCSummary(batch, entries);
    });

    // Batch 被取消 → 不触发 Phase C，直接通知用户
    batch.on('batch:cancelled', (batchId: string, reason: string) => {
      logger.info('DispatchBatch.已取消', { batchId, reason }, LogCategory.ORCHESTRATOR);
      this.messageHub.orchestratorMessage(`任务已取消: ${reason}`);
    });
  }

  /**
   * 通过 Worker 隔离策略调度就绪任务
   *
   * 同类型 Worker 串行 + 不同类型并行：
   * 每个 WorkerSlot 同一时刻最多 1 个 running 任务。
   * 当某个 Worker 完成后，再自动启动同类型的下一个排队任务。
   */
  private dispatchReadyTasksWithIsolation(batch: DispatchBatch): void {
    if (batch.status !== 'active') return;

    const readyTasks = batch.getReadyTasksIsolated();
    for (const entry of readyTasks) {
      logger.info('DispatchBatch.隔离调度.启动', {
        taskId: entry.taskId, worker: entry.worker,
      }, LogCategory.ORCHESTRATOR);
      this.launchDispatchWorker(entry.taskId, entry.worker, entry.task, entry.files);
    }
  }

  /**
   * Phase C 汇总 — 所有 Worker 完成后触发 orchestrator 汇总 LLM 调用
   *
   * 输入：用户原始需求 + 各 Worker 执行结果
   * 输出：面向用户的最终结论，流式输出到主对话区
   */
  private async triggerPhaseCSummary(batch: DispatchBatch, entries: DispatchEntry[]): Promise<void> {
    const userPrompt = batch.userPrompt || this.activeUserPrompt;
    if (!userPrompt) {
      logger.warn('Phase C 汇总: 无用户原始请求，跳过', undefined, LogCategory.ORCHESTRATOR);
      batch.archive();
      return;
    }

    try {
      this.messageHub.progress('Summarizing', '正在汇总所有 Worker 的执行结果...');

      const summaryPrompt = buildDispatchSummaryPrompt(userPrompt, entries);

      const response = await this.adapterFactory.sendMessage(
        'orchestrator',
        summaryPrompt,
        undefined,
        {
          source: 'orchestrator',
          adapterRole: 'orchestrator',
          visibility: 'system',  // Phase C 的 LLM thinking 对用户无价值，只通过 result() 展示汇总
          messageMeta: { intent: 'phase_c_summary', batchId: batch.id },
        }
      );

      this.recordOrchestratorTokens(response.tokenUsage);

      if (response.error) {
        logger.error('Phase C 汇总 LLM 失败', { error: response.error }, LogCategory.ORCHESTRATOR);
        // C-14 降级展示：直接展示 Worker 原始结果摘要
        this.phaseCFallback(entries);
      } else {
        this.messageHub.result(response.content || '');
      }
    } catch (error: any) {
      logger.error('Phase C 汇总异常', { error: error.message }, LogCategory.ORCHESTRATOR);
      this.phaseCFallback(entries);
    } finally {
      batch.archive();
    }
  }

  /**
   * Phase C 降级展示 — 汇总 LLM 失败时直接拼接 Worker 结果
   */
  private phaseCFallback(entries: DispatchEntry[]): void {
    const lines = entries.map(e => {
      const status = e.status === 'completed' ? '✅' : e.status === 'failed' ? '❌' : '⏭️';
      return `${status} **[${e.worker}]** ${e.result?.summary || '无输出'}`;
    });
    this.messageHub.result(lines.join('\n'));
  }

  // Phase B+ 中间调用频率限制：同一 batch 内最小间隔 30 秒
  private lastPhaseBPlusTimestamp: number = 0;
  private static readonly PHASE_B_PLUS_MIN_INTERVAL = 30_000;

  /**
   * Phase B+ — dispatch 模式的 Worker 上报处理
   *
   * progress 类型：更新 subTaskCard，不触发 LLM
   * question 类型：触发 orchestrator 中间 LLM 调用
   * completed/failed 类型：由 DispatchBatch 状态机处理，这里仅记录
   */
  private async handleDispatchWorkerReport(
    report: WorkerReport,
    batch: DispatchBatch | null,
  ): Promise<OrchestratorResponse> {
    const defaultResponse: OrchestratorResponse = { action: 'continue', timestamp: Date.now() };

    // progress 类型：更新 subTaskCard
    if (report.type === 'progress' && report.progress) {
      this.messageHub.subTaskCard({
        id: report.assignmentId,
        title: report.progress.currentStep?.substring(0, 40) || '',
        status: 'running',
        worker: report.workerId,
        summary: `${report.progress.percentage}% - ${report.progress.currentStep}`,
      });
      return defaultResponse;
    }

    // question 类型：触发 Phase B+ 中间 LLM 调用
    if (report.type === 'question' && report.question) {
      const now = Date.now();
      if (now - this.lastPhaseBPlusTimestamp < MissionDrivenEngine.PHASE_B_PLUS_MIN_INTERVAL) {
        logger.info('Phase B+ 频率限制，跳过中间调用', {
          worker: report.workerId,
          interval: now - this.lastPhaseBPlusTimestamp,
        }, LogCategory.ORCHESTRATOR);
        return defaultResponse;
      }

      this.lastPhaseBPlusTimestamp = now;

      try {
        const batchStatus = batch ? batch.getSummary() : { total: 0 };
        const prompt = `Worker ${report.workerId} 在执行过程中遇到问题需要决策：

## Worker 上报
${report.question.content}

## 当前 Batch 状态
${JSON.stringify(batchStatus)}

## 用户原始需求
${this.activeUserPrompt}

请决定：
1. 如果可以给出明确指令帮助 Worker 继续，请给出指令
2. 如果需要追加新的 Worker，可以调用 dispatch_task
3. 如果问题需要用户介入，请说明`;

        const response = await this.adapterFactory.sendMessage(
          'orchestrator',
          prompt,
          undefined,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            includeToolCalls: true,
            visibility: 'system',  // Phase B+ 中间决策对用户不可见
            messageMeta: { intent: 'phase_b_plus', batchId: batch?.id },
          }
        );

        this.recordOrchestratorTokens(response.tokenUsage);

        if (response.content) {
          // 将 orchestrator 的中间响应作为调整指令返回给 Worker
          return createAdjustResponse({
            newInstructions: response.content,
          });
        }
      } catch (error: any) {
        logger.error('Phase B+ 中间调用失败', { error: error.message }, LogCategory.ORCHESTRATOR);
      }

      return defaultResponse;
    }

    return defaultResponse;
  }

  /**
   * 映射到 IntentHandlerMode
   */
  private mapToHandlerMode(mode?: string): IntentHandlerMode {
    const modeMap: Record<string, IntentHandlerMode> = {
      ask: IntentHandlerMode.ASK,
      direct: IntentHandlerMode.DIRECT,
      explore: IntentHandlerMode.EXPLORE,
      task: IntentHandlerMode.TASK,
      demo: IntentHandlerMode.DEMO,
      clarify: IntentHandlerMode.CLARIFY,
    };
    return modeMap[mode ?? 'task'] || IntentHandlerMode.TASK;
  }

  private normalizeIntent(intent?: string): IntentDecision['intent'] {
    switch (intent) {
      case 'question':
      case 'trivial':
      case 'exploratory':
      case 'task':
      case 'demo':
      case 'ambiguous':
      case 'open_ended':
        return intent;
      default:
        return 'task';
    }
  }

  /**
   * 从 LLM 响应中提取意图分类 JSON（优先 fenced json，其次嵌入 JSON）
   */
  private extractIntentClassificationPayload(content: string): {
    intent?: string;
    recommendedMode?: string;
    confidence?: number;
    needsClarification?: boolean;
    clarificationQuestions?: string[];
    reason?: string;
  } | null {
    const fencedJsonRegex = /```json\s*([\s\S]*?)```/gi;
    let fencedMatch: RegExpExecArray | null = fencedJsonRegex.exec(content);
    while (fencedMatch) {
      const parsed = this.tryParseIntentClassificationPayload(fencedMatch[1]);
      if (parsed) {
        return parsed;
      }
      fencedMatch = fencedJsonRegex.exec(content);
    }

    const embeddedJsons = extractEmbeddedJson(content);
    for (const embedded of embeddedJsons) {
      const parsed = this.tryParseIntentClassificationPayload(embedded.jsonText);
      if (parsed) {
        return parsed;
      }
    }

    return this.tryParseIntentClassificationPayload(content);
  }

  private tryParseIntentClassificationPayload(candidate: string): {
    intent?: string;
    recommendedMode?: string;
    confidence?: number;
    needsClarification?: boolean;
    clarificationQuestions?: string[];
    reason?: string;
  } | null {
    try {
      const parsed = JSON.parse(candidate.trim()) as unknown;
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        return null;
      }

      const payload = parsed as Record<string, unknown>;
      const hasKey =
        typeof payload.intent === 'string' ||
        typeof payload.recommendedMode === 'string';
      if (!hasKey) {
        return null;
      }

      return {
        intent: typeof payload.intent === 'string' ? payload.intent : undefined,
        recommendedMode: typeof payload.recommendedMode === 'string' ? payload.recommendedMode : undefined,
        confidence: typeof payload.confidence === 'number' ? payload.confidence : undefined,
        needsClarification: typeof payload.needsClarification === 'boolean' ? payload.needsClarification : undefined,
        clarificationQuestions: Array.isArray(payload.clarificationQuestions)
          ? payload.clarificationQuestions.filter((q): q is string => typeof q === 'string')
          : undefined,
        reason: typeof payload.reason === 'string' ? payload.reason : undefined,
      };
    } catch {
      return null;
    }
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
   * 统一执行入口 - ReAct 模式
   *
   * 单次 LLM 调用 + 工具循环。
   * LLM 在统一系统提示词下自主决策：直接回答 / 工具操作 / 分配 Worker。
   */
  async execute(userPrompt: string, taskId: string, sessionId?: string, imagePaths?: string[]): Promise<string> {
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
      this._cachedRequirementAnalysis = null;
      this.resetSupplementaryInstructions();
      this.currentSessionId = sessionId;
      this.activeUserPrompt = trimmedPrompt;
      this.activeImagePaths = imagePaths;

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        this.currentSessionId = resolvedSessionId;
        await this.ensureContextReady(resolvedSessionId);

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
        const allProfiles = this.profileLoader.getAllProfiles();
        const availableWorkers = Array.from(allProfiles.keys());
        const workerProfiles = Array.from(allProfiles.values()).map(p => ({
          worker: p.worker,
          displayName: p.persona.displayName,
          strengths: p.persona.strengths,
        }));
        const availableToolsSummary = await this.getAvailableToolsSummary();
        const systemPrompt = buildUnifiedSystemPrompt({
          availableWorkers,
          workerProfiles,
          projectContext,
          sessionSummary: context || undefined,
          relevantADRs,
          availableToolsSummary,
        });

        // 4. 单次 LLM 调用（自动包含工具循环）
        const response = await this.adapterFactory.sendMessage(
          'orchestrator',
          trimmedPrompt,
          imagePaths,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            systemPrompt,
            includeToolCalls: true,
            messageMeta: { taskId, sessionId: resolvedSessionId, mode: 'unified' },
          }
        );

        this.recordOrchestratorTokens(response.tokenUsage);

        if (response.error) {
          throw new Error(response.error);
        }

        this.lastExecutionSuccess = true;
        this.lastExecutionErrors = [];
        this.setState('idle');
        this.currentTaskId = null;
        return response.content || '';

      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        this.lastExecutionSuccess = false;
        this.lastExecutionErrors = [errorMessage];
        logger.error('编排器.统一执行.失败', { error: errorMessage }, LogCategory.ORCHESTRATOR);
        this.setState('idle');
        this.currentTaskId = null;
        throw error;
      } finally {
        this.isRunning = false;
        // 发布任务完成/失败事件，驱动知识库自动提取、状态栏更新等
        if (this.lastExecutionSuccess) {
          globalEventBus.emitEvent('task:completed', { data: { taskId } });
        } else {
          globalEventBus.emitEvent('task:failed', { data: { taskId, error: this.lastExecutionErrors[0] } });
        }
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
   */
  async executeWithTaskContext(
    userPrompt: string,
    sessionId?: string,
    imagePaths?: string[]
  ): Promise<{ taskId: string; result: string }> {
    const result = await this.execute(userPrompt, '', sessionId, imagePaths);
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

    if (this.shouldUseOrchestratorToolingPath(requirementAnalysis)) {
      await this.missionOrchestrator.cancelMission(mission.id, 'createPlan 不适用于编排者直执工具任务');
      throw new Error('createPlan 不适用于编排者直执工具任务，请直接执行该请求');
    }

    if (!requirementAnalysis.needsWorker) {
      await this.missionOrchestrator.cancelMission(mission.id, '需求分析无效：createPlan 需要 Worker');
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

    this.isRunning = true;
    this.currentTaskId = mission.externalTaskId || taskId || null;
    this.lastMissionId = mission.id;
    this._context.mission = mission;
    this.setState('running');
    // 预置失败状态，确保 finally 中事件类型正确（只有 try 块成功才会覆盖为 true）
    this.lastExecutionSuccess = false;
    this.lastExecutionErrors = [];

    try {
      // 执行 Mission（使用 MissionOrchestrator）
      this.applySupplementaryInstructionsToMission(mission);
      const executionResult = await this.missionOrchestrator.execute(mission, {
        workingDirectory: this.workspaceRoot,
        timeout: this.config.timeout,
        parallel: plan.executionMode === 'parallel',
        onOutput: (workerId, output) => {
          this.emit('workerOutput', { workerId, output });
        },
        onReport: (report) => this.handleWorkerReport(report),
        reportTimeout: 5000,
        getSupplementaryInstructions: (workerId) => this.consumeSupplementaryInstructions(workerId),
      });

      // 记录 Worker Token 使用到 executionStats
      this.recordWorkerTokenUsage(executionResult.assignmentResults);

      // 验证和总结
      const verification = await this.missionOrchestrator.verifyMission(mission.id);
      this.lastExecutionSuccess = executionResult.success && verification.passed;
      this.lastExecutionErrors = [
        ...(executionResult.errors || []),
        ...(verification.passed ? [] : [verification.summary || '验证未通过']),
      ];
      const summary = await this.missionOrchestrator.summarizeMission(mission.id);

      return this.formatSummary(summary, this.lastExecutionSuccess, this.lastExecutionErrors);
    } finally {
      this.setState('idle');
      this.currentTaskId = null;
      this.isRunning = false;
      this._context.mission = null;
      // 发布任务完成/失败事件
      const missionTaskId = mission.externalTaskId || taskId;
      if (this.lastExecutionSuccess) {
        globalEventBus.emitEvent('task:completed', { data: { taskId: missionTaskId } });
      } else {
        globalEventBus.emitEvent('task:failed', { data: { taskId: missionTaskId, error: this.lastExecutionErrors[0] } });
      }
    }
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
    // 预置失败状态，确保异常时 emit 正确事件
    this.lastExecutionSuccess = false;
    this.lastExecutionErrors = [];
    let isPendingApproval = false;

    try {
      // 执行 Mission
      this.applySupplementaryInstructionsToMission(mission);
      const executionResult = await this.missionOrchestrator.execute(mission, {
        workingDirectory: this.workspaceRoot,
        timeout: this.config.timeout,
        getSupplementaryInstructions: (workerId) => this.consumeSupplementaryInstructions(workerId),
      });

      // 记录 Worker Token 使用到 executionStats
      this.recordWorkerTokenUsage(executionResult.assignmentResults);

      // 验证和总结
      let summaryContent = '';

      // 如果再次暂停（例如还有其他审批），则不进行验证/总结
      if (executionResult.hasPendingApprovals) {
        isPendingApproval = true;
        summaryContent = '任务部分完成，等待进一步审批。';
        this.messageHub.orchestratorMessage(summaryContent, {
          metadata: { phase: 'pending_approval' }
        });
        return summaryContent;
      }

      const verification = await this.missionOrchestrator.verifyMission(mission.id);
      this.lastExecutionSuccess = executionResult.success && verification.passed;
      this.lastExecutionErrors = [
        ...(executionResult.errors || []),
        ...(verification.passed ? [] : [verification.summary || '验证未通过']),
      ];
      const summary = await this.missionOrchestrator.summarizeMission(mission.id);
      summaryContent = this.formatSummary(summary, this.lastExecutionSuccess, this.lastExecutionErrors);

      return summaryContent;
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      this.lastExecutionSuccess = false;
      this.lastExecutionErrors = [errorMessage];
      throw error;
    } finally {
      // pending 场景：任务仍在进行中，不重置状态、不 emit 事件
      if (!isPendingApproval) {
        this.setState('idle');
        this.currentTaskId = null;
        this.isRunning = false;
        // 发布任务完成/失败事件
        const missionTaskId = mission.externalTaskId || missionId;
        if (this.lastExecutionSuccess) {
          globalEventBus.emitEvent('task:completed', { data: { taskId: missionTaskId } });
        } else {
          globalEventBus.emitEvent('task:failed', { data: { taskId: missionTaskId, error: this.lastExecutionErrors[0] } });
        }
      }
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
   * 准备决策上下文（用于意图分类/需求分析）
   *
   * 只注入“最近对话 + 长期记忆”，避免把项目知识和共享上下文带入决策阶段导致噪声。
   * 该上下文用于解析“继续/然后/接着”等省略指令。
   */
  private async prepareDecisionContext(): Promise<string> {
    const sessionId = this.currentSessionId || this.contextSessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (!sessionId) {
      return '';
    }

    await this.ensureContextReady(sessionId);

    const missionId = this._context.mission?.id || this.lastMissionId || `session:${sessionId}`;
    const options = this.contextManager.buildAssemblyOptions(missionId, 'orchestrator', 2400);
    options.localTurns = { min: 1, max: 8 };

    return this.contextManager.getAssembledContextText(options, {
      excludePartTypes: ['project_knowledge', 'shared_context', 'contracts'],
    });
  }

  /**
   * 获取可用工具摘要（供需求分析 + 统一系统提示词共用）
   * 动态加载所有已注册工具（内置 + MCP + Skill），确保 LLM 知晓全部可用能力
   */
  private async getAvailableToolsSummary(): Promise<string> {
    try {
      const toolManager = this.adapterFactory.getToolManager();
      const tools = await toolManager.getTools();
      if (tools.length === 0) {
        return '';
      }

      // 按来源分组，排除编排工具（dispatch_task 等已在 Worker 分配章节说明）
      const orchestrationToolNames = ['dispatch_task', 'plan_mission', 'send_worker_message'];
      const mcpTools = tools.filter(t => t.metadata?.source === 'mcp');
      const skillTools = tools.filter(t => t.metadata?.source === 'skill');

      const lines: string[] = [];

      // 内置工具：按类别分组，使用明确的用途说明（完整罗列全部 13 个非编排内置工具）
      lines.push('内置工具:');
      // 分类映射：工具名 → 用途说明
      const builtinToolDescriptions: Record<string, { category: string; desc: string }> = {
        'text_editor': { category: '文件操作', desc: '查看目录结构、读取/编辑/创建文件（优先使用）' },
        'grep_search': { category: '文件操作', desc: '正则搜索代码内容（优先使用）' },
        'remove_files': { category: '文件操作', desc: '删除文件' },
        'launch-process': { category: '终端命令', desc: '执行构建/测试/启动服务等进程（不要用于读文件或浏览目录）' },
        'read-process': { category: '终端命令', desc: '读取终端进程输出' },
        'write-process': { category: '终端命令', desc: '向运行中的终端写入输入' },
        'kill-process': { category: '终端命令', desc: '终止终端进程' },
        'list-processes': { category: '终端命令', desc: '列出所有终端进程' },
        'web_search': { category: '网络工具', desc: '搜索互联网信息（无需浏览器）' },
        'web_fetch': { category: '网络工具', desc: '获取 URL 页面内容（无需浏览器）' },
        'codebase_retrieval': { category: '代码智能', desc: '语义搜索代码库' },
        'mermaid_diagram': { category: '可视化', desc: '生成 Mermaid 图表' },
      };

      // 按类别分组输出
      const categoryOrder = ['文件操作', '终端命令', '网络工具', '代码智能', '可视化'];
      for (const category of categoryOrder) {
        const categoryTools = Object.entries(builtinToolDescriptions)
          .filter(([, v]) => v.category === category);
        if (categoryTools.length > 0) {
          const toolList = categoryTools.map(([name, v]) => `${name}（${v.desc}）`).join('、');
          lines.push(`- ${category}：${toolList}`);
        }
      }

      // 检查是否有未映射的内置工具（动态发现新增工具）
      const builtinTools = tools.filter(t => t.metadata?.source === 'builtin' && !orchestrationToolNames.includes(t.name));
      const unmappedTools = builtinTools.filter(t => !builtinToolDescriptions[t.name]);
      if (unmappedTools.length > 0) {
        for (const tool of unmappedTools) {
          const desc = tool.description ? tool.description.split(/[。\n]/)[0].substring(0, 60) : '';
          lines.push(`- 其他：${tool.name}（${desc}）`);
        }
      }

      // MCP 工具：动态安装的外部工具，附带描述
      if (mcpTools.length > 0) {
        lines.push('');
        lines.push('MCP 扩展工具（用户已安装，可直接调用）:');
        for (const tool of mcpTools) {
          const desc = tool.description ? ` - ${tool.description.substring(0, 80)}` : '';
          lines.push(`- ${tool.name}${desc}`);
        }
      }

      // Skill 自定义工具：动态加载的技能工具
      if (skillTools.length > 0) {
        lines.push('');
        lines.push('Skill 自定义工具（用户已安装，可直接调用）:');
        for (const tool of skillTools) {
          const desc = tool.description ? ` - ${tool.description.substring(0, 80)}` : '';
          lines.push(`- ${tool.name}${desc}`);
        }
      }

      return lines.join('\n');
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

    const missionId = this._context.mission?.id || this.lastMissionId;
    if (missionId) {
      return this.contextManager.getAssembledContextText(
        this.contextManager.buildAssemblyOptions(missionId, 'orchestrator', 8000)
      );
    }

    const defaultMissionId = sessionId ? `session:${sessionId}` : 'session:default';
    return this.contextManager.getAssembledContextText(
      this.contextManager.buildAssemblyOptions(defaultMissionId, 'orchestrator', 8000)
    );
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

    // C-09: 取消活跃的 DispatchBatch，信号链传递到所有 Worker
    if (this.activeBatch && this.activeBatch.status === 'active') {
      const runningWorkers = this.activeBatch.getEntries()
        .filter(e => e.status === 'running')
        .map(e => e.worker);

      this.activeBatch.cancelAll('用户取消');

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
    globalEventBus.emitEvent('task:failed', { data: { taskId, error: _error } });
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
    globalEventBus.emitEvent('task:completed', { data: { taskId } });
  }

  /**
   * 标记任务为执行中（仅修改状态，不触发执行链路）
   * 用于外部已自行管理执行流程的场景（如 Direct Worker 模式）
   */
  async markTaskExecuting(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'executing';
      mission.startedAt = Date.now();
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }

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
    // 删除原 draft mission，execute() 会创建完整的新 mission
    await this.missionStorage.delete(taskId);
    // 触发统一执行链路
    await this.execute(userPrompt, taskId, sessionId);
  }

  /**
   * 销毁引擎
   */
  dispose(): void {
    this.messageHub.dispose();
    this.missionOrchestrator.dispose();
    this.removeAllListeners();
    logger.info('编排器.任务引擎.销毁.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  // ============================================================================
  // 私有方法
  // ============================================================================


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

    // Phase A 决策摘要持久化：将关键规划决策写入 SharedContextPool
    this.persistPhaseADecisions(mission, participants, categories);
  }

  /**
   * 将 Phase A 规划决策持久化到 SharedContextPool
   *
   * 让 Worker 在执行时能通过上下文组装器获取编排者的全局决策，
   * 包括：任务目标、参与者分工、契约约束、风险评估。
   */
  private persistPhaseADecisions(
    mission: Mission,
    participants: WorkerSlot[],
    categories: string[]
  ): void {
    try {
      const pool = this.contextManager.getSharedContextPool();

      // 1. 任务目标与分析决策
      const goalContent = [
        `目标: ${mission.goal}`,
        `分析: ${mission.analysis}`,
        `风险等级: ${mission.riskLevel}`,
        mission.riskFactors?.length ? `风险因素: ${mission.riskFactors.join('; ')}` : '',
        `参与者: ${participants.join(', ')}`,
        `任务分类: ${categories.join(', ')}`,
      ].filter(Boolean).join('\n');

      pool.add(createSharedContextEntry({
        missionId: mission.id,
        source: 'orchestrator',
        type: 'decision',
        content: goalContent,
        tags: ['phase-a', 'goal', 'analysis'],
        importance: 'high',
      }));

      // 2. 职责分配决策
      if (mission.assignments.length > 0) {
        const assignmentContent = mission.assignments.map(a =>
          `${a.workerId}: ${a.shortTitle || a.responsibility}`
        ).join('\n');

        pool.add(createSharedContextEntry({
          missionId: mission.id,
          source: 'orchestrator',
          type: 'decision',
          content: `职责分配:\n${assignmentContent}`,
          tags: ['phase-a', 'assignment'],
          importance: 'high',
        }));
      }

      // 3. 契约约束
      if (mission.contracts.length > 0) {
        const contractContent = mission.contracts.map(c =>
          `[${c.type}] ${c.description}`
        ).join('\n');

        pool.add(createSharedContextEntry({
          missionId: mission.id,
          source: 'orchestrator',
          type: 'contract',
          content: contractContent,
          tags: ['phase-a', 'contract'],
          importance: 'critical',
        }));
      }

      logger.info('Phase A 决策已持久化到 SharedContextPool', {
        missionId: mission.id,
        participants,
      }, LogCategory.ORCHESTRATOR);
    } catch (error) {
      // 持久化失败不阻断执行
      logger.warn('Phase A 决策持久化失败', {
        missionId: mission.id,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
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
        logger.warn('编排器.上下文.压缩模型.不可用_切换编排模型', {
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
            const shouldSwitchToOrchestrator = !compressorReady
              || this.isAuthOrQuotaError(error)
              || this.isConnectionError(error)
              || this.isModelError(error)
              || this.isConfigError(error);
            if (!shouldSwitchToOrchestrator) {
              throw error;
            }
            logger.warn('编排器.上下文.压缩模型.切换_使用编排模型', {
              reason: !compressorReady ? 'not_available'
                : this.isAuthOrQuotaError(error) ? 'auth_or_quota'
                : this.isConnectionError(error) ? 'connection'
                : this.isModelError(error) ? 'model'
                : 'config',
              error: this.normalizeErrorMessage(error),
            }, LogCategory.ORCHESTRATOR);
            const orchestratorClient = createLLMClient(orchestratorConfig);
            return await sendWithRetry(orchestratorClient, 'orchestrator', message);
          }
        },
      };

      this.contextManager.setCompressorAdapter(adapter);
      const activeConfig = compressorReady ? compressorConfig : orchestratorConfig;
      logger.info('编排器.上下文.压缩模型.已设置', {
        model: activeConfig.model,
        provider: activeConfig.provider,
        useOrchestratorModel: !compressorReady,
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


  // ============================================================================
  // Phase 2: 需求分析（合并目标理解 + 路由决策）
  // ============================================================================

  /**
   * Phase 2: 需求分析
   * 一次 LLM 调用，同时输出目标理解和路由决策
   *
   * @see docs/workflow/workflow-design.md - 5 阶段工作流
   */
  private async analyzeRequirement(
    userPrompt: string,
    mode: IntentHandlerMode
  ): Promise<RequirementAnalysis> {
    const categoryHints = Array.from(this.profileLoader.getAllCategories().entries())
      .map(([name, config]) => `- ${name}: ${config.description}`)
      .join('\n');

    const sessionContext = await this.prepareDecisionContext();

    // 获取可用工具摘要，让 LLM 知道有哪些 MCP/Skill 工具可用
    const availableToolsSummary = await this.getAvailableToolsSummary();

    const prompt = buildRequirementAnalysisPrompt(userPrompt, mode, categoryHints, sessionContext, availableToolsSummary);

    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        visibility: 'system',  // 🔧 需求分析是内部决策，不应输出到 UI
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

        if (typeof parsed.needsWorker !== 'boolean') {
          throw new Error('需求分析结果无效：needsWorker 必须显式为 true 或 false');
        }
        const needsWorker = parsed.needsWorker;
        const needsTooling = Boolean(parsed.needsTooling);
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

        // 验证：非工具直执场景下，needsWorker=false 必须有 directResponse
        if (!needsWorker && !needsTooling && directResponse.length === 0) {
          throw new Error('需求分析结果无效：needsWorker=false 但缺少 directResponse');
        }
        // 验证：涉及文件修改时必须走 Worker
        if (!needsWorker && requiresModification) {
          throw new Error('需求分析结果无效：requiresModification=true 时必须 needsWorker=true');
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
          needsTooling,
          requiresModification,
          reason: parsed.reason || '需求分析完成',
        };
      }
    } catch (error) {
      logger.warn('编排器.需求分析.解析失败', { error }, LogCategory.ORCHESTRATOR);
    }

    throw new Error('需求分析解析失败');
  }

  /**
   * 判断是否应走“编排者工具直执”路径
   */
  private shouldUseOrchestratorToolingPath(requirementAnalysis: RequirementAnalysis): boolean {
    // 未标记需要工具 → 不走工具直执
    if (!requirementAnalysis.needsTooling) {
      return false;
    }

    // 涉及文件修改 → 必须走 Worker
    if (requirementAnalysis.requiresModification) {
      return false;
    }

    // 高风险任务 → 必须走 Worker
    if (requirementAnalysis.riskLevel === 'high') {
      return false;
    }

    // needsTooling=true 且无文件修改且非高风险 → 编排器直执
    return true;
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
