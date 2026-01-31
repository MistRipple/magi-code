import { getState } from '../stores/messages.svelte';
import type { DisplayTarget } from '../types/message-routing';
import { classifyMessage } from './message-classifier';
import { resolveDisplayTarget } from '../config/routing-table';
import type { StandardMessage } from '../../../../protocol/message-protocol';

const messageTargetMap = new Map<string, DisplayTarget>();

export function routeStandardMessage(standard: StandardMessage): DisplayTarget {
  const { category, worker } = classifyMessage(standard);
  const target = resolveDisplayTarget(category, worker);
  messageTargetMap.set(standard.id, target);
  return target;
}

export function getMessageTarget(messageId: string): DisplayTarget | null {
  const cached = messageTargetMap.get(messageId);
  if (cached) return cached;

  const state = getState();
  const inThread = state.threadMessages.some(m => m.id === messageId);
  const agents: Array<'claude' | 'codex' | 'gemini'> = ['claude', 'codex', 'gemini'];
  let workerMatch: 'claude' | 'codex' | 'gemini' | null = null;
  for (const agent of agents) {
    if (state.agentOutputs[agent].some(m => m.id === messageId)) {
      workerMatch = agent;
      break;
    }
  }

  if (inThread && workerMatch) {
    const target: DisplayTarget = { location: 'both', worker: workerMatch };
    messageTargetMap.set(messageId, target);
    return target;
  }

  if (inThread) {
    const target: DisplayTarget = { location: 'thread' };
    messageTargetMap.set(messageId, target);
    return target;
  }

  if (workerMatch) {
    const target: DisplayTarget = { location: 'worker', worker: workerMatch };
    messageTargetMap.set(messageId, target);
    return target;
  }

  return null;
}

export function clearMessageTargets(): void {
  messageTargetMap.clear();
}
