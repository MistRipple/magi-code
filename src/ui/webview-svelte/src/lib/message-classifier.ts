import type { StandardMessage } from '../../../../protocol/message-protocol';
import { MessageType } from '../../../../protocol/message-protocol';
import { MessageCategory } from '../types/message-routing';
import { InteractionType } from '../../../../protocol/message-protocol';

const WORKER_SLOTS = new Set(['claude', 'codex', 'gemini']);

export function normalizeWorkerSlot(value: unknown): 'claude' | 'codex' | 'gemini' | null {
  if (!value || typeof value !== 'string') return null;
  const lower = value.toLowerCase().trim();
  if (WORKER_SLOTS.has(lower)) return lower as 'claude' | 'codex' | 'gemini';
  return null;
}

export function classifyMessage(standard: StandardMessage): {
  category: MessageCategory;
  worker?: 'claude' | 'codex' | 'gemini';
} {
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const resolvedWorker = normalizeWorkerSlot(standard.agent) ?? normalizeWorkerSlot(meta?.worker);
  const dispatchToWorker = Boolean(meta?.dispatchToWorker);
  const hasSummaryCard = Boolean(meta?.subTaskCard);
  const isStatusMessage = Boolean(meta?.isStatusMessage);

  if (isStatusMessage && standard.type === MessageType.PROGRESS) {
    return { category: MessageCategory.SYSTEM_PHASE };
  }

  if (hasSummaryCard && standard.source === 'worker') {
    return { category: MessageCategory.TASK_SUMMARY_CARD, worker: resolvedWorker ?? undefined };
  }

  if (standard.source === 'orchestrator') {
    if (dispatchToWorker && resolvedWorker) {
      return { category: MessageCategory.WORKER_INSTRUCTION, worker: resolvedWorker };
    }
    if (standard.type === MessageType.INTERACTION && standard.interaction) {
      switch (standard.interaction.type) {
        case InteractionType.PLAN_CONFIRMATION:
          return { category: MessageCategory.INTERACTION_CONFIRMATION };
        case InteractionType.PERMISSION:
          return { category: MessageCategory.INTERACTION_TOOL_AUTH };
        case InteractionType.QUESTION:
        case InteractionType.CLARIFICATION:
          return { category: MessageCategory.INTERACTION_QUESTION };
        default:
          return { category: MessageCategory.SYSTEM_NOTICE };
      }
    }

    switch (standard.type) {
      case MessageType.PLAN:
        return { category: MessageCategory.ORCHESTRATOR_PLAN };
      case MessageType.THINKING:
        return { category: MessageCategory.ORCHESTRATOR_THINKING };
      case MessageType.RESULT:
        return { category: MessageCategory.ORCHESTRATOR_SUMMARY };
      case MessageType.PROGRESS:
        return { category: MessageCategory.PROGRESS_UPDATE };
      case MessageType.ERROR:
        return { category: MessageCategory.SYSTEM_ERROR };
      case MessageType.SYSTEM:
        return { category: MessageCategory.SYSTEM_NOTICE };
      case MessageType.TEXT:
      default:
        return { category: MessageCategory.ORCHESTRATOR_ANALYSIS };
    }
  }

  if (standard.source === 'worker') {
    switch (standard.type) {
      case MessageType.THINKING:
        return { category: MessageCategory.WORKER_THINKING, worker: resolvedWorker ?? undefined };
      case MessageType.TOOL_CALL:
        return { category: MessageCategory.WORKER_TOOL_USE, worker: resolvedWorker ?? undefined };
      case MessageType.ERROR:
        return { category: MessageCategory.SYSTEM_ERROR, worker: resolvedWorker ?? undefined };
      case MessageType.RESULT:
      case MessageType.PROGRESS:
      case MessageType.TEXT:
      default:
        return { category: MessageCategory.WORKER_OUTPUT, worker: resolvedWorker ?? undefined };
    }
  }

  return { category: MessageCategory.SYSTEM_NOTICE };
}
