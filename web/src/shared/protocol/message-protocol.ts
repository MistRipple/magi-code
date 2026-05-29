/**
 * 统一消息协议 (Unified Message Protocol)
 *
 * 从 magi 原始 src/protocol/message-protocol.ts 完整拷贝。
 * 前端渲染、Bridge、Store 等模块均依赖此文件的类型定义。
 */

import type { AnyAgentId } from '../types/agent-types';

// ============================================================================
// 消息类型枚举
// ============================================================================

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
  /** 处理思路 */
  THINKING = 'thinking',

  // ============== 新增消息类型（方案 B 扩展）==============
  /** 用户输入消息 */
  USER_INPUT = 'user_input',
  /** 任务状态卡片（代理执行状态摘要，主对话区展示） */
  TASK_CARD = 'task_card',
  /** 任务说明（编排者派发给代理的详细任务描述） */
  INSTRUCTION = 'instruction',
}

// ============================================================================
// 统一消息分类
// ============================================================================

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

export enum ControlMessageType {
  PHASE_CHANGED = 'phase_changed',
  TASK_ACCEPTED = 'task_accepted',
  TASK_REJECTED = 'task_rejected',
  TASK_STARTED = 'task_started',
  TASK_COMPLETED = 'task_completed',
  TASK_FAILED = 'task_failed',
  WORKER_STATUS = 'worker_status',
}

export type NotifyLevel = 'info' | 'success' | 'warning' | 'error';
export type NotifyDisplayMode = 'auto' | 'toast' | 'notification_center' | 'silent';
export type NotifyCategory = 'incident' | 'audit' | 'feedback';

/**
 * 数据消息类型
 */
export type DataMessageType =
  | 'clarificationRequest'
  | 'auxiliaryConnectionTestResult'
  | 'customToolAdded'
  | 'customToolRemoved'
  | 'executionTokenRuntime'
  | 'executionStatsUpdate'
  | 'emptyWorkspaceStateLoaded'
  | 'instructionSkillRemoved'
  | 'lanAccessInfo'
  | 'tunnelState'
  | 'llmRetryRuntime'
  | 'mcpServerAdded'
  | 'mcpServerDeleted'
  | 'mcpServerTools'
  | 'mcpServerUpdated'
  | 'mcpToolsRefreshed'
  | 'missionExecutionFailed'
  | 'modelListFetched'
  | 'orchestratorConnectionTestResult'
  | 'orchestratorRuntimeState'
  | 'projectKnowledgeLoaded'
  | 'processingStateChanged'
  | 'registryAgentsLoaded'
  | 'recoveryRequest'
  | 'repositoryAdded'
  | 'repositoryAddFailed'
  | 'repositoryDeleted'
  | 'repositoryRefreshed'
  | 'settingsBootstrapLoaded'
  | 'sessionBootstrapLoaded'
  | 'sessionTurnAccepted'
  | 'sessionTurnCanonicalEventUpdated'
  | 'sessionNotificationsLoaded'
  | 'sessionNotificationsStatus'
  | 'sessionsUpdated'
  | 'skillInstalled'
  | 'skillInstallFailed'
  | 'skillUpdated'
  | 'allSkillsUpdated'
  | 'skillLibraryLoaded'
  | 'stateUpdate'
  | 'taskStatusChanged'
  | 'workerConnectionTestResult'
  | 'workerSessionCreated'
  | 'workerSessionResumed'
  | 'workspaceSessionCleared'
  | 'executionChainInterrupted'
  | 'messageCreated';

/**
 * 消息生命周期状态
 */
export enum MessageLifecycle {
  STARTED = 'started',
  STREAMING = 'streaming',
  COMPLETED = 'completed',
  FAILED = 'failed',
  CANCELLED = 'cancelled',
}

export type MessageSource = 'orchestrator' | 'worker';
export type MessageVisibility = 'user' | 'system' | 'debug';

// ============================================================================
// 内容块类型
// ============================================================================

export interface TextBlock {
  type: 'text';
  blockId: string;
  content: string;
  isMarkdown?: boolean;
}

export interface CodeBlock {
  type: 'code';
  blockId: string;
  language: string;
  content: string;
  filename?: string;
  highlightLines?: number[];
  isEmbedded?: boolean;
}

export interface ThinkingBlock {
  type: 'thinking';
  blockId: string;
  content: string;
  isComplete?: boolean;
  summary?: string;
}

export type StandardizedToolStatus =
  | 'success'
  | 'error'
  | 'timeout'
  | 'killed'
  | 'blocked'
  | 'rejected'
  | 'aborted';

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

export interface ToolCallBlock {
  type: 'tool_call';
  toolName: string;
  toolId: string;
  status: 'pending' | 'running' | 'completed' | 'failed';
  input?: string;
  output?: string;
  error?: string;
  standardized?: StandardizedToolResultPayload;
  recoverable?: boolean;
  duration?: number;
}

interface FileChangePayload {
  filePath: string;
  oldPath?: string;
  changeType: 'create' | 'modify' | 'delete' | 'rename';
  additions?: number;
  deletions?: number;
  diff?: string;
  contentKind?: 'text' | 'large_text' | 'binary' | 'symlink' | 'special';
  size?: number;
  mime?: string;
  error?: string;
  symlinkTarget?: string;
  headSummary?: string;
  tailSummary?: string;
  toolCallId?: string;
}

export interface ToolResultBlock {
  type: 'tool_result';
  toolCallId: string;
  content: string;
  input?: string;
  isError?: boolean;
  standardized?: StandardizedToolResultPayload;
  fileChange?: FileChangePayload;
}

export interface FileChangeBlock extends FileChangePayload {
  type: 'file_change';
}

export interface PlanBlock {
  type: 'plan';
  blockId: string;
  goal: string;
  analysis?: string;
  constraints?: string[];
  acceptanceCriteria?: string[];
  riskLevel?: 'low' | 'medium' | 'high';
  riskFactors?: string[];
  rawJson?: string;
}

export type ContentBlock =
  | TextBlock
  | CodeBlock
  | ThinkingBlock
  | ToolCallBlock
  | ToolResultBlock
  | FileChangeBlock
  | PlanBlock;

// ============================================================================
// 交互请求类型
// ============================================================================

export enum InteractionType {
  PLAN_CONFIRMATION = 'plan_confirmation',
  QUESTION = 'question',
  PERMISSION = 'permission',
  CLARIFICATION = 'clarification',
}

export interface InteractionRequest {
  type: InteractionType;
  requestId: string;
  prompt: string;
  options?: Array<{
    value: string;
    label: string;
    isDefault?: boolean;
  }>;
  required: boolean;
  timeout?: number;
}

// ============================================================================
// 标准消息接口
// ============================================================================

export interface ControlPayload {
  controlType: ControlMessageType;
  payload: Record<string, unknown>;
}

export interface NotifyPresentation {
  displayMode?: NotifyDisplayMode;
  category?: NotifyCategory;
  source?: string;
  actionRequired?: boolean;
  persistToCenter?: boolean;
  countUnread?: boolean;
  title?: string;
}

export interface NotifyPayload extends NotifyPresentation {
  level: NotifyLevel;
  duration?: number;
}

export interface DataPayload {
  dataType: DataMessageType;
  payload: Record<string, unknown>;
}

export interface MessageMetadata {
  taskId?: string;
  executionGroupId?: string;
  subTaskId?: string;
  assignmentId?: string;
  percentage?: number;
  modifiedFiles?: string[];
  createdFiles?: string[];
  phase?: string;
  duration?: number;
  responseDurationMs?: number;
  error?: string;
  recoverable?: boolean;
  questionId?: string;
  questionPattern?: string;
  questionTimestamp?: number;
  adapterRole?: 'worker' | 'orchestrator';
  isStatusMessage?: boolean;
  assignedWorker?: string;
  worker?: string;
  workerId?: string;
  roleId?: string;
  dispatchToWorker?: boolean;
  laneIndex?: number;
  laneTotal?: number;
  laneTaskIds?: string[];
  laneCurrentTaskId?: string;
  laneTasks?: Array<{
    taskId: string;
    title: string;
    status: 'pending' | 'waiting_deps' | 'running' | 'review_required' | 'completed' | 'failed' | 'skipped' | 'cancelled';
    dependsOn?: string[];
    isCurrent?: boolean;
  }>;
  laneTaskCards?: Array<{
    taskId: string;
    title: string;
    worker?: string;
    status: 'pending' | 'waiting_deps' | 'running' | 'review_required' | 'completed' | 'failed' | 'skipped' | 'cancelled';
    summary?: string;
    fullSummary?: string;
    error?: string;
    reviewStatus?: 'approved' | 'needs_revision' | 'rejected';
    reviewFeedback?: string;
    failureCode?: string;
    recoverable?: boolean;
    modifiedFiles?: string[];
    createdFiles?: string[];
    duration?: number;
  }>;
  subTaskCard?: unknown;
  extra?: Record<string, unknown>;
  intent?: string;
  decision?: string;
  forced?: boolean;
  reason?: string;
  requestId?: string;
  timelineAnchorTimestamp?: number;
  turnId?: string;
  turnOrderSeq?: number;
  turnSeq?: number;
  turnItemId?: string;
  turnItemKind?: string;
  itemSeq?: number;
  rustStreamItemId?: string;
  rustEventItemId?: string;
  eventId?: string;
  eventSeq?: number;
  cardId?: string;
  parentCardId?: string;
  blockSeq?: number;
  cardStreamSeq?: number;
  finalStreamSeq?: number;
  lateArrival?: boolean;
  lateFromCardId?: string;
  sessionId?: string;
  role?: 'user' | 'assistant' | 'system';
  isPlaceholder?: boolean;
  placeholderState?: 'pending' | 'received' | 'thinking' | 'connecting';
  userMessageId?: string;
  placeholderMessageId?: string;
  sendingAnimation?: boolean;
  wasPlaceholder?: boolean;
  images?: Array<{ dataUrl: string }>;
  isSupplementary?: boolean;
  toolCallId?: string;
  toolName?: string;
}

export interface StandardMessage {
  id: string;
  eventId?: string;
  eventSeq?: number;
  traceId: string;
  category: MessageCategory;
  type: MessageType;
  source: MessageSource;
  agent: AnyAgentId;
  lifecycle: MessageLifecycle;
  blocks: ContentBlock[];
  interaction?: InteractionRequest;
  metadata: MessageMetadata;
  timestamp: number;
  updatedAt: number;
  control?: ControlPayload;
  notify?: NotifyPayload;
  data?: DataPayload;
  visibility?: MessageVisibility;
}

// ============================================================================
// 工厂函数
// ============================================================================

let messageIdCounter = 0;

export function generateMessageId(): string {
  return `msg-${Date.now()}-${++messageIdCounter}`;
}

export function createStandardMessage(
  params: Omit<StandardMessage, 'id' | 'timestamp' | 'updatedAt'> & { id?: string }
): StandardMessage {
  const now = Date.now();
  const { id, ...rest } = params;
  return {
    id: id || generateMessageId(),
    timestamp: now,
    updatedAt: now,
    visibility: 'user',
    ...rest,
  };
}

export function createTextMessage(
  text: string,
  source: MessageSource,
  agent: AnyAgentId,
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
    blocks: [{ type: 'text', blockId: `${traceId}:text`, content: text, isMarkdown: true }],
    metadata: {},
    ...options,
  });
}

export function createUserInputMessage(
  text: string,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  const message = createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.USER_INPUT,
    source: 'orchestrator',
    agent: 'orchestrator',
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', blockId: `${traceId}:text`, content: text, isMarkdown: true }],
    metadata: {},
    ...options,
  });
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : {};
  const existingAnchorTimestamp = typeof metadata.timelineAnchorTimestamp === 'number'
    && Number.isFinite(metadata.timelineAnchorTimestamp)
    && metadata.timelineAnchorTimestamp > 0
    ? Math.floor(metadata.timelineAnchorTimestamp)
    : 0;
  if (existingAnchorTimestamp > 0) {
    return message;
  }
  return {
    ...message,
    metadata: {
      ...metadata,
      timelineAnchorTimestamp: message.timestamp,
    },
  };
}

export function createStreamingMessage(
  source: MessageSource,
  agent: AnyAgentId,
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
    visibility: 'user',
    ...options,
  });
}

export function createErrorMessage(
  error: string,
  source: MessageSource,
  agent: AnyAgentId,
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
    blocks: [{ type: 'text', blockId: `${traceId}:error`, content: error }],
    metadata: { error },
    ...options,
  });
}

export function createInteractionMessage(
  interaction: InteractionRequest,
  source: MessageSource,
  agent: AnyAgentId,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.CONTENT,
    type: MessageType.INTERACTION,
    source,
    agent,
    traceId,
    lifecycle: MessageLifecycle.STREAMING,
    blocks: [{ type: 'text', blockId: `${traceId}:interaction`, content: interaction.prompt }],
    interaction,
    metadata: {},
    ...options,
  });
}

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

export function createNotifyMessage(
  content: string,
  level: NotifyLevel,
  traceId: string,
  duration?: number,
  presentation?: NotifyPresentation,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    category: MessageCategory.NOTIFY,
    type: MessageType.SYSTEM,
    source: 'orchestrator',
    agent: 'orchestrator',
    traceId,
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', blockId: `${traceId}:notify`, content }],
    metadata: {},
    notify: { level, duration, ...(presentation || {}) },
    ...options,
  });
}

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
