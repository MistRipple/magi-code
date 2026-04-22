/**
 * 消息处理共享工具函数
 *
 * 从 message-handler.ts 中提取的无状态/弱状态工具层，
 * 被 message-handler.ts 和 data-message-handlers.ts 共同使用，
 * 用于打破两者之间的循环依赖。
 */

import {
  setRetryRuntime,
  clearRetryRuntime,
} from '../stores/messages.svelte';
import type { Message, ContentBlock, ToolCall, ThinkingBlock, RetryRuntimeState } from '../types/message';
import type { ContentBlock as StandardContentBlock } from '../shared/protocol/message-protocol';
import { normalizeMessagePayload } from './message-payload';
import { resolveTaskCardWorkerSlot } from './worker-role-utils';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';
import { resolveTimelineWorkerId } from '../shared/timeline-worker-lifecycle';

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

export function resolveWorkerSlotFromMessage(message: Message): string | null {
  if (message.type === 'task_card') {
    return resolveTaskCardWorkerSlot(message.metadata as Record<string, unknown> | undefined);
  }
  const worker = resolveTimelineWorkerId(
    message.metadata as Record<string, unknown> | undefined,
    { fallbacks: [message.source] },
  );
  return worker;
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
          durationMs: typeof block.duration === 'number' && Number.isFinite(block.duration)
            ? Math.max(0, Math.floor(block.duration))
            : undefined,
        };
        return {
          type: 'tool_call',
          content: '',
          toolCall,
        };
      }
      case 'tool_result': {
        const resolvedContent = (typeof block.content === 'string' ? block.content : '').trim()
          || (typeof block.standardized?.message === 'string' ? block.standardized.message.trim() : '');
        const toolStatus = mapToolResultStatus(block.isError, block.standardized?.status);
        const toolCall: ToolCall = {
          id: block.toolCallId,
          name: block.standardized?.toolName || 'tool_result',
          arguments: {},
          status: toolStatus,
          result: toolStatus === 'error' ? undefined : (resolvedContent || undefined),
          error: toolStatus === 'error' ? (resolvedContent || undefined) : undefined,
          standardized: block.standardized,
        };
        return {
          type: 'tool_result',
          content: resolvedContent,
          toolCall,
          fileChange: block.fileChange
            ? {
                filePath: block.fileChange.filePath,
                changeType: block.fileChange.changeType,
                additions: block.fileChange.additions,
                deletions: block.fileChange.deletions,
                diff: block.fileChange.diff,
              }
            : undefined,
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
      case 'dispatch_group':
        return {
          type: 'dispatch_group',
          content: '',
          blockId: block.blockId,
          dispatchWaveId: block.dispatchWaveId,
          status: block.status,
          summaryText: block.summaryText,
          lanes: Array.isArray(block.lanes)
            ? block.lanes.map((lane) => ({
                laneId: lane.laneId,
                laneVersion: lane.laneVersion,
                worker: lane.worker,
                title: lane.title,
                description: lane.description,
                status: lane.status,
                startedAt: lane.startedAt,
                endedAt: lane.endedAt,
                liveActivity: lane.liveActivity,
                toolUseCount: lane.toolUseCount,
                progressSummary: lane.progressSummary
                  ? {
                      completedTaskCount: lane.progressSummary.completedTaskCount,
                      totalTaskCount: lane.progressSummary.totalTaskCount,
                      blockedTaskCount: lane.progressSummary.blockedTaskCount,
                      awaitingApprovalTaskCount: lane.progressSummary.awaitingApprovalTaskCount,
                      reviewRequiredTaskCount: lane.progressSummary.reviewRequiredTaskCount,
                    }
                  : undefined,
                jumpTarget: lane.jumpTarget,
              }))
            : [],
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

function mapToolResultStatus(
  isError: boolean | undefined,
  standardizedStatus?: string,
): ToolCall['status'] {
  if (isError === true) {
    return 'error';
  }
  switch ((standardizedStatus || '').toLowerCase()) {
    case 'error':
    case 'timeout':
    case 'killed':
      return 'error';
    default:
      return 'success';
  }
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
