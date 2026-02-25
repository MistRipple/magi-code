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
import { IntentGate, IntentHandlerMode } from '../intent-gate';
import { VerificationRunner, VerificationConfig } from '../verification-runner';
import { MissionOrchestrator } from './mission-orchestrator';
import {
  Mission,
  MissionStorageManager,
  FileBasedMissionStorage,
} from '../mission';
import { ExecutionStats } from '../execution-stats';
import { MessageHub } from './message-hub';
import { WisdomManager, type WisdomStorage } from '../wisdom';
import { buildIntentClassificationPrompt } from '../prompts/intent-classification';
import { buildUnifiedSystemPrompt } from '../prompts/orchestrator-prompts';
import { isAbortError } from '../../errors';
import { SupplementaryInstructionQueue } from './supplementary-instruction-queue';
import { DispatchManager } from './dispatch-manager';
import { configureResilientCompressor } from './resilient-compressor-adapter';
import { TaskViewService } from '../../services/task-view-service';

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
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private categoryResolver = new CategoryResolver();

  private intentGate?: IntentGate;
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
      getLastMissionId: () => this.lastMissionId || undefined,
      getProjectKnowledgeBase: () => this.projectKnowledgeBase,
      recordOrchestratorTokens: (usage, phase) => this.recordOrchestratorTokens(usage, phase),
      recordWorkerTokenUsage: (results) => this.recordWorkerTokenUsage(results),
      getSnapshotManager: () => this.snapshotManager ?? null,
      getContextManager: () => this.contextManager ?? null,
      getTodoManager: () => this.missionOrchestrator.getTodoManager() ?? null,
      getSupplementaryQueue: () => this.supplementaryQueue,
    });
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
          const result = IntentGate.parseClassificationResponse(response.content ?? '');
          if (result) {

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

    // 初始化 VerificationRunner
    if (this.config.verification && this.config.strategy?.enableVerification) {
      this.verificationRunner = new VerificationRunner(
        this.workspaceRoot,
        this.config.verification
      );
    }

    await configureResilientCompressor(this.contextManager, this.executionStats);

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
        const enabledProfiles = this.profileLoader.getEnabledProfiles();
        const availableWorkers = Array.from(enabledProfiles.keys());
        const workerProfiles = Array.from(enabledProfiles.values())
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
        const orchestratorAssignmentId = `orchestrator-${mission.id}`;
        orchestratorToolManager.setSnapshotContext({
          missionId: mission.id,
          assignmentId: orchestratorAssignmentId,
          todoId: orchestratorAssignmentId,
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
        const currentBatch = this.dispatchManager.getActiveBatch();
        if (currentBatch && currentBatch.status !== 'archived') {
          await currentBatch.waitForArchive();
        }

        if (response.error) {
          throw new Error(response.error);
        }

        // 反应式编排兜底：若 Batch 处于“等待最终汇总”且编排者未输出正文，
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
        // 更新 Mission 生命周期
        // 核心原则：只有编排者通过 dispatch_task 主动创建的任务才保留在 Tasks 面板
        // 无 dispatch（纯对话 / 层级2直接操作 / LLM 出错 / 中断）→ 删除空 Mission
        if (this.lastMissionId) {
          try {
            const batch = this.dispatchManager.getActiveBatch();
            if (batch?.status === 'archived') {
              this.dispatchManager.markReactiveBatchSummarized(batch.id);
            }
            const hadDispatch = batch !== null;

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

    const missionId = this.lastMissionId || `session:${sessionId}`;
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

    const missionId = this.lastMissionId;
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
    await configureResilientCompressor(this.contextManager, this.executionStats);
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
    // 删除原 draft mission，execute() 会创建完整的新 mission
    await this.missionStorage.delete(taskId);
    // 触发统一执行链路
    await this.execute(userPrompt, taskId, sessionId);
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
