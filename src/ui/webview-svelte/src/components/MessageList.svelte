<script lang="ts">
  import type { Message } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import Icon from './Icon.svelte';
  import { onMount, tick } from 'svelte';

  // Props - Svelte 5 语法
  interface Props {
    messages: Message[];
    /** 空状态配置（可选） */
    emptyState?: {
      icon?: string;
      title?: string;
      hint?: string;
    };
  }
  let { messages, emptyState }: Props = $props();

  // 🛡️ 防御性编程：过滤无效的消息
  const safeMessages = $derived(
    (messages || []).filter(m => !!m && !!m.id)
  );

  /**
   * 生成消息的稳定 Svelte key
   *
   * 核心问题：
   * 1. 用户消息和占位消息共享同一个 requestId，但它们是两条不同的消息
   * 2. 一个 requestId 可能对应多条响应消息（多轮流式、多个 Worker 等）
   *
   * 解决方案：
   * - 用户消息：使用 message.id（唯一）
   * - 占位消息：使用 response-${requestId}（用于与首条真实消息共享 key）
   * - 从占位消息转换的首条真实消息（wasPlaceholder=true）：使用 response-${requestId}
   * - 其他所有消息：使用 message.id（避免 key 冲突）
   */
  function getMessageKey(message: import('../types/message').Message): string {
    // 1. 用户消息：使用自己的 ID（唯一，不会与响应消息冲突）
    const isUserMessage = message.role === 'user' || message.metadata?.role === 'user';
    if (isUserMessage) {
      return message.id;
    }

    // 2. 占位消息：使用 response-${requestId}
    //    这是为了让首条真实消息替换占位消息时，Svelte 认为是同一个元素
    if (message.metadata?.isPlaceholder) {
      const requestId = message.metadata?.requestId;
      if (requestId) {
        return `response-${requestId}`;
      }
      return message.id;
    }

    // 3. 从占位消息转换而来的首条真实消息（wasPlaceholder=true）
    //    使用与占位消息相同的 key，实现 DOM 原地更新
    if (message.metadata?.wasPlaceholder) {
      const requestId = message.metadata?.requestId;
      if (requestId) {
        return `response-${requestId}`;
      }
      return message.id;
    }

    // 4. 其他所有消息（后续流式消息、多轮响应等）：使用 message.id
    //    每条消息有唯一的 key，避免冲突
    return message.id;
  }

  /* 🔧 计算流式消息的内容签名，用于触发滚动
     当任何流式消息的内容变化时，需要重新滚动到底部 */
  const streamingContentSignature = $derived.by(() => {
    const streamingMsgs = safeMessages.filter(m => m.isStreaming);
    if (streamingMsgs.length === 0) return '';
    // 使用内容长度作为签名，避免频繁的字符串比较
    return streamingMsgs.map(m => `${m.id}:${(m.content || '').length}:${(m.blocks || []).length}`).join('|');
  });

  // 空状态默认值
  const emptyIcon = $derived((emptyState?.icon || 'chat') as import('../lib/icons').IconName);
  const emptyTitle = $derived(emptyState?.title || '开始一个新对话');
  const emptyHint = $derived(emptyState?.hint || '在下方输入框中输入你的问题');

  // 容器引用
  let containerRef: HTMLDivElement | null = $state(null);

  // 是否应该自动滚动到底部
  let shouldAutoScroll = $state(true);
  // 是否显示滚动按钮
  let showScrollBtn = $state(false);

  // 监听消息变化，自动滚动到底部
  // 🔧 同时监听流式消息内容变化，确保内容增长时也能自动滚动
  $effect(() => {
    const _len = safeMessages.length;
    const _sig = streamingContentSignature; // 订阅流式内容变化
    void _len;
    void _sig;
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
    showScrollBtn = !isNearBottom && safeMessages.length > 0;
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
    {#if safeMessages.length === 0}
      <div class="empty-state">
        <div class="empty-icon">
          <Icon name={emptyIcon} size={48} />
        </div>
        <p class="empty-text">{emptyTitle}</p>
        <p class="empty-hint">{emptyHint}</p>
      </div>
    {:else}
      {#each safeMessages as message (getMessageKey(message))}
        <MessageItem {message} />
      {/each}
    {/if}
  </div>

  <!-- 滚动按钮：绝对定位在消息列表右下角 -->
  {#if showScrollBtn}
    <button class="scroll-to-bottom" onclick={scrollToBottom} title="回到底部">
      <Icon name="chevron-down" size={16} />
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
    z-index: 100;
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
