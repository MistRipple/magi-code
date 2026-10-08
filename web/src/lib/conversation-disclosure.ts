import type { Message, TimelineRenderItem, ToolCall, ToolCallStatus } from '../types/message';
import { browserToolSummary } from './browser-tool-display';
import { inferConversationPresentationRole, type ConversationPresentationRole } from './conversation-presentation';
import { firstToolDisplayText, resolveToolCardTarget } from './tool-call-display';
import { resolveToolDisplayName } from './tool-display-name';
import { parseToolIdentity } from './tool-identity';

export type ConversationStreamEntry =
  | { kind: 'event'; key: string; item: TimelineRenderItem }
  | { kind: 'tool-group'; key: string; items: TimelineRenderItem[] }
  | { kind: 'item'; key: string; item: TimelineRenderItem; role: 'artifact' | 'attention' }
  | { kind: 'agent-group'; key: string; items: TimelineRenderItem[] };

export interface ConversationPresentationEntry {
  item: TimelineRenderItem;
  role: ConversationPresentationRole;
}

/**
 * 把一轮里按呈现角色标注好的条目组织成摘要模式的过程流：委派合并成一个代理组，
 * 产物和待处理交互单独成块，连续工具调用合并成工具组，其余文字作为事件。
 *
 * 结果只依赖每个条目之前的内容：流式追加新条目时，已经产生的条目不会消失或改变 key，
 * 用户在阶段和工具组上的展开状态因此得以保持。
 */
export function buildConversationStreamEntries(
  presentationItems: ConversationPresentationEntry[],
): ConversationStreamEntry[] {
  const result: ConversationStreamEntry[] = [];
  const delegationItems = presentationItems
    .filter((entry) => entry.role === 'delegation')
    .map((entry) => entry.item);
  let agentGroupEmitted = false;
  let toolGroupItems: TimelineRenderItem[] = [];
  let toolSeen = false;

  const flushToolGroup = () => {
    if (toolGroupItems.length === 0) return;
    result.push({
      kind: 'tool-group',
      key: `tool-group:${toolGroupItems[0].key}`,
      items: toolGroupItems,
    });
    toolGroupItems = [];
  };

  for (const entry of presentationItems) {
    if (entry.role === 'final') continue;
    if (entry.role === 'delegation') {
      flushToolGroup();
      if (!agentGroupEmitted) {
        result.push({
          kind: 'agent-group',
          key: `agent-group:${entry.item.key}`,
          items: delegationItems,
        });
        agentGroupEmitted = true;
      }
      continue;
    }
    if (entry.role === 'artifact' || entry.role === 'attention') {
      flushToolGroup();
      result.push({
        kind: 'item',
        key: `${entry.role}:${entry.item.key}`,
        item: entry.item,
        role: entry.role,
      });
      continue;
    }
    if (isToolLikeMessage(entry.item.message)) {
      toolGroupItems.push(entry.item);
      toolSeen = true;
      continue;
    }
    // 工具调用之间的思考只是模型内部过程，不应把连续工具调用切成多个组，因此省略。
    // 去留只看它之前是否出现过工具，保证追加新条目不会让已显示的思考消失。
    if (toolSeen && entry.item.message.type === 'thinking') continue;
    flushToolGroup();
    result.push({ kind: 'event', key: `event:${entry.item.key}`, item: entry.item });
  }
  flushToolGroup();
  return result;
}

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
 * 这条助手消息是否是本轮的最终输出。唯一依据是后端写入的 assistantOutputKind：
 * 只有 final / error 才是最终输出。流式中的文字是 progress，模型之后可能继续调用工具，
 * 前端不能凭位置或耗时元数据把它提前当成最终回答——那会让它先出现在过程下方、再被收回。
 */
export function isConversationFinalMessage(message: Message): boolean {
  return inferConversationPresentationRole(message) === 'final';
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
  unconfirmed: boolean;
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
  // 命名空间优先于动作词：browser_read、browser_type 等是浏览器操作，不是文件读写。
  if (baseName.startsWith('browser_') || baseName === 'browser') return 'browser';
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
    || metadataStatus === 'success' || metadataStatus === 'error' || metadataStatus === 'unconfirmed') {
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
    // 副作用可能已发生但无法确认（item 状态 indeterminate），和普通失败分开呈现。
    unconfirmed: toolStatus(item) === 'unconfirmed',
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
  // 浏览器工具各自有展示名（浏览器点击、浏览器读取正文……），比笼统的“浏览器操作”更有信息量。
  // 有目标与无目标是两条独立文案，不能让带 {target} 的文案在缺少目标时原样显示占位符。
  const label = descriptor.action === 'browser'
    ? browserToolLabel(descriptor, translate)
    : target && TARGETED_TOOL_ACTIONS.has(descriptor.action)
      ? translate(`${actionKey}Target`, { target })
      : translate(actionKey);
  if (descriptor.unconfirmed) return `${label} · ${translate('messageList.turnDisclosure.toolUnconfirmed')}`;
  return descriptor.status === 'error'
    ? `${label} · ${translate('messageList.turnDisclosure.toolFailed')}`
    : label;
}

const TARGETED_TOOL_ACTIONS = new Set<ConversationToolAction>(['read', 'edit', 'command', 'search']);

/** 浏览器工具：展示名 + 对人有意义的目标（网址、输入内容、实际点击的元素……）。 */
function browserToolLabel(descriptor: ConversationToolDescriptor, translate: ConversationTranslate): string {
  const displayName = resolveToolDisplayName(descriptor.name, { t: translate });
  const call = toolCall(descriptor.item);
  const args = call?.arguments && typeof call.arguments === 'object' ? call.arguments as Record<string, unknown> : {};
  const summary = compactTarget(browserToolSummary(descriptor.name, args, call?.result) || '', false);
  return summary ? `${displayName} ${summary}` : displayName;
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
  const unconfirmedCount = descriptors.filter((descriptor) => descriptor.unconfirmed).length;
  const errorCount = descriptors.filter(
    (descriptor) => descriptor.status === 'error',
  ).length;
  if (errorCount === items.length) {
    return translate('messageList.turnDisclosure.toolGroupAllFailed', { count: items.length });
  }
  const failureSuffix = errorCount > 0
    ? translate('messageList.turnDisclosure.toolGroupFailureSuffix', { count: errorCount })
    : '';
  const unconfirmedSuffix = unconfirmedCount > 0
    ? translate('messageList.turnDisclosure.toolGroupUnconfirmedSuffix', { count: unconfirmedCount })
    : '';
  return `${label}${failureSuffix}${unconfirmedSuffix}`;
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

export interface ConversationPhasePresentationState {
  /** 阶段是否是正在进行的当前阶段。 */
  active: boolean;
  /** 阶段当前是否展开。 */
  expanded: boolean;
  /** 用户是否手动展开 / 收起过：手动操作优先于自动状态。 */
  manualOverride: boolean;
}

export interface ConversationPhasePresentation {
  /**
   * 正在流式输出的一段纯文字：不要标题行，正文直接显示。
   * 这段文字在回合结束前还无法确定是不是最终回答（结束时才会归入回答区），
   * 如果先挂一个「标题 + 同样内容的正文」，结束时标题凭空消失，用户会看到同一段话出现两遍。
   */
  bodyOnly: boolean;
  /** 展开后标题行是否会重复首条正文：此时标题应改为中性的状态文案而不是内容预览。 */
  headerRepeatsBody: boolean;
}

export function resolveConversationPhasePresentation(
  phase: ConversationPhase,
  state: ConversationPhasePresentationState,
): ConversationPhasePresentation {
  const firstEntry = phase.entries[0];
  const firstShownBelow = resolveConversationPhaseDetails(phase).some(
    (detail) => detail.kind === 'rich' && detail.entry === firstEntry,
  );
  const textOnly = phase.entries.every((entry) => entry.kind === 'event');
  return {
    bodyOnly: state.active && !state.manualOverride && textOnly && firstShownBelow,
    headerRepeatsBody: state.expanded && firstShownBelow,
  };
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
