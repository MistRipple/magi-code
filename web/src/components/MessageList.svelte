<script lang="ts">
  import type {
    Message,
    ScrollPositions,
    TimelineRenderItem,
  } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import Icon from './Icon.svelte';
  import { tick, onDestroy } from 'svelte';
  import {
    clearMessageJump,
    messagesState,
    setSessionHistoryState,
    updatePanelScrollState,
    hasActiveLocalTimelineTurn,
  } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';

  // Props - Svelte 5 语法
  interface Props {
    /** 子代理 taskId —— displayContext='task' 时作为 panelKey 与 data-panel-id 的来源 */
    taskId?: string;
    renderItems: TimelineRenderItem[];
    /** 空状态配置（可选） */
    emptyState?: {
      icon?: string;
      title?: string;
      hint?: string;
    };
    /** 是否为只读模式（主对话区模式），隐藏冗余操作按钮 */
    readOnly?: boolean;
    /** 显示上下文：thread=主对话区, task=右侧子代理 tab */
    displayContext?: 'thread' | 'task';
    /** 当前面板是否处于可见激活状态（用于 display:none -> visible 场景下的滚动恢复） */
    isActive?: boolean;
  }
  let { taskId, renderItems, emptyState, readOnly = false, displayContext = 'thread', isActive = true }: Props = $props();

  const safeRenderItems = $derived(
    (renderItems || [])
      .filter((item): item is TimelineRenderItem => Boolean(item && item.message && item.message.id))
  );

  const activeRenderItems = $derived(safeRenderItems);

  const currentSessionId = $derived.by(() => (
    typeof messagesState.currentSessionId === 'string' ? messagesState.currentSessionId.trim() : ''
  ));

  const safeRenderMessages = $derived.by(() => activeRenderItems.map((item) => item.message));

  function resolveStreamingMessageVersion(message: Message): string {
    const metadata = (message.metadata && typeof message.metadata === 'object')
      ? (message.metadata as Record<string, unknown>)
      : {};
    const eventSeq = typeof metadata.eventSeq === 'number' && Number.isFinite(metadata.eventSeq)
      ? Math.max(0, Math.floor(metadata.eventSeq))
      : 0;
    const cardStreamSeq = typeof metadata.cardStreamSeq === 'number' && Number.isFinite(metadata.cardStreamSeq)
      ? Math.max(0, Math.floor(metadata.cardStreamSeq))
      : 0;
    const updatedAt = typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)
      ? Math.max(0, Math.floor(message.updatedAt))
      : 0;
    return `${eventSeq}:${cardStreamSeq}:${updatedAt}:${(message.content || '').length}:${(message.blocks || []).length}`;
  }

  /* 🔧 计算流式消息的内容签名，用于触发滚动
     当任何流式消息的内容变化时，需要重新滚动到底部 */
  const streamingContentSignature = $derived.by(() => {
    const streamingMsgs = safeRenderMessages.filter(m => m.isStreaming);
    if (streamingMsgs.length === 0) return '';
    // 用 event/card 序号 + 更新时间作为流式版本签名，避免 tool_call 高度变化漏触发自动滚动
    return streamingMsgs.map(m => `${m.id}:${resolveStreamingMessageVersion(m)}`).join('|');
  });

  const currentStreamingRenderItem = $derived.by(() => {
    for (let i = activeRenderItems.length - 1; i >= 0; i -= 1) {
      const item = activeRenderItems[i];
      if (item.message.isStreaming) {
        return item;
      }
    }
    return null;
  });

  const currentStreamingMessage = $derived.by(() => currentStreamingRenderItem?.message || null);

  // 计时器代表「整个 turn 还在跑」，锚到该 turn 内 displayOrder 最大的非 user_input render item，
  // 槽位渲染在 .message-content 末尾，使指示器始终位于当前 turn 最下方一张卡片的底部。
  // 反向遍历 activeRenderItems（已按 displayOrder 升序排列）找匹配 turnId 的第一项即为末尾项；
  // 跳过 user_input 是因为 user 模板无 streaming-indicator 槽位，命中后会导致整 turn 无指示器。
  const streamingIndicatorRenderKey = $derived.by(() => {
    const streamingItem = currentStreamingRenderItem;
    if (!streamingItem) {
      return null;
    }
    const turnId = typeof streamingItem.message.metadata?.turnId === 'string'
      ? streamingItem.message.metadata.turnId.trim()
      : '';
    if (!turnId) {
      return streamingItem.key;
    }
    for (let i = activeRenderItems.length - 1; i >= 0; i -= 1) {
      const item = activeRenderItems[i];
      if (item.message.type === 'user_input') {
        continue;
      }
      const itemTurnId = typeof item.message.metadata?.turnId === 'string'
        ? item.message.metadata.turnId.trim()
        : '';
      if (itemTurnId === turnId) {
        return item.key;
      }
    }
    return streamingItem.key;
  });

  const resolvedStreamingStartAt = $derived.by(() => {
    const message = currentStreamingMessage;
    if (!message) {
      return 0;
    }
    const processingStartAt = messagesState.thinkingStartAt;
    if (
      typeof processingStartAt === 'number'
      && Number.isFinite(processingStartAt)
      && processingStartAt > 0
    ) {
      return processingStartAt;
    }
    const timestamp = message.timestamp;
    if (typeof timestamp === 'number' && Number.isFinite(timestamp) && timestamp > 0) {
      return timestamp;
    }
    return 0;
  });

  let stableStreamingStartAt = $state(0);
  let stableStreamingSessionId = $state('');

  $effect(() => {
    const sessionId = currentSessionId || '';
    if (sessionId !== stableStreamingSessionId) {
      stableStreamingSessionId = sessionId;
      stableStreamingStartAt = 0;
    }
    if (!currentStreamingMessage) {
      stableStreamingStartAt = 0;
      return;
    }
    const nextStartAt = resolvedStreamingStartAt;
    if (!(typeof nextStartAt === 'number' && Number.isFinite(nextStartAt) && nextStartAt > 0)) {
      return;
    }
    if (stableStreamingStartAt === 0 || nextStartAt < stableStreamingStartAt) {
      stableStreamingStartAt = nextStartAt;
    }
  });

  const timerStartTime = $derived.by(() => stableStreamingStartAt);

  const shouldRunTimer = $derived.by(() => {
    return timerStartTime > 0;
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

  // 空状态默认值
  const emptyIcon = $derived((emptyState?.icon || 'chat') as import('../lib/icons').IconName);
  const emptyTitle = $derived(emptyState?.title || i18n.t('messageList.empty.title'));
  const emptyHint = $derived(emptyState?.hint || i18n.t('messageList.empty.hint'));
  const showSuggestions = $derived(displayContext === 'thread' && !emptyState);
  const suggestionItems = $derived([
    i18n.t('messageList.suggestions.s1'),
    i18n.t('messageList.suggestions.s2'),
    i18n.t('messageList.suggestions.s3'),
  ]);
  function fillComposer(text: string) {
    if (typeof window === 'undefined') return;
    window.dispatchEvent(new CustomEvent('magi:fillComposer', { detail: { text } }));
  }
  const panelKey = $derived.by((): keyof ScrollPositions => (displayContext === 'task' ? (taskId || 'task') : 'thread'));
  const persistedScrollTop = $derived(messagesState.scrollPositions[panelKey] || 0);
  const persistedScrollAnchor = $derived(messagesState.scrollAnchors[panelKey]);
  const shouldAutoScroll = $derived(messagesState.autoScrollEnabled[panelKey]);
  const sessionHistory = $derived(messagesState.sessionHistory);
  const canLoadOlderHistory = $derived(Boolean(
    currentSessionId
    && sessionHistory.sessionId === currentSessionId
    && sessionHistory.hasMoreBefore
    && sessionHistory.beforeCursor
    && !sessionHistory.isLoadingBefore
  ));

  // 容器引用
  let containerRef: HTMLDivElement | null = $state(null);
  let historySentinelRef: HTMLDivElement | null = $state(null);
  const showScrollBtn = $derived(!shouldAutoScroll && safeRenderMessages.length > 0);
  let wasActive = $state(false);
  let lastObservedScrollTop = $state(0);
  let activationRestoreNonce = 0;
  let restoreAttemptTimers: Array<ReturnType<typeof setTimeout>> = [];
  let historyObserver: IntersectionObserver | null = null;
  let programmaticScrollDepth = 0;

  const HISTORY_LOAD_THRESHOLD_PX = 120;

  function disconnectHistoryObserver() {
    if (!historyObserver) return;
    historyObserver.disconnect();
    historyObserver = null;
  }

  function clearRestoreAttemptTimers() {
    if (restoreAttemptTimers.length === 0) return;
    for (const timer of restoreAttemptTimers) {
      clearTimeout(timer);
    }
    restoreAttemptTimers = [];
  }

  function scheduleActivationScrollRestore() {
    const restoreNonce = ++activationRestoreNonce;
    clearRestoreAttemptTimers();

    const attemptRestore = () => {
      if (restoreNonce !== activationRestoreNonce) return;
      if (!containerRef || !isActive) return;
      restorePanelScrollPosition(false);
    };

    // 多阶段恢复：覆盖 tab 切换后异步布局变化（代码高亮/卡片内容扩展）导致的位置漂移。
    tick().then(() => {
      attemptRestore();
      requestAnimationFrame(() => {
        attemptRestore();
      });
      restoreAttemptTimers = [
        setTimeout(() => attemptRestore(), 96),
        setTimeout(() => attemptRestore(), 220),
      ];
    });
  }

  function setContainerScrollPosition(nextTop: number) {
    if (!containerRef) return;
    programmaticScrollDepth += 1;
    const maxScrollTop = Math.max(0, containerRef.scrollHeight - containerRef.clientHeight);
    const clampedTop = Math.max(0, Math.min(nextTop, maxScrollTop));
    containerRef.style.scrollBehavior = 'auto';
    containerRef.scrollTop = clampedTop;
    lastObservedScrollTop = containerRef.scrollTop;
    requestAnimationFrame(() => {
      if (containerRef) {
        containerRef.style.scrollBehavior = '';
      }
      programmaticScrollDepth = Math.max(0, programmaticScrollDepth - 1);
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
    setContainerScrollPosition(containerRef.scrollHeight - containerRef.clientHeight);
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
    const _len = safeRenderMessages.length;
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
      scheduleActivationScrollRestore();
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

    const existsInCurrentList = safeRenderMessages.some((message) => message.id === targetMessageId);
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

  $effect(() => {
    const container = containerRef;
    const sentinel = historySentinelRef;
    const sessionId = currentSessionId;
    const historyState = sessionHistory;
    const canObserveHistory = Boolean(
      container
      && sentinel
      && isActive
      && sessionId
      && historyState.sessionId === sessionId
      && historyState.hasMoreBefore
      && historyState.beforeCursor
    );

    disconnectHistoryObserver();
    if (!canObserveHistory || !container || !sentinel) {
      return;
    }

    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        void loadOlderHistory();
      }
    }, {
      root: container,
      rootMargin: `${HISTORY_LOAD_THRESHOLD_PX}px 0px 0px 0px`,
      threshold: 0,
    });
    observer.observe(sentinel);
    historyObserver = observer;

    return () => {
      if (historyObserver === observer) {
        historyObserver = null;
      }
      observer.disconnect();
    };
  });

  function loadOlderHistory(): void {
    const sessionId = (messagesState.currentSessionId || '').trim();
    const workspaceId = (messagesState.currentWorkspaceId || '').trim();
    if (!sessionId || hasActiveLocalTimelineTurn()) {
      return;
    }
    setSessionHistoryState(sessionId, {
      workspaceId,
      hasMoreBefore: false,
      beforeCursor: null,
      isLoadingBefore: false,
    });
  }

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
    if (programmaticScrollDepth === 0) {
      activationRestoreNonce += 1;
      clearRestoreAttemptTimers();
    }
    lastObservedScrollTop = scrollTop;
    syncPanelScrollState(scrollTop, nextAutoScroll);
    if (scrollTop <= HISTORY_LOAD_THRESHOLD_PX) {
      void loadOlderHistory();
    }
  }

  function handleWheel(event: WheelEvent) {
    if (
      event.deltaY < 0
      && containerRef
      && containerRef.scrollTop <= HISTORY_LOAD_THRESHOLD_PX
    ) {
      void loadOlderHistory();
    }
  }

  // 滚动到底部
  function scrollToBottom() {
    updatePanelScrollState(panelKey, { autoScrollEnabled: true }, { persist: false });
    scrollPanelToBottom(false);
  }

  onDestroy(() => {
    activationRestoreNonce += 1;
    clearRestoreAttemptTimers();
    disconnectHistoryObserver();
    if (!containerRef) {
      return;
    }
    syncPanelScrollState(containerRef.scrollTop, shouldAutoScroll);
  });
</script>

<div class="message-list-wrapper">
  <div
    class="message-list"
    bind:this={containerRef}
    onscroll={handleScroll}
    onwheel={handleWheel}
    data-panel-id={displayContext === 'thread' ? 'thread' : (taskId || 'task')}
    data-display-context={displayContext}
    data-panel-active={isActive ? 'true' : 'false'}
  >
    {#if safeRenderItems.length > 0 && (canLoadOlderHistory || sessionHistory.isLoadingBefore)}
      <div class="history-sentinel" bind:this={historySentinelRef} aria-hidden="true"></div>
    {/if}
    {#if safeRenderItems.length === 0}
      <div class="empty-state">
        <div class="empty-icon">
          <Icon name={emptyIcon} size={48} />
        </div>
        <p class="empty-text">{emptyTitle}</p>
        <p class="empty-hint">{emptyHint}</p>
        {#if showSuggestions}
          <div class="empty-suggestions">
            <span class="suggestions-title">{i18n.t('messageList.suggestions.title')}</span>
            {#each suggestionItems as text (text)}
              <button type="button" class="suggestion-card" onclick={() => fillComposer(text)}>
                <span class="suggestion-text">{text}</span>
              </button>
            {/each}
          </div>
        {/if}
        {#if canLoadOlderHistory || sessionHistory.isLoadingBefore}
          <button
            type="button"
            class="empty-history-load"
            onclick={() => void loadOlderHistory()}
            disabled={!canLoadOlderHistory}
          >
            <Icon name={sessionHistory.isLoadingBefore ? 'loader' : 'chevron-up'} size={14} />
            <span>{sessionHistory.isLoadingBefore ? i18n.t('messageList.loadingOlder') : i18n.t('messageList.loadOlder')}</span>
          </button>
        {/if}
      </div>
    {:else}
      {#each safeRenderItems as item (item.key)}
        <MessageItem
          message={item.message}
          {readOnly}
          {displayContext}
          showStreamingIndicator={item.key === streamingIndicatorRenderKey}
          streamingElapsedSeconds={item.key === streamingIndicatorRenderKey ? elapsedSeconds : 0}
        />
      {/each}
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

  .history-sentinel {
    flex: 0 0 1px;
    width: 100%;
    height: 1px;
    margin-bottom: calc(-1 * var(--space-3));
    pointer-events: none;
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

  .empty-suggestions {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    align-items: stretch;
    width: 100%;
    max-width: 480px;
    margin-top: var(--space-5);
  }

  .suggestions-title {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground-muted);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    text-align: left;
    margin-bottom: var(--space-1);
  }

  .suggestion-card {
    display: flex;
    align-items: center;
    text-align: left;
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1, rgba(255,255,255,0.02));
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast), transform var(--transition-fast);
  }

  .suggestion-card:hover {
    background: var(--surface-hover, rgba(255,255,255,0.05));
    border-color: color-mix(in srgb, var(--info) 40%, var(--border));
    transform: translateY(-1px);
  }

  .suggestion-card:active {
    transform: translateY(0);
  }

  .suggestion-text {
    flex: 1;
    line-height: 1.5;
  }

  .empty-history-load {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    min-height: 32px;
    margin-top: var(--space-4);
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: 1;
    cursor: pointer;
  }

  .empty-history-load:hover:not(:disabled) {
    border-color: var(--primary);
    color: var(--primary);
  }

  .empty-history-load:disabled {
    cursor: default;
    opacity: 0.65;
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
