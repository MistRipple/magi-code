/**
 * WebviewProvider - Webview 面板提供者
 * 负责：对话面板、任务视图、变更视图、CLI 输出
 */

import { logger, LogCategory } from '../logging';
import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  CLIType,
  UIState,
  CLIStatus,
  CLIStatusCode,
  WebviewToExtensionMessage,
  ExtensionToWebviewMessage,
  MessageSource,
  LogEntry,
  PermissionMatrix,
  StrategyConfig,
} from '../types';
import {
  StandardMessage,
  StreamUpdate,
  MessageLifecycle,
  MessageType,
  ContentBlock,
} from '../protocol/message-protocol';
import { UnifiedSessionManager } from '../session';
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
import { SnapshotManager } from '../snapshot-manager';
import { DiffGenerator } from '../diff-generator';
import { globalEventBus } from '../events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { TaskAnalyzer, CLISelector } from '../task';
import { CLI_CAPABILITIES, CLIResponse } from '../cli/types';
import { IntelligentOrchestrator } from '../orchestrator/intelligent-orchestrator';
import { AceIndexManager } from '../ace/index-manager';
import { normalizeOrchestratorMessage, isInternalStateMessage } from '../normalizer';
import { UnifiedMessageBus, type ProcessingState } from '../normalizer/unified-message-bus';
import { ProfileStorage, StoredProfileConfig } from '../orchestrator/profile';
import { parseContentToBlocks } from '../utils/content-parser';
// Mission-Driven Architecture 类型 - 直接从子模块导入
import {
  MissionOrchestrator,
  MissionExecutor,
  ExecutionProgress,
  BlockedItem,
  MissionSummary,
  MissionVerificationResult,
} from '../orchestrator/core';
import {
  Mission,
  Assignment,
  WorkerTodo,
} from '../orchestrator/mission';

export class WebviewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'multiCli.mainView';

  private _view?: vscode.WebviewView;
  private sessionManager: UnifiedSessionManager;
  private taskManager: UnifiedTaskManager | null = null;
  private taskManagerSessionId: string | null = null;
  private taskManagerReady: Promise<void> | null = null;
  private snapshotManager: SnapshotManager;
  private diffGenerator: DiffGenerator;
  private cliStatuses: Map<CLIType, CLIStatus> = new Map();
  private cliOutputs: Map<CLIType, string[]> = new Map();
  private readonly messageFlowLogEnabled = process.env.MULTICLI_MESSAGE_FLOW_LOG === '1';
  private readonly messageFlowLogPath: string;

  // 统一消息总线（替代原有的 MessageDeduplicator）
  private messageBus: UnifiedMessageBus;

  // 多 CLI 适配器工厂
  private cliFactory: CLIAdapterFactory;

  // 任务分析器和 CLI 选择器
  private taskAnalyzer: TaskAnalyzer;
  private cliSelector: CLISelector;

  // 智能编排器
  private intelligentOrchestrator: IntelligentOrchestrator;

  // Mission-Driven 编排器（新架构）
  private missionOrchestrator?: MissionOrchestrator;
  private missionExecutor?: MissionExecutor;

  // Hard Stop 确认机制
  private pendingConfirmation: {
    resolve: (confirmed: boolean) => void;
    reject: (error: Error) => void;
  } | null = null;
  private pendingQuestion: {
    resolve: (answer: string | null) => void;
    reject: (error: Error) => void;
  } | null = null;
  // 需求澄清机制
  private pendingClarification: {
    resolve: (result: { answers: Record<string, string>; additionalInfo?: string } | null) => void;
    reject: (error: Error) => void;
  } | null = null;
  // Worker 问题机制
  private pendingWorkerQuestion: {
    resolve: (answer: string | null) => void;
    reject: (error: Error) => void;
  } | null = null;

  // 当前选择的 CLI（null 表示自动选择/智能编排）
  private selectedCli: CLIType | null = null;


  private activeSessionId: string | null = null;
  private logs: LogEntry[] = [];
  private logFlushTimer: NodeJS.Timeout | null = null;

 
  private readonly authSecretKey = 'multiCli.apiKey';
  private readonly authStatusKey = 'multiCli.loggedIn';
  private loginInFlight = false;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly context: vscode.ExtensionContext,
    private readonly workspaceRoot: string
  ) {
    this.messageFlowLogPath = path.join(this.workspaceRoot, '.multicli', 'logs', 'message-flow.jsonl');

    // 初始化统一消息总线（替代原有的 MessageDeduplicator）
    this.messageBus = new UnifiedMessageBus({
      enabled: true,
      minStreamInterval: 100,
      batchInterval: 50,
      retentionTime: 5 * 60 * 1000,
      debug: false,
    });
    this.setupMessageBusListeners();

    // 初始化统一会话管理器
    this.sessionManager = new UnifiedSessionManager(workspaceRoot);
    // 统一任务管理器（按会话初始化）
    this.snapshotManager = new SnapshotManager(this.sessionManager, workspaceRoot);
    this.diffGenerator = new DiffGenerator(this.sessionManager, workspaceRoot);

    // 确保有当前会话
    this.ensureSessionAlignment();
    if (this.activeSessionId) {
      void this.initTaskManagerForSession(this.activeSessionId);
    }

    // 初始化任务分析器和 CLI 选择器（画像系统驱动）
    this.taskAnalyzer = new TaskAnalyzer();
    const config = vscode.workspace.getConfiguration('multiCli');
    const timeout = config.get<number>('timeout') ?? 300000;
    const idleTimeout = config.get<number>('idleTimeout') ?? 120000;
    const maxTimeout = config.get<number>('maxTimeout') ?? 900000;
    const cliPaths = {
      claude: config.get<string>('claude.path') ?? 'claude',
      codex: config.get<string>('codex.path') ?? 'codex',
      gemini: config.get<string>('gemini.path') ?? 'gemini',
    };
    const permissions = this.normalizePermissions(config.get<Partial<PermissionMatrix>>('permissions'));
    const strategy = this.normalizeStrategy(config.get<Partial<StrategyConfig>>('strategy'));
    const cliSelection = config.get<{ enabled?: boolean; healthThreshold?: number }>('cliSelection') || {};

    // 初始化多 CLI 适配器工厂
    this.cliFactory = new CLIAdapterFactory({ cwd: workspaceRoot, idleTimeout, maxTimeout, cliPaths });
    this.setupCLIAdapters();
    this.cliSelector = new CLISelector();
    this.cliSelector.configureSmartSelection({
      enabled: cliSelection.enabled,
      healthThreshold: cliSelection.healthThreshold,
    });

    // 初始化智能编排器
    this.intelligentOrchestrator = new IntelligentOrchestrator(
      this.cliFactory,
      this.sessionManager,
      this.snapshotManager,
      this.workspaceRoot,
      { timeout, idleTimeout, maxTimeout, permissions, strategy, cliSelection }
    );
    // 设置 Hard Stop 确认回调
    this.setupOrchestratorConfirmation();
    this.setupOrchestratorQuestions();

    // 初始化 CLI 输出缓冲
    this.cliOutputs.set('claude', []);
    this.cliOutputs.set('codex', []);
    this.cliOutputs.set('gemini', []);

    // 绑定事件
    this.bindEvents();
  }

  private logMessageFlow(eventType: string, payload: unknown): void {
    if (!this.messageFlowLogEnabled) return;
    try {
      const dir = path.dirname(this.messageFlowLogPath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      const record = {
        timestamp: Date.now(),
        sessionId: this.activeSessionId,
        eventType,
        payload,
      };
      fs.appendFileSync(this.messageFlowLogPath, JSON.stringify(record) + '\n', 'utf-8');
    } catch (error) {
      logger.warn('界面.消息流.写入_失败', error, LogCategory.UI);
    }
  }

  private async initTaskManagerForSession(sessionId: string): Promise<void> {
    if (this.taskManager && this.taskManagerSessionId === sessionId) {
      return;
    }
    const repository = new SessionManagerTaskRepository(this.sessionManager, sessionId);
    const manager = new UnifiedTaskManager(sessionId, repository);
    this.taskManager = manager;
    this.taskManagerSessionId = sessionId;
    this.taskManagerReady = manager.initialize();
    await this.taskManagerReady;
    this.intelligentOrchestrator.setTaskManager(manager, sessionId);
  }

  private async getTaskManager(): Promise<UnifiedTaskManager> {
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      throw new Error('未找到有效的会话 ID');
    }
    await this.initTaskManagerForSession(sessionId);
    if (!this.taskManager) {
      throw new Error('UnifiedTaskManager 未初始化');
    }
    return this.taskManager;
  }

  private normalizePermissions(input?: Partial<PermissionMatrix>): PermissionMatrix {
    return {
      allowEdit: input?.allowEdit ?? true,
      allowBash: input?.allowBash ?? true,
      allowWeb: input?.allowWeb ?? true,
    };
  }

  private normalizeStrategy(input?: Partial<StrategyConfig>): StrategyConfig {
    return {
      enableVerification: input?.enableVerification ?? true,
      enableRecovery: input?.enableRecovery ?? true,
      autoRollbackOnFailure: input?.autoRollbackOnFailure ?? false,
    };
  }

  /** 设置所有 CLI 适配器事件监听 */
  private setupCLIAdapters(): void {
    // 🔧 重构：所有消息通过 UnifiedMessageBus 发送
    // CLI Adapter 的事件直接接入 MessageBus
    this.cliFactory.on('standardMessage', (message: any) => {
      this.messageBus.sendMessage(message);
    });

    this.cliFactory.on('standardUpdate', (update: any) => {
      this.messageBus.sendUpdate(update);
    });

    this.cliFactory.on('standardComplete', (message: any) => {
      this.messageBus.sendMessage(message);
    });

    this.cliFactory.on('stateChange', ({ type, state }: { type: CLIType; state: string }) => {
      const status: CLIStatus = {
        type: type,
        code: state === 'error' ? CLIStatusCode.RUNTIME_ERROR : CLIStatusCode.AVAILABLE,
        available: state !== 'error',
        path: type,
      };
      this.cliStatuses.set(type, status);
      this.sendStateUpdate();
    });
  }

  /** 设置 MessageBus 事件监听，转发消息到前端 */
  private setupMessageBusListeners(): void {
    // MessageBus 事件转发到前端
    this.messageBus.on('message', (message) => {
      this.postMessage({
        type: 'standardMessage',
        message,
        sessionId: this.activeSessionId
      } as any);
      this.logMessageFlow('standardMessage [SENT]', message);
    });

    this.messageBus.on('update', (update) => {
      this.postMessage({
        type: 'standardUpdate',
        update,
        sessionId: this.activeSessionId
      } as any);
      this.logMessageFlow('standardUpdate [SENT]', update);
    });

    this.messageBus.on('complete', (message) => {
      this.postMessage({
        type: 'standardComplete',
        message,
        sessionId: this.activeSessionId
      } as any);
      this.logMessageFlow('standardComplete [SENT]', message);
    });

    // 处理状态变化 - 推送到前端
    this.messageBus.on('processingStateChanged', (state) => {
      this.postMessage({
        type: 'processingStateChanged',
        state,
        sessionId: this.activeSessionId
      } as any);
    });
  }

  /** 设置智能编排器的 Hard Stop 确认回调 */
  private setupOrchestratorConfirmation(): void {
    // 设置 Hard Stop 确认回调
    this.intelligentOrchestrator.setConfirmationCallback(async (plan, formattedPlan) => {
      const mode = this.intelligentOrchestrator.getInteractionMode();
      if (mode === 'auto') {
        logger.info('界面.编排器.确认.自动_跳过', { mode }, LogCategory.UI);
        return true;
      }
      return new Promise<boolean>((resolve, reject) => {
        // 保存 resolve/reject 以便后续处理用户响应
        this.pendingConfirmation = { resolve, reject };

        // 发送确认请求消息
        this.postMessage({
          type: 'confirmationRequest',
          plan: plan,
          formattedPlan: formattedPlan,
        } as any);

        logger.info('界面.编排器.确认.等待', { mode }, LogCategory.UI);
      });
    });

    // 设置恢复确认回调
    this.intelligentOrchestrator.setRecoveryConfirmationCallback(async (failedTask, error, options) => {
      return new Promise<'retry' | 'rollback' | 'continue'>((resolve) => {
        // 保存 resolver
        this.recoveryConfirmationResolver = resolve;

        // 发送恢复请求到 Webview
        this.postMessage({
          type: 'recoveryRequest',
          taskId: failedTask.id,
          error: error,
          canRetry: options.retry,
          canRollback: options.rollback,
        });

        logger.info('界面.编排器.恢复.等待', { taskId: failedTask.id }, LogCategory.UI);
      });
    });
  }

  /** 设置编排者补充问题回调 */
  private setupOrchestratorQuestions(): void {
    this.intelligentOrchestrator.setQuestionCallback(async (questions, plan) => {
      return new Promise<string | null>((resolve, reject) => {
        this.pendingQuestion = { resolve, reject };
        this.postMessage({
          type: 'questionRequest',
          questions,
          plan
        } as any);
        logger.info('界面.编排器.提问.等待', undefined, LogCategory.UI);
      });
    });

    // 设置需求澄清回调
    this.intelligentOrchestrator.setClarificationCallback(async (questions, context, ambiguityScore, originalPrompt) => {
      return new Promise((resolve, reject) => {
        this.pendingClarification = { resolve, reject };
        this.postMessage({
          type: 'clarificationRequest',
          questions,
          context,
          ambiguityScore,
          originalPrompt,
          sessionId: this.activeSessionId
        } as any);
        logger.info('界面.编排器.澄清.等待', { ambiguityScore }, LogCategory.UI);
      });
    });

    // 设置 Worker 问题回调
    this.intelligentOrchestrator.setWorkerQuestionCallback(async (workerId, question, context, options) => {
      return new Promise((resolve, reject) => {
        this.pendingWorkerQuestion = { resolve, reject };
        this.postMessage({
          type: 'workerQuestionRequest',
          workerId,
          question,
          context,
          options,
          sessionId: this.activeSessionId
        } as any);
        logger.info('界面.子代理.提问.等待', { workerId }, LogCategory.UI);
      });
    });
  }

  /** 处理用户对执行计划的确认响应 */
  private handlePlanConfirmation(confirmed: boolean): void {
    if (this.pendingConfirmation) {
      logger.info('界面.编排器.确认.结果', { confirmed }, LogCategory.UI);
      this.pendingConfirmation.resolve(confirmed);
      this.pendingConfirmation = null;

      // 通知 Webview 确认已处理
      this.postMessage({
        type: 'toast',
        message: confirmed ? '执行计划已确认，开始执行...' : '执行计划已取消',
        toastType: confirmed ? 'success' : 'info',
      });
    }
  }

  /** 处理用户补充问题的回答 */
  private handleQuestionAnswer(answer: string | null): void {
    if (this.pendingQuestion) {
      const normalized = answer && answer.trim().length > 0 ? answer.trim() : null;
      this.pendingQuestion.resolve(normalized);
      this.pendingQuestion = null;
      this.postMessage({
        type: 'toast',
        message: normalized ? '已提交问题回答，继续分析...' : '已取消问题补充',
        toastType: normalized ? 'success' : 'info',
      });
    }
  }

  /** 处理用户澄清回答 */
  private handleClarificationAnswer(answers: Record<string, string> | null, additionalInfo?: string): void {
    if (this.pendingClarification) {
      if (answers && Object.keys(answers).length > 0) {
        this.pendingClarification.resolve({ answers, additionalInfo });
        this.postMessage({
          type: 'toast',
          message: '已提交澄清信息，继续分析...',
          toastType: 'success',
        });
      } else {
        this.pendingClarification.resolve(null);
        this.postMessage({
          type: 'toast',
          message: '已跳过澄清，使用原始需求...',
          toastType: 'info',
        });
      }
      this.pendingClarification = null;
    }
  }

  /** 处理 Worker 问题回答 */
  private handleWorkerQuestionAnswer(answer: string | null): void {
    if (this.pendingWorkerQuestion) {
      this.pendingWorkerQuestion.resolve(answer);
      this.pendingWorkerQuestion = null;
      this.postMessage({
        type: 'toast',
        message: answer ? '已回答 Worker 问题，继续执行...' : '已跳过 Worker 问题...',
        toastType: answer ? 'success' : 'info',
      });
    }
  }

  /** 处理用户回答 CLI 询问 */
  private handleCliQuestionAnswer(
    cli: CLIType,
    questionId: string,
    answer: string,
    adapterRole?: 'worker' | 'orchestrator'
  ): void {
    logger.info('界面.CLI.提问.回答', { cli, questionId, answer, role: adapterRole || 'worker' }, LogCategory.UI);

    const role = adapterRole || 'worker';
    const success = this.cliFactory.writeInput(cli, answer, role);

    if (success) {
      this.postMessage({
        type: 'cliQuestionAnswered',
        cli: cli,
        questionId: questionId,
        answer: answer,
        success: true,
        sessionId: this.activeSessionId
      } as any);

      this.postMessage({
        type: 'toast',
        message: `已发送回答: ${answer}`,
        toastType: 'success',
      });
    } else {
      this.postMessage({
        type: 'cliQuestionAnswered',
        cli: cli,
        questionId: questionId,
        answer: answer,
        success: false,
        error: 'CLI 未在等待输入或会话已关闭',
        sessionId: this.activeSessionId
      } as any);

      this.postMessage({
        type: 'toast',
        message: '发送回答失败：CLI 未在等待输入',
        toastType: 'error',
      });
    }
  }

  /** 绑定全局事件 */
  private bindEvents(): void {
    // 任务相关事件
    globalEventBus.on('task:created', () => this.sendStateUpdate());
    globalEventBus.on('task:state_changed', () => this.sendStateUpdate());
    globalEventBus.on('task:started', (event) => {
      this.sendStateUpdate();
     
      this.postMessage({
        type: 'phaseChanged',
        phase: 'started',
        taskId: event.taskId || '',
        isRunning: true
      } as any);
    });
    globalEventBus.on('task:completed', (event) => {
      this.sendStateUpdate();
     
      this.postMessage({
        type: 'phaseChanged',
        phase: 'completed',
        taskId: event.taskId || '',
        isRunning: false
      } as any);
    });
    globalEventBus.on('task:failed', (event) => {
      this.sendStateUpdate();
     
      this.postMessage({
        type: 'phaseChanged',
        phase: 'failed',
        taskId: event.taskId || '',
        isRunning: false
      } as any);
    });
    globalEventBus.on('task:cancelled', (event) => {
      this.sendStateUpdate();

      this.postMessage({
        type: 'phaseChanged',
        phase: 'cancelled',
        taskId: event.taskId || '',
        isRunning: false
      } as any);
    });
    globalEventBus.on('subtask:started', (event) => {
      const data = event.data as { cli?: string; description?: string; targetFiles?: string[]; reason?: string; dispatchId?: string };
      if (data?.description) {
        // 发送到主对话窗口（使用标准消息）
        this.sendOrchestratorMessage({
          content: `子任务开始: ${data.description}`,
          messageType: 'progress',
          taskId: event.taskId,
          metadata: {
            subTaskId: event.subTaskId || '',
            status: 'started',
            cli: data.cli || 'system',
            description: data.description,
            dispatchId: data.dispatchId,
          },
        });

        // 发送任务卡片到对应的 CLI 面板
        if (data.cli) {
          this.postMessage({
            type: 'cliTaskCard',
            cli: data.cli as CLIType,
            taskId: event.taskId || '',
            subTaskId: event.subTaskId || '',
            description: data.description,
            targetFiles: data.targetFiles || [],
            reason: data.reason || '',
            status: 'started',
            dispatchId: data.dispatchId,
            sessionId: this.activeSessionId,
          });
        }
      }
      this.sendStateUpdate();
    });
    globalEventBus.on('subtask:completed', (event) => {
      const data = event.data as { success?: boolean; cli?: string; cliType?: string; description?: string; modifiedFiles?: string[]; duration?: number };
      const summaryCard = this.buildSubTaskSummaryCard({
        description: data?.description,
        cli: data?.cli || data?.cliType,
        duration: data?.duration,
        modifiedFiles: data?.modifiedFiles,
        subTaskId: event.subTaskId,
      }, 'completed');
      this.sendOrchestratorMessage({
        content: '',
        messageType: 'result',
        taskId: event.taskId,
        metadata: {
          subTaskId: event.subTaskId || '',
          status: 'completed',
          success: data?.success ?? true,
          cli: data?.cli || data?.cliType || '',
          description: data?.description,
          subTaskCard: summaryCard,
        },
      });
      this.sendStateUpdate();
    });
    globalEventBus.on('execution:stats_updated', () => {
      this.sendExecutionStats();
    });
    globalEventBus.on('subtask:failed', (event) => {
      const data = event.data as { error?: string | object; cli?: string; cliType?: string; description?: string; modifiedFiles?: string[]; duration?: number };

      let errorMsg = '未知错误';
      if (data?.error) {
        if (typeof data.error === 'string') {
          errorMsg = data.error;
        } else if (typeof data.error === 'object') {
          // 尝试提取错误信息
          const errObj = data.error as { message?: string; error?: string };
          errorMsg = errObj.message || errObj.error || JSON.stringify(data.error);
        }
      }
      const summaryCard = this.buildSubTaskSummaryCard({
        description: data?.description,
        cli: data?.cli || data?.cliType,
        duration: data?.duration,
        modifiedFiles: data?.modifiedFiles,
        subTaskId: event.subTaskId,
        error: errorMsg,
      }, 'failed');
      this.sendOrchestratorMessage({
        content: '',
        messageType: 'error',
        taskId: event.taskId,
        metadata: {
          subTaskId: event.subTaskId || '',
          status: 'failed',
          cli: data?.cli || data?.cliType || '',
          error: errorMsg,
          description: data?.description,
          subTaskCard: summaryCard,
        },
      });
      this.sendStateUpdate();
    });


    globalEventBus.on('orchestrator:ui_message', (event) => {
      const data = event.data as any;
      if (!data?.content) return;

      // 🔍 检查点 1：记录原始 CLI 数据
      console.log('[DEBUG-LAYER-1] 原始 Orchestrator 消息:', {
        type: data.type,
        contentLength: data.content.length,
        contentPreview: data.content.substring(0, 200),
        hasJson: /\{[\s\S]*"[^"]+"\s*:/.test(data.content),
      });

      // 过滤内部状态消息
      if (isInternalStateMessage(data)) {
        return;
      }

      // 转换为标准消息格式（只发送标准消息）
      const standardMessage = normalizeOrchestratorMessage(data, event.taskId);

      this.postMessage({
        type: 'standardMessage',
        message: standardMessage,
        sessionId: this.activeSessionId,
      } as any);
    });

   
    globalEventBus.on('orchestrator:phase_changed', (event) => {
      const data = event.data as { phase: string; isRunning?: boolean; timestamp?: number };
      if (data?.phase) {
        // 修复页面跳动：只发送 phaseChanged 消息，不触发 sendStateUpdate
        // phaseChanged 只更新阶段指示器，不会重建整个 DOM
        this.postMessage({
          type: 'phaseChanged',
          phase: data.phase,
          taskId: event.taskId || '',
          isRunning: data.isRunning ?? this.intelligentOrchestrator.running
        } as any);
        // 移除 sendStateUpdate() 调用，避免频繁 DOM 重建导致页面跳动
      }
    });

    globalEventBus.on('orchestrator:dependency_analysis', (event) => {
      const data = event.data as { message?: string };

      // 记录简要信息到日志
      if (data?.message) {
        this.appendLog({
          level: 'info',
          message: data.message,
          source: 'orchestrator',
          timestamp: Date.now(),
        });
      }

      // 发送完整依赖分析数据到前端进行可视化
      if (this._view) {
        this._view.webview.postMessage({
          type: 'dependencyAnalysis',
          data: event.data,
        });
      }
    });

    // 打断任务事件
    globalEventBus.on('task:cancelled', () => {
      this.interruptCurrentTask();
    });

    globalEventBus.on('snapshot:created', () => this.sendStateUpdate());
    globalEventBus.on('snapshot:reverted', () => this.sendStateUpdate());

    // CLI 状态相关事件
    globalEventBus.on('cli:statusChanged', (event) => {
      const data = event.data as { cli: string; available: boolean; version?: string };
      this.sendStateUpdate();
      // 通知 UI CLI 状态变化
      this.postMessage({ type: 'cliStatusChanged', cli: data.cli, available: data.available, version: data.version });
    });

    globalEventBus.on('cli:healthCheck', () => {
      this.sendStateUpdate();
    });

    globalEventBus.on('cli:error', (event) => {
      const data = event.data as { cli: string; error: string };
      // 通知 UI 显示错误
      this.postMessage({ type: 'cliError', cli: data.cli, error: data.error });
    });

    globalEventBus.on('cli:session_event', (event) => {
      const data = event.data as {
        type?: string;
        cli?: CLIType;
        role?: string;
        requestId?: string;
        reason?: string;
        error?: string;
      };
      const pieces = [
        data?.type || 'session',
        data?.cli ? `cli=${data.cli}` : '',
        data?.role ? `role=${data.role}` : '',
        data?.requestId ? `req=${data.requestId}` : '',
        data?.reason ? `reason=${data.reason}` : '',
        data?.error ? `error=${data.error}` : '',
      ].filter(Boolean);
      const level = data?.type?.includes('failed') ? 'error' : 'info';
      this.appendLog({
        level,
        message: pieces.join(' '),
        source: data?.cli ?? 'system',
        timestamp: Date.now(),
      });
      // session_event 仅写入日志，不推送到 CLI 面板，避免干扰用户对话
    });

    // ============= Mission-Driven 架构事件 =============
    // 这些事件来自 MissionOrchestrator 和 MissionExecutor
    this.bindMissionEvents();
  }

  /**
   * 绑定 Mission-Driven 架构事件
   * 将 MissionOrchestrator 和 MissionExecutor 的事件转发到 Webview
   */
  private bindMissionEvents(): void {
    // 如果 MissionOrchestrator 未初始化，跳过
    if (!this.missionOrchestrator) return;

    // Mission 生命周期事件
    this.missionOrchestrator.on('missionCreated', (data: { mission: Mission }) => {
      this.postMessage({
        type: 'missionCreated',
        mission: data.mission,
        sessionId: this.activeSessionId,
      } as any);
      this.sendStateUpdate();
    });

    this.missionOrchestrator.on('missionPlanned', (data: { mission: Mission; contracts: any[]; assignments: Assignment[] }) => {
      this.postMessage({
        type: 'missionPlanned',
        missionId: data.mission.id,
        contracts: data.contracts,
        assignments: data.assignments,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionOrchestrator.on('missionApproved', (data: { mission: Mission }) => {
      this.postMessage({
        type: 'missionApproved',
        missionId: data.mission.id,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionOrchestrator.on('missionCompleted', (data: { mission: Mission }) => {
      this.postMessage({
        type: 'missionCompleted',
        missionId: data.mission.id,
        sessionId: this.activeSessionId,
      } as any);
      this.sendStateUpdate();
    });

    this.missionOrchestrator.on('missionFailed', (data: { mission: Mission; error: string }) => {
      this.postMessage({
        type: 'missionFailed',
        missionId: data.mission.id,
        error: data.error,
        sessionId: this.activeSessionId,
      } as any);
      this.sendStateUpdate();
    });

    this.missionOrchestrator.on('missionPaused', (data: { mission: Mission; reason: string }) => {
      this.postMessage({
        type: 'missionPaused',
        missionId: data.mission.id,
        reason: data.reason,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionOrchestrator.on('missionResumed', (data: { mission: Mission }) => {
      this.postMessage({
        type: 'missionResumed',
        missionId: data.mission.id,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionOrchestrator.on('missionCancelled', (data: { mission: Mission; reason: string }) => {
      this.postMessage({
        type: 'missionCancelled',
        missionId: data.mission.id,
        reason: data.reason,
        sessionId: this.activeSessionId,
      } as any);
      this.sendStateUpdate();
    });

    // 验证事件
    this.missionOrchestrator.on('verificationStarted', (data: { missionId: string }) => {
      this.postMessage({
        type: 'missionVerificationStarted',
        missionId: data.missionId,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionOrchestrator.on('verificationCompleted', (data: { missionId: string; result: MissionVerificationResult }) => {
      this.postMessage({
        type: 'missionVerificationCompleted',
        missionId: data.missionId,
        result: data.result,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 总结事件
    this.missionOrchestrator.on('summarizationCompleted', (data: { missionId: string; summary: MissionSummary }) => {
      this.postMessage({
        type: 'missionSummary',
        missionId: data.missionId,
        summary: data.summary,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 如果 MissionExecutor 未初始化，跳过执行事件
    if (!this.missionExecutor) return;

    // 执行事件
    this.missionExecutor.on('executionStarted', (data: { missionId: string }) => {
      this.postMessage({
        type: 'missionExecutionStarted',
        missionId: data.missionId,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionExecutor.on('executionCompleted', (data: any) => {
      this.postMessage({
        type: 'missionExecutionCompleted',
        missionId: data.mission.id,
        success: data.success,
        duration: data.duration,
        sessionId: this.activeSessionId,
      } as any);
      this.sendStateUpdate();
    });

    this.missionExecutor.on('executionFailed', (data: { missionId: string; error: string }) => {
      this.postMessage({
        type: 'missionExecutionFailed',
        missionId: data.missionId,
        error: data.error,
        sessionId: this.activeSessionId,
      } as any);
      this.sendStateUpdate();
    });

    // Assignment 事件
    this.missionExecutor.on('assignmentStarted', (data: { missionId: string; assignmentId: string; workerId: CLIType }) => {
      this.postMessage({
        type: 'assignmentStarted',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionExecutor.on('assignmentCompleted', (data: { missionId: string; assignmentId: string; success: boolean }) => {
      this.postMessage({
        type: 'assignmentCompleted',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        success: data.success,
        sessionId: this.activeSessionId,
      } as any);
    });

    // Todo 事件
    this.missionExecutor.on('todoStarted', (data: { missionId: string; assignmentId: string; todoId: string }) => {
      this.postMessage({
        type: 'todoStarted',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionExecutor.on('todoCompleted', (data: { missionId: string; assignmentId: string; todoId: string; output: any }) => {
      this.postMessage({
        type: 'todoCompleted',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        output: data.output,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionExecutor.on('todoFailed', (data: { missionId: string; assignmentId: string; todoId: string; error: string }) => {
      this.postMessage({
        type: 'todoFailed',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        error: data.error,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 动态 Todo 事件
    this.missionExecutor.on('dynamicTodoAdded', (data: { missionId: string; assignmentId: string; todo: WorkerTodo }) => {
      this.postMessage({
        type: 'dynamicTodoAdded',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todo: data.todo,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 审批请求事件
    this.missionExecutor.on('approvalRequested', (data: { missionId: string; assignmentId: string; todoId: string; reason: string }) => {
      this.postMessage({
        type: 'todoApprovalRequested',
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        reason: data.reason,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 阻塞事件
    this.missionExecutor.on('blocked', (blockedItem: BlockedItem) => {
      this.postMessage({
        type: 'missionBlocked',
        blockedItem,
        sessionId: this.activeSessionId,
      } as any);
    });

    this.missionExecutor.on('unblocked', (blockedItem: BlockedItem) => {
      this.postMessage({
        type: 'missionUnblocked',
        blockedItem,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 进度更新事件
    this.missionExecutor.on('progressUpdated', (progress: ExecutionProgress) => {
      this.postMessage({
        type: 'missionProgress',
        progress,
        sessionId: this.activeSessionId,
      } as any);
    });

    // 契约验证事件
    this.missionExecutor.on('contractVerified', (data: { missionId: string; contractId: string; success: boolean }) => {
      this.postMessage({
        type: 'contractVerified',
        missionId: data.missionId,
        contractId: data.contractId,
        success: data.success,
        sessionId: this.activeSessionId,
      } as any);
    });
  }

  /**
   * 设置 MissionOrchestrator
   * 用于 Mission-Driven 架构
   */
  setMissionOrchestrator(orchestrator: MissionOrchestrator): void {
    this.missionOrchestrator = orchestrator;
    this.bindMissionEvents();
  }

  /**
   * 设置 MissionExecutor
   * 用于 Mission-Driven 架构
   */
  setMissionExecutor(executor: MissionExecutor): void {
    this.missionExecutor = executor;
    this.bindMissionEvents();
  }

  /**
   * 获取 MissionOrchestrator
   */
  getMissionOrchestrator(): MissionOrchestrator | undefined {
    return this.missionOrchestrator;
  }

  /**
   * 获取 MissionExecutor
   */
  getMissionExecutor(): MissionExecutor | undefined {
    return this.missionExecutor;
  }

  /** 打断当前任务 - 增强版：添加等待和超时机制 */
  private async interruptCurrentTask(options?: { silent?: boolean }): Promise<void> {
    logger.info('界面.任务.中断.请求', undefined, LogCategory.UI);


    const taskManager = await this.getTaskManager();
    const tasks = await taskManager.getAllTasks();
    const runningTask = tasks.find(t => t.status === 'running');
    const hasRunningTask = runningTask || this.intelligentOrchestrator.running;


    // 1. 首先中断 Orchestrator（这会触发 AbortController）
    if (this.intelligentOrchestrator.running) {
      logger.info('界面.任务.中断.编排器', undefined, LogCategory.UI);
      await this.intelligentOrchestrator.interrupt();
    }

    // 2. 中断所有 CLI 并等待完成
    logger.info('界面.任务.中断.CLI.开始', undefined, LogCategory.UI);
    try {
      const interruptCompleted = await Promise.race([
        this.cliFactory.interruptAll().then(() => true),
        new Promise<boolean>((resolve) => setTimeout(resolve, 5000, false)) // 5秒超时
      ]);
      if (!interruptCompleted) {
        logger.warn('界面.任务.中断.CLI.超时', undefined, LogCategory.UI);
      }
      await this.cliFactory.disconnectAll();
      await this.cliFactory.resetAllSessions();
      logger.info('界面.任务.中断.CLI.重置', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.任务.中断.CLI.错误', error, LogCategory.UI);
      try {
        await this.cliFactory.disconnectAll();
        await this.cliFactory.resetAllSessions();
      } catch (cleanupError) {
        logger.error('界面.任务.中断.CLI.清理_失败', cleanupError, LogCategory.UI);
      }
    }

    // 3. 更新任务状态
    if (runningTask) {
      await taskManager.cancelTask(runningTask.id);
    }

    // 清理编排者流式输出缓存，避免跨任务串流
    this.streamMessageIds.clear();


    if (hasRunningTask && !options?.silent) {
      // 4. 通知 UI
      this.postMessage({ type: 'toast', message: '任务已打断', toastType: 'info' });


      this.postMessage({
        type: 'taskInterrupted',
        message: '任务已打断'
      } as any);

      this.sendOrchestratorMessage({
        content: '任务已打断，可在变更中查看已修改的文件，或选择继续执行。',
        messageType: 'text',
        metadata: { phase: 'interrupted' },
      });
    }

    this.sendStateUpdate();
  }

  // 流式消息 ID 管理
  private streamMessageIds: Map<string, string> = new Map(); // key: `${source}-${cli}-${target}`, value: messageId

  /**
   * 发送编排器标准消息（非流式）
   * 用于发送进度更新、子任务摘要、错误等消息
   */
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
    } else if (messageType === 'error') {
      type = MessageType.ERROR;
      lifecycle = MessageLifecycle.FAILED;
    } else if (messageType === 'result') {
      type = MessageType.RESULT;
    }

    const standardMessage: StandardMessage = {
      id: `msg-orch-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      traceId: this.activeSessionId || 'default',
      type,
      source: 'orchestrator',
      cli: 'claude',
      timestamp: Date.now(),
      updatedAt: Date.now(),
      blocks: Array.isArray(blocks) ? blocks : (content ? [{ type: 'text', content, isMarkdown: false }] : []),
      lifecycle,
      metadata: {
        taskId,
        ...metadata,
      },
    };

    this.postMessage({
      type: 'standardMessage',
      message: standardMessage,
      sessionId: this.activeSessionId,
    } as any);
    this.logMessageFlow('standardMessage', standardMessage);
  }

  /** 实现 WebviewViewProvider 接口 */
  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken
  ): void {
    this._view = webviewView;

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [this.extensionUri],
    };

    // HTML 中已注入 initialSessionId，webview 加载时即有正确的 sessionId
    webviewView.webview.html = this.getHtmlContent(webviewView.webview);

    // 处理来自 Webview 的消息
    webviewView.webview.onDidReceiveMessage(
      (message: WebviewToExtensionMessage) => this.handleMessage(message),
      undefined,
      this.context.subscriptions
    );

    // 启动时检测所有 CLI 的可用性
    this.checkCliAvailability();
  }

  /** 检测所有 CLI 的可用性并更新状态 */
  private async checkCliAvailability(): Promise<void> {
    try {
      const availability = await this.cliFactory.checkAllAvailability();
      logger.info('界面.CLI.可用性', availability, LogCategory.UI);

      // 更新 CLI 状态
      const cliTypes: CLIType[] = ['claude', 'codex', 'gemini'];
      for (const cli of cliTypes) {
        const status: CLIStatus = {
          type: cli,
          code: availability[cli] ? CLIStatusCode.AVAILABLE : CLIStatusCode.NOT_INSTALLED,
          available: availability[cli],
          path: cli,
          lastChecked: new Date(),
        };
        this.cliStatuses.set(cli, status);
      }

      // 通知 UI 更新状态
      this.sendStateUpdate();

      // 发送单独的状态变更通知
      for (const cli of cliTypes) {
        this.postMessage({
          type: 'cliStatusChanged',
          cli,
          available: availability[cli],
        });
      }
    } catch (error) {
      logger.error('界面.CLI.可用性_失败', error, LogCategory.UI);
    }
  }

  /** 处理 Webview 消息 */
  private async handleMessage(message: WebviewToExtensionMessage): Promise<void> {
    logger.info('界面.Webview.消息.收到', { type: message.type }, LogCategory.UI);

    switch (message.type) {
      case 'getState':
        this.sendStateUpdate();
        break;

      case 'login':
        await this.handleLoginMessage(message);
        break;

      case 'logout':
        await this.handleLogoutMessage();
        break;

      case 'getStatus':
        await this.handleGetStatusMessage();
        break;

      case 'executeTask':
        logger.info('界面.任务.执行.请求', { promptLength: String((message as any).prompt || '').length, imageCount: (message as any).images?.length || 0 }, LogCategory.UI);
        const images = (message as any).images || [];
        await this.executeTask((message as any).prompt, undefined, images);
        break;

      case 'interruptTask':
       
        logger.info('界面.任务.中断.消息', { taskId: message.taskId }, LogCategory.UI);
        await this.interruptCurrentTask();
        break;

      case 'pauseTask':
       
        logger.info('界面.任务.暂停.消息', { taskId: (message as any).taskId }, LogCategory.UI);
        this.postMessage({ type: 'toast', message: '暂停功能开发中', toastType: 'info' });
        break;

      case 'resumeTask':
       
        logger.info('界面.任务.恢复.消息', { taskId: (message as any).taskId }, LogCategory.UI);
        await this.resumeInterruptedTask();
        break;

      case 'appendMessage':
       
        logger.info('界面.消息.补充.请求', undefined, LogCategory.UI);
        await this.handleAppendMessage((message as any).taskId, (message as any).content);
        break;

      case 'approveChange':
        // 批准单个变更
        this.snapshotManager.acceptChange(message.filePath);
        globalEventBus.emitEvent('change:approved', { data: { filePath: message.filePath } });
        this.postMessage({ type: 'toast', message: '变更已批准', toastType: 'success' });
        this.sendStateUpdate();
        break;

      case 'revertChange':
        this.snapshotManager.revertToSnapshot(message.filePath);
        this.postMessage({ type: 'toast', message: '变更已还原', toastType: 'info' });
        this.sendStateUpdate();
        break;

      case 'approveAllChanges':
        // 批准所有变更
        {
          const allChanges = this.snapshotManager.getPendingChanges();
          for (const change of allChanges) {
            this.snapshotManager.acceptChange(change.filePath);
          }
          this.postMessage({ type: 'toast', message: `已批准 ${allChanges.length} 个变更`, toastType: 'success' });
        }
        this.sendStateUpdate();
        break;

      case 'revertAllChanges':
        // 还原所有变更
        {
          const changes = this.snapshotManager.getPendingChanges();
          for (const change of changes) {
            this.snapshotManager.revertToSnapshot(change.filePath);
          }
          this.postMessage({ type: 'toast', message: `已还原 ${changes.length} 个变更`, toastType: 'info' });
        }
        this.sendStateUpdate();
        break;

      case 'viewDiff':
        // 在 VS Code 原生 diff 视图中查看变更（类似 Augment）
        await this.openVscodeDiff(message.filePath);
        break;

      case 'openFile':
        // 在编辑器中打开文件（从代码块点击文件路径）
        await this.openFileInEditor(message.filepath);
        break;

      case 'newSession':
        await this.handleNewSession();
        break;

      case 'saveCurrentSession':
        // 新增：保存当前会话的消息和 CLI 输出
        this.saveCurrentSessionData(message.messages, message.cliOutputs);
        break;

      case 'switchSession':

        if (this.activeSessionId !== message.sessionId) {
          await this.interruptCurrentTask({ silent: true });
        }
        // 切换会话
        await this.switchToSession(message.sessionId);
        const switchedSession = this.sessionManager.switchSession(message.sessionId);
        if (switchedSession) {

          this.activeSessionId = message.sessionId;
          // 恢复 CLI sessionIds
          this.postMessage({ type: 'sessionSwitched', sessionId: message.sessionId });
        }
        this.sendStateUpdate();
        break;

      case 'renameSession':
        // 重命名会话
        if (this.sessionManager.renameSession(message.sessionId, message.name)) {
          this.postMessage({ type: 'sessionsUpdated', sessions: this.sessionManager.getAllSessions() as any[] });
          this.postMessage({ type: 'toast', message: '会话已重命名', toastType: 'success' });
        }
        break;

      case 'closeSession':
        // 删除会话（统一管理器会清理所有相关资源）
        if (this.sessionManager.deleteSession(message.sessionId)) {
          // 如果删除后没有会话，创建一个新的
          if (this.sessionManager.getAllSessions().length === 0) {
            const newSession = this.sessionManager.createSession();
            this.activeSessionId = newSession.id;
            this.postMessage({ type: 'sessionCreated', session: newSession as any });
          }
          this.postMessage({ type: 'sessionsUpdated', sessions: this.sessionManager.getAllSessions() as any[] });
          this.postMessage({ type: 'toast', message: '会话已删除', toastType: 'info' });
        }
        this.sendStateUpdate();
        break;

      case 'selectCli':
        // 用户手动选择 CLI（null 表示自动选择）
        this.selectedCli = message.cli || null;
        logger.info('界面.CLI.选择.变更', { cli: this.selectedCli || 'auto' }, LogCategory.UI);
        break;

      case 'confirmPlan':
        // 用户确认执行计划（Hard Stop 响应）
        this.handlePlanConfirmation((message as any).confirmed);
        break;

      case 'answerQuestions':
        this.handleQuestionAnswer((message as any).answer ?? null);
        break;

      case 'answerClarification':
        // 用户回答澄清问题
        this.handleClarificationAnswer(
          (message as any).answers ?? null,
          (message as any).additionalInfo
        );
        break;

      case 'answerWorkerQuestion':
        // 用户回答 Worker 问题
        this.handleWorkerQuestionAnswer((message as any).answer ?? null);
        break;

      case 'answerCliQuestion':
        // 用户回答 CLI 询问
        this.handleCliQuestionAnswer(
          (message as any).cli,
          (message as any).questionId,
          (message as any).answer,
          (message as any).adapterRole
        );
        break;

      case 'updateSetting':
        // 更新设置
        this.handleSettingUpdate(message.key, message.value);
        break;

      case 'setInteractionMode':
        // 设置交互模式
        this.handleSetInteractionMode((message as any).mode);
        break;

      case 'confirmRecovery':
        // 用户确认恢复策略
        await this.handleRecoveryConfirmation((message as any).decision);
        break;

      case 'requestExecutionStats':
       
        this.sendExecutionStats();
        break;
      case 'resetExecutionStats':
        await this.handleResetExecutionStats();
        break;

      case 'checkCliStatus':

        this.sendCliStatus();
        break;

      case 'getProfileConfig':
        this.sendProfileConfig();
        break;

      case 'saveProfileConfig':
        await this.handleSaveProfileConfig(message.data);
        break;

      case 'resetProfileConfig':
        await this.handleResetProfileConfig();
        break;

      case 'clearAllTasks':
       
        this.handleClearAllTasks();
        break;

      case 'getPromptEnhanceConfig':
        // 获取 Prompt 增强配置（从系统级存储）
        this.sendPromptEnhanceConfig();
        break;

      case 'updatePromptEnhance':
        // 更新 Prompt 增强配置
        this.handleUpdatePromptEnhance((message as any).config);
        break;

      case 'testPromptEnhance':
        // 测试 Prompt 增强连接
        this.handleTestPromptEnhance((message as any).baseUrl, (message as any).apiKey);
        break;

      case 'enhancePrompt':
        // 执行 Prompt 增强（从系统级存储读取配置）
        this.handleEnhancePrompt((message as any).prompt);
        break;
    }
  }

  /** 处理登录消息 */
  private async handleLoginMessage(message: Extract<WebviewToExtensionMessage, { type: 'login' }>): Promise<void> {
    if (this.loginInFlight) {
      this.postMessage({ type: 'loginError', message: '登录处理中，请稍后重试' } as any);
      return;
    }

    const rawApiKey = message.apiKey;
    const apiKey = typeof rawApiKey === 'string' ? rawApiKey.trim() : '';
    if (!apiKey) {
      this.postMessage({ type: 'loginError', message: 'API Key 不能为空' } as any);
      return;
    }

    this.loginInFlight = true;
    try {
      await this.storeApiKey(apiKey);
      await this.context.globalState.update(this.authStatusKey, true);
      this.postMessage({ type: 'loginSuccess' } as any);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      try {
        await this.removeApiKey();
      } catch {
        // 忽略回滚失败，避免覆盖原始错误
      }
      this.postMessage({ type: 'loginError', message: `登录失败: ${errorMsg}` } as any);
    } finally {
      this.loginInFlight = false;
    }
  }

  /** 处理登出消息 */
  private async handleLogoutMessage(): Promise<void> {
    if (this.loginInFlight) {
      this.postMessage({ type: 'loginError', message: '登录处理中，无法登出' } as any);
      return;
    }

    try {
      await this.removeApiKey();
      await this.context.globalState.update(this.authStatusKey, false);
      this.postMessage({ type: 'authStatus', loggedIn: false } as any);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'loginError', message: `登出失败: ${errorMsg}` } as any);
    }
  }

  /** 处理状态查询消息 */
  private async handleGetStatusMessage(): Promise<void> {
    try {
      const loggedIn = await this.isLoggedIn();
      this.postMessage({ type: 'authStatus', loggedIn } as any);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'loginError', message: `获取状态失败: ${errorMsg}` } as any);
    }
  }

  /** 保存 API Key 到安全存储 */
  private async storeApiKey(apiKey: string): Promise<void> {
    await this.context.secrets.store(this.authSecretKey, apiKey);
  }

  /** 读取 API Key */
  private async getApiKey(): Promise<string | undefined> {
    return this.context.secrets.get(this.authSecretKey);
  }

  /** 删除 API Key */
  private async removeApiKey(): Promise<void> {
    await this.context.secrets.delete(this.authSecretKey);
  }

  /** 判断是否已登录 */
  private async isLoggedIn(): Promise<boolean> {
    const flag = this.context.globalState.get<boolean>(this.authStatusKey, false);
    if (!flag) {
      return false;
    }
    try {
      const apiKey = await this.getApiKey();
      if (!apiKey) {
        await this.context.globalState.update(this.authStatusKey, false);
        return false;
      }
      return true;
    } catch (error) {
      await this.context.globalState.update(this.authStatusKey, false);
      return false;
    }
  }

  /** 处理 Prompt 增强配置更新 - 存储到 ~/.multicli/config.json */
  private async handleUpdatePromptEnhance(config: { enabled: boolean; baseUrl: string; apiKey: string }): Promise<void> {
    try {
      const configPath = this.getMultiCliConfigPath();
      const configDir = path.dirname(configPath);

      // 确保目录存在
      if (!fs.existsSync(configDir)) {
        fs.mkdirSync(configDir, { recursive: true });
      }

      // 读取现有配置或创建新配置
      let existingConfig: any = {};
      if (fs.existsSync(configPath)) {
        try {
          existingConfig = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
        } catch {
          existingConfig = {};
        }
      }

      // 更新 promptEnhance 配置
      existingConfig.promptEnhance = {
        baseUrl: config.baseUrl,
        apiKey: config.apiKey
      };

      // 写入配置文件
      fs.writeFileSync(configPath, JSON.stringify(existingConfig, null, 2), 'utf-8');
      logger.info('界面.提示词_增强.配置.已保存', { path: configPath }, LogCategory.UI);
    } catch (error) {
      logger.error('界面.提示词_增强.配置.保存_失败', error, LogCategory.UI);
    }
  }

  /** 获取 MultiCLI 配置文件路径 */
  private getMultiCliConfigPath(): string {
    return path.join(os.homedir(), '.multicli', 'config.json');
  }

  /** 获取 Prompt 增强配置 - 从 ~/.multicli/config.json 读取 */
  private async getPromptEnhanceConfig(): Promise<{ baseUrl: string; apiKey: string }> {
    try {
      const configPath = this.getMultiCliConfigPath();
      if (fs.existsSync(configPath)) {
        const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
        if (config.promptEnhance) {
          return {
            baseUrl: config.promptEnhance.baseUrl || '',
            apiKey: config.promptEnhance.apiKey || ''
          };
        }
      }
    } catch (error) {
      logger.error('界面.提示词_增强.配置.读取_失败', error, LogCategory.UI);
    }
    return { baseUrl: '', apiKey: '' };
  }

  /** 发送 Prompt 增强配置到前端 */
  private async sendPromptEnhanceConfig(): Promise<void> {
    const config = await this.getPromptEnhanceConfig();
    this.postMessage({
      type: 'promptEnhanceConfig',
      baseUrl: config.baseUrl,
      apiKey: config.apiKey
    } as any);
  }

  /** 测试 Augment API 连接 */
  private async handleTestPromptEnhance(baseUrl: string, apiKey: string): Promise<void> {
    if (!baseUrl || !apiKey) {
      this.postMessage({ type: 'promptEnhanceResult', success: false, message: '请填写 API 地址和密钥' } as any);
      return;
    }

    try {
      // 使用 prompt-enhancer 端点测试连接（发送一个简单的测试请求）
      const testUrl = baseUrl.replace(/\/$/, '') + '/prompt-enhancer';
      const response = await fetch(testUrl, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${apiKey}`,
          'Content-Type': 'application/json'
        },
        body: JSON.stringify({
          nodes: [{ id: 1, type: 0, text_node: { content: 'test' } }],
          chat_history: [],
          blobs: { checkpoint_id: null, added_blobs: [], deleted_blobs: [] },
          conversation_id: null,
          model: 'claude-sonnet-4-5',
          mode: 'CHAT',
          user_guided_blobs: [],
          external_source_ids: [],
          user_guidelines: '',
          workspace_guidelines: '',
          rules: []
        }),
        signal: AbortSignal.timeout(15000)
      });

      if (response.ok) {
        this.postMessage({ type: 'promptEnhanceResult', success: true, message: '连接成功' } as any);
      } else if (response.status === 401) {
        this.postMessage({ type: 'promptEnhanceResult', success: false, message: 'Token 无效或已过期' } as any);
      } else if (response.status === 403) {
        this.postMessage({ type: 'promptEnhanceResult', success: false, message: '访问被拒绝' } as any);
      } else {
        const errorText = await response.text().catch(() => '');
        this.postMessage({ type: 'promptEnhanceResult', success: false, message: `连接失败: ${response.status}` } as any);
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'promptEnhanceResult', success: false, message: `连接错误: ${errorMsg}` } as any);
    }
  }

  /** 执行 Prompt 增强 - 使用 Augment prompt-enhancer API + ACE 索引 */
  private async handleEnhancePrompt(prompt: string): Promise<void> {
    // 从配置读取 Augment API 配置
    const { baseUrl, apiKey } = await this.getPromptEnhanceConfig();

    if (!baseUrl || !apiKey) {
      this.postMessage({ type: 'promptEnhanced', enhancedPrompt: '', error: '请先在设置中配置 Augment API' } as any);
      return;
    }

    // 获取项目根目录
    const workspaceFolders = vscode.workspace.workspaceFolders;
    const projectRoot = workspaceFolders?.[0]?.uri.fsPath || '';

    // 1. 自动索引项目并获取 blobs
    let blobNames: string[] = [];
    if (projectRoot) {
      try {
        logger.info('界面.ACE.索引.开始', { projectRoot }, LogCategory.UI);
        const aceManager = new AceIndexManager(projectRoot, baseUrl, apiKey);
        const indexResult = await aceManager.indexProject();
        if (indexResult.status !== 'error') {
          blobNames = aceManager.loadIndex();
          logger.info('界面.ACE.索引.完成', { blobCount: blobNames.length }, LogCategory.UI);
        }
      } catch (error) {
        logger.error('界面.ACE.索引.失败', error, LogCategory.UI);
        // 索引失败不阻止增强，继续使用空 blobs
      }
    }

    // 2. 收集上下文（5-10 轮对话）
    const conversationHistory = this.sessionManager.formatConversationHistory(10);

    // 3. 检测语言
    const isChinese = /[\u4e00-\u9fa5]/.test(prompt);
    const languageGuideline = isChinese ? 'Please respond in Chinese (Simplified Chinese). 请用中文回复。' : '';

    // 4. 解析对话历史为 chat_history 格式
    const chatHistory = this.parseChatHistory(conversationHistory);

    // 5. 构造符合 Augment prompt-enhancer 格式的 payload（包含 blobs）
    const payload = {
      nodes: [{ id: 1, type: 0, text_node: { content: prompt } }],
      chat_history: chatHistory,
      blobs: {
        checkpoint_id: null,
        added_blobs: blobNames,  // 传入 ACE 索引的 blobs
        deleted_blobs: [],
      },
      conversation_id: null,
      model: 'claude-sonnet-4-5',
      mode: 'CHAT',
      user_guided_blobs: [],
      external_source_ids: [],
      user_guidelines: languageGuideline,
      workspace_guidelines: '',
      rules: []
    };

    try {
      const apiUrl = baseUrl.replace(/\/$/, '') + '/prompt-enhancer';
      const response = await fetch(apiUrl, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${apiKey}`,
          'Content-Type': 'application/json'
        },
        body: JSON.stringify(payload),
        signal: AbortSignal.timeout(60000)
      });

      if (!response.ok) {
        if (response.status === 401) {
          this.postMessage({ type: 'promptEnhanced', enhancedPrompt: '', error: 'Token 已失效或无效' } as any);
        } else if (response.status === 403) {
          this.postMessage({ type: 'promptEnhanced', enhancedPrompt: '', error: '访问被拒绝' } as any);
        } else {
          this.postMessage({ type: 'promptEnhanced', enhancedPrompt: '', error: `API 错误: ${response.status}` } as any);
        }
        return;
      }

      const data = await response.json() as { text?: string };
      let enhancedPrompt = data.text?.trim() || '';

      // 移除 Augment 特定的工具引用
      if (enhancedPrompt) {
        enhancedPrompt = this.cleanEnhancedPrompt(enhancedPrompt);
        this.postMessage({ type: 'promptEnhanced', enhancedPrompt, error: '' } as any);
      } else {
        this.postMessage({ type: 'promptEnhanced', enhancedPrompt: '', error: '未获取到增强结果' } as any);
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'promptEnhanced', enhancedPrompt: '', error: errorMsg } as any);
    }
  }

  /** 清理增强后的 Prompt，移除 Augment 特定的工具引用 */
  private cleanEnhancedPrompt(text: string): string {
    let result = text;

    // 移除 codebase-retrieval 工具引用（子代理本身有代码搜索能力）
    // 匹配各种格式：`codebase-retrieval`、"codebase-retrieval"、codebase-retrieval 工具等
    result = result.replace(/使用\s*`?codebase-retrieval`?\s*工具[^。\n]*/g, '');
    result = result.replace(/通过\s*`?codebase-retrieval`?\s*[^。\n]*/g, '');
    result = result.replace(/调用\s*`?codebase-retrieval`?\s*[^。\n]*/g, '');
    result = result.replace(/`codebase-retrieval`/g, '代码搜索');
    result = result.replace(/codebase-retrieval/g, '代码搜索');
    result = result.replace(/codebase_retrieval/g, '代码搜索');

    // 清理多余的空行
    result = result.replace(/\n{3,}/g, '\n\n');

    return result.trim();
  }

  /** 解析对话历史为 chat_history 格式 */
  private parseChatHistory(conversationHistory: string): Array<{role: string, content: string}> {
    if (!conversationHistory) return [];

    const lines = conversationHistory.split('\n\n');
    const chatHistory: Array<{role: string, content: string}> = [];

    for (const line of lines) {
      const trimmed = line.trim();
      if (trimmed.startsWith('User:') || trimmed.startsWith('用户:')) {
        chatHistory.push({
          role: 'user',
          content: trimmed.replace(/^(User:|用户:)\s*/, '')
        });
      } else if (trimmed.startsWith('Assistant:') || trimmed.startsWith('AI:') || trimmed.startsWith('助手:')) {
        chatHistory.push({
          role: 'assistant',
          content: trimmed.replace(/^(Assistant:|AI:|助手:)\s*/, '')
        });
      }
    }

    return chatHistory;
  }

  /** 发送 CLI 连接状态到前端 */
  private async sendCliStatus(): Promise<void> {
    try {
      const availability = await this.cliFactory.checkAllAvailability();
      const statuses: Record<string, { status: string; version?: string }> = {};

      for (const cli of ['claude', 'codex', 'gemini'] as CLIType[]) {
        const isAvailable = availability[cli];
        statuses[cli] = {
          status: isAvailable ? 'available' : 'not_installed',
          version: isAvailable ? '已安装' : undefined
        };
      }

      this.postMessage({
        type: 'cliStatusUpdate',
        statuses
      } as ExtensionToWebviewMessage);
    } catch (error) {
      logger.error('界面.CLI.状态.检查_失败', error, LogCategory.UI);
    }
  }

  /** 发送执行统计数据到前端 */
  private sendExecutionStats(): void {
    const executionStats = this.intelligentOrchestrator.getExecutionStats();
    if (!executionStats) {
      logger.info('界面.执行统计.未初始化', undefined, LogCategory.UI);
      return;
    }

    const stats = (['claude', 'codex', 'gemini'] as CLIType[]).map(cli => {
      const cliStats = executionStats.getStats(cli);
      return {
        cli,
        totalExecutions: cliStats.totalExecutions,
        successCount: cliStats.successCount,
        failureCount: cliStats.failureCount,
        successRate: cliStats.successRate,
        avgDuration: cliStats.avgDuration,
        isHealthy: cliStats.isHealthy,
        healthScore: cliStats.healthScore,
        lastError: cliStats.lastError,
        lastExecutionTime: cliStats.lastExecutionTime,
        totalInputTokens: cliStats.totalInputTokens,
        totalOutputTokens: cliStats.totalOutputTokens,
      };
    });

   
    const orchestratorTokens = this.intelligentOrchestrator.getOrchestratorTokenUsage();
    const orchestratorStats = {
      totalTasks: stats.reduce((sum, s) => sum + s.totalExecutions, 0),
      totalSuccess: stats.reduce((sum, s) => sum + s.successCount, 0),
      totalFailed: stats.reduce((sum, s) => sum + s.failureCount, 0),
      totalInputTokens: stats.reduce((sum, s) => sum + s.totalInputTokens, 0) + (orchestratorTokens?.inputTokens || 0),
      totalOutputTokens: stats.reduce((sum, s) => sum + s.totalOutputTokens, 0) + (orchestratorTokens?.outputTokens || 0),
    };

    this.postMessage({
      type: 'executionStatsUpdate',
      stats,
      orchestratorStats,
    } as ExtensionToWebviewMessage);
  }

  private async handleResetExecutionStats(): Promise<void> {
    const executionStats = this.intelligentOrchestrator.getExecutionStats();
    if (!executionStats) {
      return;
    }
    await executionStats.clearStats();
    this.intelligentOrchestrator.resetOrchestratorTokenUsage();
    this.sendExecutionStats();
    this.postMessage({ type: 'toast', message: '执行统计已重置', toastType: 'info' });
  }

  // ============================================================================
  // 画像配置处理
  // ============================================================================

  /** 发送画像配置到 UI */
  private sendProfileConfig(): void {
    const storage = new ProfileStorage();
    const storedConfig = storage.getConfig();

    // 默认配置 - 与系统画像定义完全一致
    const defaultWorkers: Record<string, any> = {
      claude: {
        role: '你是一个资深软件架构师，专注于系统设计、代码质量和可维护性。\n你的代码应该是清晰、可扩展、易于测试的。',
        focus: ['优先考虑代码的可维护性和扩展性', '在修改前先分析影响范围和依赖关系', '对于跨模块修改，先确认接口契约', '保持代码风格一致性', '添加必要的类型定义和注释'],
        constraints: ['不要进行不必要的重构', '避免引入新的依赖，除非必要', '大规模修改前先与编排者确认'],
      },
      codex: {
        role: '你是一个高效的代码执行者，专注于快速、准确地完成具体任务。\n你的目标是用最少的代码变更解决问题。',
        focus: ['精准定位问题，最小化修改范围', '快速实现，不过度设计', '确保修改不引入新问题', '添加必要的错误处理'],
        constraints: ['不要进行架构级别的修改', '保持修改范围在任务描述内', '遇到需要架构决策的问题，反馈给编排者'],
      },
      gemini: {
        role: '你是一个前端专家和文档专家，专注于用户界面和开发者体验。\n你的代码应该是美观、易用、可访问的。',
        focus: ['关注用户体验和交互细节', '保持 UI 一致性和美观性', '确保响应式设计和可访问性', '编写清晰的文档和注释'],
        constraints: ['不要修改后端 API 逻辑', '遵循已定义的接口契约', '样式修改保持设计系统一致性'],
      },
    };

    const defaultCategories: Record<string, string> = {
      architecture: 'claude',
      bugfix: 'codex',
      frontend: 'gemini',
      implement: 'claude',
      refactor: 'claude',
      test: 'codex',
      document: 'claude',
      general: 'claude',
    };

    // 构建 UI 需要的格式，合并存储配置和默认配置
    const uiConfig: any = {
      workers: { ...defaultWorkers },
      categories: { ...defaultCategories },
      configPath: ProfileStorage.getConfigDir(),
    };

    // 覆盖存储的 Worker 配置
    if (storedConfig?.workers) {
      for (const [workerType, workerConfig] of Object.entries(storedConfig.workers)) {
        if (workerConfig) {
          uiConfig.workers[workerType] = {
            role: workerConfig.guidance?.role || defaultWorkers[workerType]?.role || '',
            focus: workerConfig.guidance?.focus || defaultWorkers[workerType]?.focus || [],
            constraints: workerConfig.guidance?.constraints || defaultWorkers[workerType]?.constraints || [],
          };
        }
      }
    }

    // 覆盖存储的分类配置
    if (storedConfig?.categories?.categories) {
      for (const [category, categoryConfig] of Object.entries(storedConfig.categories.categories)) {
        if (categoryConfig?.defaultWorker) {
          uiConfig.categories[category] = categoryConfig.defaultWorker;
        }
      }
    }

    this.postMessage({ type: 'profileConfig', config: uiConfig } as any);
  }

  /** 保存画像配置 */
  private async handleSaveProfileConfig(data: { workers: Record<string, any>; categories: Record<string, string> }): Promise<void> {
    try {
      const storage = new ProfileStorage();

      // 转换 UI 格式到存储格式
      const config: StoredProfileConfig = {
        workers: {},
        categories: {
          categories: {},
          rules: {
            categoryPriority: ['architecture', 'integration', 'bugfix', 'backend', 'frontend', 'test', 'docs', 'simple'],
            defaultCategory: 'simple',
            riskMapping: { high: 'fullPath', medium: 'standardPath', low: 'lightPath' },
          },
        },
      };

      // 转换 Worker 配置
      for (const [workerType, workerData] of Object.entries(data.workers)) {
        if (workerData) {
          config.workers[workerType as 'claude' | 'codex' | 'gemini'] = {
            guidance: {
              role: workerData.role || '',
              focus: workerData.focus || [],
              constraints: workerData.constraints || [],
              outputPreferences: [],
            },
            profile: {
              strengths: [],
              weaknesses: [],
            },
          };
        }
      }

      // 转换分类配置
      // 注意：keywords 是系统内置配置，不保存到用户配置文件中
      for (const [category, worker] of Object.entries(data.categories)) {
        if (config.categories?.categories) {
          (config.categories.categories as any)[category] = {
            defaultWorker: worker,
            // keywords 不保存，由系统内置提供
            priority: 'medium',
            riskLevel: 'medium',
          };
        }
      }

      await storage.saveConfig(config);
      this.postMessage({ type: 'toast', message: '画像配置已保存', toastType: 'success' });

      try {
        await this.intelligentOrchestrator.reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        this.postMessage({ type: 'toast', message: `画像重载失败: ${reloadMsg}`, toastType: 'warning' });
      }
      this.sendProfileConfig();
      logger.info('界面.画像.配置.已保存', { path: ProfileStorage.getConfigDir() }, LogCategory.UI);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'toast', message: `保存失败: ${errorMsg}`, toastType: 'error' });
    }
  }

  /** 重置画像配置 */
  private async handleResetProfileConfig(): Promise<void> {
    try {
      const storage = new ProfileStorage();
      await storage.clearConfig();
      try {
        await this.intelligentOrchestrator.reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        this.postMessage({ type: 'toast', message: `画像重载失败: ${reloadMsg}`, toastType: 'warning' });
      }
      this.postMessage({ type: 'toast', message: '画像配置已重置为默认值', toastType: 'success' });
      this.sendProfileConfig();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'toast', message: `重置失败: ${errorMsg}`, toastType: 'error' });
    }
  }

  /** 处理设置交互模式 */
  private handleSetInteractionMode(mode: import('../types').InteractionMode): void {
    logger.info('界面.交互_模式.变更', { mode }, LogCategory.UI);
    this.intelligentOrchestrator.setInteractionMode(mode);
    this.postMessage({ type: 'interactionModeChanged', mode });
    this.postMessage({
      type: 'toast',
      message: `已切换到 ${this.getModeDisplayName(mode)} 模式`,
      toastType: 'info'
    });
    this.sendStateUpdate();
  }

  /** 获取模式显示名称 */
  private getModeDisplayName(mode: import('../types').InteractionMode): string {
    switch (mode) {
      case 'ask': return '对话';
      case 'agent': return '代理';
      case 'auto': return '自动';
      default: return mode;
    }
  }

  /** 恢复确认回调的 Promise resolver */
  private recoveryConfirmationResolver: ((decision: 'retry' | 'rollback' | 'continue') => void) | null = null;

  /** 处理恢复确认 */
  private async handleRecoveryConfirmation(decision: 'retry' | 'rollback' | 'continue'): Promise<void> {
    logger.info('界面.编排器.恢复.决策', { decision }, LogCategory.UI);
    if (this.recoveryConfirmationResolver) {
      this.recoveryConfirmationResolver(decision);
      this.recoveryConfirmationResolver = null;
      return;
    }

    if (decision === 'rollback') {
      const count = this.snapshotManager.revertAllChanges();
      const message = count > 0 ? `已回滚 ${count} 个变更` : '没有可回滚的变更';
      this.postMessage({ type: 'toast', message, toastType: 'info' });
      this.sendOrchestratorMessage({
        content: `回滚完成：${message}`,
        messageType: 'result',
        metadata: { phase: 'recovery' },
      });
      return;
    }

    if (decision === 'retry') {
      await this.resumeInterruptedTask('请继续完成之前失败的任务');
      return;
    }

    this.postMessage({ type: 'toast', message: '已选择继续执行，未进行回滚', toastType: 'info' });
  }

  /** 在 VS Code 原生 diff 视图中打开文件变更（类似 Augment） */
  private async openVscodeDiff(filePath: string): Promise<void> {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      this.postMessage({ type: 'toast', message: '没有活动会话', toastType: 'warning' });
      return;
    }

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) {
      this.postMessage({ type: 'toast', message: '未找到该文件的快照', toastType: 'warning' });
      return;
    }

    try {
      // 获取原始内容（从快照文件读取）
      const snapshotFile = this.sessionManager.getSnapshotFilePath(session.id, snapshot.id);
      let originalContent = '';

      if (fs.existsSync(snapshotFile)) {
        originalContent = fs.readFileSync(snapshotFile, 'utf-8');
      }

      // 创建临时文件存储原始内容（用于 diff 左侧）
      const tempDir = path.join(os.tmpdir(), 'multicli-diff');
      if (!fs.existsSync(tempDir)) {
        fs.mkdirSync(tempDir, { recursive: true });
      }

      const fileName = path.basename(relativePath);
      const tempFile = path.join(tempDir, `original-${Date.now()}-${fileName}`);
      fs.writeFileSync(tempFile, originalContent, 'utf-8');

      // 创建 URI
      const originalUri = vscode.Uri.file(tempFile);
      const modifiedUri = vscode.Uri.file(path.join(this.workspaceRoot, relativePath));

      // 使用 VS Code 原生 diff 命令打开
      const title = `${fileName} (原始 ↔ 修改后)`;
      await vscode.commands.executeCommand('vscode.diff', originalUri, modifiedUri, title);

      // 清理临时文件（延迟删除，确保 diff 视图已加载）
      setTimeout(() => {
        try {
          if (fs.existsSync(tempFile)) {
            fs.unlinkSync(tempFile);
          }
        } catch (e) {
          // 忽略清理错误
        }
      }, 5000);

    } catch (error) {
      logger.error('界面.差异.打开_失败', error, LogCategory.UI);
      this.postMessage({ type: 'toast', message: '打开 diff 视图失败', toastType: 'error' });
    }
  }

  /** 在编辑器中打开文件（从代码块点击文件路径） */
  private async openFileInEditor(filepath: string): Promise<void> {
    if (!filepath) {
      return;
    }

    try {
      // 处理相对路径和绝对路径
      const absolutePath = path.isAbsolute(filepath)
        ? filepath
        : path.join(this.workspaceRoot, filepath);

      // 检查文件是否存在
      if (!fs.existsSync(absolutePath)) {
        this.postMessage({ type: 'toast', message: `文件不存在: ${filepath}`, toastType: 'warning' });
        return;
      }

      // 打开文件
      const uri = vscode.Uri.file(absolutePath);
      const document = await vscode.workspace.openTextDocument(uri);
      await vscode.window.showTextDocument(document, {
        preview: false,
        preserveFocus: false
      });
    } catch (error) {
      logger.error('界面.文件.打开_失败', error, LogCategory.UI);
      this.postMessage({ type: 'toast', message: `打开文件失败: ${filepath}`, toastType: 'error' });
    }
  }

  /** 清理所有任务 */
  private handleClearAllTasks(): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      this.postMessage({ type: 'toast', message: '没有活动会话', toastType: 'warning' });
      return;
    }

    // 检查是否有正在运行的任务
    const runningTask = session.tasks.find(t => t.status === 'running');
    if (runningTask) {
      this.postMessage({ type: 'toast', message: '有任务正在执行，无法清理', toastType: 'warning' });
      return;
    }

    // 清空任务列表
    const taskCount = session.tasks.length;
    session.tasks = [];
    this.sessionManager.saveCurrentSession();

    this.postMessage({ type: 'toast', message: `已清理 ${taskCount} 个任务`, toastType: 'success' });
    this.sendStateUpdate();
  }

  /** 获取最近被打断的任务 */
  private async getLastInterruptedTask(): Promise<{ id: string; prompt: string } | null> {
    const taskManager = await this.getTaskManager();
    const tasks = await taskManager.getAllTasks();
    const interrupted = [...tasks].reverse().find(t => t.status === 'cancelled');
    if (!interrupted) return null;
    return { id: interrupted.id, prompt: interrupted.prompt };
  }

  /** 构建恢复提示词 */
  private buildResumePrompt(originalPrompt: string, extraInstruction?: string): string {
    const pendingChanges = this.snapshotManager.getPendingChanges();
    const changeList = pendingChanges.length
      ? pendingChanges.map(c => `- ${c.filePath} (+${c.additions}/-${c.deletions})`).join('\n')
      : '无';

    const extra = extraInstruction ? `\n\n补充指令:\n${extraInstruction}` : '';

    return [
      '请继续完成上一次被打断的任务。',
      `原始需求:\n${originalPrompt}`,
      `已产生的变更:\n${changeList}` + extra,
    ].join('\n\n');
  }

  /** 恢复被打断的任务 */
  private async resumeInterruptedTask(extraInstruction?: string): Promise<void> {
    if (this.intelligentOrchestrator.running) {
      this.postMessage({ type: 'toast', message: '当前仍有任务在执行', toastType: 'warning' });
      return;
    }

    const lastTask = await this.getLastInterruptedTask();
    if (!lastTask) {
      this.postMessage({ type: 'toast', message: '没有可恢复的任务', toastType: 'info' });
      return;
    }

    const prompt = this.buildResumePrompt(lastTask.prompt, extraInstruction);
    this.sendOrchestratorMessage({
      content: '正在恢复上一次任务...',
      messageType: 'progress',
      metadata: { phase: 'resuming' },
    });
    await this.executeTask(prompt, undefined, []);
  }

  /** 处理补充内容消息 */
  private async handleAppendMessage(taskId: string, content: string): Promise<void> {
    logger.info('界面.消息.补充.请求', { taskId, preview: content.substring(0, 50) }, LogCategory.UI);

    // 检查是否有正在运行的任务
    if (!this.intelligentOrchestrator.running) {
      this.postMessage({ type: 'toast', message: '没有正在执行的任务', toastType: 'warning' });
      return;
    }

    // 目前的实现：将补充内容作为新消息发送到当前 CLI
    // 未来可以扩展为真正的追加到当前执行上下文
    try {
      // 添加用户消息到对话
      this.postMessage({
        type: 'toast',
        message: '补充内容已发送',
        toastType: 'info'
      });

      // 发送到当前活跃的 CLI
      // 注意：这是一个简化实现，真正的追加需要 CLI 支持
      logger.info('界面.消息.补充.降级', { reason: 'simplified_implementation' }, LogCategory.UI);
    } catch (error) {
      logger.error('界面.消息.补充.失败', error, LogCategory.UI);
      this.postMessage({ type: 'toast', message: '补充内容失败', toastType: 'error' });
    }
  }

  /** 处理设置更新 */
  private handleSettingUpdate(key: string, value: unknown): void {
    const config = vscode.workspace.getConfiguration('multiCli');

    // 处理其他配置
    if (key === 'autoSnapshot') {
      config.update('autoSnapshot', value, vscode.ConfigurationTarget.Global);
    }
    else if (key === 'timeout') {
      config.update('timeout', parseInt(value as string, 10), vscode.ConfigurationTarget.Global);
    }

    this.postMessage({ type: 'toast', message: '设置已保存', toastType: 'success' });
  }

  /** 执行任务 */
  private async executeTask(prompt: string, forceCli?: CLIType, images?: Array<{dataUrl: string}>): Promise<void> {
    logger.info('界面.任务.执行.开始', { promptLength: prompt.length, imageCount: images?.length || 0, forceCli: forceCli || undefined }, LogCategory.UI);


    if (!this.activeSessionId) {
      const currentSession = this.sessionManager.getCurrentSession();
      this.activeSessionId = currentSession?.id || null;
      logger.info('界面.会话.当前.设置', { sessionId: this.activeSessionId }, LogCategory.UI);
    }

    // 如果有图片，保存到临时文件
    const imagePaths: string[] = [];
    if (images && images.length > 0) {
      const tmpDir = path.join(os.tmpdir(), 'multicli-images');
      if (!fs.existsSync(tmpDir)) {
        fs.mkdirSync(tmpDir, { recursive: true });
      }
      for (let i = 0; i < images.length; i++) {
        const img = images[i];
        const matches = img.dataUrl.match(/^data:image\/(\w+);base64,(.+)$/);
        if (matches) {
          const ext = matches[1] === 'jpeg' ? 'jpg' : matches[1];
          const base64Data = matches[2];
          const filePath = path.join(tmpDir, `image_${Date.now()}_${i}.${ext}`);
          fs.writeFileSync(filePath, Buffer.from(base64Data, 'base64'));
          imagePaths.push(filePath);
          logger.info('界面.图片.已保存', { path: filePath }, LogCategory.UI);
        }
      }
    }

    // 判断执行模式：智能编排 vs 直接执行
    const useIntelligentMode = !forceCli && !this.selectedCli;


    this.sessionManager.addMessage('user', prompt);
    this.sendStateUpdate();

    const orchestrationCommand = this.parseOrchestrationCommand(prompt);
    if (orchestrationCommand?.type === 'plan') {
      await this.executePlanOnly(orchestrationCommand.payload, imagePaths);
      return;
    }
    if (orchestrationCommand?.type === 'start-work') {
      await this.executeStartWork(orchestrationCommand.payload);
      return;
    }

    if (useIntelligentMode) {
      // 智能编排模式：Claude 分析 → 分配 CLI → 执行 → 总结
      await this.executeWithIntelligentOrchestrator(prompt, imagePaths);
    } else {
      // 直接执行模式：指定 CLI 直接执行
      await this.executeWithDirectCli(prompt, forceCli || this.selectedCli!, imagePaths);
    }
  }

  private parseOrchestrationCommand(prompt: string): { type: 'plan' | 'start-work'; payload?: string } | null {
    const trimmed = prompt.trim();
    if (!trimmed) return null;
    if (trimmed.startsWith('/plan') || trimmed.startsWith('@plan')) {
      const payload = trimmed.replace(/^[@/]+plan\b/i, '').trim();
      return { type: 'plan', payload };
    }
    if (trimmed.startsWith('/start-work') || trimmed.startsWith('/start')) {
      const payload = trimmed.replace(/^\/start-work\b/i, '').replace(/^\/start\b/i, '').trim();
      return { type: 'start-work', payload };
    }
    return null;
  }

  private async executePlanOnly(payload: string | undefined, imagePaths: string[]): Promise<void> {
    const planPrompt = (payload || '').trim();
    if (!planPrompt) {
      this.postMessage({ type: 'toast', message: '请输入计划内容，例如：/plan 实现登录功能', toastType: 'warning' });
      return;
    }
    const taskManager = await this.getTaskManager();
    const task = await taskManager.createTask({ prompt: planPrompt });
    try {
      const record = await this.intelligentOrchestrator.createPlan(
        planPrompt,
        task.id,
        this.activeSessionId || task.id
      );
      this.saveMessageToSession(planPrompt, record.formattedPlan, undefined, 'orchestrator');
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendOrchestratorMessage({
        content: errorMsg,
        messageType: 'error',
      });
    } finally {
      this.sendStateUpdate();
    }
  }

  private async executeStartWork(planId?: string): Promise<void> {
    const sessionId = this.activeSessionId;
    const record = planId && sessionId
      ? this.intelligentOrchestrator.getPlanById(planId, sessionId)
      : (sessionId ? this.intelligentOrchestrator.getActivePlanForSession(sessionId) : null);

    if (!record) {
      this.postMessage({ type: 'toast', message: '未找到可执行的计划，请先使用 /plan 生成计划', toastType: 'warning' });
      return;
    }

    const taskManager = await this.getTaskManager();
    let task = await taskManager.getTask(record.taskId);
    if (!task) {
      task = await taskManager.createTask({ prompt: record.prompt });
      await taskManager.updateTaskPlan(task.id, { planId: record.id, planSummary: record.plan.summary || record.plan.analysis || '执行计划' });
    }

    try {
      const result = await this.intelligentOrchestrator.executePlan(
        record,
        task.id,
        sessionId || record.sessionId
      );
      this.saveMessageToSession('/start-work', result, undefined, 'orchestrator');
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendOrchestratorMessage({
        content: errorMsg,
        messageType: 'error',
      });
    } finally {
      this.sendStateUpdate();
    }
  }

  private formatDuration(durationMs?: number): string {
    if (!durationMs || durationMs < 0) return '未知';
    const totalSeconds = Math.max(1, Math.round(durationMs / 1000));
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    if (minutes <= 0) return `${seconds}s`;
    return `${minutes}m ${seconds}s`;
  }

  private buildSubTaskChangeList(subTaskId: string, modifiedFiles?: string[]): string[] {
    if (subTaskId) {
      const changes = this.snapshotManager.getPendingChanges().filter(c => c.subTaskId === subTaskId);
      if (changes.length > 0) {
        return changes.map(change => `${change.filePath} (+${change.additions}, -${change.deletions})`);
      }
    }
    if (modifiedFiles && modifiedFiles.length > 0) {
      return modifiedFiles;
    }
    return [];
  }

  private buildVerificationReminderList(): string[] {
    return [
      '运行相关测试/构建，确认无报错',
      '关键流程手动验证（尤其是 UI/交互路径）',
      '确认变更文件已进入快照列表',
    ];
  }

  private buildSubTaskSummaryCard(data: { description?: string; cli?: string; duration?: number; modifiedFiles?: string[]; subTaskId?: string; error?: string }, status: 'completed' | 'failed') {
    const title = status === 'completed' ? '子任务完成' : '子任务失败';
    const description = data.description || data.subTaskId || '未知子任务';
    const executor = data.cli || 'unknown';
    const duration = this.formatDuration(data.duration);
    const changes = this.buildSubTaskChangeList(data.subTaskId || '', data.modifiedFiles);
    const verification = this.buildVerificationReminderList();
    return {
      title,
      status,
      description,
      executor,
      duration,
      changes,
      verification,
      error: status === 'failed' ? (data.error || '未知错误') : undefined,
    };
  }

  /** 智能编排模式执行 */
  private async executeWithIntelligentOrchestrator(prompt: string, imagePaths: string[]): Promise<void> {
    logger.info('界面.执行.模式.智能', undefined, LogCategory.UI);

    const interactionMode = this.intelligentOrchestrator.getInteractionMode();

    // ask 模式不创建任务（简单对话不需要任务跟踪）
    const isAskMode = interactionMode === 'ask';

   
    // 如果最终不需要 Worker，任务会被标记为已完成（无子任务）
    const taskManager = isAskMode ? null : await this.getTaskManager();
    const task = taskManager ? await taskManager.createTask({ prompt }) : null;
    const taskId = task?.id || `temp-${Date.now()}`;

    if (task) {
      await taskManager!.startTask(task.id);
      await this.sendStateUpdate();
    }

    try {
      // 调用智能编排器
      const result = await this.intelligentOrchestrator.execute(
        prompt,
        taskId,
        this.activeSessionId || taskId
      );

      // 获取执行计划，判断是否需要 Worker
      const plan = this.intelligentOrchestrator.plan;
      const needsWorker = plan?.needsWorker !== false && (plan?.subTasks?.length ?? 0) > 0;

      if (task) {
        if (needsWorker) {
         
          await taskManager!.completeTask(task.id);
          logger.info('界面.任务.完成', { taskId: task.id, subTaskCount: plan?.subTasks?.length || 0 }, LogCategory.UI);
        } else {
          // 不需要 Worker，标记任务为已完成（无子任务）
          await taskManager!.completeTask(task.id);
          logger.info('界面.任务.完成.无_子任务', { taskId: task.id }, LogCategory.UI);
        }
      }

      // 保存消息历史
      this.saveMessageToSession(prompt, result, undefined, 'orchestrator');

    } catch (error) {
      logger.error('界面.执行.智能.失败', error, LogCategory.UI);
      const errorMsg = error instanceof Error ? error.message : String(error);


      if (task) {
        await taskManager!.failTask(task.id, errorMsg);
      }

      this.sendOrchestratorMessage({
        content: errorMsg,
        messageType: 'error',
      });
    }

    this.sendStateUpdate();
  }

  /** 直接 CLI 执行模式 */
  private async executeWithDirectCli(prompt: string, targetCli: CLIType, imagePaths: string[]): Promise<void> {
    logger.info('界面.执行.模式.直接', { cli: targetCli }, LogCategory.UI);

    const startTime = Date.now();
    const taskManager = await this.getTaskManager();
    const task = await taskManager.createTask({ prompt });
    await taskManager.startTask(task.id);
    this.sendStateUpdate();

    // 清空目标 CLI 的输出
    this.cliOutputs.set(targetCli, []);

    try {
      logger.info('界面.执行.直接.请求', { cli: targetCli }, LogCategory.UI);
      const response = await this.cliFactory.sendMessage(targetCli, prompt, imagePaths);
      logger.info('界面.执行.直接.响应', { cli: targetCli, preview: response.content?.substring(0, 100) }, LogCategory.UI);
      const executionStats = this.intelligentOrchestrator.getExecutionStats();
      if (executionStats) {
        executionStats.recordExecution({
          cli: targetCli,
          taskId: task.id,
          subTaskId: `direct-${task.id}`,
          success: !response.error,
          duration: Date.now() - startTime,
          error: response.error,
          inputTokens: response.tokenUsage?.inputTokens,
          outputTokens: response.tokenUsage?.outputTokens,
        });
      }

      if (response.error) {
        await taskManager.failTask(task.id, response.error);
        this.postMessage({ type: 'cliError', cli: targetCli, error: response.error });
      } else {
        await taskManager.completeTask(task.id);
        this.saveMessageToSession(prompt, response.content || '', targetCli, 'worker');
      }

    } catch (error) {
      logger.error('界面.执行.直接.失败', error, LogCategory.UI);
      const errorMsg = error instanceof Error ? error.message : String(error);
      await taskManager.failTask(task.id, errorMsg);
      const executionStats = this.intelligentOrchestrator.getExecutionStats();
      if (executionStats) {
        executionStats.recordExecution({
          cli: targetCli,
          taskId: task.id,
          subTaskId: `direct-${task.id}`,
          success: false,
          duration: Date.now() - startTime,
          error: errorMsg,
        });
      }
      this.postMessage({ type: 'cliError', cli: targetCli, error: errorMsg });
    }

    this.sendStateUpdate();
  }

  /** 发送状态更新到 Webview */
  private sendStateUpdate(): void {
    const state = this.buildUIState();
    this.postMessage({ type: 'stateUpdate', state });
  }

  /** 创建并切换到新会话（对齐任务/对话会话） */
  public async createNewSession(): Promise<void> {
    await this.handleNewSession();
  }

  /** 处理新会话创建流程 */
  private async handleNewSession(): Promise<void> {
    // 创建新会话前，先中断当前任务
    await this.interruptCurrentTask({ silent: true });
    // 创建新会话时，重置所有 CLI 进程
    await this.cliFactory.resetAllSessions();
    const newSession = this.sessionManager.createSession();
    // 更新活跃会话ID
    this.activeSessionId = newSession.id;
    logger.info('界面.会话.已创建', { sessionId: this.activeSessionId }, LogCategory.UI);
    // 通知 webview 新会话已创建
    this.postMessage({ type: 'sessionCreated', session: newSession as any });
    this.postMessage({ type: 'sessionsUpdated', sessions: this.sessionManager.getAllSessions() as any[] });
    this.sendStateUpdate();
  }

  /** 切换到指定会话 */
  private async switchToSession(sessionId: string): Promise<void> {
    await this.cliFactory.resetAllSessions();
    this.activeSessionId = sessionId;
    this.ensureSessionExists(sessionId);
  }

  /** 确保任务会话存在并已切换 */
  private ensureSessionExists(sessionId: string) {
    const existing = this.sessionManager.getSession(sessionId);
    if (existing) {
      this.sessionManager.switchSession(sessionId);
      return existing;
    }
    return this.sessionManager.createSession(undefined, sessionId);
  }

  /** 初始化会话（用于启动时恢复） */
  private ensureSessionAlignment(): void {
    const session = this.sessionManager.getCurrentSession();
    if (session) {
      this.activeSessionId = session.id;
      return;
    }

    const newSession = this.sessionManager.createSession();
    this.activeSessionId = newSession.id;
  }

  /** 保存消息到当前会话 */
  private saveMessageToSession(
    userPrompt: string,
    assistantResponse: string,
    cli?: CLIType,
    source?: MessageSource
  ): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      return;
    }
    if (assistantResponse) {
      this.sessionManager.addMessage('assistant', assistantResponse, cli, source);
    }
    this.sendStateUpdate();
  }

  /** 保存当前会话的完整数据（从前端同步） */
  private saveCurrentSessionData(messages: any[], cliOutputs: Record<string, any[]>): void {
    const currentSession = this.sessionManager.getCurrentSession();
    if (!currentSession) {
      logger.info('界面.会话.保存.跳过', { reason: 'no_current_session' }, LogCategory.UI);
      return;
    }

    // 转换前端消息格式为后端格式
    const sessionMessages = messages.map(m => ({
      id: m.id || `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`,
      role: m.role as 'user' | 'assistant',
      content: m.content,
      cli: m.cli,
      timestamp: typeof m.timestamp === 'number' ? m.timestamp : Date.now(),
      images: m.images,
      source: m.source,
    }));

    // 使用新的 API 保存会话数据
    this.sessionManager.updateSessionData(currentSession.id, sessionMessages, cliOutputs);
    logger.info('界面.会话.保存.完成', { messageCount: sessionMessages.length }, LogCategory.UI);
  }

  /** 构建 UI 状态 */
  private buildUIState(): UIState {
    const currentSession = this.sessionManager.getCurrentSession();
    const tasks = currentSession?.tasks ?? [];
    const currentTask = tasks.find(t => t.status === 'running') ?? tasks[tasks.length - 1];
    const activePlanRecordRaw = currentSession?.id
      ? this.intelligentOrchestrator.getActivePlanForSession(currentSession.id)
      : null;
    const activePlanRecord = activePlanRecordRaw?.plan?.needsWorker === false ? null : activePlanRecordRaw;
    // 使用统一会话管理器的会话数据
    const allSessions = this.sessionManager.getAllSessions();

    // 构建 CLI 状态（包含能力信息）
    const cliStatuses: CLIStatus[] = Array.from(this.cliStatuses.values()).map(status => ({
      ...status,
      capabilities: CLI_CAPABILITIES[status.type],
    }));


    const isRunning = currentTask?.status === 'running' || this.intelligentOrchestrator.running;

    return {
      // 使用 activeSessionId 作为单一真相来源，确保与消息发送时的 sessionId 一致
      currentSessionId: this.activeSessionId ?? currentSession?.id,
      sessions: allSessions as any[],
      currentTask,
      tasks,
      cliStatuses,
      degradationStrategy: {
        level: 3,
        availableCLIs: ['claude', 'codex', 'gemini'],
        missingCLIs: [],
        hasOrchestrator: true,
        recommendation: '',
        canProceed: true,
        fallbackMap: {},
      },
      pendingChanges: this.snapshotManager.getPendingChanges(),
      isRunning, 
      logs: this.logs,
      interactionMode: this.intelligentOrchestrator.getInteractionMode(),
      orchestratorPhase: this.intelligentOrchestrator.phase,
      activePlan: activePlanRecord
        ? {
          planId: activePlanRecord.id,
          formattedPlan: activePlanRecord.formattedPlan,
          updatedAt: activePlanRecord.updatedAt,
          review: activePlanRecord.review
            ? { status: activePlanRecord.review.status, summary: activePlanRecord.review.summary }
            : undefined,
        }
        : undefined,
    };
  }

  private appendLog(entry: LogEntry): void {
    this.logs.push(entry);
    if (this.logs.length > 200) {
      this.logs.splice(0, this.logs.length - 200);
    }
    if (!this.logFlushTimer) {
      this.logFlushTimer = setTimeout(() => {
        this.logFlushTimer = null;
        this.sendStateUpdate();
      }, 200);
    }
  }


  /** 发送消息到 Webview */
  private postMessage(message: ExtensionToWebviewMessage): void {
    this._view?.webview.postMessage(message);
  }

  /** 获取 HTML 内容 - 从外部模板文件加载 */
  private getHtmlContent(webview: vscode.Webview): string {
    // 读取外部 HTML 模板文件
    const templatePath = path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'index.html');
    let html = fs.readFileSync(templatePath, 'utf-8');

    const stylesUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'styles.css'))
    );
    const loginScriptUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'login.js'))
    );
    // lib 目录 URI（用于加载 marked 和 highlight.js）
    const libUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'lib'))
    );

    html = html.replace('href="styles.css"', `href="${stylesUri}"`);
    html = html.replace('src="login.js"', `src="${loginScriptUri}"`);
    // 替换 lib 目录占位符
    html = html.replace(/\{\{libUri\}\}/g, libUri.toString());

    // 替换 CSP 占位符
    html = html.replace(/\{\{cspSource\}\}/g, webview.cspSource);

    // 注入初始 sessionId，确保 webview 加载时就有正确的值（避免时序问题）
    const initialSessionId = this.activeSessionId || '';
    html = html.replace(/\{\{initialSessionId\}\}/g, initialSessionId);

    return html;
  }

  /** 获取管理器实例 */
  getSessionManager(): UnifiedSessionManager { return this.sessionManager; }
  getSnapshotManager(): SnapshotManager { return this.snapshotManager; }
  getDiffGenerator(): DiffGenerator { return this.diffGenerator; }

  /** 清理所有资源 - VSCode 关闭时调用 */
  async dispose(): Promise<void> {
    logger.info('界面.销毁.开始', undefined, LogCategory.UI);

    try {
      // 1. 中断当前任务
      if (this.intelligentOrchestrator) {
        logger.info('界面.销毁.编排器.中断', undefined, LogCategory.UI);
        this.intelligentOrchestrator.interrupt();
      }

      // 2. 清理 CLI 适配器（终止所有 CLI 进程）
      if (this.cliFactory) {
        logger.info('界面.销毁.CLI.清理', undefined, LogCategory.UI);
        await this.cliFactory.dispose();
      }

      // 3. 移除事件监听器
      globalEventBus.clear();
      logger.info('界面.销毁.事件.已清理', undefined, LogCategory.UI);

      // 4. 清理待确认的 Promise
      if (this.pendingConfirmation) {
        this.pendingConfirmation.reject(new Error('扩展已停用'));
        this.pendingConfirmation = null;
      }
      if (this.pendingQuestion) {
        this.pendingQuestion.reject(new Error('扩展已停用'));
        this.pendingQuestion = null;
      }

      // 5. 清理 Webview
      this._view = undefined;

      logger.info('界面.销毁.完成', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.销毁.失败', error, LogCategory.UI);
    }
  }
}
