/**
 * Mission-Driven Engine - OrchestratorAgent 替代品
 *
 * 职责：
 * - 提供与 OrchestratorAgent 相同的公共接口
 * - 内部使用 MissionOrchestrator + MissionExecutor
 * - 支持 IntelligentOrchestrator 无缝切换
 *
 * 迁移策略：
 * - Phase 1: 创建适配层，保持接口兼容
 * - Phase 2: 替换 IntelligentOrchestrator 中的 OrchestratorAgent
 * - Phase 3: 删除旧组件
 */

import { EventEmitter } from 'events';
import path from 'path';
import { CLIAdapterFactory } from '../../cli/adapter-factory';
import { UnifiedSessionManager } from '../../session/unified-session-manager';
import { SnapshotManager } from '../../snapshot-manager';
import { ContextManager } from '../../context/context-manager';
import { UnifiedTaskManager } from '../../task/unified-task-manager';
import { logger, LogCategory } from '../../logging';
import { CLIType, PermissionMatrix, StrategyConfig, SubTask } from '../../types';
import { TokenUsage } from '../../cli/types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { PlanRecord, PlanStorage } from '../plan-storage';
import { ExecutionPlan, OrchestratorState, QuestionCallback } from '../protocols/types';
import { IntentGate, IntentHandlerMode } from '../intent-gate';
import { VerificationRunner, VerificationConfig } from '../verification-runner';
import { MissionOrchestrator } from './mission-orchestrator';
import { MissionExecutor, ExecutionProgress } from './mission-executor';
import {
  Mission,
  MissionStatus,
  MissionStorageManager,
  FileBasedMissionStorage,
} from '../mission';

/**
 * 用户确认回调类型（与 OrchestratorAgent 兼容）
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
 * 引擎配置（与 OrchestratorAgent 兼容）
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
    reviewer?: CLIType;
  };
  verification?: Partial<VerificationConfig>;
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: CLIType;
  };
  permissions?: PermissionMatrix;
  strategy?: StrategyConfig;
  cliSelection?: {
    enabled?: boolean;
    healthThreshold?: number;
  };
}

/**
 * 执行上下文（与 OrchestratorAgent 兼容）
 */
export interface MissionDrivenContext {
  plan: ExecutionPlan | null;
  mission: Mission | null;
}

/**
 * MissionDrivenEngine - 基于 Mission-Driven Architecture 的编排引擎
 */
export class MissionDrivenEngine extends EventEmitter {
  private cliFactory: CLIAdapterFactory;
  private sessionManager: UnifiedSessionManager;
  private snapshotManager: SnapshotManager;
  private contextManager: ContextManager;
  private workspaceRoot: string;
  private config: MissionDrivenEngineConfig;

  // Mission-Driven 核心组件
  private missionOrchestrator: MissionOrchestrator;
  private missionExecutor: MissionExecutor;
  private missionStorage: MissionStorageManager;
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;

  // 兼容性组件（用于 PlanRecord 转换）
  private planStorage: PlanStorage;
  private intentGate?: IntentGate;
  private verificationRunner?: VerificationRunner;

  // 状态
  private _state: OrchestratorState = 'idle';
  private _context: MissionDrivenContext = { plan: null, mission: null };
  private taskManager: UnifiedTaskManager | null = null;
  private taskManagerSessionId: string | null = null;

  // 回调
  private confirmationCallback?: ConfirmationCallback;
  private questionCallback?: QuestionCallback;
  private clarificationCallback?: ClarificationCallback;
  private workerQuestionCallback?: WorkerQuestionCallback;
  private recoveryConfirmationCallback?: RecoveryConfirmationCallback;
  private planConfirmationPolicy?: (risk: string) => boolean;

  // Token 统计
  private orchestratorTokens = { inputTokens: 0, outputTokens: 0 };

  constructor(
    cliFactory: CLIAdapterFactory,
    config: MissionDrivenEngineConfig,
    workspaceRoot: string,
    snapshotManager: SnapshotManager,
    sessionManager: UnifiedSessionManager
  ) {
    super();
    this.cliFactory = cliFactory;
    this.config = config;
    this.workspaceRoot = workspaceRoot;
    this.snapshotManager = snapshotManager;
    this.sessionManager = sessionManager;

    // 初始化基础组件
    this.profileLoader = new ProfileLoader();
    this.guidanceInjector = new GuidanceInjector();
    this.contextManager = new ContextManager(workspaceRoot);
    this.planStorage = new PlanStorage(workspaceRoot);

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

    // 初始化 Mission 执行器
    this.missionExecutor = new MissionExecutor(
      this.missionOrchestrator,
      this.profileLoader,
      this.guidanceInjector
    );

    this.setupEventForwarding();
  }

  /**
   * 设置事件转发（将 Mission 事件转换为 OrchestratorAgent 兼容事件）
   */
  private setupEventForwarding(): void {
    // Mission 状态变化 -> OrchestratorState 变化
    this.missionOrchestrator.on('missionPhaseChanged', ({ phase }) => {
      const stateMap: Record<string, OrchestratorState> = {
        goal_understanding: 'analyzing',
        collaboration_planning: 'analyzing',
        worker_planning: 'dispatching',
        plan_review: 'verifying',
        execution: 'monitoring',
        verification: 'verifying',
        summary: 'summarizing',
      };
      this._state = stateMap[phase] || 'idle';
      this.emit('stateChange', this._state);
    });

    // Mission 执行进度
    this.missionExecutor.on('progress', (progress: ExecutionProgress) => {
      this.emit('progress', progress);
    });

    // Worker 输出
    this.missionExecutor.on('workerOutput', ({ workerId, output }) => {
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
   * 获取当前上下文
   */
  get context(): MissionDrivenContext {
    return this._context;
  }

  /**
   * 初始化引擎
   */
  async initialize(): Promise<void> {
    // ProfileLoader 不需要显式加载

    // 初始化 IntentGate（使用 CLI 进行意图决策）
    const decider = async (prompt: string) => {
      const response = await this.cliFactory.sendMessage(
        'claude',
        `分析以下用户输入的意图，返回 JSON：
{
  "intent": "question|trivial|exploratory|task|ambiguous|open_ended",
  "recommendedMode": "ask|direct|explore|task|clarify",
  "confidence": 0.0-1.0,
  "needsClarification": boolean,
  "clarificationQuestions": [],
  "reason": "..."
}

用户输入: ${prompt}`,
        undefined,
        { source: 'orchestrator', adapterRole: 'orchestrator' }
      );
      this.recordOrchestratorTokens(response.tokenUsage);
      try {
        const jsonMatch = response.content?.match(/\{[\s\S]*\}/);
        if (jsonMatch) {
          const parsed = JSON.parse(jsonMatch[0]);
          return {
            intent: parsed.intent || 'task',
            recommendedMode: this.mapToHandlerMode(parsed.recommendedMode),
            confidence: parsed.confidence || 0.8,
            needsClarification: Boolean(parsed.needsClarification),
            clarificationQuestions: parsed.clarificationQuestions || [],
            reason: parsed.reason || '',
          };
        }
      } catch (e) {
        // 解析失败，默认任务模式
      }
      return {
        intent: 'task' as const,
        recommendedMode: IntentHandlerMode.TASK,
        confidence: 0.8,
        needsClarification: false,
        clarificationQuestions: [],
        reason: 'Default task mode',
      };
    };
    this.intentGate = new IntentGate(decider);

    // 初始化 VerificationRunner
    if (this.config.verification && this.config.strategy?.enableVerification) {
      this.verificationRunner = new VerificationRunner(
        this.workspaceRoot,
        this.config.verification
      );
    }

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
   * 设置任务管理器
   */
  setTaskManager(taskManager: UnifiedTaskManager, sessionId: string): void {
    this.taskManager = taskManager;
    this.taskManagerSessionId = sessionId;
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
    this._state = 'analyzing';
    this.emit('stateChange', this._state);

    try {
      const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;

      // 1. 意图分析
      const intentResult = await this.missionOrchestrator.processRequest(
        userPrompt,
        resolvedSessionId
      );

      // 2. 处理非任务模式
      if (intentResult.skipMission) {
        if (intentResult.mode === IntentHandlerMode.CLARIFY && intentResult.clarificationQuestions) {
          // 需要澄清
          if (this.clarificationCallback) {
            const answers = await this.clarificationCallback(
              intentResult.clarificationQuestions,
              '',
              0.5,
              userPrompt
            );
            if (answers) {
              // 重新执行带答案的请求
              const clarifiedPrompt = `${userPrompt}\n\n补充信息：${JSON.stringify(answers)}`;
              return this.execute(clarifiedPrompt, taskId, sessionId);
            }
          }
          return intentResult.suggestion;
        }

        if (intentResult.mode === IntentHandlerMode.ASK) {
          // 直接对话模式
          return this.executeAskMode(userPrompt, taskId, resolvedSessionId);
        }

        return intentResult.suggestion;
      }

      // 3. 创建并执行 Mission
      const mission = intentResult.mission!;
      this._context.mission = mission;

      // 4. 理解目标
      await this.understandGoalWithLLM(mission, userPrompt, resolvedSessionId);

      // 5. 规划协作
      await this.planCollaborationWithLLM(mission, resolvedSessionId);

      // 6. 用户确认（如果需要）
      if (this.planConfirmationPolicy?.('medium')) {
        const plan = this.missionToPlan(mission);
        const formatted = this.formatPlanForUser(mission);

        if (this.confirmationCallback) {
          const confirmed = await this.confirmationCallback(plan, formatted);
          if (!confirmed) {
            await this.missionOrchestrator.cancelMission(mission.id, '用户取消');
            return '任务已取消。';
          }
        }
      }

      // 7. 批准并执行
      await this.missionOrchestrator.approveMission(mission.id);

      // 8. 执行 Mission
      this._state = 'dispatching';
      this.emit('stateChange', this._state);

      await this.missionExecutor.execute(mission, {
        workingDirectory: this.workspaceRoot,
        timeout: this.config.timeout,
        onProgress: (progress) => {
          this.emit('progress', progress);
        },
        onOutput: (workerId, output) => {
          this.emit('workerOutput', { workerId, output });
        },
      });

      // 9. 验证结果
      this._state = 'verifying';
      this.emit('stateChange', this._state);

      const verificationResult = await this.missionOrchestrator.verifyMission(mission.id);

      // 10. 生成总结
      this._state = 'summarizing';
      this.emit('stateChange', this._state);

      const summary = await this.missionOrchestrator.summarizeMission(mission.id);

      this._state = 'idle';
      this.emit('stateChange', this._state);

      return this.formatSummary(summary, verificationResult.passed);

    } catch (error) {
      this._state = 'idle';
      this.emit('stateChange', this._state);
      throw error;
    }
  }

  /**
   * 仅创建计划（不执行）
   */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    const resolvedSessionId = sessionId || this.sessionManager.getCurrentSession()?.id || taskId;

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

    // 转换为 PlanRecord（兼容性）
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

    // 保存到 PlanStorage（兼容性）
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

    // 尝试从存储加载对应的 Mission
    let mission = await this.missionStorage.load(plan.id || taskId);

    if (!mission) {
      // 如果没有 Mission，从 Plan 创建一个
      mission = this.planToMission(plan, taskId, resolvedSessionId);
      await this.missionStorage.save(mission);
    }

    // 执行 Mission
    await this.missionExecutor.execute(mission, {
      workingDirectory: this.workspaceRoot,
      timeout: this.config.timeout,
    });

    // 验证和总结
    const verification = await this.missionOrchestrator.verifyMission(mission.id);
    const summary = await this.missionOrchestrator.summarizeMission(mission.id);

    return this.formatSummary(summary, verification.passed);
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
    return this.contextManager.getContext(8000);
  }

  /**
   * 记录编排器 Token 使用
   */
  recordOrchestratorTokens(usage?: TokenUsage): void {
    if (usage) {
      this.orchestratorTokens.inputTokens += usage.inputTokens || 0;
      this.orchestratorTokens.outputTokens += usage.outputTokens || 0;
    }
  }

  /**
   * 记录助手消息
   */
  async recordAssistantMessage(content: string): Promise<void> {
    // 可以在这里记录对话历史
    logger.debug('编排器.任务引擎.消息.已记录', { length: content.length }, LogCategory.ORCHESTRATOR);
  }

  /**
   * 取消执行
   */
  async cancel(): Promise<void> {
    if (this._context.mission) {
      await this.missionOrchestrator.cancelMission(this._context.mission.id, '用户取消');
    }
    this._state = 'idle';
    this.emit('stateChange', this._state);
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

  /**
   * 设置扩展上下文
   */
  setExtensionContext(_context: import('vscode').ExtensionContext): void {
    // 可以用于持久化统计数据
  }

  /**
   * 获取执行统计
   */
  getExecutionStats(): null {
    // 执行统计功能将在 Phase 6 性能优化中实现
    return null;
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
    const response = await this.cliFactory.sendMessage(
      'claude',
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
      { source: 'orchestrator', adapterRole: 'orchestrator' }
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
    // 选择参与者
    const participants = await this.missionOrchestrator.selectParticipants(mission);

    // 定义契约
    await this.missionOrchestrator.defineContracts(mission, participants);

    // 分配职责
    await this.missionOrchestrator.assignResponsibilities(mission, participants);
  }

  /**
   * 执行 Ask 模式
   */
  private async executeAskMode(
    userPrompt: string,
    taskId: string,
    sessionId: string
  ): Promise<string> {
    const context = await this.prepareContext(sessionId, userPrompt);
    const prompt = context
      ? `请结合以下会话上下文回答用户问题。\n\n${context}\n\n## 用户问题\n${userPrompt}`
      : userPrompt;

    const response = await this.cliFactory.sendMessage(
      'claude',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: { taskId, intent: 'ask' },
      }
    );

    this.recordOrchestratorTokens(response.tokenUsage);

    if (response.error) {
      throw new Error(response.error);
    }

    return response.content || '';
  }

  /**
   * Mission 转换为 ExecutionPlan（兼容性）
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
   * ExecutionPlan 转换为 Mission（兼容性）
   */
  private planToMission(plan: ExecutionPlan, taskId: string, sessionId: string): Mission {
    const now = Date.now();
    // ExecutionPlan 使用 summary 字段代替 goal
    const goal = plan.summary || plan.analysis;
    // 处理 riskLevel - Mission 的 RiskLevel 不包含 'critical'
    const riskLevel = (plan.riskLevel === 'critical' ? 'high' : (plan.riskLevel || 'low')) as 'low' | 'medium' | 'high';
    return {
      id: plan.id || taskId,
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
      assignments: plan.subTasks.map((subTask) => ({
        id: subTask.id,
        missionId: plan.id || taskId,
        workerId: subTask.assignedWorker,
        assignmentReason: {
          profileMatch: { category: 'general', score: 0.8, matchedKeywords: [] },
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
      })),
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
  private formatSummary(summary: import('./mission-orchestrator').MissionSummary, passed: boolean): string {
    const totalTodos = summary.completedTodos + summary.failedTodos + summary.skippedTodos;
    let output = `## 任务完成\n\n`;
    output += `**状态**: ${passed ? '✅ 验证通过' : '⚠️ 需要检查'}\n\n`;
    output += `**目标**: ${summary.goal}\n\n`;
    output += `**完成 Todo**: ${summary.completedTodos}/${totalTodos}\n\n`;

    if (summary.modifiedFiles.length > 0) {
      output += `### 修改的文件\n\n`;
      summary.modifiedFiles.forEach((file) => {
        output += `- ${file}\n`;
      });
    }

    return output;
  }
}
