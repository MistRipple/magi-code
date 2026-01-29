<script lang="ts">
  import { tick } from 'svelte';
  import type { Message } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import Icon from './Icon.svelte';

  // Props
  interface Props {
    messages: Message[];
  }

  let { messages }: Props = $props();

  // 滚动相关状态
  let contentEl: HTMLDivElement | null = $state(null);
  let showScrollBtn = $state(false);
  let isUserScrolling = $state(false);
  let hasInitialScrolled = false;

  // 检测滚动位置
  function handleScroll() {
    if (!contentEl) return;
    const { scrollTop, scrollHeight, clientHeight } = contentEl;
    const distanceFromBottom = scrollHeight - scrollTop - clientHeight;

    // 距离底部超过 100px 时显示按钮
    showScrollBtn = distanceFromBottom > 100;
    isUserScrolling = distanceFromBottom > 50;
  }

  // 滚动到底部
  function scrollToBottom() {
    if (contentEl) {
      contentEl.scrollTo({ top: contentEl.scrollHeight, behavior: 'smooth' });
    }
  }

  // 立即滚动到底部（无动画）
  function scrollToBottomImmediate() {
    if (contentEl) {
      contentEl.scrollTop = contentEl.scrollHeight;
    }
  }

  // 当 contentEl 绑定后立即滚动到底部
  $effect(() => {
    if (contentEl && messages.length > 0 && !hasInitialScrolled) {
      // 等待 DOM 完全渲染
      tick().then(() => {
        setTimeout(() => {
          scrollToBottomImmediate();
          hasInitialScrolled = true;
        }, 0);
      });
    }
  });

  // 消息变化时自动滚动（如果用户没有手动滚动）
  let prevMessageCount = 0;
  $effect(() => {
    const currentCount = messages.length;
    // 只有消息数量增加时才自动滚动
    if (hasInitialScrolled && currentCount > prevMessageCount && contentEl && !isUserScrolling) {
      tick().then(() => {
        contentEl?.scrollTo({ top: contentEl.scrollHeight, behavior: 'smooth' });
      });
    }
    prevMessageCount = currentCount;
  });
</script>

<div class="agent-tab">
  <div class="agent-content" bind:this={contentEl} onscroll={handleScroll}>
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

  <!-- 回到底部悬浮按钮 -->
  {#if showScrollBtn}
    <button class="scroll-to-bottom" onclick={scrollToBottom} title="回到底部">
      <Icon name="chevron-down" size={16} />
    </button>
  {/if}
</div>

<style>
  .agent-tab {
    position: relative;
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
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .message-list {
    display: flex;
    flex-direction: column;
    gap: var(--spacing-md);
  }

  /* 回到底部按钮 */
  .scroll-to-bottom {
    position: absolute;
    bottom: 20px;
    right: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    padding: 0;
    background: var(--surface-2);
    color: var(--primary);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    box-shadow: var(--shadow-lg);
    cursor: pointer;
    transition: all var(--transition-fast);
    z-index: 10;
    animation: slideUp 0.2s ease-out;
  }

  @keyframes slideUp {
    from { opacity: 0; transform: translateY(8px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .scroll-to-bottom:hover {
    background: var(--primary);
    color: white;
    border-color: var(--primary);
    transform: translateY(-2px);
  }
</style>

