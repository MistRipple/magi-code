<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import { untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ConversationToolItem from './ConversationToolItem.svelte';

  interface Props {
    items: TimelineRenderItem[];
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    canEditMessage: (item: TimelineRenderItem) => boolean;
    editMessage: (item: TimelineRenderItem) => void;
    continueInterruptedSession: () => void;
  }

  let {
    items,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
    canEditMessage,
    editMessage,
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
    onclick={toggle}
  >
    <span class="tool-group-icon"><Icon name="plus" size={14} /></span>
    <span class="tool-group-label">{groupLabel}</span>
    <span class="tool-group-count">{i18n.t('messageList.turnDisclosure.toolCount', { count: items.length })}</span>
    <span class="tool-group-chevron" class:rotated={expanded}>
      <Icon name="chevron-right" size={13} />
    </span>
  </button>

  {#if expanded}
    <div class="tool-group-list">
      {#each items as item (item.key)}
        <ConversationToolItem
          {item}
          {readOnly}
          {displayContext}
          {filePreviewScopeForItem}
          {canEditMessage}
          {editMessage}
          {continueInterruptedSession}
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
    gap: 9px;
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

  .tool-group-icon {
    display: inline-flex;
    color: var(--primary);
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
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .tool-group-chevron {
    display: inline-flex;
    margin-left: auto;
    transition: transform var(--transition-fast);
  }

  .tool-group-chevron.rotated {
    transform: rotate(90deg);
  }

  .tool-group-list {
    margin: 0 0 7px 22px;
    padding: 3px 0 3px 14px;
    border-left: 1px solid color-mix(in srgb, var(--border) 80%, transparent);
  }
</style>
