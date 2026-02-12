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
import type { MessageHub } from './message-hub';
import type { MissionOrchestrator } from './mission-orchestrator';
import type { Assignment } from '../mission';
import type { WorkerReport, OrchestratorResponse } from '../protocols/worker-report';
import { createAdjustResponse } from '../protocols/worker-report';
import { DispatchBatch, CancellationError, type DispatchEntry, type DispatchResult, type DispatchStatus } from './dispatch-batch';
import { LLMConfigLoader } from '../../llm/config';
import { buildDispatchSummaryPrompt } from '../prompts/orchestrator-prompts';
import { PlanningExecutor } from './executors/planning-executor';
import { WorkerPipeline } from './worker-pipeline';
import type { SnapshotManager } from '../../snapshot-manager';

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
  getLastMissionId: () => string | undefined;
  getProjectKnowledgeBase: () => import('../../knowledge/project-knowledge-base').ProjectKnowledgeBase | undefined;
  // 治理依赖（WorkerPipeline 使用）
  getSnapshotManager: () => SnapshotManager | null;
  getContextManager: () => import('../../context/context-manager').ContextManager | null;
  getTodoManager: () => import('../../todo').TodoManager | null;
  // Token 统计
  recordOrchestratorTokens: (usage?: TokenUsage, phase?: 'planning' | 'verification') => void;
  recordWorkerTokenUsage: (results: Map<string, import('../worker').AutonomousExecutionResult>) => void;
}

/**
 * DispatchManager - L3 统一调度管理器
 */
export class DispatchManager {
  // Phase B+ 中间调用频率限制：同一 batch 内最小间隔 30 秒
  private lastPhaseBPlusTimestamp = 0;
  private static readonly PHASE_B_PLUS_MIN_INTERVAL = 30_000;

  private pipeline = new WorkerPipeline();
  private activeBatch: DispatchBatch | null = null;
  private _planningExecutor: PlanningExecutor | null = null;

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
   * 注入编排工具（dispatch_task / send_worker_message）的回调处理器
   */
  setupOrchestrationToolHandlers(): void {
    const toolManager = this.deps.adapterFactory.getToolManager();
    const orchestrationExecutor = toolManager.getOrchestrationExecutor();

    // 从 ProfileLoader 动态注入 Worker 列表到工具定义
    const allProfiles = this.deps.profileLoader.getAllProfiles();
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

        // 校验 Worker 是否已启用
        const workerFullConfig = LLMConfigLoader.loadFullConfig();
        if (workerFullConfig.workers[worker as WorkerSlot]?.enabled === false) {
          return { task_id: '', status: 'failed' as const, worker, error: `Worker "${worker}" 未启用，请检查 LLM 配置` };
        }

        // 生成唯一 task_id
        const taskId = `dispatch-${Date.now()}-${worker}-${Math.random().toString(36).substring(2, 5)}`;

        // 确保 DispatchBatch 存在（一次 orchestrator LLM 调用共享一个 Batch）
        if (!this.activeBatch || this.activeBatch.status !== 'active') {
          // 使用 Mission ID 作为 Batch ID，确保 Todo 关联到正确的 Mission
          const missionId = this.deps.getLastMissionId();
          this.activeBatch = new DispatchBatch(missionId);
          this.activeBatch.userPrompt = this.deps.getActiveUserPrompt();
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
        this.deps.messageHub.subTaskCard({
          id: taskId,
          title: task,
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

      sendMessage: async (params) => {
        const { worker, message } = params;
        logger.info('编排工具.send_worker_message', {
          worker, messagePreview: message.substring(0, 80),
        }, LogCategory.ORCHESTRATOR);

        this.deps.messageHub.workerInstruction(worker, message);
        return { delivered: true };
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
  launchDispatchWorker(taskId: string, worker: WorkerSlot, task: string, files?: string[]): void {
    const batch = this.activeBatch;

    // 标记开始运行
    batch?.markRunning(taskId);
    this.deps.messageHub.subTaskCard({
      id: taskId,
      title: task,
      status: 'running',
      worker,
    });

    // 发送任务指令到 Worker Tab，让用户看到 orchestrator 对 worker 的要求
    this.deps.messageHub.workerInstruction(worker, task, {
      assignmentId: taskId,
      missionId: batch?.id,
    });

    (async () => {
      // 确保 Worker 存在
      const workerInstance = await this.deps.missionOrchestrator.ensureWorkerForDispatch(worker);

      // 构建轻量 Assignment
      const missionId = batch?.id || 'dispatch';
      const assignment: Assignment = {
        id: taskId,
        missionId,
        workerId: worker,
        shortTitle: task,
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
      const snapshotManager = this.deps.getSnapshotManager();
      const contextManager = this.deps.getContextManager();

      // 通过 WorkerPipeline 统一执行
      const pipelineResult = await this.pipeline.execute({
        assignment,
        workerInstance,
        adapterFactory: this.deps.adapterFactory,
        workspaceRoot: this.deps.workspaceRoot,
        projectContext,
        missionId,
        onReport: (report) => this.handleDispatchWorkerReport(report, batch),
        cancellationToken: batch?.cancellationToken,
        imagePaths: this.deps.getActiveImagePaths(),
        // 治理开关（auto 模式：有 files 时自动启用）
        enableSnapshot: hasFiles && snapshotManager != null,
        enableLSP: hasFiles,
        enableTargetEnforce: hasFiles,
        enableContextUpdate: contextManager != null,
        snapshotManager,
        contextManager,
        todoManager: this.deps.getTodoManager(),
      });

      const result = pipelineResult.executionResult;

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
        worker,
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
        worker, taskId, success: result.success, summary,
      }, LogCategory.ORCHESTRATOR);
    })().catch(async (error: any) => {
      // C-09: 取消异常不按失败处理，cancelAll 已标记 cancelled 状态
      if (error instanceof CancellationError || error?.isCancellation) {
        this.deps.messageHub.subTaskCard({
          id: taskId,
          title: task,
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

      this.deps.messageHub.subTaskCard({
        id: taskId,
        title: task,
        status: 'failed',
        worker,
        summary: errorMsg,
        error: errorMsg,
      });

      this.deps.messageHub.workerError(
        worker,
        `任务执行失败: ${errorMsg}`,
      );

      batch?.markFailed(taskId, { success: false, summary: errorMsg, errors: [errorMsg] });

      // C-15: Worker 崩溃后状态清理
      try {
        const workerInstance = this.deps.missionOrchestrator.getWorker(worker);
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
    batch.on('task:ready', (_taskId: string, _entry: DispatchEntry) => {
      this.dispatchReadyTasksWithIsolation(batch);
    });

    // Worker 完成后重新检查是否有同类型排队任务可启动
    batch.on('task:statusChanged', (_taskId: string, status: DispatchStatus) => {
      if (status === 'completed' || status === 'failed' || status === 'skipped' || status === 'cancelled') {
        setImmediate(() => this.dispatchReadyTasksWithIsolation(batch));
      }
    });

    // 全部完成 → Phase C 汇总
    batch.on('batch:allCompleted', (batchId: string, entries: DispatchEntry[]) => {
      const summary = batch.getSummary();
      logger.info('DispatchBatch.全部完成', { batchId, ...summary }, LogCategory.ORCHESTRATOR);
      void this.triggerPhaseCSummary(batch, entries);
    });

    // Batch 被取消 → 不触发 Phase C，直接通知用户
    batch.on('batch:cancelled', (batchId: string, reason: string) => {
      logger.info('DispatchBatch.已取消', { batchId, reason }, LogCategory.ORCHESTRATOR);
      this.deps.messageHub.orchestratorMessage(`任务已取消: ${reason}`);
    });
  }

  /**
   * 通过 Worker 隔离策略调度就绪任务
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
   */
  private async triggerPhaseCSummary(batch: DispatchBatch, entries: DispatchEntry[]): Promise<void> {
    const userPrompt = batch.userPrompt || this.deps.getActiveUserPrompt();
    if (!userPrompt) {
      logger.warn('Phase C 汇总: 无用户原始请求，跳过', undefined, LogCategory.ORCHESTRATOR);
      batch.archive();
      return;
    }

    try {
      this.deps.messageHub.progress('Summarizing', '正在汇总所有 Worker 的执行结果...');

      const summaryPrompt = buildDispatchSummaryPrompt(userPrompt, entries);

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
            messageMeta: { intent: 'phase_c_summary', batchId: batch.id },
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
        this.deps.messageHub.result(response.content || '');
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
    this.deps.messageHub.result(lines.join('\n'));
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
            messageMeta: { intent: 'phase_b_plus', batchId: batch?.id },
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

  dispose(): void {
    this.activeBatch = null;
    this._planningExecutor = null;
  }
}
