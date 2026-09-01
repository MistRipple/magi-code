<script lang="ts">
  import { formatDuration, formatElapsed } from '../lib/utils';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { Message, TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';
  import TurnRuntimeIndicator from './TurnRuntimeIndicator.svelte';
  import TurnRuntimeSummary from './TurnRuntimeSummary.svelte';
  import ConversationAgentGroup from './ConversationAgentGroup.svelte';
  import ConversationToolGroup from './ConversationToolGroup.svelte';
  import ConversationPhase from './ConversationPhase.svelte';
  import {
    conversationPresentationRole,
  } from '../lib/conversation-presentation';
  import {
    buildConversationDisclosureBlocks,
    type ConversationDisclosureBlock,
    type ConversationStreamEntry,
  } from '../lib/conversation-disclosure';

  interface Props {
    turnId: string;
    items: TimelineRenderItem[];
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    runtimeActive?: boolean;
    elapsedSeconds?: number;
    initialExpanded?: boolean;
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    canEditMessage: (item: TimelineRenderItem) => boolean;
    editMessage: (item: TimelineRenderItem) => void;
    continueInterruptedSession: () => void;
  }

  let {
    turnId,
    items,
    readOnly = false,
    displayContext = 'thread',
    runtimeActive = false,
    elapsedSeconds = 0,
    initialExpanded = false,
    filePreviewScopeForItem,
    canEditMessage,
    editMessage,
    continueInterruptedSession,
  }: Props = $props();

  // 自动状态只负责实时轮次；用户手动展开/收起后，流式更新不能覆盖这个选择。
  let expanded = $state(untrack(() => initialExpanded || runtimeActive));
  let manualTurnOverride = false;
  let previousLive = false;

  function metadataString(message: Message, key: string): string {
    const value = message.metadata?.[key];
    return typeof value === 'string' ? value.trim() : '';
  }

  function isToolLikeMessage(message: Message): boolean {
    return message.type === 'tool_call'
      || (message.blocks || []).some((block) => (
        Boolean(block)
          && typeof block === 'object'
          && (block.type === 'tool_call' || block.type === 'tool_result' || block.type === 'file_change')
      ));
  }

  function isFinalMessage(message: Message): boolean {
    const outputKind = metadataString(message, 'assistantOutputKind');
    return outputKind === 'final'
      || outputKind === 'error'
      || typeof message.metadata?.responseDurationMs === 'number';
  }

  function isUsefulFinalCandidate(message: Message): boolean {
    if (isToolLikeMessage(message) || message.type === 'thinking') return false;
    if (message.type === 'error' || message.type === 'result' || message.type === 'text') return true;
    return Boolean(
      message.content?.trim()
      || (message.blocks || []).some((block) => (
        Boolean(block)
          && typeof block === 'object'
          && (Boolean(block.content?.trim())
            || block.type === 'file_change'
            || block.type === 'plan'
            || block.type === 'code')
      )),
    );
  }

  const userItems = $derived(items.filter((item) => item.message.type === 'user_input'));
  const assistantItems = $derived(items.filter((item) => item.message.type !== 'user_input'));
  const explicitFinalItems = $derived(assistantItems.filter((item) => isFinalMessage(item.message)));
  const finalItems = $derived.by(() => {
    if (explicitFinalItems.length > 0) return explicitFinalItems;
    for (let index = assistantItems.length - 1; index >= 0; index -= 1) {
      const candidate = assistantItems[index];
      if (isUsefulFinalCandidate(candidate.message)) return [candidate];
    }
    // 只有明确的最终输出才离开过程区；工具结果不能伪装成最终回答。
    return [];
  });
  const finalItemKeys = $derived(new Set(finalItems.map((item) => item.key)));
  const presentationItems = $derived(assistantItems.map((item) => ({
    item,
    role: conversationPresentationRole(item, finalItemKeys),
  })));
  const processItems = $derived(
    presentationItems.filter((entry) => entry.role === 'process').map((entry) => entry.item),
  );
  const delegationItems = $derived(
    presentationItems.filter((entry) => entry.role === 'delegation').map((entry) => entry.item),
  );
  const streamEntries = $derived.by(() => {
    const result: ConversationStreamEntry[] = [];
    let agentGroupEmitted = false;
    let toolGroupItems: TimelineRenderItem[] = [];
    const hasToolItems = processItems.some((item) => isToolLikeMessage(item.message));

    const flushToolGroup = () => {
      if (toolGroupItems.length === 0) return;
      const firstKey = toolGroupItems[0].key;
      result.push({
        kind: 'tool-group',
        key: `tool-group:${firstKey}`,
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
        continue;
      }
      // 同一轮中的思考输出只是模型内部过程，不应把连续工具调用切成多个组。
      // 有工具时省略这些重复的思考行；没有工具时仍保留思考事件供用户展开查看。
      if (hasToolItems && entry.item.message.type === 'thinking') continue;
      flushToolGroup();
      result.push({ kind: 'event', key: `event:${entry.item.key}`, item: entry.item });
    }
    flushToolGroup();
    return result;
  });

  const hasProcess = $derived(
    streamEntries.some((entry) => entry.kind === 'event' || entry.kind === 'tool-group')
      || runtimeActive,
  );
  const isLive = $derived(
    runtimeActive
      || items.some((item) => item.message.isStreaming)
      || items.some((item) => {
        const status = metadataString(item.message, 'turnStatus');
        return status === 'pending' || status === 'running';
      }),
  );

  $effect(() => {
    const nextLive = isLive;
    if (nextLive !== previousLive) {
      if (!manualTurnOverride) expanded = nextLive;
      previousLive = nextLive;
    }
  });
  const durationMs = $derived.by(() => {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const value = items[index].message.metadata?.responseDurationMs;
      if (typeof value === 'number' && Number.isFinite(value) && value >= 0) return value;
    }
    return null;
  });
  const completedAt = $derived.by(() => {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const value = items[index].message.metadata?.responseCompletedAt;
      if (typeof value === 'number' && Number.isFinite(value) && value >= 0) return value;
    }
    return null;
  });
  const durationLabel = $derived.by(() => {
    if (isLive) return formatElapsed(Math.max(0, elapsedSeconds));
    if (durationMs === null) return '';
    return durationMs > 0 && durationMs < 1000 ? '<1s' : formatDuration(durationMs);
  });
  const disclosureLabel = $derived.by(() => {
    const prefix = isLive
      ? i18n.t('messageList.turnDisclosure.processing')
      : i18n.t('messageList.turnDisclosure.processed');
    const durationPart = durationLabel ? ` ${durationLabel}` : '';
    return `${prefix}${durationPart}`;
  });

  const disclosureBlocks = $derived<ConversationDisclosureBlock[]>(
    buildConversationDisclosureBlocks(streamEntries),
  );
  const activePhaseKey = $derived.by(() => {
    if (!isLive) return '';
    for (let index = disclosureBlocks.length - 1; index >= 0; index -= 1) {
      const block = disclosureBlocks[index];
      if (block.kind === 'phase') return block.phase.key;
    }
    return '';
  });

  function toggle(): void {
    manualTurnOverride = true;
    expanded = !expanded;
  }
</script>

<article
  class="conversation-turn"
  data-conversation-turn-id={turnId}
  data-layout-observation="conversation-turn"
>
  {#each userItems as item (item.key)}
    <MessageItem
      message={item.message}
      {readOnly}
      {displayContext}
      filePreviewScope={filePreviewScopeForItem(item)}
      canEdit={canEditMessage(item)}
      onEdit={() => editMessage(item)}
      onContinueInterrupted={continueInterruptedSession}
    />
  {/each}

  {#if hasProcess}
    <section class="turn-disclosure" class:expanded>
      <button
        type="button"
        class="turn-disclosure-header"
        aria-expanded={expanded}
        aria-controls={`turn-process-${turnId}`}
        onclick={toggle}
      >
        <span class="turn-disclosure-label">{disclosureLabel}</span>
        <span class="turn-disclosure-chevron" class:rotated={expanded}>
          <Icon name="chevron-right" size={14} />
        </span>
      </button>

    </section>
  {:else if durationLabel}
    <div class="turn-status-header">
      <span class="turn-disclosure-label">{disclosureLabel}</span>
    </div>
  {/if}

  <div class="turn-stream" id={`turn-process-${turnId}`}>
    {#each disclosureBlocks as block (block.kind === 'phase' ? block.phase.key : block.key)}
      {#if block.kind === 'phase'}
        {#if expanded}
        <ConversationPhase
          phase={block.phase}
          active={block.phase.key === activePhaseKey}
          {readOnly}
          {displayContext}
          {filePreviewScopeForItem}
          {continueInterruptedSession}
        />
        {/if}
      {:else if block.kind === 'agent-group'}
        <ConversationAgentGroup
          items={block.items}
          {readOnly}
          {displayContext}
          {filePreviewScopeForItem}
          {continueInterruptedSession}
        />
      {:else if block.kind === 'tool-group'}
        {#if expanded}
          <div class="turn-process-entry">
            <ConversationToolGroup
              items={block.items}
              {readOnly}
              {displayContext}
              {filePreviewScopeForItem}
              {continueInterruptedSession}
            />
          </div>
        {/if}
      {:else}
      <section
          class="turn-promoted"
          data-turn-attention={block.role === 'attention' ? 'true' : undefined}
          data-turn-artifact={block.role === 'artifact' ? 'true' : undefined}
      >
        <MessageItem
          message={block.item.message}
          {readOnly}
          {displayContext}
          filePreviewScope={filePreviewScopeForItem(block.item)}
          onContinueInterrupted={continueInterruptedSession}
          hideResponseDuration
          presentationRole={block.role}
        />
      </section>
      {/if}
    {/each}
    {#if expanded && runtimeActive}
    <div class="turn-process-entry turn-runtime-row"><TurnRuntimeIndicator {elapsedSeconds} /></div>
    {/if}
  </div>

  {#each finalItems as item (item.key)}
    <MessageItem
      message={item.message}
      {readOnly}
      {displayContext}
      filePreviewScope={filePreviewScopeForItem(item)}
      canEdit={canEditMessage(item)}
      onEdit={() => editMessage(item)}
      onContinueInterrupted={continueInterruptedSession}
      hideResponseDuration
    />
  {/each}

  {#if !isLive && durationMs !== null}
    <TurnRuntimeSummary durationMs={durationMs} {completedAt} />
  {/if}
</article>

<style>
  .conversation-turn {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
  }

  .turn-disclosure {
    min-width: 0;
  }

  .turn-disclosure-header,
  .turn-status-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 38px;
    gap: 8px;
    padding: 0;
    border: 0;
    border-bottom: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
  }

  .turn-disclosure-header {
    cursor: pointer;
  }

  .turn-status-header {
    cursor: default;
  }

  .turn-disclosure-header:hover,
  .turn-disclosure-header:focus-visible {
    color: var(--foreground);
  }

  .turn-disclosure-header:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 3px;
  }

  .turn-disclosure-label {
    font-size: var(--text-base);
    font-variant-numeric: tabular-nums;
    font-weight: var(--font-medium);
  }

  .turn-disclosure-chevron {
    display: inline-flex;
    flex: 0 0 auto;
    transition: transform var(--transition-fast), color var(--transition-fast);
  }

  .turn-disclosure-chevron.rotated {
    transform: rotate(90deg);
    color: var(--foreground);
  }

  .turn-stream {
    display: flex;
    flex-direction: column;
    gap: 0;
    min-width: 0;
  }

  .turn-process-entry {
    min-width: 0;
    padding-left: 8px;
    border-left: 1px solid color-mix(in srgb, var(--border) 74%, transparent);
  }

  .turn-runtime-row :global(.turn-runtime-indicator) {
    margin-left: 0;
  }

  .turn-promoted {
    min-width: 0;
    padding: 2px 0;
  }

  .turn-promoted :global(.message-item.assistant) {
    padding-inline: 0;
  }

  @media (max-width: 560px) {
    .turn-disclosure-header,
    .turn-status-header {
      min-height: 40px;
    }

    .turn-process-entry {
      padding-left: 6px;
    }
  }
</style>
