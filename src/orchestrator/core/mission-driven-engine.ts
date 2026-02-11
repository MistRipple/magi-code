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
import { ExecutionPlan, OrchestratorState, QuestionCallback, RequirementAnalysis } from '../protocols/types';
import { IntentDecision, IntentGate, IntentHandlerMode } from '../intent-gate';
import { VerificationRunner, VerificationConfig } from '../verification-runner';
import { MissionOrchestrator } from './mission-orchestrator';
import {
  Mission,
  Assignment,
  MissionStorageManager,
  FileBasedMissionStorage,
  MissionStateMapper,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { LLMConfigLoader } from '../../llm/config';
import { MessageHub, type SubTaskCardPayload } from './message-hub';
import {
  MessageType,
  createStandardMessage,
  MessageCategory,
  MessageLifecycle,
  type ContentBlock
} from '../../protocol/message-protocol';
import type { WorkerReport, WorkerEvidence, FileChangeRecord } from '../protocols/worker-report';
import { WisdomManager, type WisdomStorage } from '../wisdom';
import { buildIntentClassificationPrompt } from '../prompts/intent-classification';
import { buildUnifiedSystemPrompt } from '../prompts/orchestrator-prompts';
import { extractEmbeddedJson } from '../../utils/content-parser';
import { isAbortError } from '../../errors';
// AutonomousWorker 和 TodoExecuteOptions 通过 MissionOrchestrator 间接使用，不直接引用
import { DispatchBatch } from './dispatch-batch';
import { createSharedContextEntry } from '../../context/shared-context-pool';
import { globalEventBus } from '../../events';
import { SupplementaryInstructionQueue } from './supplementary-instruction-queue';
import { DispatchManager } from './dispatch-manager';
import { PlanningExecutor } from './executors/planning-executor';

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
      planningExecutor: () => {
        const todoManager = this.missionOrchestrator.getTodoManager();
        if (!todoManager) {
          throw new Error('TodoManager 未初始化');
        }
        return new PlanningExecutor(todoManager);
      },
      workspaceRoot: this.workspaceRoot,
      getActiveBatch: () => this.activeBatch,
      setActiveBatch: (batch) => { this.activeBatch = batch; },
      getActiveUserPrompt: () => this.activeUserPrompt,
      getActiveImagePaths: () => this.activeImagePaths,
      getCurrentSessionId: () => this.currentSessionId,
      getLastMissionId: () => this.lastMissionId || undefined,
      getProjectKnowledgeBase: () => this.projectKnowledgeBase,
      recordOrchestratorTokens: (usage, phase) => this.recordOrchestratorTokens(usage, phase),
      recordWorkerTokenUsage: (results) => this.recordWorkerTokenUsage(results),
      getSnapshotManager: () => this.snapshotManager ?? null,
      getContextManager: () => this.contextManager ?? null,
      getTodoManager: () => this.missionOrchestrator.getTodoManager() ?? null,
    });

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
   * 将 MissionOrchestrator 的编排事件适配为 MessageHub UI 消息
   *
   * 事件通道职责分工（P0-1 修复后的 3 通道架构）：
   *
   * 通道1 - MessageHub（UI 消息统一出口）：
   *   MDE 监听 MO 事件 → 转换为 subTaskCard/notify/sendMessage → 前端渲染
   *   这是本方法的核心职责
   *
   * 通道2 - MissionOrchestrator（编排业务事件）：
   *   WebviewProvider.bindMissionEvents() 直接监听 MO 事件
   *   负责数据状态同步（todoStarted/todoCompleted → sendData → 前端 store）
   *
   * 通道3 - globalEventBus（跨模块生命周期事件）：
   *   task:completed/failed 等由 MDE.execute() 触发
   *   WebviewProvider 监听并更新全局 UI 状态
   */
  private setupEventForwarding(): void {
    // Worker 输出：路由到 MessageHub，前端通过 Worker Tab 显示
    this.missionOrchestrator.on('workerOutput', ({ workerId, output }) => {
      this.messageHub.workerOutput(workerId, output);
    });

    // 分析结果：转换为 PLAN 类型消息
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

    // 执行计划卡片：构建 Plan Card 消息
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

    // 任务开始：发送 Running 状态子任务卡片
    this.missionOrchestrator.on('assignmentStarted', ({ assignmentId }) => {
      const mission = this._context.mission;
      const assignment = mission?.assignments.find(a => a.id === assignmentId);
      if (assignment && mission) {
        const mapped = this.missionStateMapper.mapAssignmentToAssignmentView(assignment);
        
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

    // Todo 进度：统一用于所有模式（dispatch / Mission）的进度追踪
    this.missionOrchestrator.on('todoStarted', ({ assignmentId, content }) => {
      this.reportTodoProgress(assignmentId, `正在执行: ${content}`);
    });

    this.missionOrchestrator.on('todoCompleted', ({ assignmentId, content }) => {
      this.reportTodoProgress(assignmentId, `完成: ${content}`, true);
    });

    this.missionOrchestrator.on('todoFailed', ({ assignmentId, content, error }) => {
      this.reportTodoProgress(assignmentId, `失败: ${content} - ${error || '未知错误'}`);
    });

    // Worker 洞察：高优先级洞察推送给用户
    this.missionOrchestrator.on('insightGenerated', ({ workerId, type, content, importance }) => {
      const typeLabels: Record<string, string> = {
        decision: '决策', contract: '契约', risk: '风险', constraint: '约束',
      };
      const label = typeLabels[type] || type;
      const level = importance === 'critical' ? 'warning' : 'info';
      this.messageHub.notify(`[${workerId}] ${label}: ${content}`, level);
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
   * 统一报告 Todo 进度（消除 todoStarted/todoCompleted/todoFailed 的重复代码）
   *
   * 同时支持 Mission 模式和 dispatch 模式：
   * - Mission 模式：从 _context.mission 查找 assignment，使用 MissionStateMapper 映射
   * - dispatch 模式：从 activeBatch 查找 entry
   *
   * @param assignmentId 任务分配 ID
   * @param summary 进度摘要文本
   * @param includeFileChanges 是否包含文件变更信息（仅 todoCompleted 需要）
   */
  private reportTodoProgress(assignmentId: string, summary: string, includeFileChanges = false): void {
    // Mission 模式
    const mission = this._context.mission;
    const assignment = mission?.assignments.find(a => a.id === assignmentId);
    if (assignment && mission) {
      const mapped = this.missionStateMapper.mapAssignmentToAssignmentView(assignment);
      const prefix = this.buildSubTaskTitlePrefix(mission, assignment.id);
      this.messageHub.subTaskCard({
        id: mapped.id,
        title: prefix + mapped.title,
        status: 'running',
        worker: mapped.worker,
        summary,
        ...(includeFileChanges && {
          modifiedFiles: mapped.modifiedFiles,
          createdFiles: mapped.createdFiles,
        }),
      });
      return;
    }
    // dispatch 模式：从 activeBatch 查找
    const entry = this.activeBatch?.getEntry(assignmentId);
    if (entry) {
      this.messageHub.subTaskCard({
        id: assignmentId,
        title: entry.task,
        status: 'running',
        worker: entry.worker,
        summary,
      });
    }
  }

  /**
   * 发送子任务状态更新卡片（主对话区）
   */
  private emitSubTaskStatusCard(
    report: Pick<WorkerReport, 'assignmentId' | 'workerId'>,
    status: SubTaskCardPayload['status'],
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

    const mapped = this.missionStateMapper.mapAssignmentToAssignmentView(assignment);
    const subTask: SubTaskCardPayload = {
      id: mapped.id,
      title: this.buildSubTaskTitlePrefix(mission, assignment.id) + mapped.title,
      worker: mapped.worker,
      status,
      summary: summary || mapped.summary,
      ...(status === 'failed' && { error: summary || '执行失败' }),
      modifiedFiles: mapped.modifiedFiles,
      createdFiles: mapped.createdFiles,
      duration: mapped.duration,
    };

    this.messageHub.subTaskCard(subTask);
    return true;
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

    const mapped = this.missionStateMapper.mapAssignmentToAssignmentView(assignment);
    
    // 🔧 P2: 依赖链序号显示 [1/3]
    const assignmentIndex = mission.assignments.findIndex(a => a.id === assignment.id);
    const totalAssignments = mission.assignments.length;
    const prefix = totalAssignments > 1 ? `[${assignmentIndex + 1}/${totalAssignments}] ` : '';

    const subTask: SubTaskCardPayload = {
      id: mapped.id,
      title: prefix + mapped.title,
      worker: mapped.worker,
      status: statusOverride === 'completed' ? 'completed' : 'failed',
      summary: report.result?.summary || report.error || mapped.summary,
      ...(statusOverride === 'failed' && { error: report.error || report.result?.summary || '执行失败' }),
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
    await this.profileLoader.reload();
    logger.info('画像配置已重载', undefined, LogCategory.ORCHESTRATOR);
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
      this.supplementaryQueue.reset();
      this.currentSessionId = sessionId;
      this.activeUserPrompt = trimmedPrompt;
      this.activeImagePaths = imagePaths;

      try {
        const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;
        this.currentSessionId = resolvedSessionId;
        await this.ensureContextReady(resolvedSessionId);

        // 创建 Mission 记录（统一 Todo 系统：编排模式也需要 Mission 作为 Todo 的宿主）
        const mission = await this.missionStorage.createMission({
          sessionId: resolvedSessionId,
          userPrompt: trimmedPrompt,
          context: '',
        });
        this.lastMissionId = mission.id;
        mission.status = 'executing';
        mission.startedAt = Date.now();
        await this.missionStorage.update(mission);
        // 同步到 MissionOrchestrator，确保 Worker 转发的 Todo 事件能关联到正确的 Mission
        this.missionOrchestrator.setCurrentMissionId(mission.id);

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
        // 仅保留 LLM 配置中已启用（enabled=true 且 apiKey 有效）的 Worker，
        // 避免编排器将任务分配给未配置的 Worker 导致运行时错误
        const fullConfig = LLMConfigLoader.loadFullConfig();
        const availableWorkers = Array.from(allProfiles.keys())
          .filter(w => fullConfig.workers[w]?.enabled !== false);
        const workerProfiles = Array.from(allProfiles.values())
          .filter(p => availableWorkers.includes(p.worker))
          .map(p => ({
          worker: p.worker,
          displayName: p.persona.displayName,
          strengths: p.persona.strengths,
          assignedCategories: p.assignedCategories,
        }));
        const availableToolsSummary = await this.getAvailableToolsSummary();
        let systemPrompt = buildUnifiedSystemPrompt({
          availableWorkers,
          workerProfiles,
          projectContext,
          sessionSummary: context || undefined,
          relevantADRs,
          availableToolsSummary,
        });

        // 追加用户规则（buildUnifiedSystemPrompt 不含用户规则，需显式注入）
        const userRulesPrompt = this.adapterFactory.getUserRulesPrompt();
        if (userRulesPrompt) {
          systemPrompt = `${systemPrompt}\n\n${userRulesPrompt}`;
        }

        // 4. 设置编排者快照上下文（确保编排者直接工具调用也能记录快照）
        const orchestratorToolManager = this.adapterFactory.getToolManager();
        orchestratorToolManager.setSnapshotContext({
          missionId: taskId,
          assignmentId: `orchestrator-${taskId}`,
          todoId: `orchestrator-${taskId}`,
          workerId: 'orchestrator',
        });

        // 5. 单次 LLM 调用（自动包含工具循环）
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

        // 等待 dispatch batch 归档（含 Worker 执行 + Phase C 汇总）
        // dispatch_task 是非阻塞的，sendMessage 返回时 Worker 可能还在后台执行。
        // 必须等待 activeBatch 归档后再返回，确保 executeTask 的 finally 块
        // 在所有工作完成后才发出 TASK_COMPLETED 信号。
        if (this.activeBatch && this.activeBatch.status !== 'archived') {
          await this.activeBatch.waitForArchive();
        }

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
        // 中断导致的 abort 不视为执行失败，静默处理
        if (isAbortError(error)) {
          logger.info('编排器.统一执行.中断', { error: errorMessage }, LogCategory.ORCHESTRATOR);
          this.lastExecutionSuccess = false;
          this.lastExecutionErrors = [];
          this.setState('idle');
          this.currentTaskId = null;
          return '';
        }
        this.lastExecutionSuccess = false;
        this.lastExecutionErrors = [errorMessage];
        logger.error('编排器.统一执行.失败', { error: errorMessage }, LogCategory.ORCHESTRATOR);
        this.setState('idle');
        this.currentTaskId = null;
        throw error;
      } finally {
        this.isRunning = false;
        // 清除编排者快照上下文
        this.adapterFactory.getToolManager().clearSnapshotContext('orchestrator');
        // 清除 MissionOrchestrator 的 Mission ID 关联
        this.missionOrchestrator.setCurrentMissionId(null);
        // 更新 Mission 状态（统一 Todo 系统：确保编排模式的 Mission 生命周期完整）
        if (this.lastMissionId) {
          try {
            if (this.lastExecutionSuccess) {
              await this.completeTaskById(this.lastMissionId);
            } else if (this.lastExecutionErrors.length > 0) {
              await this.failTaskById(this.lastMissionId, this.lastExecutionErrors[0]);
            } else {
              // 中断场景：标记为 cancelled
              await this.cancelTaskById(this.lastMissionId);
            }
          } catch {
            // Mission 状态更新失败不影响主流程
          }
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
   * 准备决策上下文（用于意图分类/需求分析）
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

  /**
   * 格式化总结
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
