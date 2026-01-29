<script lang="ts">
  import type { Message } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import Icon from './Icon.svelte';
  import { onMount } from 'svelte';

  // Props - Svelte 5 语法
  interface Props {
    messages: Message[];
  }
  let { messages }: Props = $props();

  // 容器引用
  let containerRef: HTMLDivElement | null = $state(null);
  
  // 是否应该自动滚动到底部
  let shouldAutoScroll = $state(true);

  // 监听消息变化，自动滚动到底部
  $effect(() => {
    // 依赖 messages 长度变化
    const _len = messages.length;
    void _len; // 消除未使用变量警告

    if (shouldAutoScroll && containerRef) {
      // 使用 requestAnimationFrame 确保 DOM 更新后再滚动
      requestAnimationFrame(() => {
        if (containerRef) {
          containerRef.scrollTop = containerRef.scrollHeight;
        }
      });
    }
  });

  // 检测用户是否手动滚动
  function handleScroll(event: Event) {
    const target = event.target as HTMLDivElement;
    const { scrollTop, scrollHeight, clientHeight } = target;
    
    // 距离底部小于 50px 时启用自动滚动
    const isNearBottom = scrollHeight - scrollTop - clientHeight < 50;
    shouldAutoScroll = isNearBottom;
  }

  onMount(() => {
    // 初始滚动到底部
    if (containerRef) {
      containerRef.scrollTop = containerRef.scrollHeight;
    }
  });
</script>

<div 
  class="message-list"
  bind:this={containerRef}
  onscroll={handleScroll}
>
  {#if messages.length === 0}
    <div class="empty-state">
      <div class="empty-icon">
        <Icon name="chat" size={48} />
      </div>
      <p class="empty-text">开始一个新对话</p>
      <p class="empty-hint">在下方输入框中输入你的问题</p>
    </div>
  {:else}
    {#each messages as message (message.id)}
      <MessageItem {message} />
    {/each}
  {/if}
  
  {#if !shouldAutoScroll && messages.length > 0}
    <button 
      class="scroll-to-bottom"
      onclick={() => {
        shouldAutoScroll = true;
        if (containerRef) {
          containerRef.scrollTop = containerRef.scrollHeight;
        }
      }}
    >
      ↓ 滚动到底部
    </button>
  {/if}
</div>

<style>
  .message-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    scroll-behavior: smooth;
    padding: var(--space-4);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    text-align: center;
    color: var(--foreground-muted);
    padding: var(--space-8);
  }

  .empty-icon {
    width: var(--icon-2xl);
    height: var(--icon-2xl);
    margin-bottom: var(--space-4);
    opacity: 0.3;
    color: var(--foreground-muted);
  }

  .empty-text {
    font-size: var(--text-lg);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.7;
  }

  .scroll-to-bottom {
    position: fixed;
    bottom: 80px;
    right: var(--space-5);
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    background: var(--primary);
    color: white;
    border: none;
    border-radius: var(--radius-full);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    box-shadow: var(--shadow-lg);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .scroll-to-bottom:hover {
    background: var(--primary-hover);
    transform: translateY(-2px);
  }
</style>

