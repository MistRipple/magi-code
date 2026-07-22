import type {
  AgentId,
  ContentBlock,
  Message,
  MessageImage,
  SessionTimelineProjection,
  ThinkingSegment,
  ThinkingSegmentStatus,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
} from '../types/message';
import type {
  CanonicalToolCall,
  CanonicalTurn,
  CanonicalTurnItem,
  CanonicalTurnItemStatus,
} from '../shared/protocol/canonical-turn';
import { isCanonicalTerminalStatus } from '../shared/protocol/canonical-turn';
import type { ContentBlock as StandardContentBlock } from '../shared/protocol/message-protocol';
import type { CanonicalTurnReducerState } from './turn-reducer';
import { coerceToolArgumentsRecord } from '../lib/tool-call-display';
import { buildCanonicalToolFileChangeBlocks } from '../lib/canonical-tool-file-change';
import { mapStandardBlocks } from '../lib/message-utils';

/**
 * 单个 turn 的「呈现层」预计算结果。
 *
 * 设计目的：把所有「按呈现序读取」的需求收敛到这一个数据结构，消除散落在
 * projection 各处的 `presentationMap.get(...) ?? item.itemSeq` 兜底——
 * fallback 到 itemSeq 正是 Task #96 暴露的 bug：存储序 (itemSeq) 对非增量
 * 推理 provider 来说和呈现序相反，任一处兜底都会把"总耗时"锚点等顺序敏感
 * 计算静默落回到错误的存储序。
 *
 * 强约束：`presentationSeq` 必定 cover 本 turn 的所有 item，缺失即编程错误
 * （buildTurnPresentation 漏处理某个 kind），由 `requirePresentationSeq`
 * 抛出运行时硬错误，而不是静默回退。
 */
interface TurnPresentation {
  /** 按呈现序排好的完整 item 数组——所有需要顺序遍历的下游消费者直接读这个。 */
  readonly orderedItems: readonly CanonicalTurnItem[];
  /** itemId → 1-based 呈现序号。downstream `presentationSeq * 1000` 写入 metadata.itemSeq。 */
  readonly presentationSeq: ReadonlyMap<string, number>;
  /** 预计算的「总耗时」锚点 itemId；turn 未终态或无 responseDurationMs 时为 undefined。 */
  readonly responseDurationAnchorItemId: string | undefined;
}

function normalizeSessionId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeCanonicalWorkerId(item: CanonicalTurnItem): AgentId | undefined {
  const workerId = typeof item.worker?.workerId === 'string' ? item.worker.workerId.trim() : '';
  return (workerId || undefined) as AgentId | undefined;
}

function normalizeCanonicalRoleId(item: CanonicalTurnItem): AgentId | undefined {
  const roleId = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  return (roleId || undefined) as AgentId | undefined;
}

function normalizeCanonicalTaskId(item: CanonicalTurnItem): string | undefined {
  const taskId = typeof item.worker?.taskId === 'string' ? item.worker.taskId.trim() : '';
  return taskId || undefined;
}

function isAgentTaskSidechainItem(item: CanonicalTurnItem): boolean {
  // Sidechain 归属只由执行实例事实决定：代理 item 会携带 roleId/workerId；
  // root agent item 可能也携带 taskId，但不应因此被移出主线。
  const roleId = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  if (roleId && roleId !== 'orchestrator') {
    return true;
  }
  const workerId = typeof item.worker?.workerId === 'string' ? item.worker.workerId.trim() : '';
  return workerId.length > 0;
}

function resolveSidechainTaskId(item: CanonicalTurnItem): string | undefined {
  return isAgentTaskSidechainItem(item) ? normalizeCanonicalTaskId(item) : undefined;
}

function resolveMessageSource(item: CanonicalTurnItem): Message['source'] {
  if (item.kind === 'user_message') {
    return 'user';
  }
  if (isAgentTaskSidechainItem(item)) {
    return normalizeCanonicalRoleId(item) || normalizeCanonicalWorkerId(item) || 'agent';
  }
  return 'orchestrator';
}

function statusToToolStatus(status: CanonicalTurnItemStatus): 'pending' | 'running' | 'success' | 'error' {
  if (status === 'completed') {
    return 'success';
  }
  if (status === 'blocked' || status === 'failed' || status === 'cancelled') {
    return 'error';
  }
  if (status === 'running') {
    return 'running';
  }
  return 'pending';
}

function valueToDisplayText(value: unknown): string | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value === 'string') {
    return value;
  }
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function buildToolBlock(tool: CanonicalToolCall, status: CanonicalTurnItemStatus): ContentBlock {
  const toolStatus = statusToToolStatus(status);
  const resultText = valueToDisplayText(tool.result);
  const errorText = tool.error || (toolStatus === 'error' ? resultText : undefined);
  return {
    id: `tool_call:${tool.callId}`,
    type: 'tool_call',
    content: '',
    toolCall: {
      id: tool.callId,
      name: tool.name,
      arguments: coerceToolArgumentsRecord(tool.arguments),
      status: toolStatus,
      result: toolStatus === 'error' ? undefined : resultText,
      error: errorText,
    },
  };
}

function buildMessageBlocks(
  item: CanonicalTurnItem,
  content: string,
  artifactId: string,
): ContentBlock[] | undefined {
  if (item.kind === 'assistant_thinking') {
    const blockId = `thinking:${item.itemId}`;
    return [{
      id: blockId,
      type: 'thinking',
      content: '',
      thinking: {
        groupId: artifactId,
        segments: [{
          segmentId: item.itemId,
          messageId: artifactId,
          content,
          status: item.status,
          createdAt: item.createdAt,
          updatedAt: item.updatedAt,
        }],
        status: item.status,
        isStreaming: !isCanonicalTerminalStatus(item.status),
        startedAt: item.createdAt,
        updatedAt: item.updatedAt,
      },
    }];
  }
  if (item.blocks && item.blocks.length > 0) {
    return mapStandardBlocks(item.blocks as StandardContentBlock[]);
  }
  if (item.kind === 'tool_call' && item.tool) {
    const fileChangeBlocks = buildCanonicalToolFileChangeBlocks({
      blockIdBase: item.tool.callId,
      sessionId: item.sessionId,
      toolName: item.tool.name,
      arguments: item.tool.arguments,
      result: item.tool.result,
      status: statusToToolStatus(item.status),
    });
    if (fileChangeBlocks.length > 0) {
      return fileChangeBlocks;
    }
    return [buildToolBlock(item.tool, item.status)];
  }
  return undefined;
}

function resolveMessageRole(item: CanonicalTurnItem): Message['role'] {
  if (item.kind === 'user_message') {
    return 'user';
  }
  if (item.kind === 'system_notice' || item.kind === 'task_status') {
    return 'system';
  }
  return 'assistant';
}

function resolveMessageType(item: CanonicalTurnItem): Message['type'] {
  if (item.kind === 'user_message') {
    return 'user_input';
  }
  if (item.kind === 'assistant_thinking') {
    return 'thinking';
  }
  if (item.kind === 'tool_call') {
    return 'tool_call';
  }
  if (item.kind === 'system_notice') {
    return isAgentTaskSidechainItem(item)
      || item.metadata?.noticeKind === 'context_compaction'
      || item.metadata?.noticeKind === 'model_context_fallback'
      ? 'system-notice'
      : 'text';
  }
  if (item.kind === 'assistant_text') {
    return 'text';
  }
  if (item.status === 'failed') {
    return 'error';
  }
  return 'text';
}

function resolveItemContent(item: CanonicalTurnItem): string {
  if (typeof item.content === 'string') {
    return item.content;
  }
  if (item.kind === 'assistant_text') {
    return '';
  }
  if (item.kind === 'tool_call') {
    return item.title || item.tool?.name || '';
  }
  return item.title || '';
}

function resolveItemTimestamp(item: CanonicalTurnItem): number {
  const compactedAt = item.metadata?.noticeKind === 'context_compaction'
    ? item.metadata.compactedAt
    : undefined;
  return typeof compactedAt === 'number' && Number.isFinite(compactedAt)
    ? Math.floor(compactedAt)
    : item.createdAt;
}

function normalizeMessageImagesFromMetadata(
  metadata: Record<string, unknown> | undefined,
): MessageImage[] | undefined {
  const images = metadata?.images;
  if (!Array.isArray(images)) {
    return undefined;
  }
  const normalized = images
    .filter((image): image is Record<string, unknown> => (
      Boolean(image)
      && typeof image === 'object'
      && !Array.isArray(image)
    ))
    .map((image) => {
      const dataUrl = typeof image.dataUrl === 'string' ? image.dataUrl.trim() : '';
      if (!dataUrl) {
        return null;
      }
      const name = typeof image.name === 'string' ? image.name.trim() : '';
      return {
        ...(name ? { name } : {}),
        dataUrl,
      };
    })
    .filter((image): image is MessageImage => image !== null);
  return normalized.length > 0 ? normalized : undefined;
}

function normalizeMessageContextReferencesFromMetadata(
  metadata: Record<string, unknown> | undefined,
): import('../types/message').MessageContextReference[] | undefined {
  const references = metadata?.contextReferences;
  if (!Array.isArray(references)) return undefined;
  const normalized = references
    .filter((reference): reference is Record<string, unknown> => (
      Boolean(reference) && typeof reference === 'object' && !Array.isArray(reference)
    ))
    .map((reference) => {
      const kind = reference.kind === 'file' || reference.kind === 'directory'
        ? reference.kind
        : null;
      const path = typeof reference.path === 'string' ? reference.path.trim() : '';
      if (!kind || !path) return null;
      const name = typeof reference.name === 'string' && reference.name.trim()
        ? reference.name.trim()
        : path.split(/[\\/]/u).filter(Boolean).pop() || path;
      return { kind, path, name };
    })
    .filter((reference): reference is import('../types/message').MessageContextReference => (
      reference !== null
    ));
  return normalized.length > 0 ? normalized : undefined;
}

function messageMetadataWithoutTransportImages(
  metadata: Record<string, unknown> | undefined,
): Record<string, unknown> {
  if (!metadata) {
    return {};
  }
  const messageMetadata = { ...metadata };
  delete messageMetadata.images;
  delete messageMetadata.contextReferences;
  return messageMetadata;
}

function shouldRenderItem(item: CanonicalTurnItem): boolean {
  if (item.visibility.renderable === false) {
    return false;
  }
  if (
    item.kind === 'assistant_text'
    && isCanonicalTerminalStatus(item.status)
    && resolveItemContent(item).trim().length === 0
    && (!item.blocks || item.blocks.length === 0)
  ) {
    return false;
  }
  return true;
}

function isTurnResponseDurationAnchorCandidate(item: CanonicalTurnItem): boolean {
  return item.kind !== 'user_message'
    && item.kind !== 'system_notice'
    && !isAgentTaskSidechainItem(item)
    && shouldRenderItem(item);
}

function canShowTurnResponseDuration(
  presentation: TurnPresentation,
  item: CanonicalTurnItem,
): boolean {
  // 锚点由 buildTurnPresentation 一次性沿 orderedItems 反向扫描预计算，
  // 这里只做单等比较——无运行时 sort、无 fallback。
  return presentation.responseDurationAnchorItemId === item.itemId;
}

function buildMessage(
  turn: CanonicalTurn,
  item: CanonicalTurnItem,
  artifactId: string,
  presentation: TurnPresentation,
): Message {
  const content = resolveItemContent(item);
  const isAgentTaskSidechain = isAgentTaskSidechainItem(item);
  const workerId = isAgentTaskSidechain ? normalizeCanonicalWorkerId(item) : undefined;
  const roleId = isAgentTaskSidechain ? normalizeCanonicalRoleId(item) : undefined;
  const taskId = normalizeCanonicalTaskId(item);
  const blocks = buildMessageBlocks(item, content, artifactId);
  // 流式态对 assistant 的 text 与 thinking 都成立：
  //   - assistant_text：边推 token 边渲染正文；
  //   - assistant_thinking：边推 thinking delta 边在卡片头亮起"思考中..."。
  // 旧实现只覆盖 assistant_text，导致 ThinkingBlock 流式标题永远不触发——
  // ThinkingBlockRenderer 拿到 isStreaming=false，shouldShowStreamingState 恒为 false。
  const isStreaming =
    (item.kind === 'assistant_text' || item.kind === 'assistant_thinking') &&
    !isCanonicalTerminalStatus(item.status);
  const responseDurationMs = canShowTurnResponseDuration(presentation, item)
    ? turn.responseDurationMs
    : undefined;
  const responseCompletedAt = responseDurationMs !== undefined
    && typeof turn.completedAt === 'number'
    && Number.isFinite(turn.completedAt)
    && turn.completedAt >= 0
    ? turn.completedAt
    : undefined;
  const presentationSeq = requirePresentationSeq(presentation, item);
  const images = item.kind === 'user_message'
    ? normalizeMessageImagesFromMetadata(item.metadata)
    : undefined;
  const contextReferences = item.kind === 'user_message'
    ? normalizeMessageContextReferencesFromMetadata(item.metadata)
    : undefined;
  const noticeType = item.metadata?.noticeType;
  const normalizedNoticeType = noticeType === 'success'
    || noticeType === 'error'
    || noticeType === 'warning'
    || noticeType === 'info'
    ? noticeType
    : undefined;
  return {
    id: artifactId,
    role: resolveMessageRole(item),
    source: resolveMessageSource(item),
    content: item.kind === 'assistant_thinking' ? '' : content,
    ...(blocks ? { blocks } : {}),
    ...(images ? { images } : {}),
    ...(contextReferences ? { contextReferences } : {}),
    timestamp: resolveItemTimestamp(item),
    updatedAt: item.updatedAt,
    isStreaming,
    isComplete: !isStreaming,
    type: resolveMessageType(item),
    ...(normalizedNoticeType ? { noticeType: normalizedNoticeType } : {}),
    metadata: {
      ...messageMetadataWithoutTransportImages(item.metadata),
      turnId: item.turnId,
      turnSeq: item.turnSeq,
      turnStatus: turn.status,
      turnItemId: item.itemId,
      turnItemKind: item.kind,
      turnItemStatus: item.status,
      // 这里写入的是「呈现序号」(presentationSeq * 1000)，已经被 buildTurnPresentation
      // 按 protocol 语义重排——thinking 项会被排到对应 assistant_text 之前。
      // 原始存储序号 (`item.itemSeq`，审计字段) 通过 canonicalItemSeq 单独保留。
      itemSeq: presentationSeq * 1000,
      canonicalItemSeq: item.itemSeq,
      blockSeq: item.itemSeq,
      cardStreamSeq: item.itemSeq,
      ...(workerId ? { workerId } : {}),
      ...(roleId ? { roleId } : {}),
      // sourceThreadId 保留为底层审计事实；UI tab 路由只看 metadata.taskId。
      ...(item.sourceThreadId ? { sourceThreadId: item.sourceThreadId } : {}),
      // metadata.taskId 是 RightPane agent run tab 按代理过滤 timeline 的唯一信号。
      ...(taskId ? { taskId } : {}),
      toolCallId: item.tool?.callId,
      toolName: item.tool?.name,
      renderRevision: [
        item.itemVersion ?? 0,
        item.updatedAt,
        item.status,
        content.length,
        blocks?.length ?? 0,
        valueToDisplayText(item.tool?.result)?.length ?? 0,
        item.tool?.error?.length ?? 0,
      ].join(':'),
      ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
      ...(responseCompletedAt !== undefined ? { responseCompletedAt } : {}),
      canonical: true,
    },
  };
}

function resolveArtifactId(turn: CanonicalTurn, item: CanonicalTurnItem): string {
  // P7：artifact 身份统一用 itemId，所有 item kind（含 tool_call）走同一路径。
  // 历史上 tool_call 曾走 `callId || itemId` 双轨：first emit 时 item.tool 尚未填充会落 itemId，
  // 后续 callId 出现又切到 callId，导致同一工具卡在流式过程中 artifactId 漂移、被 Svelte 当作
  // 两个 entry 销毁重建，引起视觉位置错位。callId 仍保留在 block id 与 metadata.toolCallId
  // 用于工具协议层匹配 tool_result，不再参与 artifact 身份。
  return `turn:${turn.turnId}:${item.itemId}`;
}

/**
 * 取 item 在本 turn 中的「呈现序号」(1-based)；缺失即编程错误，抛运行时硬错误。
 *
 * 不做兜底（之前的 `?? item.itemSeq` 兜底是 Task #96 的真凶——存储序对非
 * 增量推理 provider 来说是反序的，静默回退会让"总耗时"锚点等顺序敏感计算
 * 落到错误位置且没有可见信号）。新增 item kind 时如果 buildTurnPresentation
 * 漏处理，这里会立即崩溃，迫使作者补全 kind-aware 重排逻辑。
 *
 * 返回 1-based index；`× 1000` 由调用方显式表达——给每个槽位留 1000 unit
 * 微调空间，整数化以兼容 messages.svelte.ts 对 itemSeq 的 `Math.floor` 规范化。
 */
function requirePresentationSeq(presentation: TurnPresentation, item: CanonicalTurnItem): number {
  const index = presentation.presentationSeq.get(item.itemId);
  if (index === undefined) {
    throw new Error(
      `turn-projection: item ${item.itemId} (kind=${item.kind}) missing from TurnPresentation; `
      + `buildTurnPresentation 漏处理某个 item kind`,
    );
  }
  return index;
}

/**
 * 构造单个 turn 的「呈现层」预计算结果，承担 projection 所有顺序敏感需求。
 *
 * **根因背景**：后端存储里的 `item_seq` 由 `upsert_current_turn_item` 按
 * `max(items.item_seq)+1` 分配——这是「首次 upsert 到达顺序」，不是「协议语义顺序」。
 * 对 Anthropic（thinking_delta → text_delta），到达顺序天然等于协议顺序；
 * 但对部分 OpenAI 兼容 provider（DeepSeek、Qwen 某些版本），`reasoning_content`
 * 不增量流式推送，整段在最终消息里给出，runtime 只能在 post-streaming 阶段
 * 才补上 thinking item，拿到比 text item 更大的 `item_seq`——此时若把
 * `item_seq` 当展示序直接乘 1000，thinking 卡片就排到回答卡片之后。
 *
 * **真正的根因**是「`item_seq` 同时承担存储分配序与展示顺序」这一 conflation。
 * 修复方式：在 projection 层用 kind 语义重排，剥离两者：
 *   - 存储层 `item_seq` 仍然是单调到达序（保留审计、status 不变性）；
 *   - 展示序由本函数按协议语义重新计算——同一 round 内 `thinking` 永远先于
 *     `text`，`tool_call` / `user_message` 等作为 round
 *     边界保持原位（它们的 `item_seq` 已经能正确反映 round 顺序）。
 *
 * 算法：按 `item_seq` 升序遍历 turn.items；
 *   - 累积 `assistant_thinking` / `assistant_text` 到 round buffer；
 *   - 碰到其它 kind 视为 round 边界——先 flush（thinking 排前、text 排后）
 *     再追加边界 item 自身；
 *   - 遍历结束后 flush 末尾残留 buffer。
 *
 * 同时一次性沿 orderedItems 反向扫描预计算 responseDurationAnchorItemId，
 * 避免后续每次询问 anchor 都重新 sort + filter；也彻底脱离 itemSeq——
 * 锚点选择只依赖呈现序，不存在 fallback 路径。
 *
 * 单一 round 内同 kind 多 item 间保持原 `item_seq` 顺序，确保增量 thinking /
 * 多段 text 的稳定性。
 */
function buildTurnPresentation(turn: CanonicalTurn): TurnPresentation {
  const sorted = turn.items
    .slice()
    .sort((left, right) =>
      left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId),
    );

  const ordered: CanonicalTurnItem[] = [];
  let thinkingBuffer: CanonicalTurnItem[] = [];
  let textBuffer: CanonicalTurnItem[] = [];

  const flush = (): void => {
    ordered.push(...thinkingBuffer, ...textBuffer);
    thinkingBuffer = [];
    textBuffer = [];
  };

  for (const item of sorted) {
    if (item.kind === 'assistant_thinking') {
      thinkingBuffer.push(item);
    } else if (item.kind === 'assistant_text') {
      textBuffer.push(item);
    } else {
      flush();
      ordered.push(item);
    }
  }
  flush();

  const presentationSeq = new Map<string, number>();
  ordered.forEach((item, index) => {
    presentationSeq.set(item.itemId, index + 1);
  });

  let responseDurationAnchorItemId: string | undefined;
  if (
    isCanonicalTerminalStatus(turn.status)
    && typeof turn.responseDurationMs === 'number'
    && Number.isFinite(turn.responseDurationMs)
    && turn.responseDurationMs >= 0
  ) {
    for (let i = ordered.length - 1; i >= 0; i -= 1) {
      const candidate = ordered[i]!;
      if (isTurnResponseDurationAnchorCandidate(candidate)) {
        responseDurationAnchorItemId = candidate.itemId;
        break;
      }
    }
  }

  return { orderedItems: ordered, presentationSeq, responseDurationAnchorItemId };
}

function buildArtifact(
  turn: CanonicalTurn,
  item: CanonicalTurnItem,
  presentation: TurnPresentation,
): TimelineProjectionArtifact | null {
  if (!shouldRenderItem(item)) {
    return null;
  }
  const artifactId = resolveArtifactId(turn, item);
  const sidechainTaskId = resolveSidechainTaskId(item);
  const presentationSeq = requirePresentationSeq(presentation, item);
  return {
    artifactId,
    kind: item.kind === 'tool_call' ? 'tool' : 'message',
    displayOrder: turn.turnSeq * 1_000_000 + presentationSeq * 1000,
    artifactVersion: item.itemVersion,
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: item.itemSeq,
    timestamp: resolveItemTimestamp(item),
    cardId: artifactId,
    taskId: sidechainTaskId,
    messageIds: [artifactId, item.itemId],
    message: buildMessage(turn, item, artifactId, presentation),
  };
}

function buildTurnProjectionArtifacts(turn: CanonicalTurn): Array<TimelineProjectionArtifact | null> {
  const presentation = buildTurnPresentation(turn);
  // 遍历 presentation.orderedItems（已按呈现序排好），不再用 turn.items 原顺序——
  // 虽然下游 collapseArtifactsByStableCard 仍会按 displayOrder 总排序，但用
  // orderedItems 让数据流意图更显式：呈现序在这里就已经固定。
  return presentation.orderedItems.map((item) => buildArtifact(turn, item, presentation));
}

function compareArtifacts(left: TimelineProjectionArtifact, right: TimelineProjectionArtifact): number {
  return left.displayOrder - right.displayOrder || left.artifactId.localeCompare(right.artifactId);
}

function mergeMessageIds(left: string[] | undefined, right: string[] | undefined): string[] {
  const merged: string[] = [];
  const seen = new Set<string>();
  for (const messageId of [...(left || []), ...(right || [])]) {
    if (!messageId || seen.has(messageId)) {
      continue;
    }
    seen.add(messageId);
    merged.push(messageId);
  }
  return merged;
}

function mergeDuplicateArtifact(
  first: TimelineProjectionArtifact,
  latest: TimelineProjectionArtifact,
): TimelineProjectionArtifact {
  const firstMetadata = first.message.metadata || {};
  const latestMetadata = latest.message.metadata || {};
  const stableItemSeq = typeof firstMetadata.itemSeq === 'number'
    ? firstMetadata.itemSeq
    : latestMetadata.itemSeq;
  const stableBlockSeq = typeof firstMetadata.blockSeq === 'number'
    ? firstMetadata.blockSeq
    : latestMetadata.blockSeq;
  const stableCardStreamSeq = typeof firstMetadata.cardStreamSeq === 'number'
    ? firstMetadata.cardStreamSeq
    : latestMetadata.cardStreamSeq;

  return {
    ...latest,
    artifactId: first.artifactId,
    displayOrder: Math.min(first.displayOrder, latest.displayOrder),
    cardStreamSeq: Math.min(first.cardStreamSeq, latest.cardStreamSeq),
    timestamp: Math.min(first.timestamp, latest.timestamp),
    cardId: first.cardId,
    taskId: first.taskId || latest.taskId,
    messageIds: mergeMessageIds(first.messageIds, latest.messageIds),
    message: {
      ...latest.message,
      id: first.message.id,
      timestamp: first.message.timestamp,
      metadata: {
        ...latestMetadata,
        itemSeq: stableItemSeq,
        blockSeq: stableBlockSeq,
        cardStreamSeq: stableCardStreamSeq,
      },
    },
  };
}

function resolveArtifactCollapseKey(artifact: TimelineProjectionArtifact): string {
  const metadata = artifact.message.metadata || {};
  const toolCallId = typeof metadata.toolCallId === 'string' ? metadata.toolCallId.trim() : '';
  const turnId = typeof metadata.turnId === 'string' ? metadata.turnId.trim() : '';
  if (artifact.kind === 'tool' && toolCallId && turnId) {
    // artifactId 仍以 itemId 为稳定 UI 身份，避免流式过程中 first emit 无 tool 时发生身份漂移；
    // collapse key 只用于同一 turn 内 tool started/result 分裂 item 的投影归并。
    return `turn:${turnId}:tool-call:${toolCallId}`;
  }
  return artifact.artifactId;
}

function collapseArtifactsByStableCard(
  artifacts: TimelineProjectionArtifact[],
): TimelineProjectionArtifact[] {
  const artifactByCollapseKey = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of artifacts) {
    const collapseKey = resolveArtifactCollapseKey(artifact);
    const existing = artifactByCollapseKey.get(collapseKey);
    artifactByCollapseKey.set(
      collapseKey,
      existing ? mergeDuplicateArtifact(existing, artifact) : artifact,
    );
  }
  return Array.from(artifactByCollapseKey.values()).sort(compareArtifacts);
}

function normalizeMetadataIdentity(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function thinkingBlock(artifact: TimelineProjectionArtifact): ContentBlock | null {
  if (artifact.message.type !== 'thinking') {
    return null;
  }
  const block = artifact.message.blocks?.find((candidate) => candidate.type === 'thinking');
  return block?.thinking ? block : null;
}

function hasSameThinkingOwner(
  left: TimelineProjectionArtifact,
  right: TimelineProjectionArtifact,
): boolean {
  const leftMetadata = left.message.metadata || {};
  const rightMetadata = right.message.metadata || {};
  return normalizeMetadataIdentity(leftMetadata.turnId) === normalizeMetadataIdentity(rightMetadata.turnId)
    && normalizeMetadataIdentity(leftMetadata.sourceThreadId) === normalizeMetadataIdentity(rightMetadata.sourceThreadId)
    && normalizeMetadataIdentity(leftMetadata.taskId) === normalizeMetadataIdentity(rightMetadata.taskId)
    && normalizeMetadataIdentity(leftMetadata.workerId) === normalizeMetadataIdentity(rightMetadata.workerId)
    && normalizeMetadataIdentity(leftMetadata.roleId) === normalizeMetadataIdentity(rightMetadata.roleId)
    && normalizeMetadataIdentity(left.message.source) === normalizeMetadataIdentity(right.message.source);
}

function aggregateThinkingStatus(segments: ThinkingSegment[]): ThinkingSegmentStatus {
  if (segments.some((segment) => segment.status === 'failed')) return 'failed';
  if (segments.some((segment) => segment.status === 'blocked')) return 'blocked';
  if (segments.some((segment) => segment.status === 'cancelled')) return 'cancelled';
  if (segments.some((segment) => segment.status === 'running')) return 'running';
  if (segments.some((segment) => segment.status === 'pending')) return 'pending';
  return 'completed';
}

function mergeThinkingArtifacts(
  first: TimelineProjectionArtifact,
  latest: TimelineProjectionArtifact,
): TimelineProjectionArtifact {
  const firstBlock = thinkingBlock(first);
  const latestBlock = thinkingBlock(latest);
  if (!firstBlock?.thinking || !latestBlock?.thinking) {
    throw new Error('turn-projection: ThinkingGroup artifact 缺少结构化 thinking block');
  }
  const segments = [...firstBlock.thinking.segments, ...latestBlock.thinking.segments];
  const status = aggregateThinkingStatus(segments);
  const isStreaming = status === 'pending' || status === 'running';
  const firstMetadata = first.message.metadata || {};
  const latestMetadata = latest.message.metadata || {};
  const startedAt = segments.reduce<number | undefined>((earliest, segment) => {
    if (typeof segment.createdAt !== 'number') return earliest;
    return earliest === undefined ? segment.createdAt : Math.min(earliest, segment.createdAt);
  }, undefined);
  const updatedAt = segments.reduce<number | undefined>((latestTimestamp, segment) => {
    if (typeof segment.updatedAt !== 'number') return latestTimestamp;
    return latestTimestamp === undefined ? segment.updatedAt : Math.max(latestTimestamp, segment.updatedAt);
  }, undefined);
  const groupBlock: ContentBlock = {
    id: firstBlock.id,
    type: 'thinking',
    content: '',
    thinking: {
      groupId: firstBlock.thinking.groupId,
      segments,
      status,
      isStreaming,
      ...(startedAt !== undefined ? { startedAt } : {}),
      ...(updatedAt !== undefined ? { updatedAt } : {}),
    },
  };

  return {
    ...latest,
    artifactId: first.artifactId,
    displayOrder: first.displayOrder,
    artifactVersion: Math.max(first.artifactVersion ?? 0, latest.artifactVersion ?? 0),
    anchorEventSeq: Math.min(first.anchorEventSeq, latest.anchorEventSeq),
    latestEventSeq: Math.max(first.latestEventSeq, latest.latestEventSeq),
    cardStreamSeq: Math.min(first.cardStreamSeq, latest.cardStreamSeq),
    timestamp: Math.min(first.timestamp, latest.timestamp),
    cardId: first.cardId,
    taskId: first.taskId,
    messageIds: mergeMessageIds(first.messageIds, latest.messageIds),
    message: {
      ...latest.message,
      id: first.message.id,
      content: '',
      blocks: [groupBlock],
      timestamp: first.message.timestamp,
      updatedAt: updatedAt ?? latest.message.updatedAt,
      isStreaming,
      isComplete: status === 'completed',
      metadata: {
        ...latestMetadata,
        turnItemId: firstMetadata.turnItemId,
        turnItemStatus: status,
        itemSeq: firstMetadata.itemSeq,
        canonicalItemSeq: firstMetadata.canonicalItemSeq,
        blockSeq: firstMetadata.blockSeq,
        cardStreamSeq: firstMetadata.cardStreamSeq,
        thinkingSegmentIds: segments.map((segment) => segment.segmentId),
        renderRevision: segments
          .map((segment) => `${segment.segmentId}:${segment.status}:${segment.updatedAt ?? 0}:${segment.content.length}`)
          .join('|'),
      },
    },
  };
}

function timelineLaneKey(artifact: TimelineProjectionArtifact): string {
  const metadata = artifact.message.metadata || {};
  return [
    normalizeMetadataIdentity(metadata.turnId),
    normalizeMetadataIdentity(metadata.sourceThreadId),
    normalizeMetadataIdentity(metadata.taskId),
    normalizeMetadataIdentity(metadata.workerId),
    normalizeMetadataIdentity(metadata.roleId),
    normalizeMetadataIdentity(artifact.message.source),
  ].join('\u0000');
}

function groupAdjacentThinkingArtifacts(
  artifacts: TimelineProjectionArtifact[],
): TimelineProjectionArtifact[] {
  const grouped: TimelineProjectionArtifact[] = [];
  const latestThinkingByLane = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of artifacts) {
    const laneKey = timelineLaneKey(artifact);
    const previous = latestThinkingByLane.get(laneKey);
    if (thinkingBlock(artifact) && previous && hasSameThinkingOwner(previous, artifact)) {
      const merged = mergeThinkingArtifacts(previous, artifact);
      const previousIndex = grouped.indexOf(previous);
      if (previousIndex < 0) {
        throw new Error('turn-projection: ThinkingGroup lane state lost its visible artifact');
      }
      grouped[previousIndex] = merged;
      latestThinkingByLane.set(laneKey, merged);
    } else if (thinkingBlock(artifact)) {
      grouped.push(artifact);
      latestThinkingByLane.set(laneKey, artifact);
    } else {
      grouped.push(artifact);
      latestThinkingByLane.delete(laneKey);
    }
  }
  return grouped.sort(compareArtifacts);
}

function projectStableArtifacts(
  artifacts: TimelineProjectionArtifact[],
): TimelineProjectionArtifact[] {
  return groupAdjacentThinkingArtifacts(collapseArtifactsByStableCard(artifacts));
}

function renderEntry(artifact: TimelineProjectionArtifact): TimelineProjectionRenderEntry {
  return {
    entryId: artifact.artifactId,
    artifactId: artifact.artifactId,
  };
}

export function buildCanonicalTimelineProjection(state: CanonicalTurnReducerState): SessionTimelineProjection | null {
  const sessionId = normalizeSessionId(state.sessionId);
  if (!sessionId) {
    return null;
  }
  const artifacts = projectStableArtifacts(state.turns
    .filter((turn) => turn.status !== 'superseded')
    .flatMap((turn) => buildTurnProjectionArtifacts(turn))
    .filter((artifact): artifact is TimelineProjectionArtifact => Boolean(artifact))
    .sort(compareArtifacts));
  // 主时间线只承接 root agent artifacts；
  // 代理 artifacts 由 RightPane agent run tab 按 metadata.taskId 过滤呈现。
  const threadRenderEntries = artifacts
    .filter((artifact) => !artifact.taskId)
    .map(renderEntry);
  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: Date.now(),
    lastAppliedEventSeq: state.lastAppliedEventSeq,
    artifacts,
    threadRenderEntries,
  };
}

function artifactTurnId(artifact: TimelineProjectionArtifact): string {
  const value = artifact.message.metadata?.turnId;
  return typeof value === 'string' ? value : '';
}

function reuseEquivalentArtifact(
  previousById: Map<string, TimelineProjectionArtifact>,
  artifact: TimelineProjectionArtifact,
): TimelineProjectionArtifact {
  const previous = previousById.get(artifact.artifactId);
  return previous && JSON.stringify(previous) === JSON.stringify(artifact) ? previous : artifact;
}

function mergeSortedArtifacts(
  left: TimelineProjectionArtifact[],
  right: TimelineProjectionArtifact[],
): TimelineProjectionArtifact[] {
  const merged: TimelineProjectionArtifact[] = [];
  let leftIndex = 0;
  let rightIndex = 0;
  while (leftIndex < left.length && rightIndex < right.length) {
    if (compareArtifacts(left[leftIndex]!, right[rightIndex]!) <= 0) {
      merged.push(left[leftIndex++]!);
    } else {
      merged.push(right[rightIndex++]!);
    }
  }
  merged.push(...left.slice(leftIndex), ...right.slice(rightIndex));
  return merged;
}

export function updateCanonicalTimelineProjection(
  previous: SessionTimelineProjection | null,
  state: CanonicalTurnReducerState,
  changedTurnIds: readonly string[] = [],
): SessionTimelineProjection | null {
  if (!previous || previous.sessionId !== state.sessionId || changedTurnIds.length === 0) {
    return buildCanonicalTimelineProjection(state);
  }
  const changedTurnIdSet = new Set(changedTurnIds);
  const previousById = new Map<string, TimelineProjectionArtifact>();
  const retained: TimelineProjectionArtifact[] = [];
  for (const artifact of previous.artifacts) {
    if (changedTurnIdSet.has(artifactTurnId(artifact))) {
      previousById.set(artifact.artifactId, artifact);
    } else {
      retained.push(artifact);
    }
  }
  const changed = projectStableArtifacts(state.turns
    .filter((turn) => changedTurnIdSet.has(turn.turnId))
    .filter((turn) => turn.status !== 'superseded')
    .flatMap((turn) => buildTurnProjectionArtifacts(turn))
    .filter((artifact): artifact is TimelineProjectionArtifact => Boolean(artifact)))
    .map((artifact) => reuseEquivalentArtifact(previousById, artifact));
  const artifacts = mergeSortedArtifacts(retained, changed);
  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId: state.sessionId,
    updatedAt: Date.now(),
    lastAppliedEventSeq: state.lastAppliedEventSeq,
    artifacts,
    threadRenderEntries: artifacts
      .filter((artifact) => !artifact.taskId)
      .map(renderEntry),
  };
}
