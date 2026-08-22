<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { ContentBlock, TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { resolveToolDisplayName } from '../lib/tool-display-name';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';

  interface Props {
    item: TimelineRenderItem;
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    canEditMessage: (item: TimelineRenderItem) => boolean;
    editMessage: (item: TimelineRenderItem) => void;
    continueInterruptedSession: () => void;
  }

  let {
    item,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
    canEditMessage,
    editMessage,
    continueInterruptedSession,
  }: Props = $props();

  let expanded = $state(untrack(() => false));
  const message = $derived(item.message);

  function firstToolBlock(blocks: ContentBlock[] | undefined): ContentBlock | undefined {
    return (blocks || []).find((block) => block.type === 'tool_call' && block.toolCall?.name);
  }

  const toolBlock = $derived(firstToolBlock(message.blocks));
  const toolName = $derived(
    (typeof message.metadata?.toolName === 'string' ? message.metadata.toolName.trim() : '')
      || toolBlock?.toolCall?.name
      || 'tool',
  );
  const argumentsRecord = $derived(
    toolBlock?.toolCall?.arguments && typeof toolBlock.toolCall.arguments === 'object'
      ? toolBlock.toolCall.arguments as Record<string, unknown>
      : {},
  );
  const target = $derived.by(() => {
    const metadataPath = typeof message.metadata?.filePath === 'string'
      ? message.metadata.filePath.trim()
      : '';
    if (metadataPath) return metadataPath;
    for (const key of ['file_path', 'path', 'command', 'query', 'url']) {
      const value = argumentsRecord[key];
      if (typeof value === 'string' && value.trim()) return value.trim();
    }
    return '';
  });

  const label = $derived.by(() => {
    const displayName = resolveToolDisplayName(toolName, i18n);
    if (target) return `${displayName} · ${target}`;
    return displayName;
  });
  const isActive = $derived(
    message.isStreaming
      || message.metadata?.turnItemStatus === 'pending'
      || message.metadata?.turnItemStatus === 'running',
  );
  const statusLabel = $derived.by(() => {
    if (message.metadata?.turnItemStatus === 'failed' || message.metadata?.turnItemStatus === 'cancelled') {
      return i18n.t('messageList.turnDisclosure.toolFailed');
    }
    if (isActive) return i18n.t('messageList.turnDisclosure.toolRunning');
    return i18n.t('messageList.turnDisclosure.toolCompleted');
  });

  function toggle(): void {
    expanded = !expanded;
  }
</script>

<div class="conversation-tool-item" class:expanded class:active={isActive}>
  <button
    type="button"
    class="tool-item-header"
    aria-expanded={expanded}
    onclick={toggle}
  >
    <span class="tool-item-icon"><Icon name="tool" size={14} /></span>
    <span class="tool-item-label">{label}</span>
    <span class="tool-item-status">{statusLabel}</span>
    <span class="tool-item-chevron" class:rotated={expanded}>
      <Icon name="chevron-right" size={12} />
    </span>
  </button>

  {#if expanded}
    <div class="tool-item-detail">
      <MessageItem
        message={message}
        {readOnly}
        {displayContext}
        filePreviewScope={filePreviewScopeForItem(item)}
        canEdit={canEditMessage(item)}
        onEdit={() => editMessage(item)}
        onContinueInterrupted={continueInterruptedSession}
        hideResponseDuration
      />
    </div>
  {/if}
</div>

<style>
  .tool-item-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 32px;
    gap: 8px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
    cursor: pointer;
  }

  .tool-item-header:hover,
  .tool-item-header:focus-visible {
    color: var(--foreground);
  }

  .tool-item-header:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 2px;
  }

  .tool-item-icon {
    display: inline-flex;
    flex: 0 0 auto;
    color: var(--foreground-muted);
  }

  .conversation-tool-item.active .tool-item-icon {
    color: var(--primary);
  }

  .tool-item-label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tool-item-status {
    flex: 0 0 auto;
    color: var(--foreground-muted);
    font-size: 11px;
  }

  .tool-item-chevron {
    display: inline-flex;
    flex: 0 0 auto;
    margin-left: auto;
    transition: transform var(--transition-fast);
  }

  .tool-item-chevron.rotated {
    transform: rotate(90deg);
  }

  .tool-item-detail {
    padding: 4px 0 8px 22px;
    border-left: 1px solid color-mix(in srgb, var(--border) 75%, transparent);
  }

  .tool-item-detail :global(.message-item) {
    margin-top: 0;
  }

  .tool-item-detail :global(.message-item.assistant) {
    padding-inline: 0;
  }
</style>
