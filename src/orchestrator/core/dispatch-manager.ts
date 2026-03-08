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
import { t } from '../../i18n';
import type { WorkerSlot } from '../../types';
import type { TokenUsage } from '../../types/agent-types';
import type { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import type { ProfileLoader } from '../profile/profile-loader';
import type { MessageHub } from './message-hub';
import type { MissionOrchestrator } from './mission-orchestrator';
import type { Assignment } from '../mission';
import type { WorkerReport, OrchestratorResponse } from '../protocols/worker-report';
import { createAbortResponse, createAdjustResponse } from '../protocols/worker-report';
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
import { DispatchRoutingService, type DispatchRoutingDecision } from './dispatch-routing-service';
import { DispatchResumeContextStore } from './dispatch-resume-context-store';
import { DispatchIdempotencyStore } from './dispatch-idempotency-store';
import { isModelOriginIssue, toModelOriginUserMessage } from '../../errors/model-origin';
import { trackModelOriginEvent } from '../../errors/model-origin-observability';
import { createHash } from 'crypto';

type DispatchAckState = 'pending' | 'acked' | 'nacked';

interface DispatchExecutionProtocolState {
  taskId: string;
  batchId: string;
  worker: WorkerSlot;
  dispatchAttemptId: string;
  idempotencyKey: string;
  leaseId: string;
  leaseExpireAt: number;
  heartbeatAt: number;
  ackState: DispatchAckState;
  createdAt: number;
  ackAt?: number;
  nackReason?: string;
  timeoutTriggered: boolean;
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
  /** 获取当前对话轮次唯一标识（用于快照 missionId 分组） */
  getCurrentTurnId: () => string | null;
  getProjectKnowledgeBase: () => import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase | undefined;
  /** Worker 终态报告的 Wisdom 提取与持久化入口（由 MissionDrivenEngine 注入） */
  processWorkerWisdom: (report: WorkerReport) => void;
  // 治理依赖（WorkerPipeline 使用）
  getSnapshotManager: () => SnapshotManager | null;
  getContextManager: () => import('../../context/context-manager').ContextManager | null;
  getTodoManager: () => import('../../todo').TodoManager | null;
  // Token 统计
  recordOrchestratorTokens: (usage?: TokenUsage, phase?: 'planning' | 'verification') => void;
  recordWorkerTokenUsage: (results: Map<string, import('../worker').AutonomousExecutionResult>) => void;
  // 补充指令队列（反应式编排：运行时注入 Worker 指令）
  getSupplementaryQueue: () => SupplementaryInstructionQueue | null;
  // Plan Ledger 回写（dispatch 注册时落账）
  onDispatchTaskRegistered?: (payload: {
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
  }) => Promise<void> | void;
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
  private static readonly MAX_MISSION_SESSION_RECORDS = 100;
  private static readonly ACK_TIMEOUT_MS = 20_000;
  private static readonly HEARTBEAT_INTERVAL_MS = 10_000;
  private static readonly LEASE_TTL_MS = 60_000;
  private static readonly LEASE_WATCH_INTERVAL_MS = 2_000;
  private static readonly EXECUTION_TIMEOUT_MS = 20 * 60 * 1000;

  // Phase B+ 中间调用频率限制：按 batch 隔离，避免跨批次互相污染
  private phaseBPlusTimestamps = new Map<string, number>();
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
  /** Worker 路由与可用性判定服务 */
  private readonly routingService: DispatchRoutingService;
  /** Resume 上下文存储 */
  private readonly resumeContextStore: DispatchResumeContextStore;
  /** 记录 dispatch task 的分类（用于可解释性与后续诊断） */
  private dispatchTaskCategories = new Map<string, string>();
  /** dispatch 幂等账本（跨进程重放去重） */
  private readonly dispatchIdempotencyStore: DispatchIdempotencyStore;
  /** 活跃的 Assignment 映射（Worker 执行期间可查，供 split_todo handler 使用） */
  private activeAssignments = new Map<string, Assignment>();
  /** Worker Lane 运行态：同一 Worker 同一时刻仅允许一个执行链 */
  private activeWorkerLanes = new Set<WorkerSlot>();
  /** Batch 级调度合并定时器 */
  private dispatchScheduleTimers = new Map<string, NodeJS.Timeout>();
  /** 执行协议状态（ack/nack + lease + heartbeat） */
  private executionProtocolStates = new Map<string, DispatchExecutionProtocolState>();
  private leaseWatcherTimer?: NodeJS.Timeout;

  constructor(private deps: DispatchManagerDeps) {
    this.routingService = new DispatchRoutingService(
      this.deps.profileLoader,
      DispatchManager.WORKER_SLOTS,
      DispatchManager.WORKER_FALLBACK_PRIORITY,
      DispatchManager.RUNTIME_UNAVAILABLE_COOLDOWN_MS,
    );
    this.resumeContextStore = new DispatchResumeContextStore(
      DispatchManager.MAX_MISSION_SESSION_RECORDS,
    );
    this.dispatchIdempotencyStore = new DispatchIdempotencyStore(this.deps.workspaceRoot);
    this.setupMissionEventListeners();
    this.startLeaseWatcher();
  }

  /**
   * 订阅 MissionOrchestrator 的 Todo/Insight 事件，
   * 将进度信息直接通过 MessageHub 发送到前端 SubTaskCard
   */
  private setupMissionEventListeners(): void {
    const mo = this.deps.missionOrchestrator;

    mo.on('todoStarted', ({ assignmentId, content }: { assignmentId: string; content: string }) => {
      this.reportTodoProgress(assignmentId, t('dispatch.todo.started', { content }));
    });

    mo.on('todoCompleted', ({ assignmentId, content }: { assignmentId: string; content: string }) => {
      this.reportTodoProgress(assignmentId, t('dispatch.todo.completed', { content }));
    });

    mo.on('todoFailed', ({ assignmentId, content, error }: { assignmentId: string; content: string; error?: string }) => {
      this.reportTodoProgress(assignmentId, t('dispatch.todo.failed', {
        content,
        error: error || t('dispatch.common.unknownError'),
      }));
    });

    mo.on('assignmentStarted', ({ assignmentId, workerId }: { assignmentId: string; workerId?: WorkerSlot }) => {
      this.markProtocolAck(assignmentId, workerId);
    });

    mo.on('workerHeartbeat', ({
      assignmentId,
      workerId,
      timestamp,
    }: {
      assignmentId: string;
      workerId: WorkerSlot;
      timestamp: number;
      todoId?: string;
      sessionId?: string;
    }) => {
      this.updateProtocolHeartbeat(assignmentId, workerId, timestamp);
    });

    mo.on('insightGenerated', ({ workerId, type, content, importance }: { workerId: string; type: string; content: string; importance: string }) => {
      const typeLabels: Record<string, string> = {
        decision: t('dispatch.insight.type.decision'),
        contract: t('dispatch.insight.type.contract'),
        risk: t('dispatch.insight.type.risk'),
        constraint: t('dispatch.insight.type.constraint'),
      };
      const label = typeLabels[type] || type;
      const level = importance === 'critical' ? 'warning' : 'info';
      this.deps.messageHub.notify(t('dispatch.insight.notification', { workerId, label, content }), level);
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
        throw new Error(t('dispatch.errors.todoManagerNotInitialized'));
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
    return this.routingService.getWorkerAvailability();
  }

  /**
   * 新一轮执行前重置调度状态
   *
   * 目的：彻底切断上一轮归档 batch、完成队列与反应式标记，
   * 避免“无 dispatch 的新一轮”被误判为存在历史 dispatch。
   */
  resetForNewExecutionCycle(): void {
    if (this.activeBatch?.status === 'active') {
      this.activeBatch.cancelAll(t('dispatch.batch.resetCancelReason'));
    }

    if (this.activeBatch) {
      this.reactiveBatchAwaitingSummary.delete(this.activeBatch.id);
      this.clearBatchTaskCategories(this.activeBatch);
    }

    this.activeBatch = null;
    this.reactiveMode = false;
    this.phaseBPlusTimestamps.clear();
    this.completionQueue.reset();
    this.activeWorkerLanes.clear();
    this.clearAllProtocolStates();
    this.clearDispatchScheduleTimers();
    this.clearResumeContext();
  }

  private resolveDispatchRouting(
    goal: string,
    explicitCategory?: string,
  ): { ok: true; decision: DispatchRoutingDecision } | { ok: false; error: string } {
    return this.routingService.resolveDispatchRouting(goal, explicitCategory);
  }

  private resolveExecutionWorker(
    preferredWorker: WorkerSlot,
    options?: {
      busyWorkers?: Set<WorkerSlot>;
      excludedWorkers?: Set<WorkerSlot>;
      allowBusyFallback?: boolean;
    },
  ): { ok: true; selectedWorker: WorkerSlot; degraded: boolean; routingReason: string } | { ok: false; error: string } {
    return this.routingService.resolveExecutionWorker(preferredWorker, options);
  }

  activateResumeContext(sourceMissionId: string, resumePrompt?: string): boolean {
    const currentSessionId = this.deps.getCurrentSessionId();
    if (!currentSessionId) {
      return false;
    }
    const result = this.resumeContextStore.activate(currentSessionId, sourceMissionId, resumePrompt);
    if (!result.ok) {
      return false;
    }

    logger.info('Dispatch.ResumeContext.已激活', {
      sessionId: currentSessionId,
      sourceMissionId,
      workerCount: result.workerCount,
    }, LogCategory.ORCHESTRATOR);

    return true;
  }

  clearResumeContext(): void {
    this.resumeContextStore.clear(this.deps.getCurrentSessionId());
  }

  private getResumeContextForWorker(worker: WorkerSlot): { resumeSessionId?: string; resumePrompt?: string } {
    const currentSessionId = this.deps.getCurrentSessionId();
    if (!currentSessionId) {
      return {};
    }
    return this.resumeContextStore.getForWorker(currentSessionId, worker);
  }

  private recordMissionWorkerSession(
    missionId: string,
    worker: WorkerSlot,
    workerSessionId: string,
  ): void {
    this.resumeContextStore.recordWorkerSession(missionId, worker, workerSessionId);
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
        const { task_name, goal, acceptance, constraints, context, files, scopeHint, dependsOn, category, requiresModification, contracts, idempotencyKey } = params;
        if (typeof requiresModification !== 'boolean') {
          return {
            task_id: '',
            status: 'failed' as const,
            error: t('dispatch.errors.requiresModificationBoolean'),
          };
        }
        const taskName = typeof task_name === 'string' ? task_name.trim() : '';
        const goalText = typeof goal === 'string' ? goal.trim() : '';
        if (!taskName && !goalText) {
          return {
            task_id: '',
            status: 'failed' as const,
            error: t('dispatch.errors.taskNameOrGoalRequired'),
          };
        }
        const taskTitle = taskName || goalText;
        const normalizedAcceptance = Array.isArray(acceptance)
          ? acceptance.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
          : [];
        const normalizedConstraints = Array.isArray(constraints)
          ? constraints.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
          : [];
        const normalizedContext = Array.isArray(context)
          ? context.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
          : [];
        if (normalizedAcceptance.length === 0 || normalizedConstraints.length === 0 || normalizedContext.length === 0) {
          return {
            task_id: '',
            status: 'failed' as const,
            error: t('dispatch.errors.acceptanceConstraintsContextRequired'),
          };
        }
        const scopeHintValues = Array.isArray(scopeHint)
          ? scopeHint.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
          : undefined;
        const filesValues = Array.isArray(files)
          ? files.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
          : undefined;
        const dependsOnValues = Array.isArray(dependsOn)
          ? dependsOn.filter((item): item is string => typeof item === 'string' && item.trim().length > 0).map(item => item.trim())
          : undefined;
        const normalizedScopeHint = scopeHintValues && scopeHintValues.length > 0 ? scopeHintValues : undefined;
        const normalizedFiles = filesValues && filesValues.length > 0 ? filesValues : undefined;
        const normalizedDependsOn = dependsOnValues && dependsOnValues.length > 0 ? dependsOnValues : undefined;
        const collaborationContracts = this.normalizeCollaborationContracts(contracts);
        logger.info('编排工具.dispatch_task.开始', {
          category,
          requiresModification,
          scopeHintCount: normalizedScopeHint?.length || 0,
          goalPreview: taskTitle.substring(0, 80),
          acceptanceCount: normalizedAcceptance.length,
          constraintCount: normalizedConstraints.length,
          contextCount: normalizedContext.length,
          dependsOn: normalizedDependsOn,
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
            t('dispatch.notify.routingAdjusted', {
              category: decision.category,
              selectedWorker: decision.selectedWorker,
              routingReason: decision.routingReason,
            }),
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
            error: t('dispatch.errors.createTaskFailed', { error: error?.message || String(error) }),
          };
        }

        const resolvedSessionId = this.deps.getCurrentSessionId()?.trim() || '';
        const sessionScopeForIdempotency = resolvedSessionId || 'unknown-session';
        const effectiveIdempotencyKey = this.buildDispatchIdempotencyKey({
          sessionId: sessionScopeForIdempotency,
          missionId,
          providedKey: idempotencyKey,
          category: decision.category,
          taskName: taskTitle,
          goal: goalText || taskTitle,
          acceptance: normalizedAcceptance,
          constraints: normalizedConstraints,
          context: normalizedContext,
          scopeHint: normalizedScopeHint,
          files: normalizedFiles,
          dependsOn: normalizedDependsOn,
        });
        // 先生成候选 taskId，再通过 claimOrGet 做原子幂等占位，避免跨实例 TOCTOU 竞争。
        const taskId = `dispatch-${Date.now()}-${decision.selectedWorker}-${Math.random().toString(36).substring(2, 5)}`;
        let idempotencyClaim: ReturnType<DispatchIdempotencyStore['claimOrGet']>;
        try {
          idempotencyClaim = this.dispatchIdempotencyStore.claimOrGet({
            key: effectiveIdempotencyKey,
            sessionId: sessionScopeForIdempotency,
            missionId,
            taskId,
            worker: decision.selectedWorker,
            category: decision.category,
            taskName: taskTitle,
            routingReason: decision.routingReason,
            degraded: decision.degraded,
            status: 'dispatched',
          });
        } catch (idempotencyError: any) {
          logger.warn('编排工具.dispatch_task.幂等占位失败', {
            idempotencyKey: effectiveIdempotencyKey,
            sessionId: sessionScopeForIdempotency,
            missionId,
            error: idempotencyError?.message || String(idempotencyError),
          }, LogCategory.ORCHESTRATOR);
          return {
            task_id: '',
            status: 'failed' as const,
            error: `dispatch 幂等占位失败：${idempotencyError?.message || String(idempotencyError)}`,
          };
        }
        if (!idempotencyClaim.claimed) {
          const existingIdempotent = idempotencyClaim.record;
          const inCurrentBatch = this.activeBatch?.getEntry(existingIdempotent.taskId);
          if (!inCurrentBatch) {
            const blockedReason = `idempotency_key 命中历史派发(${existingIdempotent.taskId})，但当前批次不可恢复该任务，已阻止重复派发`;
            logger.warn('编排工具.dispatch_task.幂等重放阻断', {
              idempotencyKey: effectiveIdempotencyKey,
              existingTaskId: existingIdempotent.taskId,
              existingStatus: existingIdempotent.status,
              sessionId: sessionScopeForIdempotency,
              missionId,
            }, LogCategory.ORCHESTRATOR);
            return {
              task_id: existingIdempotent.taskId,
              status: 'failed' as const,
              worker: existingIdempotent.worker,
              category: existingIdempotent.category,
              routing_reason: existingIdempotent.routingReason,
              degraded: existingIdempotent.degraded,
              error: blockedReason,
            };
          }

          logger.info('编排工具.dispatch_task.幂等复用', {
            idempotencyKey: effectiveIdempotencyKey,
            taskId: existingIdempotent.taskId,
            status: existingIdempotent.status,
            sessionId: sessionScopeForIdempotency,
            missionId,
          }, LogCategory.ORCHESTRATOR);
          return {
            task_id: existingIdempotent.taskId,
            status: 'dispatched' as const,
            worker: existingIdempotent.worker,
            category: existingIdempotent.category,
            routing_reason: `${existingIdempotent.routingReason} [idempotency_reused]`,
            degraded: existingIdempotent.degraded,
          };
        }
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
          if ((!normalizedScopeHint || normalizedScopeHint.length === 0) && this.shouldWarnMissingScopeHintForParallelTask(this.activeBatch, normalizedDependsOn)) {
            throw new Error(t('dispatch.errors.parallelScopeHintRequired'));
          }

          this.activeBatch.register({
            taskId,
            worker: decision.selectedWorker,
            goal: taskTitle,
            acceptance: normalizedAcceptance,
            constraints: normalizedConstraints,
            context: normalizedContext,
            task: taskTitle,
            scopeHint: normalizedScopeHint,
            files: normalizedFiles,
            requiresModification,
            dependsOn: normalizedDependsOn,
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
          this.dispatchIdempotencyStore.removeByTaskId(taskId);
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

        // 发送 subTaskCard（状态基于注册后的真实 DispatchStatus）
        const entry = this.activeBatch.getEntry(taskId);
        const entryStatus = entry
          ? entry.status
          : (normalizedDependsOn && normalizedDependsOn.length > 0 ? 'waiting_deps' : 'pending');
        const cardStatus = this.mapDispatchStatusToInitialCardStatus(entryStatus);

        if (resolvedSessionId && this.deps.onDispatchTaskRegistered) {
          try {
            await this.deps.onDispatchTaskRegistered({
              sessionId: resolvedSessionId,
              missionId,
              taskId,
              worker: decision.selectedWorker,
              title: taskTitle,
              category: decision.category,
              dependsOn: normalizedDependsOn,
              scopeHint: normalizedScopeHint,
              files: normalizedFiles,
              requiresModification,
            });
          } catch (ledgerError) {
            logger.warn('编排工具.dispatch_task.计划账本回写失败', {
              taskId,
              missionId,
              error: ledgerError instanceof Error ? ledgerError.message : String(ledgerError),
            }, LogCategory.ORCHESTRATOR);
          }
        }
        if (entryStatus === 'skipped') {
          this.dispatchIdempotencyStore.updateStatusByTaskId(taskId, 'cancelled');
        }

        this.deps.messageHub.subTaskCard({
          id: taskId,
          title: taskTitle,
          status: cardStatus,
          worker: decision.selectedWorker,
          ...(entryStatus === 'skipped' && entry?.result?.summary
            ? { summary: entry.result.summary }
            : {}),
        });

        // 通过隔离策略决定是否立即启动（约束 5）
        if (entryStatus === 'pending') {
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

        const queue = this.deps.getSupplementaryQueue();
        const delivered = queue ? queue.inject(message, true, worker) : false;
        if (!delivered) {
          logger.warn('编排工具.send_worker_message.运行时注入失败', {
            worker,
            messagePreview: message.substring(0, 80),
          }, LogCategory.ORCHESTRATOR);
        }
        this.deps.messageHub.workerInstruction(worker, message);
        return { delivered };
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
            error: t('dispatch.errors.assignmentNotActive', { assignmentId: callerContext.assignmentId }),
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
              error: t('dispatch.errors.splitDepthExceeded'),
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
            error: t('dispatch.errors.todoManagerNotInitialized'),
          };
        }

        const childTodoIds: string[] = [];
        const currentSessionId = this.deps.getCurrentSessionId()?.trim();
        if (!currentSessionId) {
          return {
            success: false,
            childTodoIds: [],
            error: t('dispatch.errors.splitTodoMissingSessionContext'),
          };
        }
        for (const subtask of subtasks) {
          const child = await todoManager.create({
            sessionId: currentSessionId,
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
          throw new Error(t('dispatch.errors.todoManagerNotInitializedGetTodos'));
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
          throw new Error(t('dispatch.errors.workerMissingMissionContext'));
        }

        const sessionId = explicitSessionId
          || extractSessionId(explicitMissionId)
          || extractSessionId(callerMissionId)
          || this.deps.getCurrentSessionId();
        if (!sessionId) {
          throw new Error(t('dispatch.errors.sessionNotFoundForTodos'));
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
          return { success: false, error: t('dispatch.errors.todoManagerNotInitializedUpdateTodo') };
        }

        try {
          if (!params.updates || params.updates.length === 0) {
            return { success: false, error: t('dispatch.errors.updateTodoMissingUpdates') };
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
              throw new Error(t('dispatch.errors.updateTodoMissingTodoId'));
            }

            const hasStatus = update.status !== undefined;
            const hasContent = update.content !== undefined;
            if (!hasStatus && !hasContent) {
              throw new Error(t('dispatch.errors.updateTodoMissingFields', { todoId: update.todoId }));
            }

            if (update.status !== undefined && !allowedStatus.has(update.status as UpdateTodoStatus)) {
              throw new Error(t('dispatch.errors.updateTodoInvalidStatus', {
                todoId: update.todoId,
                status: update.status,
              }));
            }

            const todo = await todoManager.get(update.todoId);
            if (!todo) {
              throw new Error(`Todo not found: ${update.todoId}`);
            }

            const targetStatus = update.status as UpdateTodoStatus | undefined;
            if (targetStatus === 'pending' && !pendingAllowedSource.has(todo.status)) {
              throw new Error(t('dispatch.errors.updateTodoCannotResetPending', {
                todoId: update.todoId,
                currentStatus: todo.status,
              }));
            }

            if (targetStatus === 'skipped' && !skippedAllowedSource.has(todo.status)) {
              throw new Error(t('dispatch.errors.updateTodoCannotSetSkipped', {
                todoId: update.todoId,
                currentStatus: todo.status,
              }));
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
      const errorMsg = t('dispatch.errors.dispatchCategoryMissing', { taskId });
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
        t('dispatch.notify.executionRoutingAdjusted', {
          taskId,
          fromWorker: worker,
          toWorker: effectiveWorker,
          routingReason: executionRouting.routingReason,
        }),
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
    const protocolState = this.registerProtocolState(taskId, batch?.id, effectiveWorker);

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

      const currentSessionId = this.deps.getCurrentSessionId()?.trim();
      if (!currentSessionId) {
        this.markProtocolNack(taskId, 'missing-session-context');
        const errorMsg = t('dispatch.errors.currentSessionMissingCannotCreateTodo');
        this.deps.messageHub.subTaskCard({
          id: taskId,
          title: task,
          status: 'failed',
          worker: effectiveWorker,
          summary: errorMsg,
          error: errorMsg,
        });
        batch?.markFailed(taskId, { success: false, summary: errorMsg, errors: [errorMsg] });
        return;
      }

      // 一级 Todo 由 PlanningExecutor 统一创建（编排层唯一入口）
      await this.getPlanningExecutor().createMacroTodo(missionId, currentSessionId, assignment);

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
        const currentSessionId = this.deps.getCurrentSessionId();
        pipelineResult = await this.pipeline.execute({
          assignment,
          workerInstance,
          adapterFactory: this.deps.adapterFactory,
          workspaceRoot: this.deps.workspaceRoot,
          projectContext,
          missionId: this.deps.getCurrentTurnId() || missionId,
          sessionId: currentSessionId,
          onReport: (report) => this.handleDispatchWorkerReport(report, batch),
          cancellationToken: batch?.cancellationToken,
          heartbeatIntervalMs: DispatchManager.HEARTBEAT_INTERVAL_MS,
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
      this.routingService.clearWorkerRuntimeUnavailable(effectiveWorker);

      // 直接使用 Worker 生成的结构化总结（唯一生产者：AutonomousWorker.buildStructuredSummary）
      const verificationWarnings = result.verification.degraded
        ? (result.verification.warnings.length > 0 ? result.verification.warnings : [t('dispatch.verification.degradedFallbackWarning')])
        : [];
      const summary = result.summary;
      const modifiedFiles = [...new Set([
        ...result.completedTodos.flatMap(t => t.output?.modifiedFiles || []),
        ...result.failedTodos.flatMap(t => t.output?.modifiedFiles || []),
      ])];

      if (verificationWarnings.length > 0) {
        this.deps.messageHub.notify(
          t('dispatch.notify.verificationDegraded', { taskId, warning: verificationWarnings[0] }),
          'warning',
        );
      }

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
        quality: {
          verificationDegraded: result.verification.degraded,
          warnings: verificationWarnings,
        },
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
        this.markProtocolNack(taskId, error.message || 'cancelled');
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

      const rawErrorMsg = error?.message || String(error);
      const userErrorMsg = toModelOriginUserMessage(rawErrorMsg).trim() || rawErrorMsg;
      this.markProtocolNack(taskId, userErrorMsg);
      if (isModelOriginIssue(rawErrorMsg) || isModelOriginIssue(userErrorMsg)) {
        trackModelOriginEvent('surfaced', 'dispatch:worker-failed', rawErrorMsg, {
          worker: effectiveWorker,
          taskId,
        });
      }
      if (this.routingService.shouldMarkRuntimeUnavailable(rawErrorMsg)) {
        this.routingService.markWorkerRuntimeUnavailable(effectiveWorker, rawErrorMsg);
        logger.warn('Dispatch.Worker.运行时不可用.已标记冷却', {
          worker: effectiveWorker,
          taskId,
          reason: rawErrorMsg,
          surfacedReason: userErrorMsg,
        }, LogCategory.ORCHESTRATOR);
      } else {
        logger.warn('Dispatch.Worker.业务失败.不标记冷却', {
          worker: effectiveWorker,
          taskId,
          reason: rawErrorMsg,
          surfacedReason: userErrorMsg,
        }, LogCategory.ORCHESTRATOR);
      }

      this.deps.messageHub.subTaskCard({
        id: taskId,
        title: task,
        status: 'failed',
        worker: effectiveWorker,
        summary: userErrorMsg,
        error: userErrorMsg,
      });

      this.deps.messageHub.workerError(
        effectiveWorker,
        userErrorMsg,
      );

      batch?.markFailed(taskId, { success: false, summary: userErrorMsg, errors: [userErrorMsg] });

      // C-15: Worker 崩溃后状态清理
      try {
        const workerInstance = this.deps.missionOrchestrator.getWorker(effectiveWorker);
        workerInstance?.clearAllSessions();
      } catch { /* 清理失败不阻塞 */ }

      logger.error('编排工具.dispatch_task.Worker失败', {
        worker: effectiveWorker,
        taskId,
        error: rawErrorMsg,
        surfacedError: userErrorMsg,
        dispatchAttemptId: protocolState.dispatchAttemptId,
      }, LogCategory.ORCHESTRATOR);
    } finally {
      this.clearProtocolState(taskId);
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

  private mapDispatchStatusToInitialCardStatus(status: DispatchStatus): 'pending' | 'running' | 'completed' | 'failed' | 'stopped' | 'skipped' {
    switch (status) {
      case 'waiting_deps':
        return 'pending';
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'skipped':
        return 'skipped';
      case 'cancelled':
        return 'stopped';
      case 'running':
      case 'pending':
      default:
        return 'running';
    }
  }

  private buildWorkerLaneInstructionContent(entries: DispatchEntry[], currentTaskId: string): string {
    const current = entries.find(entry => entry.taskId === currentTaskId);
    const currentIndex = entries.findIndex(entry => entry.taskId === currentTaskId);
    const laneIndex = currentIndex >= 0 ? currentIndex + 1 : 1;
    const laneTotal = Math.max(entries.length, 1);
    const list = entries.length > 0 ? entries : [{
      taskId: currentTaskId,
      task: current?.task || t('dispatch.lane.unknownTask'),
      status: 'running' as const,
      dependsOn: [] as string[],
    }];

    const lines = [
      t('dispatch.lane.header'),
      t('dispatch.lane.currentTask', { task: current?.task || t('dispatch.lane.unknownTask') }),
      t('dispatch.lane.progress', { laneIndex, laneTotal }),
      t('dispatch.lane.description'),
      '',
      t('dispatch.lane.taskList'),
      ...list.map((item, index) => {
        const dependsText = item.dependsOn.length > 0
          ? t('dispatch.lane.dependsOn', { dependsOn: item.dependsOn.join(', ') })
          : '';
        return `${index + 1}. [${this.getWorkerLaneTaskStatusLabel(item.status, item.taskId === currentTaskId)}] ${item.task}${dependsText}`;
      }),
    ];

    return lines.join('\n');
  }

  private getWorkerLaneTaskStatusLabel(status: DispatchStatus, isCurrent: boolean): string {
    if (isCurrent) {
      return t('dispatch.lane.status.running');
    }
    switch (status) {
      case 'completed':
        return t('dispatch.lane.status.completed');
      case 'failed':
        return t('dispatch.lane.status.failed');
      case 'skipped':
        return t('dispatch.lane.status.skipped');
      case 'cancelled':
        return t('dispatch.lane.status.cancelled');
      case 'waiting_deps':
        return t('dispatch.lane.status.waitingDeps');
      case 'running':
        return t('dispatch.lane.status.running');
      case 'pending':
      default:
        return t('dispatch.lane.status.pending');
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
        this.clearProtocolState(_taskId);
        const mappedStatus = status === 'completed'
          ? 'completed'
          : status === 'failed'
            ? 'failed'
            : 'cancelled';
        this.dispatchIdempotencyStore.updateStatusByTaskId(_taskId, mappedStatus);
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
          this.deps.messageHub.notify(t('dispatch.audit.reactiveInterventionBlocked'), 'error');
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
      this.phaseBPlusTimestamps.delete(batchId);
      this.clearBatchTaskCategories(batch);
      this.clearProtocolStatesByBatch(batchId);
      this.activeWorkerLanes.clear();
      this.clearDispatchScheduleTimers(batchId);
      logger.info('DispatchBatch.已取消', { batchId, reason }, LogCategory.ORCHESTRATOR);
      this.deps.messageHub.orchestratorMessage(t('dispatch.notify.taskCancelledWithReason', { reason }));
    });

    batch.on('phase:changed', (_batchId: string, phase) => {
      if (phase === 'archived') {
        this.phaseBPlusTimestamps.delete(batch.id);
        this.clearBatchTaskCategories(batch);
        this.clearProtocolStatesByBatch(batch.id);
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
    if (batch.status !== 'active') {
      return;
    }

    const readyTasks = batch.getReadyTasks();
    if (readyTasks.length === 0) {
      return;
    }

    const busyWorkers = new Set<WorkerSlot>(this.activeWorkerLanes);
    const selectedWorkers = new Set<WorkerSlot>();

    for (const entry of readyTasks) {
      const routing = this.resolveExecutionWorker(entry.worker, {
        busyWorkers,
        excludedWorkers: selectedWorkers,
        allowBusyFallback: true,
      });
      if (!routing.ok) {
        logger.debug('Dispatch.WorkerLane.就绪任务暂不可执行', {
          batchId: batch.id,
          taskId: entry.taskId,
          worker: entry.worker,
          reason: routing.error,
        }, LogCategory.ORCHESTRATOR);
        continue;
      }

      const selectedWorker = routing.selectedWorker;
      if (busyWorkers.has(selectedWorker) || selectedWorkers.has(selectedWorker)) {
        continue;
      }

      if (selectedWorker !== entry.worker) {
        const previousWorker = entry.worker;
        entry.worker = selectedWorker;
        this.deps.messageHub.notify(
          t('dispatch.notify.schedulingRoutingAdjusted', {
            taskId: entry.taskId,
            fromWorker: previousWorker,
            toWorker: selectedWorker,
            routingReason: routing.routingReason,
          }),
          'warning',
        );
        logger.warn('Dispatch.WorkerLane.忙碌改派', {
          batchId: batch.id,
          taskId: entry.taskId,
          from: previousWorker,
          to: selectedWorker,
          reason: routing.routingReason,
        }, LogCategory.ORCHESTRATOR);
      }

      selectedWorkers.add(selectedWorker);
      busyWorkers.add(selectedWorker);
    }

    for (const worker of selectedWorkers) {
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
      this.deps.messageHub.progress(t('dispatch.phaseC.progressTitle'), t('dispatch.phaseC.progressMessage'));

      const finalAuditOutcome = auditOutcome || this.ensureBatchAuditOutcome(batch, entries);

      if (finalAuditOutcome.level === 'intervention') {
        const blockedReport = this.buildInterventionReport(finalAuditOutcome, entries);
        this.deps.messageHub.notify(t('dispatch.audit.phaseCInterventionBlocked'), 'error');
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
          setTimeout(() => reject(new Error(t('dispatch.phaseC.timeout', { seconds: PHASE_C_TIMEOUT / 1000 }))), PHASE_C_TIMEOUT)
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
      return `${status} **[${e.worker}]** ${e.result?.summary || t('dispatch.phaseC.noOutput')}`;
    });
    this.deps.messageHub.notify(t('dispatch.phaseC.fallbackNotice'), 'warning');
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
        case 'completed': return t('dispatch.reactiveSummary.status.completed');
        case 'failed': return t('dispatch.reactiveSummary.status.failed');
        case 'skipped': return t('dispatch.reactiveSummary.status.skipped');
        case 'cancelled': return t('dispatch.reactiveSummary.status.cancelled');
        case 'running': return t('dispatch.reactiveSummary.status.running');
        case 'pending':
        case 'waiting_deps':
          return t('dispatch.reactiveSummary.status.waiting');
        default:
          return status;
      }
    };

    const taskLines = entries.map((entry, index) =>
      t('dispatch.reactiveSummary.taskLine', {
        index: index + 1,
        worker: entry.worker,
        task: entry.task,
        status: statusLabel(entry.status),
        summary: entry.result?.summary || t('dispatch.reactiveSummary.noResultSummary'),
      })
    );

    const lines = [
      t('dispatch.reactiveSummary.header', {
        total: summary.total,
        completed: summary.completed,
        failed: summary.failed,
        skipped: summary.skipped,
        cancelled: summary.cancelled,
      }),
      ...taskLines,
      modifiedFiles.length > 0
        ? t('dispatch.reactiveSummary.modifiedFiles', { files: modifiedFiles.join('，') })
        : t('dispatch.reactiveSummary.modifiedFilesNone'),
    ];

    return lines.join('\n');
  }

  /**
   * Phase B+ — Worker 上报处理
   *
   * progress 类型：更新 subTaskCard，不触发 LLM
   * question 类型：触发 orchestrator 中间 LLM 调用
   * completed/failed 类型：触发 Wisdom 提取（状态机由 DispatchBatch 主流程处理）
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
      const isBlocking = report.question.blocking === true;
      const now = Date.now();
      const throttleKey = batch?.id || `assignment:${report.assignmentId}`;
      const lastTs = this.phaseBPlusTimestamps.get(throttleKey) || 0;
      if (!isBlocking && now - lastTs < DispatchManager.PHASE_B_PLUS_MIN_INTERVAL) {
        logger.info('Phase B+ 频率限制，跳过中间调用', {
          worker: report.workerId,
          interval: now - lastTs,
          throttleKey,
        }, LogCategory.ORCHESTRATOR);
        return defaultResponse;
      }
      if (!isBlocking) {
        this.phaseBPlusTimestamps.set(throttleKey, now);
      }

      try {
        const batchStatus = batch ? batch.getSummary() : { total: 0 };
        const prompt = t('dispatch.phaseBPlus.prompt', {
          workerId: report.workerId,
          question: report.question.content,
          batchStatus: JSON.stringify(batchStatus),
          userPrompt: this.deps.getActiveUserPrompt(),
        });

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
        if (isBlocking) {
          const reason = t('dispatch.phaseBPlus.blockingNoDecision', {
            question: report.question.content.substring(0, 120),
          });
          this.deps.messageHub.notify(reason, 'warning');
          return createAbortResponse(reason);
        }
      } catch (error: any) {
        logger.error('Phase B+ 中间调用失败', { error: error.message }, LogCategory.ORCHESTRATOR);
        if (isBlocking) {
          const reason = t('dispatch.phaseBPlus.blockingDecisionFailed', { error: error.message });
          this.deps.messageHub.notify(reason, 'warning');
          return createAbortResponse(reason);
        }
      }

      return defaultResponse;
    }

    // completed/failed 类型：提取并持久化 Worker 经验（不改变原有状态机）
    if (report.type === 'completed' || report.type === 'failed') {
      try {
        this.deps.processWorkerWisdom(report);
      } catch (error) {
        logger.warn('Dispatch.Worker.Wisdom.处理失败', {
          assignmentId: report.assignmentId,
          worker: report.workerId,
          type: report.type,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
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
          t('dispatch.waitForWorkers.timeout', {
            seconds: Math.round(elapsedMs / 1000),
            pendingCount: pendingTaskIds.length,
          }),
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

  private startLeaseWatcher(): void {
    if (this.leaseWatcherTimer) {
      return;
    }
    this.leaseWatcherTimer = setInterval(() => {
      this.checkProtocolLeases();
    }, DispatchManager.LEASE_WATCH_INTERVAL_MS);
  }

  private stopLeaseWatcher(): void {
    if (!this.leaseWatcherTimer) {
      return;
    }
    clearInterval(this.leaseWatcherTimer);
    this.leaseWatcherTimer = undefined;
  }

  private registerProtocolState(
    taskId: string,
    batchId: string | undefined,
    worker: WorkerSlot,
  ): DispatchExecutionProtocolState {
    const now = Date.now();
    const dispatchAttemptId = `dispatch-attempt-${taskId}-${now}-${Math.random().toString(36).slice(2, 8)}`;
    const state: DispatchExecutionProtocolState = {
      taskId,
      batchId: batchId || 'unknown-batch',
      worker,
      dispatchAttemptId,
      idempotencyKey: dispatchAttemptId,
      leaseId: `lease-${taskId}-${now}-${Math.random().toString(36).slice(2, 8)}`,
      leaseExpireAt: now + DispatchManager.LEASE_TTL_MS,
      heartbeatAt: now,
      ackState: 'pending',
      createdAt: now,
      timeoutTriggered: false,
    };
    this.executionProtocolStates.set(taskId, state);
    return state;
  }

  private markProtocolAck(taskId: string, workerId?: WorkerSlot): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    const state = this.executionProtocolStates.get(normalizedTaskId);
    if (!state) {
      return;
    }
    if (workerId && state.worker !== workerId) {
      logger.warn('Dispatch.Protocol.ACK.Worker不一致', {
        taskId: normalizedTaskId,
        expectedWorker: state.worker,
        actualWorker: workerId,
      }, LogCategory.ORCHESTRATOR);
    }
    const now = Date.now();
    state.ackState = 'acked';
    state.ackAt = now;
    state.heartbeatAt = now;
    state.leaseExpireAt = now + DispatchManager.LEASE_TTL_MS;
  }

  private markProtocolNack(taskId: string, reason: string): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    const state = this.executionProtocolStates.get(normalizedTaskId);
    if (!state) {
      return;
    }
    state.ackState = 'nacked';
    state.nackReason = reason;
    state.leaseExpireAt = Date.now();
  }

  private updateProtocolHeartbeat(taskId: string, workerId: WorkerSlot, timestamp: number): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    const state = this.executionProtocolStates.get(normalizedTaskId);
    if (!state) {
      return;
    }
    const hbAt = Number.isFinite(timestamp) ? Math.floor(timestamp) : Date.now();
    if (state.worker !== workerId) {
      logger.warn('Dispatch.Protocol.Heartbeat.Worker不一致', {
        taskId: normalizedTaskId,
        expectedWorker: state.worker,
        actualWorker: workerId,
      }, LogCategory.ORCHESTRATOR);
      return;
    }
    if (state.ackState === 'pending') {
      state.ackState = 'acked';
      state.ackAt = hbAt;
    }
    state.heartbeatAt = hbAt;
    state.leaseExpireAt = hbAt + DispatchManager.LEASE_TTL_MS;
    if (this.activeBatch?.id === state.batchId) {
      this.activeBatch.touchActivity();
    }
  }

  private checkProtocolLeases(): void {
    if (this.executionProtocolStates.size === 0) {
      return;
    }
    const now = Date.now();
    for (const state of this.executionProtocolStates.values()) {
      if (state.timeoutTriggered) {
        continue;
      }
      if (now - state.createdAt > DispatchManager.EXECUTION_TIMEOUT_MS) {
        this.failByProtocolTimeout(state, 'execution-timeout');
        continue;
      }
      if (state.ackState === 'pending' && now - state.createdAt > DispatchManager.ACK_TIMEOUT_MS) {
        this.failByProtocolTimeout(state, 'ack-timeout');
        continue;
      }
      if (state.ackState === 'nacked') {
        this.failByProtocolTimeout(state, `nack:${state.nackReason || 'unknown'}`);
        continue;
      }
      if (state.ackState !== 'acked') {
        continue;
      }
      if (state.leaseExpireAt <= now) {
        this.failByProtocolTimeout(state, 'lease-expired');
      }
    }
  }

  private failByProtocolTimeout(state: DispatchExecutionProtocolState, reasonCode: string): void {
    if (state.timeoutTriggered) {
      return;
    }
    state.timeoutTriggered = true;

    const batch = this.activeBatch?.id === state.batchId ? this.activeBatch : null;
    const entry = batch?.getEntry(state.taskId);
    if (!batch || !entry || isTerminalStatus(entry.status)) {
      this.clearProtocolState(state.taskId);
      return;
    }

    const timeoutMessage = `Worker ${state.worker} 执行异常终止（${reasonCode}），已结束本轮任务并回传编排者。`;
    this.deps.messageHub.subTaskCard({
      id: state.taskId,
      title: entry.task,
      status: 'failed',
      worker: state.worker,
      summary: timeoutMessage,
      error: timeoutMessage,
    });
    this.deps.messageHub.workerError(state.worker, timeoutMessage);
    batch.markFailed(state.taskId, {
      success: false,
      summary: timeoutMessage,
      errors: [timeoutMessage],
    });
    void this.deps.adapterFactory.interrupt(state.worker).catch((error: any) => {
      logger.warn('Dispatch.Protocol.Timeout.中断Worker失败', {
        taskId: state.taskId,
        worker: state.worker,
        reasonCode,
        error: error?.message || String(error),
      }, LogCategory.ORCHESTRATOR);
    });
    logger.warn('Dispatch.Protocol.Timeout.已触发', {
      taskId: state.taskId,
      worker: state.worker,
      reasonCode,
      dispatchAttemptId: state.dispatchAttemptId,
      leaseId: state.leaseId,
    }, LogCategory.ORCHESTRATOR);
    this.clearProtocolState(state.taskId);
  }

  private clearProtocolState(taskId: string): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    this.executionProtocolStates.delete(normalizedTaskId);
  }

  private clearProtocolStatesByBatch(batchId: string): void {
    for (const [taskId, state] of this.executionProtocolStates.entries()) {
      if (state.batchId === batchId) {
        this.executionProtocolStates.delete(taskId);
      }
    }
  }

  private clearAllProtocolStates(): void {
    this.executionProtocolStates.clear();
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
      t('dispatch.delegation.goal', { goal: input.goal }),
      t('dispatch.delegation.acceptance', { acceptance: input.acceptance.map(item => `- ${item}`).join('\n') }),
      t('dispatch.delegation.constraints', { constraints: input.constraints.map(item => `- ${item}`).join('\n') }),
      t('dispatch.delegation.context', { context: input.context.map(item => `- ${item}`).join('\n') }),
    ];

    if (input.predecessorContext) {
      lines.push(input.predecessorContext);
    }

    if (input.scopeHint && input.scopeHint.length > 0) {
      lines.push(t('dispatch.delegation.scopeHint', { scopeHint: input.scopeHint.map(item => `- ${item}`).join('\n') }));
    }

    if (input.files && input.files.length > 0) {
      lines.push(t('dispatch.delegation.strictFiles', { files: input.files.map(item => `- ${item}`).join('\n') }));
    }

    const contracts = input.collaborationContracts;
    if (contracts.producerContracts.length > 0 || contracts.consumerContracts.length > 0 || contracts.interfaceContracts.length > 0 || contracts.freezeFiles.length > 0) {
      lines.push(t('dispatch.delegation.contractsHeader'));
      if (contracts.producerContracts.length > 0) {
        lines.push(t('dispatch.delegation.producerContracts', { contracts: contracts.producerContracts.join('、') }));
      }
      if (contracts.consumerContracts.length > 0) {
        lines.push(t('dispatch.delegation.consumerContracts', { contracts: contracts.consumerContracts.join('、') }));
      }
      if (contracts.interfaceContracts.length > 0) {
        lines.push(t('dispatch.delegation.interfaceContracts', { contracts: contracts.interfaceContracts.join('；') }));
      }
      if (contracts.freezeFiles.length > 0) {
        lines.push(t('dispatch.delegation.freezeFiles', { files: contracts.freezeFiles.join('、') }));
      }
    }

    lines.push(t('dispatch.delegation.executionRequirements'));
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

      const depSummary = depEntry.result.summary || t('dispatch.predecessor.noSummary');
      const depFiles = depEntry.result.modifiedFiles?.join('、') || t('dispatch.predecessor.none');
      sections.push(t('dispatch.predecessor.item', {
        depId,
        worker: depEntry.worker,
        summary: depSummary,
        files: depFiles,
      }));
    }

    if (sections.length === 0) return undefined;
    return t('dispatch.predecessor.header', { sections: sections.join('\n') });
  }

  private buildScopeHintGuidance(scopeHint?: string[]): string {
    if (!scopeHint || scopeHint.length === 0) {
      return '';
    }
    return [
      t('dispatch.scopeHint.header'),
      ...scopeHint.map(item => `- ${item}`),
      '',
      t('dispatch.scopeHint.footer'),
    ].join('\n');
  }

  private shouldWarnMissingScopeHintForParallelTask(batch: DispatchBatch, dependsOn?: string[]): boolean {
    const dependencySet = new Set(dependsOn || []);
    return batch.getEntries().some(entry =>
      !isTerminalStatus(entry.status) && !dependencySet.has(entry.taskId)
    );
  }

  private normalizeCollaborationContracts(
    raw?: DispatchTaskCollaborationContracts,
  ): DispatchCollaborationContracts {
    // raw 已经过 orchestration-executor.normalizeContracts() 的边界验证和 trim，
    // 此处只做类型转换：optional string[] → required string[]（空数组兜底）
    return {
      producerContracts: raw?.producer_contracts || [],
      consumerContracts: raw?.consumer_contracts || [],
      interfaceContracts: raw?.interface_contracts || [],
      freezeFiles: raw?.freeze_files || [],
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
      if (entry.result?.quality?.verificationDegraded) {
        const warning = entry.result.quality.warnings?.[0] || t('dispatch.verification.degradedFallbackWarning');
        escalate(
          entry.taskId,
          'watch',
          'verification',
          t('dispatch.audit.issue.verificationDegraded', { warning }),
        );
      }

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
            t('dispatch.audit.issue.outOfStrictFiles', { files: outOfStrictFiles.join('、') }),
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
            t('dispatch.audit.issue.outOfScopeHint', { files: outOfHintFiles.join('、') }),
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
            t('dispatch.audit.issue.touchedFreezeFiles', { files: touchedFreezeFiles.join('、') }),
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
            escalate(a, 'intervention', 'cross_task', t('dispatch.audit.issue.parallelConflict', { taskId: b, file }));
            escalate(b, 'intervention', 'cross_task', t('dispatch.audit.issue.parallelConflict', { taskId: a, file }));
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
      .join('\n') || t('dispatch.audit.issue.none');

    return [
      t('dispatch.audit.appendix.header'),
      t('dispatch.audit.appendix.overallLevel', { level: auditOutcome.level }),
      t('dispatch.audit.appendix.distribution', {
        normal: auditOutcome.summary.normal,
        watch: auditOutcome.summary.watch,
        intervention: auditOutcome.summary.intervention,
      }),
      t('dispatch.audit.appendix.issueList'),
      issueLines,
      t('dispatch.audit.appendix.strictFollow'),
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
        t('dispatch.audit.report.issueLine', {
          index: index + 1,
          taskId: issue.taskId,
          taskTitle: titleByTaskId.get(issue.taskId) || t('dispatch.audit.report.unknownTask'),
          detail: issue.detail,
        })
      ).join('\n')
      : t('dispatch.audit.report.none');

    const watchLines = watchIssues.length > 0
      ? watchIssues.map((issue, index) =>
        t('dispatch.audit.report.issueLine', {
          index: index + 1,
          taskId: issue.taskId,
          taskTitle: titleByTaskId.get(issue.taskId) || t('dispatch.audit.report.unknownTask'),
          detail: issue.detail,
        })
      ).join('\n')
      : t('dispatch.audit.report.none');

    return [
      t('dispatch.audit.report.header'),
      t('dispatch.audit.report.resultBlocked'),
      '',
      t('dispatch.audit.report.interventionTitle'),
      interventionLines,
      '',
      t('dispatch.audit.report.watchTitle'),
      watchLines,
      '',
      t('dispatch.audit.report.suggestedActionsTitle'),
      t('dispatch.audit.report.action1'),
      t('dispatch.audit.report.action2'),
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

  private buildDispatchIdempotencyKey(input: {
    sessionId: string;
    missionId: string;
    providedKey?: string;
    category: string;
    taskName: string;
    goal: string;
    acceptance: string[];
    constraints: string[];
    context: string[];
    scopeHint?: string[];
    files?: string[];
    dependsOn?: string[];
  }): string {
    const scopePrefix = `${input.sessionId}::${input.missionId}::`;
    const provided = typeof input.providedKey === 'string' ? input.providedKey.trim() : '';
    if (provided) {
      const digest = createHash('sha1').update(provided).digest('hex');
      return `${scopePrefix}provided:${digest}`;
    }

    const normalizeArray = (items?: string[]): string[] =>
      Array.isArray(items)
        ? items.map(item => item.trim()).filter(Boolean).sort()
        : [];

    const payload = {
      category: input.category,
      taskName: input.taskName.trim(),
      goal: input.goal.trim(),
      acceptance: normalizeArray(input.acceptance),
      constraints: normalizeArray(input.constraints),
      context: normalizeArray(input.context),
      scopeHint: normalizeArray(input.scopeHint),
      files: normalizeArray(input.files),
      dependsOn: normalizeArray(input.dependsOn),
    };
    const digest = createHash('sha1').update(JSON.stringify(payload)).digest('hex');
    return `${scopePrefix}auto:${digest}`;
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
    this.resumeContextStore.dispose();
    this.dispatchTaskCategories.clear();
    this.routingService.clearAllRuntimeUnavailable();
    this.activeWorkerLanes.clear();
    this.stopLeaseWatcher();
    this.clearAllProtocolStates();
    this.clearDispatchScheduleTimers();
  }
}
