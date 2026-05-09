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

function metadataFlagTrue(metadata: Record<string, unknown> | undefined, key: string): boolean {
  if (!metadata) return false;
  const value = metadata[key];
  if (typeof value === 'boolean') return value;
  if (typeof value === 'string') {
    const normalized = value.trim().toLowerCase();
    return normalized === 'true' || normalized === '1';
  }
  return false;
}

function isWorkerSummaryMessage(
  metadata: Record<string, unknown> | undefined,
  blocks: TimelinePresentationBlockLike[] | undefined,
): boolean {
  if (!metadata && !blocks) return false;
  if (metadataFlagTrue(metadata, 'workerSummary')) return true;
  const turnItemKind = typeof metadata?.turnItemKind === 'string' ? metadata.turnItemKind.trim() : '';
  if (turnItemKind === 'worker_summary' || turnItemKind === 'orchestrator_summary' || turnItemKind === 'worker_dispatch') {
    return true;
  }
  const summaryKind = typeof metadata?.summaryKind === 'string' ? metadata.summaryKind.trim() : '';
  if (summaryKind === 'worker_summary' || summaryKind === 'dispatch_group') {
    return true;
  }
  if (Array.isArray(blocks) && blocks.some((block) => block?.type === 'dispatch_group')) {
    return true;
  }
  return false;
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
    // 运行时内部工具（worker_dispatch / worker_wait / send_worker_message 等）
    // 不允许进入主线工具卡路径，应作为普通编排消息处理（或更上层过滤掉）。
    if (hasOnlyRuntimeInternalToolBlocks(message.blocks)) {
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

  // 运行时内部工具（worker_dispatch / worker_wait 等）不允许进入任何主线可见位置。
  // 它们应被收纳进任务面板或运行时日志；主线既不展示工具卡也不生成普通消息外壳。
  if (hasOnlyRuntimeInternalToolBlocks(input.blocks)) {
    return {
      threadVisible: false,
      includeWorkerTab: input.hasWorker,
    };
  }

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
    // Worker 摘要消息（由后端或编排者合成的 worker_summary / dispatch_group 卡）
    // 是 worker → 主线的唯一合法入口，此外 worker 原始执行流一律只进 worker tab。
    if (isWorkerSummaryMessage(input.metadata, input.blocks)) {
      return {
        threadVisible: true,
        includeWorkerTab: true,
      };
    }
    return {
      threadVisible: false,
      includeWorkerTab: true,
    };
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
