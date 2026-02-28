/**
 * 统一消息协议 (Unified Message Protocol)
 *
 * 核心设计理念：
 * 1. 所有 Agent 输出在适配层完成标准化，Webview 只负责渲染
 * 2. 明确的消息类型枚举，消除格式猜测
 * 3. 统一的生命周期状态机，消除状态混乱
 * 4. 可扩展的内容结构，支持各种富文本元素
 */

import type { AgentType } from '../types/agent-types';

// ============================================================================
// 消息类型枚举
// ============================================================================

/**
 * 标准消息类型
 * 有限且明确，Webview 根据类型选择渲染组件
 */
export enum MessageType {
  /** 普通文本消息 */
  TEXT = 'text',
  /** 执行计划 */
  PLAN = 'plan',
  /** 进度更新 */
  PROGRESS = 'progress',
  /** 执行结果 */
  RESULT = 'result',
  /** 错误消息 */
  ERROR = 'error',
  /** 需要用户交互（确认/问题/权限） */
  INTERACTION = 'interaction',
  /** 系统通知 */
  SYSTEM = 'system-notice',
  /** 工具调用 */
  TOOL_CALL = 'tool_call',
  /** 思考过程 */
  THINKING = 'thinking',

  // ============== 新增消息类型（方案 B 扩展）==============
  /** 用户输入消息 */
  USER_INPUT = 'user_input',
  /** 任务状态卡片（Worker 执行状态摘要，主对话区展示） */
  TASK_CARD = 'task_card',
  /** 任务说明（编排者派发给 Worker 的详细任务描述） */
  INSTRUCTION = 'instruction',
}

// ============================================================================
// 统一消息分类（unified-message-channel-design.md v2.5）
// ============================================================================

/**
 * 消息大类（MessageCategory）
 *
 * 用于统一消息通道的顶层分类路由：
 * - CONTENT: 内容消息（LLM 响应、结果等），使用 blocks 渲染
 * - CONTROL: 控制消息（阶段、任务状态），驱动前端状态机
 * - NOTIFY: 通知消息（Toast），短暂提示
 * - DATA: 数据消息（状态同步），后端数据下发
 */
export enum MessageCategory {
  /** 内容消息（LLM 响应、结果、错误） */
  CONTENT = 'content',
  /** 控制消息（阶段变化、任务状态） */
  CONTROL = 'control',
  /** 通知消息（Toast 提示） */
  NOTIFY = 'notify',
  /** 数据消息（状态同步、配置加载） */
  DATA = 'data',
}

/**
 * 控制消息子类型（ControlMessageType）
 *
 * CONTROL 类别消息的具体类型，用于前端状态机处理
 */
export enum ControlMessageType {
  /** 阶段变化 */
  PHASE_CHANGED = 'phase_changed',
  /** 任务已接受 */
  TASK_ACCEPTED = 'task_accepted',
  /** 任务被拒绝 */
  TASK_REJECTED = 'task_rejected',
  /** 任务开始执行 */
  TASK_STARTED = 'task_started',
  /** 任务完成 */
  TASK_COMPLETED = 'task_completed',
  /** 任务失败 */
  TASK_FAILED = 'task_failed',
  /** Worker 状态更新 */
  WORKER_STATUS = 'worker_status',
}

/**
 * 通知消息级别（NotifyLevel）
 */
export type NotifyLevel = 'info' | 'success' | 'warning' | 'error';

/**
 * 数据消息类型（DataMessageType）
 */
export type DataMessageType =
  | 'allWorkerConfigsLoaded'
  | 'assignmentCompleted'
  | 'assignmentPlanned'
  | 'assignmentStarted'
  | 'clarificationRequest'
  | 'compressorConfigLoaded'
  | 'compressorConnectionTestResult'
  | 'confirmationRequest'
  | 'customToolAdded'
  | 'customToolRemoved'
  | 'dynamicTodoAdded'
  | 'executionStatsUpdate'
  | 'instructionSkillRemoved'
  | 'interactionModeChanged'
  | 'mcpServerAdded'
  | 'mcpServerDeleted'
  | 'mcpServerTools'
  | 'mcpServerUpdated'
  | 'mcpServersLoaded'
  | 'mcpToolsRefreshed'
  | 'missionExecutionFailed'
  | 'missionFailed'
  | 'missionPlanned'
  | 'modelListFetched'
  | 'orchestratorConfigLoaded'
  | 'orchestratorConnectionTestResult'
  | 'profileConfig'
  | 'profileConfigReset'
  | 'profileConfigSaved'
  | 'projectKnowledgeLoaded'
  | 'processingStateChanged'
  | 'promptEnhanceConfigLoaded'
  | 'promptEnhanceResult'
  | 'promptEnhanced'
  | 'questionRequest'
  | 'recoveryRequest'
  | 'repositoriesLoaded'
  | 'repositoryAdded'
  | 'repositoryAddFailed'
  | 'repositoryDeleted'
  | 'repositoryRefreshed'
  | 'sessionCreated'
  | 'sessionLoaded'
  | 'sessionMessagesLoaded'
  | 'sessionSwitched'
  | 'sessionsUpdated'
  | 'skillInstalled'
  | 'skillUpdated'
  | 'allSkillsUpdated'
  | 'skillLibraryLoaded'
  | 'skillsConfigLoaded'
  | 'stateUpdate'
  | 'todoApprovalRequested'
  | 'todoCompleted'
  | 'todoFailed'
  | 'todoStarted'
  | 'toolAuthorizationRequest'
  | 'workerConnectionTestResult'
  | 'workerQuestionRequest'
  | 'workerSessionCreated'
  | 'workerSessionResumed'
  | 'workerStatusUpdate';

/**
 * 消息生命周期状态
 * 明确的状态机，消除 streaming/pendingComplete 等混乱标记
 */
export enum MessageLifecycle {
  /** 消息开始，准备接收内容 */
  STARTED = 'started',
  /** 正在流式输出 */
  STREAMING = 'streaming',
  /** 消息完成 */
  COMPLETED = 'completed',
  /** 消息失败 */
  FAILED = 'failed',
  /** 消息被取消 */
  CANCELLED = 'cancelled',
}

/**
 * 消息来源
 */
export type MessageSource = 'orchestrator' | 'worker';

/**
 * 消息可见性
 * - 'user': 用户可见（默认）
 * - 'system': 仅系统日志可见，不展示给用户
 * - 'debug': 仅调试模式可见
 */
export type MessageVisibility = 'user' | 'system' | 'debug';

// ============================================================================
// 内容块类型
// ============================================================================

/**
 * 文本内容块
 */
export interface TextBlock {
  type: 'text';
  content: string;
  /** 是否为 Markdown 格式 */
  isMarkdown?: boolean;
}

/**
 * 代码块
 */
export interface CodeBlock {
  type: 'code';
  language: string;
  content: string;
  filename?: string;
  /** 高亮行号 */
  highlightLines?: number[];
  /** 是否为嵌入式代码块（在文本中间的代码块，通常是内部数据，不需要显示给用户） */
  isEmbedded?: boolean;
}

/**
 * 思考过程块
 */
export interface ThinkingBlock {
  type: 'thinking';
  content: string;
  /** 思考摘要（用于折叠显示） */
  summary?: string;
  /** 用于增量更新的块 ID */
  blockId?: string;
}

/**
 * 工具结果标准化状态
 * 统一三类工具（builtin/mcp/skill）的机器可读状态语义
 */
export type StandardizedToolStatus =
  | 'success'
  | 'error'
  | 'timeout'
  | 'killed'
  | 'blocked'
  | 'rejected'
  | 'aborted';

/**
 * 工具结果标准化数据（供前端/统计/诊断使用）
 */
export interface StandardizedToolResultPayload {
  schemaVersion: 'tool-result.v1';
  source: 'builtin' | 'mcp' | 'skill';
  toolName: string;
  toolCallId: string;
  status: StandardizedToolStatus;
  message: string;
  data?: unknown;
  errorCode?: string;
  sourceId?: string;
}

/**
 * 工具调用块
 */
export interface ToolCallBlock {
  type: 'tool_call';
  toolName: string;
  toolId: string;
  /** 工具调用状态 */
  status: 'pending' | 'running' | 'completed' | 'failed';
  /** 输入参数（JSON 字符串格式，后端统一序列化） */
  input?: string;
  /** 输出结果 */
  output?: string;
  /** 错误信息 */
  error?: string;
  /** 统一工具结果元数据（可选） */
  standardized?: StandardizedToolResultPayload;
  /** 是否可恢复（用于错误/切换判断） */
  recoverable?: boolean;
  /** 持续时间（毫秒） */
  duration?: number;
}

/**
 * 文件变更块
 */
export interface FileChangeBlock {
  type: 'file_change';
  filePath: string;
  changeType: 'create' | 'modify' | 'delete';
  additions?: number;
  deletions?: number;
  /** Diff 内容 */
  diff?: string;
}

/**
 * 规划块 - 结构化的任务规划数据
 * 用于展示 AI 生成的任务分析和执行计划
 */
export interface PlanBlock {
  type: 'plan';
  /** 目标描述 */
  goal: string;
  /** 任务分析 */
  analysis?: string;
  /** 约束条件 */
  constraints?: string[];
  /** 验收标准 */
  acceptanceCriteria?: string[];
  /** 风险等级 */
  riskLevel?: 'low' | 'medium' | 'high';
  /** 风险因素 */
  riskFactors?: string[];
  /** 原始 JSON 内容（用于调试或详细查看） */
  rawJson?: string;
}

/**
 * 内容块联合类型
 */
export type ContentBlock =
  | TextBlock
  | CodeBlock
  | ThinkingBlock
  | ToolCallBlock
  | FileChangeBlock
  | PlanBlock;

// ============================================================================
// 交互请求类型
// ============================================================================

/**
 * 交互请求类型
 */
export enum InteractionType {
  /** 计划确认 */
  PLAN_CONFIRMATION = 'plan_confirmation',
  /** 问题询问 */
  QUESTION = 'question',
  /** 权限请求 */
  PERMISSION = 'permission',
  /** 澄清请求 */
  CLARIFICATION = 'clarification',
}

/**
 * 交互请求
 */
export interface InteractionRequest {
  type: InteractionType;
  /** 请求 ID（用于响应匹配） */
  requestId: string;
  /** 提示文本 */
  prompt: string;
  /** 选项（如果有） */
  options?: Array<{
    value: string;
    label: string;
    isDefault?: boolean;
  }>;
  /** 是否必须响应 */
  required: boolean;
  /** 超时时间（毫秒） */
  timeout?: number;
}

// ============================================================================
// 标准消息接口
// ============================================================================

/**
 * CONTROL 类别专属字段
 */
export interface ControlPayload {
  /** 控制消息子类型 */
  controlType: ControlMessageType;
  /** 控制消息负载 */
  payload: Record<string, unknown>;
}

/**
 * NOTIFY 类别专属字段
 */
export interface NotifyPayload {
  /** 通知级别 */
  level: NotifyLevel;
  /** 显示时长（毫秒），默认 3000 */
  duration?: number;
}

/**
 * DATA 类别专属字段
 */
export interface DataPayload {
  /** 数据消息类型 */
  dataType: DataMessageType;
  /** 数据负载 */
  payload: Record<string, unknown>;
}

/**
 * 标准消息 - 统一消息协议的核心
 *
 * 所有 Agent 输出经过 Normalizer 转换后都变成这个格式
 * Webview 只需要根据这个接口渲染，无需任何解析逻辑
 *
 * 🔧 统一消息通道（unified-message-channel-design.md v2.5）：
 * - 新增 category 字段（必填），用于顶层分类路由
 * - 新增 control/notify/data 专属字段
 */
export interface StandardMessage {
  /** 消息唯一标识 */
  id: string;

  /** 事件唯一标识（用于事件流排障） */
  eventId?: string;

  /** 事件顺序号（会话内单调递增） */
  eventSeq?: number;

  /** 追踪 ID（用于关联同一任务的多条消息） */
  traceId: string;

  /**
   * 🔧 消息大类（必填）
   * 用于前端路由：CONTENT → 渲染，CONTROL → 状态机，NOTIFY → Toast，DATA → 同步
   */
  category: MessageCategory;

  /** 消息类型 */
  type: MessageType;

  /** 消息来源 */
  source: MessageSource;

  /** Agent 类型 */
  agent: AgentType;

  /** 生命周期状态 */
  lifecycle: MessageLifecycle;

  /** 内容块列表（已解析，标准化） */
  blocks: ContentBlock[];

  /** 交互请求（如果需要用户响应） */
  interaction?: InteractionRequest;

  /** 元数据 */
  metadata: MessageMetadata;

  /** 时间戳 */
  timestamp: number;

  /** 更新时间（流式消息会持续更新） */
  updatedAt: number;

  // ========== 分类专属字段 ==========

  /**
   * 🔧 CONTROL 类别专属
   * 当 category === CONTROL 时必填
   */
  control?: ControlPayload;

  /**
   * 🔧 NOTIFY 类别专属
   * 当 category === NOTIFY 时必填
   */
  notify?: NotifyPayload;

  /**
   * 🔧 DATA 类别专属
   * 当 category === DATA 时必填
   */
  data?: DataPayload;

  /**
   * 消息可见性（仅 CONTENT 类别需要）
   * - 'user': 用户可见（默认）
   * - 'system': 仅系统日志可见，不展示给用户
   * - 'debug': 仅调试模式可见
   */
  visibility?: MessageVisibility;
}

/**
 * 消息元数据
 */
export interface MessageMetadata {
  /** 任务 ID */
  taskId?: string;
  /** Mission ID */
  missionId?: string;
  /** 子任务 ID */
  subTaskId?: string;
  /** Assignment ID */
  assignmentId?: string;
  /** Todo ID */
  todoId?: string;
  /** 进度百分比 */
  percentage?: number;
  /** 修改的文件 */
  modifiedFiles?: string[];
  /** 新建的文件 */
  createdFiles?: string[];
  /** 阶段 */
  phase?: string;
  /** 持续时间（毫秒） */
  duration?: number;
  /** 错误信息 */
  error?: string;
  /** 是否可恢复（用于错误/切换判断） */
  recoverable?: boolean;
  /** Worker 询问唯一 ID */
  questionId?: string;
  /** Worker 询问匹配模式 */
  questionPattern?: string;
  /** Worker 询问时间戳 */
  questionTimestamp?: number;
  /** 适配器角色 */
  adapterRole?: 'worker' | 'orchestrator';
  /** 🔧 标记为状态消息（区别于 LLM 对话响应，不参与内容去重） */
  isStatusMessage?: boolean;
  /** 任务分配的目标 Worker（编排者消息） */
  assignedWorker?: string;
  /** 目标 Worker（用于路由到对应 Tab） */
  worker?: string;
  /** 是否派发给 Worker 的指令消息 */
  dispatchToWorker?: boolean;
  /** 子任务数据（主对话区 TASK_CARD 消息携带的完整数据） */
  subTaskCard?: unknown;
  /** 扩展数据 */
  extra?: Record<string, unknown>;
  /** 意图类型（ask/task 等） */
  intent?: string;
  /** 决策方式（llm/rule 等） */
  decision?: string;
  /** 是否为强制补发内容 */
  forced?: boolean;
  /** 触发原因（用于诊断） */
  reason?: string;
  /** 请求 ID（用于占位/响应绑定） */
  requestId?: string;
  /** 卡片实体 ID（流式更新必须绑定同一 cardId） */
  cardId?: string;
  /** 父卡片 ID（补遗卡片或衍生卡片回溯来源） */
  parentCardId?: string;
  /** 单卡片内流式序号（严格递增） */
  cardStreamSeq?: number;
  /** 卡片封口时的最终流式序号 */
  finalStreamSeq?: number;
  /** 是否为晚到流式补遗 */
  lateArrival?: boolean;
  /** 晚到流式来源的原 cardId */
  lateFromCardId?: string;
  /** 会话 ID（用于跨会话标记） */
  sessionId?: string;
  /** 消息角色（user/assistant/system） */
  role?: 'user' | 'assistant' | 'system';
  /** 是否为占位消息 */
  isPlaceholder?: boolean;
  /** 占位消息状态 */
  placeholderState?: 'pending' | 'received' | 'thinking' | 'connecting';
  /** 用户消息 ID（占位关联） */
  userMessageId?: string;
  /** 占位消息 ID（占位关联） */
  placeholderMessageId?: string;
  /** 发送动画标记 */
  sendingAnimation?: boolean;
  /** 曾为占位消息（用于过渡动画） */
  wasPlaceholder?: boolean;
  /** 用户上传的图片（base64 Data URL 格式） */
  images?: Array<{ dataUrl: string }>;
  /** P0-3: 是否为补充指令（执行中发送的追加消息） */
  isSupplementary?: boolean;
}

// ============================================================================
// 流式更新类型
// ============================================================================

/**
 * 流式更新事件
 * 用于增量更新消息内容，避免全量替换
 */
export interface StreamUpdate {
  /** 消息 ID */
  messageId: string;

  /** 事件唯一标识（用于事件流排障） */
  eventId?: string;

  /** 事件顺序号（会话内单调递增） */
  eventSeq?: number;

  /** 卡片实体 ID（默认回退为 messageId） */
  cardId?: string;

  /** 单卡片内流式序号（严格递增） */
  cardStreamSeq?: number;

  /** 更新类型 */
  updateType: 'append' | 'replace' | 'block_update' | 'lifecycle_change';

  /** 追加的文本（updateType='append' 时） */
  appendText?: string;

  /** 替换的内容块（updateType='replace' 或 'block_update' 时） */
  blocks?: ContentBlock[];

  /** 新的生命周期状态（updateType='lifecycle_change' 时） */
  lifecycle?: MessageLifecycle;

  /** Token 使用统计（实时更新） */
  tokenUsage?: {
    inputTokens?: number;
    outputTokens?: number;
    cacheReadTokens?: number;
    cacheWriteTokens?: number;
  };

  /** 时间戳 */
  timestamp: number;
}

// ============================================================================
// 工厂函数
// ============================================================================

let messageIdCounter = 0;

/**
 * 生成消息 ID
 */
export function generateMessageId(): string {
  return `msg-${Date.now()}-${++messageIdCounter}`;
}

/**
 * 创建标准消息
 */
export function createStandardMessage(
  params: Omit<StandardMessage, 'id' | 'timestamp' | 'updatedAt'> & { id?: string }
): StandardMessage {
  const now = Date.now();
  const { id, ...rest } = params;
  return {
    id: id || generateMessageId(),
    timestamp: now,
    updatedAt: now,
    visibility: 'user',  // 默认用户可见
    ...rest,
  };
}

/**
 * 创建文本消息
 */
export function createTextMessage(
  text: string,
  source: MessageSource,
  agent: AgentType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source,
    agent,
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: text, isMarkdown: true }],
    metadata: {},
    ...options,
  });
}

/**
 * 创建用户输入消息
 * 使用 MessageType.USER_INPUT 类型，无需 metadata.role 标识
 */
export function createUserInputMessage(
  text: string,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.USER_INPUT,
    source: 'orchestrator',  // 用户消息通过编排者中转
    agent: 'orchestrator',
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: text, isMarkdown: true }],
    metadata: {},
    ...options,
  });
}

/**
 * 创建流式消息（初始状态）
 */
export function createStreamingMessage(
  source: MessageSource,
  agent: AgentType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source,
    agent,
    traceId,
    lifecycle: MessageLifecycle.STARTED,
    blocks: [],
    metadata: {},
    visibility: 'user',  // 默认用户可见
    ...options,
  });
}

/**
 * 创建错误消息
 */
export function createErrorMessage(
  error: string,
  source: MessageSource,
  agent: AgentType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.ERROR,
    source,
    agent,
    traceId,
    lifecycle: MessageLifecycle.FAILED,
    blocks: [{ type: 'text', content: error }],
    metadata: { error },
    ...options,
  });
}

/**
 * 创建交互请求消息
 */
export function createInteractionMessage(
  interaction: InteractionRequest,
  source: MessageSource,
  agent: AgentType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.INTERACTION,
    source,
    agent,
    traceId,
    lifecycle: MessageLifecycle.STREAMING, // 等待用户响应
    blocks: [{ type: 'text', content: interaction.prompt }],
    interaction,
    metadata: {},
    ...options,
  });
}

// ============================================================================
// 统一消息通道工厂函数（unified-message-channel-design.md v2.5）
// ============================================================================

/**
 * 创建控制消息
 */
export function createControlMessage(
  controlType: ControlMessageType,
  payload: Record<string, unknown>,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTROL,
    type: MessageType.SYSTEM,
    source: 'orchestrator',
    agent: 'orchestrator',
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [],
    metadata: {},
    control: { controlType, payload },
    ...options,
  });
}

/**
 * 创建通知消息（Toast）
 */
export function createNotifyMessage(
  content: string,
  level: NotifyLevel,
  traceId: string,
  duration?: number,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.NOTIFY,
    type: MessageType.SYSTEM,
    source: 'orchestrator',
    agent: 'orchestrator',
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content }],
    metadata: {},
    notify: { level, duration },
    ...options,
  });
}

/**
 * 创建数据消息
 */
export function createDataMessage(
  dataType: DataMessageType,
  payload: Record<string, unknown>,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.DATA,
    type: MessageType.SYSTEM,
    source: 'orchestrator',
    agent: 'orchestrator',
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [],
    metadata: {},
    data: { dataType, payload },
    ...options,
  });
}
