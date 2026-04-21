import type { StandardMessage } from '../shared/protocol/message-protocol';
import { canBindRequestPlaceholder } from '../shared/request-placeholder-binding';

export function canStandardMessageTakeOverRequestPlaceholder(standard: StandardMessage): boolean {
  return canBindRequestPlaceholder({
    type: standard.type,
    source: standard.source,
    visibility: standard.visibility,
    metadata: standard.metadata as Record<string, unknown> | undefined,
  });
}

export function shouldTakeOverRequestPlaceholder(
  standard: StandardMessage,
  requestBinding?: { placeholderMessageId?: string; realMessageId?: string } | undefined,
): boolean {
  if (!canStandardMessageTakeOverRequestPlaceholder(standard)) {
    return false;
  }
  const placeholderMessageId = typeof requestBinding?.placeholderMessageId === 'string'
    ? requestBinding.placeholderMessageId.trim()
    : '';
  if (!placeholderMessageId || placeholderMessageId === standard.id) {
    return false;
  }
  const realMessageId = typeof requestBinding?.realMessageId === 'string'
    ? requestBinding.realMessageId.trim()
    : '';
  return !realMessageId || realMessageId === standard.id;
}
