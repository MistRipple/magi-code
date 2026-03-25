export interface RequestPlaceholderBindingMessageLike {
  type?: string;
  source?: string;
  visibility?: string;
  metadata?: Record<string, unknown>;
}

const NON_BINDING_MESSAGE_TYPES = new Set([
  'user_input',
  'instruction',
  'task_card',
  'progress',
  'system-notice',
]);

/**
 * 请求级 placeholder 代表“主线这轮响应”的固定落位。
 * 只有编排者自己的用户可见主线响应，才允许接管该 placeholder。
 */
export function canBindRequestPlaceholder(
  message: RequestPlaceholderBindingMessageLike,
): boolean {
  const visibility = typeof message.visibility === 'string'
    ? message.visibility.trim().toLowerCase()
    : '';
  if (visibility === 'system' || visibility === 'debug') {
    return false;
  }

  const source = typeof message.source === 'string'
    ? message.source.trim().toLowerCase()
    : '';
  if (source !== 'orchestrator') {
    return false;
  }

  const type = typeof message.type === 'string'
    ? message.type.trim()
    : '';
  if (!type || NON_BINDING_MESSAGE_TYPES.has(type)) {
    return false;
  }

  const metadata = message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata
    : undefined;
  if (!metadata) {
    return true;
  }

  if (metadata.isPlaceholder === true || metadata.isStatusMessage === true) {
    return false;
  }

  return true;
}
