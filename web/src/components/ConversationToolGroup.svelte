<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';
  import { resolveConversationToolGroupLabel } from '../lib/conversation-disclosure';

  interface Props {
    items: TimelineRenderItem[];
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    continueInterruptedSession: () => void;
  }

  let {
    items,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
    continueInterruptedSession,
  }: Props = $props();

  let expanded = $state(untrack(() => false));

  const contentId = $derived(`conversation-tool-group-${(items[0]?.key || 'empty').replace(/[^a-zA-Z0-9_-]/gu, '-')}`);
  const groupLabel = $derived(
    resolveConversationToolGroupLabel(items, i18n.t.bind(i18n)),
  );

  function toggle(): void {
    expanded = !expanded;
  }
</script>

<section class="conversation-tool-group" class:expanded>
  <button
    type="button"
    class="tool-group-header"
    aria-expanded={expanded}
    aria-controls={contentId}
    onclick={toggle}
  >
    <span class="tool-group-chevron" class:rotated={expanded}>
      <Icon name="chevron-right" size={12} />
    </span>
    <span class="tool-group-label">{groupLabel}</span>
  </button>

  {#if expanded}
    <div class="tool-group-list" id={contentId}>
      {#each items as item (item.key)}
        <MessageItem
          message={item.message}
          {readOnly}
          {displayContext}
          filePreviewScope={filePreviewScopeForItem(item)}
          onContinueInterrupted={continueInterruptedSession}
          hideResponseDuration
        />
      {/each}
    </div>
  {/if}
</section>

<style>
  .conversation-tool-group {
    margin: 0;
  }

  .tool-group-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 28px;
    gap: 7px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
    cursor: pointer;
  }

  .tool-group-header:hover,
  .tool-group-header:focus-visible {
    color: var(--foreground);
  }

  .tool-group-header:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 3px;
  }

  .tool-group-chevron {
    display: inline-flex;
    flex: 0 0 12px;
    color: var(--foreground-muted);
    opacity: 0.62;
    transition: transform var(--transition-fast), opacity var(--transition-fast);
  }

  .tool-group-label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tool-group-header:hover .tool-group-chevron,
  .tool-group-header:focus-visible .tool-group-chevron,
  .tool-group-chevron.rotated {
    opacity: 1;
  }

  .tool-group-chevron.rotated {
    transform: rotate(90deg);
  }

  .tool-group-list {
    margin: 0;
    padding: 0 0 1px 8px;
    border-left: 0;
  }

  .conversation-tool-group .tool-group-list :global(.message-item.assistant) {
    margin-top: 0;
    padding-inline: 0;
  }

  .conversation-tool-group .tool-group-list :global(.tool-call) {
    margin-top: 0;
    overflow: visible;
    border: 0;
    border-radius: 0;
    background: transparent;
  }

  .conversation-tool-group .tool-group-list :global(.tool-header) {
    min-height: 34px;
    gap: 8px;
    padding: 4px 0;
    transition: color var(--transition-fast);
  }

  /*
   * 摘要模式中的工具行是过程信息，不是列表选中项。
   * 这里移除通用 tool-header 的整行 hover 背景，避免无内缩的底色直接顶到两侧；
   * 交互反馈改为颜色和指针，键盘导航仍保留可见焦点。
   */
  .conversation-tool-group .tool-group-list :global(.tool-header:hover) {
    background: transparent;
    color: var(--foreground);
  }

  .conversation-tool-group .tool-group-list :global(.tool-header:hover .tool-icon),
  .conversation-tool-group .tool-group-list :global(.tool-header:focus-visible .tool-icon) {
    color: var(--foreground);
  }

  .conversation-tool-group .tool-group-list :global(.tool-header:focus-visible) {
    outline: 1px solid color-mix(in srgb, var(--primary) 72%, transparent);
    outline-offset: 2px;
    border-radius: var(--radius-sm);
  }

  .conversation-tool-group .tool-group-list :global(.chevron) {
    flex: 0 0 12px;
    color: var(--foreground-muted);
  }

  .conversation-tool-group .tool-group-list :global(.tool-icon) {
    flex: 0 0 14px;
    color: var(--foreground-muted);
    opacity: 0.78;
  }

  .conversation-tool-group .tool-group-list :global(.status-dot) {
    width: 6px;
    height: 6px;
  }

  .conversation-tool-group .tool-group-list :global(.tool-content) {
    margin: 0 0 8px;
    padding: 7px 0 7px 8px;
    border-top: 0;
    border-left: 1px solid color-mix(in srgb, var(--border) 76%, transparent);
    background: transparent;
  }

  .conversation-tool-group .tool-group-list :global(.tool-content.diagram-content) {
    margin-left: 0;
    padding: 0;
    border-left: 0;
    background: var(--code-bg);
  }

  @media (max-width: 560px) {
    .tool-group-header {
      min-height: 32px;
    }

    .tool-group-list {
      padding-left: 6px;
    }

    .conversation-tool-group .tool-group-list :global(.tool-content) {
      padding-left: 6px;
    }
  }
</style>
