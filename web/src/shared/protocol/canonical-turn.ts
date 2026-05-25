export const CANONICAL_TURN_SCHEMA_VERSION = 'canonical-turn.v1' as const;

export type CanonicalTurnStatus = 'pending' | 'running' | 'completed' | 'blocked' | 'failed' | 'cancelled';

export type CanonicalTurnItemKind =
  | 'user_message'
  | 'assistant_text'
  | 'assistant_thinking'
  | 'tool_call'
  | 'task_status'
  | 'system_notice';

export type CanonicalTurnItemStatus = CanonicalTurnStatus;

export type CanonicalTurnEventKind = 'turn_started' | 'turn_item_upsert' | 'turn_completed';

export interface CanonicalTurnVisibility {
  renderable: boolean;
}

export interface CanonicalToolCall {
  callId: string;
  name: string;
  arguments?: unknown;
  result?: unknown;
  error?: string;
}

export interface CanonicalWorkerRef {
  taskId?: string;
  workerId?: string;
  roleId?: string;
  title?: string;
}

export interface CanonicalTurnItem {
  sessionId: string;
  turnId: string;
  turnSeq: number;
  itemId: string;
  itemSeq: number;
  kind: CanonicalTurnItemKind;
  createdAt: number;
  status: CanonicalTurnItemStatus;
  itemVersion?: number;
  updatedAt: number;
  title?: string;
  content?: string;
  blocks?: unknown[];
  tool?: CanonicalToolCall;
  worker?: CanonicalWorkerRef;
  /**
   * item 归属的 thread_id。orchestrator 主线 item 对应 session 级
   * orchestrator thread，代理 item 对应各 task thread。
   * 由后端 canonical projection 保证非空，是前端 thread/task 详情的唯一路由信号。
   */
  sourceThreadId: string;
  visibility: CanonicalTurnVisibility;
  metadata?: Record<string, unknown>;
}

export interface CanonicalTurn {
  sessionId: string;
  turnId: string;
  turnSeq: number;
  acceptedAt: number;
  completedAt?: number;
  status: CanonicalTurnStatus;
  responseDurationMs?: number;
  usage?: unknown;
  items: CanonicalTurnItem[];
  metadata?: Record<string, unknown>;
}

export interface CanonicalTurnEvent {
  schemaVersion: typeof CANONICAL_TURN_SCHEMA_VERSION;
  eventId: string;
  eventSeq: number;
  kind: CanonicalTurnEventKind;
  sessionId: string;
  turnId: string;
  turnSeq: number;
  occurredAt: number;
  turn?: CanonicalTurn;
  item?: CanonicalTurnItem;
  stream?: CanonicalTurnStreamUpdate;
}

export interface CanonicalTurnStreamUpdate {
  delta: string;
  contentLength: number;
  reset: boolean;
}

const CANONICAL_TURN_STATUSES: CanonicalTurnStatus[] = ['pending', 'running', 'completed', 'blocked', 'failed', 'cancelled'];
const CANONICAL_TURN_ITEM_KINDS: CanonicalTurnItemKind[] = [
  'user_message',
  'assistant_text',
  'assistant_thinking',
  'tool_call',
  'task_status',
  'system_notice',
];
const CANONICAL_TURN_EVENT_KINDS: CanonicalTurnEventKind[] = ['turn_started', 'turn_item_upsert', 'turn_completed'];

function readRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function readString(record: Record<string, unknown>, ...keys: string[]): string {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'string' && value.trim()) {
      return value.trim();
    }
  }
  return '';
}

function readRawString(record: Record<string, unknown>, ...keys: string[]): string | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'string') {
      return value;
    }
  }
  return undefined;
}

function readNumber(record: Record<string, unknown>, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'number' && Number.isFinite(value)) {
      return Math.floor(value);
    }
  }
  return undefined;
}

function readBoolean(record: Record<string, unknown>, defaultValue: boolean, ...keys: string[]): boolean {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'boolean') {
      return value;
    }
  }
  return defaultValue;
}

function readStatus(value: string): CanonicalTurnStatus | undefined {
  return CANONICAL_TURN_STATUSES.includes(value as CanonicalTurnStatus)
    ? value as CanonicalTurnStatus
    : undefined;
}

function readItemKind(value: string): CanonicalTurnItemKind | undefined {
  return CANONICAL_TURN_ITEM_KINDS.includes(value as CanonicalTurnItemKind)
    ? value as CanonicalTurnItemKind
    : undefined;
}

function readEventKind(value: string): CanonicalTurnEventKind | undefined {
  return CANONICAL_TURN_EVENT_KINDS.includes(value as CanonicalTurnEventKind)
    ? value as CanonicalTurnEventKind
    : undefined;
}

function normalizeCanonicalToolCall(value: unknown): CanonicalToolCall | undefined {
  const record = readRecord(value);
  if (!record) {
    return undefined;
  }
  const callId = readString(record, 'callId', 'call_id');
  const name = readString(record, 'name');
  if (!callId || !name) {
    return undefined;
  }
  const error = readString(record, 'error') || undefined;
  return {
    callId,
    name,
    ...(record.arguments !== undefined ? { arguments: record.arguments } : {}),
    ...(record.result !== undefined ? { result: record.result } : {}),
    ...(error ? { error } : {}),
  };
}

function normalizeCanonicalWorkerRef(value: unknown): CanonicalWorkerRef | undefined {
  const record = readRecord(value);
  if (!record) {
    return undefined;
  }
  const worker: CanonicalWorkerRef = {
    ...(readString(record, 'taskId', 'task_id') ? { taskId: readString(record, 'taskId', 'task_id') } : {}),
    ...(readString(record, 'workerId', 'worker_id') ? { workerId: readString(record, 'workerId', 'worker_id') } : {}),
    ...(readString(record, 'roleId', 'role_id') ? { roleId: readString(record, 'roleId', 'role_id') } : {}),
    ...(readString(record, 'title') ? { title: readString(record, 'title') } : {}),
  };
  return Object.keys(worker).length > 0 ? worker : undefined;
}

function normalizeCanonicalVisibility(value: unknown): CanonicalTurnVisibility {
  const record = readRecord(value) || {};
  return {
    renderable: readBoolean(record, true, 'renderable'),
  };
}

function normalizeCanonicalTurnStreamUpdate(
  record: Record<string, unknown>,
): CanonicalTurnStreamUpdate | undefined {
  const delta = readRawString(record, 'streamDelta', 'stream_delta');
  const contentLength = readNumber(record, 'streamContentLength', 'stream_content_length');
  if (delta === undefined || contentLength === undefined) {
    return undefined;
  }
  return {
    delta,
    contentLength,
    reset: readBoolean(record, false, 'streamReset', 'stream_reset'),
  };
}

export function normalizeCanonicalTurnItem(value: unknown): CanonicalTurnItem | undefined {
  const record = readRecord(value);
  if (!record) {
    return undefined;
  }
  const sessionId = readString(record, 'sessionId', 'session_id');
  const turnId = readString(record, 'turnId', 'turn_id');
  const itemId = readString(record, 'itemId', 'item_id');
  const kind = readItemKind(readString(record, 'kind'));
  const status = readStatus(readString(record, 'status'));
  const turnSeq = readNumber(record, 'turnSeq', 'turn_seq');
  const itemSeq = readNumber(record, 'itemSeq', 'item_seq');
  const createdAt = readNumber(record, 'createdAt', 'created_at');
  const updatedAt = readNumber(record, 'updatedAt', 'updated_at') ?? createdAt;
  const sourceThreadId = readString(record, 'sourceThreadId', 'source_thread_id');
  if (!sessionId || !turnId || !itemId || !kind || !status || turnSeq === undefined || itemSeq === undefined || createdAt === undefined || updatedAt === undefined || !sourceThreadId) {
    return undefined;
  }
  const title = readString(record, 'title') || undefined;
  const content = typeof record.content === 'string' ? record.content : undefined;
  const blocks = Array.isArray(record.blocks) ? record.blocks : undefined;
  const metadata = readRecord(record.metadata) || undefined;
  return {
    sessionId,
    turnId,
    turnSeq,
    itemId,
    itemSeq,
    kind,
    createdAt,
    status,
    ...(readNumber(record, 'itemVersion', 'item_version') !== undefined ? { itemVersion: readNumber(record, 'itemVersion', 'item_version') } : {}),
    updatedAt,
    ...(title ? { title } : {}),
    ...(content !== undefined ? { content } : {}),
    ...(blocks ? { blocks } : {}),
    ...(normalizeCanonicalToolCall(record.tool) ? { tool: normalizeCanonicalToolCall(record.tool) } : {}),
    ...(normalizeCanonicalWorkerRef(record.worker) ? { worker: normalizeCanonicalWorkerRef(record.worker) } : {}),
    sourceThreadId,
    visibility: normalizeCanonicalVisibility(record.visibility),
    ...(metadata ? { metadata } : {}),
  };
}

export function normalizeCanonicalTurn(value: unknown): CanonicalTurn | undefined {
  const record = readRecord(value);
  if (!record) {
    return undefined;
  }
  const sessionId = readString(record, 'sessionId', 'session_id');
  const turnId = readString(record, 'turnId', 'turn_id');
  const turnSeq = readNumber(record, 'turnSeq', 'turn_seq');
  const acceptedAt = readNumber(record, 'acceptedAt', 'accepted_at');
  const status = readStatus(readString(record, 'status'));
  if (!sessionId || !turnId || turnSeq === undefined || acceptedAt === undefined || !status) {
    return undefined;
  }
  const completedAt = readNumber(record, 'completedAt', 'completed_at');
  const responseDurationMs = readNumber(record, 'responseDurationMs', 'response_duration_ms');
  const metadata = readRecord(record.metadata) || undefined;
  const items = Array.isArray(record.items)
    ? record.items
      .map(normalizeCanonicalTurnItem)
      .filter((item): item is CanonicalTurnItem => Boolean(item))
      .sort((left, right) => left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId))
    : [];
  return {
    sessionId,
    turnId,
    turnSeq,
    acceptedAt,
    ...(completedAt !== undefined ? { completedAt } : {}),
    status,
    ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
    ...(record.usage !== undefined ? { usage: record.usage } : {}),
    items,
    ...(metadata ? { metadata } : {}),
  };
}

export function parseCanonicalTurnEventPayload(
  payload: unknown,
  options: { eventId?: string; eventSeq?: number; occurredAt?: number } = {},
): CanonicalTurnEvent | undefined {
  const record = readRecord(payload);
  if (!record) {
    return undefined;
  }
  const schemaVersion = readString(record, 'canonical_schema_version', 'canonicalSchemaVersion', 'schemaVersion', 'schema_version');
  if (schemaVersion !== CANONICAL_TURN_SCHEMA_VERSION) {
    return undefined;
  }
  const turn = normalizeCanonicalTurn(record.canonical_turn ?? record.canonicalTurn ?? record.turn);
  const item = normalizeCanonicalTurnItem(record.canonical_item ?? record.canonicalItem ?? record.item);
  const kind = readEventKind(readString(record, 'canonical_event_kind', 'canonicalEventKind', 'kind'));
  const sessionId = turn?.sessionId || item?.sessionId || readString(record, 'sessionId', 'session_id');
  const turnId = turn?.turnId || item?.turnId || readString(record, 'turnId', 'turn_id');
  const turnSeq = turn?.turnSeq ?? item?.turnSeq ?? readNumber(record, 'turnSeq', 'turn_seq');
  if (!kind || !sessionId || !turnId || turnSeq === undefined) {
    return undefined;
  }
  const stream = normalizeCanonicalTurnStreamUpdate(record);
  return {
    schemaVersion: CANONICAL_TURN_SCHEMA_VERSION,
    eventId: options.eventId || readString(record, 'eventId', 'event_id') || `${sessionId}:${turnId}:${options.eventSeq ?? 0}`,
    eventSeq: typeof options.eventSeq === 'number' && Number.isFinite(options.eventSeq)
      ? Math.max(0, Math.floor(options.eventSeq))
      : (readNumber(record, 'eventSeq', 'event_seq') ?? 0),
    kind,
    sessionId,
    turnId,
    turnSeq,
    occurredAt: typeof options.occurredAt === 'number' && Number.isFinite(options.occurredAt)
      ? Math.floor(options.occurredAt)
      : (readNumber(record, 'occurredAt', 'occurred_at') ?? Date.now()),
    ...(turn ? { turn } : {}),
    ...(item ? { item } : {}),
    ...(stream ? { stream } : {}),
  };
}

export function isCanonicalTerminalStatus(status: CanonicalTurnStatus): boolean {
  return status === 'completed' || status === 'blocked' || status === 'failed' || status === 'cancelled';
}

export function canTransitionCanonicalStatus(
  current: CanonicalTurnStatus,
  next: CanonicalTurnStatus
): boolean {
  if (current === next) {
    return true;
  }
  if (current === 'pending') {
    return next === 'running'
      || next === 'completed'
      || next === 'blocked'
      || next === 'failed'
      || next === 'cancelled';
  }
  if (current === 'running') {
    return next === 'completed' || next === 'blocked' || next === 'failed' || next === 'cancelled';
  }
  return false;
}

export function validateCanonicalTurnUpdate(
  existing: CanonicalTurn,
  incoming: CanonicalTurn
): string | null {
  const immutableChecks: Array<[string, unknown, unknown]> = [
    ['sessionId', existing.sessionId, incoming.sessionId],
    ['turnId', existing.turnId, incoming.turnId],
    ['turnSeq', existing.turnSeq, incoming.turnSeq],
    ['acceptedAt', existing.acceptedAt, incoming.acceptedAt],
  ];

  for (const [field, left, right] of immutableChecks) {
    if (left !== right) {
      return `canonical turn ${incoming.turnId} attempted to change immutable field ${field}`;
    }
  }

  if (!canTransitionCanonicalStatus(existing.status, incoming.status)) {
    return `canonical turn ${incoming.turnId} illegal status transition: ${existing.status} -> ${incoming.status}`;
  }

  return null;
}

export function validateCanonicalTurnItemUpdate(
  existing: CanonicalTurnItem,
  incoming: CanonicalTurnItem
): string | null {
  const immutableChecks: Array<[string, unknown, unknown]> = [
    ['sessionId', existing.sessionId, incoming.sessionId],
    ['turnId', existing.turnId, incoming.turnId],
    ['turnSeq', existing.turnSeq, incoming.turnSeq],
    ['itemId', existing.itemId, incoming.itemId],
    ['itemSeq', existing.itemSeq, incoming.itemSeq],
    ['kind', existing.kind, incoming.kind],
    ['createdAt', existing.createdAt, incoming.createdAt],
    ['tool.callId', existing.tool?.callId, incoming.tool?.callId],
  ];

  for (const [field, left, right] of immutableChecks) {
    if (left !== right) {
      return `canonical turn item ${incoming.itemId} attempted to change immutable field ${field}`;
    }
  }

  if (!canTransitionCanonicalStatus(existing.status, incoming.status)) {
    return `canonical turn item ${incoming.itemId} illegal status transition: ${existing.status} -> ${incoming.status}`;
  }

  return null;
}
