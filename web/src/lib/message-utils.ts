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
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';

export function safeParseJson(value?: string): Record<string, unknown> | null {
  if (!value || typeof value !== 'string') return null;
  try {
    const parsed = JSON.parse(value) as unknown;
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : { raw: parsed };
  } catch {
    return { raw: value };
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

export function mapStandardBlocks(blocks: StandardContentBlock[]): ContentBlock[] {
  const list = ensureArray<StandardContentBlock>(blocks);
  const invalid = list.filter((block) => !block || typeof block !== 'object' || !('type' in block));
  if (invalid.length > 0) {
    throw new Error('[MessageHandler] 标准消息块无效');
  }
  return list.map((block) => {
    switch (block.type) {
      case 'text': {
        const blockId = requireBlockId(block, 'text');
        return {
          id: blockId,
          type: 'text',
          content: typeof block.content === 'string' ? block.content : '',
        };
      }
      case 'code': {
        const blockId = requireBlockId(block, 'code');
        return {
          id: blockId,
          type: 'code',
          content: typeof block.content === 'string' ? block.content : '',
          language: typeof block.language === 'string' ? block.language : undefined,
        };
      }
      case 'thinking': {
        const blockId = requireBlockId(block, 'thinking');
        const status = block.isComplete === false ? 'running' : 'completed';
        const thinking: ThinkingBlock = {
          groupId: `thinking-group:${blockId}`,
          segments: [{
            segmentId: blockId,
            messageId: blockId,
            content: typeof block.content === 'string' ? block.content : '',
            summary: typeof block.summary === 'string' ? block.summary : undefined,
            status,
          }],
          status,
          isStreaming: status === 'running',
        };
        return {
          id: blockId,
          type: 'thinking',
          content: '',
          thinking,
        };
      }
      case 'tool_call': {
        const toolId = requireToolCallBlockId(block.toolId, 'tool_call');
        const toolStatus = mapToolStatus(
          block.status,
          block.standardized?.status,
          block.output,
          block.error
        );
        const standardizedStatus = (block.standardized?.status || '').toLowerCase();
        const standardizedHardError = standardizedStatus === 'error'
          || standardizedStatus === 'timeout'
          || standardizedStatus === 'killed'
          || standardizedStatus === 'blocked'
          || standardizedStatus === 'rejected'
          || standardizedStatus === 'aborted';
        const standardizedError = block.standardized
          && standardizedHardError
          ? (block.standardized.message || undefined)
          : undefined;
        const resolvedError = block.error || standardizedError || (toolStatus === 'error' ? block.output : undefined);
        const toolCall: ToolCall = {
          id: toolId,
          name: block.toolName,
          arguments: safeParseJson(block.input) || {},
          status: toolStatus,
          result: toolStatus === 'error' ? undefined : block.output,
          error: resolvedError,
          standardized: block.standardized,
          durationMs: typeof block.duration === 'number' && Number.isFinite(block.duration)
            ? Math.max(0, Math.floor(block.duration))
            : undefined,
        };
        return {
          id: `tool_call:${toolId}`,
          type: 'tool_call',
          content: '',
          toolCall,
        };
      }
      case 'tool_result': {
        const toolCallId = requireToolCallBlockId(block.toolCallId, 'tool_result');
        const resolvedContent = (typeof block.content === 'string' ? block.content : '').trim()
          || (typeof block.standardized?.message === 'string' ? block.standardized.message.trim() : '');
        const toolStatus = mapToolResultStatus(block.isError, block.standardized?.status);
        const toolCall: ToolCall = {
          id: toolCallId,
          name: block.standardized?.toolName || 'tool_result',
          arguments: safeParseJson(block.input) || {},
          status: toolStatus,
          result: toolStatus === 'error' ? undefined : (resolvedContent || undefined),
          error: toolStatus === 'error' ? (resolvedContent || undefined) : undefined,
          standardized: block.standardized,
        };
        return {
          id: `tool_result:${toolCallId}`,
          type: 'tool_call',
          content: resolvedContent,
          toolCall,
          fileChange: block.fileChange
            ? {
                sessionId: block.fileChange.sessionId,
                workspaceId: block.fileChange.workspaceId,
                workspacePath: block.fileChange.workspacePath,
                filePath: block.fileChange.filePath,
                oldPath: block.fileChange.oldPath,
                changeType: block.fileChange.changeType,
                additions: block.fileChange.additions,
                deletions: block.fileChange.deletions,
                diff: block.fileChange.diff,
                contentKind: block.fileChange.contentKind,
                size: block.fileChange.size,
                mime: block.fileChange.mime,
                error: block.fileChange.error,
                symlinkTarget: block.fileChange.symlinkTarget,
                headSummary: block.fileChange.headSummary,
                tailSummary: block.fileChange.tailSummary,
                toolCallId: block.fileChange.toolCallId,
              }
            : undefined,
        };
      }
      case 'file_change': {
        const filePath = requireFileChangePath(block.filePath);
        return {
          id: `file_change:${filePath}`,
          type: 'file_change',
          content: '',
          fileChange: {
            sessionId: block.sessionId,
            workspaceId: block.workspaceId,
            workspacePath: block.workspacePath,
            filePath,
            oldPath: block.oldPath,
            changeType: block.changeType,
            additions: block.additions,
            deletions: block.deletions,
            diff: block.diff,
            contentKind: block.contentKind,
            size: block.size,
            mime: block.mime,
            error: block.error,
            symlinkTarget: block.symlinkTarget,
            headSummary: block.headSummary,
            tailSummary: block.tailSummary,
            toolCallId: block.toolCallId,
          },
        };
      }
      case 'plan': {
        const blockId = requireBlockId(block, 'plan');
        return {
          id: blockId,
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
      }
      default:
        throw new Error(`[MessageHandler] 未支持的标准消息块类型: ${(block as { type: string }).type}`);
    }
  });
}

function requireBlockId(
  block: { blockId?: unknown },
  kind: string,
): string {
  const value = typeof block.blockId === 'string' ? block.blockId.trim() : '';
  if (!value) {
    throw new Error(`[MessageHandler] ${kind} block 缺少 blockId`);
  }
  return value;
}

function requireToolCallBlockId(value: string | undefined, kind: string): string {
  const trimmed = typeof value === 'string' ? value.trim() : '';
  if (!trimmed) {
    throw new Error(`[MessageHandler] ${kind} block 缺少 toolCallId`);
  }
  return trimmed;
}

function requireFileChangePath(value: string | undefined): string {
  const trimmed = typeof value === 'string' ? value.trim() : '';
  if (!trimmed) {
    throw new Error('[MessageHandler] file_change block 缺少 filePath');
  }
  return trimmed;
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
        return 'error';
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
    case 'blocked':
    case 'rejected':
    case 'aborted':
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
