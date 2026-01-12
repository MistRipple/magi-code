/**
 * WebviewProvider - Webview 面板提供者
 * 负责：对话面板、任务视图、变更视图、CLI 输出
 */

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
} from '../types';
import { SessionManager } from '../session-manager';
import { ChatSessionManager } from '../chat-session-manager';
import { TaskManager } from '../task-manager';
import { SnapshotManager } from '../snapshot-manager';
import { DiffGenerator } from '../diff-generator';
import { globalEventBus } from '../events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { TaskAnalyzer, CLISelector } from '../task';
import { CLI_CAPABILITIES, CLIResponse } from '../cli/types';
import { IntelligentOrchestrator } from '../orchestrator/intelligent-orchestrator';
import { AceIndexManager } from '../ace/index-manager';

export class WebviewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'multiCli.mainView';

  private _view?: vscode.WebviewView;
  private sessionManager: SessionManager;
  private chatSessionManager: ChatSessionManager;
  private taskManager: TaskManager;
  private snapshotManager: SnapshotManager;
  private diffGenerator: DiffGenerator;
  private cliStatuses: Map<CLIType, CLIStatus> = new Map();
  private cliOutputs: Map<CLIType, string[]> = new Map();
  private orchestratorStreamBuffer = '';
  private orchestratorStreamCli: CLIType | null = null;
  private orchestratorStreamPending = '';
  private orchestratorStreamFlushTimer: NodeJS.Timeout | null = null;

  // 多 CLI 适配器工厂
  private cliFactory: CLIAdapterFactory;

  // 任务分析器和 CLI 选择器
  private taskAnalyzer: TaskAnalyzer;
  private cliSelector: CLISelector;

  // 智能编排器
  private intelligentOrchestrator: IntelligentOrchestrator;

  // Hard Stop 确认机制
  private pendingConfirmation: {
    resolve: (confirmed: boolean) => void;
    reject: (error: Error) => void;
  } | null = null;
  private pendingQuestion: {
    resolve: (answer: string | null) => void;
    reject: (error: Error) => void;
  } | null = null;

  // 当前选择的 CLI（null 表示自动选择/智能编排）
  private selectedCli: CLIType | null = null;

  // 🆕 当前活跃的会话ID，用于会话隔离
  private activeSessionId: string | null = null;
  private logs: LogEntry[] = [];
  private logFlushTimer: NodeJS.Timeout | null = null;

  // 🆕 登录状态与密钥存储
  private readonly authSecretKey = 'multiCli.apiKey';
  private readonly authStatusKey = 'multiCli.loggedIn';
  private loginInFlight = false;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly context: vscode.ExtensionContext,
    private readonly workspaceRoot: string
  ) {
    // 初始化管理器
    this.sessionManager = new SessionManager(workspaceRoot);
    this.chatSessionManager = new ChatSessionManager(workspaceRoot);
    this.taskManager = new TaskManager(this.sessionManager);
    this.snapshotManager = new SnapshotManager(this.sessionManager, workspaceRoot);
    this.diffGenerator = new DiffGenerator(this.sessionManager, workspaceRoot);

    // 对齐会话管理器，确保任务/快照与对话会话一致
    this.ensureSessionAlignment();

    // 初始化任务分析器和 CLI 选择器（从配置读取 skills）
    this.taskAnalyzer = new TaskAnalyzer();
    const config = vscode.workspace.getConfiguration('multiCli');
    const timeout = config.get<number>('timeout') ?? 300000;
    const idleTimeout = config.get<number>('idleTimeout') ?? 120000;
    const maxTimeout = config.get<number>('maxTimeout') ?? 900000;

    // 初始化多 CLI 适配器工厂
    this.cliFactory = new CLIAdapterFactory({ cwd: workspaceRoot, timeout, idleTimeout, maxTimeout });
    this.setupCLIAdapters();
    const userSkills = config.get<Record<string, string>>('skills') || {};
    this.cliSelector = new CLISelector(userSkills as Partial<import('../task/cli-selector').CLISkillsConfig>);

    // 初始化智能编排器
    this.intelligentOrchestrator = new IntelligentOrchestrator(
      this.cliFactory,
      this.taskManager,
      this.snapshotManager,
      this.workspaceRoot,
      { timeout, idleTimeout, maxTimeout }
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

  /** 设置所有 CLI 适配器事件监听 */
  private setupCLIAdapters(): void {
    // 监听工厂的统一事件
    this.cliFactory.on('output', ({
      type,
      chunk,
      source,
      adapterRole
    }: {
      type: CLIType;
      chunk: string;
      source?: string;
      adapterRole?: 'worker' | 'orchestrator';
    }) => {
      if (adapterRole === 'orchestrator') {
        if (!chunk) {
          return;
        }
        if (this.orchestratorStreamCli !== type) {
          this.orchestratorStreamBuffer = '';
          this.orchestratorStreamCli = type;
        }
        this.orchestratorStreamBuffer += chunk;
        this.orchestratorStreamPending += chunk;
        if (!this.orchestratorStreamFlushTimer) {
          this.orchestratorStreamFlushTimer = setTimeout(() => {
            const pending = this.orchestratorStreamPending;
            this.orchestratorStreamPending = '';
            this.orchestratorStreamFlushTimer = null;
            if (!pending) {
              return;
            }
            this.postMessage({
              type: 'streamingUpdate',
              content: pending,
              append: true,
              sentAt: Date.now(),
              source: source || 'orchestrator',
              cli: type
            } as any);
          }, 50);
        }
        return;
      }
      const outputs = this.cliOutputs.get(type) || [];
      outputs.push(chunk);
      this.cliOutputs.set(type, outputs);
      // 添加 sessionId，用于会话隔离
      this.postMessage({
        type: 'subTaskOutput',
        subTaskId: type,
        output: chunk,
        cliType: type,
        source: source as MessageSource | undefined,
        sessionId: this.activeSessionId
      });
    });
    this.cliFactory.on('response', ({
      type,
      response,
      adapterRole,
      source
    }: {
      type: CLIType;
      response: CLIResponse;
      adapterRole?: 'worker' | 'orchestrator';
      source?: string;
    }) => {
      if (adapterRole !== 'orchestrator') {
        return;
      }
      const content = response.error
        ? `错误: ${response.error}`
        : (response.content || this.orchestratorStreamBuffer);
      if (content) {
        this.postMessage({
          type: 'streamingComplete',
          content,
          sentAt: Date.now(),
          source: source || 'orchestrator',
          cli: type,
          error: response.error
        } as any);
      }
      this.orchestratorStreamBuffer = '';
      this.orchestratorStreamCli = null;
      this.orchestratorStreamPending = '';
      if (this.orchestratorStreamFlushTimer) {
        clearTimeout(this.orchestratorStreamFlushTimer);
        this.orchestratorStreamFlushTimer = null;
      }
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

  /** 设置智能编排器的 Hard Stop 确认回调 */
  private setupOrchestratorConfirmation(): void {
    // 设置 Hard Stop 确认回调
    this.intelligentOrchestrator.setConfirmationCallback(async (plan, formattedPlan) => {
      return new Promise<boolean>((resolve, reject) => {
        // 保存 resolve/reject 以便后续处理用户响应
        this.pendingConfirmation = { resolve, reject };

        // 发送确认请求消息
        this.postMessage({
          type: 'confirmationRequest',
          plan: plan,
          formattedPlan: formattedPlan,
        } as any);

        console.log('[MultiCLI] Hard Stop: 等待用户确认执行计划...');
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

        console.log('[MultiCLI] Recovery: 等待用户决策...');
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
        console.log('[MultiCLI] Orchestrator: 等待用户补充信息...');
      });
    });
  }

  /** 处理用户对执行计划的确认响应 */
  private handlePlanConfirmation(confirmed: boolean): void {
    if (this.pendingConfirmation) {
      console.log(`[MultiCLI] 用户确认结果: ${confirmed ? 'Y' : 'N'}`);
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

  /** 绑定全局事件 */
  private bindEvents(): void {
    // 任务相关事件
    globalEventBus.on('task:created', () => this.sendStateUpdate());
    globalEventBus.on('task:started', (event) => {
      this.sendStateUpdate();
      // 🆕 发送运行状态到前端
      this.postMessage({
        type: 'phaseChanged',
        phase: 'started',
        taskId: event.taskId || '',
        isRunning: true
      } as any);
    });
    globalEventBus.on('task:completed', (event) => {
      this.sendStateUpdate();
      // 🆕 发送完成状态到前端
      this.postMessage({
        type: 'phaseChanged',
        phase: 'completed',
        taskId: event.taskId || '',
        isRunning: false
      } as any);
    });
    globalEventBus.on('task:failed', (event) => {
      this.sendStateUpdate();
      // 🆕 发送失败状态到前端
      this.postMessage({
        type: 'phaseChanged',
        phase: 'failed',
        taskId: event.taskId || '',
        isRunning: false
      } as any);
    });
    globalEventBus.on('task:interrupted', (event) => {
      this.sendStateUpdate();
      // 🆕 发送中断状态到前端
      this.postMessage({
        type: 'phaseChanged',
        phase: 'interrupted',
        taskId: event.taskId || '',
        isRunning: false
      } as any);
    });
    globalEventBus.on('subtask:started', (event) => {
      // 🔧 问题4修复：将主线信息发送到主对话窗口
      const data = event.data as { cli?: string; description?: string };
      if (data?.description) {
        this.postMessage({
          type: 'mainlineUpdate',
          updateType: 'subtask_started',
          taskId: event.taskId || '',
          subTaskId: event.subTaskId || '',
          cli: data.cli || 'system',
          description: data.description,
          timestamp: Date.now()
        } as any);
      }
      this.sendStateUpdate();
    });
    globalEventBus.on('subtask:completed', (event) => {
      // 🔧 问题4修复：将完成信息发送到主对话窗口
      const data = event.data as { success?: boolean; cli?: string; cliType?: string };
      this.postMessage({
        type: 'mainlineUpdate',
        updateType: 'subtask_completed',
        taskId: event.taskId || '',
        subTaskId: event.subTaskId || '',
        success: data?.success ?? true,
        cli: data?.cli || data?.cliType,
        timestamp: Date.now()
      } as any);
      this.sendStateUpdate();
    });
    globalEventBus.on('execution:stats_updated', () => {
      this.sendExecutionStats();
    });
    globalEventBus.on('subtask:failed', (event) => {
      // 🔧 问题4修复：将失败信息发送到主对话窗口
      const data = event.data as { error?: string | object; cli?: string; cliType?: string };
      // 🆕 修复：确保 error 是字符串，避免显示 [object Object]
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
      this.postMessage({
        type: 'mainlineUpdate',
        updateType: 'subtask_failed',
        taskId: event.taskId || '',
        subTaskId: event.subTaskId || '',
        cli: data?.cli || data?.cliType || '',  // 🆕 传递 CLI 信息
        error: errorMsg,
        timestamp: Date.now()
      } as any);
      this.sendStateUpdate();
    });

    // 🆕 Orchestrator UI 消息（主对话窗口）
    globalEventBus.on('orchestrator:ui_message', (event) => {
      const data = event.data as any;
      if (!data?.content) return;

      this.postMessage({
        type: 'orchestratorMessage',
        content: data.content,
        phase: data.metadata?.phase || data.type || '',
        taskId: event.taskId || data.taskId || '',
        messageType: data.type,
        metadata: data.metadata,
      } as any);
    });

    // 🆕 Orchestrator Phase 状态变化事件 - 增强版
    globalEventBus.on('orchestrator:phase_changed', (event) => {
      const data = event.data as { phase: string; isRunning?: boolean; timestamp?: number };
      if (data?.phase) {
        // 🔧 修复页面跳动：只发送 phaseChanged 消息，不触发 sendStateUpdate
        // phaseChanged 只更新阶段指示器，不会重建整个 DOM
        this.postMessage({
          type: 'phaseChanged',
          phase: data.phase,
          taskId: event.taskId || '',
          isRunning: data.isRunning ?? this.intelligentOrchestrator.running
        } as any);
        // 🔧 移除 sendStateUpdate() 调用，避免频繁 DOM 重建导致页面跳动
      }
    });

    // 打断任务事件
    globalEventBus.on('task:interrupt', () => {
      this.interruptCurrentTask();
    });

    globalEventBus.on('subtask:output', (event) => {
      const data = event.data as { output: string; cliType?: CLIType };
      if (data?.output) {
        this.postMessage({
          type: 'subTaskOutput',
          subTaskId: event.subTaskId!,
          output: data.output,
          cliType: data.cliType
        });
      }
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
  }

  /** 🆕 打断当前任务 - 增强版：添加等待和超时机制 */
  private async interruptCurrentTask(options?: { silent?: boolean }): Promise<void> {
    console.log('[MultiCLI] 收到中断请求');

    // 🆕 检查是否有正在运行的任务或 Orchestrator
    const tasks = this.taskManager.getAllTasks();
    const runningTask = tasks.find(t => t.status === 'running');
    const hasRunningTask = runningTask || this.intelligentOrchestrator.running;

    // 1. 首先中断 Orchestrator（这会触发 AbortController）
    if (this.intelligentOrchestrator.running) {
      console.log('[MultiCLI] 中断 Orchestrator');
      await this.intelligentOrchestrator.interrupt();
    }

    // 2. 中断所有 CLI 并等待完成
    console.log('[MultiCLI] 中断所有 CLI...');
    try {
      const interruptCompleted = await Promise.race([
        this.cliFactory.interruptAll().then(() => true),
        new Promise<boolean>((resolve) => setTimeout(resolve, 5000, false)) // 5秒超时
      ]);
      if (!interruptCompleted) {
        console.warn('[MultiCLI] CLI 中断超时，尝试强制断开连接');
      }
      await this.cliFactory.disconnectAll();
      await this.cliFactory.resetAllSessions();
      console.log('[MultiCLI] CLI 已断开并重置会话');
    } catch (error) {
      console.error('[MultiCLI] CLI 中断出错:', error);
      try {
        await this.cliFactory.disconnectAll();
        await this.cliFactory.resetAllSessions();
      } catch (cleanupError) {
        console.error('[MultiCLI] CLI 清理失败:', cleanupError);
      }
    }

    // 3. 重置会话，避免取消后的会话残留影响下次请求
    await this.cliFactory.resetAllSessions();

    // 4. 更新任务状态
    if (runningTask) {
      this.taskManager.updateTaskStatus(runningTask.id, 'interrupted');
    }

    // 清理编排者流式输出缓存，避免跨任务串流
    this.orchestratorStreamBuffer = '';
    this.orchestratorStreamCli = null;
    this.orchestratorStreamPending = '';
    if (this.orchestratorStreamFlushTimer) {
      clearTimeout(this.orchestratorStreamFlushTimer);
      this.orchestratorStreamFlushTimer = null;
    }

    // 🆕 只有在确实有任务运行时才发送中断消息
    if (hasRunningTask && !options?.silent) {
      // 4. 通知 UI
      this.postMessage({ type: 'toast', message: '任务已打断', toastType: 'info' });

      // 🆕 通知 UI 更新所有运行中的状态卡片为已取消状态
      this.postMessage({
        type: 'taskInterrupted',
        message: '任务已打断'
      } as any);

      this.postMessage({
        type: 'orchestratorMessage',
        content: '任务已打断，可在变更中查看已修改的文件，或选择继续执行。',
        phase: 'interrupted',
        messageType: 'interrupted'
      } as any);
    }

    this.sendStateUpdate();
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
      console.log('[MultiCLI] CLI 可用性检测结果:', availability);

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
      console.error('[MultiCLI] CLI 可用性检测失败:', error);
    }
  }

  /** 处理 Webview 消息 */
  private async handleMessage(message: WebviewToExtensionMessage): Promise<void> {
    console.log('[MultiCLI] 收到 Webview 消息:', message.type);

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
        console.log('[MultiCLI] 处理 executeTask, prompt:', (message as any).prompt);
        const images = (message as any).images || [];
        await this.executeTask((message as any).prompt, undefined, images);
        break;

      case 'interruptTask':
        // 🆕 使用增强版中断逻辑
        console.log('[MultiCLI] 收到 interruptTask 消息, taskId:', message.taskId);
        await this.interruptCurrentTask();
        break;

      case 'pauseTask':
        // 🆕 暂停任务（目前暂不支持真正的暂停，仅记录状态）
        console.log('[MultiCLI] 收到 pauseTask 消息, taskId:', (message as any).taskId);
        this.postMessage({ type: 'toast', message: '暂停功能开发中', toastType: 'info' });
        break;

      case 'resumeTask':
        // 🆕 恢复任务
        console.log('[MultiCLI] 收到 resumeTask 消息, taskId:', (message as any).taskId);
        await this.resumeInterruptedTask();
        break;

      case 'appendMessage':
        // 🆕 补充内容到当前执行的任务
        console.log('[MultiCLI] 收到 appendMessage 消息');
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

      case 'newSession':
        await this.handleNewSession();
        break;

      case 'saveCurrentSession':
        // 🔧 新增：保存当前会话的消息和 CLI 输出
        this.saveCurrentSessionData(message.messages, message.cliOutputs);
        break;

      case 'switchSession':
        // 🆕 切换会话前，先中断当前任务
        if (this.activeSessionId !== message.sessionId) {
          await this.interruptCurrentTask();
        }
        // 切换会话时，同步 CLI 的会话 ID
        await this.switchToSession(message.sessionId);
        // 同时切换聊天会话
        const switchedSession = this.chatSessionManager.switchSession(message.sessionId);
        if (switchedSession) {
          // 🆕 更新活跃会话ID
          this.activeSessionId = message.sessionId;
          // 恢复 CLI sessionIds
          this.postMessage({ type: 'sessionSwitched', sessionId: message.sessionId });
        }
        this.sendStateUpdate();
        break;

      case 'renameSession':
        // 重命名会话
        if (this.chatSessionManager.renameSession(message.sessionId, message.name)) {
          this.postMessage({ type: 'sessionsUpdated', sessions: this.chatSessionManager.getAllSessions() as any[] });
          this.postMessage({ type: 'toast', message: '会话已重命名', toastType: 'success' });
        }
        break;

      case 'closeSession':
        // 删除会话
        if (this.chatSessionManager.deleteSession(message.sessionId)) {
          // 如果删除后没有会话，创建一个新的
          if (this.chatSessionManager.getAllSessions().length === 0) {
            const { chatSession } = this.createAlignedSession();
            this.activeSessionId = chatSession.id;
            this.postMessage({ type: 'sessionCreated', session: chatSession as any });
          }
          this.postMessage({ type: 'sessionsUpdated', sessions: this.chatSessionManager.getAllSessions() as any[] });
          this.postMessage({ type: 'toast', message: '会话已删除', toastType: 'info' });
        }
        this.sessionManager.endSession(message.sessionId);
        this.sendStateUpdate();
        break;

      case 'selectCli':
        // 用户手动选择 CLI（null 表示自动选择）
        this.selectedCli = message.cli || null;
        console.log('[MultiCLI] 用户选择 CLI:', this.selectedCli || '自动');
        break;

      case 'confirmPlan':
        // 用户确认执行计划（Hard Stop 响应）
        this.handlePlanConfirmation((message as any).confirmed);
        break;

      case 'answerQuestions':
        this.handleQuestionAnswer((message as any).answer ?? null);
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
        // 🆕 请求执行统计数据
        this.sendExecutionStats();
        break;
      case 'resetExecutionStats':
        await this.handleResetExecutionStats();
        break;

      case 'checkCliStatus':
        // 🆕 请求 CLI 连接状态
        this.sendCliStatus();
        break;

      case 'clearAllTasks':
        // 🆕 清理所有任务
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
      console.log('[MultiCLI] Prompt 增强配置已保存到:', configPath);
    } catch (error) {
      console.error('[MultiCLI] 保存配置失败:', error);
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
      console.error('[MultiCLI] 读取配置失败:', error);
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
        console.log('[MultiCLI] 开始 ACE 索引...');
        const aceManager = new AceIndexManager(projectRoot, baseUrl, apiKey);
        const indexResult = await aceManager.indexProject();
        if (indexResult.status !== 'error') {
          blobNames = aceManager.loadIndex();
          console.log(`[MultiCLI] ACE 索引完成，共 ${blobNames.length} 个文件块`);
        }
      } catch (error) {
        console.error('[MultiCLI] ACE 索引失败:', error);
        // 索引失败不阻止增强，继续使用空 blobs
      }
    }

    // 2. 收集上下文（5-10 轮对话）
    const conversationHistory = this.chatSessionManager.formatConversationHistory(10);

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

  /** 🆕 发送 CLI 连接状态到前端 */
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
      console.error('[MultiCLI] CLI 状态检测失败:', error);
    }
  }

  /** 🆕 发送执行统计数据到前端 */
  private sendExecutionStats(): void {
    const executionStats = this.intelligentOrchestrator.getExecutionStats();
    if (!executionStats) {
      console.log('[MultiCLI] 执行统计模块未初始化');
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
        totalInputTokens: cliStats.totalInputTokens,
        totalOutputTokens: cliStats.totalOutputTokens,
      };
    });

    // 🆕 计算编排者汇总统计
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

  /** 处理设置交互模式 */
  private handleSetInteractionMode(mode: import('../types').InteractionMode): void {
    console.log(`[MultiCLI] 设置交互模式: ${mode}`);
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
    console.log(`[MultiCLI] 用户恢复决策: ${decision}`);
    if (this.recoveryConfirmationResolver) {
      this.recoveryConfirmationResolver(decision);
      this.recoveryConfirmationResolver = null;
      return;
    }

    if (decision === 'rollback') {
      const count = this.snapshotManager.revertAllChanges();
      const message = count > 0 ? `已回滚 ${count} 个变更` : '没有可回滚的变更';
      this.postMessage({ type: 'toast', message, toastType: 'info' });
      this.postMessage({
        type: 'orchestratorMessage',
        content: `回滚完成：${message}`,
        phase: 'recovery',
        messageType: 'recovery_result'
      } as any);
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
      // 获取原始内容（从快照）
      const snapshotDir = path.join(this.workspaceRoot, '.multicli', 'snapshots');
      const snapshotFile = path.join(snapshotDir, session.id, `${snapshot.id}.snapshot`);
      let originalContent = snapshot.originalContent;

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
      console.error('[MultiCLI] 打开 diff 视图失败:', error);
      this.postMessage({ type: 'toast', message: '打开 diff 视图失败', toastType: 'error' });
    }
  }

  /** 🆕 清理所有任务 */
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
  private getLastInterruptedTask(): { id: string; prompt: string } | null {
    const tasks = this.taskManager.getAllTasks();
    const interrupted = [...tasks].reverse().find(t => t.status === 'interrupted');
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

    const lastTask = this.getLastInterruptedTask();
    if (!lastTask) {
      this.postMessage({ type: 'toast', message: '没有可恢复的任务', toastType: 'info' });
      return;
    }

    const prompt = this.buildResumePrompt(lastTask.prompt, extraInstruction);
    this.postMessage({
      type: 'orchestratorMessage',
      content: '正在恢复上一次任务...',
      phase: 'resuming',
      messageType: 'resume'
    } as any);
    await this.executeTask(prompt, undefined, []);
  }

  /** 🆕 处理补充内容消息 */
  private async handleAppendMessage(taskId: string, content: string): Promise<void> {
    console.log(`[MultiCLI] 补充内容到任务 ${taskId}: ${content.substring(0, 50)}...`);

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
      console.log('[MultiCLI] 补充内容功能：当前为简化实现');
    } catch (error) {
      console.error('[MultiCLI] 补充内容失败:', error);
      this.postMessage({ type: 'toast', message: '补充内容失败', toastType: 'error' });
    }
  }

  /** 处理设置更新 */
  private handleSettingUpdate(key: string, value: unknown): void {
    const config = vscode.workspace.getConfiguration('multiCli');

    // 处理 skills 配置
    if (key.startsWith('skill-')) {
      const taskType = key.replace('skill-', '');
      const currentSkills = config.get<Record<string, string>>('skills') || {};
      currentSkills[taskType] = value as string;
      config.update('skills', currentSkills, vscode.ConfigurationTarget.Global);

      // 更新 CLI 选择器
      this.cliSelector.updateSkills({ [taskType]: value as CLIType });
      console.log('[MultiCLI] 更新技能配置:', taskType, '->', value);
    }
    // 处理其他配置
    else if (key === 'autoSnapshot') {
      config.update('autoSnapshot', value, vscode.ConfigurationTarget.Global);
    }
    else if (key === 'timeout') {
      config.update('timeout', parseInt(value as string, 10), vscode.ConfigurationTarget.Global);
    }

    this.postMessage({ type: 'toast', message: '设置已保存', toastType: 'success' });
  }

  /** 执行任务 */
  private async executeTask(prompt: string, forceCli?: CLIType, images?: Array<{dataUrl: string}>): Promise<void> {
    console.log('[MultiCLI] executeTask 开始, prompt:', prompt, '图片数量:', images?.length || 0);

    // 🆕 确保 activeSessionId 已设置
    if (!this.activeSessionId) {
      const currentSession = this.chatSessionManager.getCurrentSession();
      this.activeSessionId = currentSession?.id || null;
      console.log('[MultiCLI] 设置 activeSessionId:', this.activeSessionId);
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
          console.log('[MultiCLI] 图片已保存:', filePath);
        }
      }
    }

    // 判断执行模式：智能编排 vs 直接执行
    const useIntelligentMode = !forceCli && !this.selectedCli;

    // 🆕 先记录用户消息，用第一条消息自动生成会话标题
    this.chatSessionManager.addMessage('user', prompt);
    this.sendStateUpdate();

    if (useIntelligentMode) {
      // 智能编排模式：Claude 分析 → 分配 CLI → 执行 → 总结
      await this.executeWithIntelligentOrchestrator(prompt, imagePaths);
    } else {
      // 直接执行模式：指定 CLI 直接执行
      await this.executeWithDirectCli(prompt, forceCli || this.selectedCli!, imagePaths);
    }
  }

  /** 智能编排模式执行 */
  private async executeWithIntelligentOrchestrator(prompt: string, imagePaths: string[]): Promise<void> {
    console.log('[MultiCLI] 使用智能编排模式');

    const interactionMode = this.intelligentOrchestrator.getInteractionMode();

    // ask 模式不创建任务（简单对话不需要任务跟踪）
    const isAskMode = interactionMode === 'ask';

    // 🆕 修复：先创建任务，让编排器可以正确同步子任务
    // 如果最终不需要 Worker，任务会被标记为已完成（无子任务）
    const task = isAskMode ? null : this.taskManager.createTask(prompt);
    const taskId = task?.id || `temp-${Date.now()}`;

    if (task) {
      this.taskManager.updateTaskStatus(task.id, 'running');
      this.sendStateUpdate();
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
          // 🆕 子任务已经在 syncPlanToTaskManager 中添加，这里只需更新状态
          this.taskManager.updateTaskStatus(task.id, 'completed');
          console.log('[MultiCLI] 任务已完成:', task.id, '子任务数:', plan?.subTasks?.length || 0);
        } else {
          // 不需要 Worker，标记任务为已完成（无子任务）
          this.taskManager.updateTaskStatus(task.id, 'completed');
          console.log('[MultiCLI] 编排者直接回答，任务已完成（无子任务）');
        }
      }

      // 保存消息历史
      this.saveMessageToSession(prompt, result, undefined, 'orchestrator');

    } catch (error) {
      console.error('[MultiCLI] 智能编排执行错误:', error);
      const errorMsg = error instanceof Error ? error.message : String(error);

      // 🆕 修复：错误时更新任务状态为失败
      if (task) {
        this.taskManager.updateTaskStatus(task.id, 'failed');
      }

      this.postMessage({
        type: 'orchestratorMessage',
        content: errorMsg,
        phase: 'error',
        messageType: 'error',
      });
    }

    this.sendStateUpdate();
  }

  /** 直接 CLI 执行模式 */
  private async executeWithDirectCli(prompt: string, targetCli: CLIType, imagePaths: string[]): Promise<void> {
    console.log(`[MultiCLI] 使用直接执行模式, CLI: ${targetCli}`);

    const task = this.taskManager.createTask(prompt);
    this.taskManager.updateTaskStatus(task.id, 'running');
    this.sendStateUpdate();

    // 清空目标 CLI 的输出
    this.cliOutputs.set(targetCli, []);

    // 发送用户 prompt 到 CLI 输出面板
    const promptMsg = JSON.stringify({
      type: 'user_prompt',
      prompt: prompt,
      cli: targetCli,
      time: new Date().toLocaleTimeString(),
      hasImages: imagePaths.length > 0
    });
    this.postMessage({ type: 'subTaskOutput', subTaskId: targetCli, output: promptMsg + '\n', cliType: targetCli });

    try {
      console.log(`[MultiCLI] 调用 ${targetCli} CLI...`);
      const response = await this.cliFactory.sendMessage(targetCli, prompt, imagePaths);
      console.log(`[MultiCLI] ${targetCli} CLI 响应:`, response.content?.substring(0, 100));

      if (response.error) {
        this.taskManager.updateTaskStatus(task.id, 'failed');
        this.postMessage({ type: 'cliError', cli: targetCli, error: response.error });
      } else {
        this.taskManager.updateTaskStatus(task.id, 'completed');
        this.saveMessageToSession(prompt, response.content || '', targetCli, 'worker');
      }

      this.postMessage({
        type: 'cliResponse',
        cli: targetCli,
        content: response.content,
        error: response.error,
      });

    } catch (error) {
      console.error(`[MultiCLI] ${targetCli} executeTask 错误:`, error);
      this.taskManager.updateTaskStatus(task.id, 'failed');
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.postMessage({ type: 'cliError', cli: targetCli, error: errorMsg });
      this.postMessage({
        type: 'cliResponse',
        cli: targetCli,
        content: '',
        error: errorMsg,
        source: 'orchestrator',
      });
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
    const { chatSession } = this.createAlignedSession();
    // 更新活跃会话ID
    this.activeSessionId = chatSession.id;
    console.log('[MultiCLI] 创建新会话，已重置所有 CLI 进程, activeSessionId:', this.activeSessionId);
    // 通知 webview 新会话已创建
    this.postMessage({ type: 'sessionCreated', session: chatSession as any });
    this.postMessage({ type: 'sessionsUpdated', sessions: this.chatSessionManager.getAllSessions() as any[] });
    this.sendStateUpdate();
  }

  /** 切换到指定会话 */
  private async switchToSession(sessionId: string): Promise<void> {
    await this.cliFactory.resetAllSessions();
    this.ensureSessionExists(sessionId);
  }

  /** 确保任务会话存在并已切换 */
  private ensureSessionExists(sessionId: string) {
    const existing = this.sessionManager.getSession(sessionId);
    if (existing) {
      this.sessionManager.switchSession(sessionId);
      return existing;
    }
    return this.sessionManager.createSession(sessionId);
  }

  /** 创建对齐的任务会话和聊天会话 */
  private createAlignedSession(name?: string) {
    const session = this.sessionManager.createSession();
    const chatSession = this.chatSessionManager.createSession(name, session.id);
    return { session, chatSession };
  }

  /** 初始化会话对齐（用于启动时恢复） */
  private ensureSessionAlignment(): void {
    const chatSession = this.chatSessionManager.getCurrentSession();
    if (chatSession) {
      this.ensureSessionExists(chatSession.id);
      this.activeSessionId = chatSession.id;
      return;
    }

    const session = this.sessionManager.getCurrentSession();
    if (session) {
      this.chatSessionManager.createSession(undefined, session.id);
      this.activeSessionId = session.id;
      return;
    }

    const { chatSession: newChatSession } = this.createAlignedSession();
    this.activeSessionId = newChatSession.id;
  }

  /** 保存消息到当前会话 */
  private saveMessageToSession(
    userPrompt: string,
    assistantResponse: string,
    cli?: CLIType,
    source?: MessageSource
  ): void {
    const session = this.chatSessionManager.getCurrentSession();
    if (!session) {
      return;
    }
    if (assistantResponse) {
      this.chatSessionManager.addMessage('assistant', assistantResponse, cli, source);
    }
    this.sendStateUpdate();
  }

  /** 保存当前会话的完整数据（从前端同步） */
  private saveCurrentSessionData(messages: any[], cliOutputs: Record<string, any[]>): void {
    const currentSession = this.chatSessionManager.getCurrentSession();
    if (!currentSession) {
      console.log('[MultiCLI] saveCurrentSessionData: 没有当前会话');
      return;
    }

    // 转换前端消息格式为后端格式
    const sessionMessages = messages.map(m => ({
      id: m.id || `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`,
      role: m.role as 'user' | 'assistant',
      content: m.content,
      cli: m.cli,
      timestamp: m.time ? new Date().getTime() : Date.now(),
      images: m.images,
      source: m.source,
    }));

    // 使用新的 API 保存会话数据
    this.chatSessionManager.updateSessionData(currentSession.id, sessionMessages, cliOutputs);
    console.log('[MultiCLI] 保存会话数据，消息数:', sessionMessages.length);
  }

  /** 构建 UI 状态 */
  private buildUIState(): UIState {
    const session = this.sessionManager.getCurrentSession();
    const chatSession = this.chatSessionManager.getCurrentSession();
    const tasks = this.taskManager.getAllTasks();
    const currentTask = tasks.find(t => t.status === 'running') ?? tasks[tasks.length - 1];
    // 🔧 修复：使用 ChatSessionManager 的会话数据作为主数据源
    const allChatSessions = this.chatSessionManager.getAllSessions();

    // 构建 CLI 状态（包含能力信息）
    const cliStatuses: CLIStatus[] = Array.from(this.cliStatuses.values()).map(status => ({
      ...status,
      capabilities: CLI_CAPABILITIES[status.type],
    }));

    // 🆕 修复：isRunning 同时考虑 Task 状态和 Orchestrator 运行状态
    const isRunning = currentTask?.status === 'running' || this.intelligentOrchestrator.running;

    return {
      currentSession: session ?? undefined,
      currentSessionId: chatSession?.id ?? session?.id,
      sessions: allChatSessions as any[],  // 🔧 修复：使用 chatSessionManager 的会话
      chatSessions: this.chatSessionManager.getSessionMetas() as any[],
      currentChatSession: chatSession as any,
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
      isRunning,  // 🆕 使用修复后的 isRunning
      logs: this.logs,
      interactionMode: this.intelligentOrchestrator.getInteractionMode(),
      orchestratorPhase: this.intelligentOrchestrator.phase,
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

    html = html.replace('href="styles.css"', `href="${stylesUri}"`);
    html = html.replace('src="login.js"', `src="${loginScriptUri}"`);

    // 替换 CSP 占位符
    html = html.replace(/\{\{cspSource\}\}/g, webview.cspSource);

    return html;
  }

  /** 获取管理器实例 */
  getSessionManager(): SessionManager { return this.sessionManager; }
  getTaskManager(): TaskManager { return this.taskManager; }
  getSnapshotManager(): SnapshotManager { return this.snapshotManager; }
  getDiffGenerator(): DiffGenerator { return this.diffGenerator; }

  /** 🆕 清理所有资源 - VSCode 关闭时调用 */
  async dispose(): Promise<void> {
    console.log('[WebviewProvider] 开始清理资源...');

    try {
      // 1. 中断当前任务
      if (this.intelligentOrchestrator) {
        console.log('[WebviewProvider] 中断编排器...');
        this.intelligentOrchestrator.interrupt();
      }

      // 2. 清理 CLI 适配器（终止所有 CLI 进程）
      if (this.cliFactory) {
        console.log('[WebviewProvider] 清理 CLI 适配器...');
        await this.cliFactory.dispose();
      }

      // 3. 移除事件监听器
      globalEventBus.clear();
      console.log('[WebviewProvider] 事件监听器已移除');

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

      console.log('[WebviewProvider] 资源清理完成');
    } catch (error) {
      console.error('[WebviewProvider] 清理资源时出错:', error);
    }
  }
}
