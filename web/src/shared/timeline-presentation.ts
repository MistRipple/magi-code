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

function normalizeTimelineAliasCandidate(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

export function collectTimelineAliasIds(
  message: Pick<TimelinePresentationMessageLike, 'id' | 'metadata' | 'role' | 'type'>,
): string[] {
  const aliases = new Set<string>();
  const addAlias = (value: unknown): void => {
    const normalized = normalizeTimelineAliasCandidate(value);
    if (normalized) {
      aliases.add(normalized);
    }
  };

  addAlias(message.id);

  const metadata = message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata
    : undefined;
  if (!metadata) {
    return Array.from(aliases);
  }

  const sessionId = normalizeTimelineAliasCandidate(metadata.sessionId);
  const turnId = normalizeTimelineAliasCandidate(metadata.turnId);
  const turnItemId = normalizeTimelineAliasCandidate(metadata.turnItemId);
  const rustStreamItemId = normalizeTimelineAliasCandidate(metadata.rustStreamItemId);
  const rustEventItemId = normalizeTimelineAliasCandidate(metadata.rustEventItemId);
  const toolCallId = normalizeTimelineAliasCandidate(metadata.toolCallId);

  if (turnItemId) {
    addAlias(turnItemId);
    addAlias(`rust-timeline:${turnItemId}`);
    if (turnId) {
      addAlias(`turn:${turnId}:${turnItemId}`);
    }
    if (sessionId) {
      addAlias(`turn-live:${sessionId}:${turnItemId}`);
    }
  }
  if (rustStreamItemId) {
    addAlias(rustStreamItemId);
    addAlias(`rust-timeline:${rustStreamItemId}`);
    if (sessionId) {
      addAlias(`turn-live:${sessionId}:${rustStreamItemId}`);
    }
  }
  if (rustEventItemId) {
    addAlias(rustEventItemId);
    addAlias(`rust-timeline:${rustEventItemId}`);
    if (sessionId) {
      addAlias(`turn-live:${sessionId}:${rustEventItemId}`);
    }
  }
  if (toolCallId) {
    addAlias(`rust-timeline:turn-item-tool-started-${toolCallId}`);
    addAlias(`rust-timeline:turn-item-tool-result-${toolCallId}`);
  }

  return Array.from(aliases);
}

function resolveTimelineMetadata(message: Pick<TimelinePresentationMessageLike, 'metadata'>): Record<string, unknown> | undefined {
  return message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata
    : undefined;
}


function hasToolBlock(blocks: TimelinePresentationBlockLike[] | undefined): boolean {
  return Array.isArray(blocks) && blocks.some((block) => block?.type === 'tool_call' || block?.type === 'tool_result');
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
    return 'tool';
  }
  return 'message';
}

export function messageHasRenderableTimelineContent(
  message: TimelinePresentationMessageLike,
): boolean {
  const metadata = resolveTimelineMetadata(message);
  const normalizedRole = typeof message.role === 'string' ? message.role.trim().toLowerCase() : '';

  // request placeholder 是主线响应的固定锚点。
  // 即使尚未收到正文/thinking/tool_call，也必须先落到时间轴中，
  // 后续所有流式更新才能原位接管，live/restore 才能保持一致。
  if (metadata?.isPlaceholder === true) {
    return true;
  }

  // thinking 消息即使 content 暂时为空也应保留占位（流式场景）
  if (message.type === 'thinking') {
    return true;
  }

  // 普通 started / streaming 空消息同样需要先占住时间轴锚点。
  // 否则 live 侧 update 先到时会找不到目标，导致主线正文/worker 正文被丢弃。
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
      || block.type === 'plan'
      || block.type === 'dispatch_group';
  });
}

export function resolveTimelineWorkerVisibility(
  input: {
    hasWorker: boolean;
    type?: string;
    source?: string;
    blocks?: TimelinePresentationBlockLike[];
    metadata?: Record<string, unknown>;
  },
): { threadVisible: boolean; includeWorkerTab: boolean } {
  if (input.type === 'user_input') {
    return {
      threadVisible: true,
      includeWorkerTab: input.hasWorker,
    };
  }

  if (input.type === 'instruction') {
    return {
      threadVisible: false,
      includeWorkerTab: input.hasWorker,
    };
  }

  const normalizedSource = typeof input.source === 'string' ? input.source.trim().toLowerCase() : '';

  // 编排者消息始终保持主线可见。
  // 即使消息携带了 worker/agent 元数据（如编排者使用 Worker 模型执行的分析/总结），
  // 也不应因此退化为 Worker-only 消息。
  if (normalizedSource === 'orchestrator') {
    return {
      threadVisible: true,
      includeWorkerTab: false,
    };
  }

  if (normalizedSource === 'worker') {
    if (!input.hasWorker) {
      return {
        threadVisible: false,
        includeWorkerTab: false,
      };
    }
    if (hasToolBlock(input.blocks)) {
      return {
        threadVisible: true,
        includeWorkerTab: true,
      };
    }
    switch (input.type) {
      case 'error':
      case 'interaction':
        return {
          threadVisible: false,
          includeWorkerTab: input.hasWorker,
        };
      case 'system-notice':
      case 'progress':
      case 'result':
        return {
          threadVisible: false,
          includeWorkerTab: input.hasWorker,
        };
      default:
        return {
          threadVisible: false,
          includeWorkerTab: true,
        };
    }
  }

  if (input.hasWorker) {
    return {
      threadVisible: false,
      includeWorkerTab: true,
    };
  }

  return {
    threadVisible: true,
    includeWorkerTab: false,
  };
}
