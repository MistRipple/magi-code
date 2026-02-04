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
import { ExecutionPlan, OrchestratorState, QuestionCallback } from '../protocols/types';
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
import type { WorkerReport, OrchestratorResponse, WorkerEvidence, FileChangeRecord } from '../protocols/worker-report';
import { WisdomManager, type WisdomStorage } from '../wisdom';
import { buildIntentClassificationPrompt } from '../prompts/intent-classification';
import { buildWorkerNeedDecisionPrompt } from '../prompts/orchestrator-prompts';

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
    this.missionOrchestrator.on('workerOutput', ({ workerId, output }) => {
      this.emit('workerOutput', { workerId, output });
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

  /**
   * 设置任务管理器
   * 用于同步 Mission.assignments 到 SubTasks，确保 UI 能正确显示 Worker 信息
   * @param taskManager UnifiedTaskManager 实例
   */
  setTaskManager(taskManager: import('../../task/unified-task-manager').UnifiedTaskManager): void {
    // 传递给 MissionOrchestrator，由其在执行时同步 Assignment 到 SubTask
    this.missionOrchestrator.setTaskManager(taskManager);
    logger.info('引擎.任务管理器.设置', undefined, LogCategory.ORCHESTRATOR);
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
   * 发送任务分配说明到对应 Worker Tab
   *
   * 优先使用 AI 生成的自然语言委托说明 (delegationBriefing)
   * 如果没有，则使用用户原始需求作为任务描述
   */
  private sendWorkerDispatchMessage(mission: Mission, assignment: Assignment): void {
    // 优先使用 AI 生成的自然语言委托说明
    let content: string;
    if (assignment.delegationBriefing) {
      content = assignment.delegationBriefing;
    } else {
      // 兜底：使用用户原始需求
      content = mission.userPrompt || mission.goal;
    }

    this.messageHub.orchestratorMessage(content, {
      metadata: {
        dispatchToWorker: true,
        worker: assignment.workerId,
        assignmentId: assignment.id,
        missionId: mission.id,
      },
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
      return baseResponse;
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
      this.messageHub.workerOutput(report.workerId, summary, {
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
      this.messageHub.workerOutput(report.workerId, `执行遇到问题：${error}`, {
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
    const subTask: MessageHubSubTaskView = {
      id: mapped.id,
      title: mapped.title,
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
            streamToUI: false,
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
          // **DIRECT 模式**: 由编排者 LLM 决策是否需要 Worker
          logger.info('编排器.执行.DIRECT模式', { prompt: activePrompt.substring(0, 50) }, LogCategory.ORCHESTRATOR);
          const decision = await this.decideWorkerNeedWithLLM(
            activePrompt,
            IntentHandlerMode.DIRECT
          );
          logger.info('编排器.DIRECT决策结果', {
            needsWorker: decision.needsWorker,
            hasDirectResponse: !!decision.directResponse?.trim(),
            directResponseLength: decision.directResponse?.length || 0,
            reason: decision.reason,
          }, LogCategory.ORCHESTRATOR);
          if (!decision.needsWorker) {
            if (decision.directResponse?.trim()) {
              logger.info('编排器.DIRECT模式.发送响应', {
                contentLength: decision.directResponse.length,
                contentPreview: decision.directResponse.substring(0, 100),
              }, LogCategory.ORCHESTRATOR);
              this.messageHub.result(decision.directResponse, {
                metadata: { intent: 'ask', decision: 'llm' },
              });
              this.setState('idle');
              this.currentTaskId = null;
              return decision.directResponse;
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
          // 需要 Worker：走完整 Mission 流程
          this.lastRoutingDecision = decision;
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
          // **EXPLORE 模式**: 由编排者 LLM 决策是否需要 Worker
          logger.info('编排器.执行.EXPLORE模式', { prompt: activePrompt.substring(0, 50) }, LogCategory.ORCHESTRATOR);
          const decision = await this.decideWorkerNeedWithLLM(
            activePrompt,
            IntentHandlerMode.EXPLORE
          );
          logger.info('编排器.EXPLORE决策结果', {
            needsWorker: decision.needsWorker,
            hasDirectResponse: !!decision.directResponse?.trim(),
            directResponseLength: decision.directResponse?.length || 0,
            reason: decision.reason,
          }, LogCategory.ORCHESTRATOR);
          if (!decision.needsWorker) {
            if (decision.directResponse?.trim()) {
              logger.info('编排器.EXPLORE模式.发送响应', {
                contentLength: decision.directResponse.length,
                contentPreview: decision.directResponse.substring(0, 100),
              }, LogCategory.ORCHESTRATOR);
              this.messageHub.result(decision.directResponse, {
                metadata: { intent: 'ask', decision: 'llm' },
              });
              this.setState('idle');
              this.currentTaskId = null;
              return decision.directResponse;
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
          // 需要 Worker：走完整 Mission 流程
          this.lastRoutingDecision = decision;
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

      // 4. 理解目标
      await this.understandGoalWithLLM(mission, activePrompt, resolvedSessionId);

      // 5. 规划协作
      if (!this.lastRoutingDecision) {
        const routingDecision = await this.decideWorkerNeedWithLLM(
          activePrompt,
          IntentHandlerMode.TASK
        );
        if (!routingDecision.needsWorker || !routingDecision.category) {
          throw new Error('编排器路由决策无效：TASK 模式必须解析分类');
        }
        this.lastRoutingDecision = routingDecision;
      }
      await this.planCollaborationWithLLM(mission, resolvedSessionId);

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

      // 7. 批准并执行
      await this.missionOrchestrator.approveMission(mission.id);

      // 8. 执行 Mission

      const analysis = this.lastTaskAnalysis as unknown as {
        wantsParallel?: boolean;
        explicitWorkers?: WorkerSlot[];
        suggestedMode?: 'sequential' | 'parallel';
      } | null;
      const wantsParallel = Boolean(
        analysis?.wantsParallel
        || (analysis?.explicitWorkers?.length || 0) > 1
        || analysis?.suggestedMode === 'parallel'
      );

      // 使用 MissionOrchestrator.execute()（MissionExecutor 已合并）
      const executionResult = await this.missionOrchestrator.execute(mission, {
        workingDirectory: this.workspaceRoot,
        timeout: this.config.timeout,
        parallel: wantsParallel,
        onProgress: (progress) => {
          this.emit('progress', progress);
        },
        onOutput: (workerId, output) => {
          this.emit('workerOutput', { workerId, output });
        },
        onReport: (report) => this.handleWorkerReport(report),
        reportTimeout: 5000,
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

    // 理解目标并规划
    await this.understandGoalWithLLM(mission, userPrompt, resolvedSessionId);
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
    const executionResult = await this.missionOrchestrator.execute(mission, {
      workingDirectory: this.workspaceRoot,
      timeout: this.config.timeout,
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
  getExecutionStats(): ExecutionStats {
    return this.executionStats;
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
   */
  private async understandGoalWithLLM(
    mission: Mission,
    userPrompt: string,
    _sessionId: string
  ): Promise<void> {
    // 使用 Claude 分析用户请求
    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      `分析以下用户请求，提取：
1. 目标（goal）：用户想要达成什么
2. 分析（analysis）：任务的复杂度和关键点
3. 约束（constraints）：任何限制条件
4. 验收标准（acceptanceCriteria）：如何判断任务完成

用户请求：${userPrompt}

请以 JSON 格式返回：
{
  "goal": "...",
  "analysis": "...",
  "constraints": ["..."],
  "acceptanceCriteria": ["..."],
  "riskLevel": "low|medium|high",
  "riskFactors": ["..."]
}`,
      undefined,
      { source: 'orchestrator', adapterRole: 'orchestrator', streamToUI: false }
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
          streamToUI: false,
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

    if (response.content && response.content.trim()) {
      return response.content;
    }

    logger.info('编排器.ASK模式.尝试fallback', undefined, LogCategory.ORCHESTRATOR);

    const fallbackResponse = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: false,
        adapterRole: 'orchestrator',
        messageMeta: { taskId, intent: 'ask', fallback: true },
      }
    );
    this.recordOrchestratorTokens(fallbackResponse.tokenUsage);
    if (fallbackResponse.error) {
      throw new Error(fallbackResponse.error);
    }

    const fallbackContent = fallbackResponse.content || '';
    logger.info('编排器.ASK模式.fallback响应', {
      hasContent: !!fallbackContent.trim(),
      contentLength: fallbackContent.length,
    }, LogCategory.ORCHESTRATOR);

    if (fallbackContent.trim()) {
      const requestId = this.messageHub.getRequestContext();
      const stats = requestId ? this.messageHub.getRequestMessageStats(requestId) : undefined;
      const hasAssistantOutput = (stats?.assistantThreadContent ?? 0) > 0;
      logger.info('编排器.ASK模式.检查是否需要强制发送', {
        requestId,
        assistantThreadContent: stats?.assistantThreadContent ?? 0,
        hasAssistantOutput,
      }, LogCategory.ORCHESTRATOR);
      if (!hasAssistantOutput) {
        logger.info('编排器.ASK模式.强制发送fallback消息', {
          contentLength: fallbackContent.length,
        }, LogCategory.ORCHESTRATOR);
        this.messageHub.orchestratorMessage(fallbackContent, {
          metadata: { intent: 'ask', reason: 'fallback', forced: true },
        });
      }
    }

    return fallbackContent;
  }

  /**
   * Mission 转换为 ExecutionPlan（用于 UI 状态展示）
   */
  private missionToPlan(mission: Mission): ExecutionPlan {
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
      executionMode: 'sequential',
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
