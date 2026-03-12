/**
 * WebviewProvider - Webview 面板提供者
 * 负责：对话面板、任务视图、变更视图、Agent 输出
 */

import { logger, LogCategory } from '../logging';
import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { t, setLocale as setExtensionLocale, type LocaleCode } from '../i18n';
import { ConfigManager } from '../config';
import {
  UIState,
  WebviewToExtensionMessage,
  ExtensionToWebviewMessage,
  MessageSource,
  LogEntry,
  PermissionMatrix,
  StrategyConfig,
  WorkerStatus,
  WorkerSlot,
} from '../types';
import {
  StandardMessage,
  MessageLifecycle,
  MessageType,
  MessageCategory,
  ContentBlock,
  StandardizedToolResultPayload,
  DataMessageType,
  NotifyLevel,
  ControlMessageType,
  createStandardMessage,
  createUserInputMessage,
  createStreamingMessage,
  createErrorMessage,
} from '../protocol/message-protocol';
import { UnifiedSessionManager, type SessionMessage } from '../session';
import { TaskView } from '../task/task-view-adapter';
import { SnapshotManager } from '../snapshot-manager';
import { DiffGenerator } from '../diff-generator';
import { globalEventBus } from '../events';
import { IAdapterFactory } from '../adapters/adapter-factory-interface';
import { LLMAdapterFactory } from '../llm/adapter-factory';
import { MissionDrivenEngine } from '../orchestrator/core';
import { MessageHub } from '../orchestrator/core/message-hub';
import { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import { InstructionSkillDefinition } from '../tools/skills-manager';
import { buildInstructionSkillPrompt } from '../tools/skill-installation';
import { MermaidPanel } from './mermaid-panel';
import type { CommandHandler, CommandHandlerContext } from './handlers/types';
import { ConfigCommandHandler, McpCommandHandler, SkillsCommandHandler, KnowledgeCommandHandler } from './handlers';
import { isAbortError } from '../errors';
import { isModelOriginIssue, toModelOriginUserMessage } from '../errors/model-origin';
import { trackModelOriginEvent } from '../errors/model-origin-observability';
import { EventBindingService } from './event-binding-service';
import { WorkerStatusService } from './worker-status-service';
import { PromptEnhancerService } from '../services/prompt-enhancer-service';
import {
  MissionOrchestrator,
} from '../orchestrator/core';
import { WorkspaceFolderInfo, WorkspaceRoots } from '../workspace/workspace-roots';

type WebviewMessagePriority = 'high' | 'normal';

type OrchestratorRuntimeReason = ReturnType<MissionDrivenEngine['getLastExecutionStatus']>['runtimeReason'];
type OrchestratorExecutionResult = {
  success: boolean;
  taskId: string;
  runtimeReason: OrchestratorRuntimeReason;
  errors: string[];
  recoverable: boolean;
  error?: string;
};
type PendingRecoveryContext = {
  taskId: string;
  prompt: string;
  sessionId: string;
  runtimeReason: OrchestratorRuntimeReason;
  errors: string[];
  canRetry: boolean;
  canRollback: boolean;
};
type OrchestratorQueueItem = {
  prompt: string;
  imagePaths: string[];
  sessionId: string;
  turnId: string;
  resolve: (result: OrchestratorExecutionResult) => void;
};
type QueuedUserTurn = {
  id: string;
  content: string;
  createdAt: number;
};

const HIGH_PRIORITY_MESSAGE_TYPES = new Set<ExtensionToWebviewMessage['type']>([
  'unifiedMessage',
  'unifiedUpdate',
  'unifiedComplete',
]);

const COALESCE_MESSAGE_TYPES = new Set<ExtensionToWebviewMessage['type']>();

class WebviewMessageBus {
  private highQueue: ExtensionToWebviewMessage[] = [];
  private normalQueue: ExtensionToWebviewMessage[] = [];
  private processing = false;

  constructor(
    private readonly getView: () => vscode.WebviewView | undefined,
    private readonly getPriority: (message: ExtensionToWebviewMessage) => WebviewMessagePriority,
    private readonly coalesceTypes: Set<ExtensionToWebviewMessage['type']>
  ) {}

  send(message: ExtensionToWebviewMessage): void {
    const priority = this.getPriority(message);
    const queue = priority === 'high' ? this.highQueue : this.normalQueue;

    if (priority === 'normal' && this.coalesceTypes.has(message.type)) {
      for (let i = queue.length - 1; i >= 0; i -= 1) {
        if (queue[i].type === message.type) {
          queue[i] = message;
          this.flush();
          return;
        }
      }
    }

    queue.push(message);
    this.flush();
  }

  private flush(): void {
    if (this.processing) {
      return;
    }
    this.processing = true;
    void this.processLoop();
  }

  private async processLoop(): Promise<void> {
    try {
      while (true) {
        const next = this.highQueue.shift() ?? this.normalQueue.shift();
        if (!next) {
          break;
        }
        const view = this.getView();
        if (!view) {
          // Webview 不可用，清空队列
          logger.warn('界面.消息.Webview不可用', {
            highQueueLen: this.highQueue.length,
            normalQueueLen: this.normalQueue.length,
            droppedType: next.type,
          }, LogCategory.UI);
          this.highQueue.length = 0;
          this.normalQueue.length = 0;
          break;
        }
        try {
          await view.webview.postMessage(next);
        } catch (postError) {
          logger.warn('界面.消息.发送失败', {
            messageId: 'message' in next ? (next.message as StandardMessage)?.id : undefined,
            error: String(postError),
          }, LogCategory.UI);
        }
      }
    } catch (error) {
      logger.warn('界面.消息.循环异常', { error: String(error) }, LogCategory.UI);
    } finally {
      this.processing = false;
      if (this.highQueue.length || this.normalQueue.length) {
        this.flush();
      }
    }
  }
}

export class WebviewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'magi.mainView';

  private readonly MAX_REASONABLE_ARRAY_LENGTH = 1_000_000;
  private readonly workspaceRoot: string;
  private readonly workspaceFolders: WorkspaceFolderInfo[];
  private readonly workspaceRoots: WorkspaceRoots;

  private _view?: vscode.WebviewView;
  private sessionManager: UnifiedSessionManager;
  private snapshotManager: SnapshotManager;
  private diffGenerator: DiffGenerator;
  private readonly messageFlowLogEnabled = process.env.MAGI_MESSAGE_FLOW_LOG === '1';
  private readonly messageFlowLogPath: string;
  private webviewMessageBus: WebviewMessageBus;

  // 统一消息出口
  private messageHub: MessageHub;
  private requestTimeouts: Map<string, NodeJS.Timeout> = new Map();
  // messageId → requestId 映射，用于 StreamUpdate 事件中清除超时
  private messageIdToRequestId: Map<string, string> = new Map();

  // 适配器工厂（LLM 模式）
  private adapterFactory: IAdapterFactory;

  // 编排引擎
  private orchestratorEngine: MissionDrivenEngine;

  // Mission-Driven 编排器（新架构）- MissionExecutor 已合并到 MissionOrchestrator
  private missionOrchestrator?: MissionOrchestrator;

  // 项目知识库
  private projectKnowledgeBase?: ProjectKnowledgeBase;

  // 提示词增强服务
  private promptEnhancer: PromptEnhancerService;

  // 事件绑定服务（从 WVP 提取）
  private eventBindingService: EventBindingService;

  private recentRequestIds: Map<string, number> = new Map();

  // Worker 状态检查服务
  private workerStatusService: WorkerStatusService;
  private interactionModeUpdatedAt = 0;


  private activeSessionId: string | null = null;
  private logs: LogEntry[] = [];
  private logFlushTimer: NodeJS.Timeout | null = null;


  private readonly authSecretKey = 'magi.apiKey';
  private readonly authStatusKey = 'magi.loggedIn';
  private loginInFlight = false;
  private startupRecoveryPromise: Promise<void> | null = null;
  private runtimeInitializationPromise: Promise<void>;
  private runtimeInitializationError: string | null = null;
  private locale: LocaleCode;

  // CommandHandler 委派
  private readonly commandHandlers: CommandHandler[];
  private readonly handlerCtx: CommandHandlerContext;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly context: vscode.ExtensionContext,
    workspaceFolders: WorkspaceFolderInfo[]
  ) {
    const normalizedFolders = workspaceFolders
      .filter(folder => folder && folder.path)
      .map(folder => ({ name: folder.name, path: folder.path }));
    if (normalizedFolders.length === 0) {
      throw new Error(t('provider.errors.noWorkspaceDetected'));
    }

    this.workspaceFolders = normalizedFolders;
    this.workspaceRoots = new WorkspaceRoots(normalizedFolders);
    this.workspaceRoot = this.workspaceRoots.getPrimaryFolder().path;
    this.locale = this.normalizeLocaleCode(ConfigManager.getInstance().get('locale'), 'zh-CN');
    setExtensionLocale(this.locale);
    this.messageFlowLogPath = path.join(this.workspaceRoot, '.magi', 'logs', 'message-flow.jsonl');
    this.webviewMessageBus = new WebviewMessageBus(
      () => this._view,
      this.getWebviewMessagePriority.bind(this),
      COALESCE_MESSAGE_TYPES
    );

    // MessageHub 由 MissionDrivenEngine 创建，初始化后再绑定监听

    // 初始化统一会话管理器
    this.sessionManager = new UnifiedSessionManager(this.workspaceRoot);
    // 统一任务管理器（按会话初始化）
    this.snapshotManager = new SnapshotManager(this.sessionManager, this.workspaceRoot);
    this.diffGenerator = new DiffGenerator(this.sessionManager, this.workspaceRoot);

    // 确保有当前会话
    this.ensureSessionAlignment();

    const config = vscode.workspace.getConfiguration('magi');
    const timeout = config.get<number>('timeout') ?? 300000;
    const idleTimeout = config.get<number>('idleTimeout') ?? 120000;
    const maxTimeout = config.get<number>('maxTimeout') ?? 900000;
    const permissions = this.normalizePermissions(config.get<Partial<PermissionMatrix>>('permissions'));
    const strategy = this.normalizeStrategy(config.get<Partial<StrategyConfig>>('strategy'));

    // 初始化 LLM 适配器工厂
    this.adapterFactory = new LLMAdapterFactory({
      cwd: this.workspaceRoot,
      workspaceFolders: this.workspaceFolders,
    });

    // 初始化编排引擎
    this.orchestratorEngine = new MissionDrivenEngine(
      this.adapterFactory,
      { timeout, maxRetries: 3, permissions, strategy },
      this.workspaceRoot,
      this.snapshotManager,
      this.sessionManager
    );
    this.messageHub = this.orchestratorEngine.getMessageHub();

    // 🔧 统一消息通道：注入 MessageHub 到 AdapterFactory（必须在创建 Adapter 之前）
    // MessageHub 从 orchestratorEngine 获取后，立即注入给 AdapterFactory
    // 这样 Adapter 可以直接通过 MessageHub 发送消息
    (this.adapterFactory as LLMAdapterFactory).setMessageHub(this.messageHub);
    this.syncMessageHubTrace(this.activeSessionId);

    // 注入 SnapshotManager 到 ToolManager（确保工具级文件写入自动创建快照）
    (this.adapterFactory as LLMAdapterFactory).getToolManager().setSnapshotManager(this.snapshotManager);

    // 异步初始化运行时（AdapterFactory + OrchestratorEngine）并建立统一门闩
    const adapterFactoryInit = (this.adapterFactory as LLMAdapterFactory).initialize().catch(err => {
      logger.error('Failed to initialize LLM adapter factory', { error: err.message }, LogCategory.LLM);
      throw err;
    });
    const orchestratorEngineInit = this.orchestratorEngine.initialize().catch(err => {
      logger.error('Failed to initialize orchestrator engine', { error: err.message }, LogCategory.ORCHESTRATOR);
      throw err;
    });
    this.runtimeInitializationPromise = Promise.all([adapterFactoryInit, orchestratorEngineInit])
      .then(() => {
        this.runtimeInitializationError = null;
      })
      .catch((error) => {
        this.runtimeInitializationError = error instanceof Error ? error.message : String(error);
      });

    this.interactionModeUpdatedAt = Date.now();

    this.orchestratorEngine.setExtensionContext(this.context);

    // 初始化项目知识库
    this.initializeProjectKnowledgeBase();

    // 初始化提示词增强服务
    this.promptEnhancer = new PromptEnhancerService({
      workspaceRoot: this.workspaceRoot,
      getToolManager: () => this.adapterFactory.getToolManager?.(),
      getConversationHistory: (maxRounds) => this.sessionManager.formatConversationHistory(maxRounds),
    });

    // 先注入 codebase_retrieval 基础服务，消除启动空窗：
    // 即使 PKB 还在初始化，也可先走本地回退检索（grep/lsp）。
    this.injectCodebaseRetrievalService();

    // 初始化 CommandHandler 委派
    this.handlerCtx = {
      sendData: (dataType, payload) => this.sendData(dataType, payload),
      sendToast: (msg, level, duration) => this.sendToast(msg, level, duration),
      sendStateUpdate: () => this.sendStateUpdate(),
      getAdapterFactory: () => this.adapterFactory,
      getOrchestratorEngine: () => this.orchestratorEngine,
      getProjectKnowledgeBase: () => this.projectKnowledgeBase,
      getWorkspaceRoot: () => this.workspaceRoot,
      getPromptEnhancer: () => this.promptEnhancer,
      getExtensionUri: () => this.extensionUri,
      refreshWorkerStatus: () => {
        this.workerStatusService.invalidateCache();
        void this.workerStatusService.sendWorkerStatus(true).catch((error) => {
          logger.warn('界面.配置变更.模型状态刷新_失败', { error: String(error) }, LogCategory.UI);
        });
      },
    };
    this.commandHandlers = [
      new ConfigCommandHandler(),
      new McpCommandHandler(),
      new SkillsCommandHandler(),
      new KnowledgeCommandHandler(),
    ];

    // 初始化 Worker 状态检查服务
    this.workerStatusService = new WorkerStatusService({
      sendData: (dataType, payload) => this.sendData(dataType, payload),
      getAdapterFactory: () => this.adapterFactory,
    });

    // 初始化事件绑定服务（统一管理 globalEventBus / MessageHub / Adapter / MO 事件）
    this.eventBindingService = new EventBindingService({
      getActiveSessionId: () => this.activeSessionId,
      getMessageHub: () => this.messageHub,
      getOrchestratorEngine: () => this.orchestratorEngine,
      getAdapterFactory: () => this.adapterFactory,
      getMissionOrchestrator: () => this.missionOrchestrator,
      getMessageIdToRequestId: () => this.messageIdToRequestId,
      sendStateUpdate: () => this.sendStateUpdate(),
      sendData: (dataType, payload) => this.sendData(dataType, payload),
      sendToast: (msg, level, duration) => this.sendToast(msg, level, duration),
      sendExecutionStats: () => this.sendExecutionStats(),
      sendOrchestratorMessage: (params) => this.sendOrchestratorMessage(params),
      appendLog: (entry) => this.appendLog(entry),
      postMessage: (message) => this.postMessage(message),
      logMessageFlow: (eventType, payload) => this.logMessageFlow(eventType, payload),
      resolveRequestTimeoutFromMessage: (message) => this.resolveRequestTimeoutFromMessage(message),
      clearRequestTimeout: (requestId) => this.clearRequestTimeout(requestId),
      interruptCurrentTask: (options) => this.interruptCurrentTask(options),
      tryResumePendingRecovery: () => { void this.tryResumePendingRecovery(); },
    });
    this.eventBindingService.bindAll();
    this.configureToolAuthorizationBridge();
  }

  private configureToolAuthorizationBridge(): void {
    const toolManager = this.adapterFactory.getToolManager();
    toolManager.setAuthorizationCallback(async (toolName, toolArgs) => {
      const modeConfig = this.orchestratorEngine.getModeConfig();
      if (!modeConfig.requireToolAuthorization) {
        return true;
      }
      return this.eventBindingService.requestToolAuthorization(toolName, toolArgs);
    });
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

  /**
   * 获取当前会话的所有任务视图
   * 统一 Todo 系统 - 替代 UnifiedTaskManager
   */
  private async getTaskViews(): Promise<TaskView[]> {
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (!sessionId) {
      return [];
    }
    return this.orchestratorEngine.listTaskViews(sessionId);
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

  /**
   * 初始化项目知识库
   */
  private async initializeProjectKnowledgeBase(): Promise<void> {
    try {
      this.projectKnowledgeBase = new ProjectKnowledgeBase({
        projectRoot: this.workspaceRoot
      });
      await this.projectKnowledgeBase.initialize();

      // 设置辅助模型客户端（用于知识提取 + 查询扩展）
      await this.setupKnowledgeExtractionClient();

      // 注入知识库到编排器
      this.orchestratorEngine.setKnowledgeBase(this.projectKnowledgeBase);

      // 监听任务完成事件，自动提取知识
      this.setupAutoKnowledgeExtraction();

      // 设置文件监听器，支持搜索引擎增量更新
      this.setupFileSystemWatcher();

      // 注入代码库检索基础设施（codebase_retrieval 的唯一实现）
      this.injectCodebaseRetrievalService();

      const codeIndex = this.projectKnowledgeBase.getCodeIndex();
      logger.info('项目知识库.已初始化', {
        files: codeIndex ? codeIndex.files.length : 0
      }, LogCategory.SESSION);
    } catch (error: any) {
      logger.error('项目知识库.初始化失败', { error: error.message }, LogCategory.SESSION);
    }
  }

  /** 向 Webview 推送最新的知识库数据 */
  private sendProjectKnowledgeToWebview(): void {
    if (!this.projectKnowledgeBase) { return; }
    const codeIndex = this.projectKnowledgeBase.getCodeIndex();
    const adrs = this.projectKnowledgeBase.getADRs();
    const faqs = this.projectKnowledgeBase.getFAQs();
    const learnings = this.projectKnowledgeBase.getLearnings();
    this.sendData('projectKnowledgeLoaded', { codeIndex, adrs, faqs, learnings });
  }

  /**
   * 设置知识提取客户端（使用辅助模型，同时为本地检索提供查询扩展）
   */
  private async setupKnowledgeExtractionClient(): Promise<void> {
    try {
      const { createKnowledgeExtractionClient } = await import('../knowledge/knowledge-extraction-client');
      const executionStats = this.orchestratorEngine.getExecutionStats();
      const client = await createKnowledgeExtractionClient(executionStats);

      const knowledgeBase = this.projectKnowledgeBase;
      if (!knowledgeBase) {
        logger.warn('项目知识库.辅助模型客户端.未设置_知识库未初始化', undefined, LogCategory.SESSION);
        return;
      }

      knowledgeBase.setLLMClient(client);
    } catch (error: any) {
      logger.error('项目知识库.辅助模型客户端.设置失败', { error: error.message }, LogCategory.SESSION);
    }
  }

  /**
   * 设置自动知识提取
   * 监听任务完成事件，自动从会话中提取 ADR 和 FAQ
   */
  private setupAutoKnowledgeExtraction(): void {
    const EXTRACTION_THRESHOLD = 3; // 每完成 3 个任务提取一次

    // 按会话独立统计完成任务数量，避免跨会话串扰
    const completedTaskCountBySession = new Map<string, number>();
    // 每个会话最近一次提取时的消息数量（水位线）
    const extractedMessageCountBySession = new Map<string, number>();

    const resolveSessionIdFromEvent = (event: unknown): string | null => {
      if (!event || typeof event !== 'object') {
        return null;
      }
      const eventRecord = event as Record<string, unknown>;
      const data = (eventRecord.data && typeof eventRecord.data === 'object')
        ? eventRecord.data as Record<string, unknown>
        : undefined;
      const topLevelSessionId = typeof eventRecord.sessionId === 'string' ? eventRecord.sessionId : '';
      const dataSessionId = typeof data?.sessionId === 'string' ? data.sessionId : '';
      const resolved = dataSessionId || topLevelSessionId;
      return resolved || null;
    };

    const extractKnowledgeWithWatermark = async (
      sessionId: string,
      trigger: 'threshold' | 'session-ended',
    ): Promise<void> => {
      const session = this.sessionManager.getSession(sessionId);
      if (!session) {
        return;
      }

      const currentMessageCount = session.messages.length;
      const lastExtractedMessageCount = extractedMessageCountBySession.get(sessionId) || 0;
      if (currentMessageCount <= lastExtractedMessageCount) {
        logger.debug('项目知识库.自动提取.跳过_无新消息', {
          sessionId,
          trigger,
          currentMessageCount,
          lastExtractedMessageCount,
        }, LogCategory.SESSION);
        return;
      }

      const extracted = await this.extractKnowledgeFromSession(sessionId);
      if (extracted) {
        extractedMessageCountBySession.set(sessionId, currentMessageCount);
      }
    };

    // 监听任务完成事件
    globalEventBus.on('task:completed', async (event: any) => {
      // 触发代码索引刷新（防抖，不会每次都全量扫描）
      this.projectKnowledgeBase?.refreshIndex();

      const sessionId = resolveSessionIdFromEvent(event);
      if (!sessionId) {
        logger.warn('项目知识库.自动提取.跳过_任务缺少会话标识', {
          taskId: typeof event?.taskId === 'string' ? event.taskId : undefined,
          dataTaskId: typeof event?.data?.taskId === 'string' ? event.data.taskId : undefined,
        }, LogCategory.SESSION);
        return;
      }

      const completedCount = (completedTaskCountBySession.get(sessionId) || 0) + 1;
      completedTaskCountBySession.set(sessionId, completedCount);

      // 达到阈值时提取知识（按会话独立计数）
      if (completedCount >= EXTRACTION_THRESHOLD) {
        completedTaskCountBySession.set(sessionId, 0);
        await extractKnowledgeWithWatermark(sessionId, 'threshold');
      }
    });

    // 监听会话结束事件
    globalEventBus.on('session:ended', async (event: any) => {
      const sessionId = resolveSessionIdFromEvent(event);
      if (sessionId) {
        await extractKnowledgeWithWatermark(sessionId, 'session-ended');
        completedTaskCountBySession.delete(sessionId);
      }
    });

    logger.info('项目知识库.自动提取.已启用', {
      threshold: EXTRACTION_THRESHOLD
    }, LogCategory.SESSION);
  }

  /**
   * 设置文件监听器
   * 监听工作区文件变更，通知搜索引擎进行增量更新
   */
  private setupFileSystemWatcher(): void {
    if (!this.projectKnowledgeBase) return;

    const pkb = this.projectKnowledgeBase;
    const watchedExtensions = pkb.getIndexedExtensions();

    for (const folder of this.workspaceFolders) {
      if (watchedExtensions.length === 0) continue;

      const extensionPattern = `**/*.{${watchedExtensions.join(',')}}`;
      const watcher = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(folder.path, extensionPattern)
      );

      watcher.onDidChange((uri) => {
        pkb.onFileEvent(uri.fsPath, 'changed');
      });
      watcher.onDidCreate((uri) => {
        pkb.onFileEvent(uri.fsPath, 'created');
      });
      watcher.onDidDelete((uri) => {
        pkb.onFileEvent(uri.fsPath, 'deleted');
      });

      // 注册到扩展上下文，确保扩展停用时自动释放
      this.context.subscriptions.push(watcher);
    }

    logger.info('项目知识库.文件监听器.已启用', {
      watchedExtensionCount: watchedExtensions.length,
      watchedExtensions: watchedExtensions.slice(0, 30),
    }, LogCategory.SESSION);
  }

  /**
   * 注入 CodebaseRetrievalService 到 codebase_retrieval 执行器
   * 使用惰性引用，解决 PKB 初始化时序问题
   */
  private injectCodebaseRetrievalService(): void {
    const toolManager = this.adapterFactory.getToolManager?.();
    if (!toolManager) return;

    const { CodebaseRetrievalService } = require('../services/codebase-retrieval-service');
    const retrievalService = new CodebaseRetrievalService({
      getKnowledgeBase: () => this.projectKnowledgeBase,
      executeTool: async (toolCall: { id: string; name: string; arguments: Record<string, any> }) =>
        toolManager.executeInternalTool(toolCall),
      extractKeywords: (query: string) => this.promptEnhancer.extractKeywords(query),
      workspaceFolders: this.workspaceFolders,
    });

    toolManager.getCodebaseRetrievalExecutor().setCodebaseRetrievalService(retrievalService);

    logger.info('CodebaseRetrieval.服务已注入', undefined, LogCategory.SESSION);
  }

  /**
   * 从指定会话提取知识
   */
  private async extractKnowledgeFromSession(sessionId: string): Promise<boolean> {
    try {
      const session = this.sessionManager.getSession(sessionId);
      if (!session || session.messages.length < 5) {
        // 消息太少，不值得提取
        return false;
      }

      logger.info('项目知识库.开始提取知识', {
        sessionId,
        messageCount: session.messages.length
      }, LogCategory.SESSION);

      // 转换消息格式
      const messages = session.messages.map(m => ({
        role: m.role,
        content: m.content
      }));

      const knowledgeBase = this.projectKnowledgeBase;
      if (!knowledgeBase) {
        logger.warn('项目知识库.知识提取跳过_知识库未初始化', { sessionId }, LogCategory.SESSION);
        return false;
      }

      let changed = false;

      // 提取 ADR
      const adrs = await knowledgeBase.extractADRFromSession(messages);
      const adrCountBefore = knowledgeBase.getADRs().length;
      if (adrs.length > 0) {
        // 存储提取到的 ADR
        for (const adr of adrs) {
          knowledgeBase.addADR(adr);
        }
        const addedAdrs = Math.max(knowledgeBase.getADRs().length - adrCountBefore, 0);

        logger.info('项目知识库.ADR提取成功', {
          count: addedAdrs,
          titles: adrs.map(a => a.title)
        }, LogCategory.SESSION);

        if (addedAdrs > 0) {
          this.sendToast(t('toast.extractedAdrs', { count: addedAdrs }), 'success');
          changed = true;
        }
      }

      // 提取 FAQ
      const faqs = await knowledgeBase.extractFAQFromSession(messages);
      const faqCountBefore = knowledgeBase.getFAQs().length;
      if (faqs.length > 0) {
        // 存储提取到的 FAQ
        for (const faq of faqs) {
          knowledgeBase.addFAQ(faq);
        }
        const addedFaqs = Math.max(knowledgeBase.getFAQs().length - faqCountBefore, 0);

        logger.info('项目知识库.FAQ提取成功', {
          count: addedFaqs,
          questions: faqs.map(f => f.question)
        }, LogCategory.SESSION);

        if (addedFaqs > 0) {
          this.sendToast(t('toast.extractedFaqs', { count: addedFaqs }), 'success');
          changed = true;
        }
      }

      // 提取 Learning（优先 LLM，失败时启发式回退）
      const learningCandidates = await knowledgeBase.extractLearningsFromSession(messages);
      let addedLearnings = 0;
      if (learningCandidates.length > 0) {
        for (const candidate of learningCandidates) {
          const result = knowledgeBase.addLearning(candidate.content, candidate.context || `session:${sessionId}`, candidate.tags);
          if (result.status === 'inserted') {
            addedLearnings += 1;
          }
        }

        if (addedLearnings > 0) {
          logger.info('项目知识库.Learning提取成功', {
            count: addedLearnings,
            sessionId,
          }, LogCategory.SESSION);
          this.sendToast(t('toast.extractedLearnings', { count: addedLearnings }), 'success');
          changed = true;
        }
      }

      if (changed) {
        this.sendProjectKnowledgeToWebview();
      } else {
        logger.info('项目知识库.未提取到新知识', { sessionId }, LogCategory.SESSION);
      }
      return true;
    } catch (error: any) {
      logger.error('项目知识库.知识提取失败', {
        sessionId,
        error: error.message
      }, LogCategory.SESSION);
      return false;
    }
  }

  private emitUserAndPlaceholder(
    requestId: string,
    prompt: string,
    imageCount: number,
    images?: Array<{ dataUrl: string }>,
    targetWorker?: string,
    turnId?: string
  ): {
    userMessageId: string;
    placeholderMessageId: string;
  } {
    const traceId = this.messageHub.getTraceId();
    // 图片已通过缩略图展示，不再在文本中附加 [附件: X 张图片]
    const displayContent = prompt;

    const userMessage = createUserInputMessage(displayContent, traceId, {
      metadata: {
        requestId,
        turnId,
        sendingAnimation: true,
        // 附带图片数据供前端展示
        images: images && images.length > 0 ? images : undefined,
        // 指定 Worker 直接对话时，标记目标 Worker，前端据此在 Worker 面板展示用户消息
        ...(targetWorker ? { targetWorker } : {}),
      },
    });

    const placeholderMessage = createStreamingMessage('orchestrator', 'orchestrator', traceId, {
      metadata: {
        isPlaceholder: true,
        placeholderState: 'pending',
        requestId,
        userMessageId: userMessage.id,
      },
    });

    userMessage.metadata.placeholderMessageId = placeholderMessage.id;

    const userSent = this.messageHub.sendMessage(userMessage);
    const placeholderSent = this.messageHub.sendMessage(placeholderMessage);

    if (!userSent || !placeholderSent) {
      logger.error('界面.消息.占位发送_失败', {
        userSent,
        placeholderSent,
        requestId,
        userMessageId: userMessage.id,
        placeholderMessageId: placeholderMessage.id,
      }, LogCategory.UI);
      throw new Error(t('provider.errors.messagePlaceholderSendFailed'));
    }

    // 注册 messageId → requestId 映射，供 StreamUpdate 超时清除使用
    this.messageIdToRequestId.set(placeholderMessage.id, requestId);

    return { userMessageId: userMessage.id, placeholderMessageId: placeholderMessage.id };
  }

  private scheduleRequestTimeout(requestId: string): void {
    this.clearRequestTimeout(requestId);
    const timeout = setTimeout(() => {
      if (!this.requestTimeouts.has(requestId)) {
        return;
      }
      // 首 token 超时由前端 message-handler 统一处理（60s 触发 toast）
      // 后端仅做静默清理，不发送错误消息，避免与前端超时重复提示
      logger.warn('请求超时.后端兜底清理', { requestId }, LogCategory.UI);
      this.clearRequestTimeout(requestId);
    }, 65000);
    this.requestTimeouts.set(requestId, timeout);
  }

  private clearRequestTimeout(requestId: string): void {
    const timeout = this.requestTimeouts.get(requestId);
    if (timeout) {
      clearTimeout(timeout);
      this.requestTimeouts.delete(requestId);
    }
    // 清理 messageId → requestId 映射
    for (const [msgId, reqId] of this.messageIdToRequestId) {
      if (reqId === requestId) {
        this.messageIdToRequestId.delete(msgId);
        break;
      }
    }
  }

  private resolveRequestTimeoutFromMessage(message: StandardMessage): void {
    const meta = message.metadata as Record<string, unknown> | undefined;
    const requestId = meta?.requestId as string | undefined;
    if (!requestId) {
      return;
    }
    const isPlaceholder = Boolean(meta?.isPlaceholder);
    // 使用 MessageType.USER_INPUT 判断用户消息
    const isUserInput = message.type === MessageType.USER_INPUT;
    if (isPlaceholder || isUserInput) {
      return;
    }
    this.clearRequestTimeout(requestId);
  }

  /** 处理交互响应 */
  private async handleInteractionResponse(requestId: string, response: any): Promise<void> {
    if (!requestId) return;

    // 处理动态任务审批
    if (requestId.startsWith('approval-')) {
      const todoId = requestId.replace('approval-', '');
      // 允许的肯定响应值
      const isApproved = response === true || response === 'approved' || response === 'yes' ||
                        (typeof response === 'object' && response.value === 'approved');

      if (isApproved) {
        try {
          const orchestrator = this.orchestratorEngine.getMissionOrchestrator();
          if (orchestrator) {
            await orchestrator.approveTodo(todoId);
            this.sendToast(t('toast.taskApproved'), 'success');
          }
        } catch (error) {
          logger.error('界面.交互.审批_失败', error, LogCategory.UI);
          this.sendToast(t('toast.approvalFailed'), 'error');
        }
      } else {
        // 拒绝逻辑
        this.sendToast(t('toast.taskRejected'), 'info');

        // 【新增】记录被拒绝的方案到 Memory
        const contextManager = this.orchestratorEngine.getContextManager();
        if (contextManager) {
          contextManager.addRejectedApproach(
            t('provider.approvalRejectedReason'),
            t('provider.approvalRejectedDetail'),
            'user'
          );
        }
      }
    }
  }

  private shouldProcessRequest(requestId?: string | null): boolean {
    if (!requestId) return true;
    const now = Date.now();
    const lastSeen = this.recentRequestIds.get(requestId);
    if (lastSeen && now - lastSeen < 30000) {
      return false;
    }
    this.recentRequestIds.set(requestId, now);
    if (this.recentRequestIds.size > 200) {
      for (const [key, ts] of this.recentRequestIds) {
        if (now - ts > 60000) {
          this.recentRequestIds.delete(key);
        }
      }
    }
    return true;
  }

  /**
   * 设置 MissionOrchestrator
   * 用于 Mission-Driven 架构
   */
  setMissionOrchestrator(orchestrator: MissionOrchestrator): void {
    this.missionOrchestrator = orchestrator;
    this.eventBindingService.bindMissionEvents();
  }

  /**
   * 获取 MissionOrchestrator
   */
  getMissionOrchestrator(): MissionOrchestrator | undefined {
    return this.missionOrchestrator;
  }

  // MissionExecutor 已合并到 MissionOrchestrator，移除 setMissionExecutor 和 getMissionExecutor

  /** 打断当前任务 - 增强版：添加等待和超时机制 */
  private async interruptCurrentTask(options?: { silent?: boolean }): Promise<void> {
    logger.info('界面.任务.中断.请求', undefined, LogCategory.UI);

    // 统一 Todo 系统 - 使用 TaskView
    const tasks = await this.getTaskViews();
    const runningTasks = tasks.filter(t => t.status === 'running');
    const hasRunningTask = runningTasks.length > 0 || this.orchestratorEngine.running;


    // 1. 先取消编排引擎（设置 CancellationToken + 中断 Worker 适配器）
    //    确保 Worker 在 abort 错误恢复后通过 cancellationToken.isCancelled 立即退出循环，
    //    避免竞态：先 abort 适配器 → Worker 恢复 → 获取下一个 todo → 此时 token 尚未 cancel
    if (this.orchestratorEngine.running) {
      logger.info('界面.任务.中断.编排器', undefined, LogCategory.UI);
      await this.orchestratorEngine.interrupt();
    }

    // 2. 兜底中断所有适配器（覆盖引擎未跟踪的适配器，如 orchestrator 自身）
    logger.info('界面.任务.中断.适配器.开始', undefined, LogCategory.UI);
    try {
      await this.adapterFactory.interruptAll();
      logger.info('界面.任务.中断.适配器.完成', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.任务.中断.适配器.错误', error, LogCategory.UI);
    }

    // 3. 更新任务状态
    if (runningTasks.length > 0) {
      for (const task of runningTasks) {
        await this.orchestratorEngine.cancelTaskById(task.id);

        // 🔧 P1-4: 发送停止状态卡片，确保 UI 视觉反馈
        // 遍历该任务下的所有子任务，更新其状态
        if (task.subTasks && task.subTasks.length > 0) {
          for (const subTask of task.subTasks) {
            // 仅更新未完成的子任务
            if (subTask.status !== 'completed' && subTask.status !== 'failed' && subTask.status !== 'skipped') {
              this.messageHub.subTaskCard({
                id: subTask.assignmentId || subTask.id, // 优先使用 assignmentId 以匹配 Mission 体系
                title: subTask.title || subTask.description || t('provider.subTaskFallbackTitle'),
                status: 'stopped',
                worker: subTask.assignedWorker,
                summary: t('provider.userAborted'),
              });
            }
          }
        }
      }
    }

    const interruptSessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
    if (interruptSessionId) {
      const recovery = await this.orchestratorEngine.getTaskViewService().recoverRunningState({
        sessionId: interruptSessionId,
      });
      if (recovery.recovered) {
        this.recoveredSinceLastUpdate = true;
      }
    }

    // 清理编排者流式输出缓存，避免跨任务串流
    this.streamMessageIds.clear();
    // 发送 task_failed 控制消息，确保前端 clearProcessingState() 被触发
    // 前端只响应 task_completed/task_failed 来清除处理态，processingStateChanged(false) 会被忽略
    this.messageHub.sendControl(ControlMessageType.TASK_FAILED, {
      error: t('provider.userCancelled'),
      cancelled: true,
      timestamp: Date.now(),
    });
    // 同步清理后端管道的处理态
    this.cancelProcessingResetFallback();
    this.messageHub.forceProcessingState(false);

    if (hasRunningTask && !options?.silent) {
      const interruptionSummary = this.buildInterruptionSummary(runningTasks);
      const interruptSessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id;
      if (interruptSessionId) {
        this.saveMessageToSession('', interruptionSummary, undefined, 'orchestrator', interruptSessionId);
      }

      // 4. 通知 UI
      this.sendToast(t('toast.taskInterrupted'), 'info');


      this.sendOrchestratorMessage({
        content: t('provider.taskInterruptedHint'),
        messageType: 'text',
        metadata: { phase: 'interrupted' },
      });
    }

    this.sendStateUpdate();
  }

  // 流式消息 ID 管理
  private streamMessageIds: Map<string, string> = new Map(); // key: `${source}-${worker}-${target}`, value: messageId
  private recoveredSinceLastUpdate = false;

  /**
   * 发送编排器标准消息（非流式）
   * 用于发送进度更新、子任务摘要、错误等消息
   */
  /**
   * 发送编排器标准消息
   * 🔧 重构：所有消息通过 MessageBus 发送，确保统一的去重和状态管理
   *
   * 消息类型说明：
   * - progress: 进度提示（如"正在分析..."），使用 PROGRESS 类型
   * - error: 错误消息，使用 ERROR 类型
   * - result: 结果消息（通常不应手动发送，LLM响应已通过流式传输）
   * - text: 普通文本消息
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
      lifecycle = MessageLifecycle.STREAMING; // 进度消息标记为流式状态
    } else if (messageType === 'error') {
      type = MessageType.ERROR;
      lifecycle = MessageLifecycle.FAILED;
    } else if (messageType === 'result') {
      type = MessageType.RESULT;
    }

    const safeBlocks: ContentBlock[] = Array.isArray(blocks)
      ? this.assertBlocks(blocks, 'sendOrchestratorMessage.blocks')
      : (content ? [{ type: 'text' as const, content, isMarkdown: false }] : []);

    const standardMessage = createStandardMessage({
      traceId: this.activeSessionId || 'default',
      category: MessageCategory.CONTENT,  // 🔧 统一消息通道：编排器消息为 CONTENT 类别
      type,
      source: 'orchestrator',
      agent: 'orchestrator',
      blocks: safeBlocks,
      lifecycle,
      metadata: {
        taskId,
        isStatusMessage: true, // 标记为状态消息，区别于 LLM 对话响应
        ...metadata,
      },
    });

    // 🔧 通过 MessageHub 统一出口发送
    this.messageHub.sendMessage(standardMessage);
    this.logMessageFlow('orchestratorMessage via MessageHub', standardMessage);
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

    let html = '';
    try {
      // HTML 中已注入 initialSessionId，webview 加载时即有正确的 sessionId
      html = this.getHtmlContent(webviewView.webview);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      logger.error('界面.Webview.加载失败', { error: message }, LogCategory.UI);
      webviewView.webview.html = this.getWebviewErrorHtml(message);
      vscode.window.showErrorMessage(t('provider.errors.webviewRenderFailed', { error: message }));
      return;
    }
    webviewView.webview.html = html;

    // 处理来自 Webview 的消息
    webviewView.webview.onDidReceiveMessage(
      (message: WebviewToExtensionMessage) => this.handleMessage(message),
      undefined,
      this.context.subscriptions
    );

    // Webview 初始化时强制中断可能残留的任务，避免重启后状态错乱。
    // 使用启动恢复栅栏，确保 getState/requestState 不会读到残留 running 状态。
    this.startupRecoveryPromise = this.interruptCurrentTask({ silent: true })
      .catch((error) => {
        logger.warn('界面.启动.残留任务清理_失败', { error: String(error) }, LogCategory.UI);
      })
      .finally(() => {
        this.startupRecoveryPromise = null;
      });

    // 🔧 启动时进行真正的 LLM 连接测试（替代浅层检查）
    // 使用 sendWorkerStatus(true) 强制检测所有模型连接状态
    // 这会发送 workerStatusUpdate 消息，前端能正确处理并更新 BottomTabs 状态
    void this.workerStatusService.sendWorkerStatus(true).catch((error) => {
      logger.warn('界面.启动.模型状态检测_失败', { error: String(error) }, LogCategory.UI);
    });

    // 发送执行统计数据
    this.sendExecutionStats();
  }


  /** 处理 Webview 消息 */
  private async handleMessage(message: WebviewToExtensionMessage): Promise<void> {
    logger.info('界面.Webview.消息.收到', { type: message.type }, LogCategory.UI);

    // 启动恢复栅栏：优先完成残留运行态清理，再处理任意前端请求。
    if (this.startupRecoveryPromise) {
      await this.startupRecoveryPromise;
    }

    // 运行时初始化门闩：执行型请求必须等待 Adapter/Engine 初始化完成。
    if (this.shouldAwaitRuntimeInitialization(message.type)) {
      try {
        await this.ensureRuntimeInitialized();
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : String(error);
        logger.error('界面.运行时.初始化门闩_失败', {
          type: message.type,
          error: errorMsg,
        }, LogCategory.UI);
        if (message.type === 'executeTask' && message.requestId) {
          this.messageHub.taskRejected(message.requestId, errorMsg);
        }
        this.sendToast(t('toast.initFailed', { error: errorMsg }), 'error');
        return;
      }
    }

    // Handler 委派：Config / MCP / Skills / Knowledge
    for (const handler of this.commandHandlers) {
      if (handler.supportedTypes.has(message.type)) {
        await handler.handle(message, this.handlerCtx);
        return;
      }
    }

    switch (message.type) {
      case 'getState':
        this.sendStateUpdate();
        await this.sendCurrentSessionToWebview();
        break;

      case 'requestState':
        this.sendStateUpdate();
        await this.sendCurrentSessionToWebview();
        break;

      case 'webviewReady':
        // Webview 就绪后立即推送完整系统数据（任务、变更、会话等）
        // 这些数据不在 vscode.getState() 持久化范围内，必须由后端主动推送
        logger.info('界面.Webview.就绪', undefined, LogCategory.UI);
        this.sendStateUpdate();
        await this.sendCurrentSessionToWebview();
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

      case 'uiError': {
        logger.error('界面.UI_错误', {
          component: message.component,
          detail: message.detail,
          stack: message.stack,
        }, LogCategory.UI);
        break;
      }

      case 'executeTask':
        logger.info('界面.任务.执行.请求', { promptLength: String(message.prompt || '').length, imageCount: message.images?.length || 0 }, LogCategory.UI);
        const execImages = message.images || [];
        const execRequestId = message.requestId;
        const requestedModeRaw = message.mode;
        const requestedMode = requestedModeRaw === 'ask' || requestedModeRaw === 'auto' ? requestedModeRaw : undefined;
        if (typeof requestedModeRaw === 'string' && !requestedMode) {
          logger.warn('界面.任务.执行.模式_非法', { requestedModeRaw, requestId: execRequestId }, LogCategory.UI);
          this.sendToast(t('toast.invalidInteractionMode'), 'warning');
        }
        if (!this.shouldProcessRequest(execRequestId)) {
          if (execRequestId) {
            this.messageHub.taskRejected(execRequestId, t('provider.duplicateRequestIgnored'));
            const traceId = this.messageHub.getTraceId();
            const errorMessage = createErrorMessage(
              t('provider.duplicateRequestIgnored'),
              'orchestrator',
              'orchestrator',
              traceId,
              { metadata: { requestId: execRequestId } }
            );
            this.messageHub.sendMessage(errorMessage);
          }
          break;
        }
        try {
          if (requestedMode) {
            this.handleSetInteractionMode(requestedMode);
          }
          await this.executeTask(message.prompt, undefined, execImages, execRequestId);
        } catch (error: any) {
          const errorMsg = error instanceof Error ? error.message : String(error);
          if (execRequestId) {
            this.messageHub.taskRejected(execRequestId, errorMsg);
          }
          throw error;
        }
        break;

      case 'interruptTask':

        logger.info('界面.任务.中断.消息', { taskId: message.taskId }, LogCategory.UI);
        await this.interruptCurrentTask({ silent: Boolean(message.silent) });
        break;

      case 'startTask':
        await this.handleStartTask(message.taskId);
        break;

      case 'deleteTask':
        await this.handleDeleteTask(message.taskId);
        break;

      case 'pauseTask':

        logger.info('界面.任务.暂停.消息', { taskId: message.taskId }, LogCategory.UI);
        this.sendToast(t('toast.pauseInDev'), 'info');
        break;

      case 'resumeTask':

        logger.info('界面.任务.恢复.消息', { taskId: message.taskId }, LogCategory.UI);
        await this.resumeInterruptedTask({ taskId: message.taskId });
        break;

      case 'appendMessage':

        logger.info('界面.消息.补充.请求', undefined, LogCategory.UI);
        await this.handleAppendMessage(message.taskId, message.content);
        break;
      case 'updateQueuedMessage':
        this.handleUpdateQueuedMessage(message.queueId, message.content);
        break;
      case 'deleteQueuedMessage':
        this.handleDeleteQueuedMessage(message.queueId);
        break;

      case 'approveChange':
        // 批准单个变更
        this.snapshotManager.acceptChange(message.filePath);
        globalEventBus.emitEvent('change:approved', { data: { filePath: message.filePath } });
        this.sendToast(t('toast.changeApproved'), 'success');
        this.sendStateUpdate();
        break;

      case 'revertChange':
        this.snapshotManager.revertToSnapshot(message.filePath);
        this.sendToast(t('toast.changeReverted'), 'info');
        this.sendStateUpdate();
        break;

      case 'approveAllChanges':
        // 批准所有变更
        {
          const allChanges = this.snapshotManager.getPendingChanges();
          for (const change of allChanges) {
            this.snapshotManager.acceptChange(change.filePath);
          }
          this.sendToast(t('toast.changesApproved', { count: allChanges.length }), 'success');
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
          this.sendToast(t('toast.changesReverted', { count: changes.length }), 'info');
        }
        this.sendStateUpdate();
        break;

      case 'revertMission':
        // 撤销指定轮次（Mission）的所有变更
        {
          const targetMissionId = message.missionId;
          if (!targetMissionId) {
            this.sendToast(t('toast.missingMissionId'), 'warning');
            break;
          }
          const result = this.snapshotManager.revertMission(targetMissionId);
          if (result.reverted > 0) {
            this.sendToast(t('toast.roundReverted', { count: result.reverted }), 'info');
          } else {
            this.sendToast(t('toast.noChangesToRevert'), 'info');
          }
        }
        this.sendStateUpdate();
        break;

      case 'viewDiff':
        // 在 VS Code 原生 diff 视图中查看变更（类似 Augment）
        await this.openVscodeDiff(message.filePath);
        break;

      case 'openFile':
        // 在编辑器中打开文件（从代码块点击文件路径）
        {
          const targetPath = this.resolveOpenFilePath(message);
          if (!targetPath) {
            this.sendToast(t('toast.openFileMissingPath'), 'warning');
            break;
          }
          await this.openFileInEditor(targetPath);
        }
        break;

      case 'openLink':
        // 在外部浏览器中打开链接（从 markdown 链接点击）
        if (message.url && typeof message.url === 'string') {
          vscode.env.openExternal(vscode.Uri.parse(message.url));
        }
        break;

      case 'confirmPlan': {
        const accepted = this.orchestratorEngine.resolvePlanConfirmation(Boolean(message.confirmed));
        if (!accepted) {
          logger.warn('界面.计划确认.无待处理请求', undefined, LogCategory.UI);
        }
        break;
      }

      case 'newSession':
        if (Array.isArray(message.currentMessages)) {
          this.saveCurrentSessionData(message.currentMessages);
        }
        await this.handleNewSession();
        break;

      case 'saveCurrentSession':
        // 保存当前会话的消息
        this.saveCurrentSessionData(message.messages);
        break;

      case 'switchSession':
        if (Array.isArray(message.currentMessages)) {
          this.saveCurrentSessionData(message.currentMessages);
        }

        if (this.activeSessionId !== message.sessionId) {
          await this.interruptCurrentTask({ silent: true });
        }
        // 切换会话
        await this.switchToSession(message.sessionId);
        this.sendStateUpdate();
        break;

      case 'renameSession':
        // 重命名会话
        if (this.sessionManager.renameSession(message.sessionId, message.name)) {
          this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() });
          this.sendToast(t('toast.sessionRenamed'), 'success');
        }
        break;

      case 'closeSession':
        void this.performSessionDelete(message.sessionId);
        break;

      case 'deleteSession': {
        // 🔧 新增：带确认的删除会话（VS Code Webview 沙盒不支持 confirm()）
        const sessionIdToDelete = message.sessionId;
        const needConfirm = message.requireConfirm;

        if (needConfirm) {
          const confirmButton = t('toast.deleteSessionConfirmButton');
          vscode.window.showWarningMessage(
            t('toast.deleteSessionConfirm'),
            { modal: true },
            confirmButton
          ).then((selection) => {
            if (selection === confirmButton) {
              void this.performSessionDelete(sessionIdToDelete);
            }
          });
        } else {
          // 无需确认直接删除
          void this.performSessionDelete(sessionIdToDelete);
        }
        break;
      }

      case 'toolAuthorizationResponse':
        // 用户响应工具授权请求
        this.eventBindingService.handleToolAuthorizationResponse(message.requestId, message.allowed);
        break;

      case 'interactionResponse':
        // 🔧 P3: 处理交互响应 (如动态审批)
        await this.handleInteractionResponse(message.requestId, message.response);
        break;

      case 'updateSetting':
        // 更新设置
        void this.handleSettingUpdate(message.key, message.value);
        break;

      case 'setInteractionMode':
        // 设置交互模式
        this.handleSetInteractionMode(message.mode);
        break;

      case 'confirmRecovery':
        // 用户确认恢复策略
        await this.handleRecoveryConfirmation(message.decision);
        break;

      case 'confirmDeliveryRepair': {
        const decision = message.decision === 'repair' ? 'repair' : 'stop';
        const accepted = this.orchestratorEngine.resolveDeliveryRepairConfirmation(decision);
        if (!accepted) {
          logger.warn('界面.交付修复确认.无待处理请求', undefined, LogCategory.UI);
        }
        break;
      }

      case 'requestExecutionStats':

        this.sendExecutionStats();
        break;
      case 'resetExecutionStats':
        await this.handleResetExecutionStats();
        break;

      case 'checkWorkerStatus':
        this.workerStatusService.sendWorkerStatus(Boolean(message.force));
        break;


      case 'clearAllTasks':

        this.handleClearAllTasks();
        break;

      case 'getDeepTaskState': {
        // 推送当前深度任务模式状态到前端
        const deepTaskValue = vscode.workspace.getConfiguration('magi').get<boolean>('deepTask', false);
        this.sendData('deepTaskChanged', { enabled: deepTaskValue });
        break;
      }

      case 'openMermaidPanel':
        // 在新标签页打开 Mermaid 图表
        this.handleOpenMermaidPanel(message.code, message.title);
        break;
    }
  }

  private shouldAwaitRuntimeInitialization(messageType: WebviewToExtensionMessage['type']): boolean {
    switch (messageType) {
      case 'getState':
      case 'requestState':
      case 'webviewReady':
      case 'login':
      case 'logout':
      case 'getStatus':
      case 'uiError':
      case 'openFile':
      case 'openLink':
      case 'saveCurrentSession':
      case 'getDeepTaskState':
        return false;
      default:
        return true;
    }
  }

  private async ensureRuntimeInitialized(): Promise<void> {
    await this.runtimeInitializationPromise;
    if (this.runtimeInitializationError) {
      throw new Error(this.runtimeInitializationError);
    }
  }

  /** 处理登录消息 */
  private async handleLoginMessage(message: Extract<WebviewToExtensionMessage, { type: 'login' }>): Promise<void> {
    if (this.loginInFlight) {
      // 登录处理中，不向前端发送未消费事件
      return;
    }

    const rawApiKey = message.apiKey;
    const apiKey = typeof rawApiKey === 'string' ? rawApiKey.trim() : '';
    if (!apiKey) {
      // API Key 为空，不向前端发送未消费事件
      return;
    }

    this.loginInFlight = true;
    try {
      await this.storeApiKey(apiKey);
      await this.context.globalState.update(this.authStatusKey, true);
      // 登录成功状态仅记录本地
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      try {
        await this.removeApiKey();
      } catch {
        // 忽略回滚失败，避免覆盖原始错误
      }
      // 登录失败，不向前端发送未消费事件
    } finally {
      this.loginInFlight = false;
    }
  }

  /** 处理登出消息 */
  private async handleLogoutMessage(): Promise<void> {
    if (this.loginInFlight) {
      // 登出处理中，不向前端发送未消费事件
      return;
    }

    try {
      await this.removeApiKey();
      await this.context.globalState.update(this.authStatusKey, false);
      // auth 状态仅记录本地
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      // 登出失败，不向前端发送未消费事件
    }
  }

  /** 处理状态查询消息 */
  private async handleGetStatusMessage(): Promise<void> {
    try {
      const loggedIn = await this.isLoggedIn();
      // auth 状态仅记录本地
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      // 获取状态失败，不向前端发送未消费事件
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

  /**
   * 在新标签页打开 Mermaid 图表
   */
  private handleOpenMermaidPanel(code: string, title?: string): void {
    if (!code) {
      logger.warn('Mermaid.打开失败', { reason: '代码为空' }, LogCategory.UI);
      return;
    }

    try {
      MermaidPanel.createOrShow(this.extensionUri, code, title);
      logger.info('Mermaid.新标签页.已打开', { title }, LogCategory.UI);
    } catch (error: any) {
      logger.error('Mermaid.新标签页.失败', { error: error.message }, LogCategory.UI);
      this.sendToast(t('toast.openChartFailed', { error: error.message }), 'error');
    }
  }

  /** 发送执行统计数据到前端 */
  private sendExecutionStats(): void {
    const executionStats = this.orchestratorEngine.getExecutionStats();
    if (!executionStats) {
      logger.info('界面.执行统计.未初始化', undefined, LogCategory.UI);
      return;
    }

    const modelCatalog = this.buildModelCatalog();
    const catalogMap = new Map(modelCatalog.map(entry => [entry.id, entry]));
    const modelIds = modelCatalog.map(entry => entry.id);
    const normalizeProvider = (provider?: string): 'openai' | 'anthropic' | 'unknown' => {
      if (provider === 'openai' || provider === 'anthropic') {
        return provider;
      }
      return 'unknown';
    };

    const stats = executionStats.getAllStats(modelIds).map(workerStats => ({
      worker: workerStats.worker,
      provider: normalizeProvider(catalogMap.get(workerStats.worker)?.provider),
      totalExecutions: workerStats.totalExecutions,
      successCount: workerStats.successCount,
      failureCount: workerStats.failureCount,
      successRate: workerStats.successRate,
      avgDuration: workerStats.avgDuration,
      isHealthy: workerStats.isHealthy,
      healthScore: workerStats.healthScore,
      lastError: workerStats.lastError,
      lastExecutionTime: workerStats.lastExecutionTime,
      totalInputTokens: workerStats.totalInputTokens,
      totalOutputTokens: workerStats.totalOutputTokens,
    }));

    const orchestratorStats = {
      totalTasks: stats.reduce((sum, s) => sum + s.totalExecutions, 0),
      totalSuccess: stats.reduce((sum, s) => sum + s.successCount, 0),
      totalFailed: stats.reduce((sum, s) => sum + s.failureCount, 0),
      totalInputTokens: stats.reduce((sum, s) => sum + (s.totalInputTokens || 0), 0),
      totalOutputTokens: stats.reduce((sum, s) => sum + (s.totalOutputTokens || 0), 0),
      totalTokens: stats.reduce((sum, s) => sum + (s.totalInputTokens || 0) + (s.totalOutputTokens || 0), 0),
    };

    this.sendData('executionStatsUpdate', { stats, orchestratorStats, modelCatalog });
  }

  private buildModelCatalog(): { id: string; label: string; model?: string; provider?: string; enabled?: boolean; role?: 'worker' | 'orchestrator' | 'auxiliary' | 'unknown' }[] {
    try {
      const { LLMConfigLoader } = require('../llm/config');
      const fullConfig = LLMConfigLoader.loadFullConfig();
      const entries: { id: string; label: string; model?: string; provider?: string; enabled?: boolean; role?: 'worker' | 'orchestrator' | 'auxiliary' | 'unknown' }[] = [];

      const toLabel = (id: string) => id.charAt(0).toUpperCase() + id.slice(1);
      const addEntry = (id: string, label: string, config: any, role: 'worker' | 'orchestrator' | 'auxiliary') => {
        entries.push({
          id,
          label,
          model: config?.model,
          provider: config?.provider,
          enabled: config?.enabled !== false,
          role,
        });
      };

      if (fullConfig?.workers) {
        for (const [workerId, workerConfig] of Object.entries(fullConfig.workers)) {
          addEntry(workerId, toLabel(workerId), workerConfig, 'worker');
        }
      }

      if (fullConfig?.orchestrator) {
        addEntry('orchestrator', 'Orchestrator', fullConfig.orchestrator, 'orchestrator');
      }

      if (fullConfig?.auxiliary) {
        addEntry('auxiliary', 'Auxiliary', fullConfig.auxiliary, 'auxiliary');
      }

      return entries;
    } catch (error) {
      logger.warn('界面.模型目录.加载失败', { error: (error as Error).message }, LogCategory.UI);
      return [];
    }
  }

  private async handleResetExecutionStats(): Promise<void> {
    const executionStats = this.orchestratorEngine.getExecutionStats();
    if (!executionStats) {
      return;
    }
    await executionStats.clearStats();
    this.orchestratorEngine.resetOrchestratorTokenUsage();
    this.adapterFactory.resetAllTokenUsage();
    this.sendExecutionStats();
    this.sendToast(t('toast.statsReset'), 'info');
  }

  /** 处理设置交互模式 */
  private handleSetInteractionMode(mode: import('../types').InteractionMode): void {
    if (mode !== 'ask' && mode !== 'auto') {
      logger.error('界面.交互_模式.非法值', { mode }, LogCategory.UI);
      this.sendToast(t('toast.invalidMode'), 'error');
      return;
    }

    const currentMode = this.orchestratorEngine.getInteractionMode();
    const changed = currentMode !== mode;

    if (changed) {
      logger.info('界面.交互_模式.变更', { mode }, LogCategory.UI);
      this.orchestratorEngine.setInteractionMode(mode);
      this.interactionModeUpdatedAt = Date.now();
      this.sendToast(t('toast.modeSwitched', { mode: this.getModeDisplayName(mode) }), 'info');
    } else {
      logger.info('界面.交互_模式.保持', { mode }, LogCategory.UI);
      if (!this.interactionModeUpdatedAt) {
        this.interactionModeUpdatedAt = Date.now();
      }
    }

    this.sendData('interactionModeChanged', { mode, updatedAt: this.interactionModeUpdatedAt });
    if (changed) {
      this.sendStateUpdate();
    }
  }

  /** 获取模式显示名称 */
  private getModeDisplayName(mode: import('../types').InteractionMode): string {
    switch (mode) {
      case 'ask': return t('provider.modeAsk');
      case 'auto': return t('provider.modeAuto');
      default: return mode;
    }
  }

  /** 恢复确认回调的 Promise resolver */
  private recoveryConfirmationResolver: ((decision: 'retry' | 'rollback' | 'continue') => void) | null = null;
  private pendingRecoveryRetry = false;
  private pendingRecoveryPrompt: string | null = null;
  private pendingRecoveryContext: PendingRecoveryContext | null = null;
  private pendingExecutionQueue: OrchestratorQueueItem[] = [];
  private orchestratorQueueRunning = false;
  private queuedMessagesBySession: Map<string, QueuedUserTurn[]> = new Map();
  private queuedMessagesDrainRunning = false;
  private processingResetFallbackTimer: NodeJS.Timeout | null = null;

  private cancelProcessingResetFallback(): void {
    if (this.processingResetFallbackTimer) {
      clearTimeout(this.processingResetFallbackTimer);
      this.processingResetFallbackTimer = null;
    }
  }

  private scheduleProcessingResetFallback(reason: string): void {
    this.cancelProcessingResetFallback();
    // 避免在任务完成瞬间强制封口：给迟到的 streaming/update 一个收敛窗口，
    // 仅在处理态异常残留时才执行兜底清理。
    this.processingResetFallbackTimer = setTimeout(() => {
      this.processingResetFallbackTimer = null;
      const processing = this.messageHub.getProcessingState();
      if (
        this.orchestratorEngine.running
        || this.orchestratorQueueRunning
        || this.pendingExecutionQueue.length > 0
        || this.queuedMessagesDrainRunning
      ) {
        return;
      }
      logger.warn('界面.消息.处理态_延迟兜底清理', {
        reason,
        backendProcessing: processing.isProcessing,
        source: processing.source,
        agent: processing.agent,
      }, LogCategory.UI);
      this.messageHub.forceProcessingState(false);
    }, 1500);
  }

  /** 处理恢复确认 */
  private async handleRecoveryConfirmation(decision: 'retry' | 'rollback' | 'continue'): Promise<void> {
    logger.info('界面.编排器.恢复.决策', { decision }, LogCategory.UI);
    if (this.recoveryConfirmationResolver) {
      this.recoveryConfirmationResolver(decision);
      this.recoveryConfirmationResolver = null;
      return;
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
      return;
    }

    if (decision === 'retry') {
      if (this.orchestratorEngine.running) {
        this.pendingRecoveryRetry = true;
        this.pendingRecoveryPrompt = t('provider.resumePrompt.defaultRetry');
        logger.warn('界面.编排器.恢复.重试_延迟_引擎运行中', undefined, LogCategory.UI);
        this.sendToast(t('toast.taskStillRunning'), 'info');
        return;
      }
      await this.resumeInterruptedTask({ extraInstruction: t('provider.resumePrompt.defaultRetry') });
      return;
    }

    this.clearPendingRecoveryState();
    this.sendToast(t('toast.continueWithoutRollback'), 'info');
  }

  private async tryResumePendingRecovery(): Promise<void> {
    if (!this.pendingRecoveryRetry) return;
    if (this.orchestratorEngine.running) return;
    const prompt = this.pendingRecoveryPrompt || t('provider.resumePrompt.defaultRetry');
    this.pendingRecoveryRetry = false;
    this.pendingRecoveryPrompt = null;
    logger.info('界面.编排器.恢复.重试_触发', undefined, LogCategory.UI);
    await this.resumeInterruptedTask({ extraInstruction: prompt });
  }

  private enqueueOrchestratorExecution(
    prompt: string,
    imagePaths: string[],
    sessionId: string,
    turnId: string
  ): Promise<OrchestratorExecutionResult> {
    return new Promise((resolve) => {
      this.pendingExecutionQueue.push({ prompt, imagePaths, sessionId, turnId, resolve });
      if (this.orchestratorQueueRunning) {
        return;
      }
      this.orchestratorQueueRunning = true;
      void this.processOrchestratorQueue();
    });
  }

  private async processOrchestratorQueue(): Promise<void> {
    try {
      while (this.pendingExecutionQueue.length > 0) {
        while (this.orchestratorEngine.running) {
          await new Promise(resolve => setTimeout(resolve, 200));
        }
        const next = this.pendingExecutionQueue.shift();
        if (!next) {
          continue;
        }
        logger.info('界面.编排器.排队执行_触发', { queueRemaining: this.pendingExecutionQueue.length }, LogCategory.UI);
        try {
          const result = await this.executeWithOrchestrator(next.prompt, next.imagePaths, next.sessionId, next.turnId);
          next.resolve(result);
        } catch (error) {
          const errorMsg = error instanceof Error ? error.message : String(error);
          next.resolve({
            success: false,
            taskId: '',
            runtimeReason: 'failed',
            errors: [errorMsg],
            recoverable: false,
            error: errorMsg,
          });
        }
      }
    } finally {
      this.orchestratorQueueRunning = false;
    }
  }

  private cancelPendingOrchestratorQueue(reason: string): void {
    if (this.pendingExecutionQueue.length === 0) {
      return;
    }
    const pending = [...this.pendingExecutionQueue];
    this.pendingExecutionQueue = [];
    for (const item of pending) {
      item.resolve({
        success: false,
        taskId: '',
        runtimeReason: 'cancelled',
        errors: [reason],
        recoverable: false,
        error: reason,
      });
    }
    logger.warn('界面.编排器.排队任务.已清理', {
      reason,
      count: pending.length,
    }, LogCategory.UI);
  }

  private resolveActiveSessionId(): string {
    return this.activeSessionId || this.sessionManager.getCurrentSession()?.id || '';
  }

  private getQueuedMessages(sessionId: string, createIfMissing = false): QueuedUserTurn[] {
    let queue = this.queuedMessagesBySession.get(sessionId);
    if (!queue && createIfMissing) {
      queue = [];
      this.queuedMessagesBySession.set(sessionId, queue);
    }
    return queue || [];
  }

  private sendQueuedMessagesUpdate(sessionId?: string): void {
    const resolvedSessionId = (sessionId || this.resolveActiveSessionId()).trim();
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

  private handleUpdateQueuedMessage(queueId: string, content: string): void {
    const id = queueId.trim();
    const trimmedContent = content.trim();
    if (!id) {
      return;
    }
    if (!trimmedContent) {
      this.sendToast(t('toast.supplementEmpty'), 'warning');
      return;
    }
    const sessionId = this.resolveActiveSessionId();
    if (!sessionId) {
      return;
    }
    const queue = this.getQueuedMessages(sessionId, false);
    const target = queue.find((item) => item.id === id);
    if (!target) {
      return;
    }
    target.content = trimmedContent;
    this.sendQueuedMessagesUpdate(sessionId);
    void this.drainQueuedMessagesIfIdle();
  }

  private handleDeleteQueuedMessage(queueId: string): void {
    const id = queueId.trim();
    if (!id) {
      return;
    }
    const sessionId = this.resolveActiveSessionId();
    if (!sessionId) {
      return;
    }
    const queue = this.getQueuedMessages(sessionId, false);
    if (queue.length === 0) {
      return;
    }
    const nextQueue = queue.filter((item) => item.id !== id);
    if (nextQueue.length === queue.length) {
      return;
    }
    if (nextQueue.length === 0) {
      this.queuedMessagesBySession.delete(sessionId);
    } else {
      this.queuedMessagesBySession.set(sessionId, nextQueue);
    }
    this.sendQueuedMessagesUpdate(sessionId);
    void this.drainQueuedMessagesIfIdle();
  }

  private async drainQueuedMessagesIfIdle(): Promise<void> {
    const sessionId = this.resolveActiveSessionId();
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
    logger.info('界面.消息.暂存队列.开始出队', {
      sessionId,
      pending: initialQueue.length,
    }, LogCategory.UI);

    try {
      while (true) {
        if (this.orchestratorEngine.running) {
          break;
        }
        if (this.resolveActiveSessionId() !== sessionId) {
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
        logger.info('界面.消息.暂存队列.执行', {
          sessionId,
          queueId: next.id,
          remaining: this.getQueuedMessages(sessionId, false).length,
        }, LogCategory.UI);
        await this.executeTask(next.content, undefined, [], requestId);
      }
    } finally {
      this.queuedMessagesDrainRunning = false;
      const remaining = this.getQueuedMessages(sessionId, false).length;
      if (remaining > 0 && !this.orchestratorEngine.running && this.resolveActiveSessionId() === sessionId) {
        void this.drainQueuedMessagesIfIdle();
      }
    }
  }

  /** 在 VS Code 原生 diff 视图中打开文件变更（类似 Augment） */
  private async openVscodeDiff(filePath: string): Promise<void> {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      this.sendToast(t('toast.noActiveSession'), 'warning');
      return;
    }

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) {
      this.sendToast(t('toast.snapshotNotFound'), 'warning');
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
      const tempDir = path.join(os.tmpdir(), 'magi-diff');
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
      const title = t('provider.diffTitle', { fileName });
      await vscode.commands.executeCommand('vscode.diff', originalUri, modifiedUri, title);

      // 监听 diff 标签页关闭后再清理临时文件
      const disposable = vscode.workspace.onDidCloseTextDocument(doc => {
        if (doc.uri.fsPath === tempFile) {
          disposable.dispose();
          try {
            if (fs.existsSync(tempFile)) {
              fs.unlinkSync(tempFile);
            }
          } catch (e) {
            // 忽略清理错误
          }
        }
      });

    } catch (error) {
      logger.error('界面.差异.打开_失败', error, LogCategory.UI);
      this.sendToast(t('toast.diffViewFailed'), 'error');
    }
  }

  /** 在编辑器中打开文件（从代码块点击文件路径） */
  private async openFileInEditor(filepath: string): Promise<void> {
    if (!filepath) {
      return;
    }

    try {
      // 处理相对路径和绝对路径
      const resolved = this.workspaceRoots.resolvePath(filepath, { mustExist: true });
      const absolutePath = resolved?.absolutePath || '';

      // 检查文件是否存在
      if (!fs.existsSync(absolutePath)) {
        this.sendToast(t('toast.fileNotExists', { filepath }), 'warning');
        return;
      }

      const uri = vscode.Uri.file(absolutePath);
      const stat = fs.statSync(absolutePath);
      if (stat.isDirectory()) {
        await vscode.commands.executeCommand('revealInExplorer', uri);
        return;
      }

      // 打开文件
      const document = await vscode.workspace.openTextDocument(uri);
      await vscode.window.showTextDocument(document, {
        preview: false,
        preserveFocus: false
      });
    } catch (error) {
      logger.error('界面.文件.打开_失败', error, LogCategory.UI);
      this.sendToast(t('toast.openFileFailed', { filepath }), 'error');
    }
  }

  private resolveOpenFilePath(message: Extract<WebviewToExtensionMessage, { type: 'openFile' }>): string | null {
    const candidates = [message.filepath, message.filePath];
    for (const candidate of candidates) {
      if (typeof candidate === 'string' && candidate.trim().length > 0) {
        return candidate.trim();
      }
    }
    return null;
  }

  /** 清理所有任务（统一使用 Mission 系统） */
  private async handleClearAllTasks(): Promise<void> {
    const sessionId = this.activeSessionId;
    if (!sessionId) {
      this.sendToast(t('toast.noActiveSession'), 'warning');
      return;
    }

    // 检查是否有正在运行的任务
    if (this.orchestratorEngine.running) {
      this.sendToast(t('toast.taskRunning'), 'warning');
      return;
    }

    // 统一 Todo 系统：从 Mission 获取并清理任务
    const taskViews = await this.getTaskViews();
    const taskCount = taskViews.length;

    // 删除所有 Mission（使用 deleteTaskById 方法）
    for (const tv of taskViews) {
      if (tv.missionId) {
        await this.orchestratorEngine.deleteTaskById(tv.missionId);
      }
    }

    this.sendToast(t('toast.tasksCleaned', { count: taskCount }), 'success');
    this.sendStateUpdate();
  }

  private async handleStartTask(taskId?: string): Promise<void> {
    if (!taskId) {
      this.sendToast(t('toast.missingTaskId'), 'error');
      return;
    }
    try {
      // 先通知用户任务正在启动
      this.sendToast(t('toast.taskStarting'), 'info');
      this.sendStateUpdate();
      // 触发完整执行链路（意图分析 → 规划 → 执行）
      await this.orchestratorEngine.startTaskById(taskId);
      this.sendStateUpdate();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendToast(t('toast.startFailed', { error: errorMsg }), 'error');
      this.sendStateUpdate();
    }
  }

  private async handleDeleteTask(taskId?: string): Promise<void> {
    if (!taskId) {
      this.sendToast(t('toast.missingTaskId'), 'error');
      return;
    }
    try {
      // 统一 Todo 系统 - 使用 orchestratorEngine
      await this.orchestratorEngine.deleteTaskById(taskId);
      this.sendToast(t('toast.taskDeleted'), 'success');
      this.sendStateUpdate();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendToast(t('toast.deleteFailed', { error: errorMsg }), 'error');
    }
  }

  private clearPendingRecoveryState(): void {
    this.pendingRecoveryRetry = false;
    this.pendingRecoveryPrompt = null;
    this.pendingRecoveryContext = null;
  }

  private isAbortLikeRuntimeReason(runtimeReason?: OrchestratorRuntimeReason): boolean {
    return runtimeReason === 'interrupted'
      || runtimeReason === 'cancelled'
      || runtimeReason === 'external_abort';
  }

  private isRecoverableRuntimeReason(runtimeReason: OrchestratorRuntimeReason): boolean {
    return runtimeReason === 'interrupted' || runtimeReason === 'upstream_model_error';
  }

  private buildExecutionFailureReason(input: {
    runtimeReason: OrchestratorRuntimeReason;
    errors: string[];
    error?: string;
  }): string {
    const normalizedError = [input.error, ...input.errors]
      .find((item): item is string => typeof item === 'string' && item.trim().length > 0)
      ?.trim();
    switch (input.runtimeReason) {
      case 'interrupted':
        return t('provider.taskAborted');
      case 'cancelled':
      case 'external_abort':
        return t('provider.userCancelled');
      default:
        return normalizedError || t('provider.executionFailed');
    }
  }

  private buildRecoveryErrorMessage(context: PendingRecoveryContext): string {
    const normalizedError = context.errors
      .find((item): item is string => typeof item === 'string' && item.trim().length > 0)
      ?.trim();
    if (context.runtimeReason === 'upstream_model_error') {
      return this.buildModelOriginIssueMessage(normalizedError || t('provider.executionFailed'));
    }
    if (context.runtimeReason === 'interrupted') {
      return t('provider.taskAborted');
    }
    return normalizedError || t('provider.executionFailed');
  }

  private setPendingRecoveryFromExecution(input: {
    result: OrchestratorExecutionResult;
    prompt: string;
    sessionId: string;
  }): void {
    if (!input.result.recoverable) {
      this.pendingRecoveryContext = null;
      return;
    }

    const taskId = input.result.taskId.trim();
    const prompt = input.prompt.trim();
    if (!taskId || !prompt) {
      this.pendingRecoveryContext = null;
      logger.warn('界面.恢复.上下文_缺少关键字段', {
        taskId,
        sessionId: input.sessionId,
        runtimeReason: input.result.runtimeReason,
      }, LogCategory.UI);
      return;
    }

    const context: PendingRecoveryContext = {
      taskId,
      prompt,
      sessionId: input.sessionId,
      runtimeReason: input.result.runtimeReason,
      errors: [...input.result.errors],
      canRetry: true,
      canRollback: this.snapshotManager.getPendingChanges().length > 0,
    };
    this.pendingRecoveryContext = context;
    this.sendData('recoveryRequest', {
      taskId: context.taskId,
      error: this.buildRecoveryErrorMessage(context),
      canRetry: context.canRetry,
      canRollback: context.canRollback,
    });
  }

  private getPendingRecoveryContext(taskId?: string): PendingRecoveryContext | null {
    const context = this.pendingRecoveryContext;
    if (!context) {
      return null;
    }
    if (!taskId) {
      return context;
    }
    return context.taskId === taskId ? context : null;
  }

  /** 构建恢复提示词 */
  private buildResumePrompt(originalPrompt: string, extraInstruction?: string): string {
    const pendingChanges = this.snapshotManager.getPendingChanges();
    const changeList = pendingChanges.length
      ? pendingChanges.map(c => `- ${c.filePath} (+${c.additions}/-${c.deletions})`).join('\n')
      : t('provider.resumePrompt.pendingChangesNone');

    const extra = extraInstruction
      ? `\n\n${t('provider.resumePrompt.extraInstruction', { instruction: extraInstruction })}`
      : '';

    return [
      t('provider.resumePrompt.header'),
      t('provider.resumePrompt.originalRequest', { prompt: originalPrompt }),
      t('provider.resumePrompt.generatedChanges', { changes: changeList }) + extra,
    ].join('\n\n');
  }

  /**
   * 构建中断摘要（写回会话，供下一轮上下文注入）
   */
  private buildInterruptionSummary(tasks: TaskView[]): string {
    const lines: string[] = [t('provider.interruptionSummary.header')];

    for (const task of tasks) {
      const subTasks = task.subTasks || [];
      const completed = subTasks.filter(st => st.status === 'completed').length;
      const failed = subTasks.filter(st => st.status === 'failed').length;
      const running = subTasks.filter(st => st.status === 'running').length;
      const pending = subTasks.filter(st => st.status === 'pending' || st.status === 'blocked').length;
      const taskName = task.goal || task.prompt || task.id;
      lines.push(t('provider.interruptionSummary.taskLine', { taskName }));
      lines.push(t('provider.interruptionSummary.subTaskStatus', { completed, failed, running, pending }));
    }

    const pendingChanges = this.snapshotManager.getPendingChanges();
    if (pendingChanges.length > 0) {
      lines.push(t('provider.interruptionSummary.pendingChangesHeader'));
      for (const change of pendingChanges.slice(0, 20)) {
        lines.push(`  - ${change.filePath} (+${change.additions}/-${change.deletions})`);
      }
      if (pendingChanges.length > 20) {
        lines.push(t('provider.interruptionSummary.pendingChangesMore', { count: pendingChanges.length - 20 }));
      }
    } else {
      lines.push(t('provider.interruptionSummary.pendingChangesNone'));
    }

    lines.push(t('provider.interruptionSummary.footer'));
    return lines.join('\n');
  }

  /** 恢复被打断的任务 */
  private async resumeInterruptedTask(options?: { taskId?: string; extraInstruction?: string }): Promise<void> {
    if (this.orchestratorEngine.running) {
      this.sendToast(t('toast.taskStillExecuting'), 'warning');
      return;
    }

    const recoveryContext = this.getPendingRecoveryContext(options?.taskId);
    if (!recoveryContext) {
      this.sendToast(t('toast.noRecoverableTasks'), 'info');
      return;
    }

    const prompt = this.buildResumePrompt(recoveryContext.prompt, options?.extraInstruction);
    this.sendOrchestratorMessage({
      content: t('provider.resumingTask'),
      messageType: 'progress',
      metadata: { phase: 'resuming' },
    });
    await this.executeTask(prompt, undefined, [], undefined, undefined, {
      resumeMissionId: recoveryContext.taskId,
      resumeInstruction: prompt,
      recoveryBasePrompt: recoveryContext.prompt,
    });
  }

  /** 处理执行中追加输入：运行中进入暂存队列，轮次结束后按 FIFO 自动续跑 */
  private async handleAppendMessage(taskId: string, content: string): Promise<void> {
    logger.info('界面.消息.补充.请求', { taskId, preview: content.substring(0, 50) }, LogCategory.UI);

    const trimmedContent = content.trim();
    if (!trimmedContent) {
      this.sendToast(t('toast.supplementEmpty'), 'warning');
      return;
    }

    try {
      const sessionId = this.resolveActiveSessionId();
      if (!sessionId) {
        this.sendToast(t('toast.noActiveSession'), 'warning');
        return;
      }

      const shouldQueue = this.orchestratorEngine.running
        || this.orchestratorQueueRunning
        || this.pendingExecutionQueue.length > 0
        || this.queuedMessagesDrainRunning;

      if (shouldQueue) {
        const queued = this.enqueueQueuedMessage(sessionId, trimmedContent);
        this.sendQueuedMessagesUpdate(sessionId);
        logger.info('界面.消息.暂存队列.入队', {
          taskId,
          sessionId,
          queueId: queued.id,
          pending: this.getQueuedMessages(sessionId, false).length,
        }, LogCategory.UI);
        return;
      }

      // 空闲状态下直接作为新轮次执行
      const requestId = `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      await this.executeTask(trimmedContent, undefined, [], requestId);
      logger.info('界面.消息.补充.空闲直执_成功', { taskId, sessionId }, LogCategory.UI);
    } catch (error) {
      logger.error('界面.消息.补充.失败', error, LogCategory.UI);
      this.sendToast(t('toast.supplementFailed'), 'error');
    }
  }

  /** 处理设置更新 */
  private async handleSettingUpdate(key: string, value: unknown): Promise<void> {
    const config = vscode.workspace.getConfiguration('magi');

    if (key === 'locale') {
      const locale = this.normalizeLocaleCode(value);
      this.locale = locale;
      setExtensionLocale(locale);
      ConfigManager.getInstance().set('locale', locale);
      ConfigManager.getInstance().save();
      return; // 语言切换即时生效，不显示通用“设置已保存”提示
    }
    // 处理其他配置
    else if (key === 'autoSnapshot') {
      config.update('autoSnapshot', value, vscode.ConfigurationTarget.Global);
    }
    else if (key === 'timeout') {
      config.update('timeout', parseInt(value as string, 10), vscode.ConfigurationTarget.Global);
    }
    else if (key === 'deepTask') {
      const enabled = Boolean(value);
      config.update('deepTask', enabled, vscode.ConfigurationTarget.Global);
      // 回推确认状态给前端
      this.sendData('deepTaskChanged', { enabled });
      logger.info('界面.设置.深度任务', { enabled }, LogCategory.UI);
      return; // deepTask 切换不需要通用 toast（前端已有 toast）
    }

    this.sendToast(t('toast.settingsSaved'), 'success');
  }

  private normalizeLocaleCode(value: unknown, fallback: LocaleCode = this.locale): LocaleCode {
    if (value === 'zh-CN' || value === 'en-US') {
      return value;
    }
    return fallback;
  }

  /** 执行任务（统一走智能编排） */
  private async executeTask(
    prompt: string,
    _reserved?: unknown,
    images?: Array<{ dataUrl: string }>,
    requestId?: string,
    displayPrompt?: string,
    options?: {
      resumeMissionId?: string;
      resumeInstruction?: string;
      recoveryBasePrompt?: string;
    }
  ): Promise<void> {
    logger.info('界面.任务.执行.开始', { promptLength: prompt.length, imageCount: images?.length || 0 }, LogCategory.UI);
    const maxPromptLength = 10000;
    const requestKey = requestId || `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    let started = false;
    let rejected = false;
    let success = false;
    let failureReason: string | undefined;
    let executionSessionId = '';
    let promptForRecovery = displayPrompt?.trim() || prompt;
    let executionResult: OrchestratorExecutionResult | null = null;

    const rejectRequest = (reason: string) => {
      rejected = true;
      failureReason = reason;
      if (requestKey) {
        this.messageHub.taskRejected(requestKey, reason);
        const traceId = this.messageHub.getTraceId();
        const boundMessageId = this.messageHub.getRequestMessageId(requestKey);
        const errorMessage = createErrorMessage(
          reason,
          'orchestrator',
          'orchestrator',
          traceId,
          {
            id: boundMessageId,
            metadata: { requestId: requestKey },
          }
        );
        this.messageHub.sendMessage(errorMessage);
      }
      this.clearRequestTimeout(requestKey);
    };

    try {
      this.cancelProcessingResetFallback();
      this.messageHub.setRequestContext(requestKey);
      if (options?.resumeMissionId) {
        const activated = this.orchestratorEngine.activateWorkerSessionResume(
          options.resumeMissionId,
          options.resumeInstruction
        );
        logger.info('界面.任务.恢复上下文.激活', {
          missionId: options.resumeMissionId,
          activated,
        }, LogCategory.UI);
      } else {
        this.orchestratorEngine.clearWorkerSessionResume();
      }

      // 📝 长度验证逻辑：
      // - 普通用户输入：验证 prompt 长度（防止粘贴过长内容）
      // - Skill 调用：displayPrompt 存在且与 prompt 不同时，只验证 displayPrompt 长度
      //   （Skill 指令内容由系统生成，可能很长，不应受用户输入限制）
      const isSkillInvocation = displayPrompt && displayPrompt !== prompt;
      const lengthToValidate = isSkillInvocation ? displayPrompt.length : prompt.length;

      if (lengthToValidate > maxPromptLength) {
        const displayLength = isSkillInvocation ? displayPrompt.length : prompt.length;
        this.sendToast(t('input.inputTooLong', { length: displayLength, max: maxPromptLength }), 'warning');
        rejectRequest(t('provider.inputTooLong', { length: displayLength }));
        return;
      }

      if (!this.activeSessionId) {
        const currentSession = this.sessionManager.getCurrentSession();
        this.activeSessionId = currentSession?.id || null;
        logger.info('界面.会话.当前.设置', { sessionId: this.activeSessionId }, LogCategory.UI);
      }
      executionSessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id || '';
      if (!executionSessionId) {
        rejectRequest(t('provider.sessionNotFound'));
        return;
      }
      this.activeSessionId = executionSessionId;
      this.syncMessageHubTrace(executionSessionId);

      // 统一消息通道：由后端发送用户消息与占位消息
      const promptForDisplay = displayPrompt?.trim() || prompt;
      promptForRecovery = options?.recoveryBasePrompt?.trim() || promptForDisplay;
      const executionTurnId = `turn:${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
      const { userMessageId } = this.emitUserAndPlaceholder(
        requestKey,
        promptForDisplay,
        images?.length || 0,
        images,
        undefined,
        executionTurnId
      );
      this.scheduleRequestTimeout(requestKey);

      // 🔧 性能优化：强制让出事件循环 (Yield Event Loop)
      // 原因：emitUserAndPlaceholder 只是将消息入队，实际的 webview.postMessage 需要事件循环 Tick 才能执行。
      // 如果不在此处让出控制权，后续的同步 FS 操作（图片保存）和 Orchestrator 初始化会阻塞主线程，
      // 导致前端迟迟收不到用户消息的回显，造成"点击发送后卡顿"的假象。
      await new Promise(resolve => setTimeout(resolve, 0));

      // 如果有图片，保存到临时文件
      const imagePaths: string[] = [];
      if (images && images.length > 0) {
        const tmpDir = path.join(os.tmpdir(), 'magi-images');
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

      // 任务开始：发送控制消息，由 MessageHub 按真实消息生命周期驱动处理态
      started = true;
      if (requestKey) {
        this.messageHub.taskAccepted(requestKey);
      }
      this.messageHub.sendControl(ControlMessageType.TASK_STARTED, {
        requestId: requestKey,
        timestamp: Date.now(),
      });
      this.clearPendingRecoveryState();

      const resolvedSkill = this.resolveInstructionSkillPrompt(prompt);
      const effectivePrompt = resolvedSkill.prompt;

      try {
        this.sessionManager.addMessageToSession(executionSessionId, 'user', prompt, undefined, 'orchestrator', images, {
          id: userMessageId,
          type: 'user_input',
          metadata: {
            turnId: executionTurnId,
            requestId: requestKey,
          },
        });
      } catch (error: any) {
        rejectRequest(t('provider.sessionWriteFailed', { error: error?.message || String(error) }));
        return;
      }
      void this.orchestratorEngine.recordContextMessage('user', prompt, executionSessionId);
      this.sendStateUpdate();

      // 统一走智能编排模式
      const result = await this.enqueueOrchestratorExecution(
        effectivePrompt,
        imagePaths,
        executionSessionId,
        executionTurnId
      );
      executionResult = result;
      success = result.success;
      failureReason = result.error;
    } catch (error) {
      failureReason = error instanceof Error ? error.message : String(error);
      success = false;
    } finally {
      // 🔧 移除基于统计的误判检查
      // 原因：LLM 流式输出通过 Normalizer -> MessageHub 直接发送到前端
      // 但统计机制依赖 requestId 注入，流式消息可能在 requestId 设置前就开始发送
      // 导致 assistantContent 统计为 0，误判为"未产生响应"
      //
      // 实际行为：如果 LLM 有输出，消息已经通过流式通道发送到 UI
      // 如果确实需要检测消息通道故障，应该在 Adapter/Normalizer 层面实现
      const requestStats = requestKey ? this.messageHub.getRequestMessageStats(requestKey) : undefined;
      if (requestKey) {
        const stats = requestStats ?? {
          assistantContent: 0,
          assistantThreadContent: 0,
          assistantWorkerContent: 0,
          assistantDispatchContent: 0,
          userContent: 0,
          placeholderContent: 0,
          totalContent: 0,
          dataCount: 0,
        };
        logger.info('界面.消息.通道_统计', {
          requestId: requestKey,
          success,
          rejected,
          assistantContent: stats.assistantContent,
          assistantThreadContent: stats.assistantThreadContent,
          assistantWorkerContent: stats.assistantWorkerContent,
          assistantDispatchContent: stats.assistantDispatchContent,
          userContent: stats.userContent,
          placeholderContent: stats.placeholderContent,
          totalContent: stats.totalContent,
          dataCount: stats.dataCount,
          statsMissing: !requestStats,
        }, LogCategory.UI);

        // 不再使用 assistantContent 统计做成败硬判定。
        // 请求是否成功以执行链路返回值(success/failureReason)为准，
        // 流式可见性由 MessageHub 消息生命周期统一驱动。
      }
      if (started) {
        // 判断是否为中断导致的失败——中断场景不应发送错误消息
        const runtimeReason = executionResult?.runtimeReason;
        const isAbort = !success && (this.isAbortLikeRuntimeReason(runtimeReason) || (failureReason ? isAbortError(failureReason) : false));
        const normalizedFailureReason = (failureReason || t('provider.executionFailed')).trim();
        const modelOriginIssue = runtimeReason === 'upstream_model_error'
          || this.isLikelyModelOriginIssue(normalizedFailureReason);
        const userFacingFailureReason = modelOriginIssue
          ? this.buildModelOriginIssueMessage(normalizedFailureReason)
          : normalizedFailureReason;
        if (success) {
          this.messageHub.sendControl(ControlMessageType.TASK_COMPLETED, {
            requestId: requestKey,
            timestamp: Date.now(),
          });
        } else if (isAbort) {
          // 中断场景：仅发送 TASK_FAILED 控制消息用于状态流转，不发送用户可见的错误消息
          this.messageHub.sendControl(ControlMessageType.TASK_FAILED, {
            requestId: requestKey,
            error: t('provider.taskAborted'),
            timestamp: Date.now(),
          });
        } else {
          this.messageHub.sendControl(ControlMessageType.TASK_FAILED, {
            requestId: requestKey,
            error: userFacingFailureReason,
            timestamp: Date.now(),
          });
          if (!rejected && normalizedFailureReason) {
            if (modelOriginIssue) {
              trackModelOriginEvent('surfaced', 'ui:executeTask', normalizedFailureReason, {
                requestId: requestKey,
              });
              logger.warn('界面.执行.模型调用异常', {
                requestId: requestKey,
                reason: normalizedFailureReason,
              }, LogCategory.UI);
              this.messageHub.notify(t('provider.modelAnomalyDetected'), 'warning');
              this.messageHub.orchestratorMessage(userFacingFailureReason, {
                metadata: {
                  requestId: requestKey,
                  phase: 'model_origin_issue',
                  recoverable: true,
                },
              });
            } else {
              const traceId = this.messageHub.getTraceId();
              const errorMessage = createErrorMessage(
                normalizedFailureReason,
                'orchestrator',
                'orchestrator',
                traceId,
                { metadata: { requestId: requestKey } }
              );
              this.messageHub.sendMessage(errorMessage);
            }
          }
        }
        if (executionResult) {
          this.setPendingRecoveryFromExecution({
            result: executionResult,
            prompt: promptForRecovery,
            sessionId: executionSessionId,
          });
        }
      } else if (!rejected && failureReason && requestKey) {
        const normalizedFailureReason = failureReason.trim();
        const modelOriginIssue = this.isLikelyModelOriginIssue(normalizedFailureReason);
        const userFacingFailureReason = modelOriginIssue
          ? this.buildModelOriginIssueMessage(normalizedFailureReason)
          : normalizedFailureReason;
        this.messageHub.taskRejected(requestKey, userFacingFailureReason);
        const traceId = this.messageHub.getTraceId();
        const boundMessageId = this.messageHub.getRequestMessageId(requestKey);
        const errorMessage = createErrorMessage(
          userFacingFailureReason,
          'orchestrator',
          'orchestrator',
          traceId,
          {
            id: boundMessageId,
            metadata: {
              requestId: requestKey,
              ...(modelOriginIssue ? { phase: 'model_origin_issue', recoverable: true } : {}),
            },
          }
        );
        this.messageHub.sendMessage(errorMessage);
        if (modelOriginIssue) {
          trackModelOriginEvent('surfaced', 'ui:taskRejected', normalizedFailureReason, {
            requestId: requestKey,
          });
          logger.warn('界面.执行.模型调用异常.任务拒绝', {
            requestId: requestKey,
            reason: normalizedFailureReason,
          }, LogCategory.UI);
          this.messageHub.notify(t('provider.modelAnomalyDetected'), 'warning');
        }
      }
      this.messageHub.finalizeRequestContext(requestKey);
      this.messageHub.setRequestContext(undefined);
      this.clearRequestTimeout(requestKey);
      this.orchestratorEngine.clearWorkerSessionResume();
      this.scheduleProcessingResetFallback(`executeTask:${requestKey}`);
      void this.drainQueuedMessagesIfIdle();
    }
  }

  private isLikelyModelOriginIssue(reason: string): boolean {
    return isModelOriginIssue(reason);
  }

  private buildModelOriginIssueMessage(rawReason: string): string {
    const userMessage = toModelOriginUserMessage(rawReason).trim();
    return userMessage || t('provider.modelOriginFallback');
  }

  private resolveInstructionSkillPrompt(prompt: string): { prompt: string; skillName?: string } {
    const trimmed = prompt.trim();
    if (!trimmed) {
      return { prompt };
    }

    const match = trimmed.match(/^\/([^\s]+)(\s+([\s\S]*))?$/);
    if (!match) {
      return { prompt };
    }

    const skillName = match[1];
    const args = (match[3] || '').trim();

    try {
      const { LLMConfigLoader } = require('../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig();
      const skills: InstructionSkillDefinition[] = Array.isArray(config?.instructionSkills) ? config.instructionSkills : [];
      const skill = skills.find((item) => item.name === skillName);
      if (!skill) {
        return { prompt };
      }
      const mergedPrompt = buildInstructionSkillPrompt(skill, args);
      return { prompt: mergedPrompt, skillName: skill.name };
    } catch (error: any) {
      logger.warn('Failed to resolve instruction skill', { error: error.message }, LogCategory.TOOLS);
      return { prompt };
    }
  }

  /** 编排模式执行 */
  private async executeWithOrchestrator(
    prompt: string,
    imagePaths: string[],
    sessionId: string,
    turnId: string
  ): Promise<OrchestratorExecutionResult> {
    logger.info('界面.执行.模式.编排', undefined, LogCategory.UI);

    // 🔧 初始分析消息已由 MissionDrivenEngine.sendPhaseMessage 统一发送
    // 不再在这里重复发送，避免用户看到两条类似的"正在分析"消息

    let errorMsg: string | undefined;
    let taskId = '';
    let thrown = false;
    try {
      // 调用智能编排器
      // 注意：executeWithTaskContext 内部已将 LLM 响应流式发送到前端，
      // 引擎层同时负责通过 messageHub.result() 自主推送所有 [System] 带外段，
      // 因此不需要在此层做任何回灌或白名单匹配。
      const taskContext = await this.orchestratorEngine.executeWithTaskContext(prompt, sessionId, imagePaths, turnId);
      taskId = taskContext.taskId.trim();
      const result = taskContext.result;

      logger.info('界面.任务.完成', { hasResult: !!result?.trim(), resultLength: result?.length || 0 }, LogCategory.UI);

      // 保存消息历史
      this.saveMessageToSession(prompt, result, undefined, 'orchestrator', sessionId);

      // 🔧 移除误判的安全检查
      // 原因：LLM 流式输出通过 Normalizer -> MessageHub 直接发送到前端
      // 统计机制依赖 requestId 注入，但流式消息可能在 requestId 设置前就开始发送
      // 导致 assistantThreadContent 统计为 0，误触发强制补发，产生重复消息
      //
      // 正确的行为：
      // - 如果 LLM 有输出（result 非空），消息已经通过流式通道发送
      // - 如果 LLM 无输出（result 为空），应该在 engine 层面处理，而非此处补发
    } catch (error) {
      thrown = true;
      // 中断导致的 abort 错误静默处理，不向前端发送错误消息
      if (isAbortError(error)) {
        logger.info('界面.执行.智能.中断', undefined, LogCategory.UI);
      } else {
        logger.error('界面.执行.智能.失败', error, LogCategory.UI);
        errorMsg = error instanceof Error ? error.message : String(error);
      }
    }

    const executionStatus = this.orchestratorEngine.getLastExecutionStatus();
    const runtimeReason = thrown && executionStatus.success
      ? 'failed'
      : executionStatus.runtimeReason;
    const success = !thrown && executionStatus.success;
    const errors = success
      ? []
      : (runtimeReason === executionStatus.runtimeReason
        ? [...executionStatus.errors]
        : (errorMsg ? [errorMsg] : []));
    const failureReason = success
      ? undefined
      : this.buildExecutionFailureReason({
        runtimeReason,
        errors,
        error: errorMsg,
      });

    this.sendStateUpdate();
    return {
      success,
      error: failureReason,
      taskId,
      runtimeReason,
      errors,
      recoverable: this.isRecoverableRuntimeReason(runtimeReason),
    };
  }

  /** 发送状态更新到 Webview */
  private sendStateUpdate(): void {
    // 统一 Todo 系统：异步获取 TaskView 列表
    void this.buildUIState().then((state: UIState) => {
      this.sendData('stateUpdate', { state });
    }).catch((err: unknown) => {
      logger.error('界面.状态.构建失败', { error: err instanceof Error ? err.message : String(err) }, LogCategory.UI);
    });
  }

  /** 执行会话删除逻辑（供 deleteSession 消息使用） */
  private async performSessionDelete(sessionId: string): Promise<void> {
    const isDeletingCurrentSession = sessionId === this.activeSessionId;
    this.queuedMessagesBySession.delete(sessionId);

    if (!this.sessionManager.deleteSession(sessionId)) {
      this.sendStateUpdate();
      return;
    }

    const remainingSessions = this.sessionManager.getSessionMetas();

    if (remainingSessions.length === 0) {
      // 没有剩余会话，创建新会话
      const newSession = this.sessionManager.createSession();
      this.activeSessionId = newSession.id;
      this.syncMessageHubTrace(this.activeSessionId);
      this.sendData('sessionCreated', { sessionId: newSession.id, session: newSession });
      this.sendQueuedMessagesUpdate(newSession.id);
    } else if (isDeletingCurrentSession) {
      // 删除的是当前活跃会话，切换到 sessionManager 自动选择的下一个会话
      const nextSessionId = this.sessionManager.getCurrentSession()?.id ?? remainingSessions[0].id;
      await this.switchToSession(nextSessionId);
    }

    this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() });
    this.sendToast(t('toast.sessionDeleted'), 'info');
    this.sendStateUpdate();
  }

  private async sendCurrentSessionToWebview(): Promise<void> {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      return;
    }
    this.sendData('sessionLoaded', { sessionId: session.id, session: session });
    await this.orchestratorEngine.reconcilePlanLedgerForSession(session.id);
    const planSnapshot = this.orchestratorEngine.getPlanLedgerSnapshot(session.id);
    this.sendData('planLedgerLoaded', {
      sessionId: session.id,
      activePlan: planSnapshot.activePlan,
      plans: planSnapshot.plans,
    });
    this.sendQueuedMessagesUpdate(session.id);
    void this.drainQueuedMessagesIfIdle();
  }

  /** 创建并切换到新会话（对齐任务/对话会话） */
  public async createNewSession(): Promise<void> {
    await this.handleNewSession();
  }

  /** 处理新会话创建流程 */
  private async handleNewSession(): Promise<void> {
    // 创建新会话前，先中断当前任务
    await this.interruptCurrentTask({ silent: true });
    this.cancelPendingOrchestratorQueue(t('provider.sessionSwitchedQueueCancelled'));
    // 创建新会话时，重置所有适配器
    await this.adapterFactory.shutdown();
    this.messageHub.setRequestContext(undefined);
    this.cancelProcessingResetFallback();
    this.messageHub.forceProcessingState(false);
    const newSession = this.sessionManager.createSession();
    // 更新活跃会话ID
    this.activeSessionId = newSession.id;
    this.syncMessageHubTrace(this.activeSessionId);
    logger.info('界面.会话.已创建', { sessionId: this.activeSessionId }, LogCategory.UI);
    // 通知 webview 新会话已创建
    this.sendData('sessionCreated', { sessionId: newSession.id, session: newSession });
    this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() });
    this.sendQueuedMessagesUpdate(newSession.id);
    this.sendStateUpdate();
  }

  /** 切换到指定会话 */
  private async switchToSession(sessionId: string): Promise<void> {
    this.cancelPendingOrchestratorQueue(t('provider.sessionSwitchedQueueCancelled'));
    await this.adapterFactory.shutdown();
    this.messageHub.setRequestContext(undefined);
    this.cancelProcessingResetFallback();
    this.messageHub.forceProcessingState(false);
    this.activeSessionId = sessionId;
    this.ensureSessionExists(sessionId);
    this.syncMessageHubTrace(sessionId);

    // 获取会话完整数据
    const session = this.sessionManager.getSession(sessionId);
    if (session) {
      // 先广播会话已切换，再下发消息历史，避免前端跨会话过滤误拦截历史数据
      this.sendData('sessionSwitched', {
        sessionId,
        session,
      });

      // 分类消息：主对话 vs Worker 消息
      const threadMessages: any[] = [];
      const workerMessages: { claude: any[]; codex: any[]; gemini: any[] } = {
        claude: [],
        codex: [],
        gemini: [],
      };

      for (const m of session.messages) {
        if (!m?.id || typeof m.id !== 'string' || !m.id.trim()) {
          throw new Error('Session message missing id');
        }
        if (m.role !== 'user' && m.role !== 'assistant' && m.role !== 'system') {
          throw new Error(`Session message role invalid: ${String(m.role)}`);
        }
        if (typeof m.content !== 'string') {
          throw new Error('Session message content invalid');
        }
        if (typeof m.timestamp !== 'number') {
          throw new Error('Session message timestamp invalid');
        }
        const normalizedSource = this.normalizeSessionMessageSource(m.source, m.agent, m.metadata);
        const formatted = {
          id: m.id,
          role: m.role,
          content: m.content,
          source: normalizedSource.source === 'worker'
            ? (normalizedSource.agent || 'worker')
            : normalizedSource.source,
          timestamp: m.timestamp,
          agent: normalizedSource.agent || m.agent,
          images: Array.isArray(m.images) ? this.cloneSerializable(m.images) : undefined,
          blocks: Array.isArray(m.blocks) ? this.cloneSerializable(m.blocks) : undefined,
          type: typeof m.type === 'string' ? m.type : undefined,
          noticeType: typeof m.noticeType === 'string' ? m.noticeType : undefined,
          isStreaming: typeof m.isStreaming === 'boolean' ? m.isStreaming : undefined,
          isComplete: typeof m.isComplete === 'boolean' ? m.isComplete : undefined,
          metadata: m.metadata && typeof m.metadata === 'object' && !Array.isArray(m.metadata)
            ? this.cloneSerializable(m.metadata)
            : undefined,
        };

        // 根据 source 和 agent 分类
        if (normalizedSource.source === 'worker' && normalizedSource.agent) {
          const agentKey = normalizedSource.agent;
          if (workerMessages[agentKey]) {
            workerMessages[agentKey].push(formatted);
          }
        } else {
          // 主对话消息（orchestrator/system/user）
          threadMessages.push(formatted);
        }
      }

      // 发送完整的会话消息历史给前端（包括 worker 消息）
      this.sendData('sessionMessagesLoaded', {
        sessionId,
        messages: threadMessages,
        workerMessages,
      });

      await this.orchestratorEngine.reconcilePlanLedgerForSession(sessionId);
      const planSnapshot = this.orchestratorEngine.getPlanLedgerSnapshot(sessionId);
      this.sendData('planLedgerLoaded', {
        sessionId,
        activePlan: planSnapshot.activePlan,
        plans: planSnapshot.plans,
      });
      this.sendQueuedMessagesUpdate(sessionId);

      logger.info('界面.会话.消息.已加载', {
        sessionId,
        threadCount: threadMessages.length,
        claudeCount: workerMessages.claude.length,
        codexCount: workerMessages.codex.length,
        geminiCount: workerMessages.gemini.length,
      }, LogCategory.UI);
    }
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

  /** 将 MessageHub trace 与当前会话对齐，确保消息不会跨会话串流 */
  private syncMessageHubTrace(sessionId?: string | null): void {
    const resolvedSessionId = (sessionId && sessionId.trim())
      || this.activeSessionId
      || this.sessionManager.getCurrentSession()?.id
      || '';
    if (!resolvedSessionId) {
      return;
    }
    if (this.messageHub.getTraceId() === resolvedSessionId) {
      return;
    }
    this.messageHub.setTraceId(resolvedSessionId);
    logger.debug('界面.消息.Trace.同步', { traceId: resolvedSessionId }, LogCategory.UI);
  }

  /** 保存消息到当前会话 */
  private saveMessageToSession(
    userPrompt: string,
    assistantResponse: string,
    agent?: WorkerSlot,
    source?: MessageSource,
    sessionId?: string
  ): void {
    const resolvedSessionId = sessionId
      || this.activeSessionId
      || this.sessionManager.getCurrentSession()?.id
      || '';
    if (!resolvedSessionId) {
      return;
    }
    if (assistantResponse) {
      const resolvedAgent = agent || (source === 'orchestrator' ? 'orchestrator' : undefined);
      try {
        this.sessionManager.addMessageToSession(resolvedSessionId, 'assistant', assistantResponse, resolvedAgent, source);
      } catch (error: any) {
        logger.warn('界面.会话.助手消息写入失败', {
          sessionId: resolvedSessionId,
          error: error?.message || String(error),
        }, LogCategory.UI);
        this.sendStateUpdate();
        return;
      }
      void this.orchestratorEngine.recordContextMessage('assistant', assistantResponse, resolvedSessionId);
    }
    this.sendStateUpdate();
  }

  /** 保存当前会话的完整数据（从前端同步） */
  private saveCurrentSessionData(messages: any[]): void {
    const currentSession = this.sessionManager.getCurrentSession();
    if (!currentSession) {
      logger.info('界面.会话.保存.跳过', { reason: 'no_current_session' }, LogCategory.UI);
      return;
    }

    const incomingMessages = Array.isArray(messages) ? messages : [];

    // 空数据保护：如果传入消息为空但后端已有消息，拒绝覆写以防止数据丢失
    // 典型场景：webview 刚加载尚未从后端同步消息时，用户立即切换会话
    if (incomingMessages.length === 0 && currentSession.messages.length > 0) {
      logger.warn('界面.会话.保存.拒绝_空覆写', {
        sessionId: currentSession.id,
        existingCount: currentSession.messages.length,
      }, LogCategory.UI);
      return;
    }

    const seen = new Set<string>();
    let normalizedRoleCount = 0;
    const sessionMessages: SessionMessage[] = incomingMessages.map((m) => {
      const id = typeof m?.id === 'string' && m.id.trim() ? m.id.trim() : '';
      if (!id) {
        throw new Error(t('provider.errors.sessionMessageMissingId'));
      }
      if (seen.has(id)) {
        throw new Error(t('provider.errors.sessionMessageDuplicateId', { id }));
      }
      seen.add(id);

      const role = this.resolveSessionMessageRole(m);
      if (role !== m?.role) {
        normalizedRoleCount++;
      }

      if (typeof m?.content !== 'string') {
        throw new Error(t('provider.errors.sessionMessageContentNotString'));
      }
      if (typeof m?.timestamp !== 'number') {
        throw new Error(t('provider.errors.sessionMessageInvalidTimestamp'));
      }
      const normalizedSource = this.normalizeSessionMessageSource(m.source, m.agent, m.metadata);
      return {
        id,
        role,
        content: m.content,
        agent: normalizedSource.agent || (typeof m?.agent === 'string' ? m.agent : undefined),
        timestamp: m.timestamp,
        images: Array.isArray(m?.images) ? this.cloneSerializable(m.images) : undefined,
        source: normalizedSource.source,
        blocks: this.normalizeSessionBlocks(m?.blocks),
        type: typeof m?.type === 'string' ? m.type : undefined,
        noticeType: typeof m?.noticeType === 'string' ? m.noticeType : undefined,
        isStreaming: typeof m?.isStreaming === 'boolean' ? m.isStreaming : undefined,
        isComplete: typeof m?.isComplete === 'boolean' ? m.isComplete : undefined,
        metadata: m?.metadata && typeof m.metadata === 'object' && !Array.isArray(m.metadata)
          ? this.cloneSerializable(m.metadata)
          : undefined,
      };
    });

    if (normalizedRoleCount > 0) {
      logger.warn('界面.会话.保存.role归一化', {
        sessionId: currentSession.id,
        normalizedRoleCount,
      }, LogCategory.UI);
    }

    // 使用新的 API 保存会话数据
    this.sessionManager.updateSessionData(currentSession.id, sessionMessages);  // ✅ 移除 cliOutputs 参数
    logger.info('界面.会话.保存.完成', { messageCount: sessionMessages.length }, LogCategory.UI);
  }

  private resolveSessionMessageRole(message: any): 'user' | 'assistant' | 'system' {
    if (message?.type === 'user_input') {
      return 'user';
    }
    if (message?.type === 'system-notice') {
      return 'system';
    }

    const role = message?.role;
    if (role === 'user' || role === 'assistant' || role === 'system') {
      return role;
    }

    throw new Error(
      t('provider.errors.sessionMessageInvalidRoleType', {
        role: String(role),
        type: String(message?.type),
      })
    );
  }

  private normalizeSessionBlocks(rawBlocks: unknown): ContentBlock[] | undefined {
    if (rawBlocks === undefined) {
      return undefined;
    }
    if (!Array.isArray(rawBlocks)) {
      throw new Error(t('provider.errors.sessionMessageBlocksNotArray'));
    }
    if (rawBlocks.length === 0) {
      return undefined;
    }
    return rawBlocks.map((rawBlock, index) => this.normalizeSessionBlock(rawBlock, index));
  }

  private normalizeSessionBlock(rawBlock: unknown, index: number): ContentBlock {
    if (!rawBlock || typeof rawBlock !== 'object' || Array.isArray(rawBlock)) {
      throw new Error(t('provider.errors.sessionBlockInvalid', { index }));
    }
    const block = rawBlock as Record<string, unknown>;
    const blockType = block.type;
    if (typeof blockType !== 'string' || blockType.length === 0) {
      throw new Error(t('provider.errors.sessionBlockMissingType', { index }));
    }

    switch (blockType) {
      case 'text':
        return {
          type: 'text',
          content: typeof block.content === 'string' ? block.content : '',
        };
      case 'code':
        return {
          type: 'code',
          content: typeof block.content === 'string' ? block.content : '',
          language: typeof block.language === 'string' && block.language.trim().length > 0
            ? block.language
            : 'text',
        };
      case 'thinking': {
        const thinking = block.thinking;
        const thinkingObj = thinking && typeof thinking === 'object' && !Array.isArray(thinking)
          ? thinking as Record<string, unknown>
          : undefined;
        const content = typeof thinkingObj?.content === 'string'
          ? thinkingObj.content
          : (typeof block.content === 'string' ? block.content : '');
        const summary = typeof thinkingObj?.summary === 'string'
          ? thinkingObj.summary
          : undefined;
        return {
          type: 'thinking',
          content,
          ...(summary ? { summary } : {}),
        };
      }
      case 'tool_call': {
        const toolCallRaw = block.toolCall;
        if (!toolCallRaw || typeof toolCallRaw !== 'object' || Array.isArray(toolCallRaw)) {
          throw new Error(t('provider.errors.toolCallMissingObject', { index }));
        }
        const toolCall = toolCallRaw as Record<string, unknown>;
        const toolId = typeof toolCall.id === 'string' ? toolCall.id.trim() : '';
        const toolName = typeof toolCall.name === 'string' ? toolCall.name.trim() : '';
        if (!toolId || !toolName) {
          throw new Error(t('provider.errors.toolCallMissingIdOrName', { index }));
        }
        const standardized = this.normalizeStandardizedToolResult(toolCall.standardized, index);
        return {
          type: 'tool_call',
          toolId,
          toolName,
          status: this.normalizeToolCallStatus(toolCall.status, index),
          input: this.serializeToolArguments(toolCall.arguments, index),
          output: typeof toolCall.result === 'string' ? toolCall.result : undefined,
          error: typeof toolCall.error === 'string' ? toolCall.error : undefined,
          standardized,
        };
      }
      case 'file_change': {
        const fileChangeRaw = block.fileChange;
        if (!fileChangeRaw || typeof fileChangeRaw !== 'object' || Array.isArray(fileChangeRaw)) {
          throw new Error(t('provider.errors.fileChangeMissingObject', { index }));
        }
        const fileChange = fileChangeRaw as Record<string, unknown>;
        const filePath = typeof fileChange.filePath === 'string' ? fileChange.filePath : '';
        const changeType = fileChange.changeType;
        if (typeof filePath !== 'string' || filePath.trim().length === 0) {
          throw new Error(t('provider.errors.fileChangeMissingPath', { index }));
        }
        if (changeType !== 'create' && changeType !== 'modify' && changeType !== 'delete') {
          throw new Error(t('provider.errors.fileChangeInvalidType', { index }));
        }
        return {
          type: 'file_change',
          filePath,
          changeType,
          additions: typeof fileChange.additions === 'number' ? fileChange.additions : undefined,
          deletions: typeof fileChange.deletions === 'number' ? fileChange.deletions : undefined,
          diff: typeof fileChange.diff === 'string' ? fileChange.diff : undefined,
        };
      }
      case 'plan': {
        const planRaw = block.plan;
        if (!planRaw || typeof planRaw !== 'object' || Array.isArray(planRaw)) {
          throw new Error(t('provider.errors.planMissingObject', { index }));
        }
        const plan = planRaw as Record<string, unknown>;
        const goal = typeof plan.goal === 'string' ? plan.goal : '';
        if (goal.trim().length === 0) {
          throw new Error(t('provider.errors.planMissingGoal', { index }));
        }
        return {
          type: 'plan',
          goal,
          analysis: typeof plan.analysis === 'string' ? plan.analysis : undefined,
          constraints: this.normalizeStringArray(plan.constraints),
          acceptanceCriteria: this.normalizeStringArray(plan.acceptanceCriteria),
          riskLevel: plan.riskLevel === 'low' || plan.riskLevel === 'medium' || plan.riskLevel === 'high'
            ? plan.riskLevel
            : undefined,
          riskFactors: this.normalizeStringArray(plan.riskFactors),
          rawJson: typeof plan.rawJson === 'string' ? plan.rawJson : undefined,
        };
      }
      default:
        throw new Error(t('provider.errors.sessionBlockUnsupportedType', { blockType }));
    }
  }

  private normalizeToolCallStatus(status: unknown, index: number): 'pending' | 'running' | 'completed' | 'failed' {
    if (status === 'pending' || status === 'running' || status === 'completed' || status === 'failed') {
      return status;
    }
    if (status === 'success') {
      return 'completed';
    }
    if (status === 'error') {
      return 'failed';
    }
    throw new Error(t('provider.errors.toolCallInvalidStatus', { index }));
  }

  private serializeToolArguments(argumentsValue: unknown, index: number): string | undefined {
    if (argumentsValue === undefined) {
      return undefined;
    }
    try {
      return JSON.stringify(argumentsValue);
    } catch {
      throw new Error(t('provider.errors.toolCallArgumentsNotSerializable', { index }));
    }
  }

  private normalizeStringArray(value: unknown): string[] | undefined {
    if (!Array.isArray(value)) {
      return undefined;
    }
    const normalized = value.filter((item): item is string => typeof item === 'string');
    return normalized.length > 0 ? normalized : undefined;
  }

  private normalizeStandardizedToolResult(value: unknown, index: number): StandardizedToolResultPayload | undefined {
    if (value === undefined) {
      return undefined;
    }
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      throw new Error(t('provider.errors.standardizedInvalid', { index }));
    }
    const payload = value as Record<string, unknown>;
    if (payload.schemaVersion !== 'tool-result.v1') {
      throw new Error(t('provider.errors.standardizedInvalidSchemaVersion', { index }));
    }
    const source = payload.source;
    if (source !== 'builtin' && source !== 'mcp' && source !== 'skill') {
      throw new Error(t('provider.errors.standardizedInvalidSource', { index }));
    }
    const toolName = payload.toolName;
    if (typeof toolName !== 'string' || toolName.trim().length === 0) {
      throw new Error(t('provider.errors.standardizedInvalidToolName', { index }));
    }
    const toolCallId = payload.toolCallId;
    if (typeof toolCallId !== 'string' || toolCallId.trim().length === 0) {
      throw new Error(t('provider.errors.standardizedInvalidToolCallId', { index }));
    }
    const status = payload.status;
    if (
      status !== 'success'
      && status !== 'error'
      && status !== 'timeout'
      && status !== 'killed'
      && status !== 'blocked'
      && status !== 'rejected'
      && status !== 'aborted'
    ) {
      throw new Error(t('provider.errors.standardizedInvalidStatus', { index }));
    }
    const message = payload.message;
    if (typeof message !== 'string') {
      throw new Error(t('provider.errors.standardizedInvalidMessage', { index }));
    }

    const normalized: StandardizedToolResultPayload = {
      schemaVersion: 'tool-result.v1',
      source,
      toolName,
      toolCallId,
      status,
      message,
      ...(payload.data !== undefined ? { data: this.cloneSerializable(payload.data) } : {}),
      ...(typeof payload.errorCode === 'string' ? { errorCode: payload.errorCode } : {}),
      ...(typeof payload.sourceId === 'string' ? { sourceId: payload.sourceId } : {}),
    };
    return normalized;
  }

  private isWorkerSlot(value: unknown): value is WorkerSlot {
    return value === 'claude' || value === 'codex' || value === 'gemini';
  }

  private normalizeSessionMessageSource(
    source: unknown,
    agent: unknown,
    metadata: unknown
  ): { source: 'orchestrator' | 'worker' | 'system'; agent?: WorkerSlot } {
    const explicitAgent = this.isWorkerSlot(agent) ? agent : undefined;
    const sourceAgent = this.isWorkerSlot(source) ? source : undefined;
    const metadataWorker = metadata && typeof metadata === 'object' && !Array.isArray(metadata)
      ? (metadata as Record<string, unknown>).worker
      : undefined;
    const metadataAgent = this.isWorkerSlot(metadataWorker) ? metadataWorker : undefined;
    const resolvedAgent = explicitAgent || sourceAgent || metadataAgent;

    if (resolvedAgent) {
      return { source: 'worker', agent: resolvedAgent };
    }
    if (source === 'system') {
      return { source: 'system' };
    }
    if (source === 'worker') {
      return { source: 'worker' };
    }
    return { source: 'orchestrator' };
  }

  private cloneSerializable<T>(value: T): T {
    try {
      return structuredClone(value);
    } catch {
      return JSON.parse(JSON.stringify(value)) as T;
    }
  }

  /** 构建 UI 状态（统一使用 Mission + TaskView） */
  private async buildUIState(): Promise<UIState> {
    const currentSession = this.sessionManager.getCurrentSession();
    const sessionId = this.activeSessionId || currentSession?.id;

    // 统一 Todo 系统：从 Mission 获取 TaskView 列表
    let tasks: any[] = [];
    if (sessionId) {
      const taskViews = await this.getTaskViews();
      // 将 TaskView 转换为 UI Task 格式
      tasks = taskViews.map(tv => ({
        id: tv.id,
        name: tv.title || tv.goal || tv.prompt,
        prompt: tv.prompt,
        description: tv.goal,
        status: tv.status,
        priority: tv.priority,
        deliveryStatus: tv.deliveryStatus,
        deliverySummary: tv.deliverySummary,
        deliveryDetails: tv.deliveryDetails,
        deliveryWarnings: tv.deliveryWarnings,
        continuationPolicy: tv.continuationPolicy,
        continuationReason: tv.continuationReason,
        subTasks: tv.subTasks,
        createdAt: tv.createdAt,
        startedAt: tv.startedAt,
        completedAt: tv.completedAt,
        progress: tv.progress,
        missionId: tv.missionId,
        failureReason: tv.failureReason,
      }));
    }

    const engineRunning = this.orchestratorEngine.running;
    const sortedTasks = [...tasks].sort((a, b) => {
      const aTs = Number(a?.startedAt || a?.createdAt || 0);
      const bTs = Number(b?.startedAt || b?.createdAt || 0);
      return bTs - aTs;
    });
    const displayTasks = sortedTasks;

    this.assertValidArray<any>(displayTasks, 'uiState.tasks');
    const currentTask = displayTasks.find(t => t?.status === 'running') ?? displayTasks[0];

    // 使用轻量级的会话元数据（而不是完整会话数据）
    const sessionMetas = this.sessionManager.getSessionMetas();
    this.assertValidArray<any>(sessionMetas, 'uiState.sessions');

    // 构建 Worker 状态（基于 LLM 适配器）
    const workerSlots: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    const workerStatuses: WorkerStatus[] = workerSlots.map(worker => ({
      worker,
      available: this.adapterFactory.isConnected(worker),
      enabled: true,
    }));

    const isRunning = engineRunning;
    const pendingChanges = this.snapshotManager.getPendingChanges();
    this.assertValidArray<any>(pendingChanges, 'uiState.pendingChanges');
    const logs = this.logs;
    this.assertValidArray<LogEntry>(logs, 'uiState.logs');
    const planSessionId = (this.activeSessionId ?? currentSession?.id ?? '').trim();
    const activePlan = planSessionId
      ? this.orchestratorEngine.getActivePlanState(planSessionId)
      : undefined;

    const planHistory = planSessionId
      ? this.orchestratorEngine.getPlanLedgerSnapshot(planSessionId).plans
      : [];

    const stateUpdatedAt = Date.now();
    const recovered = this.recoveredSinceLastUpdate;
    this.recoveredSinceLastUpdate = false;
    return {
      currentSessionId: this.activeSessionId ?? currentSession?.id,
      sessions: sessionMetas,
      currentTask,
      tasks: displayTasks,
      locale: this.locale,
      workerStatuses,
      pendingChanges,
      isRunning,
      logs,
      interactionMode: this.orchestratorEngine.getInteractionMode(),
      interactionModeUpdatedAt: this.interactionModeUpdatedAt,
      orchestratorPhase: this.orchestratorEngine.phase,
      activePlan,
      planHistory,
      stateUpdatedAt,
      recovered,
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

  private assertValidArray<T>(value: unknown, context: string): T[] {
    if (!Array.isArray(value)) {
      const error = new Error(`[UIState Validation] ${context} is not an array`);
      logger.error('界面.状态.数组_非法', { context, valueType: typeof value }, LogCategory.UI);
      throw error;
    }
    const length = value.length;
    if (!Number.isFinite(length) || length < 0 || length > 0xffffffff) {
      const error = new Error(`[UIState Validation] ${context} has invalid length: ${length}`);
      logger.error('界面.状态.数组_长度非法', { context, length }, LogCategory.UI);
      throw error;
    }
    if (length > this.MAX_REASONABLE_ARRAY_LENGTH) {
      const error = new Error(`[UIState Validation] ${context} length is suspiciously large: ${length}`);
      logger.error('界面.状态.数组_长度异常', { context, length }, LogCategory.UI);
      throw error;
    }
    return value as T[];
  }

  private assertBlocks(blocks: ContentBlock[] | undefined, context: string): ContentBlock[] {
    if (blocks === undefined) return [];
    const safeBlocks = this.assertValidArray<ContentBlock>(blocks, context)
      .filter((block) => !!block && typeof block === 'object' && typeof (block as ContentBlock).type === 'string');
    if (safeBlocks.length !== blocks.length) {
      logger.warn('界面.消息.块_清理', { context, removed: blocks.length - safeBlocks.length }, LogCategory.UI);
    }
    return safeBlocks;
  }

  private getWebviewMessagePriority(message: ExtensionToWebviewMessage): WebviewMessagePriority {
    return HIGH_PRIORITY_MESSAGE_TYPES.has(message.type) ? 'high' : 'normal';
  }

  private sendData(dataType: DataMessageType, payload: Record<string, unknown>): void {
    this.messageHub.data(dataType, payload);
  }

  private sendToast(message: string, level: NotifyLevel = 'info', duration?: number): void {
    this.messageHub.notify(message, level, duration);
  }

  /** 发送消息到 Webview（统一消息总线，优先级调度） */
  private postMessage(message: ExtensionToWebviewMessage): void {
    this.webviewMessageBus.send(message);
  }

  /** 获取 HTML 内容 - 仅使用 Svelte webview */
  private getHtmlContent(webview: vscode.Webview): string {
    return this.getSvelteHtmlContent(webview);
  }

  /** 获取 Svelte webview HTML 内容 */
  private getSvelteHtmlContent(webview: vscode.Webview): string {
    // 读取 Svelte 构建输出的 HTML
    const templatePath = path.join(this.extensionUri.fsPath, 'dist', 'webview', 'index.html');

    if (!fs.existsSync(templatePath)) {
      const message = t('provider.errors.svelteWebviewNotBuilt', { templatePath });
      logger.error('界面.Svelte.未构建', { path: templatePath }, LogCategory.UI);
      throw new Error(message);
    }

    let html = fs.readFileSync(templatePath, 'utf-8');
    const cacheBuster = Date.now().toString();

    // 获取 webview 资源根目录
    const webviewAssetsUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.extensionUri.fsPath, 'dist', 'webview', 'assets'))
    );

    // 替换资源路径（Vite 构建使用 ./assets/ 相对路径前缀）
    html = html.replace(/src="\.\/assets\//g, `src="${webviewAssetsUri}/`);
    html = html.replace(/href="\.\/assets\//g, `href="${webviewAssetsUri}/`);

    // 添加缓存破坏参数
    html = html.replace(/\.js"/g, `.js?v=${cacheBuster}"`);
    html = html.replace(/\.css"/g, `.css?v=${cacheBuster}"`);

    // 注入 CSP meta 标签（VS Code webview 安全策略）
    const cspMeta = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src ${webview.cspSource}; font-src ${webview.cspSource}; img-src ${webview.cspSource} https: data:;">`;
    html = html.replace('<head>', `<head>\n    ${cspMeta}`);

    // 注入初始 sessionId 与 locale
    const initialSessionId = this.activeSessionId || '';
    const initialLocale = this.locale;
    const bootstrapScript = `<script>window.__INITIAL_SESSION_ID__ = ${JSON.stringify(initialSessionId)}; window.__INITIAL_LOCALE__ = ${JSON.stringify(initialLocale)};</script>`;
    html = html.replace('</head>', `${bootstrapScript}\n  </head>`);

    logger.debug('界面.Svelte.已加载', { sessionId: initialSessionId, locale: initialLocale }, LogCategory.UI);
    return html;
  }

  private getWebviewErrorHtml(errorMessage: string): string {
    const title = this.escapeHtml(t('provider.errors.webviewRenderTitle'));
    const hint = this.escapeHtml(t('provider.errors.webviewRenderHint'));
    const details = this.escapeHtml(errorMessage);
    return `<!DOCTYPE html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>${title}</title>
    <style>
      body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; padding: 24px; color: #1f2328; }
      .title { font-size: 18px; font-weight: 600; margin-bottom: 8px; }
      .hint { font-size: 13px; color: #636c76; margin-bottom: 12px; }
      .details { font-size: 12px; background: #f6f8fa; padding: 12px; border-radius: 8px; white-space: pre-wrap; }
    </style>
  </head>
  <body>
    <div class="title">${title}</div>
    <div class="hint">${hint}</div>
    <div class="details">${details}</div>
  </body>
</html>`;
  }

  private escapeHtml(input: string): string {
    return input
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
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
      if (this.orchestratorEngine) {
        logger.info('界面.销毁.编排器.中断', undefined, LogCategory.UI);
        this.orchestratorEngine.interrupt();
        this.orchestratorEngine.dispose();
      }

      // 2. 清理适配器（关闭所有连接）
      if (this.adapterFactory) {
        logger.info('界面.销毁.适配器.清理', undefined, LogCategory.UI);
        await this.adapterFactory.shutdown();
      }

      // 3. 主动拒绝所有待处理工具授权，避免悬挂
      this.eventBindingService.disposeToolAuthorization();

      // 4. 移除事件监听器
      globalEventBus.clear();
      logger.info('界面.销毁.事件.已清理', undefined, LogCategory.UI);

      // 5. 清理定时器与请求跟踪
      for (const timer of this.requestTimeouts.values()) {
        clearTimeout(timer);
      }
      this.requestTimeouts.clear();
      this.messageIdToRequestId.clear();
      this.recentRequestIds.clear();
      this.streamMessageIds.clear();
      if (this.logFlushTimer) {
        clearTimeout(this.logFlushTimer);
        this.logFlushTimer = null;
      }
      if (this.processingResetFallbackTimer) {
        clearTimeout(this.processingResetFallbackTimer);
        this.processingResetFallbackTimer = null;
      }
      this.clearPendingRecoveryState();
      this.queuedMessagesBySession.clear();

      // 6. 清理 Webview
      this._view = undefined;

      logger.info('界面.销毁.完成', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.销毁.失败', error, LogCategory.UI);
    }
  }
}
