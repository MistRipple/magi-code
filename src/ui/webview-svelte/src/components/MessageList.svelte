<script lang="ts">
  import type { Message } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import Icon from './Icon.svelte';
  import { onMount, tick } from 'svelte';

  // Props - Svelte 5 语法
  interface Props {
    messages: Message[];
  }
  let { messages }: Props = $props();

  // 容器引用
  let containerRef: HTMLDivElement | null = $state(null);

  // 是否应该自动滚动到底部
  let shouldAutoScroll = $state(true);
  // 是否显示滚动按钮
  let showScrollBtn = $state(false);

  // 监听消息变化，自动滚动到底部
  $effect(() => {
    const _len = messages.length;
    void _len;
    if (shouldAutoScroll && containerRef) {
      tick().then(() => {
        if (containerRef) {
          // 直接定位，不要平滑滚动
          containerRef.scrollTop = containerRef.scrollHeight;
        }
      });
    }
  });

  // 检测用户是否手动滚动
  function handleScroll(event: Event) {
    const target = event.target as HTMLDivElement;
    const { scrollTop, scrollHeight, clientHeight } = target;
    const isNearBottom = scrollHeight - scrollTop - clientHeight < 100;
    shouldAutoScroll = isNearBottom;
    showScrollBtn = !isNearBottom && messages.length > 0;
  }

  // 滚动到底部
  function scrollToBottom() {
    shouldAutoScroll = true;
    if (containerRef) {
      containerRef.scrollTo({ top: containerRef.scrollHeight, behavior: 'smooth' });
    }
  }

  onMount(() => {
    // 初始化时直接定位到底部（不要动画）
    if (containerRef) {
      containerRef.style.scrollBehavior = 'auto'; // 强制关闭平滑滚动
      containerRef.scrollTop = containerRef.scrollHeight;
      // 恢复平滑滚动 (下一帧)
      requestAnimationFrame(() => {
        if (containerRef) containerRef.style.scrollBehavior = '';
      });
    }
  });
</script>

<div class="message-list-wrapper">
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
  </div>

  <!-- 滚动按钮：绝对定位在消息列表右下角 -->
  {#if showScrollBtn}
    <button class="scroll-to-bottom" onclick={scrollToBottom} title="滚动到底部">
      <Icon name="chevron-down" size={12} />
      <span class="scroll-text">新消息</span>
    </button>
  {/if}
</div>

<style>
  .message-list-wrapper {
    position: relative;
    height: 100%;
    display: flex;
    flex-direction: column;
  }

  .message-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    /* 右侧减少间距以补偿滚动条宽度，使内容视觉对称 */
    padding: var(--space-4);
    padding-right: var(--space-2);
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

  /* 滚动按钮 - 绝对定位在消息列表右下角 */
  .scroll-to-bottom {
    position: absolute;
    bottom: 20px;
    right: 20px;
    display: flex;
    align-items: center;
    gap: 4px;
    height: 32px;
    padding: 0 12px;
    background: var(--surface-2);
    color: var(--primary);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    font-size: 12px;
    font-weight: var(--font-medium);
    box-shadow: var(--shadow-lg);
    cursor: pointer;
    transition: all var(--transition-fast);
    z-index: 100; /* 提高层级 */
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

  .scroll-text {
    white-space: nowrap;
  }
</style>

