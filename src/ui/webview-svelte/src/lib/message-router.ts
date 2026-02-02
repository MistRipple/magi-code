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
  return messageTargetMap.get(messageId) || null;
}

export function clearMessageTargets(): void {
  messageTargetMap.clear();
}

export function clearMessageTarget(messageId: string): void {
  messageTargetMap.delete(messageId);
}
