import type { Message, TimelineRenderItem, ToolCall, ToolCallStatus } from '../types/message';
import { firstToolDisplayText, resolveToolCardTarget } from './tool-call-display';
import { parseToolIdentity } from './tool-identity';

export type ConversationStreamEntry =
  | { kind: 'event'; key: string; item: TimelineRenderItem }
  | { kind: 'tool-group'; key: string; items: TimelineRenderItem[] }
  | { kind: 'item'; key: string; item: TimelineRenderItem; role: 'artifact' | 'attention' }
  | { kind: 'agent-group'; key: string; items: TimelineRenderItem[] };

export type ConversationPhaseEntry =
  | Extract<ConversationStreamEntry, { kind: 'event' }>
  | Extract<ConversationStreamEntry, { kind: 'tool-group' }>;

export interface ConversationPhase {
  key: string;
  entries: ConversationPhaseEntry[];
}

export type ConversationDisclosureBlock =
  | { kind: 'phase'; phase: ConversationPhase }
  | Extract<ConversationStreamEntry, { kind: 'item' | 'agent-group' | 'tool-group' }>;

export type ConversationTranslate = (
  key: string,
  vars?: Record<string, string | number>,
) => string;

function plainText(value: string): string {
  return value
    .replace(/```[\s\S]*?```/gu, ' ')
    .replace(/[`*_>#-]/gu, ' ')
    .replace(/\s+/gu, ' ')
    .trim();
}

export function resolveConversationProcessLabel(
  message: Message,
  translate: ConversationTranslate,
): string {
  const directContent = typeof message.content === 'string' ? plainText(message.content) : '';
  if (directContent) return directContent;
  for (const block of message.blocks || []) {
    if (!block || typeof block !== 'object') continue;
    if (typeof block.content === 'string') {
      const content = plainText(block.content);
      if (content) return content;
    }
    if (block.type === 'thinking') {
      const content = block.thinking?.segments
        .map((segment) => plainText(segment.content))
        .filter(Boolean)
        .join(' ');
      if (content) return content;
    }
  }

  const title = typeof message.metadata?.title === 'string' ? message.metadata.title.trim() : '';
  if (title) return title;
  if (message.type === 'thinking') return translate('messageList.turnDisclosure.thinking');
  if (message.type === 'system-notice') return translate('messageList.turnDisclosure.systemEvent');
  return translate('messageList.turnDisclosure.processEvent');
}

type ConversationToolAction =
  | 'read'
  | 'edit'
  | 'command'
  | 'browser'
  | 'search'
  | 'agent'
  | 'generate'
  | 'other';

interface ConversationToolDescriptor {
  item: TimelineRenderItem;
  name: string;
  action: ConversationToolAction;
  status: ToolCallStatus;
}

function toolCall(item: TimelineRenderItem): ToolCall | undefined {
  for (const block of item.message.blocks || []) {
    if (!block || typeof block !== 'object' || !block.toolCall) continue;
    return block.toolCall;
  }
  return undefined;
}

function toolName(item: TimelineRenderItem): string {
  const metadataName = typeof item.message.metadata?.toolName === 'string'
    ? item.message.metadata.toolName.trim()
    : '';
  if (metadataName) return metadataName;
  return toolCall(item)?.name?.trim() || '';
}

function canonicalToolName(name: string): string {
  const parsed = parseToolIdentity(name);
  const baseName = parsed.baseName || name;
  const segments = baseName.toLowerCase().split(/[.:/]/u).filter(Boolean);
  return segments.at(-1) || baseName.toLowerCase();
}

function classifyToolAction(name: string): ConversationToolAction {
  const baseName = canonicalToolName(name);
  if (baseName === 'file_read' || baseName === 'view_image' || /(?:^|_)(?:read|view)(?:_|$)/u.test(baseName)) {
    return 'read';
  }
  if (baseName === 'file_write'
    || baseName === 'file_patch'
    || baseName === 'apply_patch'
    || baseName === 'file_remove'
    || baseName === 'file_mkdir'
    || baseName === 'file_copy'
    || baseName === 'file_move'
    || /(?:^|_)(?:write|patch|edit|remove|delete|mkdir|copy|move)(?:_|$)/u.test(baseName)) {
    return 'edit';
  }
  if (baseName === 'shell_exec' || /(?:^|_)(?:shell|exec|command|terminal)(?:_|$)/u.test(baseName)) {
    return 'command';
  }
  if (baseName.startsWith('browser_') || baseName === 'browser') return 'browser';
  if (baseName === 'web_search'
    || baseName === 'search_text'
    || baseName === 'search_semantic'
    || baseName === 'knowledge_query'
    || baseName === 'knowledge_graph_query'
    || /(?:^|_)(?:search|query)(?:_|$)/u.test(baseName)) {
    return 'search';
  }
  if (baseName === 'agent_spawn' || baseName === 'agent_wait' || baseName.startsWith('agent_')) {
    return 'agent';
  }
  if (baseName === 'diagram_render'
    || baseName === 'image_generate'
    || baseName.startsWith('image_')) {
    return 'generate';
  }
  return 'other';
}

function toolStatus(item: TimelineRenderItem): ToolCallStatus {
  const call = toolCall(item);
  if (call?.status) return call.status;
  const metadataStatus = item.message.metadata?.toolStatus;
  if (metadataStatus === 'pending' || metadataStatus === 'running'
    || metadataStatus === 'success' || metadataStatus === 'error') {
    return metadataStatus;
  }
  return item.message.isStreaming ? 'running' : 'success';
}

function toolDescriptor(item: TimelineRenderItem): ConversationToolDescriptor {
  const name = toolName(item);
  return {
    item,
    name,
    action: classifyToolAction(name),
    status: toolStatus(item),
  };
}

function isToolActive(descriptor: ConversationToolDescriptor): boolean {
  return descriptor.status === 'pending' || descriptor.status === 'running'
    || descriptor.item.message.isStreaming;
}

function toolTarget(item: TimelineRenderItem, name: string): string {
  const call = toolCall(item);
  const input = call?.arguments || {};
  const inputRecord = input as Record<string, unknown>;
  const directoryView = canonicalToolName(name) === 'file_read'
    && (inputRecord.type === 'directory'
      || inputRecord.path === '.'
      || inputRecord.path === ''
      || (typeof inputRecord.path === 'string' && inputRecord.path.endsWith('/')));
  const target = resolveToolCardTarget({
    toolName: name,
    input,
    output: call?.result,
    explicitFilepath: item.message.metadata?.filePath,
    directoryView,
  });
  if (target.primaryPath) return target.primaryPath;
  if (target.paths.length > 0) return target.paths[0];

  const baseName = canonicalToolName(name);
  if (baseName === 'shell_exec') {
    return firstToolDisplayText(inputRecord.command, inputRecord.cmd);
  }
  if (baseName === 'web_search' || baseName === 'search_text' || baseName === 'search_semantic'
    || baseName === 'knowledge_query' || baseName === 'knowledge_graph_query') {
    return firstToolDisplayText(inputRecord.query, inputRecord.pattern);
  }
  if (baseName === 'agent_spawn') {
    return firstToolDisplayText(inputRecord.display_name, inputRecord.title, inputRecord.goal);
  }
  return firstToolDisplayText(
    inputRecord.path,
    inputRecord.file_path,
    inputRecord.url,
    inputRecord.title,
  );
}

function compactTarget(value: string, pathLike: boolean): string {
  const normalized = value.replace(/\s+/gu, ' ').trim();
  if (!normalized) return '';
  if (pathLike && (normalized.startsWith('/') || normalized.includes('/'))) {
    const pathParts = normalized.replaceAll('\\', '/').split('/').filter(Boolean);
    return pathParts.at(-1) || normalized;
  }
  return normalized.length > 64 ? `${normalized.slice(0, 61)}…` : normalized;
}

function singleToolLabel(
  descriptor: ConversationToolDescriptor,
  translate: ConversationTranslate,
): string {
  if (isToolActive(descriptor)) {
    return translate(`messageList.turnDisclosure.toolSingleRunning${capitalize(descriptor.action)}`);
  }
  const target = compactTarget(
    toolTarget(descriptor.item, descriptor.name),
    descriptor.action === 'read' || descriptor.action === 'edit',
  );
  const actionKey = `messageList.turnDisclosure.toolSingle${capitalize(descriptor.action)}`;
  const label = target && (descriptor.action === 'read'
    || descriptor.action === 'edit'
    || descriptor.action === 'command'
    || descriptor.action === 'search')
    ? translate(actionKey, { target })
    : translate(actionKey);
  return descriptor.status === 'error'
    ? `${label} · ${translate('messageList.turnDisclosure.toolFailed')}`
    : label;
}

function capitalize(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

export function resolveConversationToolGroupLabel(
  items: TimelineRenderItem[],
  translate: ConversationTranslate,
): string {
  if (items.length === 0) return translate('messageList.turnDisclosure.toolSingleOther');
  const descriptors = items.map(toolDescriptor);
  if (items.length === 1) return singleToolLabel(descriptors[0], translate);

  const firstAction = descriptors[0].action;
  const sameAction = descriptors.every((descriptor) => descriptor.action === firstAction);
  const action = sameAction && firstAction !== 'other' ? firstAction : 'mixed';
  const active = descriptors.some(isToolActive);
  const state = active ? 'Running' : '';
  const label = translate(
    `messageList.turnDisclosure.toolGroup${capitalize(action)}${state}`,
    { count: items.length },
  );
  const errorCount = descriptors.filter((descriptor) => descriptor.status === 'error').length;
  if (errorCount === 0) return label;
  if (errorCount === items.length) {
    return translate('messageList.turnDisclosure.toolGroupAllFailed', { count: items.length });
  }
  return `${label}${translate('messageList.turnDisclosure.toolGroupFailureSuffix', { count: errorCount })}`;
}

export function resolveConversationPhaseSummary(
  phase: ConversationPhase,
  translate: ConversationTranslate,
): string {
  const firstEntry = phase.entries[0];
  if (!firstEntry) return translate('messageList.turnDisclosure.processEvent');
  if (firstEntry.kind === 'event') {
    return resolveConversationProcessLabel(firstEntry.item.message, translate);
  }
  return resolveConversationToolGroupLabel(firstEntry.items, translate);
}

/**
 * 把连续的文字过程与其后的工具组收敛为一个阶段。
 * 新的文字输出意味着模型开始下一段工作；工具组只附着在当前阶段，
 * 避免把每一次工具调用都提升成一个用户需要理解的“阶段”。
 */
export function buildConversationDisclosureBlocks(
  entries: ConversationStreamEntry[],
): ConversationDisclosureBlock[] {
  const blocks: ConversationDisclosureBlock[] = [];
  let currentPhase: ConversationPhase | null = null;

  const flushPhase = () => {
    if (!currentPhase || currentPhase.entries.length === 0) return;
    if (currentPhase.entries.every((entry) => entry.kind === 'tool-group')) {
      blocks.push(...currentPhase.entries);
      currentPhase = null;
      return;
    }
    blocks.push({ kind: 'phase', phase: currentPhase });
    currentPhase = null;
  };

  for (const entry of entries) {
    if (entry.kind === 'event') {
      // 工具执行后再次出现文字，表示进入下一段模型工作；
      // 连续的文字更新仍属于同一个阶段。
      if (currentPhase?.entries.some((item) => item.kind === 'tool-group')) {
        flushPhase();
      }
      currentPhase ||= {
        key: `phase:${entry.key}`,
        entries: [],
      };
      currentPhase.entries.push(entry);
      continue;
    }

    if (entry.kind === 'tool-group') {
      currentPhase ||= {
        key: `phase:${entry.key}`,
        entries: [],
      };
      currentPhase.entries.push(entry);
      continue;
    }

    flushPhase();
    blocks.push(entry);
  }

  flushPhase();
  return blocks;
}
