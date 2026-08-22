<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';

  export interface ConversationStageModel {
    key: string;
    index: number;
    items: TimelineRenderItem[];
  }

  interface Props {
    stage: ConversationStageModel;
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    canEditMessage: (item: TimelineRenderItem) => boolean;
    editMessage: (item: TimelineRenderItem) => void;
    continueInterruptedSession: () => void;
    initialExpanded?: boolean;
  }

  let {
    stage,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
    canEditMessage,
    editMessage,
    continueInterruptedSession,
    initialExpanded = false,
  }: Props = $props();

  // 阶段和轮次一样，默认只展示摘要；用户展开某阶段后，保留自己的选择。
  let expanded = $state(untrack(() => initialExpanded));

  const isStreaming = $derived(stage.items.some((item) => {
    if (item.message.isStreaming) return true;
    const status = item.message.metadata?.turnItemStatus;
    return status === 'pending' || status === 'running';
  }));
  const toolCount = $derived(stage.items.filter((item) => (
    item.message.type === 'tool_call'
      || (item.message.blocks || []).some((block) => (
        block.type === 'tool_call' || block.type === 'tool_result' || block.type === 'file_change'
      ))
  )).length);
  const stageSummary = $derived.by(() => {
    if (isStreaming) return i18n.t('messageList.turnDisclosure.stageRunning');
    if (toolCount > 0) return i18n.t('messageList.turnDisclosure.toolCount', { count: toolCount });
    return i18n.t('messageList.turnDisclosure.itemCount', { count: stage.items.length });
  });

  function toggle(): void {
    expanded = !expanded;
  }
</script>

<section class="conversation-stage" class:expanded class:streaming={isStreaming}>
  <button
    type="button"
    class="conversation-stage-header"
    aria-expanded={expanded}
    onclick={toggle}
  >
    <span class="stage-chevron" class:rotated={expanded}>
      <Icon name="chevron-right" size={12} />
    </span>
    <span class="stage-icon">
      <Icon name={toolCount > 0 ? 'tool' : 'list'} size={13} />
    </span>
    <span class="stage-title">
      {i18n.t('messageList.turnDisclosure.stage', { index: stage.index })}
    </span>
    <span class="stage-summary">{stageSummary}</span>
  </button>

  {#if expanded}
    <div class="conversation-stage-content">
      {#each stage.items as item (item.key)}
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
    </div>
  {/if}
</section>

<style>
  .conversation-stage {
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--border) 82%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface-1, transparent) 68%, transparent);
  }

  .conversation-stage.streaming {
    border-color: color-mix(in srgb, var(--primary) 46%, var(--border));
  }

  .conversation-stage-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 34px;
    gap: var(--space-2);
    padding: 0 var(--space-3);
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
    cursor: pointer;
  }

  .conversation-stage-header:hover,
  .conversation-stage-header:focus-visible {
    background: var(--surface-hover, rgba(255, 255, 255, 0.04));
    color: var(--foreground);
  }

  .conversation-stage-header:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: -1px;
  }

  .stage-chevron {
    display: inline-flex;
    flex: 0 0 auto;
    transition: transform var(--transition-fast);
  }

  .stage-chevron.rotated {
    transform: rotate(90deg);
  }

  .stage-icon {
    display: inline-flex;
    flex: 0 0 auto;
    color: var(--foreground-muted);
  }

  .stage-title {
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  .stage-summary {
    margin-left: auto;
    overflow: hidden;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .conversation-stage-content {
    padding: 0 var(--space-3) var(--space-3);
    border-top: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
  }

  .conversation-stage-content :global(.message-item:first-child) {
    margin-top: var(--space-2);
  }
</style>
