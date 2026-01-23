/**
 * 智能编排器 - 独立编排者架构
 *
 * 架构重构：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */

import {
  WorkerSlot,
  InteractionMode,
  INTERACTION_MODE_CONFIGS,
  InteractionModeConfig,
  PermissionMatrix,
  StrategyConfig,
  TaskStatus,
} from '../types';
import { logger, LogCategory } from '../logging';
import { IAdapterFactory } from '../adapters/adapter-factory-interface';
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
import { UnifiedSessionManager } from '../session';
import { SnapshotManager } from '../snapshot-manager';
import { globalEventBus } from '../events';
import {
  MissionDrivenEngine,
  MissionConfirmationCallback,
  MissionRecoveryConfirmationCallback,
  MissionClarificationCallback,
  MissionWorkerQuestionCallback,
} from './core';
import { VerificationRunner, VerificationConfig } from './verification-runner';
import { PlanRecord } from './plan-storage';
import {
  ExecutionPlan,
  ExecutionResult,
  SubTask,
  OrchestratorState,
  QuestionCallback,
} from './protocols/types';

// 重新导出类型，供外部引用
export type { ExecutionPlan, ExecutionResult, SubTask };
export type ConfirmationCallback = MissionConfirmationCallback;
export type RecoveryConfirmationCallback = MissionRecoveryConfirmationCallback;
export type ClarificationCallback = MissionClarificationCallback;
export type WorkerQuestionCallback = MissionWorkerQuestionCallback;

/** 子任务计划 */
export interface SubTaskPlan {
  id: string;
  description: string;
  assignedWorker: WorkerSlot;
  reason: string;
  targetFiles?: string[];
  dependencies: string[];
  prompt: string;
}

/** 编排器配置 */
export interface OrchestratorConfig {
  timeout: number;
  /** 空闲超时时间（毫秒） */
  idleTimeout?: number;
  /** 最大执行超时时间（毫秒） */
  maxTimeout?: number;
  verification?: Partial<VerificationConfig>;
  maxRetries: number;
  integration?: {
    enabled?: boolean;
    maxRounds?: number;
    worker?: WorkerSlot;
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
  permissions?: PermissionMatrix;
  strategy?: StrategyConfig;
}

/** 编排器状态 */
export type OrchestratorPhase = OrchestratorState;

const DEFAULT_CONFIG: OrchestratorConfig = {
  timeout: 300000,
  maxRetries: 3,
  integration: {
    enabled: true,
    maxRounds: 2,
    worker: 'claude',
  },
  permissions: {
    allowEdit: true,
    allowBash: true,
    allowWeb: true,
  },
  strategy: {
    enableVerification: true,
    enableRecovery: true,
    autoRollbackOnFailure: false,
  },
};

/**
 * 智能编排器 - 基于独立编排者架构
 */
export class IntelligentOrchestrator {
  private adapterFactory: IAdapterFactory;
  private taskManager: UnifiedTaskManager | null = null;
  private taskManagerSessionId: string | null = null;
  private sessionManager: UnifiedSessionManager;
  private snapshotManager: SnapshotManager;
  private config: OrchestratorConfig;
  private workspaceRoot: string;

  // 核心：MissionDrivenEngine
  private missionDrivenEngine: MissionDrivenEngine;

  // 项目知识库
  private projectKnowledgeBase?: import('../knowledge/project-knowledge-base').ProjectKnowledgeBase;

  // 交互模式 - 默认 auto，根据用户输入智能判断
  private interactionMode: InteractionMode = 'auto';
  private readonly directAnswerKeywords = [
    '是什么', '为什么', '怎么', '如何', '能否', '可以吗', '建议', '解释', '了解', '对比', '优缺点',
    '方案', '思路', '总结', '概念', '原理', '问题', '是否', '推荐',
    '你能', '你可以', '你会', '能不能', '能吗', '支持吗', '可以', '能否'
  ];
  private readonly taskIntentKeywords = [
    '实现', '添加', '新增', '修改', '修复', '重构', '迁移', '集成', '优化', '部署', '测试', '生成',
    '创建', '删除', '更新', '写', '改', '开发', '搭建', '编排', '完善'
  ];
  private modeConfig: InteractionModeConfig = INTERACTION_MODE_CONFIGS.auto;
  private recoveryConfirmationCallback: RecoveryConfirmationCallback | null = null;
  private strategyConfig: StrategyConfig;
  private permissions: PermissionMatrix;

  // 验证器
  private verificationRunner: VerificationRunner | null = null;

  // 状态
  private isRunning = false;
  private currentTaskId: string | null = null;
  private abortController: AbortController | null = null;
  private statusUpdateInterval: NodeJS.Timeout | null = null;

  constructor(
    adapterFactory: IAdapterFactory,
    sessionManager: UnifiedSessionManager,
    snapshotManager: SnapshotManager,
    workspaceRoot: string,
    config?: Partial<OrchestratorConfig>
  ) {
    this.adapterFactory = adapterFactory;
    this.sessionManager = sessionManager;
    this.snapshotManager = snapshotManager;
    this.workspaceRoot = workspaceRoot;
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.strategyConfig = this.resolveStrategyConfig();
    this.permissions = this.resolvePermissions();

    // 创建 MissionDrivenEngine
    this.missionDrivenEngine = new MissionDrivenEngine(
      adapterFactory,
      {
        timeout: this.config.timeout,
        maxRetries: this.config.maxRetries,
        review: this.config.review,
        planReview: this.config.planReview,
        verification: this.config.verification,
        integration: this.config.integration,
        permissions: this.permissions,
        strategy: this.strategyConfig,
      },
      workspaceRoot,
      snapshotManager,
      this.sessionManager
    );

    this.setupOrchestratorEvents();
    this.syncPlanConfirmationPolicy();
    this.syncRecoveryConfirmationCallback();
  }

  /** 设置编排者事件监听 */
  private setupOrchestratorEvents(): void {
    this.missionDrivenEngine.on('stateChange', (state: OrchestratorState) => {
      globalEventBus.emitEvent('orchestrator:phase_changed', {
        taskId: this.currentTaskId || undefined,
        data: { phase: state, isRunning: this.isRunning },
      });
    });
  }

  /** 设置交互模式 */
  setInteractionMode(mode: InteractionMode): void {
    this.interactionMode = mode;
    this.modeConfig = INTERACTION_MODE_CONFIGS[mode];

    // 根据模式设置工具授权回调
    const toolManager = (this.adapterFactory as any).getToolManager?.();
    if (toolManager) {
      if (mode === 'ask') {
        // Ask 模式：设置授权回调
        toolManager.setAuthorizationCallback(async (toolName: string, toolArgs: any) => {
          return await this.requestToolAuthorization(toolName, toolArgs);
        });
      } else {
        // Auto 模式：移除授权回调
        toolManager.setAuthorizationCallback(undefined);
      }
    }

    logger.info('编排器.交互_模式.变更', { mode }, LogCategory.ORCHESTRATOR);
    globalEventBus.emitEvent('orchestrator:mode_changed', { data: { mode } });
    this.syncPlanConfirmationPolicy();
    this.syncRecoveryConfirmationCallback();
  }

  /** 获取当前交互模式 */
  getInteractionMode(): InteractionMode {
    return this.interactionMode;
  }

  /** 设置用户确认回调 */
  setConfirmationCallback(callback: ConfirmationCallback): void {
    this.missionDrivenEngine.setConfirmationCallback(callback);
  }

  /** 设置用户补充问题回调 */
  setQuestionCallback(callback: QuestionCallback): void {
    this.missionDrivenEngine.setQuestionCallback(callback);
  }

  /** 设置需求澄清回调 */
  setClarificationCallback(callback: ClarificationCallback): void {
    this.missionDrivenEngine.setClarificationCallback(callback);
  }

  /** 设置 Worker 问题回调 */
  setWorkerQuestionCallback(callback: WorkerQuestionCallback): void {
    this.missionDrivenEngine.setWorkerQuestionCallback(callback);
  }

  /** 设置恢复确认回调 */
  setRecoveryConfirmationCallback(_callback: RecoveryConfirmationCallback): void {
    this.recoveryConfirmationCallback = _callback;
    this.syncRecoveryConfirmationCallback();
    this.missionDrivenEngine.setRecoveryConfirmationCallback(_callback);
  }

  /** 获取当前阶段 */
  get phase(): OrchestratorPhase {
    return this.missionDrivenEngine.state;
  }

  /** 获取当前执行计划 */
  get plan(): ExecutionPlan | null {
    return this.missionDrivenEngine.context?.plan || null;
  }

  /** 注入统一任务管理器（按会话） */
  setTaskManager(taskManager: UnifiedTaskManager, sessionId: string): void {
    this.taskManager = taskManager;
    this.taskManagerSessionId = sessionId;
    this.missionDrivenEngine.setTaskManager(taskManager, sessionId);
  }

  /** 设置项目知识库 */
  setKnowledgeBase(knowledgeBase: import('../knowledge/project-knowledge-base').ProjectKnowledgeBase): void {
    this.projectKnowledgeBase = knowledgeBase;
    // 同时注入到 MissionDrivenEngine
    this.missionDrivenEngine.setKnowledgeBase(knowledgeBase);
    logger.info('编排器.知识库.已设置', undefined, LogCategory.ORCHESTRATOR);
  }

  /** 获取项目知识库上下文 */
  private getProjectContext(maxTokens: number = 800): string {
    if (!this.projectKnowledgeBase) {
      return '';
    }
    return this.projectKnowledgeBase.getProjectContext(maxTokens);
  }

  /** 获取相关的 ADRs */
  private getRelevantADRs(userPrompt: string): string {
    if (!this.projectKnowledgeBase) {
      return '';
    }

    const adrs = this.projectKnowledgeBase.getADRs({ status: 'accepted' });
    if (adrs.length === 0) {
      return '';
    }

    // 简单的关键词匹配（未来可以使用更智能的相似度算法）
    const keywords = userPrompt.toLowerCase().split(/\s+/);
    const relevantADRs = adrs.filter(adr => {
      const adrText = `${adr.title} ${adr.context} ${adr.decision}`.toLowerCase();
      return keywords.some(keyword => adrText.includes(keyword));
    }).slice(0, 3); // 最多3个

    if (relevantADRs.length === 0) {
      return '';
    }

    const parts: string[] = ['## 相关架构决策 (ADR)'];
    relevantADRs.forEach(adr => {
      parts.push(`\n### [${adr.id}] ${adr.title}`);
      parts.push(`**背景**: ${adr.context}`);
      parts.push(`**决策**: ${adr.decision}`);
      parts.push(`**影响**: ${adr.consequences}`);
    });

    return parts.join('\n');
  }

  /** 获取相关的 FAQs */
  private getRelevantFAQs(userPrompt: string): string {
    if (!this.projectKnowledgeBase) {
      return '';
    }

    const faqs = this.projectKnowledgeBase.searchFAQs(userPrompt);
    if (faqs.length === 0) {
      return '';
    }

    const topFAQs = faqs.slice(0, 2); // 最多2个
    const parts: string[] = ['## 相关常见问题 (FAQ)'];

    topFAQs.forEach(faq => {
      parts.push(`\n**Q**: ${faq.question}`);
      parts.push(`**A**: ${faq.answer}`);
      // 增加使用次数
      this.projectKnowledgeBase?.incrementFAQUseCount(faq.id);
    });

    return parts.join('\n');
  }

  private resolveSessionId(sessionId?: string): string {
    const resolved = sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (!resolved) {
      throw new Error('未找到有效的会话 ID');
    }
    return resolved;
  }

  private async getTaskManager(sessionId?: string): Promise<UnifiedTaskManager> {
    const resolvedSessionId = this.resolveSessionId(sessionId);
    if (this.taskManager && this.taskManagerSessionId === resolvedSessionId) {
      return this.taskManager;
    }
    const repository = new SessionManagerTaskRepository(this.sessionManager, resolvedSessionId);
    const manager = new UnifiedTaskManager(resolvedSessionId, repository);
    await manager.initialize();
    this.setTaskManager(manager, resolvedSessionId);
    return manager;
  }

  private async ensureTaskExists(taskId: string, prompt: string, sessionId?: string): Promise<void> {
    const taskManager = await this.getTaskManager(sessionId);
    const existing = await taskManager.getTask(taskId);
    if (existing) {
      return;
    }
    await taskManager.createTask({ id: taskId, prompt });
  }

  private async updateTaskStatus(
    taskId: string,
    status: TaskStatus,
    sessionId?: string,
    error?: string
  ): Promise<void> {
    const taskManager = await this.getTaskManager(sessionId);
    const task = await taskManager.getTask(taskId);
    if (!task) {
      return;
    }

    if (status === 'running') {
      if (task.status !== 'running') {
        await taskManager.startTask(taskId);
      }
      return;
    }

    if (status === 'completed') {
      await taskManager.completeTask(taskId);
      return;
    }

    if (status === 'failed') {
      await taskManager.failTask(taskId, error);
      return;
    }

    if (status === 'cancelled') {
      await taskManager.cancelTask(taskId);
      return;
    }

    if (status === 'pending') {
      if (task.status !== 'pending') {
        await taskManager.updateTask(taskId, { status: 'pending' });
      }
      return;
    }

    await taskManager.updateTask(taskId, { status });
  }

  async executeWithTaskContext(userPrompt: string, sessionId?: string): Promise<{ taskId: string; result: string }> {
    const shouldAsk = this.shouldUseAskMode(userPrompt) || this.interactionMode === 'ask';
    if (shouldAsk) {
      const result = await this.execute(userPrompt, undefined, sessionId);
      return { taskId: '', result };
    }
    const taskManager = await this.getTaskManager(sessionId);
    const task = await taskManager.createTask({ prompt: userPrompt });
    const taskId = task.id;
    const result = await this.execute(userPrompt, taskId, sessionId);
    return { taskId, result };
  }

  /** 是否正在运行 */
  get running(): boolean {
    return this.isRunning;
  }

  /** 中断当前任务 */
  async interrupt(): Promise<void> {
    await this.cancel();
  }


  /** 初始化编排者 */
  async initialize(): Promise<void> {
    await this.missionDrivenEngine.initialize();

    if (this.config.verification && this.strategyConfig.enableVerification) {
      this.verificationRunner = new VerificationRunner(
        this.workspaceRoot,
        this.config.verification
      );
    }
  }

  /**
   * 重新加载画像配置
   */
  async reloadProfiles(): Promise<void> {
    await this.missionDrivenEngine.reloadProfiles();
  }

  /**
   * 执行任务 - 主入口
   */
  async execute(userPrompt: string, taskId?: string, sessionId?: string): Promise<string> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;
    const shouldAsk = this.shouldUseAskMode(userPrompt) || this.interactionMode === 'ask';
    if (shouldAsk) {
      try {
        return await this.executeAskMode(userPrompt, taskId, sessionId);
      } finally {
        this.isRunning = false;
        this.stopStatusUpdates();
        this.abortController = null;
        this.currentTaskId = null;
      }
    }
    if (!taskId) {
      const taskManager = await this.getTaskManager(sessionId);
      const task = await taskManager.createTask({ prompt: userPrompt });
      taskId = task.id;
    } else {
      await this.ensureTaskExists(taskId, userPrompt, sessionId);
    }
    this.currentTaskId = taskId;
    this.abortController = new AbortController();

    await this.updateTaskStatus(taskId, 'running', sessionId);
    globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
    this.startStatusUpdates(taskId);

    try {
      // agent/auto 模式：使用 MissionDrivenEngine 执行
      const result = await this.missionDrivenEngine.execute(userPrompt, taskId, sessionId);
      const execStatus = this.missionDrivenEngine.getLastExecutionStatus();

      if (this.abortController?.signal.aborted) {
        await this.updateTaskStatus(taskId, 'cancelled', sessionId);
        globalEventBus.emitEvent('task:cancelled', { taskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      if (execStatus.success) {
        await this.updateTaskStatus(taskId, 'completed', sessionId);
        globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });
      } else {
        const errorMsg = execStatus.errors.join('; ') || '任务执行失败';
        await this.updateTaskStatus(taskId, 'failed', sessionId, errorMsg);
        globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      }

      return result;

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);

      if (this.abortController?.signal.aborted) {
        await this.updateTaskStatus(taskId, 'cancelled', sessionId);
        globalEventBus.emitEvent('task:cancelled', { taskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      if (this.modeConfig.autoRollbackOnFailure && this.strategyConfig.autoRollbackOnFailure) {
        const count = this.snapshotManager.revertAllChanges();
        logger.info('编排器.回滚.自动.完成', { count }, LogCategory.ORCHESTRATOR);
      }

      await this.updateTaskStatus(taskId, 'failed', sessionId, errorMsg);
      globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      throw error;

    } finally {
      this.isRunning = false;
      this.stopStatusUpdates();
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  /** 仅生成执行计划（不执行） */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;
    if (!taskId) {
      const taskManager = await this.getTaskManager(sessionId);
      const task = await taskManager.createTask({ prompt: userPrompt });
      taskId = task.id;
    } else {
      await this.ensureTaskExists(taskId, userPrompt, sessionId);
    }
    this.currentTaskId = taskId;
    this.abortController = new AbortController();

    await this.updateTaskStatus(taskId, 'running', sessionId);
    globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
    this.startStatusUpdates(taskId);

    try {
      const record = await this.missionDrivenEngine.createPlan(userPrompt, taskId, sessionId);
      const taskManager = await this.getTaskManager(sessionId);
      await taskManager.updateTaskPlan(taskId, {
        planId: record.id,
        planSummary: record.plan.summary || record.plan.analysis || '执行计划',
        status: 'ready',
      });
      await this.updateTaskStatus(taskId, 'pending', sessionId);
      return record;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      await this.updateTaskStatus(taskId, 'failed', sessionId, errorMsg);
      globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      throw error;
    } finally {
      this.isRunning = false;
      this.stopStatusUpdates();
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  /** 使用已生成的计划执行 */
  async executePlan(record: PlanRecord, taskId?: string, sessionId?: string): Promise<string> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;
    const finalTaskId = taskId || record.taskId;
    await this.ensureTaskExists(finalTaskId, record.prompt || '', sessionId || record.sessionId);
    this.currentTaskId = finalTaskId;
    this.abortController = new AbortController();

    await this.updateTaskStatus(finalTaskId, 'running', sessionId || record.sessionId);
    globalEventBus.emitEvent('task:started', { taskId: finalTaskId, data: { isRunning: true } });
    this.startStatusUpdates(finalTaskId);

    try {
      const taskManager = await this.getTaskManager(sessionId || record.sessionId);
      await taskManager.updateTaskPlan(finalTaskId, {
        planId: record.id,
        planSummary: record.plan.summary || record.plan.analysis || '执行计划',
        status: 'executing',
      });

      const result = await this.missionDrivenEngine.executePlan(
        record.plan,
        finalTaskId,
        sessionId || record.sessionId,
        record.prompt
      );
      const execStatus = this.missionDrivenEngine.getLastExecutionStatus();
      if (this.abortController?.signal.aborted) {
        await this.updateTaskStatus(finalTaskId, 'cancelled', sessionId || record.sessionId);
        globalEventBus.emitEvent('task:cancelled', { taskId: finalTaskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      if (execStatus.success) {
        await taskManager.updateTaskPlanStatus(finalTaskId, 'completed');
        await this.updateTaskStatus(finalTaskId, 'completed', sessionId || record.sessionId);
        globalEventBus.emitEvent('task:completed', { taskId: finalTaskId, data: { isRunning: false } });
      } else {
        const errorMsg = execStatus.errors.join('; ') || '任务执行失败';
        await taskManager.updateTaskPlanStatus(finalTaskId, 'failed');
        await this.updateTaskStatus(finalTaskId, 'failed', sessionId || record.sessionId, errorMsg);
        globalEventBus.emitEvent('task:failed', { taskId: finalTaskId, data: { error: errorMsg, isRunning: false } });
      }
      return result;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      const taskManager = await this.getTaskManager(sessionId || record.sessionId);
      await taskManager.updateTaskPlanStatus(finalTaskId, 'failed');
      await this.updateTaskStatus(finalTaskId, 'failed', sessionId || record.sessionId, errorMsg);
      globalEventBus.emitEvent('task:failed', { taskId: finalTaskId, data: { error: errorMsg, isRunning: false } });
      throw error;
    } finally {
      this.isRunning = false;
      this.stopStatusUpdates();
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  getActivePlanForSession(sessionId: string): PlanRecord | null {
    return this.missionDrivenEngine.getActivePlanForSession(sessionId);
  }

  getLatestPlanForSession(sessionId: string): PlanRecord | null {
    return this.missionDrivenEngine.getLatestPlanForSession(sessionId);
  }

  getPlanById(planId: string, sessionId: string): PlanRecord | null {
    return this.missionDrivenEngine.getPlanById(planId, sessionId);
  }

  /** ask 模式：仅对话 */
  private async executeAskMode(userPrompt: string, taskId?: string, sessionId?: string): Promise<string> {
    logger.info('编排器.执行.对话_模式', undefined, LogCategory.ORCHESTRATOR);

    const contextSessionId = sessionId || taskId || this.sessionManager.getCurrentSession()?.id || '';
    const taskManager = contextSessionId ? await this.getTaskManager(contextSessionId) : null;
    const task = taskManager && taskId ? await taskManager.getTask(taskId) : null;
    if (taskId) {
      if (task) {
        await this.updateTaskStatus(taskId, 'running', sessionId);
      }
      globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
      this.startStatusUpdates(taskId);
    }
    const context = await this.missionDrivenEngine.prepareContext(contextSessionId, userPrompt);

    // 获取项目知识库上下文
    const projectContext = this.getProjectContext(500);
    const relevantADRs = this.getRelevantADRs(userPrompt);
    const relevantFAQs = this.getRelevantFAQs(userPrompt);

    // 构建增强的提示词
    const knowledgeParts: string[] = [];
    if (context) {
      knowledgeParts.push(`## 会话上下文\n${context}`);
    }
    if (projectContext) {
      knowledgeParts.push(`\n## 项目信息\n${projectContext}`);
    }
    if (relevantADRs) {
      knowledgeParts.push(`\n${relevantADRs}`);
    }
    if (relevantFAQs) {
      knowledgeParts.push(`\n${relevantFAQs}`);
    }

    const prompt = knowledgeParts.length > 0
      ? `请结合以下信息回答用户问题。\n\n${knowledgeParts.join('\n')}\n\n## 用户问题\n${userPrompt}`
      : userPrompt;
    const snapshot = context ? this.truncateSnapshot(context) : undefined;

    const response = await this.adapterFactory.sendMessage(
      'orchestrator',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: {
          taskId: taskId || '',
          intent: 'ask',
          contextSnapshot: snapshot,
        },
      }
    );

    this.missionDrivenEngine.recordOrchestratorTokens(response.tokenUsage);

    if (response.error) {
      if (taskId) {
        if (task) {
          await this.updateTaskStatus(taskId, 'failed', sessionId, response.error);
        }
        globalEventBus.emitEvent('task:failed', { taskId, data: { error: response.error, isRunning: false } });
      }
      throw new Error(response.error);
    }

    if (taskId) {
      if (task) {
        await this.updateTaskStatus(taskId, 'completed', sessionId);
      }
      globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });
    }

    const content = response.content || '';
    await this.missionDrivenEngine.recordAssistantMessage(content);
    return content;
  }

  private truncateSnapshot(context: string, maxChars: number = 6000): string {
    const trimmed = context.trim();
    if (trimmed.length <= maxChars) return trimmed;
    return trimmed.slice(0, maxChars) + '\n...';
  }

  private resolvePermissions(): PermissionMatrix {
    return {
      allowEdit: this.config.permissions?.allowEdit ?? true,
      allowBash: this.config.permissions?.allowBash ?? true,
      allowWeb: this.config.permissions?.allowWeb ?? true,
    };
  }

  private resolveStrategyConfig(): StrategyConfig {
    return {
      enableVerification: this.config.strategy?.enableVerification ?? true,
      enableRecovery: this.config.strategy?.enableRecovery ?? true,
      autoRollbackOnFailure: this.config.strategy?.autoRollbackOnFailure ?? false,
    };
  }

  private shouldUseAskMode(prompt: string): boolean {
    const trimmed = prompt.trim();
    if (!trimmed) return true;
    if (this.interactionMode === 'ask') return true;
    if (trimmed.startsWith('/agent') || trimmed.startsWith('/task')) return false;

    const lower = trimmed.toLowerCase();
    if (lower.includes('```') || /[\\/].+\.\w+/.test(lower)) return false;

    const hasTaskIntent = this.taskIntentKeywords.some(k => trimmed.includes(k));
    const hasBuildVerb = /(做|制作|搭建|实现|开发|修复|重构|新增|优化|编写|添加|修改)/.test(trimmed);
    const hasBuildTarget = /(功能|页面|模块|接口|系统|组件|服务|项目|API|后端|前端|UI|界面)/i.test(trimmed);
    const capabilityPattern = /(你能|你可以|你会|能不能|能否|是否|可以|支持)/;
    const endsWithQuestionWord = /(吗|么|？|\?)$/.test(trimmed);
    const hasCapabilityQuestion = capabilityPattern.test(trimmed)
      && (endsWithQuestionWord || /(能做|能否做|可以做)/.test(trimmed))
      && !hasBuildTarget
      && !/(代码|文件|改动|实现|开发|修复|重构|新增|优化)/.test(trimmed);

    if (hasCapabilityQuestion) return true;
    const hasStructuredTaskIntent = hasTaskIntent || (hasBuildVerb && hasBuildTarget);
    if (hasStructuredTaskIntent) return false;

    const hasQuestion = trimmed.includes('?') || trimmed.includes('？');
    const hasDirectAnswerIntent = this.directAnswerKeywords.some(k => trimmed.includes(k));
    const shortPrompt = trimmed.length <= 50;

    return hasQuestion || hasDirectAnswerIntent || shortPrompt;
  }

  /** 取消当前任务 */
  async cancel(): Promise<void> {
    logger.info('编排器.任务.取消.请求', undefined, LogCategory.ORCHESTRATOR);

    // 1. 触发 AbortController
    this.abortController?.abort();

    // 2. 取消 MissionDrivenEngine 中的任务
    await this.missionDrivenEngine.cancel();

    // 3. 停止状态更新定时器
    this.stopStatusUpdates();

    if (this.currentTaskId) {
      await this.updateTaskStatus(this.currentTaskId, 'cancelled');
      globalEventBus.emitEvent('task:cancelled', { taskId: this.currentTaskId, data: { isRunning: false } });
    }

    // 4. 重置状态标志
    this.isRunning = false;
    this.abortController = null;
    this.currentTaskId = null;

    logger.info('编排器.任务.取消.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  /** 开始状态更新定时器 */
  private startStatusUpdates(taskId: string): void {
    this.stopStatusUpdates();
    this.statusUpdateInterval = setInterval(() => {
      if (this.isRunning) {
        globalEventBus.emitEvent('orchestrator:phase_changed', {
          taskId,
          data: { phase: this.missionDrivenEngine.state, isRunning: true },
        });
      }
    }, 2000);
  }

  /** 停止状态更新定时器 */
  private stopStatusUpdates(): void {
    if (this.statusUpdateInterval) {
      clearInterval(this.statusUpdateInterval);
      this.statusUpdateInterval = null;
    }
  }

  private syncPlanConfirmationPolicy(): void {
    this.missionDrivenEngine.setPlanConfirmationPolicy((_risk) => {
      if (!this.modeConfig.requirePlanConfirmation) return false;
      return true;
    });
  }

  private syncRecoveryConfirmationCallback(): void {
    const userCallback = this.recoveryConfirmationCallback;
    this.missionDrivenEngine.setRecoveryConfirmationCallback(async (failedTask, error, options) => {
      if (!this.strategyConfig.enableRecovery) {
        return 'continue';
      }
      if (this.modeConfig.autoRollbackOnFailure && this.strategyConfig.autoRollbackOnFailure && options.rollback) {
        return 'rollback';
      }
      if (!this.modeConfig.requireRecoveryConfirmation) {
        if (options.retry) return 'retry';
        if (options.rollback) return 'rollback';
        return 'continue';
      }
      return userCallback
        ? userCallback(failedTask, error, options)
        : (options.retry ? 'retry' : options.rollback ? 'rollback' : 'continue');
    });
  }

  /** 获取可用的 Worker 列表 */
  getAvailableWorkers(): WorkerSlot[] {
    return ['claude', 'codex', 'gemini'];
  }

  /** 获取执行统计摘要 */
  getStatsSummary(): string {
    return this.missionDrivenEngine.getStatsSummary();
  }

  getOrchestratorTokenUsage(): { inputTokens: number; outputTokens: number } {
    return this.missionDrivenEngine.getOrchestratorTokenUsage();
  }

  resetOrchestratorTokenUsage(): void {
    this.missionDrivenEngine.resetOrchestratorTokenUsage();
  }

  /** 设置扩展上下文（用于持久化统计数据） */
  setExtensionContext(context: import('vscode').ExtensionContext): void {
    this.missionDrivenEngine.setExtensionContext(context);
  }

  /** 获取执行统计实例（用于 UI 显示） */
  getExecutionStats(): import('./execution-stats').ExecutionStats | null {
    return this.missionDrivenEngine.getExecutionStats();
  }

  /**
   * 请求工具授权（Ask 模式）
   */
  private async requestToolAuthorization(toolName: string, toolArgs: any): Promise<boolean> {
    // 发送授权请求到前端
    return new Promise((resolve) => {
      globalEventBus.emitEvent('tool:authorization_request', {
        data: {
          toolName,
          toolArgs,
          callback: (allowed: boolean) => {
            resolve(allowed);
          },
        },
      });
    });
  }

  /** 销毁编排器 */
  dispose(): void {
    this.stopStatusUpdates();
    this.missionDrivenEngine.dispose();
    logger.info('编排器.销毁.完成', undefined, LogCategory.ORCHESTRATOR);
  }
}
