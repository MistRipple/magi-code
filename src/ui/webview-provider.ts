/**
 * WebviewProvider - Webview 面板提供者
 * 负责：对话面板、任务视图、变更视图、Agent 输出
 */

import { logger, LogCategory } from '../logging';
import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
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
  DataMessageType,
  NotifyLevel,
  ControlMessageType,
  createStandardMessage,
  createUserInputMessage,
  createStreamingMessage,
  createErrorMessage,
} from '../protocol/message-protocol';
import { UnifiedSessionManager } from '../session';
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
import { EventBindingService } from './event-binding-service';
import { WorkerStatusService } from './worker-status-service';
import { PromptEnhancerService } from '../services/prompt-enhancer-service';
import { DirectExecutionService } from '../services/direct-execution-service';
import {
  MissionOrchestrator,
} from '../orchestrator/core';
import { WorkspaceFolderInfo, WorkspaceRoots } from '../workspace/workspace-roots';

type WebviewMessagePriority = 'high' | 'normal';

type OrchestratorExecutionResult = { success: boolean; error?: string };
type OrchestratorQueueItem = {
  prompt: string;
  imagePaths: string[];
  resolve: (result: OrchestratorExecutionResult) => void;
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
  private directExecutor: DirectExecutionService;

  // 事件绑定服务（从 WVP 提取）
  private eventBindingService: EventBindingService;

  // 当前选择的 Worker（null 表示自动选择/智能编排）
  private selectedWorker: WorkerSlot | null = null;
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
      throw new Error('未检测到可用工作区目录');
    }

    this.workspaceFolders = normalizedFolders;
    this.workspaceRoots = new WorkspaceRoots(normalizedFolders);
    this.workspaceRoot = this.workspaceRoots.getPrimaryFolder().path;
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

    // 注入 SnapshotManager 到 ToolManager（确保工具级文件写入自动创建快照）
    (this.adapterFactory as LLMAdapterFactory).getToolManager().setSnapshotManager(this.snapshotManager);

    // 异步初始化 profile loader（在 MessageHub 注入之后）
    void (this.adapterFactory as LLMAdapterFactory).initialize().catch(err => {
      logger.error('Failed to initialize LLM adapter factory', { error: err.message }, LogCategory.LLM);
    });

    // 初始化编排引擎（设置 IntentGate 等关键组件）
    void this.orchestratorEngine.initialize().catch(err => {
      logger.error('Failed to initialize orchestrator engine', { error: err.message }, LogCategory.ORCHESTRATOR);
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

    // 初始化直接执行服务
    this.directExecutor = new DirectExecutionService({
      getSessionId: () => this.activeSessionId || this.sessionManager.getCurrentSession()?.id || 'default',
      getToolManager: () => (this.adapterFactory as LLMAdapterFactory).getToolManager(),
      sendMessage: (worker, prompt, images) => this.adapterFactory.sendMessage(worker, prompt, images),
      createTaskFromPrompt: (sid, p) => this.orchestratorEngine.createTaskFromPrompt(sid, p),
      markTaskExecuting: (id) => this.orchestratorEngine.markTaskExecuting(id),
      completeTaskById: (id) => this.orchestratorEngine.completeTaskById(id),
      failTaskById: (id, err) => this.orchestratorEngine.failTaskById(id, err),
      cancelTaskById: (id) => this.orchestratorEngine.cancelTaskById(id),
      getExecutionStats: () => this.orchestratorEngine.getExecutionStats(),
      sendStateUpdate: () => this.sendStateUpdate(),
      sendErrorMessage: (content, worker) => this.sendOrchestratorMessage({
        content,
        messageType: 'error',
        metadata: { worker },
      }),
      sendResultMessage: (content, worker) => {
        const requestId = this.messageHub.getRequestContext();
        this.messageHub.result(content, { metadata: { requestId, worker } });
      },
      saveMessageToSession: (prompt, content, worker) => this.saveMessageToSession(prompt, content, worker, 'worker'),
    });

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

      // 设置压缩模型客户端（用于知识提取 + ACE 降级时的 LLM 辅助搜索扩展）
      await this.setupKnowledgeExtractionClient();

      // 注入知识库到编排器
      this.orchestratorEngine.setKnowledgeBase(this.projectKnowledgeBase);

      // 监听任务完成事件，自动提取知识
      this.setupAutoKnowledgeExtraction();

      // 设置文件监听器，支持搜索引擎增量更新
      this.setupFileSystemWatcher();

      // 注入本地搜索服务到 AceExecutor（ACE 不可用时自动降级）
      this.injectLocalSearchService();

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
    this.sendData('projectKnowledgeLoaded', { codeIndex, adrs, faqs });
  }

  /**
   * 设置知识提取客户端（使用压缩模型，同时为 ACE 降级搜索提供 LLM 查询扩展）
   */
  private async setupKnowledgeExtractionClient(): Promise<void> {
    try {
      const { createKnowledgeExtractionClient } = await import('../knowledge/knowledge-extraction-client');
      const executionStats = this.orchestratorEngine.getExecutionStats();
      const client = await createKnowledgeExtractionClient(executionStats);

      const knowledgeBase = this.projectKnowledgeBase;
      if (!knowledgeBase) {
        logger.warn('项目知识库.压缩模型客户端.未设置_知识库未初始化', undefined, LogCategory.SESSION);
        return;
      }

      knowledgeBase.setLLMClient(client);
    } catch (error: any) {
      logger.error('项目知识库.压缩模型客户端.设置失败', { error: error.message }, LogCategory.SESSION);
    }
  }

  /**
   * 设置自动知识提取
   * 监听任务完成事件，自动从会话中提取 ADR 和 FAQ
   */
  private setupAutoKnowledgeExtraction(): void {
    // 任务完成计数器
    let completedTaskCount = 0;
    const EXTRACTION_THRESHOLD = 3; // 每完成 3 个任务提取一次

    // 监听任务完成事件
    globalEventBus.on('task:completed', async (event: any) => {
      completedTaskCount++;

      // 触发代码索引刷新（防抖，不会每次都全量扫描）
      this.projectKnowledgeBase?.refreshIndex();

      // 达到阈值时提取知识
      if (completedTaskCount >= EXTRACTION_THRESHOLD) {
        completedTaskCount = 0; // 重置计数器
        await this.extractKnowledgeFromCurrentSession();
      }
    });

    // 监听会话结束事件
    globalEventBus.on('session:ended', async (event: any) => {
      const sessionId = event.sessionId;
      if (sessionId) {
        await this.extractKnowledgeFromSession(sessionId);
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
    for (const folder of this.workspaceFolders) {
      const watcher = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(folder.path, '**/*.{ts,js,tsx,jsx,json,md,yml,yaml}')
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

    logger.info('项目知识库.文件监听器.已启用', undefined, LogCategory.SESSION);
  }

  /**
   * 注入 LocalCodeSearchService 到 AceExecutor
   * 使用惰性引用，解决 PKB 初始化时序问题
   */
  private injectLocalSearchService(): void {
    const toolManager = this.adapterFactory.getToolManager?.();
    if (!toolManager) return;

    const { LocalCodeSearchService } = require('../services/local-code-search-service');
    const localSearch = new LocalCodeSearchService({
      getKnowledgeBase: () => this.projectKnowledgeBase,
      getSearchExecutor: () => toolManager.getSearchExecutor(),
      getLspExecutor: () => toolManager.getLspExecutor(),
      extractKeywords: (query: string) => this.promptEnhancer.extractKeywords(query),
      workspaceFolders: this.workspaceFolders,
    });

    toolManager.getAceExecutor().setLocalSearchService(localSearch);
    logger.info('AceExecutor.本地搜索服务.已注入', undefined, LogCategory.SESSION);
  }

  /**
   * 从当前会话提取知识
   */
  private async extractKnowledgeFromCurrentSession(): Promise<void> {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      return;
    }

    await this.extractKnowledgeFromSession(session.id);
  }

  /**
   * 从指定会话提取知识
   */
  private async extractKnowledgeFromSession(sessionId: string): Promise<void> {
    try {
      const session = this.sessionManager.getSession(sessionId);
      if (!session || session.messages.length < 5) {
        // 消息太少，不值得提取
        return;
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
        return;
      }

      // 提取 ADR
      const adrs = await knowledgeBase.extractADRFromSession(messages);
      if (adrs.length > 0) {
        // 存储提取到的 ADR
        for (const adr of adrs) {
          knowledgeBase.addADR(adr);
        }

        logger.info('项目知识库.ADR提取成功', {
          count: adrs.length,
          titles: adrs.map(a => a.title)
        }, LogCategory.SESSION);

        // 通知前端
        this.sendToast(`自动提取了 ${adrs.length} 条架构决策记录`, 'success');

        // 刷新知识库显示
        this.sendProjectKnowledgeToWebview();
      }

      // 提取 FAQ
      const faqs = await knowledgeBase.extractFAQFromSession(messages);
      if (faqs.length > 0) {
        // 存储提取到的 FAQ
        for (const faq of faqs) {
          knowledgeBase.addFAQ(faq);
        }

        logger.info('项目知识库.FAQ提取成功', {
          count: faqs.length,
          questions: faqs.map(f => f.question)
        }, LogCategory.SESSION);

        // 通知前端
        this.sendToast(`自动提取了 ${faqs.length} 条常见问题`, 'success');

        // 刷新知识库显示
        this.sendProjectKnowledgeToWebview();
      }

      if (adrs.length === 0 && faqs.length === 0) {
        logger.info('项目知识库.未提取到新知识', { sessionId }, LogCategory.SESSION);
      }
    } catch (error: any) {
      logger.error('项目知识库.知识提取失败', {
        sessionId,
        error: error.message
      }, LogCategory.SESSION);
    }
  }

  private emitUserAndPlaceholder(requestId: string, prompt: string, imageCount: number, images?: Array<{ dataUrl: string }>, targetWorker?: string): {
    userMessageId: string;
    placeholderMessageId: string;
  } {
    const traceId = this.messageHub.getTraceId();
    // 图片已通过缩略图展示，不再在文本中附加 [附件: X 张图片]
    const displayContent = prompt;

    const userMessage = createUserInputMessage(displayContent, traceId, {
      metadata: {
        requestId,
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
      throw new Error('消息发送失败：用户消息或占位消息未成功发送');
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
      const traceId = this.messageHub.getTraceId();
      const timeoutMessage = createErrorMessage(
        '等待响应超时，请重试',
        'orchestrator',
        'orchestrator',
        traceId,
        {
          metadata: {
            requestId,
          },
        }
      );
      this.messageHub.sendMessage(timeoutMessage);
      this.clearRequestTimeout(requestId);
    }, 8000);
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
            this.sendToast('任务已批准', 'success');
          }
        } catch (error) {
          logger.error('界面.交互.审批_失败', error, LogCategory.UI);
          this.sendToast('审批操作失败', 'error');
        }
      } else {
        // 拒绝逻辑
        this.sendToast('任务已拒绝', 'info');

        // 【新增】记录被拒绝的方案到 Memory
        const contextManager = this.orchestratorEngine.getContextManager();
        if (contextManager) {
          contextManager.addRejectedApproach(
            '任务审批被用户拒绝',
            '用户选择不执行此任务',
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
                title: subTask.title || subTask.description || '子任务',
                status: 'stopped',
                worker: subTask.assignedWorker,
                summary: '用户终止',
              });
            }
          }
        }
      }
    }

    // 清理编排者流式输出缓存，避免跨任务串流
    this.streamMessageIds.clear();
    // 发送 task_failed 控制消息，确保前端 clearProcessingState() 被触发
    // 前端只响应 task_completed/task_failed 来清除处理态，processingStateChanged(false) 会被忽略
    this.messageHub.sendControl(ControlMessageType.TASK_FAILED, {
      error: '用户取消',
      cancelled: true,
      timestamp: Date.now(),
    });
    // 同步清理后端管道的处理态
    this.messageHub.forceProcessingState(false);

    if (hasRunningTask && !options?.silent) {
      // 4. 通知 UI
      this.sendToast('任务已打断', 'info');


      this.sendOrchestratorMessage({
        content: '任务已打断，可在变更中查看已修改的文件，或选择继续执行。',
        messageType: 'text',
        metadata: { phase: 'interrupted' },
      });
    }

    this.sendStateUpdate();
  }

  // 流式消息 ID 管理
  private streamMessageIds: Map<string, string> = new Map(); // key: `${source}-${worker}-${target}`, value: messageId

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

    // HTML 中已注入 initialSessionId，webview 加载时即有正确的 sessionId
    webviewView.webview.html = this.getHtmlContent(webviewView.webview);

    // 处理来自 Webview 的消息
    webviewView.webview.onDidReceiveMessage(
      (message: WebviewToExtensionMessage) => this.handleMessage(message),
      undefined,
      this.context.subscriptions
    );

    // Webview 初始化时强制中断可能残留的任务，避免重启后状态错乱
    void this.interruptCurrentTask({ silent: true });

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
        this.sendCurrentSessionToWebview();
        break;

      case 'requestState':
        this.sendStateUpdate();
        this.sendCurrentSessionToWebview();
        break;

      case 'webviewReady':
        // Webview 就绪后立即推送完整系统数据（任务、变更、会话等）
        // 这些数据不在 vscode.getState() 持久化范围内，必须由后端主动推送
        logger.info('界面.Webview.就绪', undefined, LogCategory.UI);
        this.sendStateUpdate();
        this.sendCurrentSessionToWebview();
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
        logger.info('界面.任务.执行.请求', { promptLength: String(message.prompt || '').length, imageCount: message.images?.length || 0, agent: message.agent || 'orchestrator' }, LogCategory.UI);
        const execImages = message.images || [];
        const execAgent = message.agent as WorkerSlot | undefined;
        const execRequestId = message.requestId;
        const requestedModeRaw = message.mode;
        const requestedMode = requestedModeRaw === 'ask' || requestedModeRaw === 'auto' ? requestedModeRaw : undefined;
        if (typeof requestedModeRaw === 'string' && !requestedMode) {
          logger.warn('界面.任务.执行.模式_非法', { requestedModeRaw, requestId: execRequestId }, LogCategory.UI);
          this.sendToast('收到非法交互模式参数，已按当前模式执行', 'warning');
        }
        if (!this.shouldProcessRequest(execRequestId)) {
          if (execRequestId) {
            this.messageHub.taskRejected(execRequestId, '请求重复，已忽略');
            const traceId = this.messageHub.getTraceId();
            const errorMessage = createErrorMessage(
              '请求重复，已忽略',
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
          await this.executeTask(message.prompt, execAgent || undefined, execImages, execRequestId);
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
        this.sendToast('暂停功能开发中', 'info');
        break;

      case 'resumeTask':

        logger.info('界面.任务.恢复.消息', { taskId: message.taskId }, LogCategory.UI);
        await this.resumeInterruptedTask();
        break;

      case 'appendMessage':

        logger.info('界面.消息.补充.请求', undefined, LogCategory.UI);
        await this.handleAppendMessage(message.taskId, message.content);
        break;

      case 'approveChange':
        // 批准单个变更
        this.snapshotManager.acceptChange(message.filePath);
        globalEventBus.emitEvent('change:approved', { data: { filePath: message.filePath } });
        this.sendToast('变更已批准', 'success');
        this.sendStateUpdate();
        break;

      case 'revertChange':
        this.snapshotManager.revertToSnapshot(message.filePath);
        this.sendToast('变更已还原', 'info');
        this.sendStateUpdate();
        break;

      case 'approveAllChanges':
        // 批准所有变更
        {
          const allChanges = this.snapshotManager.getPendingChanges();
          for (const change of allChanges) {
            this.snapshotManager.acceptChange(change.filePath);
          }
          this.sendToast(`已批准 ${allChanges.length} 个变更`, 'success');
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
          this.sendToast(`已还原 ${changes.length} 个变更`, 'info');
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

      case 'openLink':
        // 在外部浏览器中打开链接（从 markdown 链接点击）
        if (message.url && typeof message.url === 'string') {
          vscode.env.openExternal(vscode.Uri.parse(message.url));
        }
        break;

      case 'newSession':
        await this.handleNewSession();
        break;

      case 'saveCurrentSession':
        // 保存当前会话的消息
        this.saveCurrentSessionData(message.messages);
        break;

      case 'switchSession':

        if (this.activeSessionId !== message.sessionId) {
          await this.interruptCurrentTask({ silent: true });
        }
        // 切换会话
        await this.switchToSession(message.sessionId);
        const switchedSession = this.sessionManager.getCurrentSession();
        if (switchedSession) {
          // 恢复 Worker sessionIds
          this.sendData('sessionSwitched', {
            sessionId: message.sessionId,
            session: switchedSession,
          });
        }
        this.sendStateUpdate();
        break;

      case 'renameSession':
        // 重命名会话
        if (this.sessionManager.renameSession(message.sessionId, message.name)) {
          this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() });
          this.sendToast('会话已重命名', 'success');
        }
        break;

      case 'closeSession':
        this.performSessionDelete(message.sessionId);
        break;

      case 'deleteSession': {
        // 🔧 新增：带确认的删除会话（VS Code Webview 沙盒不支持 confirm()）
        const sessionIdToDelete = message.sessionId;
        const needConfirm = message.requireConfirm;

        if (needConfirm) {
          vscode.window.showWarningMessage(
            '确定要删除这个会话吗？此操作不可撤销。',
            { modal: true },
            '确定删除'
          ).then((selection) => {
            if (selection === '确定删除') {
              this.performSessionDelete(sessionIdToDelete);
            }
          });
        } else {
          // 无需确认直接删除
          this.performSessionDelete(sessionIdToDelete);
        }
        break;
      }

      case 'selectWorker':
        // 用户手动选择 Worker（null 表示自动选择）
        this.selectedWorker = message.worker || null;
        logger.info('界面.Worker.选择.变更', { worker: this.selectedWorker || 'auto' }, LogCategory.UI);
        break;

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
        this.handleSettingUpdate(message.key, message.value);
        break;

      case 'setInteractionMode':
        // 设置交互模式
        this.handleSetInteractionMode(message.mode);
        break;

      case 'confirmRecovery':
        // 用户确认恢复策略
        await this.handleRecoveryConfirmation(message.decision);
        break;

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

      case 'openMermaidPanel':
        // 在新标签页打开 Mermaid 图表
        this.handleOpenMermaidPanel(message.code, message.title);
        break;
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
      this.sendToast(`打开图表失败: ${error.message}`, 'error');
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

  private buildModelCatalog(): { id: string; label: string; model?: string; provider?: string; enabled?: boolean; role?: 'worker' | 'orchestrator' | 'compressor' | 'unknown' }[] {
    try {
      const { LLMConfigLoader } = require('../llm/config');
      const fullConfig = LLMConfigLoader.loadFullConfig();
      const entries: { id: string; label: string; model?: string; provider?: string; enabled?: boolean; role?: 'worker' | 'orchestrator' | 'compressor' | 'unknown' }[] = [];

      const toLabel = (id: string) => id.charAt(0).toUpperCase() + id.slice(1);
      const addEntry = (id: string, label: string, config: any, role: 'worker' | 'orchestrator' | 'compressor') => {
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

      if (fullConfig?.compressor) {
        addEntry('compressor', 'Compressor', fullConfig.compressor, 'compressor');
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
    this.sendExecutionStats();
    this.sendToast('执行统计已重置', 'info');
  }

  /** 处理设置交互模式 */
  private handleSetInteractionMode(mode: import('../types').InteractionMode): void {
    if (mode !== 'ask' && mode !== 'auto') {
      logger.error('界面.交互_模式.非法值', { mode }, LogCategory.UI);
      this.sendToast('交互模式无效，已忽略本次切换请求', 'error');
      return;
    }

    const currentMode = this.orchestratorEngine.getInteractionMode();
    const changed = currentMode !== mode;

    if (changed) {
      logger.info('界面.交互_模式.变更', { mode }, LogCategory.UI);
      this.orchestratorEngine.setInteractionMode(mode);
      this.interactionModeUpdatedAt = Date.now();
      this.sendToast(`已切换到 ${this.getModeDisplayName(mode)} 模式`, 'info');
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
      case 'ask': return '对话';
      case 'auto': return '自动';
      default: return mode;
    }
  }

  /** 恢复确认回调的 Promise resolver */
  private recoveryConfirmationResolver: ((decision: 'retry' | 'rollback' | 'continue') => void) | null = null;
  private pendingRecoveryRetry = false;
  private pendingRecoveryPrompt: string | null = null;
  private pendingExecutionQueue: OrchestratorQueueItem[] = [];
  private orchestratorQueueRunning = false;

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
      this.sendToast(message, 'info');
      this.sendOrchestratorMessage({
        content: `回滚完成：${message}`,
        messageType: 'result',
        metadata: { phase: 'recovery' },
      });
      return;
    }

    if (decision === 'retry') {
      if (this.orchestratorEngine.running) {
        this.pendingRecoveryRetry = true;
        this.pendingRecoveryPrompt = '请继续完成之前失败的任务';
        logger.warn('界面.编排器.恢复.重试_延迟_引擎运行中', undefined, LogCategory.UI);
        this.sendToast('当前任务仍在运行，已排队重试', 'info');
        return;
      }
      await this.resumeInterruptedTask('请继续完成之前失败的任务');
      return;
    }

    this.sendToast('已选择继续执行，未进行回滚', 'info');
  }

  private async tryResumePendingRecovery(): Promise<void> {
    if (!this.pendingRecoveryRetry) return;
    if (this.orchestratorEngine.running) return;
    const prompt = this.pendingRecoveryPrompt || '请继续完成之前失败的任务';
    this.pendingRecoveryRetry = false;
    this.pendingRecoveryPrompt = null;
    logger.info('界面.编排器.恢复.重试_触发', undefined, LogCategory.UI);
    await this.resumeInterruptedTask(prompt);
  }

  private enqueueOrchestratorExecution(prompt: string, imagePaths: string[]): Promise<OrchestratorExecutionResult> {
    return new Promise((resolve) => {
      this.pendingExecutionQueue.push({ prompt, imagePaths, resolve });
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
          const result = await this.executeWithOrchestrator(next.prompt, next.imagePaths);
          next.resolve(result);
        } catch (error) {
          const errorMsg = error instanceof Error ? error.message : String(error);
          next.resolve({ success: false, error: errorMsg });
        }
      }
    } finally {
      this.orchestratorQueueRunning = false;
    }
  }

  /** 在 VS Code 原生 diff 视图中打开文件变更（类似 Augment） */
  private async openVscodeDiff(filePath: string): Promise<void> {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      this.sendToast('没有活动会话', 'warning');
      return;
    }

    const relativePath = path.isAbsolute(filePath)
      ? path.relative(this.workspaceRoot, filePath)
      : filePath;

    const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
    if (!snapshot) {
      this.sendToast('未找到该文件的快照', 'warning');
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
      const title = `${fileName} (原始 ↔ 修改后)`;
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
      this.sendToast('打开 diff 视图失败', 'error');
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
        this.sendToast(`文件不存在: ${filepath}`, 'warning');
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
      this.sendToast(`打开文件失败: ${filepath}`, 'error');
    }
  }

  /** 清理所有任务（统一使用 Mission 系统） */
  private async handleClearAllTasks(): Promise<void> {
    const sessionId = this.activeSessionId;
    if (!sessionId) {
      this.sendToast('没有活动会话', 'warning');
      return;
    }

    // 检查是否有正在运行的任务
    if (this.orchestratorEngine.running) {
      this.sendToast('有任务正在执行，无法清理', 'warning');
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

    this.sendToast(`已清理 ${taskCount} 个任务`, 'success');
    this.sendStateUpdate();
  }

  private async handleStartTask(taskId?: string): Promise<void> {
    if (!taskId) {
      this.sendToast('缺少任务 ID', 'error');
      return;
    }
    try {
      // 先通知用户任务正在启动
      this.sendToast('任务启动中...', 'info');
      this.sendStateUpdate();
      // 触发完整执行链路（意图分析 → 规划 → 执行）
      await this.orchestratorEngine.startTaskById(taskId);
      this.sendStateUpdate();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendToast(`启动失败: ${errorMsg}`, 'error');
      this.sendStateUpdate();
    }
  }

  private async handleDeleteTask(taskId?: string): Promise<void> {
    if (!taskId) {
      this.sendToast('缺少任务 ID', 'error');
      return;
    }
    try {
      // 统一 Todo 系统 - 使用 orchestratorEngine
      await this.orchestratorEngine.deleteTaskById(taskId);
      this.sendToast('任务已删除', 'success');
      this.sendStateUpdate();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendToast(`删除失败: ${errorMsg}`, 'error');
    }
  }

  /** 获取最近被打断的任务 */
  private async getLastInterruptedTask(): Promise<{ id: string; prompt: string } | null> {
    // 统一 Todo 系统 - 使用 TaskView
    const tasks = await this.getTaskViews();
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
    if (this.orchestratorEngine.running) {
      this.sendToast('当前仍有任务在执行', 'warning');
      return;
    }

    const lastTask = await this.getLastInterruptedTask();
    if (!lastTask) {
      this.sendToast('没有可恢复的任务', 'info');
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

  /** 处理执行中追加输入：默认语义为“补充指令（下一决策点生效）” */
  private async handleAppendMessage(taskId: string, content: string): Promise<void> {
    logger.info('界面.消息.补充.请求', { taskId, preview: content.substring(0, 50) }, LogCategory.UI);

    const trimmedContent = content.trim();
    if (!trimmedContent) {
      this.sendToast('补充内容不能为空', 'warning');
      return;
    }

    try {
      const wasRunning = this.orchestratorEngine.running;

      if (wasRunning) {
        // 1. 在对话区显示用户追加的消息气泡（解决追加消息不可见的问题）
        const traceId = this.messageHub.getTraceId();
        const userMessage = createUserInputMessage(trimmedContent, traceId, {
          metadata: {
            isSupplementary: true,
          },
        });
        this.messageHub.sendMessage(userMessage);

        // 2. 注入补充指令队列，在下一决策点生效
        const accepted = this.orchestratorEngine.injectSupplementaryInstruction(trimmedContent);
        if (!accepted) {
          this.sendToast('当前任务不可注入补充指令，请重试', 'warning');
          return;
        }
        const pendingCount = this.orchestratorEngine.getPendingInstructionCount();
        this.messageHub.systemNotice('收到补充指令，将在下一决策点生效。', {
          phase: 'supplementary_instruction',
          isStatusMessage: true,
          extra: {
            pendingInstructionCount: pendingCount,
          },
        });
        logger.info('界面.消息.补充.已入队', { taskId, pendingCount }, LogCategory.UI);
        return;
      }

      // 竞态保护：前端认为执行中但后端已完成，作为新任务执行
      const requestId = `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      await this.executeTask(trimmedContent, undefined, [], requestId);
      logger.info('界面.消息.补充.空闲直执_成功', { taskId, wasRunning }, LogCategory.UI);
    } catch (error) {
      logger.error('界面.消息.补充.失败', error, LogCategory.UI);
      this.sendToast('补充内容失败', 'error');
    }
  }

  /** 处理设置更新 */
  private handleSettingUpdate(key: string, value: unknown): void {
    const config = vscode.workspace.getConfiguration('magi');

    // 处理其他配置
    if (key === 'autoSnapshot') {
      config.update('autoSnapshot', value, vscode.ConfigurationTarget.Global);
    }
    else if (key === 'timeout') {
      config.update('timeout', parseInt(value as string, 10), vscode.ConfigurationTarget.Global);
    }

    this.sendToast('设置已保存', 'success');
  }

  /** 执行任务 */
  private async executeTask(
    prompt: string,
    forceWorker?: WorkerSlot,
    images?: Array<{ dataUrl: string }>,
    requestId?: string,
    displayPrompt?: string
  ): Promise<void> {
    logger.info('界面.任务.执行.开始', { promptLength: prompt.length, imageCount: images?.length || 0, forceWorker: forceWorker || undefined }, LogCategory.UI);
    const maxPromptLength = 10000;
    const requestKey = requestId || `req_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
    let started = false;
    let rejected = false;
    let success = false;
    let failureReason: string | undefined;

    const rejectRequest = (reason: string) => {
      rejected = true;
      failureReason = reason;
      if (requestKey) {
        this.messageHub.taskRejected(requestKey, reason);
        const traceId = this.messageHub.getTraceId();
        const errorMessage = createErrorMessage(
          reason,
          'orchestrator',
          'orchestrator',
          traceId,
          { metadata: { requestId: requestKey } }
        );
        this.messageHub.sendMessage(errorMessage);
      }
      this.clearRequestTimeout(requestKey);
    };

    try {
      this.messageHub.setRequestContext(requestKey);

      // 📝 长度验证逻辑：
      // - 普通用户输入：验证 prompt 长度（防止粘贴过长内容）
      // - Skill 调用：displayPrompt 存在且与 prompt 不同时，只验证 displayPrompt 长度
      //   （Skill 指令内容由系统生成，可能很长，不应受用户输入限制）
      const isSkillInvocation = displayPrompt && displayPrompt !== prompt;
      const lengthToValidate = isSkillInvocation ? displayPrompt.length : prompt.length;

      if (lengthToValidate > maxPromptLength) {
        const displayLength = isSkillInvocation ? displayPrompt.length : prompt.length;
        this.sendToast(`输入内容过长（${displayLength} 字符），请控制在 ${maxPromptLength} 字符以内`, 'warning');
        rejectRequest(`输入内容过长（${displayLength} 字符）`);
        return;
      }

      if (!this.activeSessionId) {
        const currentSession = this.sessionManager.getCurrentSession();
        this.activeSessionId = currentSession?.id || null;
        logger.info('界面.会话.当前.设置', { sessionId: this.activeSessionId }, LogCategory.UI);
      }

      // 统一消息通道：由后端发送用户消息与占位消息
      const promptForDisplay = displayPrompt?.trim() || prompt;
      const resolvedTargetWorker = forceWorker || this.selectedWorker || undefined;
      this.emitUserAndPlaceholder(requestKey, promptForDisplay, images?.length || 0, images, resolvedTargetWorker);
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

      // 判断执行模式：智能编排 vs 直接执行
      const useIntelligentMode = !forceWorker && !this.selectedWorker;

      const resolvedSkill = this.resolveInstructionSkillPrompt(prompt);
      const effectivePrompt = resolvedSkill.prompt;

      this.sessionManager.addMessage('user', prompt, undefined, undefined, images);
      void this.orchestratorEngine.recordContextMessage('user', prompt, this.activeSessionId || undefined);
      this.sendStateUpdate();

      if (useIntelligentMode) {
        // 智能编排模式：统一串行化，避免引擎并发
        const result = await this.enqueueOrchestratorExecution(effectivePrompt, imagePaths);
        success = result.success;
        failureReason = result.error;
      } else {
        // 直接执行模式：指定 Worker 直接执行
        const result = await this.directExecutor.execute(effectivePrompt, forceWorker || this.selectedWorker!, imagePaths);
        success = result.success;
        failureReason = result.error;
      }
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
        const isAbort = !success && failureReason && isAbortError(failureReason);
        if (success) {
          this.messageHub.sendControl(ControlMessageType.TASK_COMPLETED, {
            requestId: requestKey,
            timestamp: Date.now(),
          });
        } else if (isAbort) {
          // 中断场景：仅发送 TASK_FAILED 控制消息用于状态流转，不发送用户可见的错误消息
          this.messageHub.sendControl(ControlMessageType.TASK_FAILED, {
            requestId: requestKey,
            error: '任务已中断',
            timestamp: Date.now(),
          });
        } else {
          this.messageHub.sendControl(ControlMessageType.TASK_FAILED, {
            requestId: requestKey,
            error: failureReason || '执行失败',
            timestamp: Date.now(),
          });
          if (!rejected && failureReason) {
            const traceId = this.messageHub.getTraceId();
            const errorMessage = createErrorMessage(
              failureReason,
              'orchestrator',
              'orchestrator',
              traceId,
              { metadata: { requestId: requestKey } }
            );
            this.messageHub.sendMessage(errorMessage);
          }
        }
      } else if (!rejected && failureReason && requestKey) {
        this.messageHub.taskRejected(requestKey, failureReason);
      }
      this.messageHub.finalizeRequestContext(requestKey);
      this.messageHub.setRequestContext(undefined);
      this.clearRequestTimeout(requestKey);
      // 任务执行链路结束，强制重置 processing 状态
      // 避免因流式消息缺少 COMPLETED lifecycle 导致 processing 动画卡住
      this.messageHub.forceProcessingState(false);
    }
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
  private async executeWithOrchestrator(prompt: string, imagePaths: string[]): Promise<OrchestratorExecutionResult> {
    logger.info('界面.执行.模式.编排', undefined, LogCategory.UI);

    // 🔧 初始分析消息已由 MissionDrivenEngine.sendPhaseMessage 统一发送
    // 不再在这里重复发送，避免用户看到两条类似的"正在分析"消息

    let errorMsg: string | undefined;
    let success = false;
    try {
      // 调用智能编排器
      // 注意：executeWithTaskContext 内部已将 LLM 响应流式发送到前端
      // 因此不需要再手动调用 sendOrchestratorMessage 发送结果，否则会导致重复消息
      const taskContext = await this.orchestratorEngine.executeWithTaskContext(prompt, this.activeSessionId || undefined, imagePaths);
      const result = taskContext.result;

      logger.info('界面.任务.完成', { hasResult: !!result?.trim(), resultLength: result?.length || 0 }, LogCategory.UI);

      // 保存消息历史
      this.saveMessageToSession(prompt, result, undefined, 'orchestrator');

      // 🔧 移除误判的安全检查
      // 原因：LLM 流式输出通过 Normalizer -> MessageHub 直接发送到前端
      // 统计机制依赖 requestId 注入，但流式消息可能在 requestId 设置前就开始发送
      // 导致 assistantThreadContent 统计为 0，误触发强制补发，产生重复消息
      //
      // 正确的行为：
      // - 如果 LLM 有输出（result 非空），消息已经通过流式通道发送
      // - 如果 LLM 无输出（result 为空），应该在 engine 层面处理，而非此处补发

      success = true;
    } catch (error) {
      // 中断导致的 abort 错误静默处理，不向前端发送错误消息
      if (isAbortError(error)) {
        logger.info('界面.执行.智能.中断', undefined, LogCategory.UI);
        success = false;
      } else {
        logger.error('界面.执行.智能.失败', error, LogCategory.UI);
        errorMsg = error instanceof Error ? error.message : String(error);
        this.sendOrchestratorMessage({
          content: errorMsg,
          messageType: 'error',
        });
        success = false;
      }
    }

    this.sendStateUpdate();
    if (!success) {
      return { success: false, error: errorMsg };
    }
    return { success: true };
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
  private performSessionDelete(sessionId: string): void {
    if (this.sessionManager.deleteSession(sessionId)) {
      // 如果删除后没有会话，创建一个新的
      if (this.sessionManager.getSessionMetas().length === 0) {
        const newSession = this.sessionManager.createSession();
        this.activeSessionId = newSession.id;
        this.sendData('sessionCreated', { session: newSession });
      }
      this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() });
      this.sendToast('会话已删除', 'info');
    }
    this.sendStateUpdate();
  }

  private sendCurrentSessionToWebview(): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      return;
    }
    this.sendData('sessionLoaded', { session: session });
  }

  /** 创建并切换到新会话（对齐任务/对话会话） */
  public async createNewSession(): Promise<void> {
    await this.handleNewSession();
  }

  /** 处理新会话创建流程 */
  private async handleNewSession(): Promise<void> {
    // 创建新会话前，先中断当前任务
    await this.interruptCurrentTask({ silent: true });
    // 创建新会话时，重置所有适配器
    await this.adapterFactory.shutdown();
    const newSession = this.sessionManager.createSession();
    // 更新活跃会话ID
    this.activeSessionId = newSession.id;
    logger.info('界面.会话.已创建', { sessionId: this.activeSessionId }, LogCategory.UI);
    // 通知 webview 新会话已创建
    this.sendData('sessionCreated', { session: newSession });
    this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() });
    this.sendStateUpdate();
  }

  /** 切换到指定会话 */
  private async switchToSession(sessionId: string): Promise<void> {
    await this.adapterFactory.shutdown();
    this.activeSessionId = sessionId;
    this.ensureSessionExists(sessionId);

    // 获取会话完整数据
    const session = this.sessionManager.getSession(sessionId);
    if (session) {
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
        const formatted = {
          id: m.id,
          role: m.role,
          content: m.content,
          source: m.source || 'orchestrator',
          timestamp: m.timestamp,
          agent: m.agent,
        };

        // 根据 source 和 agent 分类
        if (m.source === 'worker' && m.agent) {
          const agentKey = m.agent as 'claude' | 'codex' | 'gemini';
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

  /** 保存消息到当前会话 */
  private saveMessageToSession(
    userPrompt: string,
    assistantResponse: string,
    agent?: WorkerSlot,
    source?: MessageSource
  ): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      return;
    }
    if (assistantResponse) {
      this.sessionManager.addMessage('assistant', assistantResponse, agent, source);
      void this.orchestratorEngine.recordContextMessage('assistant', assistantResponse, this.activeSessionId || undefined);
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

    const seen = new Set<string>();
    const sessionMessages = messages.map((m) => {
      const id = typeof m?.id === 'string' && m.id.trim() ? m.id.trim() : '';
      if (!id) {
        throw new Error('[WebviewProvider] Session message 缺少 id');
      }
      if (seen.has(id)) {
        throw new Error(`[WebviewProvider] Session message id 重复: ${id}`);
      }
      seen.add(id);
      const role = m?.role;
      if (role !== 'user' && role !== 'assistant' && role !== 'system') {
        throw new Error(`[WebviewProvider] Session message role 无效: ${String(role)}`);
      }
      if (typeof m?.content !== 'string') {
        throw new Error('[WebviewProvider] Session message content 非字符串');
      }
      if (typeof m?.timestamp !== 'number') {
        throw new Error('[WebviewProvider] Session message timestamp 无效');
      }
      return {
        id,
        role,
        content: m.content,
        agent: m.agent,
        timestamp: m.timestamp,
        images: m.images,
        source: m.source,
      };
    });

    // 使用新的 API 保存会话数据
    this.sessionManager.updateSessionData(currentSession.id, sessionMessages);  // ✅ 移除 cliOutputs 参数
    logger.info('界面.会话.保存.完成', { messageCount: sessionMessages.length }, LogCategory.UI);
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
        name: tv.goal || tv.prompt,
        prompt: tv.prompt,
        description: tv.goal,
        status: tv.status,
        priority: tv.priority,
        subTasks: tv.subTasks,
        createdAt: tv.createdAt,
        startedAt: tv.startedAt,
        completedAt: tv.completedAt,
        progress: tv.progress,
        missionId: tv.missionId,
      }));
    }

    this.assertValidArray<any>(tasks, 'uiState.tasks');
    const currentTask = tasks.find(t => t?.status === 'running') ?? tasks[tasks.length - 1];

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

    const isRunning = currentTask?.status === 'running' || this.orchestratorEngine.running;
    const pendingChanges = this.snapshotManager.getPendingChanges();
    this.assertValidArray<any>(pendingChanges, 'uiState.pendingChanges');
    const logs = this.logs;
    this.assertValidArray<LogEntry>(logs, 'uiState.logs');

    return {
      currentSessionId: this.activeSessionId ?? currentSession?.id,
      sessions: sessionMetas,
      currentTask,
      tasks,
      workerStatuses,
      pendingChanges,
      isRunning,
      logs,
      interactionMode: this.orchestratorEngine.getInteractionMode(),
      interactionModeUpdatedAt: this.interactionModeUpdatedAt,
      orchestratorPhase: this.orchestratorEngine.phase,
      activePlan: undefined,
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
      const message = `Svelte webview 未构建: ${templatePath}`;
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

    // 注入初始 sessionId
    const initialSessionId = this.activeSessionId || '';
    const sessionScript = `<script>window.__INITIAL_SESSION_ID__ = "${initialSessionId}";</script>`;
    html = html.replace('</head>', `${sessionScript}\n  </head>`);

    logger.debug('界面.Svelte.已加载', { sessionId: initialSessionId }, LogCategory.UI);
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

      // 5. 清理 Webview
      this._view = undefined;

      logger.info('界面.销毁.完成', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.销毁.失败', error, LogCategory.UI);
    }
  }
}
