import type {
  AgentId,
  ContentBlock,
  DispatchGroupLane,
  Message,
  SessionTimelineProjection,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
  WorkerLaneStatus,
} from '../types/message';
import type {
  CanonicalToolCall,
  CanonicalTurn,
  CanonicalTurnItem,
  CanonicalTurnItemStatus,
} from '../shared/protocol/canonical-turn';
import { isCanonicalTerminalStatus } from '../shared/protocol/canonical-turn';
import type { CanonicalTurnReducerState } from './turn-reducer';

interface LaneProjectionMeta {
  laneId: string;
  laneSeq?: number;
  title: string;
  status: WorkerLaneStatus;
}

interface LaneActivitySnapshot {
  text: string;
  itemSeq: number;
}

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

function resolveWorkerTabAggregateId(item: CanonicalTurnItem): AgentId | undefined {
  // Tab 聚合键只接受 roleId。worker 实例 id 通过 metadata.workerId 单独暴露，
  // 不再参与 tab 维度，避免 drawer 切换和 tab 聚合混用两根身份。
  return normalizeCanonicalRoleId(item);
}

function isWorkerSidechainItem(item: CanonicalTurnItem): boolean {
  // P7.E：item 是否归属 worker drawer 由 worker.roleId 单一信号决定。
  // 归属 orchestrator thread 的 item 没有 roleId 或 roleId === 'orchestrator'。
  const roleId = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  return roleId.length > 0 && roleId !== 'orchestrator';
}

function resolveVisibleWorkerTabId(item: CanonicalTurnItem): AgentId | undefined {
  return isWorkerSidechainItem(item) ? resolveWorkerTabAggregateId(item) : undefined;
}

function resolveMessageSource(item: CanonicalTurnItem): Message['source'] {
  if (item.kind === 'user_message') {
    return 'user';
  }
  return resolveVisibleWorkerTabId(item) || 'orchestrator';
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

function toolArgumentsToRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
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

function readObject(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function normalizeActivityText(value: unknown): string {
  return typeof value === 'string'
    ? value.replace(/\s+/g, ' ').trim()
    : '';
}

function readStringField(record: Record<string, unknown> | undefined, keys: string[]): string {
  if (!record) {
    return '';
  }
  for (const key of keys) {
    const value = normalizeActivityText(record[key]);
    if (value) {
      return value;
    }
  }
  return '';
}

function summarizeToolResult(value: unknown): string {
  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (!trimmed) {
      return '';
    }
    try {
      const parsed = JSON.parse(trimmed);
      return summarizeToolResult(parsed) || normalizeActivityText(trimmed);
    } catch {
      return normalizeActivityText(trimmed);
    }
  }
  const record = readObject(value);
  return readStringField(record, ['summary', 'message', 'error']);
}

function summarizeLaneItem(item: CanonicalTurnItem): string {
  if (item.kind === 'worker_dispatch' || item.kind === 'user_message' || item.kind === 'system_notice') {
    return '';
  }
  const metadata = readObject(item.metadata);
  const explicitSummary = readStringField(metadata, [
    'stageSummary',
    'stage_summary',
    'summary',
    'message',
  ]);
  if (explicitSummary) {
    return explicitSummary;
  }
  if (item.kind === 'tool_call') {
    return summarizeToolResult(item.tool?.result)
      || normalizeActivityText(item.content)
      || normalizeActivityText(item.title || item.tool?.name);
  }
  return normalizeActivityText(item.content) || normalizeActivityText(item.title);
}

function buildToolBlock(tool: CanonicalToolCall, status: CanonicalTurnItemStatus): ContentBlock {
  return {
    id: `tool_call:${tool.callId}`,
    type: 'tool_call',
    content: '',
    toolCall: {
      id: tool.callId,
      name: tool.name,
      arguments: toolArgumentsToRecord(tool.arguments),
      status: statusToToolStatus(status),
      result: valueToDisplayText(tool.result),
      error: tool.error,
    },
  };
}

function buildMessageBlocks(item: CanonicalTurnItem, content: string): ContentBlock[] | undefined {
  if (item.kind === 'tool_call' && item.tool) {
    return [buildToolBlock(item.tool, item.status)];
  }
  if (item.kind === 'assistant_thinking') {
    const blockId = `thinking:${item.itemId}`;
    return [{
      id: blockId,
      type: 'thinking',
      content,
      thinking: {
        content,
        isComplete: isCanonicalTerminalStatus(item.status),
        blockId,
      },
    }];
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
    return isWorkerSidechainItem(item) ? 'system-notice' : 'text';
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

function shouldRenderItem(item: CanonicalTurnItem): boolean {
  if (item.visibility.renderable === false) {
    return false;
  }
  if (item.kind === 'worker_status') {
    return false;
  }
  if (
    item.kind === 'assistant_text'
    && isCanonicalTerminalStatus(item.status)
    && resolveItemContent(item).trim().length === 0
  ) {
    return false;
  }
  return true;
}

function isTurnResponseDurationAnchorCandidate(item: CanonicalTurnItem): boolean {
  return item.kind !== 'user_message'
    && item.kind !== 'system_notice'
    && !isWorkerSidechainItem(item)
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

function resolveDispatchGroupResponseDurationMs(
  turn: CanonicalTurn,
  presentation: TurnPresentation,
): number | undefined {
  // 锚点不存在表示无可用 item 承载 footer（例如整 turn 只有 worker_dispatch），
  // 此时把 responseDurationMs 挂到 dispatch group artifact 上兜底；锚点存在
  // 则交给那个 item 自己渲染 footer，dispatch group 不重复显示。
  if (
    !isCanonicalTerminalStatus(turn.status)
    || typeof turn.responseDurationMs !== 'number'
    || !Number.isFinite(turn.responseDurationMs)
    || turn.responseDurationMs < 0
  ) {
    return undefined;
  }
  return presentation.responseDurationAnchorItemId ? undefined : turn.responseDurationMs;
}

function collectLaneProjectionMeta(turn: CanonicalTurn): Map<string, LaneProjectionMeta> {
  const metaByLaneId = new Map<string, LaneProjectionMeta>();
  for (const item of turn.items) {
    if (item.kind !== 'worker_dispatch') {
      continue;
    }
    const laneId = typeof item.laneId === 'string' ? item.laneId.trim() : '';
    if (!laneId) {
      continue;
    }
    const title = (item.title || item.worker?.title || laneId).trim();
    metaByLaneId.set(laneId, {
      laneId,
      ...(typeof item.laneSeq === 'number' && Number.isFinite(item.laneSeq) ? { laneSeq: item.laneSeq } : {}),
      title,
      status: canonicalStatusToWorkerLaneStatus(item.status),
    });
  }
  return metaByLaneId;
}

function buildMessage(
  turn: CanonicalTurn,
  item: CanonicalTurnItem,
  artifactId: string,
  laneMetaById: Map<string, LaneProjectionMeta>,
  presentation: TurnPresentation,
): Message {
  const content = resolveItemContent(item);
  const workerTabId = resolveVisibleWorkerTabId(item);
  const isWorkerSidechain = isWorkerSidechainItem(item);
  const workerId = isWorkerSidechain ? normalizeCanonicalWorkerId(item) : undefined;
  const roleId = isWorkerSidechain ? normalizeCanonicalRoleId(item) : undefined;
  const blocks = buildMessageBlocks(item, content);
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
  const laneId = typeof item.laneId === 'string' ? item.laneId.trim() : '';
  const laneMeta = laneId ? laneMetaById.get(laneId) : undefined;
  const presentationSeq = requirePresentationSeq(presentation, item);
  return {
    id: artifactId,
    role: resolveMessageRole(item),
    source: resolveMessageSource(item),
    content,
    ...(blocks ? { blocks } : {}),
    timestamp: item.createdAt,
    updatedAt: item.updatedAt,
    isStreaming,
    isComplete: !isStreaming,
    type: resolveMessageType(item),
    metadata: {
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
      laneId: item.laneId,
      laneSeq: item.laneSeq,
      ...(laneMeta?.title ? { laneTitle: laneMeta.title } : {}),
      ...(laneMeta?.status ? { laneStatus: laneMeta.status } : {}),
      cardStreamSeq: item.itemSeq,
      ...(workerId ? { workerId } : {}),
      ...(roleId ? { roleId } : {}),
      ...(workerTabId ? { workerTabId } : {}),
      // P7.E：UI 路由单一信号 —— worker.roleId 区分主线与 worker drawer，
      // sourceThreadId 透传给后续多 thread 视图使用（resume、thread tree、跨 worker 追踪）。
      ...(item.sourceThreadId ? { sourceThreadId: item.sourceThreadId } : {}),
      taskId: item.worker?.taskId,
      toolCallId: item.tool?.callId,
      toolName: item.tool?.name,
      ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
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
 *     `text`，`tool_call` / `worker_dispatch` / `user_message` 等作为 round
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
  laneMetaById: Map<string, LaneProjectionMeta>,
  presentation: TurnPresentation,
): TimelineProjectionArtifact | null {
  if (!shouldRenderItem(item)) {
    return null;
  }
  const artifactId = resolveArtifactId(turn, item);
  const workerTabId = resolveVisibleWorkerTabId(item);
  const presentationSeq = requirePresentationSeq(presentation, item);
  return {
    artifactId,
    kind: item.kind === 'tool_call' ? 'tool' : 'message',
    displayOrder: turn.turnSeq * 1_000_000 + presentationSeq * 1000,
    artifactVersion: item.itemVersion,
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: item.itemSeq,
    timestamp: item.createdAt,
    cardId: artifactId,
    laneId: item.laneId,
    worker: workerTabId,
    messageIds: [artifactId, item.itemId],
    message: buildMessage(turn, item, artifactId, laneMetaById, presentation),
  };
}

function canonicalStatusToWorkerLaneStatus(status: CanonicalTurnItemStatus): WorkerLaneStatus {
  switch (status) {
    case 'blocked':
      return 'blocked';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    case 'completed':
      return 'completed';
    case 'running':
      return 'running';
    case 'pending':
    default:
      return 'pending';
  }
}

function mergeLaneStatus(current: WorkerLaneStatus, next: WorkerLaneStatus): WorkerLaneStatus {
  if (next === 'failed' || current === 'failed') return 'failed';
  if (next === 'blocked' || current === 'blocked') return 'blocked';
  if (next === 'cancelled' || current === 'cancelled') return 'cancelled';
  if (next === 'running' || current === 'running') return 'running';
  if (next === 'pending' || current === 'pending') return 'pending';
  return 'completed';
}

function buildDispatchGroupArtifact(
  turn: CanonicalTurn,
  presentation: TurnPresentation,
): TimelineProjectionArtifact | null {
  const dispatchItems = turn.items
    .filter((item) => item.kind === 'worker_dispatch' && typeof item.laneId === 'string' && item.laneId.trim())
    .sort((left, right) => left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId));
  if (dispatchItems.length === 0) {
    return null;
  }

  const laneById = new Map<string, DispatchGroupLane>();
  const laneVersionById = new Map<string, number>();
  const laneActivityById = new Map<string, LaneActivitySnapshot>();
  for (const item of dispatchItems) {
    const laneId = item.laneId?.trim();
    if (!laneId) {
      continue;
    }
    const worker = resolveWorkerTabAggregateId(item) || 'orchestrator';
    const title = (item.title || item.worker?.title || laneId).trim();
    const status = canonicalStatusToWorkerLaneStatus(item.status);
    laneById.set(laneId, {
      laneId,
      laneVersion: item.itemSeq,
      title,
      description: item.content || title,
      status,
      startedAt: item.createdAt,
      ...(isCanonicalTerminalStatus(item.status) ? { endedAt: item.updatedAt } : {}),
      jumpTarget: { workerTabId: worker },
    });
    laneVersionById.set(laneId, item.itemSeq);
  }

  for (const item of turn.items) {
    const laneId = item.laneId?.trim();
    if (!laneId || !laneById.has(laneId)) {
      continue;
    }
    const lane = laneById.get(laneId)!;
    lane.laneVersion = Math.max(lane.laneVersion, item.itemSeq);
    laneVersionById.set(laneId, lane.laneVersion);
    if (item.kind === 'tool_call') {
      lane.toolUseCount = (lane.toolUseCount || 0) + 1;
    }
    // P2：lane liveActivity / summary 只接受后端显式发出的 `worker_status` 摘要 item，
    // 其它 worker 侧事件（thinking/tool_call/stream）不再参与主线摘要聚合——主线
    // 信息密度由后端控制，drawer 仍保留完整事件流。
    if (item.kind === 'worker_status') {
      const activityText = summarizeLaneItem(item);
      if (activityText) {
        const currentActivity = laneActivityById.get(laneId);
        if (!currentActivity || item.itemSeq >= currentActivity.itemSeq) {
          laneActivityById.set(laneId, { text: activityText, itemSeq: item.itemSeq });
        }
      }
    }
    if (isCanonicalTerminalStatus(item.status)) {
      lane.endedAt = item.updatedAt;
    }
  }

  for (const [laneId, activity] of laneActivityById.entries()) {
    const lane = laneById.get(laneId);
    if (!lane) {
      continue;
    }
    if (lane.status === 'running' || lane.status === 'pending') {
      lane.liveActivity = activity.text;
    } else {
      lane.summary = activity.text;
    }
  }

  const lanes = Array.from(laneById.values())
    .sort((left, right) => {
      const leftSeq = dispatchItems.find((item) => item.laneId === left.laneId)?.laneSeq ?? Number.MAX_SAFE_INTEGER;
      const rightSeq = dispatchItems.find((item) => item.laneId === right.laneId)?.laneSeq ?? Number.MAX_SAFE_INTEGER;
      return leftSeq - rightSeq || left.laneId.localeCompare(right.laneId);
    })
    .map((lane) => {
      const totalTaskCount = 1;
      return {
        ...lane,
        progressSummary: {
          totalTaskCount,
          completedTaskCount: lane.status === 'completed' ? 1 : 0,
          blockedTaskCount: lane.status === 'blocked' ? 1 : 0,
          awaitingApprovalTaskCount: lane.status === 'awaiting_approval' ? 1 : 0,
          reviewRequiredTaskCount: lane.status === 'review_required' ? 1 : 0,
        },
        tasks: [{
          taskId: dispatchItems.find((item) => item.laneId === lane.laneId)?.worker?.taskId,
          title: lane.title,
          status: lane.status,
          isCurrent: lane.status === 'running' || lane.status === 'pending',
          seq: dispatchItems.find((item) => item.laneId === lane.laneId)?.laneSeq,
        }],
      };
    });
  if (lanes.length === 0) {
    return null;
  }

  const groupStatus = lanes.reduce<WorkerLaneStatus>(
    (status, lane) => mergeLaneStatus(status, lane.status),
    'completed',
  );
  const firstItem = dispatchItems[0]!;
  const artifactId = `turn:${turn.turnId}:worker-dispatch-group`;
  const responseDurationMs = resolveDispatchGroupResponseDurationMs(turn, presentation);
  // worker_dispatch 在 buildTurnPresentation 里作为 round 边界保留原顺序，
  // 必定在 presentation.presentationSeq 中，requirePresentationSeq 不会抛。
  const firstItemPresentationSeq = requirePresentationSeq(presentation, firstItem);
  const dispatchBlockId = `dispatch-group:${turn.turnId}`;
  const block: ContentBlock = {
    id: dispatchBlockId,
    type: 'dispatch_group',
    content: '',
    blockId: dispatchBlockId,
    dispatchWaveId: turn.turnId,
    status: groupStatus,
    lanes,
  };
  const message: Message = {
    id: artifactId,
    role: 'assistant',
    source: 'orchestrator',
    content: '',
    blocks: [block],
    timestamp: firstItem.createdAt,
    updatedAt: Math.max(...dispatchItems.map((item) => item.updatedAt || item.createdAt)),
    isStreaming: groupStatus === 'running' || groupStatus === 'pending',
    isComplete: groupStatus !== 'running' && groupStatus !== 'pending',
    type: 'text',
    metadata: {
      turnId: turn.turnId,
      turnSeq: turn.turnSeq,
      turnStatus: turn.status,
      turnItemId: artifactId,
      turnItemKind: 'worker_dispatch',
      turnItemStatus: groupStatus,
      // dispatch group 的 itemSeq 取首 dispatch item 的展示序（×1000），与
      // buildMessage 写入值对齐，让 compareTimelineSemanticOrder 在跨 artifact
      // 排序时数量级一致。
      itemSeq: firstItemPresentationSeq * 1000,
      canonicalItemSeq: firstItem.itemSeq,
      blockSeq: firstItem.itemSeq,
      cardStreamSeq: firstItem.itemSeq,
      dispatchWaveId: turn.turnId,
      ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
      canonical: true,
    },
  };

  return {
    artifactId,
    kind: 'message',
    displayOrder: turn.turnSeq * 1_000_000 + firstItemPresentationSeq * 1000,
    artifactVersion: Math.max(...Array.from(laneVersionById.values())),
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: firstItem.itemSeq,
    timestamp: firstItem.createdAt,
    cardId: artifactId,
    dispatchWaveId: turn.turnId,
    messageIds: [artifactId, ...dispatchItems.map((item) => item.itemId)],
    message,
  };
}

function buildTurnProjectionArtifacts(turn: CanonicalTurn): Array<TimelineProjectionArtifact | null> {
  const laneMetaById = collectLaneProjectionMeta(turn);
  const presentation = buildTurnPresentation(turn);
  return [
    buildDispatchGroupArtifact(turn, presentation),
    // 遍历 presentation.orderedItems（已按呈现序排好），不再用 turn.items 原顺序——
    // 虽然下游 collapseArtifactsByStableCard 仍会按 displayOrder 总排序，但用
    // orderedItems 让数据流意图更显式：呈现序在这里就已经固定。
    ...presentation.orderedItems.map((item) => buildArtifact(turn, item, laneMetaById, presentation)),
  ];
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
    laneId: first.laneId || latest.laneId,
    worker: first.worker || latest.worker,
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
  const artifacts = collapseArtifactsByStableCard(state.turns
    .flatMap((turn) => buildTurnProjectionArtifacts(turn))
    .filter((artifact): artifact is TimelineProjectionArtifact => Boolean(artifact))
    .sort(compareArtifacts));
  // P7.E：路由由 artifact.worker 单一信号决定。
  // - artifact.worker 为空 → orchestrator 主线 thread；
  // - artifact.worker 为非空字符串 → 对应 roleId 的 worker drawer。
  const threadRenderEntries = artifacts
    .filter((artifact) => !artifact.worker)
    .map(renderEntry);
  const workerRenderEntries: Record<string, TimelineProjectionRenderEntry[]> = {};
  for (const artifact of artifacts) {
    const workerId = artifact.worker;
    if (!workerId) {
      continue;
    }
    if (!workerRenderEntries[workerId]) {
      workerRenderEntries[workerId] = [];
    }
    workerRenderEntries[workerId].push(renderEntry(artifact));
  }
  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: Date.now(),
    lastAppliedEventSeq: state.lastAppliedEventSeq,
    artifacts,
    threadRenderEntries,
    workerRenderEntries,
  };
}
