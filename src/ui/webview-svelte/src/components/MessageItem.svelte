<script lang="ts">
  import type { Message } from '../types/message';
  import MarkdownContent from './MarkdownContent.svelte';
  import StreamingIndicator from './StreamingIndicator.svelte';

  // Props
  interface Props {
    message: Message;
  }
  let { message }: Props = $props();

  // 派生状态
  const isUser = $derived(message.role === 'user');
  const isStreaming = $derived(message.isStreaming);
  
  // 格式化时间戳
  function formatTime(timestamp: number): string {
    const date = new Date(timestamp);
    return date.toLocaleTimeString('zh-CN', { 
      hour: '2-digit', 
      minute: '2-digit' 
    });
  }

  // 获取来源图标
  function getSourceIcon(source: string): string {
    switch (source) {
      case 'claude': return '🟣';
      case 'codex': return '🟢';
      case 'gemini': return '🔵';
      default: return '🤖';
    }
  }
</script>

<div 
  class="message-item"
  class:user={isUser}
  class:assistant={!isUser}
  class:streaming={isStreaming}
  data-message-id={message.id}
>
  <div class="message-header">
    <span class="message-source">
      {#if isUser}
        👤 你
      {:else}
        {getSourceIcon(message.source)} {message.source}
      {/if}
    </span>
    <span class="message-time">{formatTime(message.timestamp)}</span>
  </div>
  
  <div class="message-content">
    {#if message.content}
      <MarkdownContent content={message.content} {isStreaming} />
    {/if}
    
    {#if isStreaming}
      <StreamingIndicator />
    {/if}
  </div>
</div>

<style>
  .message-item {
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    transition: background var(--transition-fast);
  }

  .message-item.user {
    background: var(--user-message-bg);
    margin-left: var(--space-6);
  }

  .message-item.assistant {
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    margin-right: var(--space-6);
  }

  .message-item.streaming {
    border-color: var(--info);
  }

  .message-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-3);
    font-size: var(--text-sm);
  }

  .message-source {
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .message-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .message-content {
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
    overflow-wrap: break-word;
    font-size: var(--text-base);
  }

  /* 流式输出时的渐变遮罩效果 */
  .message-item.streaming .message-content {
    position: relative;
  }

  .message-item.streaming .message-content::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 20px;
    background: linear-gradient(transparent, var(--assistant-message-bg));
    pointer-events: none;
    opacity: 0.5;
  }
</style>

