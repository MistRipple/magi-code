<script lang="ts">
  import { formatDuration, formatElapsed } from '../lib/utils';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { Message, TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';
  import TurnRuntimeIndicator from './TurnRuntimeIndicator.svelte';
  import ConversationStage, { type ConversationStageModel } from './ConversationStage.svelte';

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

  // 轮次默认收起；当前正在输出的轮次由 MessageList 传入展开，避免实时输出被隐藏。
  let expanded = $state(untrack(() => initialExpanded));

  function metadataString(message: Message, key: string): string {
    const value = message.metadata?.[key];
    return typeof value === 'string' ? value.trim() : '';
  }

  function isAssistantOutput(message: Message): boolean {
    return message.type !== 'user_input';
  }

  function isFinalMessage(message: Message): boolean {
    const outputKind = metadataString(message, 'assistantOutputKind');
    return outputKind === 'final'
      || outputKind === 'error'
      || typeof message.metadata?.responseDurationMs === 'number';
  }

  function isUsefulFinalCandidate(message: Message): boolean {
    if (message.type === 'tool_call' || message.type === 'thinking') return false;
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
  const assistantItems = $derived(items.filter((item) => isAssistantOutput(item.message)));
  const explicitFinalItems = $derived(assistantItems.filter((item) => isFinalMessage(item.message)));
  const finalItems = $derived.by(() => {
    if (explicitFinalItems.length > 0) return explicitFinalItems;
    for (let index = assistantItems.length - 1; index >= 0; index -= 1) {
      const candidate = assistantItems[index];
      if (isUsefulFinalCandidate(candidate.message)) return [candidate];
    }
    // 极少数没有最终正文的失败/中断轮次仍需保留一个可见锚点，不能因折叠把整轮内容隐藏。
    return assistantItems.length > 0 ? [assistantItems[assistantItems.length - 1]] : [];
  });
  const finalItemKeys = $derived(new Set(finalItems.map((item) => item.key)));
  const processItems = $derived(assistantItems.filter((item) => !finalItemKeys.has(item.key)));

  function modelRound(item: TimelineRenderItem): number | null {
    const value = item.message.metadata?.modelRound;
    return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
      ? value
      : null;
  }

  function isToolLike(item: TimelineRenderItem): boolean {
    return item.message.type === 'tool_call'
      || (item.message.blocks || []).some((block) => (
        block.type === 'tool_call' || block.type === 'tool_result' || block.type === 'file_change'
      ));
  }

  const stages = $derived.by(() => {
    const result: ConversationStageModel[] = [];
    let currentGroupingKey = '';
    let fallbackIndex = 0;
    for (const item of processItems) {
      const round = modelRound(item);
      const groupingKey = round !== null
        ? `round:${round}`
        : (isToolLike(item) && currentGroupingKey
          ? currentGroupingKey
          : `sequence:${fallbackIndex++}`);
      if (groupingKey !== currentGroupingKey) {
        result.push({
          key: `stage:${result.length}:${groupingKey}`,
          index: result.length + 1,
          items: [],
        });
        currentGroupingKey = groupingKey;
      }
      result[result.length - 1].items.push(item);
    }
    return result;
  });

  const hasProcess = $derived(processItems.length > 0 || runtimeActive);
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
          <Icon name="chevron-right" size={13} />
        </span>
      </button>

      {#if expanded}
        <div class="turn-process" id={`turn-process-${turnId}`}>
          {#each stages as stage (stage.key)}
            <ConversationStage
              {stage}
              {readOnly}
              {displayContext}
              filePreviewScopeForItem={filePreviewScopeForItem}
              canEditMessage={canEditMessage}
              editMessage={editMessage}
              continueInterruptedSession={continueInterruptedSession}
              initialExpanded={isLive}
            />
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
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 26px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
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
    font-variant-numeric: tabular-nums;
  }

  .turn-disclosure-chevron {
    display: inline-flex;
    transition: transform var(--transition-fast), color var(--transition-fast);
  }

  .turn-disclosure-chevron.rotated {
    transform: rotate(90deg);
    color: var(--foreground);
  }

  .turn-process {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin-top: var(--space-1);
    padding: var(--space-2) 0 0 var(--space-3);
    border-left: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
  }

  .turn-process :global(.turn-runtime-indicator) {
    margin-left: var(--space-2);
  }

  @media (max-width: 560px) {
    .turn-process {
      padding-left: var(--space-2);
    }
  }
</style>
