import { isRuntimeInternalTool } from './tool-visibility';

export type TimelinePresentationKind = 'tool' | 'message';

export interface TimelinePresentationBlockLike {
  type?: string;
  content?: string;
  toolName?: string;
  toolCall?: {
    name?: string;
  };
}

export interface TimelinePresentationMessageLike {
  role?: string;
  type?: string;
  source?: string;
  id?: string;
  content?: string;
  blocks?: TimelinePresentationBlockLike[];
  isStreaming?: boolean;
  isComplete?: boolean;
  metadata?: Record<string, unknown>;
}

function isCanonicalTimelineMessage(message: TimelinePresentationMessageLike): boolean {
  return message.metadata?.canonical === true;
}

function extractToolBlockName(block: TimelinePresentationBlockLike | undefined | null): string {
  if (!block) {
    return '';
  }
  const fromTop = typeof block.toolName === 'string' ? block.toolName.trim() : '';
  if (fromTop) {
    return fromTop;
  }
  const fromCall = typeof block.toolCall?.name === 'string' ? block.toolCall.name.trim() : '';
  return fromCall;
}

function hasToolBlock(blocks: TimelinePresentationBlockLike[] | undefined): boolean {
  return Array.isArray(blocks) && blocks.some((block) => block?.type === 'tool_call' || block?.type === 'tool_result');
}

function hasOnlyRuntimeInternalToolBlocks(blocks: TimelinePresentationBlockLike[] | undefined): boolean {
  if (!Array.isArray(blocks) || blocks.length === 0) {
    return false;
  }
  let sawToolBlock = false;
  for (const block of blocks) {
    if (!block) continue;
    if (block.type === 'tool_call' || block.type === 'tool_result') {
      sawToolBlock = true;
      const name = extractToolBlockName(block);
      if (!name || !isRuntimeInternalTool(name)) {
        return false;
      }
    }
  }
  return sawToolBlock;
}

export function resolveTimelinePrimaryToolCallName(
  blocks: TimelinePresentationBlockLike[] | undefined,
): string {
  const safeBlocks = Array.isArray(blocks) ? blocks : [];
  for (const block of safeBlocks) {
    if (!block || (block.type !== 'tool_call' && block.type !== 'tool_result')) {
      continue;
    }
    const rawName = typeof block.toolName === 'string'
      ? block.toolName.trim()
      : (typeof block.toolCall?.name === 'string' ? block.toolCall.name.trim() : '');
    if (rawName) {
      return rawName;
    }
  }
  return '';
}

export function resolveTimelinePresentationKind(
  message: TimelinePresentationMessageLike,
): TimelinePresentationKind {
  if (hasToolBlock(message.blocks)) {
    // canonical 消息已经由后端 visibility.renderable 决定是否进入时间线；
    // 这里只保留 legacy 消息的运行时内部工具过滤。
    if (!isCanonicalTimelineMessage(message) && hasOnlyRuntimeInternalToolBlocks(message.blocks)) {
      return 'message';
    }
    return 'tool';
  }
  return 'message';
}

export function messageHasRenderableTimelineContent(
  message: TimelinePresentationMessageLike,
): boolean {
  const normalizedRole = typeof message.role === 'string' ? message.role.trim().toLowerCase() : '';

  // thinking 消息即使 content 暂时为空也应保留占位（流式场景）
  if (message.type === 'thinking') {
    return true;
  }

  // 普通 started / streaming 空消息同样需要先占住时间轴锚点。
  // 否则 live 侧 update 先到时会找不到目标，导致主线正文/代理正文被丢弃。
  // 仅排除 user / system 类型，避免把纯控制型空 notice 误投进时间轴。
  if (
    message.isStreaming === true
    && normalizedRole !== 'user'
    && normalizedRole !== 'system'
    && message.type !== 'user_input'
    && message.type !== 'system-notice'
    && message.type !== 'error'
  ) {
    return true;
  }

  // system-notice 必须有实际文本内容才视为可渲染，
  // 空内容的状态型 system-notice（如 phase 通知）不应进入时间轴
  if (message.type === 'system-notice') {
    if (typeof message.content === 'string' && message.content.trim()) {
      return true;
    }
    // 降级检查 blocks
    const blocks = Array.isArray(message.blocks) ? message.blocks : [];
    return blocks.some((block) => {
      if (!block) return false;
      if (block.type === 'text') {
        return Boolean(block.content && block.content.trim());
      }
      return false;
    });
  }
  if (typeof message.content === 'string' && message.content.trim()) {
    return true;
  }
  const blocks = Array.isArray(message.blocks) ? message.blocks : [];
  return blocks.some((block) => {
    if (!block) {
      return false;
    }
    if (block.type === 'text' || block.type === 'code' || block.type === 'thinking') {
      return Boolean(block.content && block.content.trim());
    }
    return block.type === 'tool_call'
      || block.type === 'tool_result'
      || block.type === 'file_change'
      || block.type === 'plan';
  });
}
