/**
 * DispatchManager - L3 统一调度管理器
 *
 * L3 统一架构重构后的唯一 Worker 调度器。
 * 职责：
 * - 编排工具回调注册（dispatch_task / send_worker_message）
 * - DispatchBatch 创建与事件处理
 * - 通过 WorkerPipeline 执行统一管道（含可配置治理）
 * - Worker 隔离策略调度（同类型串行、不同类型并行）
 * - Phase B+ 中间 LLM 调用
 * - Phase C 汇总
 */

import { logger, LogCategory } from '../../logging';
import type { WorkerSlot } from '../../types';
import type { TokenUsage } from '../../types/agent-types';
import type { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import type { ProfileLoader } from '../profile/profile-loader';
import { LLMConfigLoader } from '../../llm/config';
import type { MessageHub } from './message-hub';
import type { MissionOrchestrator } from './mission-orchestrator';
import type { Assignment } from '../mission';
import type { WorkerReport, OrchestratorResponse } from '../protocols/worker-report';
import { createAdjustResponse } from '../protocols/worker-report';
import {
  DispatchBatch,
  CancellationError,
  isTerminalStatus,
  type DispatchEntry,
  type DispatchResult,
  type DispatchStatus,
  type DispatchCollaborationContracts,
  type DispatchAuditOutcome,
  type DispatchAuditIssue,
  type DispatchAuditLevel,
} from './dispatch-batch';
import type {
  WaitForWorkersResult,
  DispatchTaskCollaborationContracts,
  UpdateTodoStatus,
} from '../../tools/orchestration-executor';
import { buildDispatchSummaryPrompt } from '../prompts/orchestrator-prompts';
import { MessageType } from '../../protocol/message-protocol';
import { PlanningExecutor } from './executors/planning-executor';
import { WorkerPipeline } from './worker-pipeline';
import type { SnapshotManager } from '../../snapshot-manager';
import type { SupplementaryInstructionQueue } from './supplementary-instruction-queue';
import { DispatchCompletionQueue } from './dispatch-completion-queue';

interface ResumeExecutionContext {
  sessionId: string;
  sourceMissionId: string;
  resumePrompt?: string;
  workerSessionBySlot: Map<WorkerSlot, string>;
  createdAt: number;
}

interface DispatchRoutingDecision {
  selectedWorker: WorkerSlot;
  category: string;
  categorySource: 'explicit_param';
  degraded: boolean;
  routingReason: string;
}

interface WorkerAvailabilitySnapshot {
  availableWorkers: Set<WorkerSlot>;
  unavailableReasons: Map<WorkerSlot, string>;
}

interface DispatchCategoryResolution {
  category: string;
  source: 'explicit_param';
}

/**
 * DispatchManager 依赖接口
 */
export interface DispatchManagerDeps {
  adapterFactory: IAdapterFactory;
  profileLoader: ProfileLoader;
  messageHub: MessageHub;
  missionOrchestrator: MissionOrchestrator;
  workspaceRoot: string;
  // 动态状态访问
  getActiveUserPrompt: () => string;
  getActiveImagePaths: () => string[] | undefined;
  getCurrentSessionId: () => string | undefined;
  getMissionIdsBySession: (sessionId: string) => Promise<string[]>;
  ensureMissionForDispatch: () => Promise<string>;
  getProjectKnowledgeBase: () => import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase | undefined;
  // 治理依赖（WorkerPipeline 使用）
  getSnapshotManager: () => SnapshotManager | null;
  getContextManager: () => import('../../context/context-manager').ContextManager | null;
  getTodoManager: () => import('../../todo').TodoManager | null;
  // Token 统计
  recordOrchestratorTokens: (usage?: TokenUsage, phase?: 'planning' | 'verification') => void;
  recordWorkerTokenUsage: (results: Map<string, import('../worker').AutonomousExecutionResult>) => void;
  // 补充指令队列（反应式编排：运行时注入 Worker 指令）
  getSupplementaryQueue: () => SupplementaryInstructionQueue | null;
}

/**
 * DispatchManager - L3 统一调度管理器
 */
export class DispatchManager {
  private static readonly WORKER_SLOTS: WorkerSlot[] = ['claude', 'codex', 'gemini'];
  private static readonly WORKER_FALLBACK_PRIORITY: Record<WorkerSlot, WorkerSlot[]> = {
    claude: ['codex', 'gemini'],
    codex: ['claude', 'gemini'],
    gemini: ['claude', 'codex'],
  };
  private static readonly RUNTIME_UNAVAILABLE_COOLDOWN_MS = 60_000;

  // Phase B+ 中间调用频率限制：同一 batch 内最小间隔 30 秒
  private lastPhaseBPlusTimestamp = 0;
  private static readonly PHASE_B_PLUS_MIN_INTERVAL = 30_000;
  /** 同轮 dispatch 的短窗口合并调度，减少 Worker 指令卡片抖动 */
  private static readonly DISPATCH_COALESCE_MS = 120;

  private pipeline = new WorkerPipeline();
  private activeBatch: DispatchBatch | null = null;
  private _planningExecutor: PlanningExecutor | null = null;

  // 反应式编排：Worker 完成结果队列 + 等待唤醒机制
  private completionQueue = new DispatchCompletionQueue();
  /** 标记编排者是否调用了 wait_for_workers（决定是否跳过自动 Phase C） */
  private reactiveMode = false;
  /** 反应式 Batch 是否仍等待主对话区最终汇总 */
  private reactiveBatchAwaitingSummary = new Set<string>();
  /** 记录每个 Mission 的 Worker Session（用于后续断点续跑） */
  private missionWorkerSessions = new Map<string, Map<WorkerSlot, string>>();
  /** 当前会话的待恢复上下文（只在下一轮执行中生效） */
  private activeResumeContexts = new Map<string, ResumeExecutionContext>();
  /** 记录 dispatch task 的分类（用于可解释性与后续诊断） */
  private dispatchTaskCategories = new Map<string, string>();
  /** Worker 运行时暂时不可用状态（短期冷却） */
  private runtimeUnavailableWorkers = new Map<WorkerSlot, { until: number; reason: string }>();
  /** 活跃的 Assignment 映射（Worker 执行期间可查，供 split_todo handler 使用） */
  private activeAssignments = new Map<string, Assignment>();
  /** Worker Lane 运行态：同一 Worker 同一时刻仅允许一个执行链 */
  private activeWorkerLanes = new Set<WorkerSlot>();
  /** Batch 级调度合并定时器 */
  private dispatchScheduleTimers = new Map<string, NodeJS.Timeout>();
  private static readonly MAX_MISSION_SESSION_RECORDS = 100;

  constructor(private deps: DispatchManagerDeps) {
    this.setupMissionEventListeners();
  }

  /**
   * 订阅 MissionOrchestrator 的 Todo/Insight 事件，
   * 将进度信息直接通过 MessageHub 发送到前端 SubTaskCard
   */
  private setupMissionEventListeners(): void {
    const mo = this.deps.missionOrchestrator;

    mo.on('todoStarted', ({ assignmentId, content }: { assignmentId: string; content: string }) => {
      this.reportTodoProgress(assignmentId, `正在执行: ${content}`);
    });

    mo.on('todoCompleted', ({ assignmentId, content }: { assignmentId: string; content: string }) => {
      this.reportTodoProgress(assignmentId, `完成: ${content}`);
    });

    mo.on('todoFailed', ({ assignmentId, content, error }: { assignmentId: string; content: string; error?: string }) => {
      this.reportTodoProgress(assignmentId, `失败: ${content} - ${error || '未知错误'}`);
    });

    mo.on('insightGenerated', ({ workerId, type, content, importance }: { workerId: string; type: string; content: string; importance: string }) => {
      const typeLabels: Record<string, string> = {
        decision: '决策', contract: '契约', risk: '风险', constraint: '约束',
      };
      const label = typeLabels[type] || type;
      const level = importance === 'critical' ? 'warning' : 'info';
      this.deps.messageHub.notify(`[${workerId}] ${label}: ${content}`, level);
    });
  }

  /**
   * 报告 Todo 进度：从 activeBatch 查找 entry 并更新 subTaskCard
   */
  private reportTodoProgress(assignmentId: string, summary: string): void {
    const entry = this.activeBatch?.getEntry(assignmentId);
    if (entry) {
      this.deps.messageHub.subTaskCard({
        id: assignmentId,
        title: entry.task,
        status: 'running',
        worker: entry.worker,
        summary,
      });
    }
  }

  /**
   * 获取 PlanningExecutor 单例（延迟初始化）
   */
  private getPlanningExecutor(): PlanningExecutor {
    if (!this._planningExecutor) {
      const todoManager = this.deps.getTodoManager();
      if (!todoManager) {
        throw new Error('TodoManager 未初始化');
      }
      this._planningExecutor = new PlanningExecutor(todoManager);
    }
    return this._planningExecutor;
  }

  /**
   * 获取当前活跃的 DispatchBatch
   */
  getActiveBatch(): DispatchBatch | null {
    return this.activeBatch;
  }

  /**
   * 获取当前可路由 Worker 快照（供系统提示词和 UI 统一展示）
   */
  getWorkerAvailability(): { availableWorkers: WorkerSlot[]; unavailableReasons: Record<string, string> } {
    const snapshot = this.getWorkerAvailabilitySnapshot();
    return {
      availableWorkers: Array.from(snapshot.availableWorkers),
      unavailableReasons: Object.fromEntries(snapshot.unavailableReasons.entries()),
    };
  }

  /**
   * 新一轮执行前重置调度状态
   *
   * 目的：彻底切断上一轮归档 batch、完成队列与反应式标记，
   * 避免“无 dispatch 的新一轮”被误判为存在历史 dispatch。
   */
  resetForNewExecutionCycle(): void {
    if (this.activeBatch?.status === 'active') {
      this.activeBatch.cancelAll('开始新一轮执行，清理上一轮残留任务');
    }

    if (this.activeBatch) {
      this.reactiveBatchAwaitingSummary.delete(this.activeBatch.id);
      this.clearBatchTaskCategories(this.activeBatch);
    }

    this.activeBatch = null;
    this.reactiveMode = false;
    this.completionQueue.reset();
    this.activeWorkerLanes.clear();
    this.clearDispatchScheduleTimers();
    this.clearResumeContext();
  }

  private normalizeCategoryName(raw: string): string {
    return raw.trim().toLowerCase().replace(/[\s-]+/g, '_');
  }

  private getKnownCategoryNames(): string[] {
    return Array.from(this.deps.profileLoader.getAllCategories().keys()).sort();
  }

  private assertCategoryExists(category: string): { ok: true } | { ok: false; error: string } {
    if (this.deps.profileLoader.getCategory(category)) {
      return { ok: true };
    }
    return {
      ok: false,
      error: `未知任务分类 "${category}"。可选分类: ${this.getKnownCategoryNames().join(', ')}`,
    };
  }

  private resolveDispatchCategoryWithSource(
    _goal: string,
    explicitCategory?: string,
  ): { ok: true; value: DispatchCategoryResolution } | { ok: false; error: string } {
    const explicit = explicitCategory?.trim();
    if (explicit) {
      const normalized = this.normalizeCategoryName(explicit);
      const check = this.assertCategoryExists(normalized);
      if (!check.ok) {
        return { ok: false, error: check.error };
      }
      return {
        ok: true,
        value: {
          category: normalized,
          source: 'explicit_param',
        },
      };
    }
    return {
      ok: false,
      error: `dispatch_task 缺少 category 参数。可选分类: ${this.getKnownCategoryNames().join(', ')}`,
    };
  }

  private getRuntimeUnavailableReason(worker: WorkerSlot): string | null {
    const status = this.runtimeUnavailableWorkers.get(worker);
    if (!status) {
      return null;
    }
    const now = Date.now();
    if (now >= status.until) {
      this.runtimeUnavailableWorkers.delete(worker);
      return null;
    }
    const remainSeconds = Math.ceil((status.until - now) / 1000);
    return `${status.reason}（冷却 ${remainSeconds}s）`;
  }

  private markWorkerRuntimeUnavailable(worker: WorkerSlot, reason: string): void {
    this.runtimeUnavailableWorkers.set(worker, {
      until: Date.now() + DispatchManager.RUNTIME_UNAVAILABLE_COOLDOWN_MS,
      reason,
    });
  }

  private clearWorkerRuntimeUnavailable(worker: WorkerSlot): void {
    this.runtimeUnavailableWorkers.delete(worker);
  }

  /**
   * 是否应将 Worker 标记为“运行时不可用”
   *
   * 只对“基础设施/连通性”错误做短期冷却：
   * - 鉴权/配额/限流
   * - 网络/连接/超时
   * - 模型端服务不可用
   *
   * 业务任务失败（如代码错误、断言失败）不应触发 Worker 不可用。
   */
  private shouldMarkRuntimeUnavailable(errorMessage: string): boolean {
    const normalized = (errorMessage || '').toLowerCase();
    if (!normalized) {
      return false;
    }

    const infraErrorPattern =
      /unauthorized|forbidden|invalid api key|api key|auth|permission|quota|billing|payment|rate limit|limit|insufficient|suspended|disabled|timeout|timed out|network|connection|fetch failed|socket|econnreset|econnrefused|enotfound|eai_again|tls|certificate|overloaded|service unavailable|502|503|504/;

    return infraErrorPattern.test(normalized);
  }

  private getWorkerAvailabilitySnapshot(): WorkerAvailabilitySnapshot {
    const availableWorkers = new Set<WorkerSlot>();
    const unavailableReasons = new Map<WorkerSlot, string>();
    const enabledProfiles = this.deps.profileLoader.getEnabledProfiles();
    const fullConfig = LLMConfigLoader.loadFullConfig();

    for (const worker of DispatchManager.WORKER_SLOTS) {
      const workerConfig = fullConfig.workers[worker];
      if (!enabledProfiles.has(worker)) {
        unavailableReasons.set(worker, '未启用');
        continue;
      }
      if (!workerConfig) {
        unavailableReasons.set(worker, '缺少模型配置');
        continue;
      }
      if (!workerConfig.apiKey?.trim()) {
        unavailableReasons.set(worker, 'API Key 未配置');
        continue;
      }
      if (!workerConfig.baseUrl?.trim()) {
        unavailableReasons.set(worker, 'Base URL 未配置');
        continue;
      }
      if (!workerConfig.model?.trim()) {
        unavailableReasons.set(worker, '模型未配置');
        continue;
      }
      if (workerConfig.provider !== 'openai' && workerConfig.provider !== 'anthropic') {
        unavailableReasons.set(worker, `Provider 无效: ${workerConfig.provider}`);
        continue;
      }
      const runtimeReason = this.getRuntimeUnavailableReason(worker);
      if (runtimeReason) {
        unavailableReasons.set(worker, runtimeReason);
        continue;
      }
      availableWorkers.add(worker);
    }

    return { availableWorkers, unavailableReasons };
  }

  private pickFallbackWorker(
    preferredWorker: WorkerSlot,
    availableWorkers: Set<WorkerSlot>,
  ): WorkerSlot | undefined {
    return DispatchManager.WORKER_FALLBACK_PRIORITY[preferredWorker]
      .find(worker => availableWorkers.has(worker));
  }

  private resolveDispatchRouting(
    goal: string,
    explicitCategory?: string,
  ): { ok: true; decision: DispatchRoutingDecision } | { ok: false; error: string } {
    try {
      // 每次 dispatch 前刷新分工配置，确保外部改动立即生效
      this.deps.profileLoader.getAssignmentLoader().reload();
    } catch (error: any) {
      return {
        ok: false,
        error: `读取分工配置失败: ${error?.message || String(error)}`,
      };
    }
    const categoryResolution = this.resolveDispatchCategoryWithSource(goal, explicitCategory);
    if (!categoryResolution.ok) {
      return {
        ok: false,
        error: categoryResolution.error,
      };
    }
    const { category, source } = categoryResolution.value;
    let ownerWorker: WorkerSlot;
    try {
      ownerWorker = this.deps.profileLoader.getWorkerForCategory(category);
    } catch (error: any) {
      return {
        ok: false,
        error: `任务分类 ${category} 未找到有效归属 Worker: ${error?.message || String(error)}`,
      };
    }
    const availability = this.getWorkerAvailabilitySnapshot();

    if (availability.availableWorkers.has(ownerWorker)) {
      return {
        ok: true,
        decision: {
          selectedWorker: ownerWorker,
          category,
          categorySource: source,
          degraded: false,
          routingReason: `自动路由命中分类 ${category}，归属 Worker ${ownerWorker}`,
        },
      };
    }

    const ownerUnavailableReason = availability.unavailableReasons.get(ownerWorker) || '当前不可用';
    const fallbackWorker = this.pickFallbackWorker(ownerWorker, availability.availableWorkers);
    if (!fallbackWorker) {
      const reasonText = DispatchManager.WORKER_SLOTS
        .map(worker => `${worker}:${availability.unavailableReasons.get(worker) || '不可用'}`)
        .join('；');
      return {
        ok: false,
        error: `分类 ${category} 的归属 Worker ${ownerWorker} 不可用（${ownerUnavailableReason}），且无可用降级 Worker。当前状态：${reasonText}`,
      };
    }

    return {
      ok: true,
      decision: {
        selectedWorker: fallbackWorker,
        category,
        categorySource: source,
        degraded: true,
        routingReason: `分类 ${category} 归属 ${ownerWorker}，但其不可用（${ownerUnavailableReason}），已降级到 ${fallbackWorker}`,
      },
    };
  }

  private resolveExecutionWorker(
    preferredWorker: WorkerSlot,
  ): { ok: true; selectedWorker: WorkerSlot; degraded: boolean; routingReason: string } | { ok: false; error: string } {
    const availability = this.getWorkerAvailabilitySnapshot();
    if (availability.availableWorkers.has(preferredWorker)) {
      return {
        ok: true,
        selectedWorker: preferredWorker,
        degraded: false,
        routingReason: `执行前校验通过，继续由 ${preferredWorker} 执行`,
      };
    }

    const preferredUnavailableReason = availability.unavailableReasons.get(preferredWorker) || '当前不可用';
    const fallbackWorker = this.pickFallbackWorker(preferredWorker, availability.availableWorkers);
    if (!fallbackWorker) {
      return {
        ok: false,
        error: `任务目标 Worker ${preferredWorker} 不可用（${preferredUnavailableReason}），且无可用降级 Worker`,
      };
    }

    return {
      ok: true,
      selectedWorker: fallbackWorker,
      degraded: true,
      routingReason: `目标 Worker ${preferredWorker} 不可用（${preferredUnavailableReason}），执行时降级到 ${fallbackWorker}`,
    };
  }

  activateResumeContext(sourceMissionId: string, resumePrompt?: string): boolean {
    const currentSessionId = this.deps.getCurrentSessionId();
    if (!currentSessionId) {
      return false;
    }
    const workerSessions = this.missionWorkerSessions.get(sourceMissionId);
    if (!workerSessions || workerSessions.size === 0) {
      return false;
    }

    this.activeResumeContexts.set(currentSessionId, {
      sessionId: currentSessionId,
      sourceMissionId,
      resumePrompt,
      workerSessionBySlot: new Map(workerSessions),
      createdAt: Date.now(),
    });

    logger.info('Dispatch.ResumeContext.已激活', {
      sessionId: currentSessionId,
      sourceMissionId,
      workers: Array.from(workerSessions.keys()),
    }, LogCategory.ORCHESTRATOR);

    return true;
  }

  clearResumeContext(): void {
    const currentSessionId = this.deps.getCurrentSessionId();
    if (!currentSessionId) {
      return;
    }
    this.activeResumeContexts.delete(currentSessionId);
  }

  private getResumeContextForWorker(worker: WorkerSlot): { resumeSessionId?: string; resumePrompt?: string } {
    const currentSessionId = this.deps.getCurrentSessionId();
    if (!currentSessionId) {
      return {};
    }
    const context = this.activeResumeContexts.get(currentSessionId);
    if (!context) {
      return {};
    }
    const resumeSessionId = context.workerSessionBySlot.get(worker);
    if (!resumeSessionId) {
      return {};
    }
    return {
      resumeSessionId,
      resumePrompt: context.resumePrompt,
    };
  }

  private recordMissionWorkerSession(
    missionId: string,
    worker: WorkerSlot,
    workerSessionId: string,
  ): void {
    if (!missionId || !workerSessionId) {
      return;
    }
    const existing = this.missionWorkerSessions.get(missionId) || new Map<WorkerSlot, string>();
    existing.set(worker, workerSessionId);
    this.missionWorkerSessions.set(missionId, existing);

    if (this.missionWorkerSessions.size > DispatchManager.MAX_MISSION_SESSION_RECORDS) {
      const oldestMissionId = this.missionWorkerSessions.keys().next().value as string | undefined;
      if (oldestMissionId) {
        this.missionWorkerSessions.delete(oldestMissionId);
      }
    }
  }

  /**
   * 注入编排工具（dispatch_task / send_worker_message）的回调处理器
   */
  setupOrchestrationToolHandlers(): void {
    const toolManager = this.deps.adapterFactory.getToolManager();
    const orchestrationExecutor = toolManager.getOrchestrationExecutor();

    // 从 ProfileLoader 注入已启用的 Worker 列表到工具定义，
    // 确保编排 LLM 从工具 schema（enum）和系统提示词两个通道获取的信息一致
    const enabledProfiles = this.deps.profileLoader.getEnabledProfiles();
    orchestrationExecutor.setAvailableWorkers(
      Array.from(enabledProfiles.values()).map(p => ({
        slot: p.worker,
        description: p.persona.strengths.slice(0, 2).join('/'),
      }))
    );

    // 注入 Category → Worker 映射到工具定义，
    // 使 dispatch_task 的 category 参数拥有精确的 enum 枚举和分工描述
    const categoryMap = this.deps.profileLoader.getAssignmentLoader().getCategoryMap();
    const allCategories = this.deps.profileLoader.getAllCategories();
    orchestrationExecutor.setCategoryWorkerMap(
      Object.entries(categoryMap).map(([category, worker]) => ({
        category,
        displayName: allCategories.get(category)?.displayName || category,
        worker,
      }))
    );

    // Worker 可用列表变化后立即失效工具缓存，确保 schema 与运行时一致
    toolManager.refreshToolSchemas();

    orchestrationExecutor.setHandlers({
      dispatch: async (params) => {
        const { task_name, goal, acceptance, constraints, context, files, scopeHint, dependsOn, category, requiresModification, contracts } = params;
        if (typeof requiresModification !== 'boolean') {
          return {
            task_id: '',
            status: 'failed' as const,
            error: 'requires_modification 必须为布尔值',
          };
        }
        const taskTitle = task_name || goal.trim();
        const collaborationContracts = this.normalizeCollaborationContracts(contracts);
        logger.info('编排工具.dispatch_task.开始', {
          category,
          requiresModification,
          scopeHintCount: scopeHint?.length || 0,
          goalPreview: taskTitle.substring(0, 80),
          acceptanceCount: acceptance.length,
          constraintCount: constraints.length,
          contextCount: context.length,
          dependsOn,
        }, LogCategory.ORCHESTRATOR);

        const routingResult = this.resolveDispatchRouting(taskTitle, category);
        if (!routingResult.ok) {
          return {
            task_id: '',
            status: 'failed' as const,
            error: routingResult.error,
          };
        }
        const { decision } = routingResult;
        logger.info('编排工具.dispatch_task.路由决策', {
          selectedWorker: decision.selectedWorker,
          category: decision.category,
          categorySource: decision.categorySource,
          degraded: decision.degraded,
          requiresModification,
          reason: decision.routingReason,
        }, LogCategory.ORCHESTRATOR);
        if (decision.degraded) {
          this.deps.messageHub.notify(
            `任务改派：请求分类 ${decision.category}，实际执行 Worker 为 ${decision.selectedWorker}（${decision.routingReason}）`,
            'warning'
          );
        }

        let missionId: string;
        try {
          missionId = await this.deps.ensureMissionForDispatch();
        } catch (error: any) {
          return {
            task_id: '',
            status: 'failed' as const,
            error: `创建任务失败: ${error?.message || String(error)}`,
          };
        }

        // 生成唯一 task_id
        const taskId = `dispatch-${Date.now()}-${decision.selectedWorker}-${Math.random().toString(36).substring(2, 5)}`;
        this.dispatchTaskCategories.set(taskId, decision.category);

        // 确保 DispatchBatch 存在（一次 orchestrator LLM 调用共享一个 Batch）
        if (!this.activeBatch || this.activeBatch.status !== 'active') {
          if (this.activeBatch?.status === 'archived') {
            this.reactiveBatchAwaitingSummary.delete(this.activeBatch.id);
          }
          // 使用 Mission ID 作为 Batch ID，确保 Todo 关联到正确的 Mission
          this.activeBatch = new DispatchBatch(missionId);
          this.activeBatch.userPrompt = this.deps.getActiveUserPrompt();
          this.setupBatchEventHandlers(this.activeBatch);
          // reactiveMode 是执行级状态，仅由 resetForNewExecutionCycle() 重置
          this.completionQueue.reset();
        }

        // 注册到 DispatchBatch
        try {
          if ((!scopeHint || scopeHint.length === 0) && this.shouldWarnMissingScopeHintForParallelTask(this.activeBatch, dependsOn)) {
            throw new Error('并行任务必须显式提供 scope_hint，以满足文件级分区要求');
          }

          this.activeBatch.register({
            taskId,
            worker: decision.selectedWorker,
            goal: taskTitle,
            acceptance,
            constraints,
            context,
            task: taskTitle,
            scopeHint,
            files,
            requiresModification,
            dependsOn,
            collaborationContracts,
          });

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
          this.dispatchTaskCategories.delete(taskId);
          return {
            task_id: taskId,
            status: 'failed' as const,
            worker: decision.selectedWorker,
            category: decision.category,
            routing_reason: decision.routingReason,
            degraded: decision.degraded,
            error: regError.message,
          };
        }

        // 发送 subTaskCard（状态取决于注册后是否有依赖）
        const entry = this.activeBatch.getEntry(taskId);
        const hasDeps = entry ? entry.status === 'waiting_deps' : (dependsOn && dependsOn.length > 0);
        this.deps.messageHub.subTaskCard({
          id: taskId,
          title: taskTitle,
          status: hasDeps ? 'pending' : 'running',
          worker: decision.selectedWorker,
        });

        // 通过隔离策略决定是否立即启动（约束 5）
        if (!hasDeps) {
          this.scheduleDispatchReadyTasks(this.activeBatch, { reason: 'dispatch-registered' });
        }
        // 有依赖的任务由 DispatchBatch 的 task:ready 事件触发

        // 立即返回 task_id（非阻塞）
        return {
          task_id: taskId,
          status: 'dispatched' as const,
          worker: decision.selectedWorker,
          category: decision.category,
          routing_reason: decision.routingReason,
          degraded: decision.degraded,
        };
      },

      sendMessage: async (params) => {
        const { worker, message } = params;
        logger.info('编排工具.send_worker_message', {
          worker, messagePreview: message.substring(0, 80),
        }, LogCategory.ORCHESTRATOR);

        this.deps.messageHub.workerInstruction(worker, message);
        return { delivered: true };
      },

      waitForWorkers: async (params) => {
        logger.info('编排工具.wait_for_workers.开始', {
          taskIds: params.task_ids || 'all',
        }, LogCategory.ORCHESTRATOR);

        return this.waitForWorkers(params.task_ids);
      },

      splitTodo: async (params) => {
        const { subtasks, callerContext } = params;
        const assignment = this.activeAssignments?.get(callerContext.assignmentId);
        if (!assignment) {
          return {
            success: false,
            childTodoIds: [],
            error: `Assignment ${callerContext.assignmentId} 不在活跃执行中`,
          };
        }

        // 3 级约束：L1/L2 可拆分，L3（parent 的 parent 存在）不可再拆分
        const currentTodo = assignment.todos.find(t => t.id === callerContext.todoId);
        if (currentTodo?.parentId) {
          const parentTodo = assignment.todos.find(t => t.id === currentTodo.parentId);
          if (parentTodo?.parentId) {
            return {
              success: false,
              childTodoIds: [],
              error: '已达最大拆分深度（3 级），不可再次拆分',
            };
          }
        }

        let todoManager = this.deps.getTodoManager();
        if (!todoManager) {
          await this.deps.missionOrchestrator.ensureTodoManagerInitialized();
          todoManager = this.deps.getTodoManager();
        }
        if (!todoManager) {
          return {
            success: false,
            childTodoIds: [],
            error: 'TodoManager 未初始化',
          };
        }

        const childTodoIds: string[] = [];
        for (const subtask of subtasks) {
          const child = await todoManager.create({
            missionId: callerContext.missionId,
            assignmentId: callerContext.assignmentId,
            parentId: callerContext.todoId,
            content: subtask.content,
            reasoning: subtask.reasoning,
            type: subtask.type,
            workerId: callerContext.workerId as WorkerSlot,
          });
          assignment.todos.push(child);
          childTodoIds.push(child.id);
        }

        logger.info('编排工具.split_todo.完成', {
          parentTodoId: callerContext.todoId,
          childCount: childTodoIds.length,
          workerId: callerContext.workerId,
        }, LogCategory.ORCHESTRATOR);

        return { success: true, childTodoIds };
      },

      getTodos: async (params) => {
        let todoManager = this.deps.getTodoManager();
        if (!todoManager) {
          await this.deps.missionOrchestrator.ensureTodoManagerInitialized();
          todoManager = this.deps.getTodoManager();
        }
        if (!todoManager) {
          throw new Error('TodoManager 未初始化，无法获取 Todos');
        }

        const explicitMissionId = params.missionId?.trim();
        const explicitSessionId = params.sessionId?.trim();
        const callerMissionId = params.callerContext?.missionId?.trim();
        const callerWorkerId = params.callerContext?.workerId?.trim();
        const isOrchestratorCaller = !callerWorkerId || callerWorkerId === 'orchestrator';
        const statusFilter = params.status as any;

        const extractSessionId = (scopedMissionId?: string): string | undefined => {
          if (!scopedMissionId || !scopedMissionId.startsWith('session:')) {
            return undefined;
          }
          const sessionId = scopedMissionId.slice('session:'.length).trim();
          return sessionId || undefined;
        };

        const resolveConcreteMissionId = (missionLikeId?: string): string | undefined => {
          if (!missionLikeId || missionLikeId.startsWith('session:')) {
            return undefined;
          }
          return missionLikeId;
        };

        const concreteMissionId = resolveConcreteMissionId(explicitMissionId)
          || resolveConcreteMissionId(callerMissionId);
        if (concreteMissionId) {
          const assignmentId = isOrchestratorCaller ? undefined : params.callerContext?.assignmentId;
          return await todoManager.query({
            missionId: concreteMissionId,
            assignmentId,
            status: statusFilter,
          });
        }

        if (!isOrchestratorCaller) {
          throw new Error('Worker 缺少有效 mission 上下文，无法查询 Todos');
        }

        const sessionId = explicitSessionId
          || extractSessionId(explicitMissionId)
          || extractSessionId(callerMissionId)
          || this.deps.getCurrentSessionId();
        if (!sessionId) {
          throw new Error('未找到可查询的 session，请显式传入 mission_id 或 session_id');
        }

        const missionIds = (await this.deps.getMissionIdsBySession(sessionId))
          .map(id => id?.trim())
          .filter((id): id is string => Boolean(id));
        if (missionIds.length === 0) {
          return [];
        }

        const uniqueMissionIds = Array.from(new Set(missionIds));
        const todosByMission = await Promise.all(
          uniqueMissionIds.map(async (missionId, missionOrder) => {
            const todos = await todoManager.query({ missionId, status: statusFilter });
            return todos.map(todo => ({ missionOrder, todo }));
          })
        );

        return todosByMission
          .flat()
          .sort((a, b) => {
            if (a.missionOrder !== b.missionOrder) {
              return a.missionOrder - b.missionOrder;
            }
            const aCreatedAt = typeof a.todo.createdAt === 'number' ? a.todo.createdAt : 0;
            const bCreatedAt = typeof b.todo.createdAt === 'number' ? b.todo.createdAt : 0;
            return aCreatedAt - bCreatedAt;
          })
          .map(item => item.todo);
      },

      updateTodo: async (params) => {
        let todoManager = this.deps.getTodoManager();
        if (!todoManager) {
          await this.deps.missionOrchestrator.ensureTodoManagerInitialized();
          todoManager = this.deps.getTodoManager();
        }
        if (!todoManager) {
          return { success: false, error: 'TodoManager 未初始化，无法更新 Todo' };
        }

        try {
          if (!params.updates || params.updates.length === 0) {
            return { success: false, error: '缺少有效的更新内容(updates)' };
          }

          const allowedStatus = new Set<UpdateTodoStatus>(['pending', 'skipped']);
          const pendingAllowedSource = new Set(['pending', 'completed', 'failed', 'skipped']);
          const skippedAllowedSource = new Set(['pending', 'blocked', 'ready', 'running', 'skipped']);

          type UpdatePlan = {
            todoId: string;
            status?: UpdateTodoStatus;
            content?: string;
          };

          const plans: UpdatePlan[] = [];

          for (const update of params.updates) {
            if (!update.todoId) {
              throw new Error('update_todo 存在缺少 todo_id 的条目');
            }

            const hasStatus = update.status !== undefined;
            const hasContent = update.content !== undefined;
            if (!hasStatus && !hasContent) {
              throw new Error(`Todo ${update.todoId} 缺少可执行更新字段（status/content）`);
            }

            if (update.status !== undefined && !allowedStatus.has(update.status as UpdateTodoStatus)) {
              throw new Error(`Todo ${update.todoId} status=${update.status} 非法，仅支持 pending/skipped`);
            }

            const todo = await todoManager.get(update.todoId);
            if (!todo) {
              throw new Error(`Todo not found: ${update.todoId}`);
            }

            const targetStatus = update.status as UpdateTodoStatus | undefined;
            if (targetStatus === 'pending' && !pendingAllowedSource.has(todo.status)) {
              throw new Error(`Todo ${update.todoId} 当前状态=${todo.status}，不允许重置为 pending`);
            }

            if (targetStatus === 'skipped' && !skippedAllowedSource.has(todo.status)) {
              throw new Error(`Todo ${update.todoId} 当前状态=${todo.status}，不允许更新为 skipped`);
            }

            plans.push({
              todoId: update.todoId,
              status: targetStatus,
              content: update.content,
            });
          }

          for (const plan of plans) {
            if (plan.content !== undefined) {
              await todoManager.update(plan.todoId, { content: plan.content });
            }

            if (plan.status === 'pending') {
              await todoManager.resetToPending(plan.todoId);
            } else if (plan.status === 'skipped') {
              await todoManager.skip(plan.todoId);
            }
          }
          return { success: true };
        } catch (error: any) {
          return { success: false, error: error.message };
        }
      },
    });

    logger.info('编排器.编排工具回调.已注入', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 启动 dispatch Worker 执行（非阻塞）
   *
   * 通过 WorkerPipeline 统一执行管道，包含可配置的治理步骤：
   * - governance = 'auto'（默认）：有 files 时自动启用 LSP/Snapshot/TargetEnforce
   * - governance = 'full'：强制启用所有治理步骤
   */
  launchDispatchWorker(entry: DispatchEntry): void {
    void this.executeDispatchEntry(entry);
  }

  private async executeDispatchEntry(
    entry: DispatchEntry,
    options?: { emitWorkerInstruction?: boolean },
  ): Promise<void> {
    const emitWorkerInstruction = options?.emitWorkerInstruction ?? true;
    const {
      taskId,
      worker,
      task,
      goal,
      acceptance,
      constraints,
      context,
      scopeHint,
      files,
      requiresModification,
      collaborationContracts,
    } = entry;
    const batch = this.activeBatch;
    const category = this.dispatchTaskCategories.get(taskId);
    if (!category) {
      const errorMsg = `dispatchTaskCategories 中未找到 taskId=${taskId}，dispatch 注册流程存在数据不一致`;
      this.deps.messageHub.subTaskCard({
        id: taskId,
        title: task,
        status: 'failed',
        worker,
        summary: errorMsg,
        error: errorMsg,
      });
      batch?.markFailed(taskId, { success: false, summary: errorMsg, errors: [errorMsg] });
      logger.error('编排工具.dispatch_task.任务分类丢失', { taskId, worker }, LogCategory.ORCHESTRATOR);
      return;
    }
    const executionRouting = this.resolveExecutionWorker(worker);
    if (!executionRouting.ok) {
      const errorMsg = executionRouting.error;
      this.deps.messageHub.subTaskCard({
        id: taskId,
        title: task,
        status: 'failed',
        worker,
        summary: errorMsg,
        error: errorMsg,
      });
      batch?.markFailed(taskId, { success: false, summary: errorMsg, errors: [errorMsg] });
      return;
    }

    const effectiveWorker = executionRouting.selectedWorker;
    if (effectiveWorker !== worker) {
      const entry = batch?.getEntry(taskId);
      if (entry) {
        entry.worker = effectiveWorker;
      }
      this.deps.messageHub.notify(
        `任务 ${taskId} 执行前改派：${worker} -> ${effectiveWorker}（${executionRouting.routingReason}）`,
        'warning',
      );
      logger.warn('Dispatch.Worker.执行前改派', {
        taskId,
        from: worker,
        to: effectiveWorker,
        reason: executionRouting.routingReason,
      }, LogCategory.ORCHESTRATOR);
    }

    const { resumeSessionId, resumePrompt } = this.getResumeContextForWorker(effectiveWorker);

    // 标记开始运行
    batch?.markRunning(taskId);
    this.deps.messageHub.subTaskCard({
      id: taskId,
      title: task,
      status: 'running',
      worker: effectiveWorker,
    });

    if (emitWorkerInstruction) {
      // 同一 worker 的多个任务通过 lane 级稳定卡片聚合，避免重复派发多张指令卡。
      this.emitWorkerLaneInstructionCard(entry, effectiveWorker, batch);
    }

    try {
      // 确保 Worker 存在
      const workerInstance = await this.deps.missionOrchestrator.ensureWorkerForDispatch(effectiveWorker);

      // 构建轻量 Assignment
      const missionId = batch?.id || 'dispatch';
      const assignment: Assignment = {
        id: taskId,
        missionId,
        workerId: effectiveWorker,
        shortTitle: task,
        responsibility: task,
        delegationBriefing: this.buildDelegationBriefing({
          goal,
          acceptance,
          constraints,
          context,
          scopeHint,
          files,
          predecessorContext: this.buildPredecessorContext(taskId),
          collaborationContracts,
        }),
        assignmentReason: {
          profileMatch: { category, score: 100, matchedKeywords: [] },
          contractRole: 'none' as const,
          explanation: executionRouting.routingReason,
          alternatives: [],
        },
        scope: {
          includes: [task],
          excludes: [],
          scopeHints: scopeHint || [],
          targetPaths: files || [],
          requiresModification,
        },
        guidancePrompt: this.buildScopeHintGuidance(scopeHint),
        producerContracts: collaborationContracts.producerContracts,
        consumerContracts: collaborationContracts.consumerContracts,
        todos: [],
        planningStatus: 'pending' as const,
        status: 'pending' as const,
        progress: 0,
        createdAt: Date.now(),
      };

      // 获取项目上下文
      const knowledgeBase = this.deps.getProjectKnowledgeBase();
      const projectContext = knowledgeBase
        ? knowledgeBase.getProjectContext(600)
        : undefined;

      // 一级 Todo 由 PlanningExecutor 统一创建（编排层唯一入口）
      await this.getPlanningExecutor().createMacroTodo(missionId, assignment);

      // 通知 assignmentPlanned 事件（通道2：MissionOrchestrator 编排业务事件）
      // WebviewProvider.bindMissionEvents() 监听此事件驱动前端 Todo 面板更新
      this.deps.missionOrchestrator.notifyAssignmentPlanned({
        missionId,
        assignmentId: taskId,
        todos: assignment.todos || [],
      });

      // 计算治理开关（governance = 'auto'）
      const hasFiles = (files && files.length > 0) || false;
      const enableWriteGovernance = hasFiles && requiresModification;
      const snapshotManager = this.deps.getSnapshotManager();
      const contextManager = this.deps.getContextManager();

      // 通过 WorkerPipeline 统一执行
      this.activeAssignments.set(taskId, assignment);
      let pipelineResult;
      try {
        pipelineResult = await this.pipeline.execute({
        assignment,
        workerInstance,
        adapterFactory: this.deps.adapterFactory,
        workspaceRoot: this.deps.workspaceRoot,
        projectContext,
        missionId,
        onReport: (report) => this.handleDispatchWorkerReport(report, batch),
        cancellationToken: batch?.cancellationToken,
        imagePaths: this.deps.getActiveImagePaths(),
        // 反应式编排：补充指令回调（Worker 决策点消费队列中的用户追加指令）
        getSupplementaryInstructions: () => {
          const queue = this.deps.getSupplementaryQueue();
          return queue ? queue.consume(effectiveWorker) : [];
        },
        resumeSessionId,
        resumePrompt,
        // 治理开关（仅写任务启用强制写入相关治理）
        enableSnapshot: enableWriteGovernance && snapshotManager != null,
        enableLSP: enableWriteGovernance,
        enableTargetEnforce: enableWriteGovernance,
        enableContextUpdate: contextManager != null,
        snapshotManager,
        contextManager,
        todoManager: this.deps.getTodoManager(),
      });
      } finally {
        this.activeAssignments.delete(taskId);
      }

      const result = pipelineResult.executionResult;
      if (result.sessionId) {
        this.recordMissionWorkerSession(missionId, effectiveWorker, result.sessionId);
      }
      this.clearWorkerRuntimeUnavailable(effectiveWorker);

      // 直接使用 Worker 生成的结构化总结（唯一生产者：AutonomousWorker.buildStructuredSummary）
      const summary = result.summary;
      const modifiedFiles = [...new Set([
        ...result.completedTodos.flatMap(t => t.output?.modifiedFiles || []),
        ...result.failedTodos.flatMap(t => t.output?.modifiedFiles || []),
      ])];

      // 更新 subTaskCard 最终状态
      this.deps.messageHub.subTaskCard({
        id: taskId,
        title: task,
        status: result.success ? 'completed' : 'failed',
        worker: effectiveWorker,
        summary,
        modifiedFiles,
        ...(!result.success && { error: result.errors?.[0] || summary }),
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
      this.deps.recordWorkerTokenUsage(singleResult);

      logger.info('编排工具.dispatch_task.Worker完成', {
        worker: effectiveWorker, taskId, success: result.success, summary,
      }, LogCategory.ORCHESTRATOR);
    } catch (error: any) {
      // C-09: 取消异常不按失败处理，cancelAll 已标记 cancelled 状态
      if (error instanceof CancellationError || error?.isCancellation) {
        this.deps.messageHub.subTaskCard({
          id: taskId,
          title: task,
          status: 'stopped',
          worker: effectiveWorker,
          summary: error.message,
        });
        logger.info('编排工具.dispatch_task.Worker取消', {
          worker: effectiveWorker, taskId, reason: error.message,
        }, LogCategory.ORCHESTRATOR);
        return;
      }

      const errorMsg = error?.message || String(error);
      if (this.shouldMarkRuntimeUnavailable(errorMsg)) {
        this.markWorkerRuntimeUnavailable(effectiveWorker, errorMsg);
        logger.warn('Dispatch.Worker.运行时不可用.已标记冷却', {
          worker: effectiveWorker,
          taskId,
          reason: errorMsg,
        }, LogCategory.ORCHESTRATOR);
      } else {
        logger.warn('Dispatch.Worker.业务失败.不标记冷却', {
          worker: effectiveWorker,
          taskId,
          reason: errorMsg,
        }, LogCategory.ORCHESTRATOR);
      }

      this.deps.messageHub.subTaskCard({
        id: taskId,
        title: task,
        status: 'failed',
        worker: effectiveWorker,
        summary: errorMsg,
        error: errorMsg,
      });

      this.deps.messageHub.workerError(
        effectiveWorker,
        `任务执行失败: ${errorMsg}`,
      );

      batch?.markFailed(taskId, { success: false, summary: errorMsg, errors: [errorMsg] });

      // C-15: Worker 崩溃后状态清理
      try {
        const workerInstance = this.deps.missionOrchestrator.getWorker(effectiveWorker);
        workerInstance?.clearAllSessions();
      } catch { /* 清理失败不阻塞 */ }

      logger.error('编排工具.dispatch_task.Worker失败', {
        worker: effectiveWorker, taskId, error: errorMsg,
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private getNextReadyTaskForWorker(batch: DispatchBatch, worker: WorkerSlot): DispatchEntry | null {
    const ready = batch.getReadyTasks();
    for (const entry of ready) {
      if (entry.worker === worker) {
        return entry;
      }
    }
    return null;
  }

  /**
   * 合并同一 Batch 的连续调度触发，减少 Worker 指令卡片短时间抖动。
   */
  private scheduleDispatchReadyTasks(
    batch: DispatchBatch,
    options?: { immediate?: boolean; reason?: string },
  ): void {
    if (batch.status !== 'active') {
      return;
    }

    const existing = this.dispatchScheduleTimers.get(batch.id);
    if (existing) {
      clearTimeout(existing);
      this.dispatchScheduleTimers.delete(batch.id);
    }

    const delay = options?.immediate ? 0 : DispatchManager.DISPATCH_COALESCE_MS;
    const timer = setTimeout(() => {
      this.dispatchScheduleTimers.delete(batch.id);
      if (batch.status !== 'active') {
        return;
      }
      this.dispatchReadyTasksWithIsolation(batch);
    }, delay);

    this.dispatchScheduleTimers.set(batch.id, timer);
  }

  private clearDispatchScheduleTimers(batchId?: string): void {
    if (batchId) {
      const timer = this.dispatchScheduleTimers.get(batchId);
      if (timer) {
        clearTimeout(timer);
        this.dispatchScheduleTimers.delete(batchId);
      }
      return;
    }

    for (const timer of this.dispatchScheduleTimers.values()) {
      clearTimeout(timer);
    }
    this.dispatchScheduleTimers.clear();
  }

  private emitWorkerLaneInstructionCard(
    entry: DispatchEntry,
    worker: WorkerSlot,
    batch: DispatchBatch | null,
  ): void {
    if (!batch) {
      this.deps.messageHub.workerInstruction(worker, entry.task, {
        assignmentId: entry.taskId,
      });
      return;
    }

    const laneEntries = this.getWorkerLaneEntries(batch, worker);
    const laneTaskIds = laneEntries.map(item => item.taskId);
    const currentLaneIndex = laneTaskIds.indexOf(entry.taskId);
    const laneIndex = currentLaneIndex >= 0 ? currentLaneIndex + 1 : 1;
    const laneTotal = Math.max(1, laneEntries.length);
    const laneCardId = this.getWorkerLaneInstructionCardId(batch.id, worker);
    const laneId = `${batch.id}:${worker}`;

    this.deps.messageHub.workerInstruction(
      worker,
      this.buildWorkerLaneInstructionContent(laneEntries, entry.taskId),
      {
        assignmentId: entry.taskId,
        missionId: batch.id,
        laneId,
        laneCardId,
        laneIndex,
        laneTotal,
        laneTaskIds,
        laneCurrentTaskId: entry.taskId,
        laneTasks: laneEntries.map(item => ({
          taskId: item.taskId,
          title: item.task,
          status: item.status,
          dependsOn: item.dependsOn,
          isCurrent: item.taskId === entry.taskId,
        })),
      },
    );
  }

  private getWorkerLaneEntries(batch: DispatchBatch, worker: WorkerSlot): DispatchEntry[] {
    return batch
      .getEntries()
      .filter(entry => entry.worker === worker)
      .sort((a, b) => a.createdAt - b.createdAt);
  }

  private getWorkerLaneInstructionCardId(batchId: string, worker: WorkerSlot): string {
    return `worker-lane-instruction-${batchId}-${worker}`;
  }

  private buildWorkerLaneInstructionContent(entries: DispatchEntry[], currentTaskId: string): string {
    const current = entries.find(entry => entry.taskId === currentTaskId);
    const currentIndex = entries.findIndex(entry => entry.taskId === currentTaskId);
    const laneIndex = currentIndex >= 0 ? currentIndex + 1 : 1;
    const laneTotal = Math.max(entries.length, 1);
    const list = entries.length > 0 ? entries : [{
      taskId: currentTaskId,
      task: current?.task || '未知任务',
      status: 'running' as const,
      dependsOn: [] as string[],
    }];

    const lines = [
      '## Worker 任务队列',
      `当前执行：${current?.task || '未知任务'}`,
      `进度：${laneIndex}/${laneTotal}`,
      '说明：队列为实时快照，后续同 Worker 派发将自动并入此卡片。',
      '',
      '任务列表：',
      ...list.map((item, index) => {
        const dependsText = item.dependsOn.length > 0
          ? `（依赖: ${item.dependsOn.join(', ')}）`
          : '';
        return `${index + 1}. [${this.getWorkerLaneTaskStatusLabel(item.status, item.taskId === currentTaskId)}] ${item.task}${dependsText}`;
      }),
    ];

    return lines.join('\n');
  }

  private getWorkerLaneTaskStatusLabel(status: DispatchStatus, isCurrent: boolean): string {
    if (isCurrent) {
      return '进行中';
    }
    switch (status) {
      case 'completed':
        return '已完成';
      case 'failed':
        return '失败';
      case 'skipped':
        return '已跳过';
      case 'cancelled':
        return '已取消';
      case 'waiting_deps':
        return '等待依赖';
      case 'running':
        return '进行中';
      case 'pending':
      default:
        return '待执行';
    }
  }

  private launchWorkerLane(batch: DispatchBatch, worker: WorkerSlot): void {
    if (this.activeWorkerLanes.has(worker)) {
      return;
    }

    this.activeWorkerLanes.add(worker);
    logger.info('DispatchBatch.WorkerLane.启动', {
      batchId: batch.id,
      worker,
    }, LogCategory.ORCHESTRATOR);

    void (async () => {
      let executedCount = 0;
      try {
        while (batch.status === 'active') {
          const nextEntry = this.getNextReadyTaskForWorker(batch, worker);
          if (!nextEntry) {
            break;
          }
          executedCount += 1;
          await this.executeDispatchEntry(nextEntry, { emitWorkerInstruction: true });
        }
      } finally {
        this.activeWorkerLanes.delete(worker);
        logger.info('DispatchBatch.WorkerLane.结束', {
          batchId: batch.id,
          worker,
          executedCount,
          batchStatus: batch.status,
        }, LogCategory.ORCHESTRATOR);

        if (batch.status === 'active') {
          this.scheduleDispatchReadyTasks(batch, { immediate: true, reason: 'lane-finished' });
        }
      }
    })();
  }

  /**
   * 配置 DispatchBatch 事件处理
   */
  private setupBatchEventHandlers(batch: DispatchBatch): void {
    // 依赖就绪 → 通过隔离策略筛选后启动 Worker
    batch.on('task:ready', (_taskId: string, _entry: DispatchEntry) => {
      this.scheduleDispatchReadyTasks(batch, { reason: 'task-ready' });
    });

    // Worker 完成后重新检查是否有同类型排队任务可启动
    batch.on('task:statusChanged', (_taskId: string, status: DispatchStatus) => {
      if (isTerminalStatus(status)) {
        this.scheduleDispatchReadyTasks(batch, { reason: 'task-terminal' });

        // 反应式编排：将完成结果推入队列，唤醒 waitForWorkers
        const entry = batch.getEntry(_taskId);
        if (entry) {
          this.completionQueue.push(entry);
        }
      }
    });

    // 全部完成 → 根据编排模式决定后续行为
    batch.on('batch:allCompleted', (batchId: string, entries: DispatchEntry[]) => {
      const summary = batch.getSummary();
      logger.info('DispatchBatch.全部完成', { batchId, ...summary, reactiveMode: this.reactiveMode }, LogCategory.ORCHESTRATOR);
      const auditOutcome = this.ensureBatchAuditOutcome(batch, entries);

      if (this.reactiveMode) {
        // 反应式模式：编排者通过 wait_for_workers 接收结果并自行汇总
        // 标记等待主对话区最终汇总，直接归档，不触发 Phase C 汇总 LLM
        this.reactiveBatchAwaitingSummary.add(batchId);
        if (auditOutcome.level === 'intervention') {
          const blockedReport = this.buildInterventionReport(auditOutcome, entries);
          this.deps.messageHub.notify('反应式编排审计发现需干预项，已阻断自动交付', 'error');
          this.deps.messageHub.orchestratorMessage(blockedReport, { type: MessageType.RESULT });
          logger.warn('Reactive Phase 审计阻断交付', {
            batchId,
            auditOutcome,
          }, LogCategory.ORCHESTRATOR);
        }
        batch.archive();
      } else {
        this.reactiveBatchAwaitingSummary.delete(batchId);
        // 传统模式：自动触发 Phase C 汇总
        void this.triggerPhaseCSummary(batch, entries, auditOutcome);
      }
    });

    // Batch 被取消 → 不触发 Phase C，直接通知用户
    batch.on('batch:cancelled', (batchId: string, reason: string) => {
      this.reactiveBatchAwaitingSummary.delete(batchId);
      this.clearBatchTaskCategories(batch);
      this.activeWorkerLanes.clear();
      this.clearDispatchScheduleTimers(batchId);
      logger.info('DispatchBatch.已取消', { batchId, reason }, LogCategory.ORCHESTRATOR);
      this.deps.messageHub.orchestratorMessage(`任务已取消: ${reason}`);
    });

    batch.on('phase:changed', (_batchId: string, phase) => {
      if (phase === 'archived') {
        this.clearBatchTaskCategories(batch);
        this.activeWorkerLanes.clear();
        this.clearDispatchScheduleTimers(batch.id);
        this.clearResumeContext();
      }
    });
  }

  /**
   * 通过 Worker 隔离策略调度就绪任务
   */
  private dispatchReadyTasksWithIsolation(batch: DispatchBatch): void {
    if (batch.status !== 'active') return;

    const readyTasks = batch.getReadyTasksIsolated();
    const candidateWorkers = new Set<WorkerSlot>();
    for (const entry of readyTasks) {
      if (this.activeWorkerLanes.has(entry.worker)) {
        continue;
      }
      candidateWorkers.add(entry.worker);
    }

    for (const worker of candidateWorkers) {
      this.launchWorkerLane(batch, worker);
    }
  }

  /**
   * Phase C 汇总 — 所有 Worker 完成后触发 orchestrator 汇总 LLM 调用
   */
  private async triggerPhaseCSummary(
    batch: DispatchBatch,
    entries: DispatchEntry[],
    auditOutcome?: DispatchAuditOutcome,
  ): Promise<void> {
    const userPrompt = batch.userPrompt || this.deps.getActiveUserPrompt();
    if (!userPrompt) {
      logger.warn('Phase C 汇总: 无用户原始请求，跳过', undefined, LogCategory.ORCHESTRATOR);
      batch.archive();
      return;
    }

    try {
      this.deps.messageHub.progress('Summarizing', '正在汇总所有 Worker 的执行结果...');

      const finalAuditOutcome = auditOutcome || this.ensureBatchAuditOutcome(batch, entries);

      if (finalAuditOutcome.level === 'intervention') {
        const blockedReport = this.buildInterventionReport(finalAuditOutcome, entries);
        this.deps.messageHub.notify('Phase C 审计发现需干预项，已阻断自动交付', 'error');
        this.deps.messageHub.orchestratorMessage(blockedReport, { type: MessageType.RESULT });
        logger.warn('Phase C 审计阻断交付', {
          batchId: batch.id,
          auditOutcome: finalAuditOutcome,
        }, LogCategory.ORCHESTRATOR);
        return;
      }

      const summaryPrompt = `${buildDispatchSummaryPrompt(userPrompt, entries)}\n\n${this.buildAuditPromptAppendix(finalAuditOutcome)}`;

      const PHASE_C_TIMEOUT = 2 * 60 * 1000; // 2 分钟
      const response = await Promise.race([
        this.deps.adapterFactory.sendMessage(
          'orchestrator',
          summaryPrompt,
          undefined,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            visibility: 'system',
          }
        ),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error(`Phase C 汇总超时 (${PHASE_C_TIMEOUT / 1000}s)`)), PHASE_C_TIMEOUT)
        ),
      ]);

      this.deps.recordOrchestratorTokens(response.tokenUsage);

      if (response.error) {
        logger.error('Phase C 汇总 LLM 失败', { error: response.error }, LogCategory.ORCHESTRATOR);
        this.phaseCFallback(entries);
      } else {
        this.deps.messageHub.orchestratorMessage(response.content || '', { type: MessageType.RESULT });
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
    this.deps.messageHub.notify('汇总模型调用失败，以下为各 Worker 原始执行结果', 'warning');
    this.deps.messageHub.orchestratorMessage(lines.join('\n'), { type: MessageType.RESULT });
  }

  /**
   * 判断指定 Batch 是否处于“反应式模式且等待最终汇总”状态
   */
  isReactiveBatchAwaitingSummary(batchId: string): boolean {
    return this.reactiveBatchAwaitingSummary.has(batchId);
  }

  /**
   * 标记反应式 Batch 已完成最终汇总
   */
  markReactiveBatchSummarized(batchId: string): void {
    this.reactiveBatchAwaitingSummary.delete(batchId);
  }

  /**
   * 构建反应式编排的确定性兜底汇总
   *
   * 用于编排者未输出最终结论时，保证主对话区仍有可读结论。
   */
  buildReactiveBatchFallbackSummary(batch: DispatchBatch): string {
    const entries = batch.getEntries();
    const summary = batch.getSummary();
    const modifiedFiles = Array.from(new Set(entries.flatMap(e => e.result?.modifiedFiles || [])));

    const statusLabel = (status: DispatchStatus): string => {
      switch (status) {
        case 'completed': return '已完成';
        case 'failed': return '失败';
        case 'skipped': return '跳过';
        case 'cancelled': return '已取消';
        case 'running': return '执行中';
        case 'pending':
        case 'waiting_deps':
          return '等待中';
        default:
          return status;
      }
    };

    const taskLines = entries.map((entry, index) =>
      `${index + 1}. [${entry.worker}] ${entry.task} -> ${statusLabel(entry.status)}；${entry.result?.summary || '无结果摘要'}`
    );

    const lines = [
      `Worker 阶段执行完成（自动汇总）：共 ${summary.total} 项，成功 ${summary.completed} 项，失败 ${summary.failed} 项，跳过 ${summary.skipped} 项，取消 ${summary.cancelled} 项。`,
      ...taskLines,
      modifiedFiles.length > 0
        ? `涉及修改文件：${modifiedFiles.join('，')}`
        : '涉及修改文件：无',
    ];

    return lines.join('\n');
  }

  /**
   * Phase B+ — Worker 上报处理
   *
   * progress 类型：更新 subTaskCard，不触发 LLM
   * question 类型：触发 orchestrator 中间 LLM 调用
   * completed/failed 类型：由 DispatchBatch 状态机处理
   */
  private async handleDispatchWorkerReport(
    report: WorkerReport,
    batch: DispatchBatch | null,
  ): Promise<OrchestratorResponse> {
    const defaultResponse: OrchestratorResponse = { action: 'continue', timestamp: Date.now() };

    // 刷新 batch 活动时间戳，防止 idle 超时误判
    batch?.touchActivity();
    // progress 类型：更新 subTaskCard
    if (report.type === 'progress' && report.progress) {
      this.deps.messageHub.subTaskCard({
        id: report.assignmentId,
        title: report.progress.currentStep || '',
        status: 'running',
        worker: report.workerId,
        summary: `${report.progress.percentage}% - ${report.progress.currentStep}`,
      });
      return defaultResponse;
    }

    // question 类型：触发 Phase B+ 中间 LLM 调用
    if (report.type === 'question' && report.question) {
      const now = Date.now();
      if (now - this.lastPhaseBPlusTimestamp < DispatchManager.PHASE_B_PLUS_MIN_INTERVAL) {
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
${this.deps.getActiveUserPrompt()}

请决定：
1. 如果可以给出明确指令帮助 Worker 继续，请给出指令
2. 如果需要追加新的 Worker，可以调用 dispatch_task
3. 如果问题需要用户介入，请说明`;

        const response = await this.deps.adapterFactory.sendMessage(
          'orchestrator',
          prompt,
          undefined,
          {
            source: 'orchestrator',
            adapterRole: 'orchestrator',
            includeToolCalls: true,
            visibility: 'system',
          }
        );

        this.deps.recordOrchestratorTokens(response.tokenUsage);

        if (response.content) {
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

  // ===========================================================================
  // 反应式编排：完成结果队列 + waitForWorkers 阻塞机制
  // ===========================================================================

  /**
   * 等待 Worker 完成（阻塞直到指定任务或全部任务完成）
   *
   * 反应式编排的核心阻塞点：编排者 LLM 在工具循环中调用此方法，
   * 挂起直到 Worker 完成结果到达，然后基于结果决策下一步。
   */
  async waitForWorkers(taskIds?: string[]): Promise<WaitForWorkersResult> {
    this.reactiveMode = true;
    const batch = this.activeBatch;
    if (!batch) {
      return {
        results: [],
        wait_status: 'completed',
        timed_out: false,
        pending_task_ids: [],
        waited_ms: 0,
      };
    }

    const waitResult = await this.completionQueue.waitFor(batch, taskIds, {
      waitTimeoutMs: 10 * 60 * 1000,
      wakeupTimeoutMs: 30_000,
      onTimeout: (pendingTaskIds, elapsedMs) => {
        this.deps.messageHub.notify(
          `wait_for_workers 超时（${Math.round(elapsedMs / 1000)}s），仍有 ${pendingTaskIds.length} 个任务未完成`,
          'warning',
        );
      },
    });

    // 全量完成时回传程序化审计结果，供编排者在反应式模式下据此决策
    if (!waitResult.timed_out && waitResult.pending_task_ids.length === 0) {
      const auditOutcome = batch.getAuditOutcome();
      if (auditOutcome) {
        return {
          ...waitResult,
          audit: {
            level: auditOutcome.level,
            summary: auditOutcome.summary,
            issues: auditOutcome.issues.map(issue => ({
              task_id: issue.taskId,
              level: issue.level,
              dimension: issue.dimension,
              detail: issue.detail,
            })),
          },
        };
      }
    }

    return waitResult;
  }

  private clearBatchTaskCategories(batch: DispatchBatch): void {
    for (const entry of batch.getEntries()) {
      this.dispatchTaskCategories.delete(entry.taskId);
    }
  }

  private buildDelegationBriefing(input: {
    goal: string;
    acceptance: string[];
    constraints: string[];
    context: string[];
    scopeHint?: string[];
    files?: string[];
    predecessorContext?: string;
    collaborationContracts: DispatchCollaborationContracts;
  }): string {
    const lines: string[] = [
      `## 任务目标\n${input.goal}`,
      `## 验收标准\n${input.acceptance.map(item => `- ${item}`).join('\n')}`,
      `## 约束条件\n${input.constraints.map(item => `- ${item}`).join('\n')}`,
      `## 已知上下文\n${input.context.map(item => `- ${item}`).join('\n')}`,
    ];

    if (input.predecessorContext) {
      lines.push(input.predecessorContext);
    }

    if (input.scopeHint && input.scopeHint.length > 0) {
      lines.push(`## 范围线索（非硬约束）\n${input.scopeHint.map(item => `- ${item}`).join('\n')}`);
    }

    if (input.files && input.files.length > 0) {
      lines.push(`## 严格目标文件\n${input.files.map(item => `- ${item}`).join('\n')}`);
    }

    const contracts = input.collaborationContracts;
    if (contracts.producerContracts.length > 0 || contracts.consumerContracts.length > 0 || contracts.interfaceContracts.length > 0 || contracts.freezeFiles.length > 0) {
      lines.push('## 协作契约');
      if (contracts.producerContracts.length > 0) {
        lines.push(`- 生产契约: ${contracts.producerContracts.join('、')}`);
      }
      if (contracts.consumerContracts.length > 0) {
        lines.push(`- 消费契约: ${contracts.consumerContracts.join('、')}`);
      }
      if (contracts.interfaceContracts.length > 0) {
        lines.push(`- 接口约定: ${contracts.interfaceContracts.join('；')}`);
      }
      if (contracts.freezeFiles.length > 0) {
        lines.push(`- 冻结文件（禁止修改）: ${contracts.freezeFiles.join('、')}`);
      }
    }

    lines.push('## 执行要求\n先完成分析与方案判断，再执行实现与验证；禁止机械照搬步骤脚本。');
    return lines.join('\n\n');
  }

  /**
   * 收集前序任务结果并裁剪为精要上下文
   *
   * 信息裁剪原则：只传递摘要、关键决策和产出路径，
   * 不传递完整执行日志，控制下游 Worker 的上下文规模。
   */
  private buildPredecessorContext(taskId: string): string | undefined {
    const batch = this.activeBatch;
    if (!batch) return undefined;

    const entry = batch.getEntry(taskId);
    if (!entry || entry.dependsOn.length === 0) return undefined;

    const sections: string[] = [];
    for (const depId of entry.dependsOn) {
      const depEntry = batch.getEntry(depId);
      if (!depEntry?.result) continue;

      const depSummary = depEntry.result.summary || '无摘要';
      const depFiles = depEntry.result.modifiedFiles?.join('、') || '无';
      sections.push(`- **${depId}**（${depEntry.worker}）：${depSummary}；修改文件：${depFiles}`);
    }

    if (sections.length === 0) return undefined;
    return `## 前序任务结果\n以下是你依赖的前序任务的执行结果：\n${sections.join('\n')}`;
  }

  private buildScopeHintGuidance(scopeHint?: string[]): string {
    if (!scopeHint || scopeHint.length === 0) {
      return '';
    }
    return [
      '## 范围线索（非硬约束）',
      ...scopeHint.map(item => `- ${item}`),
      '',
      '你应优先从以上线索定位，但可根据任务需要自然扩展范围。',
    ].join('\n');
  }

  private shouldWarnMissingScopeHintForParallelTask(batch: DispatchBatch, dependsOn?: string[]): boolean {
    const dependencySet = new Set((dependsOn || []).map(id => id.trim()).filter(Boolean));
    return batch.getEntries().some(entry =>
      !isTerminalStatus(entry.status) && !dependencySet.has(entry.taskId)
    );
  }

  private normalizeCollaborationContracts(
    raw?: DispatchTaskCollaborationContracts,
  ): DispatchCollaborationContracts {
    const producerContracts = (raw?.producer_contracts || []).map(item => item.trim()).filter(Boolean);
    const consumerContracts = (raw?.consumer_contracts || []).map(item => item.trim()).filter(Boolean);
    const interfaceContracts = (raw?.interface_contracts || []).map(item => item.trim()).filter(Boolean);
    const freezeFiles = (raw?.freeze_files || []).map(item => item.trim()).filter(Boolean);
    return {
      producerContracts,
      consumerContracts,
      interfaceContracts,
      freezeFiles,
    };
  }

  private ensureBatchAuditOutcome(
    batch: DispatchBatch,
    entries: DispatchEntry[],
  ): DispatchAuditOutcome {
    const existing = batch.getAuditOutcome();
    if (existing) {
      return existing;
    }
    const computed = this.runStructuredAudit(entries);
    batch.setAuditOutcome(computed);
    return computed;
  }

  private runStructuredAudit(entries: DispatchEntry[]): DispatchAuditOutcome {
    const severityRank: Record<DispatchAuditLevel, number> = {
      normal: 0,
      watch: 1,
      intervention: 2,
    };

    const taskLevels = new Map<string, DispatchAuditLevel>();
    const issues: DispatchAuditIssue[] = [];
    const entryById = new Map(entries.map(entry => [entry.taskId, entry]));

    for (const entry of entries) {
      taskLevels.set(entry.taskId, 'normal');
    }

    const escalate = (
      taskId: string,
      level: DispatchAuditLevel,
      dimension: DispatchAuditIssue['dimension'],
      detail: string,
    ): void => {
      issues.push({ taskId, level, dimension, detail });
      const current = taskLevels.get(taskId) || 'normal';
      if (severityRank[level] > severityRank[current]) {
        taskLevels.set(taskId, level);
      }
    };

    for (const entry of entries) {
      const modifiedFiles = [...new Set((entry.result?.modifiedFiles || []).map(file => this.normalizePath(file)).filter(Boolean))];
      if (modifiedFiles.length === 0) {
        continue;
      }

      const strictFiles = new Set((entry.files || []).map(file => this.normalizePath(file)).filter(Boolean));
      if (strictFiles.size > 0) {
        const outOfStrictFiles = modifiedFiles.filter(file => !strictFiles.has(file));
        if (outOfStrictFiles.length > 0) {
          escalate(
            entry.taskId,
            'intervention',
            'scope',
            `改动超出严格目标文件：${outOfStrictFiles.join('、')}`,
          );
        }
      }

      if (entry.scopeHint.length > 0) {
        const outOfHintFiles = modifiedFiles.filter(file =>
          !entry.scopeHint.some(hint => this.pathMatchesHint(file, hint))
        );
        if (outOfHintFiles.length > 0) {
          escalate(
            entry.taskId,
            'watch',
            'scope',
            `改动超出 scope_hint 引导范围：${outOfHintFiles.join('、')}`,
          );
        }
      }

      const freezeFiles = new Set(entry.collaborationContracts.freezeFiles.map(file => this.normalizePath(file)).filter(Boolean));
      if (freezeFiles.size > 0) {
        const touchedFreezeFiles = modifiedFiles.filter(file => freezeFiles.has(file));
        if (touchedFreezeFiles.length > 0) {
          escalate(
            entry.taskId,
            'intervention',
            'contract',
            `触碰冻结文件：${touchedFreezeFiles.join('、')}`,
          );
        }
      }
    }

    const fileOwners = new Map<string, Set<string>>();
    for (const entry of entries) {
      for (const file of entry.result?.modifiedFiles || []) {
        const normalized = this.normalizePath(file);
        if (!normalized) continue;
        const owners = fileOwners.get(normalized) || new Set<string>();
        owners.add(entry.taskId);
        fileOwners.set(normalized, owners);
      }
    }

    for (const [file, ownerSet] of fileOwners) {
      const owners = Array.from(ownerSet);
      if (owners.length < 2) continue;
      for (let i = 0; i < owners.length; i++) {
        for (let j = i + 1; j < owners.length; j++) {
          const a = owners[i];
          const b = owners[j];
          const hasAtoB = this.hasDependencyChain(a, b, entryById, new Set());
          const hasBtoA = this.hasDependencyChain(b, a, entryById, new Set());
          if (!hasAtoB && !hasBtoA) {
            escalate(a, 'intervention', 'cross_task', `与任务 ${b} 在文件 ${file} 产生并行冲突（无依赖串行化）`);
            escalate(b, 'intervention', 'cross_task', `与任务 ${a} 在文件 ${file} 产生并行冲突（无依赖串行化）`);
          }
        }
      }
    }

    const summary = { normal: 0, watch: 0, intervention: 0 };
    for (const level of taskLevels.values()) {
      summary[level] += 1;
    }

    const level: DispatchAuditLevel =
      summary.intervention > 0 ? 'intervention'
        : summary.watch > 0 ? 'watch'
          : 'normal';

    return {
      level,
      issues,
      taskLevels: Object.fromEntries(taskLevels.entries()),
      summary,
    };
  }

  private buildAuditPromptAppendix(auditOutcome: DispatchAuditOutcome): string {
    const issueLines = auditOutcome.issues
      .map(issue => `- [${issue.level}] ${issue.taskId} (${issue.dimension}): ${issue.detail}`)
      .join('\n') || '- 无';

    return [
      '## 程序化审计结果（系统判定）',
      `总体级别: ${auditOutcome.level}`,
      `任务分布: 正常 ${auditOutcome.summary.normal}，需关注 ${auditOutcome.summary.watch}，需干预 ${auditOutcome.summary.intervention}`,
      '问题列表:',
      issueLines,
      '请在总结中严格遵循以上审计结果，不要降级“需干预”项。',
    ].join('\n');
  }

  private buildInterventionReport(
    auditOutcome: DispatchAuditOutcome,
    entries: DispatchEntry[],
  ): string {
    const titleByTaskId = new Map(entries.map(entry => [entry.taskId, entry.task]));
    const interventionIssues = auditOutcome.issues.filter(issue => issue.level === 'intervention');
    const watchIssues = auditOutcome.issues.filter(issue => issue.level === 'watch');

    const interventionLines = interventionIssues.length > 0
      ? interventionIssues.map((issue, index) =>
        `${index + 1}. ${issue.taskId}（${titleByTaskId.get(issue.taskId) || '未知任务'}）- ${issue.detail}`
      ).join('\n')
      : '无';

    const watchLines = watchIssues.length > 0
      ? watchIssues.map((issue, index) =>
        `${index + 1}. ${issue.taskId}（${titleByTaskId.get(issue.taskId) || '未知任务'}）- ${issue.detail}`
      ).join('\n')
      : '无';

    return [
      '## Phase C 审计结论',
      '结果：检测到“需干预”问题，已阻断自动交付。',
      '',
      '### 需干预',
      interventionLines,
      '',
      '### 需关注',
      watchLines,
      '',
      '### 建议动作',
      '1. 对需干预任务追加修复任务（建议串行化并补充 contracts.freeze_files / depends_on）',
      '2. 重新执行后再进入 Phase C 汇总',
    ].join('\n');
  }

  private hasDependencyChain(
    taskId: string,
    targetTaskId: string,
    entryById: Map<string, DispatchEntry>,
    visited: Set<string>,
  ): boolean {
    if (visited.has(taskId)) {
      return false;
    }
    visited.add(taskId);
    const entry = entryById.get(taskId);
    if (!entry) {
      return false;
    }
    if (entry.dependsOn.includes(targetTaskId)) {
      return true;
    }
    return entry.dependsOn.some(depId => this.hasDependencyChain(depId, targetTaskId, entryById, visited));
  }

  private normalizePath(input: string): string {
    return input.replace(/\\/g, '/').trim().replace(/^\.\//, '').replace(/\/+$/, '');
  }

  private pathMatchesHint(filePath: string, hintPath: string): boolean {
    const file = this.normalizePath(filePath);
    const hint = this.normalizePath(hintPath);
    if (!file || !hint) {
      return false;
    }
    if (file === hint || file.endsWith(`/${hint}`)) {
      return true;
    }
    return file.startsWith(`${hint}/`) || file.includes(`/${hint}/`);
  }

  dispose(): void {
    this.activeBatch = null;
    this._planningExecutor = null;
    this.completionQueue.reset();
    this.reactiveMode = false;
    this.reactiveBatchAwaitingSummary.clear();
    this.missionWorkerSessions.clear();
    this.activeResumeContexts.clear();
    this.dispatchTaskCategories.clear();
    this.runtimeUnavailableWorkers.clear();
    this.activeWorkerLanes.clear();
    this.clearDispatchScheduleTimers();
  }
}
