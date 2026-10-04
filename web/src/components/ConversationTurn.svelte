<script lang="ts">
  import { formatDuration, formatElapsed } from '../lib/utils';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { Message, TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { normalizeCanonicalTurnItemStrict } from '../shared/protocol/canonical-turn';
  import { getAgentTurnItems } from '../web/agent-api';
  import {
    mergeEarlierCanonicalTurnItems,
    readTurnHistoryWindow,
    turnStoreState,
  } from '../stores/turn-store.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';
  import TurnRuntimeIndicator from './TurnRuntimeIndicator.svelte';
  import TurnRuntimeSummary from './TurnRuntimeSummary.svelte';
  import ConversationAgentGroup from './ConversationAgentGroup.svelte';
  import ConversationToolGroup from './ConversationToolGroup.svelte';
  import ConversationPhase from './ConversationPhase.svelte';
  import {
    inferConversationPresentationRole,
  } from '../lib/conversation-presentation';
  import {
    buildConversationDisclosureBlocks,
    buildConversationStreamEntries,
    isConversationFinalMessage,
    type ConversationDisclosureBlock,
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
  // 与结束时的自动收起规则一致：没有最终回答的轮次默认展开过程。
  let expanded = $state(untrack(() => (
    initialExpanded
    || runtimeActive
    || !items.some((item) => isConversationFinalMessage(item.message))
  )));
  let manualTurnOverride = false;
  let previousLive = false;

  function metadataString(message: Message, key: string): string {
    const value = message.metadata?.[key];
    return typeof value === 'string' ? value.trim() : '';
  }

  // 超长 turn 在历史页里只带最新一段条目；被折叠的更早步骤在这里按需向前补齐。
  const canonicalTurn = $derived(turnStoreState.reducer.turns.find((turn) => turn.turnId === turnId));
  const historyWindow = $derived(readTurnHistoryWindow(canonicalTurn));
  let loadingEarlier = $state(false);
  let loadEarlierFailed = $state(false);

  async function loadEarlierSteps() {
    const window = historyWindow;
    const sessionId = canonicalTurn?.sessionId;
    if (!window || !sessionId || loadingEarlier) return;
    loadingEarlier = true;
    loadEarlierFailed = false;
    try {
      const page = await getAgentTurnItems({
        sessionId,
        turnId,
        beforeItemSeq: window.beforeItemSeq,
      });
      mergeEarlierCanonicalTurnItems({
        sessionId,
        turnId,
        items: page.items.map((item, index) => (
          normalizeCanonicalTurnItemStrict(item, `turnItems[${index}]`)
        )),
        hasMoreBefore: page.hasMoreBefore === true,
        beforeItemSeq: typeof page.beforeItemSeq === 'number' ? page.beforeItemSeq : null,
        omittedItemCount: page.omittedItemCount,
      });
    } catch {
      loadEarlierFailed = true;
    } finally {
      loadingEarlier = false;
    }
  }

  const userItems = $derived(items.filter((item) => item.message.type === 'user_input'));
  const assistantItems = $derived(items.filter((item) => item.message.type !== 'user_input'));
  // 最终区只承载后端标记为 final / error 的输出；进行中的文字按真实顺序留在过程区。
  // 呈现角色是投影写入的事实（两种显示模式共用），这里只按角色布局。
  const presentationItems = $derived(assistantItems.map((item) => ({
    item,
    role: inferConversationPresentationRole(item.message),
  })));
  const finalItems = $derived(
    presentationItems.filter((entry) => entry.role === 'final').map((entry) => entry.item),
  );
  const streamEntries = $derived(buildConversationStreamEntries(presentationItems));

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

  // 进行中自动展开过程；结束后只有存在最终回答时才自动收起——
  // 没有最终回答（例如被中断）的轮次收起后就什么也看不到，所以保持展开。
  $effect(() => {
    const nextLive = isLive;
    if (nextLive !== previousLive) {
      if (!manualTurnOverride) expanded = nextLive || finalItems.length === 0;
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
      // 系统通知独占的“阶段”只是一行静态说明，不是正在进行的模型工作。
      if (block.kind === 'phase' && !block.phase.entries.every(
        (entry) => entry.kind === 'event' && entry.item.message.type === 'system-notice',
      )) {
        return block.phase.key;
      }
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

  {#if historyWindow}
    <div class="turn-earlier-steps" data-testid="turn-earlier-steps">
      <span>{i18n.t('conversationTurn.earlierStepsHidden', { count: historyWindow.omittedItemCount })}</span>
      <button type="button" class="turn-earlier-steps-load" disabled={loadingEarlier} onclick={loadEarlierSteps}>
        {loadingEarlier ? i18n.t('messageList.loadingOlder') : i18n.t('conversationTurn.loadEarlierSteps')}
      </button>
      {#if loadEarlierFailed}
        <span class="turn-earlier-steps-error">{i18n.t('conversationTurn.loadEarlierStepsFailed')}</span>
      {/if}
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

  <!-- 耗时已经显示在轮次标题上；底部只补充完成时间，不再把同一个耗时重复一遍。 -->
  {#if !isLive && durationMs !== null && completedAt !== null}
    <TurnRuntimeSummary durationMs={durationMs} {completedAt} showDuration={false} />
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

  .turn-earlier-steps {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .turn-earlier-steps-load {
    color: var(--primary);
    background: none;
    border: 0;
    padding: 0;
    cursor: pointer;
    font: inherit;
  }

  .turn-earlier-steps-load:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .turn-earlier-steps-error {
    color: var(--error);
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
