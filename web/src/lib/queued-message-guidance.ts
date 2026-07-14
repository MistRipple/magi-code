import type { QueuedMessage } from '../types/message';

export function queuedMessageText(message: QueuedMessage): string {
  const text = typeof message.text === 'string' ? message.text : message.content;
  return typeof text === 'string' ? text.trim() : '';
}

export function canGuideQueuedMessage(message: QueuedMessage): boolean {
  return queuedMessageText(message).length > 0
    && !message.skillName
    && message.goalMode !== true
    && (!message.images || message.images.length === 0)
    && (!message.contextReferences || message.contextReferences.length === 0);
}
