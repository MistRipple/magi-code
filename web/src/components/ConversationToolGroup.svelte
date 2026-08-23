<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';

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

  function toolName(item: TimelineRenderItem): string {
    const metadataName = typeof item.message.metadata?.toolName === 'string'
      ? item.message.metadata.toolName.trim()
      : '';
    if (metadataName) return metadataName;
    for (const block of item.message.blocks || []) {
      if (block.type === 'tool_call' && block.toolCall?.name) return block.toolCall.name;
    }
    return '';
  }

  const names = $derived(items.map(toolName).filter(Boolean));
  const contentId = $derived(`conversation-tool-group-${(items[0]?.key || 'empty').replace(/[^a-zA-Z0-9_-]/gu, '-')}`);
  const groupLabel = $derived.by(() => {
    const allFileTools = names.length > 0 && names.every((name) => (
      /(?:file|patch|edit|write|remove|mkdir|move|copy)/iu.test(name)
    ));
    const allCommandTools = names.length > 0 && names.every((name) => (
      /(?:shell|exec|command|process)/iu.test(name)
    ));
    if (items.length === 1) return i18n.t('messageList.turnDisclosure.toolGroupSingle');
    if (allFileTools) return i18n.t('messageList.turnDisclosure.toolGroupFiles');
    if (allCommandTools) return i18n.t('messageList.turnDisclosure.toolGroupCommands');
    return i18n.t('messageList.turnDisclosure.toolGroupMixed');
  });

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
    <span class="tool-group-count">{i18n.t('messageList.turnDisclosure.toolCount', { count: items.length })}</span>
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
    margin: 4px 0 7px;
  }

  .tool-group-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 35px;
    gap: 8px;
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

  .tool-group-count {
    flex: 0 0 auto;
    margin-left: auto;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
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
    margin: 0 0 7px;
    padding: 2px 0 2px 18px;
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
    padding: 7px 0 7px 18px;
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
    .tool-group-list {
      padding-left: 14px;
    }

    .conversation-tool-group .tool-group-list :global(.tool-content) {
      padding-left: 12px;
    }
  }
</style>
