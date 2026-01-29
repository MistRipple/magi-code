<script lang="ts">
  import type { Message } from '../types/message';
  import MessageItem from './MessageItem.svelte';

  // Props
  interface Props {
    messages: Message[];
  }

  let { messages }: Props = $props();
</script>

<div class="agent-tab">
  <div class="agent-content">
    {#if messages.length === 0}
      <div class="empty-state">
        <p>暂无输出</p>
      </div>
    {:else}
      <div class="message-list">
        {#each messages as message (message.id)}
          <MessageItem {message} />
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .agent-tab {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .agent-content {
    flex: 1;
    overflow-y: auto;
    padding: var(--spacing-md);
  }

  .empty-state {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--vscode-descriptionForeground, #888);
    font-size: var(--font-size-sm);
  }

  .message-list {
    display: flex;
    flex-direction: column;
    gap: var(--spacing-md);
  }
</style>

