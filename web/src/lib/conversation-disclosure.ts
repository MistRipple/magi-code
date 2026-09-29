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

/** 过程标题只是一行预览，超长内容截断；完整内容由展开后的正文承担。 */
const PROCESS_LABEL_MAX_CHARS = 160;
/** 展开后仍以单行紧凑形态呈现的过程文字上限（超过则按完整正文渲染）。 */
const COMPACT_EVENT_MAX_CHARS = 90;

/**
 * 把 Markdown 收敛成一行可读预览：只去掉语法标记，保留词内的连字符/下划线、
 * 路径与数字，不在标点两侧凭空插入空格。代码块整体略过，链接保留可读文字。
 */
function plainText(value: string): string {
  const preview = value
    .replace(/```[\s\S]*?```/gu, ' ')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/gu, '$1')
    .replace(/^[ \t]*(?:[-:| ]+\|[-:| ]*)$/gmu, ' ')
    .replace(/^[ \t]{0,3}#{1,6}[ \t]+/gmu, '')
    .replace(/^[ \t]*>[ \t]?/gmu, '')
    .replace(/^[ \t]*(?:[-*+]|\d+[.)])[ \t]+/gmu, '')
    .replace(/(\*\*|__|~~)(?=\S)([\s\S]*?\S)\1/gu, '$2')
    .replace(/`([^`]*)`/gu, '$1')
    .replace(/\|/gu, ' ')
    .replace(/\s+/gu, ' ')
    .trim();
  return preview.length > PROCESS_LABEL_MAX_CHARS
    ? `${preview.slice(0, PROCESS_LABEL_MAX_CHARS - 1)}…`
    : preview;
}

/** 工具调用、工具结果和文件变更：它们永远属于过程，不是回答。 */
export function isToolLikeMessage(message: Message): boolean {
  return message.type === 'tool_call'
    || (message.blocks || []).some((block) => (
      Boolean(block)
        && typeof block === 'object'
        && (block.type === 'tool_call' || block.type === 'tool_result' || block.type === 'file_change')
    ));
}

/**
 * 这条助手消息是否是本轮的最终输出。
 * 明确的 final/error 标记优先。轮次耗时元数据会附着在本轮最后一条可渲染消息上——
 * 一轮以工具调用结束（没有文字回答）时，那条消息就是工具调用——所以凭耗时判断最终输出
 * 时必须排除工具与思考消息，否则它会被提升到过程之外，以原始卡片风格混进摘要视图。
 */
export function isConversationFinalMessage(message: Message): boolean {
  const outputKind = typeof message.metadata?.assistantOutputKind === 'string'
    ? message.metadata.assistantOutputKind.trim()
    : '';
  if (outputKind === 'final' || outputKind === 'error') return true;
  if (isToolLikeMessage(message) || message.type === 'thinking') return false;
  return typeof message.metadata?.responseDurationMs === 'number';
}

function rawMessageText(message: Message): string {
  if (typeof message.content === 'string' && message.content.trim()) return message.content;
  for (const block of message.blocks || []) {
    if (block && typeof block === 'object' && block.type === 'text'
      && typeof block.content === 'string' && block.content.trim()) {
      return block.content;
    }
  }
  return '';
}

/**
 * 这条过程消息是否能被“一行标题/一行紧凑文字”完整表达。
 * 只有短小的纯文字才算：含换行、代码、表格、列表、标题、思考块、文件变更或超长内容，
 * 都必须展开后按完整正文渲染，不能被压平成一行而丢失结构。
 */
export function isCompactProcessEvent(message: Message): boolean {
  if (message.type === 'thinking') return false;
  const blocks = message.blocks || [];
  if (blocks.some((block) => block && typeof block === 'object' && block.type !== 'text')) return false;
  const raw = rawMessageText(message).trim();
  if (!raw) return true;
  if (raw.length > COMPACT_EVENT_MAX_CHARS || raw.includes('\n')) return false;
  return !/```|^\s*(?:#{1,6}\s|>\s|[-*+]\s|\d+[.)]\s)|\|.*\|/u.test(raw);
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

export type ConversationPhaseDetailEntry =
  | { kind: 'compact'; entry: Extract<ConversationPhaseEntry, { kind: 'event' }> }
  | { kind: 'rich'; entry: Extract<ConversationPhaseEntry, { kind: 'event' }> }
  | { kind: 'tool-group'; entry: Extract<ConversationPhaseEntry, { kind: 'tool-group' }> };

/**
 * 阶段展开后需要额外展示的内容。标题已经完整表达的首条短文字不再重复，
 * 其余短文字保持紧凑行，带结构的长内容按完整正文渲染，工具组沿用工具卡片。
 * 结果为空说明展开没有更多信息，此时阶段只是一行静态标题。
 */
export function resolveConversationPhaseDetails(
  phase: ConversationPhase,
): ConversationPhaseDetailEntry[] {
  const details: ConversationPhaseDetailEntry[] = [];
  phase.entries.forEach((entry, index) => {
    if (entry.kind === 'tool-group') {
      details.push({ kind: 'tool-group', entry });
      return;
    }
    const compact = isCompactProcessEvent(entry.item.message);
    if (compact && index === 0) return;
    details.push({ kind: compact ? 'compact' : 'rich', entry });
  });
  return details;
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
    if (entry.kind === 'event' && entry.item.message.type === 'system-notice') {
      // 系统通知（如上下文压缩）不是模型的一段工作：它独占一行，
      // 不能成为阶段标题，也不能把后面的工具组归到自己名下。
      flushPhase();
      blocks.push({
        kind: 'phase',
        phase: { key: `phase:${entry.key}`, entries: [entry] },
      });
      continue;
    }

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
