<script lang="ts">
  import type { Message, ScrollPositions, Task } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import Icon from './Icon.svelte';
  import { tick } from 'svelte';
  import { clearMessageJump, getState, messagesState, updatePanelScrollState } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { deriveWorkerPanelState, getMessageRequestId } from '../lib/worker-panel-state';
  import { ensureArray } from '../lib/utils';

  // Props - Svelte 5 语法
  interface Props {
    workerName?: 'claude' | 'codex' | 'gemini';
    messages: Message[];
    /** 空状态配置（可选） */
    emptyState?: {
      icon?: string;
      title?: string;
      hint?: string;
    };
    /** 是否为只读模式（主对话区模式），隐藏冗余操作按钮 */
    readOnly?: boolean;
    /** 显示上下文：thread=主对话区, worker=Worker面板 */
    displayContext?: 'thread' | 'worker';
    /** 当前面板是否处于可见激活状态（用于 display:none -> visible 场景下的滚动恢复） */
    isActive?: boolean;
  }
  let { workerName, messages, emptyState, readOnly = false, displayContext = 'thread', isActive = true }: Props = $props();
  const appState = getState();

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
    // 方案 B：使用 MessageType.USER_INPUT 判断用户消息
    const isUserMessage = message.type === 'user_input';
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

  // 对话级处理指示器
  // - thread: 全局 isProcessing 驱动，表示「对话仍在进行」
  // - worker: 当前请求级别（同一次请求内跨多轮不重置）
  // 仅当「当前无流式消息卡片」时显示，避免与卡片内流式动画重复导致视觉留白
  const lastMessage = $derived.by(() => safeMessages.length > 0 ? safeMessages[safeMessages.length - 1] : null);
  const hasBottomStreamingMessage = $derived(Boolean(lastMessage?.isStreaming));
  const pendingRequestIds = $derived.by(() => Array.from(messagesState.pendingRequests));
  const pendingRequestIdSet = $derived.by(() => new Set(pendingRequestIds));
  const tasks = $derived(ensureArray(appState.tasks) as Task[]);
  const workerPanelState = $derived.by(() => deriveWorkerPanelState({
    messages: safeMessages,
    workerName,
    pendingRequestIds,
    tasks,
  }));
  const workerRuntimeMap = $derived(appState.workerRuntime);
  const workerRuntime = $derived.by(() => (workerName ? workerRuntimeMap[workerName] : null));

  const latestRoundAnchorMessage = $derived.by(() => {
    if (displayContext === 'worker') {
      return workerPanelState.latestRoundAnchorMessage;
    }
    for (let i = safeMessages.length - 1; i >= 0; i -= 1) {
      const message = safeMessages[i];
      if (message.type === 'user_input') {
        return message;
      }
    }
    return null;
  });

  const latestRoundRequestId = $derived.by(() => getMessageRequestId(latestRoundAnchorMessage || undefined));
  const panelHasPendingRequest = $derived.by(() => {
    if (displayContext === 'worker') {
      return Boolean(workerRuntime?.hasPendingRequest);
    }
    if (!latestRoundRequestId) return false;
    return pendingRequestIdSet.has(latestRoundRequestId);
  });

  // Worker 面板是否在处理中：
  // 仅当当前面板对应的 Worker 正在被激活处理、或本面板仍有当前请求挂起时，才显示处理指示器并计时。
  // 关键修复：禁止仅凭“最后一条是 instruction”就判定活跃，避免旧轮次导致多面板同步计时。
  const isExecuting = $derived.by(() => {
    if (displayContext !== 'worker') return false;
    return Boolean(workerRuntime?.isExecuting);
  });
  const streamingIndicatorMessageId = $derived.by(() => {
    if (!hasBottomStreamingMessage || !lastMessage) return null;
    return lastMessage.id;
  });

  // 防抖：底部消息的流式状态变 false 后延迟 300ms 再显示底部兜底指示器，
  // 避免工具调用间隙短暂状态切换导致视觉闪烁。
  let debouncedNoBottomStreaming = $state(true);
  let noStreamingTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const noStreaming = !hasBottomStreamingMessage;
    if (noStreaming) {
      noStreamingTimer = setTimeout(() => { debouncedNoBottomStreaming = true; }, 300);
    } else {
      if (noStreamingTimer) { clearTimeout(noStreamingTimer); noStreamingTimer = null; }
      debouncedNoBottomStreaming = false;
    }
    return () => { if (noStreamingTimer) { clearTimeout(noStreamingTimer); noStreamingTimer = null; } };
  });

  const showProcessingIndicator = $derived(
    displayContext === 'worker'
      ? isExecuting && debouncedNoBottomStreaming
      : messagesState.isProcessing && safeMessages.length > 0 && debouncedNoBottomStreaming
  );

  // 计时起点：
  // - thread: 从最后一条用户消息的时间戳开始
  // - worker: 从最新一条 worker 输出开始，每次新输出重置
  //   无输出时兜底到最后一条任务指令，便于观测等待时长
  const timerStartTime = $derived.by(() => {
    if (displayContext === 'worker') {
      return workerRuntime?.timerStartAt || 0;
    }

    // 主对话区：优先按当前请求的最后一条用户消息计时，确保按轮次重置
    if (panelHasPendingRequest && latestRoundAnchorMessage) {
      return latestRoundAnchorMessage.timestamp;
    }

    // 兜底：没有可匹配请求时，使用处理开始时间
    if (messagesState.thinkingStartAt) {
      return messagesState.thinkingStartAt;
    }
    return 0;
  });

  // 计时器运行条件与底部三点展示解耦：
  // 有流式消息时计时也要持续更新，避免只显示三点不显示耗时。
  const shouldRunTimer = $derived.by(() => {
    if (timerStartTime <= 0) return false;
    if (displayContext === 'worker') {
      return isExecuting || Boolean(workerRuntime?.hasStreaming);
    }
    return messagesState.isProcessing || hasBottomStreamingMessage;
  });

  let elapsedSeconds = $state(0);
  let timerInterval: ReturnType<typeof setInterval> | null = null;

  $effect(() => {
    const shouldRun = shouldRunTimer;
    if (shouldRun) {
      // 立即计算一次
      elapsedSeconds = Math.floor((Date.now() - timerStartTime) / 1000);
      timerInterval = setInterval(() => {
        elapsedSeconds = Math.floor((Date.now() - timerStartTime) / 1000);
      }, 1000);
    } else {
      if (timerInterval) {
        clearInterval(timerInterval);
        timerInterval = null;
      }
      elapsedSeconds = 0;
    }
    return () => {
      if (timerInterval) {
        clearInterval(timerInterval);
        timerInterval = null;
      }
    };
  });

  function formatElapsed(seconds: number): string {
    if (seconds < 60) return `${seconds}s`;
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}m ${s}s`;
  }

  // 空状态默认值
  const emptyIcon = $derived((emptyState?.icon || 'chat') as import('../lib/icons').IconName);
  const emptyTitle = $derived(emptyState?.title || i18n.t('messageList.empty.title'));
  const emptyHint = $derived(emptyState?.hint || i18n.t('messageList.empty.hint'));
  const panelKey = $derived.by((): keyof ScrollPositions => (displayContext === 'worker' ? (workerName || 'claude') : 'thread'));
  const persistedScrollTop = $derived(messagesState.scrollPositions[panelKey] || 0);
  const persistedScrollAnchor = $derived(messagesState.scrollAnchors[panelKey]);
  const shouldAutoScroll = $derived(messagesState.autoScrollEnabled[panelKey]);

  // 容器引用
  let containerRef: HTMLDivElement | null = $state(null);
  const showScrollBtn = $derived(!shouldAutoScroll && safeMessages.length > 0);
  let wasActive = $state(false);
  let lastObservedScrollTop = $state(0);

  function setContainerScrollPosition(nextTop: number) {
    if (!containerRef) return;
    const clampedTop = Math.max(0, nextTop);
    lastObservedScrollTop = clampedTop;
    containerRef.style.scrollBehavior = 'auto';
    containerRef.scrollTop = clampedTop;
    requestAnimationFrame(() => {
      if (containerRef) {
        containerRef.style.scrollBehavior = '';
      }
    });
  }

  function captureVisibleAnchor() {
    if (!containerRef) {
      return null;
    }
    if (containerRef.clientHeight <= 0 || containerRef.getClientRects().length === 0) {
      return null;
    }
    const containerRect = containerRef.getBoundingClientRect();
    const candidates = Array.from(containerRef.querySelectorAll<HTMLElement>('[data-message-id]'));
    for (const candidate of candidates) {
      const rect = candidate.getBoundingClientRect();
      if (rect.bottom <= containerRect.top) {
        continue;
      }
      return {
        messageId: candidate.dataset.messageId || null,
        offsetTop: Math.round(rect.top - containerRect.top),
      };
    }
    const lastCandidate = candidates[candidates.length - 1];
    if (!lastCandidate) {
      return null;
    }
    const rect = lastCandidate.getBoundingClientRect();
    return {
      messageId: lastCandidate.dataset.messageId || null,
      offsetTop: Math.round(rect.top - containerRect.top),
    };
  }

  function syncPanelScrollState(scrollTop: number, autoScrollEnabled: boolean, persist = true, anchor = captureVisibleAnchor()) {
    updatePanelScrollState(panelKey, { scrollTop, autoScrollEnabled, anchor }, { persist });
  }

  function scrollPanelToBottom(persist = true) {
    if (!containerRef) return;
    setContainerScrollPosition(containerRef.scrollHeight);
    syncPanelScrollState(containerRef.scrollTop, true, persist);
  }

  function restorePanelScrollPosition(persist = false) {
    if (!containerRef) return;
    if (shouldAutoScroll) {
      scrollPanelToBottom(persist);
      return;
    }
    const anchor = persistedScrollAnchor;
    if (anchor?.messageId) {
      const selectorSafeId = anchor.messageId.replace(/"/g, '\\"');
      const targetElement = containerRef.querySelector(`[data-message-id="${selectorSafeId}"]`) as HTMLElement | null;
      if (targetElement) {
        const containerRect = containerRef.getBoundingClientRect();
        const elementRect = targetElement.getBoundingClientRect();
        const currentOffsetTop = elementRect.top - containerRect.top;
        setContainerScrollPosition(containerRef.scrollTop + currentOffsetTop - anchor.offsetTop);
        syncPanelScrollState(containerRef.scrollTop, false, persist, anchor);
        return;
      }
    }
    setContainerScrollPosition(persistedScrollTop);
    syncPanelScrollState(containerRef.scrollTop, false, persist);
  }

  // 监听消息变化，自动滚动到底部
  // 🔧 同时监听流式消息内容变化，确保内容增长时也能自动滚动
  $effect(() => {
    const active = isActive;
    const _len = safeMessages.length;
    const _sig = streamingContentSignature; // 订阅流式内容变化
    void _len;
    void _sig;
    if (!active || !shouldAutoScroll || !containerRef) return;
    tick().then(() => {
      if (!containerRef || !isActive || !shouldAutoScroll) return;
      scrollPanelToBottom();
    });
  });

  // 面板切回可见后，按 panel 维度恢复之前的位置；仅在可见性切换瞬间执行，避免覆盖用户手动滚动
  $effect(() => {
    const active = isActive;
    if (active && !wasActive && containerRef) {
      tick().then(() => {
        if (!containerRef || !isActive) return;
        restorePanelScrollPosition(false);
      });
    }
    wasActive = active;
  });

  // 外部触发的消息定位（例如：任务面板点击历史计划，穿透定位到对应对话轮次）
  $effect(() => {
    const jumpNonce = messagesState.messageJump.nonce;
    void jumpNonce;
    const targetMessageId = messagesState.messageJump.messageId;
    if (!targetMessageId) return;
    if (displayContext !== 'thread') return;
    if (!isActive) return;
    if (!containerRef) return;

    const existsInCurrentList = safeMessages.some((message) => message.id === targetMessageId);
    if (!existsInCurrentList) return;

    tick().then(() => {
      if (!containerRef) return;
      const selectorSafeId = targetMessageId.replace(/"/g, '\\"');
      const targetElement = containerRef.querySelector(`[data-message-id="${selectorSafeId}"]`) as HTMLElement | null;
      if (!targetElement) return;

      targetElement.scrollIntoView({ behavior: 'smooth', block: 'center' });
      try {
        targetElement.animate(
          [
            { boxShadow: '0 0 0 0 rgba(14, 99, 156, 0.0)' },
            { boxShadow: '0 0 0 2px rgba(14, 99, 156, 0.55)' },
            { boxShadow: '0 0 0 0 rgba(14, 99, 156, 0.0)' },
          ],
          { duration: 900, easing: 'ease-out' }
        );
      } catch {
        // ignore: animate API 在极少数环境可能不可用
      }

      clearMessageJump();
    });
  });

  // 检测用户是否手动滚动
  function handleScroll(event: Event) {
    const target = event.target as HTMLDivElement;
    const { scrollTop, scrollHeight, clientHeight } = target;
    const distanceFromBottom = scrollHeight - scrollTop - clientHeight;
    const isNearBottom = distanceFromBottom < 100;
    const userScrolledUp = scrollTop < lastObservedScrollTop - 4;
    let nextAutoScroll = shouldAutoScroll;
    if (isNearBottom) {
      nextAutoScroll = true;
    } else if (userScrolledUp) {
      nextAutoScroll = false;
    }
    lastObservedScrollTop = scrollTop;
    syncPanelScrollState(scrollTop, nextAutoScroll);
  }

  // 滚动到底部
  function scrollToBottom() {
    updatePanelScrollState(panelKey, { autoScrollEnabled: true }, { persist: false });
    if (containerRef) {
      containerRef.scrollTo({ top: containerRef.scrollHeight, behavior: 'smooth' });
    }
  }
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
        <MessageItem
          {message}
          {readOnly}
          {displayContext}
          showStreamingIndicator={message.id === streamingIndicatorMessageId}
          streamingElapsedSeconds={message.id === streamingIndicatorMessageId && shouldRunTimer ? elapsedSeconds : 0}
        />
      {/each}
      <!-- 对话级处理指示器：无流式消息但仍在处理中时显示 -->
      {#if showProcessingIndicator}
        <div class="conversation-processing-indicator">
          <span class="streaming-dot"></span>
          <span class="streaming-dot"></span>
          <span class="streaming-dot"></span>
          {#if timerStartTime > 0}
            <span class="elapsed-time">{formatElapsed(elapsedSeconds)}</span>
          {/if}
        </div>
      {/if}
    {/if}
  </div>

  <!-- 滚动按钮：绝对定位在消息列表右下角 -->
  {#if showScrollBtn}
    <button class="scroll-to-bottom" onclick={scrollToBottom} title={i18n.t('messageList.scrollToBottom')}>
      <Icon name="chevron-down" size={16} />
    </button>
  {/if}
</div>

<style>
  .message-list-wrapper {
    position: relative;
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    display: flex;
    flex-direction: column;
  }

  .message-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    flex: 1;
    min-height: 0; /* flex 布局防溢出 */
    overflow-y: auto;
    overflow-x: hidden;
    /* 右侧减少间距以补偿滚动条宽度，使内容视觉对称 */
    padding: var(--space-4);
    padding-right: var(--space-2);
    /* 🔧 优化：禁用浏览器默认的滚动锚定，防止与自动滚动逻辑冲突导致抖动 */
    overflow-anchor: none;
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

  /* 对话级处理指示器 */
  .conversation-processing-indicator {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: var(--space-2) var(--space-4);
    /* 防御性：确保在 flex 容器中始终排列在所有消息卡片之后 */
    order: 9999;
  }

  .conversation-processing-indicator .streaming-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--info);
    opacity: 0.6;
    animation: processingPulse 1.4s ease-in-out infinite;
  }
  .conversation-processing-indicator .streaming-dot:nth-child(2) {
    animation-delay: 0.2s;
  }
  .conversation-processing-indicator .streaming-dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  .elapsed-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-left: 4px;
    font-variant-numeric: tabular-nums;
  }

  @keyframes processingPulse {
    0%, 80%, 100% {
      opacity: 0.4;
      transform: scale(1);
    }
    40% {
      opacity: 1;
      transform: scale(1.2);
    }
  }
</style>
