/**
 * 统一消息协议 (Unified Message Protocol)
 *
 * 核心设计理念：
 * 1. 所有 CLI 输出在适配层完成标准化，Webview 只负责渲染
 * 2. 明确的消息类型枚举，消除格式猜测
 * 3. 统一的生命周期状态机，消除状态混乱
 * 4. 可扩展的内容结构，支持各种富文本元素
 */

import type { CLIType } from '../cli/types';

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
  SYSTEM = 'system',
  /** 工具调用 */
  TOOL_CALL = 'tool_call',
  /** 思考过程 */
  THINKING = 'thinking',
}

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
  /** 输入参数 */
  input?: Record<string, unknown>;
  /** 输出结果 */
  output?: string;
  /** 错误信息 */
  error?: string;
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
 * 内容块联合类型
 */
export type ContentBlock =
  | TextBlock
  | CodeBlock
  | ThinkingBlock
  | ToolCallBlock
  | FileChangeBlock;

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
 * 标准消息 - 统一消息协议的核心
 *
 * 所有 CLI 输出经过 Normalizer 转换后都变成这个格式
 * Webview 只需要根据这个接口渲染，无需任何解析逻辑
 */
export interface StandardMessage {
  /** 消息唯一标识 */
  id: string;

  /** 追踪 ID（用于关联同一任务的多条消息） */
  traceId: string;

  /** 消息类型 */
  type: MessageType;

  /** 消息来源 */
  source: MessageSource;

  /** CLI 类型 */
  cli: CLIType;

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
}

/**
 * 消息元数据
 */
export interface MessageMetadata {
  /** 任务 ID */
  taskId?: string;
  /** 子任务 ID */
  subTaskId?: string;
  /** 阶段 */
  phase?: string;
  /** 持续时间（毫秒） */
  duration?: number;
  /** 错误信息 */
  error?: string;
  /** CLI 询问唯一 ID */
  questionId?: string;
  /** CLI 询问匹配模式 */
  questionPattern?: string;
  /** CLI 询问时间戳 */
  questionTimestamp?: number;
  /** 适配器角色 */
  adapterRole?: 'worker' | 'orchestrator';
  /** 扩展数据 */
  extra?: Record<string, unknown>;
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

  /** 更新类型 */
  updateType: 'append' | 'replace' | 'block_update' | 'lifecycle_change';

  /** 追加的文本（updateType='append' 时） */
  appendText?: string;

  /** 替换的内容块（updateType='replace' 或 'block_update' 时） */
  blocks?: ContentBlock[];

  /** 新的生命周期状态（updateType='lifecycle_change' 时） */
  lifecycle?: MessageLifecycle;

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
  return {
    id: params.id || generateMessageId(),
    timestamp: now,
    updatedAt: now,
    ...params,
  };
}

/**
 * 创建文本消息
 */
export function createTextMessage(
  text: string,
  source: MessageSource,
  cli: CLIType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    type: MessageType.TEXT,
    source,
    cli,
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
  cli: CLIType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    type: MessageType.TEXT,
    source,
    cli,
    traceId,
    lifecycle: MessageLifecycle.STARTED,
    blocks: [],
    metadata: {},
    ...options,
  });
}

/**
 * 创建错误消息
 */
export function createErrorMessage(
  error: string,
  source: MessageSource,
  cli: CLIType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    type: MessageType.ERROR,
    source,
    cli,
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
  cli: CLIType,
  traceId: string,
  options?: Partial<StandardMessage>
): StandardMessage {
  return createStandardMessage({
    type: MessageType.INTERACTION,
    source,
    cli,
    traceId,
    lifecycle: MessageLifecycle.STREAMING, // 等待用户响应
    blocks: [{ type: 'text', content: interaction.prompt }],
    interaction,
    metadata: {},
    ...options,
  });
}
