<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import ToolCallRenderer from './ToolCallRenderer.svelte';

  interface Props {
    items: TimelineRenderItem[];
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
  }

  let { items, filePreviewScopeForItem }: Props = $props();
</script>

<section class="conversation-agent-group">
  {#if items.length > 1}
    <div class="agent-group-label">{i18n.t('messageList.agentGroup.label', { count: items.length })}</div>
  {/if}
  <div class="agent-group-list" role="list">
    {#each items as item (item.key)}
      <div role="listitem">
        {#each item.message.blocks || [] as block (block.id)}
          {#if block?.toolCall?.name?.toLowerCase().includes('agent_spawn')}
            <ToolCallRenderer
              {block}
              filePreviewScope={filePreviewScopeForItem(item)}
              agentChildStatus={item.message.metadata?.agentChildStatus}
              agentChildStatusUpdatedAt={item.message.metadata?.agentChildStatusUpdatedAt}
              compactAgent
            />
          {/if}
        {/each}
      </div>
    {/each}
  </div>
</section>

<style>
  .conversation-agent-group { min-width: 0; }
  .agent-group-label {
    padding-top: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }
  .agent-group-list {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
</style>
