/**
 * 消息处理共享工具函数
 *
 * 从 message-handler.ts 中提取的无状态/弱状态工具层，
 * 被 message-handler.ts 和 data-message-handlers.ts 共同使用，
 * 用于打破两者之间的循环依赖。
 */

import {
  updateWorkerWaitResults,
  setRetryRuntime,
  clearRetryRuntime,
} from '../stores/messages.svelte';
import type { Message, ContentBlock, ToolCall, ThinkingBlock, RetryRuntimeState, WaitForWorkersResult, WaitForWorkersResultItem } from '../types/message';
import type { ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import { normalizeWorkerSlot } from './message-classifier';
import { normalizeMessagePayload } from './message-payload';
import {
  buildAssignmentTaskCardKey,
  buildWaitResultFromTaskCardMessage,
  resolveTaskCardKeyFromMetadata,
  resolveTaskCardScopeId,
} from './task-card-runtime';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';

// ===== 常量 =====
export type WorkerSlot = 'claude' | 'codex' | 'gemini';

export function safeParseJson(value?: string): Record<string, unknown> | null {
  if (!value || typeof value !== 'string') return null;
  try {
    return JSON.parse(value) as Record<string, unknown>;
  } catch {
    return null;
  }
}

// ===== 消息标准化 =====
export function normalizeRestoredMessages(messages: Message[]): Message[] {
  const seen = new Set<string>();
  const normalized: Message[] = [];
  for (const msg of ensureArray<Message>(messages)) {
    const next = normalizeMessagePayload(msg, '[MessageHandler] 恢复消息');
    const rawId = next.id;
    if (!rawId) {
      throw new Error('[MessageHandler] 恢复消息缺少 id');
    }
    if (seen.has(rawId)) {
      throw new Error(`[MessageHandler] 恢复消息 id 重复: ${rawId}`);
    }
    seen.add(rawId);
    normalized.push(next);
  }
  return normalized;
}

const WAIT_RESULT_STATUS_SET = new Set<WaitForWorkersResultItem['status']>([
  'completed',
  'failed',
  'skipped',
  'cancelled',
]);
const WAIT_RESULT_WAIT_STATUS_SET = new Set<WaitForWorkersResult['wait_status']>([
  'completed',
  'timeout',
]);

function normalizeWaitResultItem(raw: Record<string, unknown>): WaitForWorkersResultItem | null {
  const taskId = typeof raw.task_id === 'string' ? raw.task_id.trim() : '';
  const worker = typeof raw.worker === 'string' ? raw.worker.trim() : '';
  const statusRaw = typeof raw.status === 'string' ? raw.status.trim() : '';
  if (!taskId || !worker || !WAIT_RESULT_STATUS_SET.has(statusRaw as WaitForWorkersResultItem['status'])) {
    return null;
  }
  const summary = typeof raw.summary === 'string' ? raw.summary : '';
  const modifiedFiles = Array.isArray(raw.modified_files)
    ? raw.modified_files.filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
    : [];
  const errors = Array.isArray(raw.errors)
    ? raw.errors.filter((err): err is string => typeof err === 'string' && err.trim().length > 0)
    : undefined;
  return {
    task_id: taskId,
    worker,
    status: statusRaw as WaitForWorkersResultItem['status'],
    summary,
    modified_files: modifiedFiles,
    ...(errors && errors.length > 0 ? { errors } : {}),
  };
}

function parseWaitForWorkersPayload(raw: unknown, timestamp: number): WaitForWorkersResult | null {
  if (!raw) return null;
  const payload = typeof raw === 'string' ? safeParseJson(raw) : (raw as Record<string, unknown>);
  if (!payload || typeof payload !== 'object') return null;
  const waitStatusRaw = typeof payload.wait_status === 'string' ? payload.wait_status.trim() : '';
  if (!WAIT_RESULT_WAIT_STATUS_SET.has(waitStatusRaw as WaitForWorkersResult['wait_status'])) {
    return null;
  }
  const results = ensureArray(payload.results)
    .filter((item): item is Record<string, unknown> => !!item && typeof item === 'object')
    .map(normalizeWaitResultItem)
    .filter((item): item is WaitForWorkersResultItem => !!item);
  const pendingTaskIds = ensureArray(payload.pending_task_ids)
    .filter((item): item is string => typeof item === 'string' && item.trim().length > 0);
  const waitedMs = typeof payload.waited_ms === 'number' && Number.isFinite(payload.waited_ms)
    ? payload.waited_ms
    : 0;
  return {
    results,
    wait_status: waitStatusRaw as WaitForWorkersResult['wait_status'],
    timed_out: Boolean(payload.timed_out),
    pending_task_ids: pendingTaskIds,
    waited_ms: waitedMs,
    audit: payload.audit,
    updatedAt: timestamp,
  };
}

function extractWaitForWorkersPayloadFromMessage(message: Message): WaitForWorkersResult | null {
  const blocks = ensureArray(message.blocks);
  if (blocks.length === 0) return null;
  for (const block of blocks as any[]) {
    if (block.type !== 'tool_call' || !block.toolCall) continue;
    if (block.toolCall.name !== 'worker_wait') continue;
    const rawPayload = block.toolCall.result ?? block.toolCall.standardized?.data;
    const parsed = parseWaitForWorkersPayload(rawPayload, message.timestamp || Date.now());
    if (parsed) {
      return parsed;
    }
  }
  return null;
}

export function syncWorkerWaitResultsFromMessage(message: Message): void {
  const payload = extractWaitForWorkersPayloadFromMessage(message);
  if (payload && payload.results.length > 0) {
    const updates: Record<string, WaitForWorkersResult> = {};
    const updatedAt = typeof payload.updatedAt === 'number' ? payload.updatedAt : (message.timestamp || Date.now());
    const scopeId = resolveTaskCardScopeId(message.metadata as Record<string, unknown> | undefined);
    const grouped = new Map<string, WaitForWorkersResultItem[]>();
    for (const result of payload.results) {
      const taskId = typeof result.task_id === 'string' ? result.task_id.trim() : '';
      if (!taskId) continue;
      const cardKey = buildAssignmentTaskCardKey(taskId, scopeId);
      const list = grouped.get(cardKey) || [];
      list.push(result);
      grouped.set(cardKey, list);
    }
    for (const [cardKey, results] of grouped.entries()) {
      updates[cardKey] = {
        ...payload,
        results,
        updatedAt,
      };
    }
    if (Object.keys(updates).length > 0) {
      updateWorkerWaitResults(updates);
    }
  }

  const taskCardResult = buildWaitResultFromTaskCardMessage(message);
  if (taskCardResult) {
    updateWorkerWaitResults({ [taskCardResult.cardKey]: taskCardResult.result });
  }
}

export function handleRetryRuntimePayload(payload: Record<string, unknown>): void {
  const messageId = typeof payload.messageId === 'string' ? payload.messageId.trim() : '';
  if (!messageId) {
    return;
  }

  const phase = payload.phase;
  if (phase === 'settled') {
    clearRetryRuntime(messageId);
    return;
  }

  const attempt = typeof payload.attempt === 'number' && Number.isFinite(payload.attempt)
    ? payload.attempt
    : 0;
  const maxAttempts = typeof payload.maxAttempts === 'number' && Number.isFinite(payload.maxAttempts)
    ? payload.maxAttempts
    : 0;
  if (attempt <= 0 || maxAttempts <= 0) {
    return;
  }

  if (phase === 'attempt_started') {
    const runtime: RetryRuntimeState = {
      phase,
      attempt,
      maxAttempts,
    };
    setRetryRuntime(messageId, runtime);
    return;
  }

  if (phase !== 'scheduled') {
    return;
  }

  const delayMs = typeof payload.delayMs === 'number' && Number.isFinite(payload.delayMs)
    ? Math.max(0, payload.delayMs)
    : 0;
  const nextRetryAt = typeof payload.nextRetryAt === 'number' && Number.isFinite(payload.nextRetryAt)
    ? payload.nextRetryAt
    : Date.now() + delayMs;

  setRetryRuntime(messageId, {
    phase,
    attempt,
    maxAttempts,
    delayMs,
    nextRetryAt,
  });
}

export function resolveWorkerSlotFromMessage(message: Message): WorkerSlot | null {
  // task_card: 从 subTaskCard.worker / metadata.assignedWorker / metadata.worker 解析
  if (message.type === 'task_card') {
    const metadata = (message.metadata || {}) as Record<string, unknown>;
    const subTaskCard = (metadata.subTaskCard || {}) as Record<string, unknown>;
    return normalizeWorkerSlot(subTaskCard.worker)
      || normalizeWorkerSlot(metadata.assignedWorker)
      || normalizeWorkerSlot(metadata.worker);
  }
  // 其他消息: 从 source / metadata.worker / metadata.agent 解析
  const worker = normalizeWorkerSlot(message.source)
    || normalizeWorkerSlot((message.metadata as Record<string, unknown> | undefined)?.worker)
    || normalizeWorkerSlot((message.metadata as Record<string, unknown> | undefined)?.agent);
  return worker;
}

/**
 * 会话恢复时从已有消息重建 workerWaitResults。
 * workerWaitResults 是运行时状态（不持久化），恢复后需要从 task_card 消息和
 * 包含 worker_wait 结果的消息中重建，否则卡片完成态丢失。
 */
export function rebuildWorkerWaitResultsFromMessages(
  threadMessages: Message[],
  workerMessages: { claude: Message[]; codex: Message[]; gemini: Message[] }
): void {
  const allMessages = [
    ...threadMessages,
    ...workerMessages.claude,
    ...workerMessages.codex,
    ...workerMessages.gemini,
  ];
  for (const message of allMessages) {
    if (
      message.type === 'task_card'
      || message.type === 'instruction'
      || ensureArray(message.blocks).some((b: any) => b?.toolCall?.name === 'worker_wait')
    ) {
      syncWorkerWaitResultsFromMessage(message);
    }
  }
}

export function mapStandardBlocks(blocks: StandardContentBlock[]): ContentBlock[] {
  const list = ensureArray<StandardContentBlock>(blocks);
  const invalid = list.filter((block) => !block || typeof block !== 'object' || !('type' in block));
  if (invalid.length > 0) {
    throw new Error('[MessageHandler] 标准消息块无效');
  }
  return list.map((block) => {
    switch (block.type) {
      case 'text':
        return {
          type: 'text',
          content: typeof block.content === 'string' ? block.content : '',
        };
      case 'code':
        return {
          type: 'code',
          content: typeof block.content === 'string' ? block.content : '',
          language: typeof block.language === 'string' ? block.language : undefined,
        };
      case 'thinking': {
        const blockId = typeof block.blockId === 'string' ? block.blockId : undefined;
        const thinking: ThinkingBlock = {
          content: typeof block.content === 'string' ? block.content : '',
          isComplete: true,
          summary: typeof block.summary === 'string' ? block.summary : undefined,
          blockId,
        };
        return {
          ...(blockId ? { id: blockId } : {}),
          type: 'thinking',
          content: typeof block.content === 'string' ? block.content : '',
          thinking,
        };
      }
      case 'tool_call': {
        const toolStatus = mapToolStatus(
          block.status,
          block.standardized?.status,
          block.output,
          block.error
        );
        const standardizedStatus = (block.standardized?.status || '').toLowerCase();
        const standardizedHardError = standardizedStatus === 'error'
          || standardizedStatus === 'timeout'
          || standardizedStatus === 'killed';
        const standardizedError = block.standardized
          && standardizedHardError
          ? (block.standardized.message || undefined)
          : undefined;
        const toolCall: ToolCall = {
          id: block.toolId,
          name: block.toolName,
          arguments: safeParseJson(block.input) || {},
          status: toolStatus,
          result: block.output,
          error: block.error || standardizedError,
          standardized: block.standardized,
        };
        return {
          type: 'tool_call',
          content: '',
          toolCall,
        };
      }
      case 'file_change':
        return {
          type: 'file_change',
          content: '',
          fileChange: {
            filePath: block.filePath,
            changeType: block.changeType,
            additions: block.additions,
            deletions: block.deletions,
            diff: block.diff,
          },
        };
      case 'plan':
        return {
          type: 'plan',
          content: '',
          plan: {
            goal: block.goal,
            analysis: block.analysis,
            constraints: block.constraints,
            acceptanceCriteria: block.acceptanceCriteria,
            riskLevel: block.riskLevel,
            riskFactors: block.riskFactors,
            rawJson: block.rawJson,
          },
        };
      default:
        throw new Error(`[MessageHandler] 未支持的标准消息块类型: ${(block as { type: string }).type}`);
    }
  });
}


function mapToolStatus(
  status: string | undefined,
  standardizedStatus?: string,
  output?: string,
  error?: string,
): ToolCall['status'] {
  switch (status) {
    case 'pending':
      return 'pending';
    case 'running':
      return 'running';
    case 'success':
      return 'success';
    case 'error':
      return 'error';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
  }

  if (standardizedStatus) {
    switch (standardizedStatus.toLowerCase()) {
      case 'success':
        return 'success';
      case 'error':
      case 'timeout':
      case 'killed':
        return 'error';
      case 'blocked':
      case 'rejected':
      case 'aborted':
        return 'success';
      default:
        break;
    }
  }

  if (error) return 'error';
  if (output) return 'success';
  return 'running';
}

export function formatPlanBlock(block: any): string {
  const parts: string[] = [];
  if (block.goal) parts.push(i18n.t('messageHandler.planGoal', { goal: block.goal }));
  if (block.analysis) parts.push(i18n.t('messageHandler.planAnalysis', { analysis: block.analysis }));
  if (Array.isArray(block.constraints) && block.constraints.length > 0) {
    parts.push(`${i18n.t('messageHandler.planConstraints')}\n- ${block.constraints.join('\n- ')}`);
  }
  if (Array.isArray(block.acceptanceCriteria) && block.acceptanceCriteria.length > 0) {
    parts.push(`${i18n.t('messageHandler.planAcceptanceCriteria')}\n- ${block.acceptanceCriteria.join('\n- ')}`);
  }
  if (block.riskLevel) parts.push(i18n.t('messageHandler.planRiskLevel', { riskLevel: block.riskLevel }));
  if (Array.isArray(block.riskFactors) && block.riskFactors.length > 0) {
    parts.push(`${i18n.t('messageHandler.planRiskFactors')}\n- ${block.riskFactors.join('\n- ')}`);
  }
  return parts.join('\n\n');
}
