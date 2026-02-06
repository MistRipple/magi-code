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
import { AgentType } from '../types/agent-types';
import {
  StandardMessage,
  StreamUpdate,
  MessageLifecycle,
  MessageType,
  MessageCategory,
  ContentBlock,
  MessageMetadata,
  DataMessageType,
  NotifyLevel,
  ControlMessageType,
  InteractionType,
  createStandardMessage,
  createUserInputMessage,
  createStreamingMessage,
  createErrorMessage,
  createInteractionMessage,
} from '../protocol/message-protocol';
import { ADAPTER_EVENTS, PROCESSING_EVENTS, WEBVIEW_MESSAGE_TYPES } from '../protocol/event-names';
import { UnifiedSessionManager } from '../session';
import { TaskView } from '../task/task-view-adapter';
import { SnapshotManager } from '../snapshot-manager';
import { DiffGenerator } from '../diff-generator';
import { globalEventBus } from '../events';
import { IAdapterFactory } from '../adapters/adapter-factory-interface';
import { LLMAdapterFactory } from '../llm/adapter-factory';
import { MissionDrivenEngine } from '../orchestrator/core';
import { MessageHub } from '../orchestrator/core/message-hub';
import { WorkerAssignmentStorage, WORKER_ASSIGNMENTS_VERSION, CATEGORY_DEFINITIONS, CATEGORY_RULES } from '../orchestrator/profile';
import { parseContentToBlocks } from '../utils/content-parser';
import { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import { InstructionSkillDefinition } from '../tools/skills-manager';
import { applySkillInstall, buildInstructionSkillPrompt } from '../tools/skill-installation';
import { loadAceConfigFromFile } from '../tools/tool-manager';
import { MermaidPanel } from './mermaid-panel';
// Mission-Driven Architecture 类型 - 直接从子模块导入
import {
  MissionOrchestrator,
  ExecutionProgress,
  MissionSummary,
  MissionVerificationResult,
} from '../orchestrator/core';
import { BlockedItem } from '../orchestrator/core/executors/blocking-manager';
import { Mission, Assignment } from '../orchestrator/mission';
import type { UnifiedTodo } from '../todo/types';

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
            messageId: (next as any).message?.id,
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
  public static readonly viewType = 'multiCli.mainView';

  private readonly MAX_REASONABLE_ARRAY_LENGTH = 1_000_000;

  private _view?: vscode.WebviewView;
  private sessionManager: UnifiedSessionManager;
  private snapshotManager: SnapshotManager;
  private diffGenerator: DiffGenerator;
  private readonly messageFlowLogEnabled = process.env.MULTICLI_MESSAGE_FLOW_LOG === '1';
  private readonly messageFlowLogPath: string;
  private webviewMessageBus: WebviewMessageBus;

  // 统一消息出口
  private messageHub: MessageHub;
  private requestTimeouts: Map<string, NodeJS.Timeout> = new Map();

  // 适配器工厂（LLM 模式）
  private adapterFactory: IAdapterFactory;

  // 编排引擎
  private orchestratorEngine: MissionDrivenEngine;

  // Mission-Driven 编排器（新架构）- MissionExecutor 已合并到 MissionOrchestrator
  private missionOrchestrator?: MissionOrchestrator;

  // 项目知识库
  private projectKnowledgeBase?: ProjectKnowledgeBase;

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
  // 工具授权回调（按请求 ID 管理，避免并发覆盖）
  private toolAuthorizationCallbacks = new Map<string, (allowed: boolean) => void>();
  private toolAuthorizationQueue: Array<{ requestId: string; toolName: string; toolArgs: any }> = [];
  private activeToolAuthorizationRequestId: string | null = null;
  private activeToolAuthorizationTimer: NodeJS.Timeout | null = null;
  private readonly toolAuthorizationTimeoutMs = 60000;

  // 当前选择的 Worker（null 表示自动选择/智能编排）
  private selectedWorker: WorkerSlot | null = null;
  private recentRequestIds: Map<string, number> = new Map();

  // 模型连接状态缓存（避免频繁真实请求）
  private workerStatusCache: Record<string, { status: string; model?: string; error?: string }> | null = null;
  private workerStatusCacheAt = 0;
  private interactionModeUpdatedAt = 0;
  private workerStatusInFlight: Promise<void> | null = null;
  private readonly workerStatusCacheTtlMs = 30000;
  private readonly workerStatusSoftTtlMs = 120000;
  private readonly workerStatusTimeoutMs = 4000;
  private readonly workerStatusHardTimeoutMs = 10000;

  private streamingContextCache: Map<string, { content: string; lastFlushAt: number }> = new Map();
  private messageMetaCache: Map<string, { type?: MessageType; metadata?: Record<string, unknown> | MessageMetadata }> = new Map();
  private lastRecordedContextBySession: Map<string, { content: string; at: number }> = new Map();
  private readonly streamingContextFlushMs = 1200;
  private readonly contextDedupeWindowMs = 10000;

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
    this.webviewMessageBus = new WebviewMessageBus(
      () => this._view,
      this.getWebviewMessagePriority.bind(this),
      COALESCE_MESSAGE_TYPES
    );

    // MessageHub 由 MissionDrivenEngine 创建，初始化后再绑定监听

    // 初始化统一会话管理器
    this.sessionManager = new UnifiedSessionManager(workspaceRoot);
    // 统一任务管理器（按会话初始化）
    this.snapshotManager = new SnapshotManager(this.sessionManager, workspaceRoot);
    this.diffGenerator = new DiffGenerator(this.sessionManager, workspaceRoot);

    // 确保有当前会话
    this.ensureSessionAlignment();

    const config = vscode.workspace.getConfiguration('multiCli');
    const timeout = config.get<number>('timeout') ?? 300000;
    const idleTimeout = config.get<number>('idleTimeout') ?? 120000;
    const maxTimeout = config.get<number>('maxTimeout') ?? 900000;
    const permissions = this.normalizePermissions(config.get<Partial<PermissionMatrix>>('permissions'));
    const strategy = this.normalizeStrategy(config.get<Partial<StrategyConfig>>('strategy'));

    // 初始化 LLM 适配器工厂
    this.adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });

    // 设置错误事件处理（消息由 Adapter 直接发送到 MessageHub，不再通过事件转发）
    this.setupAdapterEvents();

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

    // 异步初始化 profile loader（在 MessageHub 注入之后）
    void (this.adapterFactory as LLMAdapterFactory).initialize().catch(err => {
      logger.error('Failed to initialize LLM adapter factory', { error: err.message }, LogCategory.LLM);
    });

    // 初始化编排引擎（设置 IntentGate 等关键组件）
    void this.orchestratorEngine.initialize().catch(err => {
      logger.error('Failed to initialize orchestrator engine', { error: err.message }, LogCategory.ORCHESTRATOR);
    });

    this.interactionModeUpdatedAt = Date.now();

    this.setupMessageHubListeners();
    this.orchestratorEngine.setExtensionContext(this.context);
    // 设置 Hard Stop 确认回调
    this.setupOrchestratorConfirmation();
    this.setupOrchestratorQuestions();

    // 初始化项目知识库
    this.initializeProjectKnowledgeBase();

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

  private generateEntityId(prefix: string): string {
    return `${prefix}_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
  }

  private asArray<T>(value: T[] | undefined | null): T[] {
    return Array.isArray(value) ? value : [];
  }

  private normalizeTodo(rawTodo: UnifiedTodo | undefined | null, assignmentId: string, seen: Set<string>): UnifiedTodo | null {
    if (!rawTodo || typeof rawTodo !== 'object') {
      return null;
    }
    const rawId = typeof rawTodo.id === 'string' ? rawTodo.id.trim() : '';
    let id = rawId || this.generateEntityId('todo');
    while (seen.has(id)) {
      id = this.generateEntityId('todo');
    }
    seen.add(id);
    return {
      ...rawTodo,
      id,
      assignmentId: rawTodo.assignmentId || assignmentId,
    };
  }

  private normalizeAssignments(rawAssignments: Assignment[] | undefined | null): Assignment[] {
    const assignments: Assignment[] = [];
    const seen = new Set<string>();
    for (const raw of this.asArray(rawAssignments)) {
      if (!raw || typeof raw !== 'object') continue;
      const rawId = typeof raw.id === 'string' ? raw.id.trim() : '';
      let id = rawId || this.generateEntityId('assignment');
      while (seen.has(id)) {
        id = this.generateEntityId('assignment');
      }
      seen.add(id);
      const todoSeen = new Set<string>();
      const todos = this.asArray(raw.todos)
        .map(todo => this.normalizeTodo(todo, id, todoSeen))
        .filter((todo): todo is UnifiedTodo => Boolean(todo));
      assignments.push({
        ...raw,
        id,
        todos,
      });
    }
    return assignments;
  }

  private normalizeTodos(rawTodos: UnifiedTodo[] | undefined | null, assignmentId: string): UnifiedTodo[] {
    const seen = new Set<string>();
    return this.asArray(rawTodos)
      .map(todo => this.normalizeTodo(todo, assignmentId, seen))
      .filter((todo): todo is UnifiedTodo => Boolean(todo));
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

      // 设置压缩模型客户端（用于自动知识提取）
      await this.setupKnowledgeExtractionClient();

      // 注入知识库到编排器
      this.orchestratorEngine.setKnowledgeBase(this.projectKnowledgeBase);

      // 监听任务完成事件，自动提取知识
      this.setupAutoKnowledgeExtraction();

      const codeIndex = this.projectKnowledgeBase.getCodeIndex();
      logger.info('项目知识库.已初始化', {
        files: codeIndex ? codeIndex.files.length : 0
      }, LogCategory.SESSION);
    } catch (error: any) {
      logger.error('项目知识库.初始化失败', { error: error.message }, LogCategory.SESSION);
    }
  }

  /**
   * 设置知识提取客户端（使用压缩模型）
   */
  private async setupKnowledgeExtractionClient(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const { UniversalLLMClient } = await import('../llm/clients/universal-client');

      // 加载压缩模型配置
      const compressorConfig = LLMConfigLoader.loadCompressorConfig();
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();

      const useCompressor = compressorConfig.enabled
        && Boolean(compressorConfig.baseUrl && compressorConfig.model);
      const activeConfig = useCompressor ? compressorConfig : orchestratorConfig;
      const activeLabel = useCompressor ? 'compressor' : 'orchestrator';

      // 创建压缩模型客户端
      const baseClient = new UniversalLLMClient({
        baseUrl: activeConfig.baseUrl,
        apiKey: activeConfig.apiKey,
        model: activeConfig.model,
        provider: activeConfig.provider,
        enabled: true,
      });
      const executionStats = this.orchestratorEngine.getExecutionStats();
      const client = {
        config: baseClient.config,
        sendMessage: async (params: any) => {
          const startedAt = Date.now();
          try {
            const response = await baseClient.sendMessage(params);
            const duration = Date.now() - startedAt;
            if (executionStats) {
              executionStats.recordExecution({
                worker: activeLabel as any,
                taskId: 'knowledge',
                subTaskId: 'extract',
                success: true,
                duration,
                inputTokens: response.usage?.inputTokens,
                outputTokens: response.usage?.outputTokens,
                phase: 'integration',
              });
            }
            return response;
          } catch (error: any) {
            const duration = Date.now() - startedAt;
            if (executionStats) {
              executionStats.recordExecution({
                worker: activeLabel as any,
                taskId: 'knowledge',
                subTaskId: 'extract',
                success: false,
                duration,
                error: error?.message,
                phase: 'integration',
              });
            }
            throw error;
          }
        },
        streamMessage: async (params: any, onChunk: any) => {
          const startedAt = Date.now();
          try {
            const response = await baseClient.streamMessage(params, onChunk);
            const duration = Date.now() - startedAt;
            if (executionStats) {
              executionStats.recordExecution({
                worker: activeLabel as any,
                taskId: 'knowledge',
                subTaskId: 'extract',
                success: true,
                duration,
                inputTokens: response.usage?.inputTokens,
                outputTokens: response.usage?.outputTokens,
                phase: 'integration',
              });
            }
            return response;
          } catch (error: any) {
            const duration = Date.now() - startedAt;
            if (executionStats) {
              executionStats.recordExecution({
                worker: activeLabel as any,
                taskId: 'knowledge',
                subTaskId: 'extract',
                success: false,
                duration,
                error: error?.message,
                phase: 'integration',
              });
            }
            throw error;
          }
        },
        testConnection: baseClient.testConnection.bind(baseClient),
      } as import('../llm/types').LLMClient;

      const knowledgeBase = this.projectKnowledgeBase;
      if (!knowledgeBase) {
        logger.warn('项目知识库.压缩模型客户端.未设置_知识库未初始化', undefined, LogCategory.SESSION);
        return;
      }

      // 设置到知识库
      knowledgeBase.setLLMClient(client);

      logger.info('项目知识库.压缩模型客户端.已设置', {
        model: activeConfig.model,
        provider: activeConfig.provider,
        fallbackToOrchestrator: !useCompressor
      }, LogCategory.SESSION);
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
        logger.info('项目知识库.ADR提取成功', {
          count: adrs.length,
          titles: adrs.map(a => a.title)
        }, LogCategory.SESSION);

        // 通知前端
        this.sendToast(`自动提取了 ${adrs.length} 条架构决策记录`, 'success');

        // 刷新知识库显示
        await this.handleGetProjectKnowledge();
      }

      // 提取 FAQ
      const faqs = await knowledgeBase.extractFAQFromSession(messages);
      if (faqs.length > 0) {
        logger.info('项目知识库.FAQ提取成功', {
          count: faqs.length,
          questions: faqs.map(f => f.question)
        }, LogCategory.SESSION);

        // 通知前端
        this.sendToast(`自动提取了 ${faqs.length} 条常见问题`, 'success');

        // 刷新知识库显示
        await this.handleGetProjectKnowledge();
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

  /**
   * 设置 LLM 适配器错误事件监听
   *
   * 消息流已简化为 4 层架构：
   * Layer 1: Normalizer.emit('message')
   * Layer 2: Adapter → messageBus.sendMessage() [直接调用]
   * Layer 3: MessageBus → emit('message')
   * Layer 4: WebviewProvider.setupMessageBusListeners() → postMessage()
   *
   * 此方法只处理错误事件，消息由 Adapter 直接发送到 MessageBus
   */
  private setupAdapterEvents(): void {
    // 只监听错误事件
    this.adapterFactory.on(ADAPTER_EVENTS.ERROR, (error: Error) => {
      logger.error('适配器错误', { error: error.message }, LogCategory.LLM);
    });
  }

  /** 设置 MessageHub 事件监听，统一转发到前端 */
  private setupMessageHubListeners(): void {
    // 标准流式消息（LLM / 编排 / Worker 等统一通道）
    this.messageHub.on('unified:message', (message) => {
      this.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_MESSAGE,  // 🔧 统一消息通道：使用新协议
        message,
        sessionId: this.activeSessionId
      } as any);
      this.logMessageFlow('messageHub.standardMessage [SENT]', message);
      this.resolveRequestTimeoutFromMessage(message);
    });

    this.messageHub.on('unified:update', (update) => {
      this.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_UPDATE,  // 🔧 统一消息通道：使用新协议
        update,
        sessionId: this.activeSessionId
      } as any);
      this.logMessageFlow('messageHub.standardUpdate [SENT]', update);
    });

    this.messageHub.on('unified:complete', (message) => {
      this.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_COMPLETE,  // 🔧 统一消息通道：使用新协议
        message,
        sessionId: this.activeSessionId
      } as any);
      this.logMessageFlow('messageHub.standardComplete [SENT]', message);
      this.resolveRequestTimeoutFromMessage(message);
    });

    // ProcessingState 权威来源（同步 UI loading 状态）
    this.messageHub.on(PROCESSING_EVENTS.STATE_CHANGED, (state) => {
      this.sendData('processingStateChanged', {
        isProcessing: state.isProcessing,
        source: state.source,
        agent: state.agent,
        startedAt: state.startedAt,
      });
    });
  }

  private emitUserAndPlaceholder(requestId: string, prompt: string, imageCount: number, images?: Array<{ dataUrl: string }>): {
    userMessageId: string;
    placeholderMessageId: string;
  } {
    const traceId = this.messageHub.getTraceId();
    // 🔧 图片已通过缩略图展示，不再在文本中附加 [附件: X 张图片]
    const displayContent = prompt;

    const userMessage = createUserInputMessage(displayContent, traceId, {
      metadata: {
        requestId,
        sendingAnimation: true,
        // 🔧 新增：附带图片数据供前端展示
        images: images && images.length > 0 ? images : undefined,
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
  }

  private resolveRequestTimeoutFromMessage(message: StandardMessage): void {
    const meta = message.metadata as Record<string, unknown> | undefined;
    const requestId = meta?.requestId as string | undefined;
    if (!requestId) {
      return;
    }
    const isPlaceholder = Boolean(meta?.isPlaceholder);
    // 方案 B：使用 MessageType.USER_INPUT 判断用户消息
    const isUserInput = message.type === MessageType.USER_INPUT;
    if (isPlaceholder || isUserInput) {
      return;
    }
    this.clearRequestTimeout(requestId);
  }

  private cacheMessageMeta(message: StandardMessage): void {
    if (!message?.id) {
      return;
    }
    this.messageMetaCache.set(message.id, {
      type: message.type,
      metadata: message.metadata || undefined,
    });
  }

  private isRecordableMessage(type?: MessageType, metadata?: Record<string, unknown> | MessageMetadata): boolean {
    if (metadata && (metadata as any).isStatusMessage) {
      return false;
    }
    if (!type) {
      return true;
    }
    return [
      MessageType.TEXT,
      MessageType.RESULT,
      MessageType.ERROR,
      MessageType.THINKING,
      MessageType.INTERACTION,
    ].includes(type);
  }

  private extractTextFromBlocks(blocks: ContentBlock[] | undefined): string {
    if (!blocks || blocks.length === 0) {
      return '';
    }
    return blocks
      .filter((block) => block.type === 'text' || block.type === 'thinking')
      .map((block) => (block as any).content || '')
      .filter(Boolean)
      .join('\n');
  }

  private updateStreamingBuffer(messageId: string, nextContent: string, updateType: 'append' | 'replace'): void {
    if (!messageId || !nextContent) {
      return;
    }
    const entry = this.streamingContextCache.get(messageId);
    const content = updateType === 'append' && entry ? entry.content + nextContent : nextContent;
    this.streamingContextCache.set(messageId, {
      content,
      lastFlushAt: entry?.lastFlushAt || 0,
    });
  }

  private recordStreamingContext(message: StandardMessage): void {
    if (!message?.id) {
      return;
    }
    if (!this.isRecordableMessage(message.type, message.metadata)) {
      return;
    }
    const content = this.extractTextFromBlocks(message.blocks);
    if (!content) {
      return;
    }
    this.updateStreamingBuffer(message.id, content, 'replace');
    this.flushStreamingContext(message.id);
  }

  private recordStreamingUpdate(update: StreamUpdate): void {
    if (!update?.messageId) {
      return;
    }
    const meta = this.messageMetaCache.get(update.messageId);
    if (!this.isRecordableMessage(meta?.type, meta?.metadata)) {
      return;
    }

    let content = '';
    let updateType: 'append' | 'replace' = 'replace';
    if (update.updateType === 'append' && update.appendText) {
      content = update.appendText;
      updateType = 'append';
    } else if (update.blocks) {
      content = this.extractTextFromBlocks(update.blocks);
      updateType = 'replace';
    }

    if (!content) {
      return;
    }

    this.updateStreamingBuffer(update.messageId, content, updateType);
    this.flushStreamingContext(update.messageId);
  }

  private flushStreamingContext(messageId: string): void {
    const entry = this.streamingContextCache.get(messageId);
    if (!entry) {
      return;
    }
    const now = Date.now();
    if (now - entry.lastFlushAt < this.streamingContextFlushMs) {
      return;
    }
    entry.lastFlushAt = now;
    const sessionId = this.activeSessionId || undefined;
    void this.orchestratorEngine.recordStreamingMessage(
      messageId,
      'assistant',
      entry.content,
      sessionId
    );
  }

  private recordFinalContext(message: StandardMessage): void {
    if (!message?.id) {
      return;
    }
    this.streamingContextCache.delete(message.id);
    this.orchestratorEngine.clearStreamingMessage(message.id);

    if (!this.isRecordableMessage(message.type, message.metadata)) {
      return;
    }
    const content = this.extractTextFromBlocks(message.blocks);
    if (!content) {
      return;
    }

    const sessionId = this.activeSessionId || 'default';
    const lastRecord = this.lastRecordedContextBySession.get(sessionId);
    const now = Date.now();
    if (lastRecord && lastRecord.content === content && now - lastRecord.at < this.contextDedupeWindowMs) {
      return;
    }
    this.lastRecordedContextBySession.set(sessionId, { content, at: now });
    void this.orchestratorEngine.recordContextMessage('assistant', content, sessionId);
  }

  private recordToolOutputsIfAny(message: StandardMessage): void {
    if (!message?.blocks || message.blocks.length === 0) {
      return;
    }
    const toolBlocks = message.blocks.filter((block) => block.type === 'tool_call') as Array<{
      toolName?: string;
      status?: string;
      output?: string;
      error?: string;
    }>;
    if (toolBlocks.length === 0) {
      return;
    }
    const sessionId = this.activeSessionId || undefined;
    for (const tool of toolBlocks) {
      const toolName = tool.toolName || 'tool';
      if (tool.status === 'completed' && tool.output) {
        void this.orchestratorEngine.recordToolOutput(toolName, tool.output, sessionId);
      } else if (tool.status === 'failed' && tool.error) {
        void this.orchestratorEngine.recordToolOutput(toolName, `Error: ${tool.error}`, sessionId);
      }
    }
  }

  /** 设置智能编排器的 Hard Stop 确认回调 */
  private setupOrchestratorConfirmation(): void {
    // 设置 Hard Stop 确认回调
    this.orchestratorEngine.setConfirmationCallback(async (plan, formattedPlan) => {
      const mode = this.orchestratorEngine.getInteractionMode();
      if (mode === 'auto') {
        logger.info('界面.编排器.确认.自动_跳过', { mode }, LogCategory.UI);
        return true;
      }
      return new Promise<boolean>((resolve, reject) => {
        // 保存 resolve/reject 以便后续处理用户响应
        this.pendingConfirmation = { resolve, reject };

        // 🔧 P3: 发送交互消息到主对话区
        const traceId = this.messageHub.getTraceId();
        const interactionMsg = createInteractionMessage(
          {
            type: InteractionType.PLAN_CONFIRMATION,
            requestId: `confirm-${Date.now()}`,
            prompt: '请确认执行计划',
            required: true
          },
          'orchestrator',
          'orchestrator',
          traceId,
          {
            blocks: [{ type: 'text', content: formattedPlan, isMarkdown: true }]
          }
        );
        this.messageHub.sendMessage(interactionMsg);

        // 发送确认请求消息 (触发 UI 状态/弹窗)
        this.sendData('confirmationRequest', {
          plan: plan,
          formattedPlan: formattedPlan,
        });

        logger.info('界面.编排器.确认.等待', { mode }, LogCategory.UI);
      });
    });

    // 设置恢复确认回调
    this.orchestratorEngine.setRecoveryConfirmationCallback(async (failedTask, error, options) => {
      return new Promise<'retry' | 'rollback' | 'continue'>((resolve) => {
        // 保存 resolver
        this.recoveryConfirmationResolver = resolve;

        // 🔧 P3: 发送交互消息到主对话区
        const traceId = this.messageHub.getTraceId();
        const interactionMsg = createInteractionMessage(
          {
            type: InteractionType.QUESTION, // 复用 QUESTION 类型或新增 RECOVERY 类型
            requestId: `recovery-${Date.now()}`,
            prompt: `任务执行出错: ${error}\n\n请选择恢复策略：`,
            options: [
              options.retry ? { value: 'retry', label: '重试' } : null,
              options.rollback ? { value: 'rollback', label: '回滚' } : null,
              { value: 'continue', label: '继续(跳过)' }
            ].filter(Boolean) as any,
            required: true
          },
          'orchestrator',
          'orchestrator',
          traceId
        );
        this.messageHub.sendMessage(interactionMsg);

        // 发送恢复请求到 Webview
        this.sendData('recoveryRequest', {
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
    this.orchestratorEngine.setQuestionCallback(async (questions, plan) => {
      return new Promise<string | null>((resolve, reject) => {
        this.pendingQuestion = { resolve, reject };

        // 🔧 P3: 发送交互消息
        const traceId = this.messageHub.getTraceId();
        const interactionMsg = createInteractionMessage(
          {
            type: InteractionType.QUESTION,
            requestId: `question-${Date.now()}`,
            prompt: '需要补充以下信息：\n' + questions.join('\n'),
            required: true
          },
          'orchestrator',
          'orchestrator',
          traceId
        );
        this.messageHub.sendMessage(interactionMsg);

        this.sendData('questionRequest', {
          questions,
          plan
        });
        logger.info('界面.编排器.提问.等待', undefined, LogCategory.UI);
      });
    });

    // 设置需求澄清回调
    this.orchestratorEngine.setClarificationCallback(async (questions, context, ambiguityScore, originalPrompt) => {
      return new Promise((resolve, reject) => {
        this.pendingClarification = { resolve, reject };

        // 🔧 P3: 发送交互消息
        const traceId = this.messageHub.getTraceId();
        const interactionMsg = createInteractionMessage(
          {
            type: InteractionType.CLARIFICATION,
            requestId: `clarify-${Date.now()}`,
            prompt: '需求存在歧义，请澄清：\n' + questions.join('\n'),
            required: true
          },
          'orchestrator',
          'orchestrator',
          traceId
        );
        this.messageHub.sendMessage(interactionMsg);

        this.sendData('clarificationRequest', {
          questions,
          context,
          ambiguityScore,
          originalPrompt,
          sessionId: this.activeSessionId
        });
        logger.info('界面.编排器.澄清.等待', { ambiguityScore }, LogCategory.UI);
      });
    });

    // 设置 Worker 问题回调
    this.orchestratorEngine.setWorkerQuestionCallback(async (workerId, question, context, options) => {
      return new Promise((resolve, reject) => {
        this.pendingWorkerQuestion = { resolve, reject };

        // 🔧 P3: 发送交互消息
        const traceId = this.messageHub.getTraceId();
        const optionItems = options?.map(o => ({ value: o, label: o })) || [];
        const interactionMsg = createInteractionMessage(
          {
            type: InteractionType.QUESTION,
            requestId: `worker-q-${Date.now()}`,
            prompt: `**${workerId}** 需要确认：\n${question}`,
            options: optionItems.length > 0 ? optionItems : undefined,
            required: true
          },
          'orchestrator',
          'orchestrator',
          traceId
        );
        this.messageHub.sendMessage(interactionMsg);

        this.sendData('workerQuestionRequest', {
          workerId,
          question,
          context,
          options,
          sessionId: this.activeSessionId
        });
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
      this.sendToast(
        confirmed ? '执行计划已确认，开始执行...' : '执行计划已取消',
        confirmed ? 'success' : 'info'
      );
    }
  }

  /** 处理用户补充问题的回答 */
  private handleQuestionAnswer(answer: string | null): void {
    if (this.pendingQuestion) {
      const normalized = answer && answer.trim().length > 0 ? answer.trim() : null;
      this.pendingQuestion.resolve(normalized);
      this.pendingQuestion = null;
      this.sendToast(
        normalized ? '已提交问题回答，继续分析...' : '已取消问题补充',
        normalized ? 'success' : 'info'
      );
    }
  }

  /** 处理用户澄清回答 */
  private handleClarificationAnswer(answers: Record<string, string> | null, additionalInfo?: string, autoSkipped = false): void {
    if (this.pendingClarification) {
      if (answers && Object.keys(answers).length > 0) {
        this.pendingClarification.resolve({ answers, additionalInfo });
        this.sendToast('已提交澄清信息，继续分析...', 'success');
      } else {
        this.pendingClarification.resolve(null);
        // auto 模式下静默跳过，不显示 toast
        if (!autoSkipped) {
          this.sendToast('已跳过澄清，使用原始需求...', 'info');
        }
      }
      this.pendingClarification = null;
    }
  }

  /** 处理 Worker 问题回答 */
  private handleWorkerQuestionAnswer(answer: string | null): void {
    if (this.pendingWorkerQuestion) {
      this.pendingWorkerQuestion.resolve(answer);
      this.pendingWorkerQuestion = null;
      this.sendToast(
        answer ? '已回答 Worker 问题，继续执行...' : '已跳过 Worker 问题...',
        answer ? 'success' : 'info'
      );
    }
  }

  private clearActiveToolAuthorizationTimer(): void {
    if (this.activeToolAuthorizationTimer) {
      clearTimeout(this.activeToolAuthorizationTimer);
      this.activeToolAuthorizationTimer = null;
    }
  }

  private pumpToolAuthorizationQueue(): void {
    if (this.activeToolAuthorizationRequestId) {
      return;
    }
    const next = this.toolAuthorizationQueue.shift();
    if (!next) {
      return;
    }

    this.activeToolAuthorizationRequestId = next.requestId;
    this.sendData('toolAuthorizationRequest', {
      requestId: next.requestId,
      toolName: next.toolName,
      toolArgs: next.toolArgs,
    });

    this.clearActiveToolAuthorizationTimer();
    this.activeToolAuthorizationTimer = setTimeout(() => {
      const requestId = this.activeToolAuthorizationRequestId;
      if (!requestId) {
        return;
      }
      const callback = this.toolAuthorizationCallbacks.get(requestId);
      if (callback) {
        logger.warn('界面.工具授权.响应超时', { requestId }, LogCategory.UI);
        this.toolAuthorizationCallbacks.delete(requestId);
        callback(false);
      }
      this.activeToolAuthorizationRequestId = null;
      this.activeToolAuthorizationTimer = null;
      this.pumpToolAuthorizationQueue();
    }, this.toolAuthorizationTimeoutMs);
  }

  /** 处理工具授权响应 */
  private handleToolAuthorizationResponse(requestId: string | undefined, allowed: boolean): void {
    if (!requestId) {
      logger.warn('界面.工具授权.响应缺少请求ID', undefined, LogCategory.UI);
      this.sendToast('工具授权响应缺少请求标识，已忽略', 'warning');
      return;
    }

    const callback = this.toolAuthorizationCallbacks.get(requestId);
    if (!callback) {
      logger.warn('界面.工具授权.回调不存在', { requestId }, LogCategory.UI);
      return;
    }

    this.toolAuthorizationCallbacks.delete(requestId);
    if (this.activeToolAuthorizationRequestId === requestId) {
      this.activeToolAuthorizationRequestId = null;
      this.clearActiveToolAuthorizationTimer();
    }

    callback(allowed);
    this.pumpToolAuthorizationQueue();
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

            // 尝试恢复当前 Mission
            const currentMission = this.orchestratorEngine.context.mission;
            if (currentMission && (currentMission.status === 'paused' || currentMission.status === 'pending_approval')) {
              this.sendOrchestratorMessage({
                content: '审批通过，继续执行任务...',
                messageType: 'text',
              });
              // 异步恢复，避免阻塞
              void this.orchestratorEngine.resumeMission(currentMission.id);
            }
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

  /** 绑定全局事件 */
  private bindEvents(): void {
    // 任务相关事件
    globalEventBus.on('task:created', () => this.sendStateUpdate());
    globalEventBus.on('task:state_changed', () => this.sendStateUpdate());
    globalEventBus.on('task:started', (event) => {
      this.sendStateUpdate();
    });
    globalEventBus.on('task:completed', (event) => {
      this.sendStateUpdate();
    });
    globalEventBus.on('task:failed', (event) => {
      this.sendStateUpdate();
      const data = event.data as { error?: string | object; stack?: string };
      let errorMsg = '任务执行失败';
      if (data?.error) {
        if (typeof data.error === 'string') {
          errorMsg = data.error;
        } else if (typeof data.error === 'object') {
          const errObj = data.error as { message?: string; error?: string };
          errorMsg = errObj.message || errObj.error || JSON.stringify(data.error);
        }
      }

      this.sendOrchestratorMessage({
        content: errorMsg,
        messageType: 'error',
        taskId: event.taskId,
        metadata: {
          error: errorMsg,
          stack: data?.stack,
        },
      });

    });
    // task:cancelled 事件：同时更新状态和中断任务
    globalEventBus.on('task:cancelled', () => {
      this.sendStateUpdate();
      this.interruptCurrentTask();
    });
    globalEventBus.on('execution:stats_updated', () => {
      this.sendExecutionStats();
    });

    // orchestrator:ui_message 已废弃：所有 UI 消息统一走 MessageHub

    globalEventBus.on('orchestrator:phase_changed', (event) => {
      const data = event.data as { phase: string; isRunning?: boolean; timestamp?: number };
      if (data?.phase) {
        // 🔧 统一消息通道：phaseChanged 走 MessageHub
        // UI 不再展示阶段步骤，仅用于同步运行态与日志
        this.messageHub.phaseChange(
          data.phase,
          data.isRunning ?? this.orchestratorEngine.running,
          event.taskId || ''
        );
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

      // 依赖分析目前不向前端分发，避免产生未消费的数据事件
    });

    globalEventBus.on('snapshot:created', () => this.sendStateUpdate());
    globalEventBus.on('snapshot:reverted', () => this.sendStateUpdate());

    // Worker 状态相关事件
    globalEventBus.on('worker:statusChanged', (event) => {
      const data = event.data as { worker: string; available: boolean; model?: string };
      this.sendStateUpdate();
      // 通知 UI Worker 状态变化
      this.messageHub.workerStatus(data.worker, data.available, data.model);
    });

    globalEventBus.on('worker:healthCheck', () => {
      this.sendStateUpdate();
    });

    globalEventBus.on('worker:error', (event) => {
      const data = event.data as { worker: string; error: string };
      this.sendOrchestratorMessage({
        content: `${data.worker || 'Worker'}: ${data.error || '发生错误'}`,
        messageType: 'error',
        metadata: { worker: data.worker },
      });
    });

    globalEventBus.on('worker:session_event', (event) => {
      const data = event.data as {
        type?: string;
        worker?: WorkerSlot;
        role?: string;
        requestId?: string;
        reason?: string;
        error?: string;
      };
      const pieces = [
        data?.type || 'session',
        data?.worker ? `worker=${data.worker}` : '',
        data?.role ? `role=${data.role}` : '',
        data?.requestId ? `req=${data.requestId}` : '',
        data?.reason ? `reason=${data.reason}` : '',
        data?.error ? `error=${data.error}` : '',
      ].filter(Boolean);
      const level = data?.type?.includes('failed') ? 'error' : 'info';
      this.appendLog({
        level,
        message: pieces.join(' '),
        source: data?.worker ?? 'system',
        timestamp: Date.now(),
      });
      // session_event 仅写入日志，不推送到 Worker 面板，避免干扰用户对话
    });

    // 工具授权请求事件
    globalEventBus.on('tool:authorization_request', (event) => {
      const data = event.data as {
        toolName: string;
        toolArgs: any;
        callback: (allowed: boolean) => void;
      };
      const requestId = `tool-auth-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;

      // 存储回调并入队，按序发送授权请求（避免并发覆盖）
      this.toolAuthorizationCallbacks.set(requestId, data.callback);
      this.toolAuthorizationQueue.push({
        requestId,
        toolName: data.toolName,
        toolArgs: data.toolArgs,
      });
      this.pumpToolAuthorizationQueue();
    });

    // ============= Mission-Driven 架构事件 =============
    // 这些事件来自 MissionOrchestrator（MissionExecutor 已合并）
    this.bindMissionEvents();
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
   * 绑定 Mission-Driven 架构事件
   * 将 MissionOrchestrator 的事件转发到 Webview（MissionExecutor 已合并）
   */
  private bindMissionEvents(): void {
    // 如果 MissionOrchestrator 未初始化，跳过
    if (!this.missionOrchestrator) return;

    // Mission 生命周期事件
    this.missionOrchestrator.on('missionCreated', () => {
      this.sendStateUpdate();
    });

    this.missionOrchestrator.on('missionPlanned', (data: { mission: Mission; contracts: any[]; assignments: Assignment[] }) => {
      const assignments = this.normalizeAssignments(data.assignments);
      this.sendData('missionPlanned', {
        missionId: data.mission.id,
        contracts: data.contracts,
        assignments,
        sessionId: this.activeSessionId,
      });
    });

    this.missionOrchestrator.on('missionCompleted', (data: { mission: Mission }) => {
      this.sendStateUpdate();
      void this.tryResumePendingRecovery();
    });

    this.missionOrchestrator.on('missionFailed', (data: { mission: Mission; error: string }) => {
      this.sendData('missionFailed', {
        missionId: data.mission.id,
        error: data.error,
        sessionId: this.activeSessionId,
      });
      this.sendOrchestratorMessage({
        content: data.error || '任务失败',
        messageType: 'error',
        metadata: { missionId: data.mission.id },
      });
      this.sendStateUpdate();
      void this.tryResumePendingRecovery();
    });

    this.missionOrchestrator.on('missionCancelled', () => {
      this.sendStateUpdate();
      void this.tryResumePendingRecovery();
    });

    this.missionOrchestrator.on('executionCompleted', (data: any) => {
      this.sendStateUpdate();
    });

    this.missionOrchestrator.on('executionFailed', (data: { missionId: string; error: string }) => {
      this.sendData('missionExecutionFailed', {
        missionId: data.missionId,
        error: data.error,
        sessionId: this.activeSessionId,
      });
      this.sendOrchestratorMessage({
        content: data.error || '任务执行失败',
        messageType: 'error',
        metadata: { missionId: data.missionId },
      });
      this.sendStateUpdate();
    });

    // Assignment 事件
    this.missionOrchestrator.on('assignmentStarted', (data: { missionId: string; assignmentId: string; workerId: WorkerSlot }) => {
      this.sendData('assignmentStarted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        sessionId: this.activeSessionId,
      });
    });

    this.missionOrchestrator.on('assignmentPlanned', (data: { missionId: string; assignmentId: string; todos: UnifiedTodo[]; warnings?: string[] }) => {
      const assignmentId = data.assignmentId || this.generateEntityId('assignment');
      const todos = this.normalizeTodos(data.todos, assignmentId);
      this.sendData('assignmentPlanned', {
        missionId: data.missionId,
        assignmentId,
        todos,
        warnings: data.warnings,
        sessionId: this.activeSessionId,
      });
    });

    this.missionOrchestrator.on('assignmentCompleted', (data: { missionId: string; assignmentId: string; success: boolean }) => {
      this.sendData('assignmentCompleted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        success: data.success,
        sessionId: this.activeSessionId,
      });
    });

    // Worker Session 事件
    this.missionOrchestrator.on('workerSessionCreated', (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot }) => {
      this.sendData('workerSessionCreated', {
        sessionId: data.sessionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
      });
    });

    this.missionOrchestrator.on('workerSessionResumed', (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot; completedTodos: number }) => {
      this.sendData('workerSessionResumed', {
        sessionId: data.sessionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        completedTodos: data.completedTodos,
      });
      this.messageHub.systemNotice(`Session 已恢复，继续执行 ${data.completedTodos} 个已完成的 Todo`, {
        sessionId: data.sessionId,
        worker: data.workerId,
      });
    });

    // Todo 事件
    this.missionOrchestrator.on('todoStarted', (data: { missionId: string; assignmentId: string; todoId: string }) => {
      this.sendData('todoStarted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        sessionId: this.activeSessionId,
      });
    });

    this.missionOrchestrator.on('todoCompleted', (data: { missionId: string; assignmentId: string; todoId: string; output: any }) => {
      this.sendData('todoCompleted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        output: data.output,
        sessionId: this.activeSessionId,
      });
    });

    this.missionOrchestrator.on('todoFailed', (data: { missionId: string; assignmentId: string; todoId: string; error: string }) => {
      this.sendData('todoFailed', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        error: data.error,
        sessionId: this.activeSessionId,
      });
    });

    // 动态 Todo 事件
    this.missionOrchestrator.on('dynamicTodoAdded', (data: { missionId: string; assignmentId: string; todo: UnifiedTodo }) => {
      const assignmentId = data.assignmentId || this.generateEntityId('assignment');
      const normalizedTodo = this.normalizeTodos([data.todo], assignmentId)[0];
      if (!normalizedTodo) {
        logger.warn('动态 Todo 无效，已跳过发送', { assignmentId, missionId: data.missionId }, LogCategory.ORCHESTRATOR);
        return;
      }
      this.sendData('dynamicTodoAdded', {
        missionId: data.missionId,
        assignmentId,
        todo: normalizedTodo,
        sessionId: this.activeSessionId,
      });
    });

    // 审批请求事件
    this.missionOrchestrator.on('approvalRequested', (data: { missionId: string; assignmentId: string; todoId: string; reason: string }) => {
      // 🔧 P3: 发送交互消息
      const traceId = this.messageHub.getTraceId();
      const interactionMsg = createInteractionMessage(
        {
          type: InteractionType.PERMISSION,
          requestId: `approval-${data.todoId}`,
          prompt: `**动态任务审批**\n\n原因: ${data.reason}\n\n请决定是否批准。`,
          required: true
        },
        'orchestrator',
        'orchestrator',
        traceId
      );
      this.messageHub.sendMessage(interactionMsg);

      this.sendData('todoApprovalRequested', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        reason: data.reason,
        sessionId: this.activeSessionId,
      });
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


    // 1. 首先中断 Orchestrator（这会触发 AbortController）
    if (this.orchestratorEngine.running) {
      logger.info('界面.任务.中断.编排器', undefined, LogCategory.UI);
      await this.orchestratorEngine.interrupt();
    }

    // 2. 中断所有适配器并等待完成
    // 关键：仅在“真正完成停止”后才允许清空 processing，避免 UI 假空闲
    let adapterInterruptCompleted = false;
    logger.info('界面.任务.中断.适配器.开始', undefined, LogCategory.UI);
    try {
      await this.adapterFactory.shutdown();
      adapterInterruptCompleted = true;
      logger.info('界面.任务.中断.适配器.完成', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.任务.中断.适配器.错误', error, LogCategory.UI);
      adapterInterruptCompleted = false;
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
                worker: subTask.assignedWorker as any, // 类型兼容性转换
                summary: '用户终止',
              });
            }
          }
        }
      }
    }

    // 清理编排者流式输出缓存，避免跨任务串流
    this.streamMessageIds.clear();
    // 仅在适配器已确认中断完成时清理处理状态，避免“Stop 提前恢复、Worker 仍执行”。
    if (adapterInterruptCompleted) {
      this.messageHub.forceProcessingState(false);
    }

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
    void this.sendWorkerStatus(true).catch((error) => {
      logger.warn('界面.启动.模型状态检测_失败', { error: String(error) }, LogCategory.UI);
    });

    // 发送执行统计数据
    this.sendExecutionStats();
  }


  /** 处理 Webview 消息 */
  private async handleMessage(message: WebviewToExtensionMessage): Promise<void> {
    logger.info('界面.Webview.消息.收到', { type: message.type }, LogCategory.UI);

    switch (message.type) {
      case 'getState':
        this.sendStateUpdate();
        this.sendCurrentSessionToWebview();
        break;

      case 'requestState':
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
        const uiError = message as any;
        logger.error('界面.UI_错误', {
          component: uiError.component,
          detail: uiError.detail,
          stack: uiError.stack,
        }, LogCategory.UI);
        break;
      }

      case 'executeTask':
        logger.info('界面.任务.执行.请求', { promptLength: String((message as any).prompt || '').length, imageCount: (message as any).images?.length || 0, agent: (message as any).agent || 'orchestrator' }, LogCategory.UI);
        const execImages = (message as any).images || [];
        const execAgent = (message as any).agent as WorkerSlot | undefined;
        const execRequestId = (message as any).requestId as string | undefined;
        const requestedModeRaw = (message as any).mode;
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
          await this.executeTask((message as any).prompt, execAgent || undefined, execImages, execRequestId);
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
        await this.interruptCurrentTask({ silent: Boolean((message as any).silent) });
        break;

      case 'startTask':
        await this.handleStartTask((message as any).taskId);
        break;

      case 'deleteTask':
        await this.handleDeleteTask((message as any).taskId);
        break;

      case 'pauseTask':
       
        logger.info('界面.任务.暂停.消息', { taskId: (message as any).taskId }, LogCategory.UI);
        this.sendToast('暂停功能开发中', 'info');
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
            session: switchedSession as any,
          });
        }
        this.sendStateUpdate();
        break;

      case 'renameSession':
        // 重命名会话
        if (this.sessionManager.renameSession(message.sessionId, message.name)) {
          this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() as any[] });
          this.sendToast('会话已重命名', 'success');
        }
        break;

      case 'closeSession':
        // 删除会话（统一管理器会清理所有相关资源）
        if (this.sessionManager.deleteSession(message.sessionId)) {
          // 如果删除后没有会话，创建一个新的
          if (this.sessionManager.getSessionMetas().length === 0) {
            const newSession = this.sessionManager.createSession();
            this.activeSessionId = newSession.id;
            this.sendData('sessionCreated', { session: newSession as any });
          }
          this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() as any[] });
          this.sendToast('会话已删除', 'info');
        }
        this.sendStateUpdate();
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
        this.selectedWorker = (message as any).worker || null;
        logger.info('界面.Worker.选择.变更', { worker: this.selectedWorker || 'auto' }, LogCategory.UI);
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
          (message as any).additionalInfo,
          (message as any).autoSkipped ?? false
        );
        break;

      case 'answerWorkerQuestion':
        // 用户回答 Worker 问题
        this.handleWorkerQuestionAnswer((message as any).answer ?? null);
        break;

      case 'toolAuthorizationResponse':
        // 用户响应工具授权请求
        this.handleToolAuthorizationResponse((message as any).requestId as string | undefined, (message as any).allowed ?? false);
        break;

      case 'interactionResponse':
        // 🔧 P3: 处理交互响应 (如动态审批)
        await this.handleInteractionResponse((message as any).requestId, (message as any).response);
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

      case 'checkWorkerStatus':
        this.sendWorkerStatus(Boolean((message as any).force));
        break;


      case 'getProfileConfig':
        await this.sendProfileConfig();
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
        this.handleUpdatePromptEnhance((message as any).config, (message as any).source);
        break;

      case 'testPromptEnhance':
        // 测试 Prompt 增强连接
        this.handleTestPromptEnhance((message as any).baseUrl, (message as any).apiKey);
        break;

      case 'enhancePrompt':
        // 执行 Prompt 增强（从系统级存储读取配置）
        this.handleEnhancePrompt((message as any).prompt);
        break;

      // LLM 配置相关
      case 'loadAllWorkerConfigs':
        await this.handleLoadAllWorkerConfigs();
        break;

      case 'saveWorkerConfig':
        await this.handleSaveWorkerConfig(message.worker, message.config);
        break;

      case 'testWorkerConnection':
        await this.handleTestWorkerConnection(message.worker, message.config);
        break;

      case 'loadOrchestratorConfig':
        await this.handleLoadOrchestratorConfig();
        break;

      case 'saveOrchestratorConfig':
        await this.handleSaveOrchestratorConfig(message.config);
        break;

      case 'testOrchestratorConnection':
        await this.handleTestOrchestratorConnection(message.config);
        break;

      case 'loadCompressorConfig':
        await this.handleLoadCompressorConfig();
        break;

      case 'saveCompressorConfig':
        await this.handleSaveCompressorConfig(message.config);
        break;

      case 'testCompressorConnection':
        await this.handleTestCompressorConnection(message.config);
        break;

      // MCP 配置相关
      case 'loadMCPServers':
        await this.handleLoadMCPServers();
        break;

      case 'addMCPServer':
        await this.handleAddMCPServer(message.server);
        break;

      case 'updateMCPServer':
        await this.handleUpdateMCPServer(message.serverId, message.updates);
        break;

      case 'deleteMCPServer':
        await this.handleDeleteMCPServer(message.serverId);
        break;

      case 'connectMCPServer':
        await this.handleConnectMCPServer(message.serverId);
        break;

      case 'disconnectMCPServer':
        await this.handleDisconnectMCPServer(message.serverId);
        break;

      case 'refreshMCPTools':
        await this.handleRefreshMCPTools(message.serverId);
        break;

      case 'getMCPServerTools':
        await this.handleGetMCPServerTools(message.serverId);
        break;

      // Skills 配置相关
      case 'loadSkillsConfig':
        await this.handleLoadSkillsConfig();
        break;

      case 'saveSkillsConfig':
        await this.handleSaveSkillsConfig(message.config);
        break;

      case 'addCustomTool':
        await this.handleAddCustomTool(message.tool);
        break;

      case 'removeCustomTool':
        await this.handleRemoveCustomTool(message.toolName);
        break;

      case 'removeInstructionSkill':
        await this.handleRemoveInstructionSkill(message.skillName);
        break;

      case 'installSkill':
        await this.handleInstallSkill(message.skillId);
        break;
      case 'applyInstructionSkill':
        const skillRequestId = (message as any).requestId as string | undefined;
        if (!this.shouldProcessRequest(skillRequestId)) {
          if (skillRequestId) {
            this.messageHub.taskRejected(skillRequestId, '请求重复，已忽略');
          }
          break;
        }
        try {
          await this.handleApplyInstructionSkill(
            (message as any).skillName,
            (message as any).args,
            (message as any).images || [],
            (message as any).agent || undefined,
            skillRequestId
          );
        } catch (error: any) {
          const errorMsg = error instanceof Error ? error.message : String(error);
          if (skillRequestId) {
            this.messageHub.taskRejected(skillRequestId, errorMsg);
          }
          throw error;
        }
        break;

      // Skills 仓库管理
      case 'loadRepositories':
        await this.handleLoadRepositories();
        break;

      case 'addRepository':
        await this.handleAddRepository(message.url);
        break;

      case 'updateRepository':
        await this.handleUpdateRepository(message.repositoryId, message.updates);
        break;

      case 'deleteRepository':
        await this.handleDeleteRepository(message.repositoryId);
        break;

      case 'refreshRepository':
        await this.handleRefreshRepository(message.repositoryId);
        break;

      case 'loadSkillLibrary':
        await this.handleLoadSkillLibrary();
        break;

      // 项目知识相关
      case 'getProjectKnowledge':
        await this.handleGetProjectKnowledge();
        break;

      case 'getADRs':
        await this.handleGetADRs(message.filter);
        break;

      case 'getFAQs':
        await this.handleGetFAQs(message.filter);
        break;

      case 'searchFAQs':
        await this.handleSearchFAQs(message.keyword);
        break;

      case 'deleteADR':
        await this.handleDeleteADR(message.id);
        break;

      case 'deleteFAQ':
        await this.handleDeleteFAQ(message.id);
        break;

      case 'openFile':
        await this.handleOpenFile(message.filepath);
        break;

      case 'addADR':
        await this.handleAddADR(message.adr);
        break;

      case 'updateADR':
        await this.handleUpdateADR(message.id, message.updates);
        break;

      case 'deleteADR':
        await this.handleDeleteADR(message.id);
        break;

      case 'addFAQ':
        await this.handleAddFAQ(message.faq);
        break;

      case 'updateFAQ':
        await this.handleUpdateFAQ(message.id, message.updates);
        break;

      case 'deleteFAQ':
        await this.handleDeleteFAQ(message.id);
        break;

      case 'openMermaidPanel':
        // 在新标签页打开 Mermaid 图表
        this.handleOpenMermaidPanel((message as any).code, (message as any).title);
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

  /** 处理 Prompt 增强配置更新 - 存储到 ~/.multicli/config.json */
  private async handleUpdatePromptEnhance(
    config: { enabled: boolean; baseUrl: string; apiKey: string },
    source: 'auto' | 'manual' = 'auto'
  ): Promise<void> {
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

      // 🔧 同步更新 ToolManager 中的 AceExecutor 配置
      const toolManager = this.adapterFactory.getToolManager?.();
      if (toolManager && config.baseUrl && config.apiKey) {
        toolManager.configureAce(config.baseUrl, config.apiKey);
        logger.info('界面.提示词_增强.配置.已同步到ToolManager', undefined, LogCategory.UI);
      }

      if (source === 'manual') {
        this.sendToast('ACE 配置已保存', 'success');
      }
    } catch (error) {
      logger.error('界面.提示词_增强.配置.保存_失败', error, LogCategory.UI);
      if (source === 'manual') {
        const errorMsg = error instanceof Error ? error.message : String(error);
        this.sendToast(`ACE 配置保存失败: ${errorMsg}`, 'error');
      }
    }
  }

  /** 获取 MultiCLI 配置文件路径 */
  private getMultiCliConfigPath(): string {
    return path.join(os.homedir(), '.multicli', 'config.json');
  }

  /** 获取 Prompt 增强配置 - 复用 ToolManager 的配置读取逻辑 */
  private async getPromptEnhanceConfig(): Promise<{ baseUrl: string; apiKey: string }> {
    return loadAceConfigFromFile();
  }

  /** 发送 Prompt 增强配置到前端 */
  private async sendPromptEnhanceConfig(): Promise<void> {
    const config = await this.getPromptEnhanceConfig();
    this.sendData('promptEnhanceConfigLoaded', {
      config: {
        baseUrl: config.baseUrl,
        apiKey: config.apiKey
      }
    });
  }

  /** 测试 Augment API 连接 */
  private async handleTestPromptEnhance(baseUrl: string, apiKey: string): Promise<void> {
    if (!baseUrl || !apiKey) {
      this.sendData('promptEnhanceResult', { success: false, message: '请填写 API 地址和密钥' });
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
        this.sendData('promptEnhanceResult', { success: true, message: '连接成功' });
      } else if (response.status === 401) {
        this.sendData('promptEnhanceResult', { success: false, message: 'Token 无效或已过期' });
      } else if (response.status === 403) {
        this.sendData('promptEnhanceResult', { success: false, message: '访问被拒绝' });
      } else {
        const errorText = await response.text().catch(() => '');
        this.sendData('promptEnhanceResult', { success: false, message: `连接失败: ${response.status}` });
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendData('promptEnhanceResult', { success: false, message: `连接错误: ${errorMsg}` });
    }
  }

  /** 执行 Prompt 增强 - 使用压缩模型 + 代码上下文 */
  private async handleEnhancePrompt(prompt: string): Promise<void> {
    try {
      // 加载压缩模型配置
      const { LLMConfigLoader } = await import('../llm/config');
      const compressorConfig = LLMConfigLoader.loadCompressorConfig();
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();

      const useCompressor = compressorConfig.enabled
        && Boolean(compressorConfig.baseUrl && compressorConfig.model);
      const activeConfig = useCompressor ? compressorConfig : orchestratorConfig;
      const activeLabel = useCompressor ? 'compressor' : 'orchestrator';

      // 获取项目根目录
      const workspaceFolders = vscode.workspace.workspaceFolders;
      const projectRoot = workspaceFolders?.[0]?.uri.fsPath || '';

      // 收集代码上下文（参考 Augment 方案）
      let codeContext = '';
      if (projectRoot) {
        try {
          codeContext = await this.collectCodeContext(projectRoot, prompt);
        } catch (error) {
          logger.warn('界面.提示词_增强.代码上下文收集失败', { error }, LogCategory.UI);
          // 代码上下文收集失败不阻止增强
        }
      }

      // 收集对话历史（5-10 轮对话）
      const conversationHistory = this.sessionManager.formatConversationHistory(10);

      // 检测语言
      const isChinese = /[\u4e00-\u9fa5]/.test(prompt);

      // 构建增强 prompt（参考 Augment 方案）
      const enhancePrompt = this.buildEnhancePrompt(prompt, conversationHistory, codeContext, isChinese);

      // 创建压缩模型客户端并调用
      const { UniversalLLMClient } = await import('../llm/clients/universal-client');
      const client = new UniversalLLMClient({
        baseUrl: activeConfig.baseUrl,
        apiKey: activeConfig.apiKey,
        model: activeConfig.model,
        provider: activeConfig.provider,
        enabled: true,
      });

      logger.info('界面.提示词_增强.开始', {
        model: activeConfig.model,
        used: activeLabel,
        fallbackToOrchestrator: !useCompressor,
        hasCodeContext: codeContext.length > 0,
        hasConversation: conversationHistory.length > 0
      }, LogCategory.UI);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: enhancePrompt }],
        maxTokens: 4096,
        temperature: 0.7,
      });

      const enhancedPrompt = response.content?.trim() || '';

      if (enhancedPrompt) {
        logger.info('界面.提示词_增强.完成', {
          originalLength: prompt.length,
          enhancedLength: enhancedPrompt.length
        }, LogCategory.UI);
        this.sendData('promptEnhanced', { enhancedPrompt, error: '' });
      } else {
        this.sendData('promptEnhanced', { enhancedPrompt: '', error: '未获取到增强结果' });
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      logger.error('界面.提示词_增强.失败', { error: errorMsg }, LogCategory.UI);
      this.sendData('promptEnhanced', { enhancedPrompt: '', error: errorMsg });
    }
  }

  /**
   * 收集代码上下文（参考 Augment 方案）
   * 优先使用 ACE 语义搜索（复用 ToolManager 中的 AceExecutor），如果不可用则回退到简单文件搜索
   */
  private async collectCodeContext(projectRoot: string, prompt: string): Promise<string> {
    const contextParts: string[] = [];
    const maxContextLength = 8000; // 限制代码上下文长度
    let currentLength = 0;

    try {
      // 1. 尝试使用 ACE 语义搜索（Augment 的核心能力）
      const aceResult = await this.tryAceSemanticSearch(projectRoot, prompt);
      if (aceResult) {
        contextParts.push(`## 相关代码（语义搜索）\n${aceResult}`);
        currentLength += aceResult.length;
        logger.info('界面.提示词_增强.ACE语义搜索成功', { resultLength: aceResult.length }, LogCategory.UI);
      } else {
        // 2. ACE 不可用，回退到简单文件搜索
        logger.info('界面.提示词_增强.ACE不可用，使用简单搜索', undefined, LogCategory.UI);

        // 获取项目结构概览
        const projectStructure = await this.getProjectStructure(projectRoot);
        if (projectStructure && currentLength + projectStructure.length <= maxContextLength) {
          contextParts.push(`## 项目结构\n${projectStructure}`);
          currentLength += projectStructure.length;
        }

        // 从提示词中提取关键词并搜索相关文件
        const keywords = this.extractKeywords(prompt);
        if (keywords.length > 0) {
          const relevantFiles = await this.findRelevantFiles(projectRoot, keywords);

          for (const file of relevantFiles) {
            if (currentLength >= maxContextLength) break;

            try {
              const content = fs.readFileSync(file, 'utf-8');
              const truncatedContent = content.length > 2000
                ? content.substring(0, 2000) + '\n... (truncated)'
                : content;

              const relativePath = path.relative(projectRoot, file);
              const fileContext = `### ${relativePath}\n\`\`\`\n${truncatedContent}\n\`\`\``;

              if (currentLength + fileContext.length <= maxContextLength) {
                contextParts.push(fileContext);
                currentLength += fileContext.length;
              }
            } catch {
              // 忽略读取失败的文件
            }
          }
        }
      }

      // 3. 检查是否有 CLAUDE.md 或类似的项目说明文件
      const guidelineFiles = ['CLAUDE.md', '.augment-guidelines', 'README.md', 'CONTRIBUTING.md'];
      for (const guideFile of guidelineFiles) {
        if (currentLength >= maxContextLength) break;

        const guidePath = path.join(projectRoot, guideFile);
        if (fs.existsSync(guidePath)) {
          try {
            const content = fs.readFileSync(guidePath, 'utf-8');
            const truncatedContent = content.length > 3000
              ? content.substring(0, 3000) + '\n... (truncated)'
              : content;

            if (currentLength + truncatedContent.length <= maxContextLength) {
              contextParts.push(`## 项目指南 (${guideFile})\n${truncatedContent}`);
              currentLength += truncatedContent.length;
              break; // 只取第一个找到的指南文件
            }
          } catch {
            // 忽略读取失败
          }
        }
      }
    } catch (error) {
      logger.warn('界面.提示词_增强.代码上下文收集异常', { error }, LogCategory.UI);
    }

    return contextParts.join('\n\n');
  }

  /**
   * 尝试使用 ACE 语义搜索获取代码上下文
   * 复用 ToolManager 中的 AceExecutor，避免重复配置读取
   * @returns 搜索结果，如果 ACE 不可用则返回 null
   */
  private async tryAceSemanticSearch(projectRoot: string, prompt: string): Promise<string | null> {
    try {
      // 复用 ToolManager 中的 AceExecutor
      const toolManager = this.adapterFactory.getToolManager?.();
      if (!toolManager) {
        logger.debug('界面.提示词_增强.ToolManager不可用', undefined, LogCategory.UI);
        return null;
      }

      // 检查 ACE 是否已配置
      if (!toolManager.isAceConfigured()) {
        logger.debug('界面.提示词_增强.ACE未配置', undefined, LogCategory.UI);
        return null;
      }

      // 通过 AceExecutor 执行语义搜索
      const aceExecutor = toolManager.getAceExecutor();
      const toolCall = {
        id: `enhance-ace-${Date.now()}`,
        name: 'codebase_retrieval',
        arguments: {
          query: prompt,
          ensure_indexed: false  // 不强制重新索引
        }
      };

      const result = await aceExecutor.execute(toolCall);

      if (!result.isError && result.content && result.content !== '未找到相关代码') {
        return result.content;
      }

      return null;
    } catch (error) {
      logger.warn('界面.提示词_增强.ACE搜索异常', { error }, LogCategory.UI);
      return null;
    }
  }

  /**
   * 获取项目结构概览
   */
  private async getProjectStructure(projectRoot: string): Promise<string> {
    const structure: string[] = [];
    const maxDepth = 3;
    const maxFiles = 50;
    let fileCount = 0;

    const excludeDirs = new Set([
      'node_modules', '.git', '.vscode', 'dist', 'build', 'out',
      '__pycache__', '.pytest_cache', 'coverage', '.next', '.nuxt'
    ]);

    const walk = (dir: string, depth: number, prefix: string = '') => {
      if (depth > maxDepth || fileCount > maxFiles) return;

      try {
        const items = fs.readdirSync(dir, { withFileTypes: true });
        const sortedItems = items.sort((a, b) => {
          // 目录优先
          if (a.isDirectory() && !b.isDirectory()) return -1;
          if (!a.isDirectory() && b.isDirectory()) return 1;
          return a.name.localeCompare(b.name);
        });

        for (const item of sortedItems) {
          if (fileCount > maxFiles) break;
          if (item.name.startsWith('.') && item.name !== '.env.example') continue;
          if (excludeDirs.has(item.name)) continue;

          const isLast = items.indexOf(item) === items.length - 1;
          const connector = isLast ? '└── ' : '├── ';
          const newPrefix = isLast ? '    ' : '│   ';

          if (item.isDirectory()) {
            structure.push(`${prefix}${connector}${item.name}/`);
            walk(path.join(dir, item.name), depth + 1, prefix + newPrefix);
          } else {
            structure.push(`${prefix}${connector}${item.name}`);
            fileCount++;
          }
        }
      } catch {
        // 忽略权限问题
      }
    };

    walk(projectRoot, 0);

    if (structure.length === 0) return '';
    if (fileCount > maxFiles) {
      structure.push('... (more files)');
    }

    return structure.slice(0, 100).join('\n'); // 限制输出行数
  }

  /**
   * 从提示词中提取关键词
   */
  private extractKeywords(prompt: string): string[] {
    // 提取可能的文件名、函数名、类名等
    const words = prompt.split(/[\s,，。.!！?？;；:：()（）[\]【】{}]+/);
    const keywords: string[] = [];

    for (const word of words) {
      const cleaned = word.trim();
      if (cleaned.length < 2) continue;
      if (cleaned.length > 50) continue;

      // 保留看起来像代码标识符的词
      if (/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(cleaned)) {
        keywords.push(cleaned);
      }
      // 保留中文关键词
      if (/[\u4e00-\u9fa5]{2,}/.test(cleaned)) {
        keywords.push(cleaned);
      }
      // 保留文件扩展名
      if (/\.[a-z]{1,5}$/i.test(cleaned)) {
        keywords.push(cleaned);
      }
    }

    return [...new Set(keywords)].slice(0, 10); // 去重并限制数量
  }

  /**
   * 查找与关键词相关的文件
   */
  private async findRelevantFiles(projectRoot: string, keywords: string[]): Promise<string[]> {
    const relevantFiles: string[] = [];
    const maxFiles = 5;
    const searchExtensions = ['.ts', '.tsx', '.js', '.jsx', '.py', '.go', '.rs', '.java', '.vue', '.svelte'];

    const excludeDirs = new Set([
      'node_modules', '.git', 'dist', 'build', 'out', '__pycache__',
      '.next', '.nuxt', 'coverage', '.vscode'
    ]);

    const searchDir = (dir: string) => {
      if (relevantFiles.length >= maxFiles) return;

      try {
        const items = fs.readdirSync(dir, { withFileTypes: true });

        for (const item of items) {
          if (relevantFiles.length >= maxFiles) break;

          const fullPath = path.join(dir, item.name);

          if (item.isDirectory()) {
            if (!excludeDirs.has(item.name) && !item.name.startsWith('.')) {
              searchDir(fullPath);
            }
          } else {
            const ext = path.extname(item.name).toLowerCase();
            if (!searchExtensions.includes(ext)) continue;

            // 检查文件名是否包含关键词
            const fileName = item.name.toLowerCase();
            const matchesFileName = keywords.some(kw =>
              fileName.includes(kw.toLowerCase())
            );

            if (matchesFileName) {
              relevantFiles.push(fullPath);
            }
          }
        }
      } catch {
        // 忽略权限问题
      }
    };

    searchDir(projectRoot);
    return relevantFiles;
  }

  /** 构建提示词增强的 prompt（参考 Augment 方案） */
  private buildEnhancePrompt(
    originalPrompt: string,
    conversationHistory: string,
    codeContext: string,
    isChinese: boolean
  ): string {
    const languageInstruction = isChinese
      ? '请用中文输出增强后的提示词。'
      : 'Please output the enhanced prompt in English.';

    // 参考 Augment 的增强原则
    return `You are an expert prompt engineer. Your task is to enhance the user's original prompt to make it clearer, more specific, and more actionable for an AI coding assistant.

## Enhancement Principles (参考 Augment 方案)

1. **Clarify Intent**: Make the task goal crystal clear
2. **Add Technical Context**: Include relevant technical details, constraints, and requirements
3. **Structure the Request**: Organize the prompt with clear sections if needed
4. **Make it Actionable**: Ensure the AI can directly execute the task
5. **Preserve User Intent**: Do not change the user's original intention
6. **Use Code Context**: Reference relevant files, functions, or patterns from the codebase when applicable
7. **Consider Existing Patterns**: Align suggestions with existing code patterns and conventions

${codeContext ? `## Codebase Context

The following is relevant context from the user's project:

${codeContext}

` : ''}## Conversation History

${conversationHistory ? conversationHistory : '(No previous conversation)'}

## Original Prompt

${originalPrompt}

## Output Requirements

- ${languageInstruction}
- Output ONLY the enhanced prompt, without any explanations or prefixes
- Do NOT include prefixes like "Enhanced prompt:" or "增强后的提示词："
- Keep it concise but complete
- If the original prompt references code or files, make sure to maintain those references
- Add specific technical details that would help the AI assistant complete the task`;
  }

  // ============================================
  // LLM 配置管理方法
  // ============================================

  /** 加载所有 Worker 配置 */
  private async handleLoadAllWorkerConfigs(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const fullConfig = LLMConfigLoader.loadFullConfig();

      this.sendData('allWorkerConfigsLoaded', { configs: fullConfig.workers });
    } catch (error: any) {
      logger.error('加载 Worker 配置失败', { error: error.message }, LogCategory.LLM);
      this.sendToast(`加载配置失败: ${error.message}`, 'error');
    }
  }

  /** 保存 Worker 配置 */
  private async handleSaveWorkerConfig(worker: WorkerSlot, config: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');

      // 保存配置
      LLMConfigLoader.updateWorkerConfig(worker, config);

      // 清除适配器缓存
      await this.adapterFactory.clearAdapter(worker);

      this.sendToast(`${worker} 配置已保存`, 'success');

      logger.info('Worker 配置已保存', { worker }, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存 Worker 配置失败', { worker, error: error.message }, LogCategory.LLM);
      this.sendToast(`保存配置失败: ${error.message}`, 'error');
    }
  }

  /** 测试 Worker 连接 */
  private async handleTestWorkerConnection(worker: WorkerSlot, config: any): Promise<void> {
    try {
      const normalizedConfig = { ...config, enabled: config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        this.sendToast(`${worker} 未启用，无法测试连接`, 'warning');
        return;
      }
      const { createLLMClient } = await import('../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      // 发送测试请求
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'Hello' }],
        maxTokens: 10,
        temperature: 0.7
      });

      if (response && response.content) {
        this.sendData('workerConnectionTestResult', { worker, success: true });
        this.sendToast(`${worker} 连接成功`, 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('Worker 连接测试失败', { worker, error: error.message }, LogCategory.LLM);

      this.sendData('workerConnectionTestResult', { worker, success: false, error: error.message });
      this.sendToast(`${worker} 连接失败: ${error.message}`, 'error');
    }
  }

  /** 加载编排者配置 */
  private async handleLoadOrchestratorConfig(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadOrchestratorConfig();

      this.sendData('orchestratorConfigLoaded', { config });
    } catch (error: any) {
      logger.error('加载编排者配置失败', { error: error.message }, LogCategory.LLM);
      this.sendToast(`加载配置失败: ${error.message}`, 'error');
    }
  }

  /** 保存编排者配置 */
  private async handleSaveOrchestratorConfig(config: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');

      // 保存配置
      LLMConfigLoader.updateOrchestratorConfig(config);

      // 清除适配器缓存
      await this.adapterFactory.clearAdapter('orchestrator');

      this.sendToast('编排者配置已保存', 'success');

      logger.info('编排者配置已保存', undefined, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存编排者配置失败', { error: error.message }, LogCategory.LLM);
      this.sendToast(`保存配置失败: ${error.message}`, 'error');
    }
  }

  /** 测试编排者连接 */
  private async handleTestOrchestratorConnection(config: any): Promise<void> {
    try {
      const normalizedConfig = { ...config, enabled: config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        this.sendToast('编排者未启用，无法测试连接', 'warning');
        return;
      }
      const { createLLMClient } = await import('../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      // 发送测试请求
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'Hello' }],
        maxTokens: 10,
        temperature: 0.7
      });

      if (response && response.content) {
        this.sendData('orchestratorConnectionTestResult', { success: true });
        this.sendToast('编排模型连接成功', 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('编模型连接测试失败', { error: error.message }, LogCategory.LLM);

      this.sendData('orchestratorConnectionTestResult', { success: false, error: error.message });
      this.sendToast(`编排模型连接失败: ${error.message}`, 'error');
    }
  }

  /** 加载压缩模型配置 */
  private async handleLoadCompressorConfig(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadCompressorConfig();

      this.sendData('compressorConfigLoaded', { config });
    } catch (error: any) {
      logger.error('加载压缩模型配置失败', { error: error.message }, LogCategory.LLM);
      this.sendToast(`加载配置失败: ${error.message}`, 'error');
    }
  }

  /** 保存压缩模型配置 */
  private async handleSaveCompressorConfig(config: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');

      // 保存配置
      LLMConfigLoader.updateCompressorConfig(config);

      await this.orchestratorEngine.reloadCompressionAdapter();

      this.sendToast('压缩模型配置已保存', 'success');

      logger.info('压缩模型配置已保存', undefined, LogCategory.LLM);
    } catch (error: any) {
      logger.error('保存压缩模型配置失败', { error: error.message }, LogCategory.LLM);
      this.sendToast(`保存配置失败: ${error.message}`, 'error');
    }
  }

  /** 测试压缩模型连接 */
  private async handleTestCompressorConnection(config: any): Promise<void> {
    let fallbackModel: string | undefined;
    try {
      const normalizedConfig = { ...config, enabled: Boolean(config?.apiKey) || config?.enabled === true };
      const { LLMConfigLoader } = await import('../llm/config');
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();
      fallbackModel = orchestratorConfig?.provider && orchestratorConfig?.model
        ? `${orchestratorConfig.provider} - ${orchestratorConfig.model}`
        : undefined;

      if (!normalizedConfig.enabled || !normalizedConfig.apiKey || !normalizedConfig.model) {
        this.sendData('compressorConnectionTestResult', {
          success: false,
          error: '压缩模型未配置或不可用',
          fallbackModel
        });
        this.sendToast('压缩模型不可用，已降级使用编排者模型', 'warning');
        return;
      }
      const { createLLMClient } = await import('../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);

      // 发送测试请求
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'Hello' }],
        maxTokens: 10,
        temperature: 0.7
      });

      if (response && response.content) {
        this.sendData('compressorConnectionTestResult', { success: true });
        this.sendToast('压缩模型连接成功', 'success');
      } else {
        throw new Error('No response from LLM');
      }
    } catch (error: any) {
      logger.error('压缩模型连接测试失败', { error: error.message }, LogCategory.LLM);

      this.sendData('compressorConnectionTestResult', {
        success: false,
        error: error.message,
        fallbackModel
      });
      this.sendToast('压缩模型连接失败，已降级使用编排者模型', 'warning');
    }
  }

  // ============================================================================
  // MCP 配置处理方法
  // ============================================================================

  /**
   * 获取或创建 MCP 管理器
   */
  private async getMCPManager(): Promise<any> {
    const executor = this.adapterFactory.getMCPExecutor();
    if (!executor || typeof (executor as any).getMCPManager !== 'function') {
      throw new Error('MCP executor not available');
    }
    return (executor as any).getMCPManager();
  }

  /**
   * 加载 MCP 服务器列表
   */
  private async handleLoadMCPServers(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const servers = LLMConfigLoader.loadMCPConfig();
      for (const server of servers) {
        if (!server || typeof server !== 'object') {
          throw new Error('Invalid MCP server entry');
        }
        if (!server.id || typeof server.id !== 'string' || !server.id.trim()) {
          throw new Error('MCP server missing id');
        }
        if (!server.name || typeof server.name !== 'string' || !server.name.trim()) {
          throw new Error(`MCP server ${server.id || '<unknown>'} missing name`);
        }
      }

      this.sendData('mcpServersLoaded', { servers });
    } catch (error: any) {
      logger.error('加载 MCP 服务器列表失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`加载 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  /**
   * 添加 MCP 服务器
   */
  private async handleAddMCPServer(server: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');

      if (!server || typeof server !== 'object') {
        throw new Error('Invalid MCP server payload');
      }
      if (!server.id || typeof server.id !== 'string' || !server.id.trim()) {
        throw new Error('MCP server missing id');
      }
      if (!server.name || typeof server.name !== 'string' || !server.name.trim()) {
        throw new Error('MCP server missing name');
      }

      LLMConfigLoader.addMCPServer(server);

      this.sendData('mcpServerAdded', { server });
      this.sendToast(`MCP 服务器 "${server.name}" 已添加`, 'success');

      await this.adapterFactory.reloadMCP();
      logger.info('MCP reloaded in adapter factory', { id: server.id }, LogCategory.TOOLS);

      logger.info('MCP 服务器已添加', { id: server.id, name: server.name }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('添加 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`添加 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  /**
   * 更新 MCP 服务器
   */
  private async handleUpdateMCPServer(serverId: string, updates: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      LLMConfigLoader.updateMCPServer(serverId, updates);

      this.sendData('mcpServerUpdated', { serverId });
      this.sendToast('MCP 服务器已更新', 'success');

      await this.adapterFactory.reloadMCP();
      logger.info('MCP reloaded in adapter factory', { id: serverId }, LogCategory.TOOLS);

      logger.info('MCP 服务器已更新', { id: serverId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('更新 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`更新 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  /**
   * 删除 MCP 服务器
   */
  private async handleDeleteMCPServer(serverId: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');

      // 先断开连接
      const manager = await this.getMCPManager();
      await manager.disconnectServer(serverId);

      // 删除配置
      LLMConfigLoader.deleteMCPServer(serverId);

      this.sendData('mcpServerDeleted', { serverId });
      this.sendToast('MCP 服务器已删除', 'success');

      await this.adapterFactory.reloadMCP();
      logger.info('MCP reloaded in adapter factory', { id: serverId }, LogCategory.TOOLS);

      logger.info('MCP 服务器已删除', { id: serverId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('删除 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`删除 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  /**
   * 连接 MCP 服务器
   */
  private async handleConnectMCPServer(serverId: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const servers = LLMConfigLoader.loadMCPConfig();
      const server = servers.find((s: any) => s.id === serverId);

      if (!server) {
        throw new Error(`MCP 服务器不存在: ${serverId}`);
      }

      if (!server.enabled) {
        throw new Error('MCP 服务器未启用');
      }

      const manager = await this.getMCPManager();
      await manager.connectServer(server);

      const tools = manager.getServerTools(serverId);

      this.sendToast(`MCP 服务器 "${server.name}" 已连接，发现 ${tools.length} 个工具`, 'success');

      logger.info('MCP 服务器已连接', { id: serverId, toolCount: tools.length }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('连接 MCP 服务器失败', { serverId, error: error.message }, LogCategory.TOOLS);

      this.sendToast(`连接 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  /**
   * 断开 MCP 服务器连接
   */
  private async handleDisconnectMCPServer(serverId: string): Promise<void> {
    try {
      const manager = await this.getMCPManager();
      await manager.disconnectServer(serverId);

      this.sendToast('MCP 服务器已断开连接', 'success');

      logger.info('MCP 服务器已断开', { id: serverId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('断开 MCP 服务器失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`断开 MCP 服务器失败: ${error.message}`, 'error');
    }
  }

  /**
   * 刷新 MCP 服务器工具列表
   */
  private async handleRefreshMCPTools(serverId: string): Promise<void> {
    try {
      const manager = await this.getMCPManager();
      let tools: any[] = [];

      if (!manager.getServerStatus(serverId)) {
        const { LLMConfigLoader } = await import('../llm/config');
        const servers = LLMConfigLoader.loadMCPConfig();
        const server = servers.find((s: any) => s.id === serverId);

        if (!server) {
          throw new Error(`MCP 服务器不存在: ${serverId}`);
        }

        if (!server.enabled) {
          throw new Error('MCP 服务器未启用');
        }

        await manager.connectServer(server);
      }

      tools = await manager.refreshServerTools(serverId);

      this.sendData('mcpToolsRefreshed', { serverId, tools });
      this.sendToast(`工具列表已刷新，发现 ${tools.length} 个工具`, 'success');

      logger.info('MCP 工具列表已刷新', { serverId, toolCount: tools.length }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('刷新 MCP 工具列表失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`刷新工具列表失败: ${error.message}`, 'error');
    }
  }

  /**
   * 获取 MCP 服务器工具列表
   */
  private async handleGetMCPServerTools(serverId: string): Promise<void> {
    try {
      const manager = await this.getMCPManager();
      const tools = manager.getServerTools(serverId);

      this.sendData('mcpServerTools', { serverId, tools });
    } catch (error: any) {
      logger.error('获取 MCP 工具列表失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`获取工具列表失败: ${error.message}`, 'error');
    }
  }

  // ============================================================================
  // Skills 配置处理
  // ============================================================================

  private async reloadSkillsIfPossible(reason: string): Promise<void> {
    await this.adapterFactory.reloadSkills();
    logger.info('Skills reloaded in adapter factory', { reason }, LogCategory.TOOLS);
  }

  /**
   * 加载 Skills 配置
   */
  private async handleLoadSkillsConfig(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig();

      this.sendData('skillsConfigLoaded', {
        config: config || {
          customTools: [],
          instructionSkills: [],
          repositories: []
        }
      });

      logger.info('Skills 配置已加载', {}, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('加载 Skills 配置失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`加载 Skills 配置失败: ${error.message}`, 'error');
    }
  }

  /**
   * 保存 Skills 配置
   */
  private async handleSaveSkillsConfig(config: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      LLMConfigLoader.saveSkillsConfig(config);

      this.sendToast('Skills 配置已保存', 'success');

      await this.reloadSkillsIfPossible('saveSkillsConfig');

      logger.info('Skills 配置已保存', {}, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('保存 Skills 配置失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`保存 Skills 配置失败: ${error.message}`, 'error');
    }
  }

  /**
   * 添加自定义工具
   */
  private async handleAddCustomTool(tool: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig() || {
        customTools: [],
        instructionSkills: [],
        repositories: []
      };

      // 检查是否已存在
      const existingIndex = config.customTools.findIndex((t: any) => t.name === tool.name);
      if (existingIndex >= 0) {
        config.customTools[existingIndex] = tool;
      } else {
        config.customTools.push(tool);
      }

      LLMConfigLoader.saveSkillsConfig(config);

      this.sendData('customToolAdded', { tool });
      this.sendToast(`自定义工具 "${tool.name}" 已添加`, 'success');

      await this.reloadSkillsIfPossible('addCustomTool');

      logger.info('自定义工具已添加', { name: tool.name }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('添加自定义工具失败', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`添加自定义工具失败: ${error.message}`, 'error');
    }
  }

  /**
   * 删除自定义工具
   */
  private async handleRemoveCustomTool(toolName: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig() || {
        customTools: [],
        instructionSkills: [],
        repositories: []
      };

      config.customTools = config.customTools.filter((t: any) => t.name !== toolName);
      LLMConfigLoader.saveSkillsConfig(config);

      this.sendData('customToolRemoved', { toolName });
      this.sendToast(`自定义工具 "${toolName}" 已删除`, 'success');

      await this.reloadSkillsIfPossible('removeCustomTool');

      logger.info('自定义工具已删除', { name: toolName }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('删除自定义工具失败', { toolName, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`删除自定义工具失败: ${error.message}`, 'error');
    }
  }

  /**
   * 删除 Instruction Skill
   */
  private async handleRemoveInstructionSkill(skillName: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig() || {
        customTools: [],
        instructionSkills: [],
        repositories: []
      };

      config.instructionSkills = (config.instructionSkills || []).filter((s: any) => s.name !== skillName);
      LLMConfigLoader.saveSkillsConfig(config);

      this.sendData('instructionSkillRemoved', { skillName });
      this.sendToast(`Instruction Skill "${skillName}" 已删除`, 'success');

      await this.reloadSkillsIfPossible('removeInstructionSkill');

      logger.info('Instruction Skill 已删除', { name: skillName }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('删除 Instruction Skill 失败', { skillName, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`删除 Instruction Skill 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 安装 Skill
   */
  private async handleInstallSkill(skillId: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const { SkillRepositoryManager } = await import('../tools/skill-repository-manager');

      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      const skills = await manager.getAllSkills(repositories);
      const skill = skills.find((item: any) => item.fullName === skillId || item.id === skillId);
      if (!skill) {
        throw new Error(`未找到 Skill: ${skillId}`);
      }

      // 加载当前配置
      const config = LLMConfigLoader.loadSkillsConfig() || {
        customTools: [],
        instructionSkills: [],
        repositories
      };

      const updatedConfig = applySkillInstall(config, skill);
      Object.assign(config, updatedConfig);

      // 保存配置
      LLMConfigLoader.saveSkillsConfig(config);

      // 发送成功消息
      this.sendData('skillInstalled', { skillId, skill });
      this.sendToast(`Skill "${skill.description}" 已安装`, 'success');

      // 重新加载配置以更新前端
      await this.handleLoadSkillsConfig();

      // 重新加载 Skills 到 ToolManager（让工具真正可用）
      await this.reloadSkillsIfPossible('installSkill');

      logger.info('Skill 已安装', { skillId, name: skill.name }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('安装 Skill 失败', { skillId, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`安装 Skill 失败: ${error.message}`, 'error');
    }
  }

  // ============================================================================
  // Skills 仓库管理
  // ============================================================================

  /**
   * 加载仓库配置
   */
  private async handleLoadRepositories(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const repositories = LLMConfigLoader.loadRepositories();

      this.sendData('repositoriesLoaded', { repositories });

      logger.info('Repositories loaded', { count: repositories.length }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load repositories', { error: error.message }, LogCategory.TOOLS);
      this.sendToast(`加载仓库失败: ${error.message}`, 'error');
    }
  }

  /**
   * 添加仓库（简化版：只需要 URL）
   */
  private async handleAddRepository(url: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const { SkillRepositoryManager } = await import('../tools/skill-repository-manager');

      // 先验证仓库
      const manager = new SkillRepositoryManager();
      const repoInfo = await manager.validateRepository(url);

      // 添加仓库
      const result = await LLMConfigLoader.addRepository(url);

      // 更新仓库名称和类型
      LLMConfigLoader.updateRepositoryName(result.id, repoInfo.name);
      LLMConfigLoader.updateRepository(result.id, { type: repoInfo.type });

      this.sendData('repositoryAdded', {
        repository: {
          id: result.id,
          url,
          name: repoInfo.name,
          type: repoInfo.type,
          enabled: true
        }
      });
      this.sendToast(`仓库 "${repoInfo.name}" 已添加（${repoInfo.skillCount} 个技能）`, 'success');

      logger.info('Repository added', { url, name: repoInfo.name, type: repoInfo.type }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to add repository', { url, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`添加仓库失败: ${error.message}`, 'error');
    }
  }

  /**
   * 更新仓库
   */
  private async handleUpdateRepository(repositoryId: string, updates: any): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      LLMConfigLoader.updateRepository(repositoryId, updates);

      this.sendToast('仓库已更新', 'success');

      // 重新加载仓库列表
      await this.handleLoadRepositories();

      logger.info('Repository updated', { id: repositoryId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to update repository', { repositoryId, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`更新仓库失败: ${error.message}`, 'error');
    }
  }

  /**
   * 删除仓库
   */
  private async handleDeleteRepository(repositoryId: string): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      LLMConfigLoader.deleteRepository(repositoryId);

      this.sendData('repositoryDeleted', { repositoryId });
      this.sendToast('仓库已删除', 'success');

      // 重新加载仓库列表
      await this.handleLoadRepositories();

      logger.info('Repository deleted', { id: repositoryId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to delete repository', { repositoryId, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`删除仓库失败: ${error.message}`, 'error');
    }
  }

  /**
   * 刷新仓库缓存
   */
  private async handleRefreshRepository(repositoryId: string): Promise<void> {
    try {
      const { SkillRepositoryManager } = await import('../tools/skill-repository-manager');
      const manager = new SkillRepositoryManager();
      manager.clearCache(repositoryId);

      this.sendData('repositoryRefreshed', { repositoryId });
      this.sendToast('仓库缓存已清除', 'success');

      logger.info('Repository cache cleared', { id: repositoryId }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to refresh repository', { repositoryId, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`刷新仓库失败: ${error.message}`, 'error');
    }
  }

  /**
   * 加载 Skill 库（从所有启用的仓库）
   */
  private async handleLoadSkillLibrary(): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const { SkillRepositoryManager } = await import('../tools/skill-repository-manager');

      // 加载仓库配置
      const repositories = LLMConfigLoader.loadRepositories();
      logger.info('Loaded repositories for skill library', {
        count: repositories.length,
        repositories: repositories.map((r: any) => ({ id: r.id, url: r.url }))
      }, LogCategory.TOOLS);

      // 获取所有 Skills
      const manager = new SkillRepositoryManager();
      const skills = await manager.getAllSkills(repositories);
      logger.info('Fetched skills from all repositories', {
        totalSkills: skills.length,
        byRepository: skills.reduce((acc: any, skill) => {
          acc[skill.repositoryId] = (acc[skill.repositoryId] || 0) + 1;
          return acc;
        }, {})
      }, LogCategory.TOOLS);

      // 检查哪些已安装
      const skillsConfig = LLMConfigLoader.loadSkillsConfig();
      const installedSkills = new Set<string>();
      // 内置工具由 ToolManager 管理，不通过 builtInTools 配置
      if (skillsConfig && Array.isArray(skillsConfig.customTools)) {
        skillsConfig.customTools.forEach((tool: any) => {
          if (tool?.name) {
            installedSkills.add(tool.name);
          }
        });
      }
      if (skillsConfig && Array.isArray(skillsConfig.instructionSkills)) {
        skillsConfig.instructionSkills.forEach((skill: any) => {
          if (skill?.name) {
            installedSkills.add(skill.name);
          }
        });
      }

      // 添加安装状态
      const skillsWithStatus = skills.map(skill => ({
        ...skill,
        installed: installedSkills.has(skill.fullName)
      }));

      this.sendData('skillLibraryLoaded', { skills: skillsWithStatus });

      logger.info('Skill library loaded', {
        totalSkills: skillsWithStatus.length,
        installedCount: skillsWithStatus.filter(s => s.installed).length
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load skill library', { error: error.message, stack: error.stack }, LogCategory.TOOLS);
      this.sendToast(`加载 Skill 库失败: ${error.message}`, 'error');
    }
  }

  /**
   * 获取项目知识（代码索引、ADR、FAQ）
   */
  private async handleGetProjectKnowledge(): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const codeIndex = kb.getCodeIndex();
      const adrs = kb.getADRs();
      const faqs = kb.getFAQs();

      this.sendData('projectKnowledgeLoaded', { codeIndex, adrs, faqs });

      logger.info('项目知识.已加载', {
        files: codeIndex ? codeIndex.files.length : 0,
        adrs: adrs.length,
        faqs: faqs.length
      }, LogCategory.SESSION);
    } catch (error: any) {
      logger.error('项目知识.加载失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`加载项目知识失败: ${error.message}`, 'error');
    }
  }

  /**
   * 获取 ADR 列表
   */
  private async handleGetADRs(filter?: { status?: string }): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const adrs = kb.getADRs(filter as any);
      logger.info('ADR.已加载', { count: adrs.length, filter }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge();
    } catch (error: any) {
      logger.error('ADR.加载失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`加载 ADR 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 获取 FAQ 列表
   */
  private async handleGetFAQs(filter?: { category?: string }): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const faqs = kb.getFAQs(filter);
      logger.info('FAQ.已加载', { count: faqs.length, filter }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge();
    } catch (error: any) {
      logger.error('FAQ.加载失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`加载 FAQ 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 搜索 FAQ
   */
  private async handleSearchFAQs(keyword: string): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const results = kb.searchFAQs(keyword);
      logger.info('FAQ.搜索完成', { keyword, count: results.length }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge();
    } catch (error: any) {
      logger.error('FAQ.搜索失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`搜索 FAQ 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 删除 ADR
   */
  private async handleDeleteADR(id: string): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const success = kb.deleteADR(id);
      if (success) {
        this.sendToast('ADR 已删除', 'success');
        logger.info('ADR.删除成功', { id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge();
      } else {
        this.sendToast('ADR 删除失败', 'error');
      }
    } catch (error: any) {
      logger.error('ADR.删除失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`删除失败: ${error.message}`, 'error');
    }
  }

  /**
   * 删除 FAQ
   */
  private async handleDeleteFAQ(id: string): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const success = kb.deleteFAQ(id);
      if (success) {
        this.sendToast('FAQ 已删除', 'success');
        logger.info('FAQ.删除成功', { id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge();
      } else {
        this.sendToast('FAQ 删除失败', 'error');
      }
    } catch (error: any) {
      logger.error('FAQ.删除失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`删除失败: ${error.message}`, 'error');
    }
  }

  /**
   * 在编辑器中打开文件
   */
  private async handleOpenFile(filePath: string): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      // 构建完整路径
      const fullPath = path.join(kb['projectRoot'], filePath);

      // 检查文件是否存在
      if (!fs.existsSync(fullPath)) {
        this.sendToast('文件不存在', 'error');
        return;
      }

      // 在编辑器中打开文件
      const document = await vscode.workspace.openTextDocument(fullPath);
      await vscode.window.showTextDocument(document);

      logger.info('文件.已打开', { path: filePath }, LogCategory.SESSION);
    } catch (error: any) {
      logger.error('文件.打开失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`打开文件失败: ${error.message}`, 'error');
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

  /**
   * 添加 ADR
   */
  private async handleAddADR(adr: any): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      kb.addADR(adr);
      this.sendToast('ADR 已添加', 'success');
      logger.info('ADR.已添加', { id: adr.id, title: adr.title }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge();
    } catch (error: any) {
      logger.error('ADR.添加失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`添加 ADR 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 更新 ADR
   */
  private async handleUpdateADR(id: string, updates: any): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const success = kb.updateADR(id, updates);
      if (success) {
        this.sendToast('ADR 已更新', 'success');
        logger.info('ADR.已更新', { id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge();
      } else {
        this.sendToast('ADR 不存在', 'warning');
      }
    } catch (error: any) {
      logger.error('ADR.更新失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`更新 ADR 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 添加 FAQ
   */
  private async handleAddFAQ(faq: any): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      kb.addFAQ(faq);
      this.sendToast('FAQ 已添加', 'success');
      logger.info('FAQ.已添加', { id: faq.id, question: faq.question }, LogCategory.SESSION);
      await this.handleGetProjectKnowledge();
    } catch (error: any) {
      logger.error('FAQ.添加失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`添加 FAQ 失败: ${error.message}`, 'error');
    }
  }

  /**
   * 更新 FAQ
   */
  private async handleUpdateFAQ(id: string, updates: any): Promise<void> {
    try {
      const kb = this.projectKnowledgeBase;
      if (!kb) {
        this.sendToast('项目知识库未初始化', 'warning');
        return;
      }

      const success = kb.updateFAQ(id, updates);
      if (success) {
        this.sendToast('FAQ 已更新', 'success');
        logger.info('FAQ.已更新', { id }, LogCategory.SESSION);
        await this.handleGetProjectKnowledge();
      } else {
        this.sendToast('FAQ 不存在', 'warning');
      }
    } catch (error: any) {
      logger.error('FAQ.更新失败', { error: error.message }, LogCategory.SESSION);
      this.sendToast(`更新 FAQ 失败: ${error.message}`, 'error');
    }
  }

  /** 发送适配器连接状态到前端 */
  private async sendWorkerStatus(force: boolean = false): Promise<void> {
    try {
      const now = Date.now();
      if (!force && this.workerStatusCache && (now - this.workerStatusCacheAt) < this.workerStatusCacheTtlMs) {
        this.sendData('workerStatusUpdate', { statuses: this.workerStatusCache });
        return;
      }

      if (this.workerStatusInFlight) {
        if (!force) {
          return;
        }
        await this.workerStatusInFlight;
      }

      const runCheck = this.performWorkerStatusCheck(force);
      this.workerStatusInFlight = runCheck;
      await runCheck;
    } catch (error: any) {
      logger.error('界面.模型状态.检查_失败', { error: error.message }, LogCategory.UI);
    } finally {
      this.workerStatusInFlight = null;
    }
  }

  private async performWorkerStatusCheck(force: boolean): Promise<void> {
    const { LLMConfigLoader } = await import('../llm/config');
    const { getOrCreateLLMClient } = await import('../llm/clients/client-factory');

    const config = LLMConfigLoader.loadFullConfig();
    const statuses: Record<string, { status: string; model?: string; error?: string }> = {};
    const now = Date.now();
    const priorityModels: Array<'orchestrator' | 'compressor'> = ['orchestrator', 'compressor'];
    const workerModels: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    const modelIds = [...priorityModels, ...workerModels];
    const formatModelLabel = (modelConfig: any): string | undefined => {
      if (!modelConfig?.provider || !modelConfig?.model) return undefined;
      return `${modelConfig.provider} - ${modelConfig.model}`;
    };
    const orchestratorLabel = formatModelLabel(config.orchestrator);
    const compressorFallbackLabel = orchestratorLabel ? `编排模型: ${orchestratorLabel}` : '编排模型';
    const setCompressorFallback = (reason: string) => {
      statuses.compressor = { status: 'fallback', model: compressorFallbackLabel, error: reason };
    };

    const getCachedStatus = (name: string) => {
      if (!this.workerStatusCache) return null;
      if ((now - this.workerStatusCacheAt) > this.workerStatusSoftTtlMs) return null;
      return this.workerStatusCache[name] || null;
    };

    const applyQuickStatus = (name: string, modelLabel?: string) => {
      const cached = getCachedStatus(name);
      if (cached) {
        statuses[name] = {
          status: cached.status,
          model: cached.model || modelLabel,
          error: cached.error
        };
        return true;
      }
      return false;
    };

    // 测试模型的通用函数（使用快速 Models API）
    const testModel = async (name: string, modelConfig: any, isRequired: boolean = false) => {
      const isCompressor = name === 'compressor';
      if (!modelConfig.enabled || !modelConfig.apiKey || !modelConfig.model) {
        if (isCompressor) {
          setCompressorFallback(!modelConfig.enabled ? '压缩模型未启用' : '压缩模型未配置');
          return;
        }
        if (!modelConfig.enabled) {
          statuses[name] = {
            status: 'disabled',
            model: '已禁用'
          };
          return;
        }
        statuses[name] = {
          status: 'not_configured',
          model: isRequired ? '未配置（必需）' : '未配置'
        };
        return;
      }

      const modelLabel = formatModelLabel(modelConfig) || '未配置';
      if (!force) {
        const isConnected = name !== 'compressor'
          && this.adapterFactory.isConnected(name as AgentType);
        if (isConnected) {
          statuses[name] = { status: 'available', model: modelLabel };
          return;
        }
        if (applyQuickStatus(name, modelLabel)) {
          return;
        }
      }

      try {
        statuses[name] = {
          status: 'checking',
          model: modelLabel
        };

        // 使用快速连接测试（Models API）
        const client = getOrCreateLLMClient(modelConfig);
        const result = await client.testConnectionFast();

        if (result.success) {
          // 检查模型是否存在（如果 API 支持）
          if (result.modelExists === false) {
            if (isCompressor) {
              setCompressorFallback(`模型不存在: ${modelConfig.model}`);
              return;
            }
            statuses[name] = { status: 'invalid_model', model: modelLabel, error: `模型不存在: ${modelConfig.model}` };
          } else {
            statuses[name] = {
              status: 'available',
              model: modelLabel
            };
          }
        } else {
          if (isCompressor) {
            setCompressorFallback(result.error || '压缩模型连接失败');
            return;
          }
          // 根据错误类型设置状态
          let status = 'error';
          if (result.error?.includes('API Key')) {
            status = 'auth_failed';
          } else if (result.error?.includes('网络') || result.error?.includes('连接')) {
            status = 'network_error';
          } else if (result.error?.includes('超时')) {
            status = 'timeout';
          }

          statuses[name] = {
            status,
            model: modelLabel,
            error: result.error
          };
        }

        logger.info(`Model connection test (fast): ${name}`, {
          provider: modelConfig.provider,
          model: modelConfig.model,
          success: result.success,
          modelExists: result.modelExists,
        }, LogCategory.LLM);
      } catch (error: any) {
        if (isCompressor) {
          setCompressorFallback(error.message || '压缩模型连接失败');
          return;
        }
        statuses[name] = { status: 'error', model: modelLabel, error: error.message };

        logger.warn(`Model connection test failed: ${name}`, {
          error: error.message
        }, LogCategory.LLM);
      }
    };

    // 初始化占位状态，确保 UI 先显示检测中/缓存结果
    modelIds.forEach(name => {
      const modelConfig = name === 'orchestrator'
        ? config.orchestrator
        : name === 'compressor'
          ? config.compressor
          : config.workers[name as WorkerSlot];

      if (name === 'compressor' && (!modelConfig?.enabled || !modelConfig?.apiKey || !modelConfig?.model)) {
        setCompressorFallback(!modelConfig?.enabled ? '压缩模型未启用' : '压缩模型未配置');
        return;
      }

      if (!modelConfig?.enabled) {
        statuses[name] = { status: 'disabled', model: '已禁用' };
        return;
      }
      if (!modelConfig?.apiKey || !modelConfig?.model) {
        statuses[name] = {
          status: 'not_configured',
          model: name === 'orchestrator' || name === 'compressor' ? '未配置（必需）' : '未配置'
        };
        return;
      }

      const modelLabel = formatModelLabel(modelConfig) || '未配置';
      if (!force) {
        const isConnected = name !== 'compressor'
          && this.adapterFactory.isConnected(name as AgentType);
        if (isConnected) {
          statuses[name] = { status: 'available', model: modelLabel };
          return;
        }
        if (applyQuickStatus(name, modelLabel)) {
          return;
        }
      }
      statuses[name] = { status: 'checking', model: modelLabel };
    });

    this.sendData('workerStatusUpdate', { statuses });

    // 所有模型并行检测（不再串行）
    await Promise.all([
      testModel('orchestrator', config.orchestrator, true),
      testModel('compressor', config.compressor, true),
      ...workerModels.map(worker => testModel(worker, config.workers[worker]))
    ]);

    this.workerStatusCache = statuses;
    this.workerStatusCacheAt = Date.now();

    this.sendData('workerStatusUpdate', { statuses });

    logger.info('Model connection status check completed', {
      results: Object.entries(statuses).map(([name, s]) => `${name}: ${s.status}`),
      mode: force ? 'hard' : 'soft'
    }, LogCategory.LLM);
  }

  /** 发送执行统计数据到前端 */
  private sendExecutionStats(): void {
    const executionStats = this.orchestratorEngine.getExecutionStats();
    if (!executionStats) {
      logger.info('界面.执行统计.未初始化', undefined, LogCategory.UI);
      return;
    }

    const modelCatalog = this.buildModelCatalog();
    const modelIds = modelCatalog.map(entry => entry.id);
    const stats = executionStats.getAllStats(modelIds).map(workerStats => ({
      worker: workerStats.worker,
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

  // ============================================================================
  // 画像配置处理
  // ============================================================================

  /** 发送画像配置到 UI */
  private async sendProfileConfig(): Promise<void> {
    const assignments = WorkerAssignmentStorage.ensureDefaults();
    const assignmentMap: Record<string, string> = {};

    for (const [worker, categories] of Object.entries(assignments.assignments)) {
      for (const category of categories) {
        assignmentMap[category] = worker;
      }
    }

    const categoryGuidance: Record<string, any> = {};
    for (const [category, definition] of Object.entries(CATEGORY_DEFINITIONS)) {
      categoryGuidance[category] = {
        displayName: definition.displayName,
        description: definition.description,
        guidance: {
          focus: definition.guidance?.focus || [],
          constraints: definition.guidance?.constraints || [],
        },
        priority: definition.priority,
        riskLevel: definition.riskLevel,
      };
    }

    const uiConfig: any = {
      assignments: assignmentMap,
      categoryGuidance,
      categoryPriority: CATEGORY_RULES.categoryPriority,
      configPath: WorkerAssignmentStorage.getConfigPath(),
    };

    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const userRules = LLMConfigLoader.loadUserRules();
      uiConfig.userRules = userRules.content || '';
    } catch (error) {
      logger.warn('加载用户规则失败', { error }, LogCategory.LLM);
    }

    this.sendData('profileConfig', { config: uiConfig });
  }

  /** 保存画像配置 */
  private async handleSaveProfileConfig(data: { assignments: Record<string, string>; userRules?: string }): Promise<void> {
    try {
      const workerAssignments: Record<WorkerSlot, string[]> = {
        claude: [],
        codex: [],
        gemini: [],
      };

      const assignmentMap = data.assignments || {};
      for (const [category, worker] of Object.entries(assignmentMap)) {
        const normalizedWorker = String(worker).toLowerCase() as WorkerSlot;
        if (!['claude', 'codex', 'gemini'].includes(normalizedWorker)) {
          throw new Error(`未知 Worker: ${worker}`);
        }
        workerAssignments[normalizedWorker].push(category);
      }

      WorkerAssignmentStorage.save({
        version: WORKER_ASSIGNMENTS_VERSION,
        assignments: workerAssignments,
      });

      try {
        const { LLMConfigLoader } = await import('../llm/config');
        const userRulesContent = typeof data.userRules === 'string' ? data.userRules : '';
        const trimmed = userRulesContent.trim();
        LLMConfigLoader.updateUserRules({
          enabled: trimmed.length > 0,
          content: userRulesContent,
        });
        this.adapterFactory.refreshUserRules();
      } catch (rulesError) {
        logger.warn('保存用户规则失败', { error: rulesError }, LogCategory.LLM);
      }
      this.sendData('profileConfigSaved', { success: true });
      this.sendToast('画像配置已保存', 'success');

      try {
        await this.orchestratorEngine.reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        this.sendToast(`画像重载失败: ${reloadMsg}`, 'warning');
      }
      await this.sendProfileConfig();
      logger.info('界面.画像.配置.已保存', { path: WorkerAssignmentStorage.getConfigPath() }, LogCategory.UI);
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendData('profileConfigSaved', { success: false, error: errorMsg });
      this.sendToast(`保存失败: ${errorMsg}`, 'error');
    }
  }

  /** 重置画像配置 */
  private async handleResetProfileConfig(): Promise<void> {
    try {
      const defaults = WorkerAssignmentStorage.buildDefault();
      WorkerAssignmentStorage.save(defaults);
      try {
        const { LLMConfigLoader } = await import('../llm/config');
        LLMConfigLoader.updateUserRules({ enabled: false, content: '' });
        this.adapterFactory.refreshUserRules();
      } catch (rulesError) {
        logger.warn('重置用户规则失败', { error: rulesError }, LogCategory.LLM);
      }
      try {
        await this.orchestratorEngine.reloadProfiles();
      } catch (reloadError) {
        const reloadMsg = reloadError instanceof Error ? reloadError.message : String(reloadError);
        this.sendToast(`画像重载失败: ${reloadMsg}`, 'warning');
      }
      this.sendToast('画像配置已重置为默认值', 'success');
      this.sendData('profileConfigReset', { success: true });
      await this.sendProfileConfig();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendData('profileConfigReset', { success: false, error: errorMsg });
      this.sendToast(`重置失败: ${errorMsg}`, 'error');
    }
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
      const absolutePath = path.isAbsolute(filepath)
        ? filepath
        : path.join(this.workspaceRoot, filepath);

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
      // 统一 Todo 系统 - 使用 orchestratorEngine
      await this.orchestratorEngine.startTaskById(taskId);
      this.sendToast('任务已开始', 'success');
      this.sendStateUpdate();
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.sendToast(`启动失败: ${errorMsg}`, 'error');
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

  /** 处理执行中追加输入：默认语义为“打断并按新输入重启” */
  private async handleAppendMessage(taskId: string, content: string): Promise<void> {
    logger.info('界面.消息.补充.请求', { taskId, preview: content.substring(0, 50) }, LogCategory.UI);

    const trimmedContent = content.trim();
    if (!trimmedContent) {
      this.sendToast('补充内容不能为空', 'warning');
      return;
    }

    try {
      const wasRunning = this.orchestratorEngine.running;

      // 默认语义：追加输入 = 打断当前执行并基于最新输入重新开始
      if (wasRunning) {
        await this.interruptCurrentTask({ silent: true });
        this.sendToast('已打断当前执行，正在按新输入重新开始', 'info');
      }

      await this.executeTask(trimmedContent, undefined, []);
      logger.info('界面.消息.补充.重启执行_成功', { taskId, wasRunning }, LogCategory.UI);
    } catch (error) {
      logger.error('界面.消息.补充.失败', error, LogCategory.UI);
      this.sendToast('补充内容失败', 'error');
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
      this.emitUserAndPlaceholder(requestKey, promptForDisplay, images?.length || 0, images);
      this.scheduleRequestTimeout(requestKey);

      // 🔧 性能优化：强制让出事件循环 (Yield Event Loop)
      // 原因：emitUserAndPlaceholder 只是将消息入队，实际的 webview.postMessage 需要事件循环 Tick 才能执行。
      // 如果不在此处让出控制权，后续的同步 FS 操作（图片保存）和 Orchestrator 初始化会阻塞主线程，
      // 导致前端迟迟收不到用户消息的回显，造成"点击发送后卡顿"的假象。
      await new Promise(resolve => setTimeout(resolve, 0));

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

      const orchestrationCommand = this.parseOrchestrationCommand(prompt);
      if (orchestrationCommand?.type === 'plan') {
        await this.executePlanOnly(orchestrationCommand.payload, imagePaths);
        success = true;
        return;
      }
      if (orchestrationCommand?.type === 'start-work') {
        await this.executeStartWork(orchestrationCommand.payload);
        success = true;
        return;
      }

      if (useIntelligentMode) {
        // 智能编排模式：统一串行化，避免引擎并发
        const result = await this.enqueueOrchestratorExecution(effectivePrompt, imagePaths);
        success = result.success;
        failureReason = result.error;
      } else {
        // 直接执行模式：指定 Worker 直接执行
        const result = await this.executeWithDirectWorker(effectivePrompt, forceWorker || this.selectedWorker!, imagePaths);
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
        if (success) {
          this.messageHub.sendControl(ControlMessageType.TASK_COMPLETED, {
            requestId: requestKey,
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
    }
  }

  private resolveInstructionSkillPrompt(prompt: string): { prompt: string; skillName?: string } {
    const trimmed = prompt.trim();
    if (!trimmed) {
      return { prompt };
    }
    if (trimmed.startsWith('/plan') || trimmed.startsWith('@plan') || trimmed.startsWith('/start-work') || trimmed.startsWith('/start')) {
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

  private async handleApplyInstructionSkill(
    skillName: string,
    args?: string,
    images?: Array<{ dataUrl: string }>,
    agent?: WorkerSlot,
    requestId?: string
  ): Promise<void> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const config = LLMConfigLoader.loadSkillsConfig();
      const skills: InstructionSkillDefinition[] = Array.isArray(config?.instructionSkills) ? config.instructionSkills : [];
      const skill = skills.find((item) => item.name === skillName);
      if (!skill) {
        this.sendToast(`未找到 Skill: ${skillName}`, 'error');
        return;
      }

      const prompt = buildInstructionSkillPrompt(skill, args || '');
      const displayPrompt = args && args.trim()
        ? `使用 Skill: ${skill.name}\n${args.trim()}`
        : `使用 Skill: ${skill.name}`;
      await this.executeTask(prompt, agent || undefined, images || [], requestId, displayPrompt);
    } catch (error: any) {
      logger.error('应用 Skill 失败', { skillName, error: error.message }, LogCategory.TOOLS);
      this.sendToast(`应用 Skill 失败: ${error.message}`, 'error');
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
      this.sendToast('请输入计划内容，例如：/plan 实现登录功能', 'warning');
      return;
    }
    // 统一 Todo 系统 - 使用 orchestratorEngine
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id || 'default';
    const task = await this.orchestratorEngine.createTaskFromPrompt(sessionId, planPrompt);
    try {
      const record = await this.orchestratorEngine.createPlan(
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
      ? this.orchestratorEngine.getPlanById(planId, sessionId)
      : (sessionId ? this.orchestratorEngine.getActivePlanForSession(sessionId) : null);

    if (!record) {
      this.sendToast('未找到可执行的计划，请先使用 /plan 生成计划', 'warning');
      return;
    }

    try {
      const result = await this.orchestratorEngine.executePlanRecord(
        record,
        record.taskId,
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
      const changes = this.snapshotManager.getPendingChanges().filter(c => c.todoId === subTaskId);
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

  private buildSubTaskSummaryCard(data: { description?: string; agent?: string; duration?: number; modifiedFiles?: string[]; subTaskId?: string; error?: string }, status: 'completed' | 'failed') {
    const title = status === 'completed' ? '子任务完成' : '子任务失败';
    const description = data.description || data.subTaskId || '未知子任务';
    // 优化 executor fallback：使用中文，并提供更友好的默认值
    // 当 agent 为空时，显示 "编排者" 而不是 "未知"，因为这通常是编排者协调的任务
    const executor = data.agent || '编排者';
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
      const taskContext = await this.orchestratorEngine.executeWithTaskContext(prompt, this.activeSessionId || undefined);
      const result = taskContext.result;

      // 获取执行计划，判断是否需要 Worker
      const plan = this.orchestratorEngine.plan;
      const needsWorker = plan?.needsWorker !== false && (plan?.subTasks?.length ?? 0) > 0;
      logger.info('界面.任务.完成', { needsWorker, subTaskCount: plan?.subTasks?.length || 0, hasResult: !!result?.trim(), resultLength: result?.length || 0 }, LogCategory.UI);

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
      logger.error('界面.执行.智能.失败', error, LogCategory.UI);
      errorMsg = error instanceof Error ? error.message : String(error);

      this.sendOrchestratorMessage({
        content: errorMsg,
        messageType: 'error',
      });
      success = false;
    }

    this.sendStateUpdate();
    if (!success) {
      return { success: false, error: errorMsg };
    }
    return { success: true };
  }

  /** 直接 Worker 执行模式 */
  private async executeWithDirectWorker(prompt: string, targetWorker: WorkerSlot, imagePaths: string[]): Promise<OrchestratorExecutionResult> {
    logger.info('界面.执行.模式.直接', { agent: targetWorker }, LogCategory.UI);

    // 立即发送"正在处理"消息
    this.sendOrchestratorMessage({
      content: `${targetWorker.toUpperCase()} 正在处理您的请求...`,
      messageType: 'progress'
    });

    const startTime = Date.now();
    // 统一 Todo 系统 - 使用 orchestratorEngine
    const sessionId = this.activeSessionId || this.sessionManager.getCurrentSession()?.id || 'default';
    const task = await this.orchestratorEngine.createTaskFromPrompt(sessionId, prompt);
    await this.orchestratorEngine.startTaskById(task.id);
    this.sendStateUpdate();

    let errorMsg: string | undefined;
    let success = false;
    try {
      logger.info('界面.执行.直接.请求', { worker: targetWorker }, LogCategory.UI);
      const response = await this.adapterFactory.sendMessage(targetWorker, prompt, imagePaths);
      logger.info('界面.执行.直接.响应', { worker: targetWorker, preview: response.content?.substring(0, 100) }, LogCategory.UI);
      const executionStats = this.orchestratorEngine.getExecutionStats();
      if (executionStats) {
        executionStats.recordExecution({
          worker: targetWorker,
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
        errorMsg = response.error;
        await this.orchestratorEngine.failTaskById(task.id, response.error);
        this.sendOrchestratorMessage({
          content: `${targetWorker.toUpperCase()}: ${response.error}`,
          messageType: 'error',
          metadata: { worker: targetWorker },
        });
      } else {
        await this.orchestratorEngine.completeTaskById(task.id);
        this.saveMessageToSession(prompt, response.content || '', targetWorker, 'worker');
        const requestId = this.messageHub.getRequestContext();
        const requestStats = requestId ? this.messageHub.getRequestMessageStats(requestId) : undefined;
        const assistantWorkerContent = requestStats?.assistantWorkerContent ?? 0;
        if (requestId && assistantWorkerContent === 0 && response.content?.trim()) {
          this.messageHub.workerOutput(targetWorker, response.content, {
            metadata: {
              requestId,
              forced: true,
              reason: 'missing_llm_stream',
            },
          });
        }
        success = true;
      }

    } catch (error) {
      logger.error('界面.执行.直接.失败', error, LogCategory.UI);
      errorMsg = error instanceof Error ? error.message : String(error);
      await this.orchestratorEngine.failTaskById(task.id, errorMsg);
      const executionStats = this.orchestratorEngine.getExecutionStats();
      if (executionStats) {
        executionStats.recordExecution({
          worker: targetWorker,
          taskId: task.id,
          subTaskId: `direct-${task.id}`,
          success: false,
          duration: Date.now() - startTime,
          error: errorMsg,
        });
      }
      this.sendOrchestratorMessage({
        content: `${targetWorker.toUpperCase()}: ${errorMsg}`,
        messageType: 'error',
        metadata: { worker: targetWorker },
      });
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
        this.sendData('sessionCreated', { session: newSession as any });
      }
      this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() as any[] });
      this.sendToast('会话已删除', 'info');
    }
    this.sendStateUpdate();
  }

  private sendCurrentSessionToWebview(): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      return;
    }
    this.sendData('sessionLoaded', { session: session as any });
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
    this.sendData('sessionCreated', { session: newSession as any });
    this.sendData('sessionsUpdated', { sessions: this.sessionManager.getSessionMetas() as any[] });
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
    const activePlanRecordRaw = currentSession?.id
      ? this.orchestratorEngine.getActivePlanForSession(currentSession.id)
      : null;
    const activePlanRecord = activePlanRecordRaw?.plan?.needsWorker === false ? null : activePlanRecordRaw;

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
      sessions: sessionMetas as any[],
      currentTask,
      tasks,
      workerStatuses,
      pendingChanges,
      isRunning,
      logs,
      interactionMode: this.orchestratorEngine.getInteractionMode(),
      interactionModeUpdatedAt: this.interactionModeUpdatedAt,
      orchestratorPhase: this.orchestratorEngine.phase,
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
    const templatePath = path.join(this.extensionUri.fsPath, 'out', 'webview', 'index.html');

    if (!fs.existsSync(templatePath)) {
      const message = `Svelte webview 未构建: ${templatePath}`;
      logger.error('界面.Svelte.未构建', { path: templatePath }, LogCategory.UI);
      throw new Error(message);
    }

    let html = fs.readFileSync(templatePath, 'utf-8');
    const cacheBuster = Date.now().toString();

    // 获取 webview 资源根目录
    const webviewAssetsUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.extensionUri.fsPath, 'out', 'webview', 'assets'))
    );

    // 替换资源路径（Vite 构建输出使用 /assets/ 前缀）
    html = html.replace(/src="\/assets\//g, `src="${webviewAssetsUri}/`);
    html = html.replace(/href="\/assets\//g, `href="${webviewAssetsUri}/`);

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
      }

      // 2. 清理适配器（关闭所有连接）
      if (this.adapterFactory) {
        logger.info('界面.销毁.适配器.清理', undefined, LogCategory.UI);
        await this.adapterFactory.shutdown();
      }

      // 3. 主动拒绝所有待处理工具授权，避免悬挂
      this.clearActiveToolAuthorizationTimer();
      this.activeToolAuthorizationRequestId = null;
      this.toolAuthorizationQueue = [];
      for (const callback of this.toolAuthorizationCallbacks.values()) {
        callback(false);
      }
      this.toolAuthorizationCallbacks.clear();

      // 4. 移除事件监听器
      globalEventBus.clear();
      logger.info('界面.销毁.事件.已清理', undefined, LogCategory.UI);

      // 5. 清理待确认的 Promise
      if (this.pendingConfirmation) {
        this.pendingConfirmation.reject(new Error('扩展已停用'));
        this.pendingConfirmation = null;
      }
      if (this.pendingQuestion) {
        this.pendingQuestion.reject(new Error('扩展已停用'));
        this.pendingQuestion = null;
      }
      // 6. 清理 Webview
      this._view = undefined;

      logger.info('界面.销毁.完成', undefined, LogCategory.UI);
    } catch (error) {
      logger.error('界面.销毁.失败', error, LogCategory.UI);
    }
  }
}
