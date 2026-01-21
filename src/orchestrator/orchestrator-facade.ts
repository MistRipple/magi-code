/**
 * Orchestrator Facade - 编排器门面
 *
 * 职责：
 * - 组合所有模块
 * - 提供统一的公共 API
 * - 管理生命周期
 * - 事件管理和状态更新
 */

import { CLIType, InteractionMode, PermissionMatrix, StrategyConfig } from '../types';
import { logger, LogCategory } from '../logging';
import { IAdapterFactory } from '../adapters/adapter-factory-interface';
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

// 导入新模块
import { ConfigResolver, OrchestratorConfig, SubTaskPlan } from './config-resolver';
import { TaskContextManager } from './task-context-manager';
import { InteractionModeManager } from './interaction-mode-manager';
import { PlanCoordinator } from './plan-coordinator';
import { ExecutionCoordinator } from './execution-coordinator';
import { UnifiedTaskManager } from '../task/unified-task-manager';

// 重新导出类型，供外部引用
export type { ExecutionPlan, ExecutionResult, SubTask, SubTaskPlan, OrchestratorConfig };
export type ConfirmationCallback = MissionConfirmationCallback;
export type RecoveryConfirmationCallback = MissionRecoveryConfirmationCallback;
export type ClarificationCallback = MissionClarificationCallback;
export type WorkerQuestionCallback = MissionWorkerQuestionCallback;

/** 编排器阶段 */
export type OrchestratorPhase = OrchestratorState;

/**
 * 智能编排器 - 基于独立编排者架构
 */
export class IntelligentOrchestrator {
  private adapterFactory: IAdapterFactory;
  private sessionManager: UnifiedSessionManager;
  private snapshotManager: SnapshotManager;
  private config: OrchestratorConfig;
  private workspaceRoot: string;
  private strategyConfig: StrategyConfig;
  private permissions: PermissionMatrix;

  // 核心：MissionDrivenEngine
  private missionDrivenEngine: MissionDrivenEngine;

  // 验证器
  private verificationRunner: VerificationRunner | null = null;

  // 新模块
  private taskContextManager: TaskContextManager;
  private interactionModeManager: InteractionModeManager;
  private planCoordinator: PlanCoordinator;
  private executionCoordinator: ExecutionCoordinator;

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
    this.config = ConfigResolver.resolveConfig(config);
    this.strategyConfig = ConfigResolver.resolveStrategyConfig(this.config);
    this.permissions = ConfigResolver.resolvePermissions(this.config);

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
        cliSelection: this.config.cliSelection,
      },
      workspaceRoot,
      snapshotManager,
      this.sessionManager
    );

    // 创建新模块
    this.taskContextManager = new TaskContextManager(sessionManager);
    this.interactionModeManager = new InteractionModeManager(this.strategyConfig);
    this.planCoordinator = new PlanCoordinator(this.missionDrivenEngine, this.taskContextManager);
    this.executionCoordinator = new ExecutionCoordinator(
      adapterFactory,
      this.missionDrivenEngine,
      snapshotManager,
      this.taskContextManager,
      this.interactionModeManager,
      sessionManager,
      this.strategyConfig.autoRollbackOnFailure
    );

    this.setupOrchestratorEvents();
    this.interactionModeManager.syncPlanConfirmationPolicy(this.missionDrivenEngine);
    this.interactionModeManager.syncRecoveryConfirmationCallback(this.missionDrivenEngine);
  }

  /** 设置编排者事件监听 */
  private setupOrchestratorEvents(): void {
    this.missionDrivenEngine.on('stateChange', (state: OrchestratorState) => {
      globalEventBus.emitEvent('orchestrator:phase_changed', {
        taskId: this.executionCoordinator.getCurrentTaskId() || undefined,
        data: { phase: state, isRunning: this.executionCoordinator.running },
      });
    });
  }

  /** 设置交互模式 */
  setInteractionMode(mode: InteractionMode): void {
    this.interactionModeManager.setInteractionMode(mode);
    this.interactionModeManager.syncPlanConfirmationPolicy(this.missionDrivenEngine);
    this.interactionModeManager.syncRecoveryConfirmationCallback(this.missionDrivenEngine);
  }

  /** 获取当前交互模式 */
  getInteractionMode(): InteractionMode {
    return this.interactionModeManager.getInteractionMode();
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
  setRecoveryConfirmationCallback(callback: RecoveryConfirmationCallback): void {
    this.interactionModeManager.setRecoveryConfirmationCallback(callback);
    this.interactionModeManager.syncRecoveryConfirmationCallback(this.missionDrivenEngine);
    this.missionDrivenEngine.setRecoveryConfirmationCallback(callback);
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
    this.taskContextManager.setTaskManager(taskManager, sessionId);
    this.missionDrivenEngine.setTaskManager(taskManager, sessionId);
  }

  /** 是否正在运行 */
  get running(): boolean {
    return this.executionCoordinator.running;
  }

  /** 中断当前任务 */
  async interrupt(): Promise<void> {
    await this.executionCoordinator.interrupt();
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
    return this.executionCoordinator.execute(userPrompt, taskId, sessionId);
  }

  /**
   * 执行任务并返回任务上下文
   */
  async executeWithTaskContext(userPrompt: string, sessionId?: string): Promise<{ taskId: string; result: string }> {
    return this.executionCoordinator.executeWithTaskContext(userPrompt, sessionId);
  }

  /** 仅生成执行计划（不执行） */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    return this.planCoordinator.createPlan(userPrompt, taskId, sessionId);
  }

  /** 使用已生成的计划执行 */
  async executePlan(record: PlanRecord, taskId?: string, sessionId?: string): Promise<string> {
    return this.planCoordinator.executePlan(record, taskId, sessionId);
  }

  getActivePlanForSession(sessionId: string): PlanRecord | null {
    return this.planCoordinator.getActivePlanForSession(sessionId);
  }

  getLatestPlanForSession(sessionId: string): PlanRecord | null {
    return this.planCoordinator.getLatestPlanForSession(sessionId);
  }

  getPlanById(planId: string, sessionId: string): PlanRecord | null {
    return this.planCoordinator.getPlanById(planId, sessionId);
  }

  /** 取消当前任务 */
  async cancel(): Promise<void> {
    await this.executionCoordinator.cancel();
  }

  /** 获取可用的 CLI 列表 */
  getAvailableCLIs(): CLIType[] {
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

  /** 销毁编排器 */
  dispose(): void {
    this.missionDrivenEngine.dispose();
    logger.info('编排器.销毁.完成', undefined, LogCategory.ORCHESTRATOR);
  }
}
