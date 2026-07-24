<script lang="ts">
  import type {
    Message,
    ScrollPositions,
    TimelineRenderItem,
  } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import TurnRuntimeIndicator from './TurnRuntimeIndicator.svelte';
  import TurnNavigationRail from './TurnNavigationRail.svelte';
  import Icon from './Icon.svelte';
  import { tick, onDestroy } from 'svelte';
  import {
    clearMessageJump,
    beginTurnEditing,
    messagesState,
    setSessionHistoryState,
    updatePanelScrollState,
    hasActiveLocalTimelineTurn,
  } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    buildTurnNavigationItems,
    isTurnNavigationStatus,
    type TurnNavigationMessage,
  } from '../lib/turn-navigation';

  // Props - Svelte 5 语法
  interface Props {
    /** 代理 taskId —— displayContext='task' 时作为 panelKey 与 data-panel-id 的来源 */
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
    /** 显示上下文：thread=主对话区, task=右侧代理 tab */
    displayContext?: 'thread' | 'task';
    /** 当前面板是否处于可见激活状态（用于 display:none -> visible 场景下的滚动恢复） */
    isActive?: boolean;
    /** 外部任务运行态：右侧代理 tab 用 task status 驱动同一套响应动画 */
    runtimeActive?: boolean;
    /** 外部任务运行起点：右侧代理 tab 用 task.created_at 计时 */
    runtimeStartedAt?: number;
  }
  let {
    taskId,
    renderItems,
    emptyState,
    readOnly = false,
    displayContext = 'thread',
    isActive = true,
    runtimeActive = false,
    runtimeStartedAt = 0,
  }: Props = $props();

  const safeRenderItems = $derived(
    (renderItems || [])
      .filter((item): item is TimelineRenderItem => Boolean(item && item.message && item.message.id))
  );

  const activeRenderItems = $derived(safeRenderItems);

  type TimelineRenderEntry =
    | { kind: 'message'; key: string; item: TimelineRenderItem }
    | { kind: 'runtime'; key: string };

  const turnNavigationItems = $derived.by(() => {
    if (displayContext !== 'thread') return [];
    const navigationMessages: TurnNavigationMessage[] = [];
    for (const item of activeRenderItems) {
      const metadata = item.message.metadata || {};
      const turnId = typeof metadata.turnId === 'string' ? metadata.turnId.trim() : '';
      const turnSeq = typeof metadata.turnSeq === 'number' && Number.isFinite(metadata.turnSeq)
        ? Math.floor(metadata.turnSeq)
        : null;
      const turnStatus = metadata.turnStatus;
      if (!turnId || turnSeq === null || !isTurnNavigationStatus(turnStatus)) continue;
      navigationMessages.push({
        id: item.message.id,
        turnId,
        turnSeq,
        turnStatus,
        type: item.message.type || '',
        content: item.message.content || '',
        timestamp: item.message.timestamp,
      });
    }
    return buildTurnNavigationItems(navigationMessages);
  });
  const showTurnNavigation = $derived(displayContext === 'thread' && turnNavigationItems.length > 1);

  const currentSessionId = $derived.by(() => (
    typeof messagesState.currentSessionId === 'string' ? messagesState.currentSessionId.trim() : ''
  ));
  const currentWorkspaceId = $derived.by(() => (
    typeof messagesState.currentWorkspaceId === 'string' ? messagesState.currentWorkspaceId.trim() : ''
  ));
  const runtimePanelKey = $derived(`${currentWorkspaceId}:${currentSessionId}:${displayContext}:${taskId || ''}`);
  const normalizedRuntimeStartedAt = $derived(
    typeof runtimeStartedAt === 'number' && Number.isFinite(runtimeStartedAt)
      ? Math.max(0, Math.floor(runtimeStartedAt))
      : 0
  );

  const safeRenderMessages = $derived.by(() => activeRenderItems.map((item) => item.message));
  const latestUserMessageId = $derived.by(() => {
    if (displayContext !== 'thread') return '';
    for (let index = activeRenderItems.length - 1; index >= 0; index -= 1) {
      const message = activeRenderItems[index]?.message;
      if (message?.type === 'user_input') {
        return message.id;
      }
    }
    return '';
  });

  function canEditUserMessage(message: Message): boolean {
    if (
      displayContext !== 'thread'
      || message.id !== latestUserMessageId
      || messagesState.isProcessing
      || messagesState.backendProcessing
    ) {
      return false;
    }
    const metadata = message.metadata || {};
    return metadata.turnStatus === 'cancelled'
      && metadata.interruptionSource === 'user'
      && typeof metadata.turnId === 'string'
      && metadata.turnId.trim().length > 0;
  }

  function editUserMessage(message: Message): void {
    if (!canEditUserMessage(message) || !currentSessionId) return;
    beginTurnEditing({
      sessionId: currentSessionId,
      turnId: String(message.metadata?.turnId || '').trim(),
      messageId: message.id,
      text: message.content || '',
      images: Array.isArray(message.images) ? message.images : [],
      contextReferences: Array.isArray(message.contextReferences) ? message.contextReferences : [],
      skillName: typeof message.metadata?.skillName === 'string' ? message.metadata.skillName : null,
      goalMode: message.metadata?.goalMode === true,
    });
  }

  function resolveMessageRenderRevision(message: Message): string {
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
    const canonicalRevision = typeof metadata.renderRevision === 'string'
      ? metadata.renderRevision
      : '';
    return `${canonicalRevision}:${eventSeq}:${cardStreamSeq}:${updatedAt}:${(message.content || '').length}:${(message.blocks || []).length}`;
  }

  const renderContentSignature = $derived.by(() => {
    return safeRenderMessages
      .map((message) => `${message.id}:${resolveMessageRenderRevision(message)}`)
      .join('|');
  });
  const messageElementSignature = $derived(safeRenderItems.map((item) => item.key).join('|'));

  const currentStreamingRenderItem = $derived.by(() => {
    for (let i = activeRenderItems.length - 1; i >= 0; i -= 1) {
      const item = activeRenderItems[i];
      if (item.message.isStreaming) {
        return item;
      }
    }
    return null;
  });

  const activeThreadRequestId = $derived.by(() => {
    if (displayContext !== 'thread') {
      return '';
    }
    let requestId = '';
    for (const pendingRequestId of messagesState.pendingRequests) {
      if (typeof pendingRequestId === 'string' && pendingRequestId.trim()) {
        requestId = pendingRequestId.trim();
      }
    }
    return requestId;
  });

  function messageMetadataString(message: Message, key: string): string {
    const value = message.metadata?.[key];
    return typeof value === 'string' ? value.trim() : '';
  }

  function isLiveTurnStatus(status: string): boolean {
    return status === 'pending' || status === 'running';
  }

  function findRenderItemByRequestId(requestId: string): TimelineRenderItem | null {
    if (!requestId) {
      return null;
    }
    for (let i = activeRenderItems.length - 1; i >= 0; i -= 1) {
      const item = activeRenderItems[i];
      if (messageMetadataString(item.message, 'requestId') === requestId) {
        return item;
      }
    }
    return null;
  }

  const currentRuntimeRenderItem = $derived.by(() => {
    const hasThreadRuntime = displayContext === 'thread' && messagesState.isProcessing;
    const hasTaskRuntime = displayContext === 'task' && runtimeActive;
    if (!hasThreadRuntime && !hasTaskRuntime) {
      return null;
    }
    // 本地主线提交以 requestId 作为当前轮次身份。新轮次占位项尚未投影时必须返回 null，
    // 不能继续复用上一轮残留的 isStreaming 卡片，否则状态和计时会延迟切换。
    if (hasThreadRuntime && activeThreadRequestId) {
      return findRenderItemByRequestId(activeThreadRequestId);
    }
    if (currentStreamingRenderItem) {
      return currentStreamingRenderItem;
    }
    for (let i = activeRenderItems.length - 1; i >= 0; i -= 1) {
      const item = activeRenderItems[i];
      if (isLiveTurnStatus(messageMetadataString(item.message, 'turnStatus'))) {
        return item;
      }
    }
    // 主线刚提交、canonical turn 尚未投影时，运行计时器独立作为时间线条目出现；
    // 代理详情仍可使用最后一项承载外部运行态。
    return hasTaskRuntime ? (activeRenderItems[activeRenderItems.length - 1] || null) : null;
  });

  const awaitingCurrentTurnProjection = $derived.by(() => (
    displayContext === 'thread'
    && messagesState.isProcessing
    && currentRuntimeRenderItem === null
  ));

  const runtimeTurnIdentity = $derived.by(() => {
    const runtimeMessage = currentRuntimeRenderItem?.message;
    const runtimeMessageRequestId = runtimeMessage
      ? messageMetadataString(runtimeMessage, 'requestId')
      : '';
    if (displayContext === 'thread' && (activeThreadRequestId || runtimeMessageRequestId)) {
      return `request:${activeThreadRequestId || runtimeMessageRequestId}`;
    }
    if (runtimeMessage) {
      const turnId = messageMetadataString(runtimeMessage, 'turnId');
      return turnId ? `turn:${turnId}` : `message:${runtimeMessage.id}`;
    }
    if (displayContext === 'task' && runtimeActive) {
      return `task:${taskId || ''}:${normalizedRuntimeStartedAt}`;
    }
    return '';
  });

  const resolvedStreamingStartAt = $derived.by(() => {
    const message = currentRuntimeRenderItem?.message || null;
    const processingStartAt = displayContext === 'thread' ? messagesState.thinkingStartAt : 0;
    if (
      typeof processingStartAt === 'number'
      && Number.isFinite(processingStartAt)
      && processingStartAt > 0
    ) {
      return processingStartAt;
    }
    if (!message) {
      return 0;
    }
    if (displayContext === 'task' && normalizedRuntimeStartedAt > 0) {
      return normalizedRuntimeStartedAt;
    }
    const timestamp = message.timestamp;
    if (typeof timestamp === 'number' && Number.isFinite(timestamp) && timestamp > 0) {
      return timestamp;
    }
    return 0;
  });

  let stableStreamingStartAt = $state(0);
  let stableStreamingPanelKey = $state('');
  let stableRuntimeTurnIdentity = $state('');

  $effect(() => {
    const panelKey = runtimePanelKey || '';
    const turnIdentity = runtimeTurnIdentity;
    if (
      panelKey !== stableStreamingPanelKey
      || turnIdentity !== stableRuntimeTurnIdentity
    ) {
      stableStreamingPanelKey = panelKey;
      stableRuntimeTurnIdentity = turnIdentity;
      stableStreamingStartAt = 0;
    }
    if (!currentRuntimeRenderItem && !awaitingCurrentTurnProjection) {
      stableStreamingStartAt = 0;
      return;
    }
    const nextStartAt = resolvedStreamingStartAt;
    if (!(typeof nextStartAt === 'number' && Number.isFinite(nextStartAt) && nextStartAt > 0)) {
      return;
    }
    if (displayContext === 'thread' && activeThreadRequestId) {
      stableStreamingStartAt = nextStartAt;
    } else if (stableStreamingStartAt === 0 || nextStartAt < stableStreamingStartAt) {
      stableStreamingStartAt = nextStartAt;
    }
  });

  const timerStartTime = $derived.by(() => stableStreamingStartAt);

  const shouldRunTimer = $derived.by(() => {
    return timerStartTime > 0 && (Boolean(currentRuntimeRenderItem) || awaitingCurrentTurnProjection);
  });

  const runtimeIndicatorKey = $derived(
    runtimeTurnIdentity ? `runtime-indicator:${runtimeTurnIdentity}` : ''
  );
  const runtimeIndicatorInsertionIndex = $derived.by(() => {
    if (!shouldRunTimer) return -1;
    const runtimeItem = currentRuntimeRenderItem;
    if (!runtimeItem) return activeRenderItems.length;
    const turnId = messageMetadataString(runtimeItem.message, 'turnId');
    if (turnId) {
      for (let index = activeRenderItems.length - 1; index >= 0; index -= 1) {
        if (messageMetadataString(activeRenderItems[index].message, 'turnId') === turnId) {
          return index + 1;
        }
      }
    }
    const runtimeIndex = activeRenderItems.findIndex((item) => item.key === runtimeItem.key);
    return runtimeIndex >= 0 ? runtimeIndex + 1 : activeRenderItems.length;
  });
  const timelineRenderEntries = $derived.by(() => {
    const entries: TimelineRenderEntry[] = activeRenderItems.map((item) => ({
      kind: 'message',
      key: item.key,
      item,
    }));
    if (!shouldRunTimer || !runtimeIndicatorKey) {
      return entries;
    }
    entries.splice(runtimeIndicatorInsertionIndex, 0, {
      kind: 'runtime',
      key: runtimeIndicatorKey,
    });
    return entries;
  });
  const runtimeLayoutSignature = $derived(
    `${runtimeIndicatorKey}:${runtimeIndicatorInsertionIndex}`
  );

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
  const showSuggestions = $derived(displayContext === 'thread' && !emptyState && !messagesState.isProcessing);
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
  let contentResizeObserver: ResizeObserver | null = null;
  let contentResizeFrame = 0;
  let programmaticScrollDepth = 0;

  const HISTORY_LOAD_THRESHOLD_PX = 120;

  function disconnectHistoryObserver() {
    if (!historyObserver) return;
    historyObserver.disconnect();
    historyObserver = null;
  }

  function disconnectContentResizeObserver() {
    contentResizeObserver?.disconnect();
    contentResizeObserver = null;
    if (contentResizeFrame) {
      cancelAnimationFrame(contentResizeFrame);
      contentResizeFrame = 0;
    }
  }

  function observeMessageLayoutChanges() {
    disconnectContentResizeObserver();
    if (!containerRef || typeof ResizeObserver === 'undefined') {
      return;
    }
    contentResizeObserver = new ResizeObserver(() => {
      if (!isActive || !shouldAutoScroll || !containerRef || contentResizeFrame) {
        return;
      }
      contentResizeFrame = requestAnimationFrame(() => {
        contentResizeFrame = 0;
        if (isActive && shouldAutoScroll && containerRef) {
          scrollPanelToBottom();
        }
      });
    });
    for (const element of containerRef.querySelectorAll<HTMLElement>('[data-message-id]')) {
      contentResizeObserver.observe(element);
    }
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

  // 任何可见卡片内容或状态变化都触发滚动判断，覆盖文本、思考、工具和文件卡片。
  $effect(() => {
    const active = isActive;
    const _len = safeRenderMessages.length;
    const _sig = renderContentSignature;
    const _runtimeSig = runtimeLayoutSignature;
    void _len;
    void _sig;
    void _runtimeSig;
    if (!active || !shouldAutoScroll || !containerRef) return;
    tick().then(() => {
      if (!containerRef || !isActive || !shouldAutoScroll) return;
      scrollPanelToBottom();
    });
  });

  $effect(() => {
    const active = isActive;
    const signature = messageElementSignature;
    void signature;
    if (!active || !containerRef) {
      disconnectContentResizeObserver();
      return;
    }
    tick().then(observeMessageLayoutChanges);
  });

  // 面板切回可见后，按 panel 维度恢复之前的位置；仅在可见性切换瞬间执行，避免覆盖用户手动滚动
  $effect(() => {
    const active = isActive;
    if (active && !wasActive && containerRef) {
      scheduleActivationScrollRestore();
    }
    wasActive = active;
  });

  // 外部触发的消息定位（例如：目标面板点击历史计划，穿透定位到对应对话轮次）
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
    disconnectContentResizeObserver();
    if (!containerRef) {
      return;
    }
    syncPanelScrollState(containerRef.scrollTop, shouldAutoScroll);
  });
</script>

<div class="message-list-wrapper" class:has-turn-navigation={showTurnNavigation}>
  <TurnNavigationRail items={turnNavigationItems} container={containerRef} />
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
    {#if timelineRenderEntries.length === 0}
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
      {#each timelineRenderEntries as entry (entry.key)}
        {#if entry.kind === 'message'}
          <MessageItem
            message={entry.item.message}
            {readOnly}
            {displayContext}
            filePreviewScope={{
              workspaceId: entry.item.workspaceId,
              workspacePath: entry.item.workspacePath,
              sessionId: entry.item.sessionId,
            }}
            canEdit={canEditUserMessage(entry.item.message)}
            onEdit={() => editUserMessage(entry.item.message)}
          />
        {:else}
          <TurnRuntimeIndicator elapsedSeconds={elapsedSeconds} />
        {/if}
      {/each}
    {/if}
  </div>

  <!-- 滚动按钮：绝对定位在消息列表右下角 -->
  {#if showScrollBtn}
    <button class="scroll-to-bottom floating-overlay-control" onclick={scrollToBottom} title={i18n.t('messageList.scrollToBottom')}>
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
    container-type: inline-size;
    container-name: message-list;
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

  @container message-list (min-width: 640px) {
    .message-list-wrapper.has-turn-navigation .message-list {
      padding-left: calc(var(--space-4) + 8px);
    }
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
    color: var(--primary);
    border-radius: var(--radius-full);
    z-index: 100;
    animation: slideUp 0.2s ease-out;
  }

  @keyframes slideUp {
    from { opacity: 0; transform: translateY(8px); }
    to { opacity: 1; transform: translateY(0); }
  }

</style>
