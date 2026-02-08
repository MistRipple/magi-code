import type { DisplayTarget } from '../types/message-routing';
import { MessageCategory } from '../types/message-routing';

export const ROUTING_TABLE: Record<MessageCategory, DisplayTarget | { location: 'worker' | 'both' | 'thread' | 'task' | 'none' }> = {
  [MessageCategory.USER_INPUT]: { location: 'thread' },
  [MessageCategory.ORCHESTRATOR_RESPONSE]: { location: 'thread' },
  [MessageCategory.ORCHESTRATOR_PLAN]: { location: 'thread' },
  [MessageCategory.ORCHESTRATOR_SUMMARY]: { location: 'thread' },
  [MessageCategory.ORCHESTRATOR_THINKING]: { location: 'thread' },
  [MessageCategory.ORCHESTRATOR_TOOL_USE]: { location: 'thread' },  // 编排者工具调用显示在主对话区
  [MessageCategory.WORKER_INSTRUCTION]: { location: 'worker' },
  [MessageCategory.WORKER_THINKING]: { location: 'worker' },
  [MessageCategory.WORKER_OUTPUT]: { location: 'worker' },
  [MessageCategory.WORKER_TOOL_USE]: { location: 'worker' },
  [MessageCategory.WORKER_CODE]: { location: 'worker' },
  [MessageCategory.WORKER_SUMMARY]: { location: 'both' },
  [MessageCategory.SYSTEM_NOTICE]: { location: 'thread' },
  [MessageCategory.SYSTEM_PHASE]: { location: 'none' },
  [MessageCategory.SYSTEM_ERROR]: { location: 'thread' },
  [MessageCategory.INTERACTION_CONFIRMATION]: { location: 'thread' },
  [MessageCategory.INTERACTION_QUESTION]: { location: 'thread' },
  [MessageCategory.INTERACTION_TOOL_AUTH]: { location: 'thread' },
  [MessageCategory.TASK_SUMMARY_CARD]: { location: 'thread' },
  [MessageCategory.PROGRESS_UPDATE]: { location: 'thread' },
};

export function resolveDisplayTarget(
  category: MessageCategory,
  worker?: 'claude' | 'codex' | 'gemini'
): DisplayTarget {
  const rule = ROUTING_TABLE[category];
  if (!rule) {
    return { location: 'thread' };
  }

  if (rule.location === 'worker' || rule.location === 'both') {
    if (worker) {
      return { location: rule.location, worker };
    }
    return { location: 'thread' };
  }

  if (rule.location === 'task' || rule.location === 'none' || rule.location === 'thread') {
    return rule as DisplayTarget;
  }

  return { location: 'thread' };
}
