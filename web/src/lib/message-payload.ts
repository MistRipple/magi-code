import type {
  ContentBlock,
  Message,
  MessageContextReference,
  MessageBrowserNodeSelection,
  BrowserNodeSelectionRect,
  MessageImage,
  MessageRole,
  MessageSource,
  MessageType,
  NoticeType,
} from '../types/message';

const MESSAGE_ROLE_SET = new Set<MessageRole>(['user', 'assistant', 'system']);
function isValidMessageSource(source: string): boolean {
  return typeof source === 'string' && source.trim().length > 0;
}
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

function parseToolInput(value: unknown): Record<string, unknown> {
  if (isPlainRecord(value)) {
    return value;
  }
  if (typeof value !== 'string' || !value.trim()) {
    return {};
  }
  try {
    const parsed = JSON.parse(value) as unknown;
    return isPlainRecord(parsed) ? parsed : { raw: parsed };
  } catch {
    return { raw: value };
  }
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
): MessageImage[] | undefined {
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
      const name = typeof item.name === 'string' ? item.name.trim() : '';
      return {
        ...(name ? { name } : {}),
        dataUrl,
      };
    });
  return normalized.length > 0 ? normalized : undefined;
}

function sanitizeMessageContextReferences(
  references: unknown,
  errorPrefix: string,
): MessageContextReference[] | undefined {
  if (references === undefined) {
    return undefined;
  }
  if (!Array.isArray(references)) {
    throw new Error(`${errorPrefix} contextReferences 无效`);
  }
  const normalized = references.map((reference) => {
    if (!isPlainRecord(reference)) {
      throw new Error(`${errorPrefix} contextReferences 条目无效`);
    }
    const kind: MessageContextReference['kind'] | null =
      reference.kind === 'file' || reference.kind === 'directory'
        ? reference.kind
        : null;
    const path = typeof reference.path === 'string' ? reference.path.trim() : '';
    const name = typeof reference.name === 'string' ? reference.name.trim() : '';
    if (!kind || !path || !name) {
      throw new Error(`${errorPrefix} contextReferences 字段无效`);
    }
    return { kind, path, name };
  });
  return normalized.length > 0 ? normalized : undefined;
}

function sanitizeMessageBrowserAnnotationRefs(
  references: unknown,
  errorPrefix: string,
): import('../types/message').MessageBrowserAnnotationReference[] | undefined {
  if (references === undefined) return undefined;
  if (!Array.isArray(references)) {
    throw new Error(`${errorPrefix} browserAnnotationRefs 无效`);
  }
  const normalized = references.map((reference) => {
    if (!isPlainRecord(reference)) {
      throw new Error(`${errorPrefix} browserAnnotationRefs 条目无效`);
    }
    const annotationId = typeof reference.annotationId === 'string' ? reference.annotationId.trim() : '';
    const browserSessionId = typeof reference.browserSessionId === 'string' ? reference.browserSessionId.trim() : '';
    const tabId = typeof reference.tabId === 'string' ? reference.tabId.trim() : '';
    const kind: 'element' | 'region' | null = reference.kind === 'element' || reference.kind === 'region'
      ? reference.kind
      : null;
    const comment = typeof reference.comment === 'string' ? reference.comment.trim() : '';
    const sequence = Number.isSafeInteger(reference.sequence) && Number(reference.sequence) > 0
      ? Number(reference.sequence)
      : undefined;
    if (!annotationId || !browserSessionId || !tabId || !kind || !comment) {
      throw new Error(`${errorPrefix} browserAnnotationRefs 字段无效`);
    }
    return {
      annotationId,
      browserSessionId,
      tabId,
      ...(sequence ? { sequence } : {}),
      kind,
      comment,
      screenshotArtifactId: typeof reference.screenshotArtifactId === 'string'
        ? reference.screenshotArtifactId.trim() || null
        : null,
    };
  });
  return normalized.length > 0 ? normalized : undefined;
}

const SENSITIVE_BROWSER_ATTRIBUTE_PATTERN = /(pass(word)?|secret|token|auth|cookie|session|csrf|credential|private[-_ ]?key)/iu;
const MAX_BROWSER_NODE_SELECTIONS = 20;
const MAX_BROWSER_NODE_TEXT_LENGTH = 4000;
const MAX_BROWSER_NODE_HTML_LENGTH = 12000;

function sanitizeBrowserNodeRect(value: unknown, errorPrefix: string): BrowserNodeSelectionRect | null | undefined {
  if (value === undefined) return undefined;
  if (value === null) return null;
  if (!isPlainRecord(value)) throw new Error(`${errorPrefix} browserNodeSelections.bounds 无效`);
  const values = ['x', 'y', 'width', 'height'].map((key) => value[key]);
  if (!values.every((item) => typeof item === 'number' && Number.isFinite(item))) {
    throw new Error(`${errorPrefix} browserNodeSelections.bounds 字段无效`);
  }
  return {
    x: Number(values[0]),
    y: Number(values[1]),
    width: Math.max(0, Number(values[2])),
    height: Math.max(0, Number(values[3])),
  };
}

function sanitizeBrowserNodeAttributes(value: unknown, errorPrefix: string): Record<string, string> {
  if (!isPlainRecord(value)) throw new Error(`${errorPrefix} browserNodeSelections.attributes 无效`);
  const attributes: Record<string, string> = {};
  for (const [key, rawValue] of Object.entries(value).slice(0, 64)) {
    const normalizedKey = key.trim().slice(0, 200);
    if (!normalizedKey || SENSITIVE_BROWSER_ATTRIBUTE_PATTERN.test(normalizedKey)) continue;
    if (typeof rawValue !== 'string') continue;
    attributes[normalizedKey] = rawValue.slice(0, 1000);
  }
  return attributes;
}

function sanitizeBrowserNodeSelection(
  selection: unknown,
  errorPrefix: string,
): MessageBrowserNodeSelection {
  if (!isPlainRecord(selection)) {
    throw new Error(`${errorPrefix} browserNodeSelections 条目无效`);
  }
  const browserSessionId = typeof selection.browserSessionId === 'string' ? selection.browserSessionId.trim() : '';
  const tabId = typeof selection.tabId === 'string' ? selection.tabId.trim() : '';
  const surfaceId = typeof selection.surfaceId === 'string' ? selection.surfaceId.trim() : '';
  const navigationRevision = Number.isSafeInteger(selection.navigationRevision)
    && Number(selection.navigationRevision) >= 0
    ? Number(selection.navigationRevision)
    : -1;
  const url = typeof selection.url === 'string' ? selection.url.trim().slice(0, 4000) : '';
  const title = typeof selection.title === 'string' ? selection.title.trim().slice(0, 1000) : '';
  const backendDomNodeId = Number.isSafeInteger(selection.backendDomNodeId)
    && Number(selection.backendDomNodeId) > 0
    ? Number(selection.backendDomNodeId)
    : -1;
  const nodeName = typeof selection.nodeName === 'string' ? selection.nodeName.trim().slice(0, 200) : '';
  if (!browserSessionId || !tabId || !surfaceId || navigationRevision < 0 || !nodeName || backendDomNodeId < 1) {
    throw new Error(`${errorPrefix} browserNodeSelections 字段无效`);
  }
  const textExcerpt = typeof selection.textExcerpt === 'string'
    ? selection.textExcerpt.trim().slice(0, MAX_BROWSER_NODE_TEXT_LENGTH)
    : '';
  const outerHtml = typeof selection.outerHtml === 'string'
    ? selection.outerHtml.slice(0, MAX_BROWSER_NODE_HTML_LENGTH)
    : '';
  const sensitiveNode = nodeName.toLowerCase() === 'input'
    && /type\s*=\s*["']password["']/iu.test(outerHtml);
  return {
    browserSessionId,
    tabId,
    surfaceId,
    navigationRevision,
    url,
    title,
    frameId: typeof selection.frameId === 'string' ? selection.frameId.trim().slice(0, 500) || null : null,
    backendDomNodeId,
    domNodeId: Number.isSafeInteger(selection.domNodeId) && Number(selection.domNodeId) > 0
      ? Number(selection.domNodeId)
      : null,
    nodeName,
    attributes: sanitizeBrowserNodeAttributes(selection.attributes, errorPrefix),
    textExcerpt: sensitiveNode ? '' : textExcerpt,
    outerHtml: sensitiveNode
      ? outerHtml.replace(/(value\s*=\s*["'])[^"']*(["'])/giu, '$1[REDACTED]$2')
      : outerHtml,
    ariaRole: typeof selection.ariaRole === 'string' ? selection.ariaRole.trim().slice(0, 200) || null : null,
    ariaName: typeof selection.ariaName === 'string' ? selection.ariaName.trim().slice(0, 500) || null : null,
    bounds: sanitizeBrowserNodeRect(selection.bounds, errorPrefix) ?? null,
  };
}

function sanitizeMessageBrowserNodeSelections(
  selections: unknown,
  errorPrefix: string,
): MessageBrowserNodeSelection[] | undefined {
  if (selections === undefined) return undefined;
  if (!Array.isArray(selections)) throw new Error(`${errorPrefix} browserNodeSelections 无效`);
  if (selections.length > MAX_BROWSER_NODE_SELECTIONS) {
    throw new Error(`${errorPrefix} browserNodeSelections 数量超过 ${MAX_BROWSER_NODE_SELECTIONS}`);
  }
  const normalized = selections.map((selection) => sanitizeBrowserNodeSelection(selection, errorPrefix));
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
    const blockId = resolveSanitizedBlockId(sanitized, type, errorPrefix);

    // 后端协议层使用扁平的 toolName/toolId/input/output 属性，
    // 前端渲染层期望嵌套的 toolCall 对象。投影恢复时数据未经 mapStandardBlocks 转换，
    // 这里自动适配：如果缺少 toolCall 但有扁平属性，则构造嵌套对象。
    if (type === 'tool_call' && !sanitized.toolCall && typeof sanitized.toolName === 'string') {
      const toolCall = adaptFlatToolCallBlock(sanitized);
      return {
        id: blockId,
        type: 'tool_call' as const,
        content: '',
        toolCall,
      } as ContentBlock;
    }

    if (type === 'tool_result' && !sanitized.toolCall) {
      const toolCall = adaptFlatToolResultBlock(sanitized);
      return {
        ...sanitized,
        id: blockId,
        type: 'tool_result' as const,
        content,
        toolCall,
      } as ContentBlock;
    }

    // 后端 thinking block 使用扁平 content/summary/blockId，前端期望嵌套 thinking 对象。
    if (type === 'thinking' && !sanitized.thinking) {
      const status = sanitized.isComplete === false ? 'running' : 'completed';
      return {
        id: blockId,
        type: 'thinking' as const,
        content: '',
        thinking: {
          groupId: `thinking-group:${blockId}`,
          segments: [{
            segmentId: blockId,
            messageId: blockId,
            content,
            summary: typeof sanitized.summary === 'string' ? sanitized.summary : undefined,
            status,
          }],
          status,
          isStreaming: status === 'running',
        },
      } as ContentBlock;
    }

    return {
      ...sanitized,
      id: blockId,
      type: type as ContentBlock['type'],
      content,
    } as ContentBlock;
  });
}

function resolveSanitizedBlockId(
  sanitized: Record<string, unknown>,
  type: string,
  errorPrefix: string,
): string {
  const explicit = typeof sanitized.id === 'string' ? sanitized.id.trim() : '';
  if (explicit) {
    return explicit;
  }
  const fallbackBlockId = typeof sanitized.blockId === 'string' ? sanitized.blockId.trim() : '';
  if (fallbackBlockId) {
    return type === 'tool_call' || type === 'tool_result'
      ? `${type}:${fallbackBlockId}`
      : fallbackBlockId;
  }
  if (type === 'tool_call' || type === 'tool_result') {
    const toolCall = sanitized.toolCall as Record<string, unknown> | undefined;
    const toolCallId = typeof toolCall?.id === 'string' ? toolCall.id.trim() : '';
    const flatToolId = typeof sanitized.toolId === 'string' ? sanitized.toolId.trim() : '';
    const flatToolCallId = typeof sanitized.toolCallId === 'string' ? sanitized.toolCallId.trim() : '';
    const identifier = toolCallId || flatToolId || flatToolCallId;
    if (identifier) {
      return `${type}:${identifier}`;
    }
  }
  if (type === 'file_change') {
    const fileChange = sanitized.fileChange as Record<string, unknown> | undefined;
    const filePath = typeof fileChange?.filePath === 'string'
      ? fileChange.filePath.trim()
      : (typeof sanitized.filePath === 'string' ? sanitized.filePath.trim() : '');
    if (filePath) {
      return `file_change:${filePath}`;
    }
  }
  throw new Error(`${errorPrefix} 消息块缺少稳定 id: type=${type}`);
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
    case 'cancelled': case 'canceled': resolvedStatus = 'cancelled'; break;
    default:
      if (standardizedStatus === 'success') resolvedStatus = 'success';
      else if (standardizedStatus === 'cancelled' || standardizedStatus === 'canceled') resolvedStatus = 'cancelled';
      else if (['killed', 'aborted'].includes(standardizedStatus)) resolvedStatus = 'cancelled';
      else if (['error', 'timeout', 'blocked', 'rejected'].includes(standardizedStatus)) resolvedStatus = 'error';
      else if (error) resolvedStatus = 'error';
      else if (output) resolvedStatus = 'success';
      else resolvedStatus = 'running';
  }

  const standardizedHardError = ['error', 'timeout', 'blocked', 'rejected'].includes(standardizedStatus);
  const standardizedError = standardized && standardizedHardError
    ? (typeof standardized.message === 'string' ? standardized.message : undefined)
    : undefined;
  const resolvedError = resolvedStatus === 'error'
    ? (error || standardizedError || output)
    : undefined;

  return {
    id: typeof block.toolId === 'string' ? block.toolId : '',
    name: typeof block.toolName === 'string' ? block.toolName : 'Tool',
    arguments: parseToolInput(block.input),
    status: resolvedStatus,
    result: resolvedStatus === 'error' ? undefined : (output || undefined),
    error: resolvedError || undefined,
    standardized: standardized as import('../types/message').StandardizedToolResult | undefined,
    durationMs: typeof block.duration === 'number' && Number.isFinite(block.duration)
      ? Math.max(0, Math.floor(block.duration))
      : undefined,
  };
}

function adaptFlatToolResultBlock(block: Record<string, unknown>): import('../types/message').ToolCall {
  const standardized = block.standardized as Record<string, unknown> | undefined;
  const standardizedStatus = typeof standardized?.status === 'string'
    ? standardized.status.toLowerCase()
    : '';
  const rawContent = typeof block.content === 'string' ? block.content.trim() : '';
  const fallbackMessage = typeof standardized?.message === 'string' ? standardized.message.trim() : '';
  const resolvedContent = rawContent || fallbackMessage;
  const isCancelled = ['cancelled', 'canceled', 'killed', 'aborted'].includes(standardizedStatus);
  const isError = !isCancelled && (
    block.isError === true
    || ['error', 'timeout', 'blocked', 'rejected'].includes(standardizedStatus)
  );

  return {
    id: typeof block.toolCallId === 'string' ? block.toolCallId : '',
    name: typeof standardized?.toolName === 'string' && standardized.toolName.trim().length > 0
      ? standardized.toolName
      : 'tool_result',
    arguments: parseToolInput(block.input),
    status: isCancelled ? 'cancelled' : (isError ? 'error' : 'success'),
    result: isError ? undefined : (resolvedContent || undefined),
    error: isError ? (resolvedContent || undefined) : undefined,
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
  const source = typeof message.source === 'string' && isValidMessageSource(message.source)
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
  const contextReferences = sanitizeMessageContextReferences(
    message.contextReferences,
    errorPrefix,
  );
  const browserAnnotationRefs = sanitizeMessageBrowserAnnotationRefs(
    message.browserAnnotationRefs,
    errorPrefix,
  );
  const browserNodeSelections = sanitizeMessageBrowserNodeSelections(
    message.browserNodeSelections,
    errorPrefix,
  );
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
    ...(contextReferences ? { contextReferences } : {}),
    ...(browserAnnotationRefs ? { browserAnnotationRefs } : {}),
    ...(browserNodeSelections ? { browserNodeSelections } : {}),
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
    } else if (isValidMessageSource(updates.source as string)) {
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

  if ('contextReferences' in updates) {
    normalized.contextReferences = updates.contextReferences === undefined
      ? undefined
      : sanitizeMessageContextReferences(updates.contextReferences, errorPrefix);
  }
  if ('browserAnnotationRefs' in updates) {
    normalized.browserAnnotationRefs = updates.browserAnnotationRefs === undefined
      ? undefined
      : sanitizeMessageBrowserAnnotationRefs(updates.browserAnnotationRefs, errorPrefix);
  }
  if ('browserNodeSelections' in updates) {
    normalized.browserNodeSelections = updates.browserNodeSelections === undefined
      ? undefined
      : sanitizeMessageBrowserNodeSelections(updates.browserNodeSelections, errorPrefix);
  }

  if ('metadata' in updates) {
    normalized.metadata = sanitizeMessageMetadata(updates.metadata, errorPrefix, {
      preserveUndefined: true,
    });
  }

  return normalized;
}
