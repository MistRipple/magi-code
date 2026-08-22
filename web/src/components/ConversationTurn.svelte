<script lang="ts">
  import { formatDuration, formatElapsed } from '../lib/utils';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { Message, TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';
  import TurnRuntimeIndicator from './TurnRuntimeIndicator.svelte';
  import ConversationProcessRow from './ConversationProcessRow.svelte';
  import ConversationToolGroup from './ConversationToolGroup.svelte';

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

  type ConversationProcessEntry =
    | { kind: 'event'; key: string; item: TimelineRenderItem }
    | { kind: 'tool-group'; key: string; items: TimelineRenderItem[] };

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

  // 轮次状态只在首次创建时读取；用户手动展开后，流式更新不能覆盖这个选择。
  let expanded = $state(untrack(() => initialExpanded));

  function metadataString(message: Message, key: string): string {
    const value = message.metadata?.[key];
    return typeof value === 'string' ? value.trim() : '';
  }

  function isToolLikeMessage(message: Message): boolean {
    return message.type === 'tool_call'
      || (message.blocks || []).some((block) => (
        block.type === 'tool_call' || block.type === 'tool_result' || block.type === 'file_change'
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
        Boolean(block.content?.trim())
          || block.type === 'file_change'
          || block.type === 'plan'
          || block.type === 'code'
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
  const processItems = $derived(assistantItems.filter((item) => !finalItemKeys.has(item.key)));

  const processEntries = $derived.by(() => {
    const result: ConversationProcessEntry[] = [];
    let toolGroupItems: TimelineRenderItem[] = [];
    let toolGroupInserted = false;
    const hasToolItems = processItems.some((item) => isToolLikeMessage(item.message));

    const insertToolGroup = () => {
      if (toolGroupInserted || toolGroupItems.length === 0) return;
      const firstKey = toolGroupItems[0].key;
      result.push({
        kind: 'tool-group',
        key: `tool-group:${firstKey}`,
        items: toolGroupItems,
      });
      toolGroupInserted = true;
    };

    for (const item of processItems) {
      if (isToolLikeMessage(item.message)) {
        toolGroupItems.push(item);
        continue;
      }
      // 同一轮中的思考输出只是模型内部过程，不应把连续工具调用切成多个组。
      // 有工具时省略这些重复的思考行；没有工具时仍保留思考事件供用户展开查看。
      if (hasToolItems && item.message.type === 'thinking') continue;
      insertToolGroup();
      result.push({ kind: 'event', key: `event:${item.key}`, item });
    }
    insertToolGroup();
    return result;
  });

  const hasProcess = $derived(processEntries.length > 0 || runtimeActive);
  const isLive = $derived(
    runtimeActive
      || items.some((item) => item.message.isStreaming)
      || items.some((item) => {
        const status = metadataString(item.message, 'turnStatus');
        return status === 'pending' || status === 'running';
      }),
  );
  const durationMs = $derived.by(() => {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const value = items[index].message.metadata?.responseDurationMs;
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
    return durationLabel ? `${prefix} ${durationLabel}` : prefix;
  });

  function toggle(): void {
    expanded = !expanded;
  }
</script>

<article class="conversation-turn" data-conversation-turn-id={turnId}>
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

      {#if expanded}
        <div class="turn-process" id={`turn-process-${turnId}`}>
          {#each processEntries as entry (entry.key)}
            {#if entry.kind === 'event'}
              <ConversationProcessRow item={entry.item} />
            {:else}
              <ConversationToolGroup
                items={entry.items}
                {readOnly}
                {displayContext}
                {filePreviewScopeForItem}
                {canEditMessage}
                {editMessage}
                {continueInterruptedSession}
              />
            {/if}
          {/each}
          {#if runtimeActive}
            <TurnRuntimeIndicator {elapsedSeconds} />
          {/if}
        </div>
      {/if}
    </section>
  {/if}

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
</article>

<style>
  .conversation-turn {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
  }

  .turn-disclosure {
    min-width: 0;
  }

  .turn-disclosure-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 42px;
    gap: 8px;
    padding: 0;
    border: 0;
    border-bottom: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
    cursor: pointer;
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

  .turn-process {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: var(--space-3);
    padding: 0 0 var(--space-2) 28px;
    border-left: 1px solid color-mix(in srgb, var(--border) 74%, transparent);
  }

  .turn-process :global(.turn-runtime-indicator) {
    margin-left: 0;
  }

  @media (max-width: 560px) {
    .turn-process {
      padding-left: 20px;
    }
  }
</style>
