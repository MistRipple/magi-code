import type {
  ContentBlock,
  Message,
  MessageRole,
  MessageSource,
  MessageType,
  NoticeType,
} from '../types/message';

const MESSAGE_ROLE_SET = new Set<MessageRole>(['user', 'assistant', 'system']);
const MESSAGE_SOURCE_SET = new Set<MessageSource>(['orchestrator', 'claude', 'codex', 'gemini', 'system']);
const MESSAGE_TYPE_SET = new Set<MessageType>([
  'text',
  'plan',
  'progress',
  'result',
  'error',
  'interaction',
  'system-notice',
  'tool_call',
  'thinking',
  'user_input',
  'task_card',
  'instruction',
]);
const NOTICE_TYPE_SET = new Set<NoticeType>(['info', 'success', 'warning', 'error']);
const CONTENT_BLOCK_TYPE_SET = new Set<ContentBlock['type']>([
  'text',
  'code',
  'thinking',
  'tool_call',
  'tool_result',
  'file_change',
  'plan',
]);

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return false;
  }
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function sanitizeSerializableValue(
  value: unknown,
  options: { preserveUndefined?: boolean } = {},
  seen: WeakSet<object> = new WeakSet<object>(),
): unknown {
  if (value === undefined) {
    return undefined;
  }
  if (value === null || typeof value === 'string' || typeof value === 'boolean') {
    return value;
  }
  if (typeof value === 'number') {
    return Number.isFinite(value) ? value : undefined;
  }
  if (typeof value === 'bigint') {
    return value.toString();
  }
  if (typeof value === 'function' || typeof value === 'symbol') {
    return undefined;
  }
  if (Array.isArray(value)) {
    const result: unknown[] = [];
    for (const item of value) {
      const sanitized = sanitizeSerializableValue(item, options, seen);
      if (sanitized !== undefined) {
        result.push(sanitized);
      }
    }
    return result;
  }
  if (value instanceof Date) {
    return Number.isFinite(value.getTime()) ? value.toISOString() : undefined;
  }
  if (value instanceof Error) {
    const normalized: Record<string, unknown> = {
      name: value.name,
      message: value.message,
    };
    if (typeof value.stack === 'string' && value.stack.trim()) {
      normalized.stack = value.stack;
    }
    const cause = sanitizeSerializableValue((value as Error & { cause?: unknown }).cause, options, seen);
    if (cause !== undefined) {
      normalized.cause = cause;
    }
    return normalized;
  }
  if (!isPlainRecord(value)) {
    return undefined;
  }
  if (seen.has(value)) {
    return undefined;
  }
  seen.add(value);
  const result: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (entry === undefined && options.preserveUndefined) {
      result[key] = undefined;
      continue;
    }
    const sanitized = sanitizeSerializableValue(entry, options, seen);
    if (sanitized !== undefined) {
      result[key] = sanitized;
    }
  }
  seen.delete(value);
  return result;
}

function sanitizeMessageMetadata(
  metadata: unknown,
  errorPrefix: string,
  options: { preserveUndefined?: boolean } = {},
): Message['metadata'] | undefined {
  if (metadata === undefined) {
    return undefined;
  }
  const sanitized = sanitizeSerializableValue(metadata, options);
  if (sanitized === undefined) {
    return undefined;
  }
  if (!isPlainRecord(sanitized)) {
    throw new Error(`${errorPrefix} metadata 无效`);
  }
  return sanitized as Message['metadata'];
}

function sanitizeMessageImages(
  images: unknown,
  errorPrefix: string,
): Array<{ dataUrl: string }> | undefined {
  if (images === undefined) {
    return undefined;
  }
  if (!Array.isArray(images)) {
    throw new Error(`${errorPrefix} images 无效`);
  }
  const normalized = images
    .filter((item): item is Record<string, unknown> => isPlainRecord(item))
    .map((item) => {
      const dataUrl = typeof item.dataUrl === 'string' ? item.dataUrl : '';
      if (!dataUrl) {
        throw new Error(`${errorPrefix} images.dataUrl 无效`);
      }
      return { dataUrl };
    });
  return normalized.length > 0 ? normalized : undefined;
}

function resolveMessageType(message: Pick<Message, 'role' | 'type'>, errorPrefix: string): MessageType {
  if (typeof message.type === 'string' && MESSAGE_TYPE_SET.has(message.type as MessageType)) {
    return message.type as MessageType;
  }
  if (message.role === 'user') {
    return 'user_input';
  }
  if (message.role === 'system') {
    return 'system-notice';
  }
  if (message.role === 'assistant') {
    return 'text';
  }
  throw new Error(`${errorPrefix} type 无效`);
}

export function sanitizeMessageBlocks(blocks: unknown, errorPrefix = '[MessagePayload]'): ContentBlock[] {
  if (blocks === undefined) {
    return [];
  }
  if (!Array.isArray(blocks)) {
    throw new Error(`${errorPrefix} blocks 无效`);
  }
  return blocks.map((block) => {
    if (!isPlainRecord(block)) {
      throw new Error(`${errorPrefix} 消息块无效`);
    }
    const sanitized = sanitizeSerializableValue(block);
    if (!isPlainRecord(sanitized)) {
      throw new Error(`${errorPrefix} 消息块无效`);
    }
    const type = typeof sanitized.type === 'string' ? sanitized.type : '';
    if (!CONTENT_BLOCK_TYPE_SET.has(type as ContentBlock['type'])) {
      throw new Error(`${errorPrefix} 消息块 type 无效`);
    }
    const content = typeof sanitized.content === 'string' ? sanitized.content : '';

    // 后端协议层使用扁平的 toolName/toolId/input/output 属性，
    // 前端渲染层期望嵌套的 toolCall 对象。投影恢复时数据未经 mapStandardBlocks 转换，
    // 这里自动适配：如果缺少 toolCall 但有扁平属性，则构造嵌套对象。
    if (type === 'tool_call' && !sanitized.toolCall && typeof sanitized.toolName === 'string') {
      const toolCall = adaptFlatToolCallBlock(sanitized);
      return {
        type: 'tool_call' as const,
        content: '',
        toolCall,
      } as ContentBlock;
    }

    // 后端 thinking block 使用扁平 content/summary/blockId，前端期望嵌套 thinking 对象。
    if (type === 'thinking' && !sanitized.thinking) {
      const blockId = typeof sanitized.blockId === 'string' ? sanitized.blockId : undefined;
      return {
        ...(blockId ? { id: blockId } : {}),
        type: 'thinking' as const,
        content,
        thinking: {
          content,
          isComplete: true,
          summary: typeof sanitized.summary === 'string' ? sanitized.summary : undefined,
          blockId,
        },
      } as ContentBlock;
    }

    return {
      ...sanitized,
      type: type as ContentBlock['type'],
      content,
    } as ContentBlock;
  });
}

/**
 * 将后端协议层的扁平 ToolCallBlock 属性适配为前端 ToolCall 嵌套对象。
 * 与 message-utils.ts 中 mapStandardBlocks 的 tool_call 分支逻辑保持一致。
 */
function adaptFlatToolCallBlock(block: Record<string, unknown>): import('../types/message').ToolCall {
  const status = typeof block.status === 'string' ? block.status : '';
  const standardized = block.standardized as Record<string, unknown> | undefined;
  const standardizedStatus = typeof standardized?.status === 'string'
    ? standardized.status.toLowerCase() : '';
  const error = typeof block.error === 'string' ? block.error : '';
  const output = typeof block.output === 'string' ? block.output : '';

  let resolvedStatus: import('../types/message').ToolCallStatus;
  switch (status) {
    case 'pending': resolvedStatus = 'pending'; break;
    case 'running': resolvedStatus = 'running'; break;
    case 'success': case 'completed': resolvedStatus = 'success'; break;
    case 'error': case 'failed': resolvedStatus = 'error'; break;
    default:
      if (standardizedStatus === 'success') resolvedStatus = 'success';
      else if (['error', 'timeout', 'killed'].includes(standardizedStatus)) resolvedStatus = 'error';
      else if (['blocked', 'rejected', 'aborted'].includes(standardizedStatus)) resolvedStatus = 'success';
      else if (error) resolvedStatus = 'error';
      else if (output) resolvedStatus = 'success';
      else resolvedStatus = 'running';
  }

  const standardizedHardError = ['error', 'timeout', 'killed'].includes(standardizedStatus);
  const standardizedError = standardized && standardizedHardError
    ? (typeof standardized.message === 'string' ? standardized.message : undefined)
    : undefined;

  let parsedArgs: Record<string, unknown> = {};
  if (typeof block.input === 'string' && block.input.trim()) {
    try { parsedArgs = JSON.parse(block.input); } catch { /* ignore */ }
  }

  return {
    id: typeof block.toolId === 'string' ? block.toolId : '',
    name: typeof block.toolName === 'string' ? block.toolName : 'Tool',
    arguments: parsedArgs,
    status: resolvedStatus,
    result: output || undefined,
    error: error || standardizedError || undefined,
    standardized: standardized as import('../types/message').StandardizedToolResult | undefined,
  };
}

export function normalizeMessagePayload(message: Message, errorPrefix = '[MessagePayload]'): Message {
  if (!message || typeof message !== 'object') {
    throw new Error(`${errorPrefix} 消息无效`);
  }
  const id = typeof message.id === 'string' && message.id.trim().length > 0 ? message.id.trim() : '';
  if (!id) {
    throw new Error(`${errorPrefix} 缺少 id`);
  }
  const role = typeof message.role === 'string' && MESSAGE_ROLE_SET.has(message.role as MessageRole)
    ? message.role as MessageRole
    : null;
  if (!role) {
    throw new Error(`${errorPrefix} role 无效`);
  }
  const source = typeof message.source === 'string' && MESSAGE_SOURCE_SET.has(message.source as MessageSource)
    ? message.source as MessageSource
    : null;
  if (!source) {
    throw new Error(`${errorPrefix} source 无效`);
  }
  if (typeof message.content !== 'string') {
    throw new Error(`${errorPrefix} content 非字符串`);
  }
  if (typeof message.timestamp !== 'number' || !Number.isFinite(message.timestamp)) {
    throw new Error(`${errorPrefix} timestamp 无效`);
  }
  const updatedAt = typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)
    ? Math.floor(message.updatedAt)
    : Math.floor(message.timestamp);

  const blocks = sanitizeMessageBlocks(message.blocks, errorPrefix);
  const metadata = sanitizeMessageMetadata(message.metadata, errorPrefix);
  const images = sanitizeMessageImages(message.images, errorPrefix);
  const type = resolveMessageType({ role, type: message.type }, errorPrefix);
  const noticeType = typeof message.noticeType === 'string' && NOTICE_TYPE_SET.has(message.noticeType as NoticeType)
    ? message.noticeType as NoticeType
    : undefined;

  return {
    id,
    role,
    source,
    content: message.content,
    ...(blocks.length > 0 ? { blocks } : {}),
    timestamp: Math.floor(message.timestamp),
    updatedAt,
    isStreaming: typeof message.isStreaming === 'boolean' ? message.isStreaming : false,
    isComplete: typeof message.isComplete === 'boolean' ? message.isComplete : !Boolean(message.isStreaming),
    type,
    ...(noticeType ? { noticeType } : {}),
    ...(images ? { images } : {}),
    ...(metadata ? { metadata } : {}),
  };
}

export function cloneMessagePayload(message: Message): Message {
  return normalizeMessagePayload(message, '[MessagePayload] 克隆消息');
}

export function sanitizeMessagePatch(
  updates: Partial<Message>,
  errorPrefix = '[MessagePayload] 补丁',
): Partial<Message> {
  if (!updates || typeof updates !== 'object') {
    throw new Error(`${errorPrefix} 无效`);
  }

  const normalized: Partial<Message> = {};

  if ('id' in updates) {
    if (updates.id === undefined) {
      normalized.id = updates.id;
    } else if (typeof updates.id === 'string' && updates.id.trim()) {
      normalized.id = updates.id.trim();
    } else {
      throw new Error(`${errorPrefix} id 无效`);
    }
  }

  if ('role' in updates) {
    if (updates.role === undefined) {
      normalized.role = updates.role;
    } else if (MESSAGE_ROLE_SET.has(updates.role as MessageRole)) {
      normalized.role = updates.role;
    } else {
      throw new Error(`${errorPrefix} role 无效`);
    }
  }

  if ('source' in updates) {
    if (updates.source === undefined) {
      normalized.source = updates.source;
    } else if (MESSAGE_SOURCE_SET.has(updates.source as MessageSource)) {
      normalized.source = updates.source;
    } else {
      throw new Error(`${errorPrefix} source 无效`);
    }
  }

  if ('content' in updates) {
    if (updates.content === undefined || typeof updates.content === 'string') {
      normalized.content = updates.content;
    } else {
      throw new Error(`${errorPrefix} content 非字符串`);
    }
  }

  if ('blocks' in updates) {
    normalized.blocks = updates.blocks === undefined
      ? undefined
      : sanitizeMessageBlocks(updates.blocks, errorPrefix);
  }

  if ('timestamp' in updates) {
    if (updates.timestamp === undefined) {
      normalized.timestamp = updates.timestamp;
    } else if (typeof updates.timestamp === 'number' && Number.isFinite(updates.timestamp)) {
      normalized.timestamp = Math.floor(updates.timestamp);
    } else {
      throw new Error(`${errorPrefix} timestamp 无效`);
    }
  }

  if ('updatedAt' in updates) {
    if (updates.updatedAt === undefined) {
      normalized.updatedAt = updates.updatedAt;
    } else if (typeof updates.updatedAt === 'number' && Number.isFinite(updates.updatedAt)) {
      normalized.updatedAt = Math.floor(updates.updatedAt);
    } else {
      throw new Error(`${errorPrefix} updatedAt 无效`);
    }
  }

  if ('isStreaming' in updates) {
    if (updates.isStreaming === undefined || typeof updates.isStreaming === 'boolean') {
      normalized.isStreaming = updates.isStreaming;
    } else {
      throw new Error(`${errorPrefix} isStreaming 无效`);
    }
  }

  if ('isComplete' in updates) {
    if (updates.isComplete === undefined || typeof updates.isComplete === 'boolean') {
      normalized.isComplete = updates.isComplete;
    } else {
      throw new Error(`${errorPrefix} isComplete 无效`);
    }
  }

  if ('type' in updates) {
    if (updates.type === undefined || MESSAGE_TYPE_SET.has(updates.type as MessageType)) {
      normalized.type = updates.type;
    } else {
      throw new Error(`${errorPrefix} type 无效`);
    }
  }

  if ('noticeType' in updates) {
    if (updates.noticeType === undefined || NOTICE_TYPE_SET.has(updates.noticeType as NoticeType)) {
      normalized.noticeType = updates.noticeType;
    } else {
      throw new Error(`${errorPrefix} noticeType 无效`);
    }
  }

  if ('images' in updates) {
    normalized.images = updates.images === undefined
      ? undefined
      : sanitizeMessageImages(updates.images, errorPrefix);
  }

  if ('metadata' in updates) {
    normalized.metadata = sanitizeMessageMetadata(updates.metadata, errorPrefix, {
      preserveUndefined: true,
    });
  }

  return normalized;
}
