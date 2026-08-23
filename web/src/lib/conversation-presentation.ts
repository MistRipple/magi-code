import type { Message, TimelineRenderItem } from '../types/message';
import { parseToolApprovalPayload } from './tool-error-payload';

export type ConversationPresentationRole =
  | 'process'
  | 'delegation'
  | 'artifact'
  | 'attention'
  | 'final';

const PRESENTATION_ROLES = new Set<ConversationPresentationRole>([
  'process',
  'delegation',
  'artifact',
  'attention',
  'final',
]);

const ARTIFACT_TOOL_NAMES = new Set([
  'diagram_render',
  'image_generate',
  'image_generation',
  'view_image',
]);

function canonicalToolName(name: string): string {
  const normalized = name.trim().toLowerCase();
  const segments = normalized.split(/[.:/]/u).filter(Boolean);
  return segments.at(-1) || normalized;
}

export function messageToolNames(message: Message): string[] {
  const names = new Set<string>();
  const metadataName = message.metadata?.toolName;
  if (typeof metadataName === 'string' && metadataName.trim()) {
    names.add(canonicalToolName(metadataName));
  }
  for (const block of message.blocks || []) {
    if (!block || typeof block !== 'object') continue;
    const name = block.toolCall?.name;
    if (typeof name === 'string' && name.trim()) {
      names.add(canonicalToolName(name));
    }
  }
  return [...names];
}

function toolPayloads(message: Message): unknown[] {
  const payloads: unknown[] = [];
  for (const block of message.blocks || []) {
    if (!block || typeof block !== 'object') continue;
    if (!block.toolCall) continue;
    payloads.push(block.toolCall.error, block.toolCall.result);
  }
  return payloads;
}

export function isApprovalAttentionMessage(message: Message): boolean {
  if (message.type === 'interaction' || message.metadata?.interaction) return true;
  return toolPayloads(message).some((payload) => Boolean(parseToolApprovalPayload(payload)));
}

export function isUserVisibleArtifactMessage(message: Message): boolean {
  const toolNames = messageToolNames(message);
  return toolNames.some((name) => ARTIFACT_TOOL_NAMES.has(name))
    || (message.images?.length || 0) > 0;
}

/**
 * 子代理过程默认留在代理详情；只有必须由用户处理的授权，以及用户应直接看到的
 * 图表/图片，才投射回主对话。原始与摘要模式都读取同一份投影，避免两套状态。
 */
export function isPromotedSidechainMessage(message: Message): boolean {
  const role = inferConversationPresentationRole(message);
  return role === 'attention' || role === 'artifact';
}

export function inferConversationPresentationRole(message: Message): ConversationPresentationRole {
  const metadataRole = message.metadata?.conversationPresentationRole;
  if (
    typeof metadataRole === 'string'
    && PRESENTATION_ROLES.has(metadataRole as ConversationPresentationRole)
  ) {
    return metadataRole as ConversationPresentationRole;
  }
  const outputKind = message.metadata?.assistantOutputKind;
  if (outputKind === 'final' || outputKind === 'error') return 'final';
  if (isApprovalAttentionMessage(message)) return 'attention';

  const toolNames = messageToolNames(message);
  if (toolNames.includes('agent_spawn')) return 'delegation';
  if (isUserVisibleArtifactMessage(message)) return 'artifact';
  return 'process';
}

export function conversationPresentationRole(
  item: TimelineRenderItem,
  finalItemKeys: ReadonlySet<string>,
): ConversationPresentationRole {
  if (finalItemKeys.has(item.key)) return 'final';
  return inferConversationPresentationRole(item.message);
}
