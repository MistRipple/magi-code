import type {
  Message,
  OrchestrationRuntimeTimelineEntry,
  TimelineRenderItem,
  ToolCall,
} from '../types/message';
import { parseModelFailureDiagnostic } from './model-failure';
import { mergeRuntimeTimelineEntries } from './runtime-timeline';
import { parseToolCallFailureDiagnostic } from './tool-call-failure';
import {
  parseToolPayloadRecord,
  publicToolPayloadMessage,
  toolPayloadErrorCode,
} from './tool-error-payload';

export interface ConversationRuntimeRecordOptions {
  isProcessing: boolean;
  processingStartedAt?: number | null;
}

export function buildConversationRuntimeRecords(
  renderItems: readonly TimelineRenderItem[],
  options: ConversationRuntimeRecordOptions,
): OrchestrationRuntimeTimelineEntry[] {
  const currentTurnMessages = resolveCurrentTurnMessages(renderItems);
  const records = currentTurnMessages.flatMap(buildMessageRuntimeRecords);
  if (records.length > 0 || !options.isProcessing) {
    return mergeRuntimeTimelineEntries(records);
  }

  const latestMessageAt = currentTurnMessages.reduce(
    (latest, message) => Math.max(latest, message.updatedAt || message.timestamp || 0),
    0,
  );
  return [{
    eventId: 'current-turn-processing',
    seq: 0,
    timestamp: latestMessageAt || options.processingStartedAt || Date.now(),
    type: 'session.turn.processing',
    summary: '',
    kind: 'progress',
    source: 'model',
    diffCount: 0,
  }];
}

export function resolveCurrentConversationTurnStartedAt(
  renderItems: readonly TimelineRenderItem[],
): number | null {
  const currentTurnMessages = resolveCurrentTurnMessages(renderItems);
  if (currentTurnMessages.length === 0) return null;
  const timestamps = currentTurnMessages
    .map((message) => message.timestamp)
    .filter((timestamp) => Number.isFinite(timestamp) && timestamp > 0);
  return timestamps.length > 0 ? Math.min(...timestamps) : null;
}

function resolveCurrentTurnMessages(renderItems: readonly TimelineRenderItem[]): Message[] {
  let latestTurnSeq = Number.NEGATIVE_INFINITY;
  for (const item of renderItems) {
    const turnSeq = item.message.metadata?.turnSeq;
    if (typeof turnSeq === 'number' && Number.isFinite(turnSeq)) {
      latestTurnSeq = Math.max(latestTurnSeq, turnSeq);
    }
  }
  if (!Number.isFinite(latestTurnSeq)) return [];
  return renderItems
    .map((item) => item.message)
    .filter((message) => message.metadata?.turnSeq === latestTurnSeq);
}

function buildMessageRuntimeRecords(message: Message): OrchestrationRuntimeTimelineEntry[] {
  const interruptionSource = textValue(message.metadata?.interruptionSource);
  if (
    message.role === 'user'
    && message.metadata?.turnStatus === 'cancelled'
    && interruptionSource
  ) {
    return [buildRuntimeRecord(message, {
      suffix: 'interrupted',
      type: 'session.turn.interrupted',
      summary: '',
      kind: interruptionSource === 'user' ? 'warning' : 'error',
      source: 'runtime',
      detail: `interruptionSource: ${interruptionSource}`,
    })];
  }

  const modelFailure = parseModelFailureDiagnostic(message.metadata?.modelFailure);
  if (modelFailure) {
    return [buildRuntimeRecord(message, {
      suffix: 'model-failure',
      type: 'session.model.failed',
      summary: modelFailure.summary,
      kind: 'error',
      source: 'model',
      detail: detailWithCode(modelFailure.code, modelFailure.detail),
    })];
  }

  const toolFailure = parseToolCallFailureDiagnostic(message.metadata?.toolCallFailure);
  if (toolFailure) {
    return [buildRuntimeRecord(message, {
      suffix: 'tool-validation-failure',
      type: 'session.tool.failed',
      summary: toolFailure.summary,
      kind: 'error',
      source: toolFailure.toolName,
      detail: detailWithCode(toolFailure.code, toolFailure.detail),
    })];
  }

  if (message.metadata?.noticeKind === 'session_interrupted') {
    const detail = textValue(message.metadata.failureDetail)
      || textValue(message.metadata.errorDetail)
      || textValue(message.metadata.error)
      || (interruptionSource ? `interruptionSource: ${interruptionSource}` : '');
    return [buildRuntimeRecord(message, {
      suffix: 'interrupted',
      type: 'session.turn.interrupted',
      summary: message.content,
      kind: interruptionSource === 'user' ? 'warning' : 'error',
      source: 'runtime',
      detail: detail || undefined,
    })];
  }

  const embeddedToolCalls = (message.blocks || [])
    .map((block) => block.type === 'tool_call' ? block.toolCall : undefined)
    .filter((toolCall): toolCall is ToolCall => Boolean(toolCall));
  if (embeddedToolCalls.length > 0) {
    return embeddedToolCalls
      .filter((toolCall) => !isUndiagnosedToolFailure(toolCall))
      .map((toolCall) => buildToolRuntimeRecord(message, toolCall));
  }

  if (message.metadata?.turnItemKind === 'tool_call') {
    const toolName = textValue(message.metadata.toolName) || message.content || 'tool';
    const status = canonicalToolStatus(message.metadata.turnItemStatus);
    return [buildToolRuntimeRecord(message, {
      id: textValue(message.metadata.toolCallId) || message.id,
      name: toolName,
      arguments: {},
      status,
    })];
  }

  if (message.type === 'error' || message.noticeType === 'error') {
    return [buildRuntimeRecord(message, {
      suffix: 'error',
      type: 'session.turn.failed',
      summary: message.content,
      kind: 'error',
      source: 'runtime',
    })];
  }
  return [];
}

function buildToolRuntimeRecord(
  message: Message,
  toolCall: ToolCall,
): OrchestrationRuntimeTimelineEntry {
  const status = toolCall.status;
  const type = status === 'error'
    ? 'session.tool.failed'
    : status === 'success'
      ? 'session.tool.succeeded'
      : 'session.tool.running';
  const detail = resolveToolRuntimeDetail(toolCall);
  return buildRuntimeRecord(message, {
    suffix: `tool-${toolCall.id}`,
    type,
    summary: resolveToolRuntimeSummary(toolCall) || toolCall.name,
    kind: status === 'error' ? 'error' : status === 'success' ? 'success' : 'progress',
    source: toolCall.name,
    detail: detail || undefined,
  });
}

function resolveToolRuntimeSummary(toolCall: ToolCall): string {
  if (toolCall.status === 'error') {
    return publicToolPayloadMessage(toolCall.error)
      || publicToolPayloadMessage(toolCall.result)
      || textValue(toolCall.standardized?.message)
      || textValue(toolCall.error)
      || textValue(toolCall.result);
  }
  if (toolCall.status === 'success') {
    return textValue(toolCall.standardized?.message)
      || structuredToolResultSummary(toolCall.result);
  }
  return textValue(toolCall.arguments.command)
    || textValue(toolCall.arguments.path)
    || textValue(toolCall.arguments.file_path);
}

function structuredToolResultSummary(value: unknown): string {
  const payload = parseToolPayloadRecord(value);
  if (!payload) return '';
  return textValue(payload.summary) || textValue(payload.message);
}

function resolveToolRuntimeDetail(toolCall: ToolCall): string {
  if (toolCall.status === 'error') {
    const rawError = publicToolPayloadMessage(toolCall.error)
      || publicToolPayloadMessage(toolCall.result)
      || textValue(toolCall.standardized?.message)
      || textValue(toolCall.error)
      || textValue(toolCall.result);
    const code = textValue(toolCall.standardized?.errorCode)
      || toolPayloadErrorCode(toolCall.error)
      || toolPayloadErrorCode(toolCall.result);
    return code && rawError ? detailWithCode(code, rawError) : rawError;
  }
  if (toolCall.status === 'success') {
    return compactSuccessDetail(
      textValue(toolCall.standardized?.message) || textValue(toolCall.result),
    );
  }
  return textValue(toolCall.arguments.command)
    || textValue(toolCall.arguments.path)
    || textValue(toolCall.arguments.file_path);
}

function compactSuccessDetail(value: string): string {
  if (!value) return '';
  const normalized = value.replace(/\r\n/g, '\n').trim();
  if (normalized.length <= 800) return normalized;
  return `${normalized.slice(0, 800).trimEnd()}\n…`;
}

function canonicalToolStatus(value: unknown): ToolCall['status'] {
  switch (value) {
    case 'completed': return 'success';
    case 'blocked':
    case 'failed':
    case 'cancelled': return 'error';
    case 'running': return 'running';
    default: return 'pending';
  }
}

function isUndiagnosedToolFailure(toolCall: ToolCall): boolean {
  return toolCall.status === 'error'
    && !textValue(toolCall.error)
    && !textValue(toolCall.standardized?.message)
    && !publicToolPayloadMessage(toolCall.result);
}

function buildRuntimeRecord(
  message: Message,
  record: {
    suffix: string;
    type: string;
    summary: string;
    kind: NonNullable<OrchestrationRuntimeTimelineEntry['kind']>;
    source: string;
    detail?: string;
  },
): OrchestrationRuntimeTimelineEntry {
  const rawSeq = message.metadata?.eventSeq ?? message.metadata?.canonicalItemSeq;
  return {
    eventId: `${message.id}:${record.suffix}`,
    seq: typeof rawSeq === 'number' && Number.isFinite(rawSeq) ? rawSeq : 0,
    timestamp: message.updatedAt || message.timestamp || 0,
    type: record.type,
    summary: record.summary,
    kind: record.kind,
    source: record.source,
    detail: record.detail,
    diffCount: 0,
  };
}

function detailWithCode(code: string, detail: string): string {
  return detail.toLowerCase().startsWith(`${code.toLowerCase()}:`)
    ? detail
    : `${code}: ${detail}`;
}

function textValue(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}
