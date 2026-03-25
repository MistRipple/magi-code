import { EventEmitter } from 'events';
import type { WorkerSlot, UIState, WorkerStatus, PermissionMatrix, StrategyConfig } from '../../types';
import { UnifiedSessionManager, type SessionRuntimeNotificationState } from '../../session';
import { resolveStandardMessageSessionBinding } from '../../session/standard-message-session-binding';
import { SnapshotManager } from '../../snapshot-manager';
import { LLMAdapterFactory } from '../../llm/adapter-factory';
import { MissionDrivenEngine } from '../../orchestrator/core/mission-driven-engine';
import { EventBindingService } from '../../ui/event-binding-service';
import type { ClientBridgeMessage, SupportedLocale } from '../../ui/shared/bridges/client-bridge';
import { logger, LogCategory } from '../../logging';
import { t, type LocaleCode } from '../../i18n';
import { OrchestrationRuntimeQueryService } from '../../orchestrator/runtime/orchestration-runtime-query-service';
import { createRuntimeHostContext, type RuntimeHostContext } from '../../host';
import { CodebaseRetrievalService } from '../../services/codebase-retrieval-service';
import { buildExecutionStatsPayload, buildModelCatalogFromLLMConfig, type ExecutionStatsPayload } from '../../shared/execution-stats-payload';
import { buildSessionBootstrapTimelineProjection, type ExecutionChainBootstrapSummary } from '../../shared/session-bootstrap';
import { materializeProjectionSourceMessagesFromTimelineRecords } from '../../session/timeline-record-adapter';
import {
  ControlMessageType,
  MessageCategory,
  MessageLifecycle,
  MessageType,
  type NotifyLevel,
  createErrorMessage,
  createStandardMessage,
  createStreamingMessage,
  createUserInputMessage,
  type ContentBlock,
  type DataMessageType,
  type StandardMessage,
} from '../../protocol/message-protocol';
import type { TaskView } from '../../task/task-view-adapter';
import type { LogEntry, MessageSource } from '../../types';
import type { WorkspaceFolderInfo } from '../../workspace/workspace-roots';

interface RuntimeWorkspaceDescriptor {
  workspaceId: string;
  name: string;
  rootPath: string;
  sessionManager: UnifiedSessionManager;
}

interface AgentRuntimeOptions {
  workspace: RuntimeWorkspaceDescriptor;
  getLocale: () => SupportedLocale;
  getDeepTask: () => boolean;
}

type RuntimeExecutionResult = {
  success: boolean;
  taskId?: string;
  error?: string;
  runtimeReason?: ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['runtimeReason'];
  finalStatus?: ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['finalStatus'];
  errors?: string[];
  failureReason?: string;
  recoverable?: boolean;
};

type AgentRuntimeReason = ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['runtimeReason'];

type PendingRecoveryContext = {
  taskId: string;
  prompt: string;
  sessionId: string;
  runtimeReason: AgentRuntimeReason;
  errors: string[];
  canRetry: boolean;
  canRollback: boolean;
};

interface QueuedUserTurn {
  id: string;
  content: string;
  createdAt: number;
}

interface SessionNotificationsPayload {
  sessionId: string;
  notifications: SessionRuntimeNotificationState;
}

function buildWorkerStatuses(factory: LLMAdapterFactory): WorkerStatus[] {
  const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];
  return workers.map((worker) => ({
    worker,
    available: factory.isConnected(worker),
    enabled: true,
  }));
}

export class AgentWorkspaceRuntime {
  private readonly workspace: RuntimeWorkspaceDescriptor;
  private readonly sessionManager: UnifiedSessionManager;
  private readonly snapshotManager: SnapshotManager;
  private readonly adapterFactory: LLMAdapterFactory;
  private readonly orchestratorEngine: MissionDrivenEngine;
  private readonly eventBindingService: EventBindingService;
  private readonly logs: LogEntry[] = [];
  private readonly messageIdToRequestId = new Map<string, string>();
  private readonly listeners = new Set<(message: ClientBridgeMessage) => void>();
  private readonly queuedMessagesBySession = new Map<string, QueuedUserTurn[]>();
  private readonly runtimeQueryService = new OrchestrationRuntimeQueryService();
  private readonly runtimeHost: RuntimeHostContext;
  private runtimeInitializationPromise: Promise<void>;
  private activeSessionId: string | null = null;
  private interactionModeUpdatedAt = Date.now();
  private queuedMessagesDrainRunning = false;
  private pendingRecoveryContext: PendingRecoveryContext | null = null;

  constructor(private readonly options: AgentRuntimeOptions) {
    this.workspace = options.workspace;
    this.sessionManager = options.workspace.sessionManager;
    this.snapshotManager = new SnapshotManager(this.sessionManager, this.workspace.rootPath);
    const workspaceFolders: WorkspaceFolderInfo[] = [{ name: this.workspace.name, path: this.workspace.rootPath }];
    this.runtimeHost = createRuntimeHostContext({
      workspaceRoot: this.workspace.rootPath,
      workspaceFolders,
      sessionManager: this.sessionManager,
      snapshotManager: this.snapshotManager,
      getCurrentSessionId: () => this.activeSessionId || this.sessionManager.getCurrentSession()?.id || null,
      workspaceRefs: [{
        workspaceId: this.workspace.workspaceId,
        rootPath: this.workspace.rootPath,
        displayName: this.workspace.name,
      }],
    });
    this.adapterFactory = new LLMAdapterFactory(this.runtimeHost);
    const permissions: PermissionMatrix = {
      allowEdit: true,
      allowBash: true,
      allowWeb: true,
    };
    const strategy: StrategyConfig = {
      enableVerification: true,
      enableRecovery: true,
      autoRollbackOnFailure: false,
    };
    this.orchestratorEngine = new MissionDrivenEngine(
      this.adapterFactory,
      { timeout: 300000, maxRetries: 3, permissions, strategy },
      this.runtimeHost,
    );
    const messageHub = this.orchestratorEngine.getMessageHub();
    this.adapterFactory.setMessageHub(messageHub);
    const toolManager = this.adapterFactory.getToolManager();
    toolManager.setSnapshotManager(this.snapshotManager);
    this.injectCodebaseRetrievalService(toolManager, workspaceFolders);
    this.syncTrace();

    this.eventBindingService = new EventBindingService({
      getActiveSessionId: () => this.activeSessionId,
      getMessageHub: () => messageHub,
      getOrchestratorEngine: () => this.orchestratorEngine,
      getAdapterFactory: () => this.adapterFactory,
      getMissionOrchestrator: () => this.orchestratorEngine.getMissionOrchestrator(),
      getMessageIdToRequestId: () => this.messageIdToRequestId,
      sendStateUpdate: () => {
        void this.sendStateUpdate();
      },
      sendData: (dataType, payload) => this.sendData(dataType, payload),
      sendToast: (msg, level, duration) => this.sendToast(msg, level, duration),
      sendExecutionStats: () => {
        /* Web 执行主链先不单独推送执行统计，交由设置 bootstrap 提供 */
      },
      sendOrchestratorMessage: (params) => this.sendOrchestratorMessage(params),
      appendLog: (entry) => this.appendLog(entry),
      postMessage: (message) => this.emit(message),
      logMessageFlow: () => {},
      resolveRequestTimeoutFromMessage: () => {},
      clearRequestTimeout: () => {},
      interruptCurrentTask: async () => this.interruptCurrentTask(),
      tryResumePendingRecovery: () => {},
      getMessageSnapshot: (messageId) => messageHub.getMessageSnapshot(messageId),
      getLiveSessionTimelineProjection: (sessionId) => {
        const session = this.sessionManager.getSession(sessionId);
        if (!session) {
          return null;
        }
        return structuredClone(buildSessionBootstrapTimelineProjection({
          session: {
            id: session.id,
            updatedAt: session.updatedAt,
            projectionMessages: materializeProjectionSourceMessagesFromTimelineRecords(session.timeline.records),
          },
          liveMessages: this.getActiveMessageSnapshots(sessionId),
        }));
      },
      persistStandardMessageToSession: (sessionId, message) => this.persistStandardMessageToSession(sessionId, message),
    });
    this.eventBindingService.bindAll();

    const adapterFactoryInit = this.adapterFactory.initialize();
    const orchestratorEngineInit = this.orchestratorEngine.initialize();
    this.runtimeInitializationPromise = Promise.all([adapterFactoryInit, orchestratorEngineInit]).then(() => {});
  }

  subscribe(listener: (message: ClientBridgeMessage) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  async ensureInitialized(): Promise<void> {
    await this.runtimeInitializationPromise;
  }

  async bindSession(sessionId?: string | null): Promise<void> {
    await this.ensureInitialized();
    this.eventBindingService.flushLiveMessageSnapshots({ silent: true });
    this.eventBindingService.resetSessionRuntimeState();
    const resolved = (sessionId && this.sessionManager.switchSession(sessionId))
      || this.sessionManager.getCurrentSession()
      || this.sessionManager.createSession();
    this.activeSessionId = resolved.id;
    this.syncTrace();
    await this.orchestratorEngine.reconcilePlanLedgerForSession(resolved.id);
  }

  async executeTask(prompt: string, sessionId?: string, requestId?: string): Promise<RuntimeExecutionResult> {
    await this.bindSession(sessionId);
    const trimmedPrompt = prompt.trim();
    if (!trimmedPrompt) {
      return { success: false, error: t('provider.errors.emptyPrompt') };
    }
    const effectiveRequestId = requestId?.trim() || `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    const turnId = `turn:${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
    const { userMessageId } = this.emitUserAndPlaceholder(effectiveRequestId, trimmedPrompt, turnId);

    this.sessionManager.addMessageToSession(this.activeSessionId!, 'user', trimmedPrompt, undefined, 'orchestrator', undefined, {
      id: userMessageId,
      type: 'user_input',
      metadata: {
        turnId,
        requestId: effectiveRequestId,
      },
    });
    void this.orchestratorEngine.recordContextMessage('user', trimmedPrompt, this.activeSessionId!);
    await this.sendStateUpdate();

    const messageHub = this.orchestratorEngine.getMessageHub();
    let taskContext: { taskId: string; result: string } | null = null;
    messageHub.taskAccepted(effectiveRequestId);
    messageHub.sendControl(ControlMessageType.TASK_STARTED, {
      requestId: effectiveRequestId,
      timestamp: Date.now(),
    });

    try {
      taskContext = await this.orchestratorEngine.executeWithTaskContext(
        trimmedPrompt,
        this.activeSessionId!,
        [],
        turnId,
        effectiveRequestId,
      );
      // normalizer 的流式消息已通过 unified:complete 路径持久化（含完整 blocks + metadata），
      // 不再额外调用 saveAssistantMessage，仅通过 recordContextMessage 记录上下文记忆。
      if (taskContext.result?.trim()) {
        void this.orchestratorEngine.recordContextMessage('assistant', taskContext.result, this.activeSessionId!);
      }
      const executionStatus = this.orchestratorEngine.getLastExecutionStatus();
      if (executionStatus.finalStatus !== 'completed') {
        const runtimeReason = executionStatus.runtimeReason;
        const errors = executionStatus.errors.length > 0
          ? [...executionStatus.errors]
          : [];
        const effectiveFailureReason = this.buildExecutionFailureReason(runtimeReason, errors);
        const recoverable = this.isRecoverableRuntimeReason(runtimeReason);

        messageHub.sendControl(ControlMessageType.TASK_FAILED, {
          requestId: effectiveRequestId,
          error: effectiveFailureReason,
          cancelled: runtimeReason === 'cancelled' || runtimeReason === 'external_abort',
          timestamp: Date.now(),
        });
        await this.publishOrchestratorRuntimeDiagnostics({
          sessionId: this.activeSessionId || '',
          requestId: effectiveRequestId,
          runtimeReason,
          finalStatus: executionStatus.finalStatus,
          errors,
          failureReason: effectiveFailureReason,
          runtimeSnapshot: executionStatus.runtimeSnapshot,
          runtimeDecisionTrace: executionStatus.runtimeDecisionTrace,
        });
        this.setPendingRecoveryFromExecution({
          result: {
            taskId: taskContext.taskId || '',
            runtimeReason,
            finalStatus: executionStatus.finalStatus,
            errors,
            failureReason: effectiveFailureReason,
            recoverable,
          },
          prompt: trimmedPrompt,
          sessionId: this.activeSessionId || '',
        });
        await this.sendStateUpdate();
        logger.warn('Agent 执行主链未完成即返回', {
          runtimeReason,
          finalStatus: executionStatus.finalStatus,
          taskId: taskContext.taskId || '',
        }, LogCategory.AGENT);
        return {
          success: false,
          error: effectiveFailureReason,
          taskId: taskContext.taskId,
          runtimeReason,
          finalStatus: executionStatus.finalStatus,
          errors,
          failureReason: effectiveFailureReason,
          recoverable,
        };
      }
      this.clearPendingRecoveryState();
      messageHub.sendControl(ControlMessageType.TASK_COMPLETED, {
        requestId: effectiveRequestId,
        timestamp: Date.now(),
      });
      await this.sendStateUpdate();
      return {
        success: true,
        taskId: taskContext.taskId,
        runtimeReason: 'completed',
        finalStatus: 'completed',
        errors: [],
        recoverable: false,
      };
    } catch (error) {
      const failureReason = error instanceof Error ? error.message : String(error);
      const executionStatus = this.orchestratorEngine.getLastExecutionStatus();
      const runtimeReason = executionStatus.runtimeReason;
      const errors = executionStatus.errors.length > 0
        ? [...executionStatus.errors]
        : [failureReason];
      const effectiveFailureReason = this.buildExecutionFailureReason(runtimeReason, errors, failureReason);
      const recoverable = this.isRecoverableRuntimeReason(runtimeReason);

      messageHub.sendControl(ControlMessageType.TASK_FAILED, {
        requestId: effectiveRequestId,
        error: failureReason,
        timestamp: Date.now(),
      });
      const traceId = messageHub.getTraceId();
      const errorMessage = createErrorMessage(
        failureReason,
        'orchestrator',
        'orchestrator',
        traceId,
        {
          metadata: {
            requestId: effectiveRequestId,
            ...(this.activeSessionId ? { sessionId: this.activeSessionId } : {}),
          },
        },
      );
      messageHub.sendMessage(errorMessage);
      await this.publishOrchestratorRuntimeDiagnostics({
        sessionId: this.activeSessionId || '',
        requestId: effectiveRequestId,
        runtimeReason,
        finalStatus: executionStatus.finalStatus,
        errors,
        failureReason: effectiveFailureReason,
        runtimeSnapshot: executionStatus.runtimeSnapshot,
        runtimeDecisionTrace: executionStatus.runtimeDecisionTrace,
      });
      this.setPendingRecoveryFromExecution({
        result: {
          taskId: taskContext?.taskId || '',
          runtimeReason,
          finalStatus: executionStatus.finalStatus,
          errors,
          failureReason: effectiveFailureReason,
          recoverable,
        },
        prompt: trimmedPrompt,
        sessionId: this.activeSessionId || '',
      });
      await this.sendStateUpdate();
      logger.error('Agent 执行主链失败', { error: failureReason }, LogCategory.AGENT);
      return {
        success: false,
        error: effectiveFailureReason,
        taskId: taskContext?.taskId,
        runtimeReason,
        finalStatus: executionStatus.finalStatus,
        errors,
        failureReason: effectiveFailureReason,
        recoverable,
      };
    } finally {
      messageHub.finalizeRequestContext(effectiveRequestId);
      messageHub.forceProcessingState(false);
    }
  }

  /**
   * 重新加载 LLM 配置并刷新运行时 adapter 缓存
   *
   * 场景：Web 模式下用户在设置页修改了模型配置后，
   * 必须清除旧的 adapter 实例缓存，使下一次编排/Worker 调用
   * 使用新配置创建的 adapter。
   */
  async reloadLLMConfig(target: 'orchestrator' | 'worker' | 'auxiliary', workerSlot?: string): Promise<void> {
    await this.ensureInitialized();
    const { clearClientCache } = await import('../../llm/clients/client-factory');
    if (target === 'orchestrator') {
      await this.adapterFactory.reloadOrchestratorConfig();
    } else if (target === 'worker' && workerSlot) {
      await this.adapterFactory.reloadWorkerConfig(workerSlot as import('../../types').WorkerSlot);
    } else if (target === 'auxiliary') {
      // 辅助模型无独立 adapter，清除 client 缓存即可
    }
    clearClientCache();
  }

  async interruptCurrentTask(): Promise<void> {
    await this.ensureInitialized();
    if (this.orchestratorEngine.running) {
      await this.orchestratorEngine.interrupt();
    }
    await this.adapterFactory.interruptAll();
    const tasks = await this.getTaskViews();
    for (const task of tasks.filter((item) => item.status === 'running')) {
      await this.orchestratorEngine.cancelTaskById(task.id);
    }
    this.orchestratorEngine.getMessageHub().sendControl(ControlMessageType.TASK_FAILED, {
      error: t('provider.userCancelled'),
      cancelled: true,
      timestamp: Date.now(),
    });
    // 通知前端当前执行链可恢复
    const activeChainId = this.orchestratorEngine.getActiveChainId();
    if (activeChainId) {
      const chainQuery = this.orchestratorEngine.getExecutionChainQuery();
      if (chainQuery.isChainRecoverable(activeChainId)) {
        this.sendData('executionChainInterrupted', {
          chainId: activeChainId,
          recoverable: true,
        });
      }
    }
    await this.sendStateUpdate();
  }

  async startTask(taskId: string): Promise<void> {
    await this.ensureInitialized();
    await this.orchestratorEngine.startTaskById(taskId);
    await this.sendStateUpdate();
  }

  async deleteTask(taskId: string): Promise<void> {
    await this.ensureInitialized();
    await this.orchestratorEngine.deleteTaskById(taskId);
    await this.sendStateUpdate();
  }

  async resumeTask(taskId: string): Promise<void> {
    await this.ensureInitialized();
    await this.orchestratorEngine.startTaskById(taskId);
    await this.sendStateUpdate();
  }

  async abandonChain(chainId: string): Promise<{ abandoned: boolean }> {
    await this.ensureInitialized();
    const abandoned = this.orchestratorEngine.abandonChain(chainId);
    if (abandoned) {
      await this.sendStateUpdate();
    }
    return { abandoned };
  }

  async appendMessage(_taskId: string, content: string): Promise<{ queued: boolean; queueId?: string }> {
    await this.ensureInitialized();
    const trimmedContent = content.trim();
    if (!trimmedContent) {
      this.sendToast(t('toast.supplementEmpty'), 'warning');
      return { queued: false };
    }
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return { queued: false };
    }
    const shouldQueue = this.orchestratorEngine.running || this.queuedMessagesDrainRunning;
    if (shouldQueue) {
      const queued = this.enqueueQueuedMessage(sessionId, trimmedContent);
      this.sendQueuedMessagesUpdate(sessionId);
      return { queued: true, queueId: queued.id };
    }
    const requestId = `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    const result = await this.executeTask(trimmedContent, sessionId, requestId);
    if (!result.success) {
      this.sendToast(t('toast.supplementFailed'), 'error');
    }
    return { queued: false };
  }

  async updateQueuedMessage(queueId: string, content: string): Promise<boolean> {
    await this.ensureInitialized();
    const id = queueId.trim();
    const trimmedContent = content.trim();
    if (!id) {
      return false;
    }
    if (!trimmedContent) {
      this.sendToast(t('toast.supplementEmpty'), 'warning');
      return false;
    }
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return false;
    }
    const queue = this.getQueuedMessages(sessionId, false);
    const target = queue.find((item) => item.id === id);
    if (!target) {
      return false;
    }
    target.content = trimmedContent;
    this.sendQueuedMessagesUpdate(sessionId);
    void this.drainQueuedMessagesIfIdle();
    return true;
  }

  async deleteQueuedMessage(queueId: string): Promise<boolean> {
    await this.ensureInitialized();
    const id = queueId.trim();
    if (!id) {
      return false;
    }
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return false;
    }
    const queue = this.getQueuedMessages(sessionId, false);
    if (queue.length === 0) {
      return false;
    }
    const nextQueue = queue.filter((item) => item.id !== id);
    if (nextQueue.length === queue.length) {
      return false;
    }
    if (nextQueue.length === 0) {
      this.queuedMessagesBySession.delete(sessionId);
    } else {
      this.queuedMessagesBySession.set(sessionId, nextQueue);
    }
    this.sendQueuedMessagesUpdate(sessionId);
    void this.drainQueuedMessagesIfIdle();
    return true;
  }

  async handleToolAuthorizationResponse(requestId: string | undefined, allowed: boolean): Promise<boolean> {
    await this.ensureInitialized();
    if (!requestId?.trim()) {
      return false;
    }
    this.eventBindingService.handleToolAuthorizationResponse(requestId, allowed);
    await this.sendStateUpdate();
    return true;
  }

  async handleInteractionResponse(requestId: string, response: unknown): Promise<boolean> {
    await this.ensureInitialized();
    const normalizedRequestId = requestId.trim();
    if (!normalizedRequestId) {
      return false;
    }

    if (!normalizedRequestId.startsWith('approval-')) {
      logger.info('Agent 交互响应暂未命中可处理分支', {
        requestId: normalizedRequestId,
      }, LogCategory.AGENT);
      return false;
    }

    const todoId = normalizedRequestId.replace('approval-', '');
    const isApproved = response === true || response === 'approved' || response === 'yes'
      || (typeof response === 'object' && response !== null && (response as { value?: unknown }).value === 'approved');

    if (isApproved) {
      try {
        const orchestrator = this.orchestratorEngine.getMissionOrchestrator();
        if (orchestrator) {
          await orchestrator.approveTodo(todoId);
          this.sendToast(t('toast.taskApproved'), 'success');
          await this.sendStateUpdate();
          return true;
        }
      } catch (error) {
        logger.error('Agent 交互审批失败', error, LogCategory.AGENT);
        this.sendToast(t('toast.approvalFailed'), 'error');
        return false;
      }
      return false;
    }

    this.sendToast(t('toast.taskRejected'), 'info');
    const contextManager = this.orchestratorEngine.getContextManager();
    if (contextManager) {
      contextManager.addRejectedApproach(
        t('provider.approvalRejectedReason'),
        t('provider.approvalRejectedDetail'),
        'user',
      );
    }
    await this.sendStateUpdate();
    return true;
  }

  async confirmRecovery(decision: 'retry' | 'rollback' | 'continue'): Promise<boolean> {
    await this.ensureInitialized();
    const context = this.pendingRecoveryContext;
    if (!context) {
      return false;
    }

    if (decision === 'rollback') {
      const pendingChanges = this.snapshotManager.getPendingChanges();
      const latestMissionId = pendingChanges.length > 0
        ? pendingChanges[pendingChanges.length - 1].missionId
        : '';
      let revertedCount = 0;
      if (latestMissionId) {
        const result = this.snapshotManager.revertMission(latestMissionId);
        revertedCount = result.reverted;
      }
      const message = revertedCount > 0
        ? t('toast.roundRollback', { count: revertedCount })
        : t('toast.noChangesToRollback');
      this.sendToast(message, 'info');
      this.sendOrchestratorMessage({
        content: t('toast.rollbackComplete', { message }),
        messageType: 'result',
        metadata: { phase: 'recovery' },
      });
      this.clearPendingRecoveryState();
      await this.sendStateUpdate();
      return true;
    }

    if (decision === 'continue') {
      this.clearPendingRecoveryState();
      this.sendToast(t('toast.continueWithoutRollback'), 'info');
      await this.sendStateUpdate();
      return true;
    }

    if (this.orchestratorEngine.running || !context.canRetry) {
      return false;
    }

    const resumePrompt = this.buildResumePrompt(context.prompt, t('provider.resumePrompt.defaultRetry'));
    this.clearPendingRecoveryState();
    const requestId = `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    const result = await this.executeTask(resumePrompt, context.sessionId, requestId);
    return result.success;
  }

  async answerClarification(
    answers: Record<string, string> | null,
    additionalInfo?: string | null,
  ): Promise<boolean> {
    await this.ensureInitialized();
    const content = this.buildClarificationAnswerText(answers, additionalInfo);
    if (!content) {
      return false;
    }
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return false;
    }
    this.enqueueQueuedMessage(sessionId, content);
    this.sendQueuedMessagesUpdate(sessionId);
    void this.drainQueuedMessagesIfIdle();
    return true;
  }

  async answerWorkerQuestion(answer: string | null): Promise<boolean> {
    await this.ensureInitialized();
    const content = this.buildWorkerQuestionAnswerText(answer);
    if (!content) {
      return false;
    }
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return false;
    }
    this.enqueueQueuedMessage(sessionId, content);
    this.sendQueuedMessagesUpdate(sessionId);
    void this.drainQueuedMessagesIfIdle();
    return true;
  }

  async setInteractionMode(mode: string): Promise<void> {
    await this.ensureInitialized();
    this.orchestratorEngine.setInteractionMode(mode as UIState['interactionMode']);
    this.interactionModeUpdatedAt = Date.now();
    this.sendData('interactionModeChanged', {
      mode,
      updatedAt: this.interactionModeUpdatedAt,
    });
    await this.sendStateUpdate();
  }

  async clearAllTasks(): Promise<void> {
    await this.ensureInitialized();
    const tasks = await this.getTaskViews();
    for (const task of tasks) {
      await this.orchestratorEngine.deleteTaskById(task.id);
    }
    await this.sendStateUpdate();
  }

  async resetExecutionStats(): Promise<void> {
    await this.ensureInitialized();
    const executionStats = this.orchestratorEngine.getExecutionStats();
    if (!executionStats) {
      return;
    }
    await executionStats.clearStats();
    this.orchestratorEngine.resetOrchestratorTokenUsage();
    this.adapterFactory.resetAllTokenUsage();
    this.sendData(
      'executionStatsUpdate',
      await this.getExecutionStatsPayload() as unknown as Record<string, unknown>,
    );
  }

  async getExecutionStatsPayload(): Promise<ExecutionStatsPayload> {
    await this.ensureInitialized();
    const { LLMConfigLoader } = await import('../../llm/config');
    const modelCatalog = buildModelCatalogFromLLMConfig(LLMConfigLoader.loadFullConfig());
    return buildExecutionStatsPayload(this.orchestratorEngine.getExecutionStats(), modelCatalog);
  }

  async getSessionNotificationsPayload(sessionId?: string | null): Promise<SessionNotificationsPayload | null> {
    await this.ensureInitialized();
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return null;
    }
    const notifications = this.sessionManager.getSessionNotifications(resolvedSessionId);
    if (!notifications) {
      return null;
    }
    return {
      sessionId: resolvedSessionId,
      notifications,
    };
  }

  async markAllSessionNotificationsRead(sessionId?: string | null): Promise<SessionNotificationsPayload | null> {
    await this.ensureInitialized();
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return null;
    }
    const notifications = this.sessionManager.markAllSessionNotificationsRead(resolvedSessionId);
    if (!notifications) {
      return null;
    }
    const payload = { sessionId: resolvedSessionId, notifications };
    this.sendSessionNotificationsUpdate(payload);
    await this.sendStateUpdate();
    return payload;
  }

  async clearSessionNotifications(sessionId?: string | null): Promise<SessionNotificationsPayload | null> {
    await this.ensureInitialized();
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return null;
    }
    const notifications = this.sessionManager.clearSessionNotifications(resolvedSessionId);
    if (!notifications) {
      return null;
    }
    const payload = { sessionId: resolvedSessionId, notifications };
    this.sendSessionNotificationsUpdate(payload);
    await this.sendStateUpdate();
    return payload;
  }

  async removeSessionNotification(notificationId: string, sessionId?: string | null): Promise<SessionNotificationsPayload | null> {
    await this.ensureInitialized();
    const normalizedNotificationId = notificationId.trim();
    if (!normalizedNotificationId) {
      return null;
    }
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return null;
    }
    const notifications = this.sessionManager.removeSessionNotification(resolvedSessionId, normalizedNotificationId);
    if (!notifications) {
      return null;
    }
    const payload = { sessionId: resolvedSessionId, notifications };
    this.sendSessionNotificationsUpdate(payload);
    await this.sendStateUpdate();
    return payload;
  }

  /**
   * 注入 CodebaseRetrievalService 到 code_search_semantic 执行器
   * Web 模式下没有 PKB，仅依赖 grep + LSP 回退检索
   */
  private injectCodebaseRetrievalService(
    toolManager: ReturnType<LLMAdapterFactory['getToolManager']>,
    workspaceFolders: WorkspaceFolderInfo[],
  ): void {
    const retrievalService = new CodebaseRetrievalService({
      getKnowledgeBase: () => undefined,
      executeTool: async (toolCall: { id: string; name: string; arguments: Record<string, any> }) =>
        toolManager.executeInternalTool(toolCall),
      extractKeywords: (query: string) => {
        const words = query.split(/[\s,，。.!！?？;；:：()（）[\]【】{}]+/);
        const keywords: string[] = [];
        for (const word of words) {
          const cleaned = word.trim();
          if (cleaned.length < 2 || cleaned.length > 50) continue;
          if (/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(cleaned)) keywords.push(cleaned);
          if (/[\u4e00-\u9fa5]{2,}/.test(cleaned)) keywords.push(cleaned);
          if (/\.[a-z]{1,5}$/i.test(cleaned)) keywords.push(cleaned);
        }
        return [...new Set(keywords)].slice(0, 10);
      },
      workspaceFolders,
    });

    toolManager.getCodebaseRetrievalExecutor().setCodebaseRetrievalService(retrievalService);
    logger.info('AgentRuntime.CodebaseRetrieval 服务已注入', undefined, LogCategory.SESSION);
  }

  async enhancePrompt(prompt: string): Promise<{ enhancedPrompt: string; error?: string }> {
    await this.ensureInitialized();
    const { PromptEnhancerService } = await import('../../services/prompt-enhancer-service');
    const enhancer = new PromptEnhancerService({
      workspaceRoot: this.workspace.rootPath,
      getToolManager: () => this.adapterFactory.getToolManager(),
      getConversationHistory: (maxRounds: number) => this.getConversationHistory(maxRounds),
    });
    return enhancer.enhance(prompt);
  }

  async buildUIState(): Promise<UIState> {
    await this.ensureInitialized();
    const currentSession = this.sessionManager.getCurrentSession();
    const sessionId = this.activeSessionId || currentSession?.id;
    let taskViews: TaskView[] = [];
    if (sessionId) {
      taskViews = await this.getTaskViews(sessionId);
    }

    const pendingChanges = this.snapshotManager.getPendingChanges();
    const activePlan = sessionId ? this.orchestratorEngine.getActivePlanState(sessionId) : undefined;
    const planHistory = sessionId ? this.orchestratorEngine.getPlanLedgerSnapshot(sessionId).plans : [];

    return this.runtimeQueryService.queryState({
      sessionId,
      sessions: this.sessionManager.getSessionMetas(),
      taskViews,
      locale: this.resolveLocale(),
      workerStatuses: buildWorkerStatuses(this.adapterFactory),
      pendingChanges,
      isRunning: this.orchestratorEngine.running,
      logs: [...this.logs],
      interactionMode: this.orchestratorEngine.getInteractionMode(),
      interactionModeUpdatedAt: this.interactionModeUpdatedAt,
      orchestratorPhase: this.orchestratorEngine.phase,
      activePlan: activePlan ?? undefined,
      planHistory,
      stateUpdatedAt: Date.now(),
      recovered: false,
    });
  }

  private async getTaskViews(sessionId?: string): Promise<TaskView[]> {
    const normalizedSessionId = (
      sessionId
      || this.activeSessionId
      || this.sessionManager.getCurrentSession()?.id
      || ''
    ).trim();
    if (!normalizedSessionId) {
      return [];
    }
    return this.orchestratorEngine.listTaskViews(normalizedSessionId);
  }

  private getConversationHistory(maxRounds: number): string {
    const currentSession = this.sessionManager.getCurrentSession();
    if (!currentSession) {
      return '';
    }
    const messages = currentSession.messages
      .filter((message) => typeof message.content === 'string' && message.content.trim())
      .slice(-Math.max(1, maxRounds * 2));
    return messages.map((message) => {
      const role = message.role === 'assistant' ? 'Assistant' : message.role === 'system' ? 'System' : 'User';
      return `${role}: ${message.content}`;
    }).join('\n\n');
  }

  private syncTrace(): void {
    const traceId = this.activeSessionId
      || this.sessionManager.getCurrentSession()?.id
      || this.sessionManager.createSession().id;
    this.activeSessionId = traceId;
    this.orchestratorEngine.getMessageHub().setSessionId(traceId);
    this.orchestratorEngine.getMessageHub().setTraceId(traceId);
  }

  private emit(message: ClientBridgeMessage): void {
    this.listeners.forEach((listener) => {
      listener(message);
    });
  }

  private appendLog(entry: LogEntry): void {
    this.logs.push(entry);
    if (this.logs.length > 200) {
      this.logs.splice(0, this.logs.length - 200);
    }
  }

  private sendData(dataType: DataMessageType, payload: Record<string, unknown>): void {
    this.orchestratorEngine.getMessageHub().data(dataType, payload);
  }

  private sendSessionNotificationsUpdate(payload: SessionNotificationsPayload): void {
    this.sendData('sessionNotificationsLoaded', {
      sessionId: payload.sessionId,
      notifications: payload.notifications as unknown as Record<string, unknown>,
    });
  }

  private sendToast(message: string, level: NotifyLevel = 'info', duration?: number): void {
    this.orchestratorEngine.getMessageHub().notify(message, level, duration, {
      displayMode: 'toast',
      category: 'feedback',
      source: 'ui-feedback',
    });
  }

  getQueuedMessagesSnapshot(sessionId?: string): Array<{ id: string; content: string; createdAt: number }> {
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return [];
    }
    return this.getQueuedMessages(resolvedSessionId, false).map((item) => ({
      id: item.id,
      content: item.content,
      createdAt: item.createdAt,
    }));
  }

  getActiveMessageSnapshots(sessionId?: string): StandardMessage[] {
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return [];
    }
    return this.orchestratorEngine.getMessageHub()
      .getActiveMessageSnapshots()
      .filter((message) => {
        const binding = resolveStandardMessageSessionBinding(message);
        return binding.sessionId === resolvedSessionId;
      });
  }

  async getRuntimeDiagnostics(sessionId?: string): Promise<Record<string, unknown> | null> {
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return null;
    }
    const diagnostics = await this.orchestratorEngine.queryRuntimeDiagnostics({ sessionId: resolvedSessionId });
    return diagnostics as unknown as Record<string, unknown> | null;
  }

  buildExecutionChainSummary(sessionId?: string): ExecutionChainBootstrapSummary | undefined {
    const resolvedSessionId = this.resolveTargetSessionId(sessionId);
    if (!resolvedSessionId) {
      return undefined;
    }
    return this.orchestratorEngine.buildExecutionChainSummary(resolvedSessionId);
  }

  private getQueuedMessages(sessionId: string, createIfMissing = false): QueuedUserTurn[] {
    let queue = this.queuedMessagesBySession.get(sessionId);
    if (!queue && createIfMissing) {
      queue = [];
      this.queuedMessagesBySession.set(sessionId, queue);
    }
    return queue || [];
  }

  private enqueueQueuedMessage(sessionId: string, content: string): QueuedUserTurn {
    const queue = this.getQueuedMessages(sessionId, true);
    const item: QueuedUserTurn = {
      id: `queued_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
      content,
      createdAt: Date.now(),
    };
    queue.push(item);
    return item;
  }

  private sendQueuedMessagesUpdate(sessionId?: string): void {
    const resolvedSessionId = (sessionId || this.activeSessionId || this.sessionManager.getCurrentSession()?.id || '').trim();
    if (!resolvedSessionId) {
      return;
    }
    const queuedMessages = this.getQueuedMessages(resolvedSessionId, false).map((item) => ({
      id: item.id,
      content: item.content,
      createdAt: item.createdAt,
    }));
    this.sendData('queuedMessagesUpdated', {
      sessionId: resolvedSessionId,
      queuedMessages,
    });
  }

  private resolveTargetSessionId(sessionId?: string | null): string | null {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (normalizedSessionId) {
      const target = this.sessionManager.getSession(normalizedSessionId);
      if (!target) {
        return null;
      }
      this.sessionManager.switchSession(normalizedSessionId);
      this.activeSessionId = normalizedSessionId;
      this.syncTrace();
      return normalizedSessionId;
    }

    const currentSession = this.sessionManager.getCurrentSession() || this.sessionManager.createSession();
    this.activeSessionId = currentSession.id;
    this.syncTrace();
    return currentSession.id;
  }

  private async drainQueuedMessagesIfIdle(): Promise<void> {
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return;
    }
    if (this.queuedMessagesDrainRunning || this.orchestratorEngine.running) {
      return;
    }
    const initialQueue = this.getQueuedMessages(sessionId, false);
    if (initialQueue.length === 0) {
      return;
    }

    this.queuedMessagesDrainRunning = true;
    try {
      while (true) {
        if (this.orchestratorEngine.running) {
          break;
        }
        if ((this.activeSessionId || '') !== sessionId) {
          break;
        }
        const queue = this.getQueuedMessages(sessionId, false);
        const next = queue.shift();
        if (!next) {
          this.queuedMessagesBySession.delete(sessionId);
          this.sendQueuedMessagesUpdate(sessionId);
          break;
        }
        if (queue.length === 0) {
          this.queuedMessagesBySession.delete(sessionId);
        }
        this.sendQueuedMessagesUpdate(sessionId);
        const requestId = `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
        await this.executeTask(next.content, sessionId, requestId);
      }
    } finally {
      this.queuedMessagesDrainRunning = false;
      const remaining = this.getQueuedMessages(sessionId, false).length;
      if (remaining > 0 && !this.orchestratorEngine.running && (this.activeSessionId || '') === sessionId) {
        void this.drainQueuedMessagesIfIdle();
      }
    }
  }

  private sendOrchestratorMessage(params: {
    content?: string;
    messageType: 'progress' | 'error' | 'result' | 'text';
    metadata?: Record<string, unknown>;
    taskId?: string;
    blocks?: ContentBlock[];
  }): void {
    const { content, messageType, metadata, taskId, blocks } = params;
    let type: MessageType = MessageType.TEXT;
    let lifecycle: MessageLifecycle = MessageLifecycle.COMPLETED;

    if (messageType === 'progress') {
      type = MessageType.PROGRESS;
      lifecycle = MessageLifecycle.STREAMING;
    } else if (messageType === 'error') {
      type = MessageType.ERROR;
      lifecycle = MessageLifecycle.FAILED;
    } else if (messageType === 'result') {
      type = MessageType.RESULT;
    }

    const safeBlocks: ContentBlock[] = Array.isArray(blocks)
      ? blocks
      : (content ? [{ type: 'text' as const, content, isMarkdown: false }] : []);

    const standardMessage = createStandardMessage({
      traceId: this.activeSessionId || 'default',
      category: MessageCategory.CONTENT,
      type,
      source: 'orchestrator',
      agent: 'orchestrator',
      blocks: safeBlocks,
      lifecycle,
      metadata: {
        taskId,
        isStatusMessage: true,
        ...(this.activeSessionId ? { sessionId: this.activeSessionId } : {}),
        ...metadata,
      },
    });
    this.orchestratorEngine.getMessageHub().sendMessage(standardMessage);
  }

  private async sendStateUpdate(): Promise<void> {
    const state = await this.buildUIState();
    this.sendData('stateUpdate', { state });
  }

  private clearPendingRecoveryState(): void {
    this.pendingRecoveryContext = null;
  }

  private isRecoverableRuntimeReason(runtimeReason: AgentRuntimeReason): boolean {
    return runtimeReason === 'interrupted' || runtimeReason === 'upstream_model_error';
  }

  private buildExecutionFailureReason(runtimeReason: AgentRuntimeReason, errors: string[], fallback?: string): string {
    const normalizedError = [fallback, ...errors]
      .find((item): item is string => typeof item === 'string' && item.trim().length > 0)
      ?.trim();
    switch (runtimeReason) {
      case 'interrupted':
        return t('provider.taskAborted');
      case 'cancelled':
      case 'external_abort':
        return t('provider.userCancelled');
      default:
        return normalizedError || t('provider.executionFailed');
    }
  }

  private async publishOrchestratorRuntimeDiagnostics(input: {
    sessionId: string;
    requestId?: string;
    runtimeReason: AgentRuntimeReason;
    finalStatus: ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['finalStatus'];
    errors: string[];
    failureReason?: string;
    runtimeSnapshot: ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['runtimeSnapshot'];
    runtimeDecisionTrace: ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['runtimeDecisionTrace'];
  }): Promise<void> {
    const sessionId = input.sessionId.trim();
    if (!sessionId) {
      return;
    }
    const diagnostics = await this.orchestratorEngine.queryRuntimeDiagnostics({
      sessionId,
      requestId: input.requestId,
    });
    if (diagnostics) {
      this.sendData('orchestratorRuntimeDiagnostics', diagnostics as unknown as Record<string, unknown>);
    } else {
      logger.warn('AgentRuntime.运行态诊断缺失', {
        sessionId,
        requestId: input.requestId,
        runtimeReason: input.runtimeReason,
        finalStatus: input.finalStatus,
      }, LogCategory.AGENT);
    }
  }

  private setPendingRecoveryFromExecution(input: {
    result: {
      taskId: string;
      runtimeReason: AgentRuntimeReason;
      finalStatus: ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['finalStatus'];
      errors: string[];
      failureReason?: string;
      recoverable: boolean;
    };
    prompt: string;
    sessionId: string;
  }): void {
    if (!input.result.recoverable) {
      this.pendingRecoveryContext = null;
      return;
    }
    const taskId = input.result.taskId.trim();
    const prompt = input.prompt.trim();
    const sessionId = input.sessionId.trim();
    if (!taskId || !prompt || !sessionId) {
      this.pendingRecoveryContext = null;
      return;
    }
    const context: PendingRecoveryContext = {
      taskId,
      prompt,
      sessionId,
      runtimeReason: input.result.runtimeReason,
      errors: [...input.result.errors],
      canRetry: true,
      canRollback: this.snapshotManager.getPendingChanges().length > 0,
    };
    this.pendingRecoveryContext = context;
    this.sendData('recoveryRequest', {
      taskId: context.taskId,
      error: input.result.failureReason || context.errors[0] || t('provider.executionFailed'),
      canRetry: context.canRetry,
      canRollback: context.canRollback,
    });
  }

  private buildResumePrompt(originalPrompt: string, extraInstruction?: string): string {
    const pendingChanges = this.snapshotManager.getPendingChanges();
    const changeList = pendingChanges.length > 0
      ? pendingChanges.map((c) => `- ${c.filePath} (+${c.additions}/-${c.deletions})`).join('\n')
      : t('provider.resumePrompt.pendingChangesNone');
    const extra = extraInstruction
      ? `\n\n${t('provider.resumePrompt.extraInstruction', { instruction: extraInstruction })}`
      : '';
    return [
      t('provider.resumePrompt.header'),
      t('provider.resumePrompt.originalRequest', { prompt: originalPrompt }),
      t('provider.resumePrompt.generatedChanges', { changes: changeList }) + extra,
      t('provider.resumePrompt.footer'),
    ].join('\n\n');
  }

  private buildClarificationAnswerText(
    answers: Record<string, string> | null,
    additionalInfo?: string | null,
  ): string {
    const normalizedPairs = Object.entries(answers || {})
      .map(([question, answer]) => [question.trim(), String(answer || '').trim()] as const)
      .filter(([, answer]) => answer.length > 0);
    const normalizedAdditionalInfo = additionalInfo?.trim() || '';
    const parts: string[] = [];
    if (normalizedPairs.length > 0) {
      parts.push('以下是澄清答复：');
      for (const [question, answer] of normalizedPairs) {
        parts.push(`- ${question}: ${answer}`);
      }
    }
    if (normalizedAdditionalInfo) {
      parts.push(`补充说明：${normalizedAdditionalInfo}`);
    }
    return parts.join('\n').trim();
  }

  private buildWorkerQuestionAnswerText(answer: string | null): string {
    const normalizedAnswer = answer?.trim() || '';
    if (!normalizedAnswer) {
      return '';
    }
    return `针对 Worker 提问的回复：${normalizedAnswer}`;
  }

  private emitUserAndPlaceholder(
    requestId: string,
    prompt: string,
    turnId: string,
  ): {
    userMessageId: string;
    placeholderMessageId: string;
  } {
    const sessionId = this.activeSessionId?.trim();
    if (!sessionId) {
      throw new Error('[AgentRuntimeService] emitUserAndPlaceholder 缺少 activeSessionId');
    }
    const traceId = this.orchestratorEngine.getMessageHub().getTraceId();
    const userMessage = createUserInputMessage(prompt, traceId, {
      metadata: {
        requestId,
        turnId,
        sendingAnimation: true,
        sessionId,
      },
    });
    const placeholderMessage = createStreamingMessage('orchestrator', 'orchestrator', traceId, {
      metadata: {
        isPlaceholder: true,
        placeholderState: 'pending',
        requestId,
        userMessageId: userMessage.id,
        sessionId,
      },
    });
    userMessage.metadata.placeholderMessageId = placeholderMessage.id;
    this.orchestratorEngine.getMessageHub().sendMessage(userMessage);
    this.orchestratorEngine.getMessageHub().sendMessage(placeholderMessage);
    this.messageIdToRequestId.set(placeholderMessage.id, requestId);
    return {
      userMessageId: userMessage.id,
      placeholderMessageId: placeholderMessage.id,
    };
  }

  private saveAssistantMessage(
    assistantResponse: string,
    agent?: WorkerSlot,
    source?: MessageSource,
  ): void {
    if (!assistantResponse.trim() || !this.activeSessionId) {
      return;
    }
    const resolvedAgent = agent || (source === 'orchestrator' ? 'orchestrator' : undefined);
    this.sessionManager.addMessageToSession(this.activeSessionId, 'assistant', assistantResponse, resolvedAgent, source);
    void this.orchestratorEngine.recordContextMessage('assistant', assistantResponse, this.activeSessionId);
  }

  private persistStandardMessageToSession(sessionId: string, message: StandardMessage): void {
    if (!message?.id) {
      return;
    }
    const binding = resolveStandardMessageSessionBinding(message);
    const incomingSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (!binding.sessionId) {
      logger.warn('运行时.LifecycleCard.持久化跳过_缺少显式归属', {
        messageId: message.id,
        messageType: message.type,
        incomingSessionId,
        bindingSource: binding.source,
      }, LogCategory.UI);
      return;
    }
    if (!incomingSessionId) {
      logger.warn('运行时.LifecycleCard.持久化跳过_调用方缺少目标会话', {
        messageId: message.id,
        messageType: message.type,
        resolvedSessionId: binding.sessionId,
      }, LogCategory.UI);
      return;
    }
    if (binding.sessionId !== incomingSessionId) {
      logger.warn('运行时.LifecycleCard.持久化跳过_归属冲突', {
        messageId: message.id,
        messageType: message.type,
        incomingSessionId,
        metadataSessionId: binding.sessionId,
        bindingSource: binding.source,
      }, LogCategory.UI);
      return;
    }
    const session = this.sessionManager.getSession(binding.sessionId);
    if (!session) {
      logger.warn('运行时.LifecycleCard.持久化跳过_会话不存在', {
        incomingSessionId: sessionId,
        resolvedSessionId: binding.sessionId,
        messageId: message.id,
        messageType: message.type,
      }, LogCategory.UI);
      return;
    }

    // 跨会话泄漏防护：lifecycle 消息的时间戳早于目标会话的创建时间，
    // 说明该消息属于上一个会话的 Worker 任务（traceId 已被新会话覆盖），拒绝持久化。
    const messageTimestamp = typeof message.timestamp === 'number' && Number.isFinite(message.timestamp) && message.timestamp > 0
      ? message.timestamp : 0;
    const sessionCreatedAt = typeof session.createdAt === 'number' && Number.isFinite(session.createdAt) && session.createdAt > 0
      ? session.createdAt : 0;
    if (messageTimestamp > 0 && sessionCreatedAt > 0 && messageTimestamp < sessionCreatedAt) {
      logger.warn('运行时.LifecycleCard.持久化跳过_跨会话泄漏', {
        messageId: message.id,
        messageType: message.type,
        messageTimestamp,
        sessionCreatedAt,
        resolvedSessionId: binding.sessionId,
        incomingSessionId: sessionId,
      }, LogCategory.UI);
      return;
    }

    this.sessionManager.persistStandardMessageToSession(binding.sessionId, message);
  }

  private resolveLocale(): LocaleCode {
    const locale = this.options.getLocale();
    return locale === 'en-US' ? 'en-US' : 'zh-CN';
  }
}

export class AgentRuntimeManager {
  private readonly runtimes = new Map<string, AgentWorkspaceRuntime>();

  constructor(
    private readonly getLocale: () => SupportedLocale,
    private readonly getDeepTask: () => boolean,
  ) {}

  getRuntime(workspace: RuntimeWorkspaceDescriptor): AgentWorkspaceRuntime {
    const existing = this.runtimes.get(workspace.workspaceId);
    if (existing) {
      return existing;
    }
    const runtime = new AgentWorkspaceRuntime({
      workspace,
      getLocale: this.getLocale,
      getDeepTask: this.getDeepTask,
    });
    this.runtimes.set(workspace.workspaceId, runtime);
    return runtime;
  }

  removeRuntime(workspaceId: string): void {
    this.runtimes.delete(workspaceId);
  }

  /**
   * 重新加载所有 runtime 的 LLM 配置
   *
   * 在全局 LLM 配置变更后，遍历所有已创建的 runtime
   * 清除其 adapter 缓存，确保下次调用使用新配置。
   */
  async reloadAllLLMConfigs(target: 'orchestrator' | 'worker' | 'auxiliary', workerSlot?: string): Promise<void> {
    const reloadTasks: Promise<void>[] = [];
    for (const runtime of this.runtimes.values()) {
      reloadTasks.push(runtime.reloadLLMConfig(target, workerSlot));
    }
    await Promise.all(reloadTasks);
  }
}
