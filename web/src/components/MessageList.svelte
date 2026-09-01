<script lang="ts">
  import type {
    Message,
    ScrollPositions,
    TimelineRenderItem,
  } from '../types/message';
  import MessageItem from './MessageItem.svelte';
  import ConversationTurn from './ConversationTurn.svelte';
  import TurnRuntimeIndicator from './TurnRuntimeIndicator.svelte';
  import TurnRuntimeSummary from './TurnRuntimeSummary.svelte';
  import TurnNavigationRail from './TurnNavigationRail.svelte';
  import Icon from './Icon.svelte';
  import { tick, onDestroy } from 'svelte';
  import {
    clearMessageJump,
    beginTurnEditing,
    commitOlderSessionHistoryPage,
    messagesState,
    setSessionHistoryState,
    updatePanelScrollState,
    hasActiveLocalTimelineTurn,
  } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    sessionSuggestions,
    type SessionSuggestion,
  } from '../stores/session-suggestions.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { getAgentSessionMessages } from '../web/agent-api';
  import { normalizeCanonicalTurnStrict } from '../shared/protocol/canonical-turn';
  import { setCanonicalTimelineError } from '../stores/turn-store.svelte';
  import { postBridgeMessage } from '../shared/bridges/bridge-runtime';
  import {
    MessageScrollCoordinator,
    MessageLayoutStabilizer,
    type ProgrammaticScrollIntent,
  } from '../lib/message-scroll-coordinator';
  import {
    canScrollableElementConsumeWheel,
    deriveMessageScrollDirection,
    isMainMessageWheelInput,
  } from '../lib/message-scroll-input';
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
    /** 外部任务终态时间：右侧代理 tab 与主线统一展示完成时间 */
    runtimeCompletedAt?: number;
    /** 外部任务总耗时：右侧代理 tab 与主线统一展示总耗时 */
    runtimeDurationMs?: number;
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
    runtimeCompletedAt = 0,
    runtimeDurationMs = 0,
  }: Props = $props();

  const safeRenderItems = $derived(
    (renderItems || [])
      .filter((item): item is TimelineRenderItem => Boolean(item && item.message && item.message.id))
  );
  const INITIAL_RENDER_WINDOW = 72;
  const RENDER_WINDOW_CHUNK = 48;
  let visibleRenderLimit = $state(INITIAL_RENDER_WINDOW);
  let previousRenderItemCount = 0;
  let previousLastRenderItemKey = '';
  let previousRenderWindowScope = '';
  const renderWindowScope = $derived(
    `${displayContext}:${taskId || ''}:${(messagesState.currentWorkspaceId || '').trim()}:${(messagesState.currentWorkspacePath || '').trim()}:${safeRenderItems[safeRenderItems.length - 1]?.sessionId || ''}`
  );

  $effect.pre(() => {
    const items = safeRenderItems;
    const scope = renderWindowScope;
    const count = items.length;
    const lastKey = items[count - 1]?.key || '';
    if (scope !== previousRenderWindowScope || previousRenderItemCount === 0) {
      visibleRenderLimit = Math.min(count, INITIAL_RENDER_WINDOW);
    } else if (count < previousRenderItemCount) {
      visibleRenderLimit = Math.min(count, INITIAL_RENDER_WINDOW);
    } else if (count > previousRenderItemCount) {
      const previousLastIndex = previousLastRenderItemKey
        ? items.findIndex((item) => item.key === previousLastRenderItemKey)
        : -1;
      if (previousLastIndex < 0) {
        visibleRenderLimit = Math.min(count, INITIAL_RENDER_WINDOW);
      } else {
        const prependedCount = Math.max(0, previousLastIndex);
        const appendedCount = Math.max(0, count - previousLastIndex - 1);
        const newlyVisibleCount = prependedCount + appendedCount;
        if (newlyVisibleCount > 0) {
          // 保留原可见窗口的起点：前置历史也必须进入 DOM，否则滚到顶部后
          // 分页虽然提交成功，用户仍看不到刚加载的消息。
          visibleRenderLimit = Math.min(count, visibleRenderLimit + newlyVisibleCount);
        }
      }
    }
    previousRenderWindowScope = scope;
    previousRenderItemCount = count;
    previousLastRenderItemKey = lastKey;
  });

  const visibleStartIndex = $derived(Math.max(0, safeRenderItems.length - visibleRenderLimit));
  const activeRenderItems = $derived(safeRenderItems.slice(visibleStartIndex));
  const hasHiddenLocalHistory = $derived(safeRenderItems.length > visibleRenderLimit);

  function continueInterruptedSession(): void {
    vscode.postMessage({ type: 'continueTask' });
  }

  type TimelineRenderEntry =
    | { kind: 'message'; key: string; item: TimelineRenderItem }
    | { kind: 'runtime'; key: string }
    | { kind: 'runtime-summary'; key: string };

  type ConversationRenderEntry =
    | { kind: 'turn'; key: string; turnId: string; items: TimelineRenderItem[]; runtimeKey?: string }
    | { kind: 'message'; key: string; item: TimelineRenderItem }
    | { kind: 'runtime'; key: string }
    | { kind: 'runtime-summary'; key: string };

  const turnNavigationItems = $derived.by(() => {
    if (displayContext !== 'thread') return [];
    const navigationMessages: TurnNavigationMessage[] = [];
    for (const item of safeRenderItems) {
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
  const normalizedRuntimeCompletedAt = $derived(
    typeof runtimeCompletedAt === 'number' && Number.isFinite(runtimeCompletedAt)
      ? Math.max(0, Math.floor(runtimeCompletedAt))
      : 0,
  );
  const normalizedRuntimeDurationMs = $derived(
    typeof runtimeDurationMs === 'number' && Number.isFinite(runtimeDurationMs)
      ? Math.max(0, Math.floor(runtimeDurationMs))
      : 0,
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
      browserAnnotationRefs: Array.isArray(message.browserAnnotationRefs)
        ? message.browserAnnotationRefs
        : [],
      browserNodeSelections: Array.isArray(message.browserNodeSelections)
        ? message.browserNodeSelections
        : [],
      skillName: typeof message.metadata?.skillName === 'string' ? message.metadata.skillName : null,
      goalMode: message.metadata?.goalMode === true,
    });
  }

  function filePreviewScopeForItem(item: TimelineRenderItem) {
    return {
      workspaceId: item.workspaceId,
      workspacePath: item.workspacePath,
      sessionId: item.sessionId,
    };
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
  const messageElementSignature = $derived(activeRenderItems.map((item) => item.key).join('|'));

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
    if (displayContext === 'task' && normalizedRuntimeStartedAt > 0) {
      return normalizedRuntimeStartedAt;
    }
    if (!message) {
      return 0;
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
    const hasRuntimeToTrack = displayContext === 'task'
      ? runtimeActive
      : Boolean(currentRuntimeRenderItem) || awaitingCurrentTurnProjection;
    if (!hasRuntimeToTrack) {
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
    if (displayContext === 'task') {
      return runtimeActive && timerStartTime > 0;
    }
    return timerStartTime > 0 && (Boolean(currentRuntimeRenderItem) || awaitingCurrentTurnProjection);
  });

  const shouldShowRuntimeSummary = $derived(
    displayContext === 'task'
      && !runtimeActive
      && normalizedRuntimeCompletedAt > 0
      && normalizedRuntimeDurationMs >= 0,
  );

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
      if (shouldShowRuntimeSummary) {
        entries.push({
          kind: 'runtime-summary',
          key: `runtime-summary:${taskId || ''}:${normalizedRuntimeCompletedAt}:${normalizedRuntimeDurationMs}`,
        });
      }
      return entries;
    }
    entries.splice(runtimeIndicatorInsertionIndex, 0, {
      kind: 'runtime',
      key: runtimeIndicatorKey,
    });
    return entries;
  });
  const conversationRenderEntries = $derived.by(() => {
    const entries: ConversationRenderEntry[] = [];
    const findTurnEntry = (turnId: string) => {
      for (let index = entries.length - 1; index >= 0; index -= 1) {
        const entry = entries[index];
        if (entry.kind === 'turn' && entry.turnId === turnId) return entry;
      }
      return null;
    };

    for (const entry of timelineRenderEntries) {
      if (entry.kind === 'message') {
        const turnId = messageMetadataString(entry.item.message, 'turnId');
        if (!turnId) {
          entries.push(entry);
          continue;
        }
        const previous = entries[entries.length - 1];
        if (previous?.kind === 'turn' && previous.turnId === turnId) {
          previous.items.push(entry.item);
        } else {
          entries.push({
            kind: 'turn',
            key: `turn:${turnId}`,
            turnId,
            items: [entry.item],
          });
        }
        continue;
      }

      if (entry.kind === 'runtime') {
        const runtimeTurnId = currentRuntimeRenderItem
          ? messageMetadataString(currentRuntimeRenderItem.message, 'turnId')
          : '';
        const turnEntry = runtimeTurnId ? findTurnEntry(runtimeTurnId) : null;
        if (turnEntry) {
          turnEntry.runtimeKey = entry.key;
        } else {
          entries.push(entry);
        }
        continue;
      }

      entries.push(entry);
    }
    return entries;
  });
  const conversationDisplayMode = $derived.by(() => (
    messagesState.settingsBootstrapSnapshot?.runtimeSettings?.conversationDisplayMode === 'summary'
      ? 'summary'
      : 'original'
  ));
  const renderEntries = $derived(
    conversationDisplayMode === 'summary'
      ? conversationRenderEntries
      : timelineRenderEntries,
  );
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
  const suggestionScope = $derived.by(() => {
    const workspaceId = typeof messagesState.currentWorkspaceId === 'string'
      ? messagesState.currentWorkspaceId.trim()
      : '';
    const workspacePath = typeof messagesState.currentWorkspacePath === 'string'
      ? messagesState.currentWorkspacePath.trim()
      : '';
    const locale = i18n.locale;
    return {
      key: `${workspaceId || workspacePath || 'personal'}:${locale}:v1`,
      workspaceId,
      workspacePath,
      sessionId: currentSessionId,
      locale,
    };
  });
  const suggestionItems = $derived(sessionSuggestions.activeGroup?.suggestions || []);
  const suggestionSkeletonSlots = $derived(
    sessionSuggestions.loadingInitial
      ? Array.from({ length: sessionSuggestions.suggestionsPerGroup }, (_, index) => index)
      : [],
  );
  const canRotateSuggestions = $derived(
    !sessionSuggestions.generating && suggestionItems.length > 0,
  );

  $effect(() => {
    if (showSuggestions) sessionSuggestions.ensure(suggestionScope);
  });

  function suggestionIcon(category: SessionSuggestion['category']): import('../lib/icons').IconName {
    switch (category) {
      case 'understand': return 'search';
      case 'inspect': return 'file-text';
      case 'plan': return 'list';
      case 'execute': return 'play';
      case 'record': return 'note';
      case 'learn': return 'lightbulb';
      default: return 'sparkles';
    }
  }

  function rotateSuggestions(): void {
    if (sessionSuggestions.generating) return;
    sessionSuggestions.rotate(suggestionScope);
  }

  function fillComposer(text: string) {
    if (typeof window === 'undefined') return;
    sessionSuggestions.markActiveSelected();
    window.dispatchEvent(new CustomEvent('magi:fillComposer', { detail: { text } }));
  }
  const panelKey = $derived.by((): keyof ScrollPositions => (displayContext === 'task' ? (taskId || 'task') : 'thread'));
  const persistedScrollTop = $derived(messagesState.scrollPositions[panelKey] || 0);
  const persistedScrollAnchor = $derived(messagesState.scrollAnchors[panelKey]);
  const shouldAutoScroll = $derived(messagesState.autoScrollEnabled[panelKey]);
  const sessionHistory = $derived(messagesState.sessionHistory);

  function currentScrollScopeKey(): string {
    return `${panelKey}\u0000${currentHistoryLoadScopeKey()}`;
  }
  const canLoadOlderHistory = $derived(Boolean(
    hasHiddenLocalHistory
    || (
      currentSessionId
      && sessionHistory.sessionId === currentSessionId
      && (sessionHistory.workspaceId || '') === currentWorkspaceId
      && (sessionHistory.workspacePath || '') === (messagesState.currentWorkspacePath || '').trim()
      && (
        sessionHistory.canonicalHasMoreBefore
        && sessionHistory.canonicalBeforeCursor
      )
      && sessionHistory.historyLoadStatus !== 'loading'
      && sessionHistory.historyLoadStatus !== 'exhausted'
    )
  ));

  // 容器引用
  let containerRef: HTMLDivElement | null = $state(null);
  const showScrollBtn = $derived(!shouldAutoScroll && safeRenderMessages.length > 0);
  let wasActive = $state(false);
  let lastObservedScrollTop = 0;
  let activationRestoreNonce = 0;
  let contentResizeObserver: ResizeObserver | null = null;
  let contentResizeFrame = 0;
  let layoutObservationNonce = 0;
  let autoScrollScheduleNonce = 0;
  const scrollCoordinator = new MessageScrollCoordinator();
  const layoutStabilizer = new MessageLayoutStabilizer();
  let scrollStateFrame = 0;
  let pendingScrollState: {
    scrollTop: number;
    autoScrollEnabled: boolean;
    persist: boolean;
    panelKey: keyof ScrollPositions;
    scopeKey: string;
    interactionEpoch: number;
    recoveryEpoch: number;
  } | null = null;
  let historyLoadGeneration = 0;
  let historyLoadScopeKey = '';
  let historyLoadRequest: { id: number; scopeKey: string; promise: Promise<boolean> } | null = null;
  let nextHistoryLoadRequestId = 0;
  let scrollInteractionEpoch = 0;
  let scrollRecoveryEpoch = 0;
  let renderWindowNonce = 0;
  let destroyed = false;

  const HISTORY_LOAD_THRESHOLD_PX = 120;

  function currentHistoryLoadScopeKey(): string {
    const workspaceId = (messagesState.currentWorkspaceId || '').trim();
    const workspacePath = (messagesState.currentWorkspacePath || '').trim();
    const sessionId = (messagesState.currentSessionId || '').trim();
    return `${displayContext}\u0000${taskId || ''}\u0000${workspaceId}\u0000${workspacePath}\u0000${sessionId}`;
  }

  // 在 DOM 结构变更前保留阅读锚点，变更后由统一布局事务补偿真实位移。
  // 这个 effect 必须放在滚动状态和容器引用初始化之后。Svelte 5 的
  // $effect.pre 可能在组件初始化阶段立即运行，提前读取 const 会触发
  // TDZ 异常并让整个桌面 Renderer 变成黑屏。
  $effect.pre(() => {
    const signature = `${messageElementSignature}:${runtimeLayoutSignature}`;
    void signature;
    if (!isActive || shouldAutoScroll || !containerRef) return;
    rememberCurrentLayoutAnchor();
  });

  $effect(() => {
    const nextScopeKey = currentHistoryLoadScopeKey();
    if (nextScopeKey === historyLoadScopeKey) return;
    historyLoadScopeKey = nextScopeKey;
    historyLoadGeneration += 1;
    renderWindowNonce += 1;
    scrollInteractionEpoch += 1;
    scrollRecoveryEpoch += 1;
    lastObservedScrollTop = containerRef?.scrollTop || 0;
    layoutObservationNonce += 1;
    autoScrollScheduleNonce += 1;
    disconnectContentResizeObserver();
    cancelScheduledScrollStateSync();
    scrollCoordinator.cancel();
    layoutStabilizer.clear();
  });

  function disconnectContentResizeObserver() {
    contentResizeObserver?.disconnect();
    contentResizeObserver = null;
    if (contentResizeFrame) {
      cancelAnimationFrame(contentResizeFrame);
      contentResizeFrame = 0;
    }
  }

  function settleOwnedHistoryRequest(
    sessionId: string,
    workspaceId: string,
    workspacePath: string,
    requestRevision: number,
    requestCanonicalBeforeCursor: string | null,
    requestId: number,
  ): void {
    const historyState = messagesState.sessionHistory;
    if (
      destroyed
      || historyState.sessionId !== sessionId
      || (historyState.workspaceId || '') !== workspaceId
      || (historyState.workspacePath || '') !== workspacePath
      || historyState.historyLoadStatus !== 'loading'
      || (messagesState.currentSessionId || '').trim() !== sessionId
      || (messagesState.currentWorkspaceId || '').trim() !== workspaceId
      || (messagesState.currentWorkspacePath || '').trim() !== workspacePath
      || historyLoadRequest?.id !== requestId
    ) return;
    setSessionHistoryState(sessionId, {
      workspaceId,
      workspacePath,
      historyLoadStatus: 'idle',
      expectedRevision: requestRevision,
      expectedCanonicalBeforeCursor: requestCanonicalBeforeCursor,
    });
  }

  function compensateLayoutAfterUpdate(
    observationNonce: number,
    observationScopeKey: string,
    observationInteractionEpoch: number,
    observationRecoveryEpoch: number,
  ): void {
    if (
      destroyed
      || !isActive
      || observationNonce !== layoutObservationNonce
      || observationScopeKey !== currentHistoryLoadScopeKey()
      || observationInteractionEpoch !== scrollInteractionEpoch
      || observationRecoveryEpoch !== scrollRecoveryEpoch
      || !containerRef
    ) return;
    if (shouldAutoScroll) {
      scrollPanelToBottom();
      return;
    }
    const currentAnchor = captureVisibleLayoutAnchor();
    const compensatedTop = layoutStabilizer.compensate(
      currentAnchor,
      containerRef.scrollTop,
      {
        scopeKey: currentScrollScopeKey(),
        interactionEpoch: observationInteractionEpoch,
        recoveryEpoch: observationRecoveryEpoch,
      },
    );
    if (compensatedTop !== null) {
      setContainerScrollPosition(compensatedTop, 'restore');
    }
    rememberCurrentLayoutAnchor();
    scheduleScrollStateSync(containerRef.scrollTop, false, false);
  }

  function observeMessageLayoutChanges(observationNonce: number) {
    disconnectContentResizeObserver();
    if (
      observationNonce !== layoutObservationNonce
      || destroyed
      || !isActive
      || !containerRef
      || typeof ResizeObserver === 'undefined'
    ) {
      return;
    }
    const observationScopeKey = currentHistoryLoadScopeKey();
    contentResizeObserver = new ResizeObserver(() => {
      if (
        observationNonce !== layoutObservationNonce
        || destroyed
        || !isActive
        || observationScopeKey !== currentHistoryLoadScopeKey()
        || !containerRef
        || contentResizeFrame
      ) {
        return;
      }
      const observationInteractionEpoch = scrollInteractionEpoch;
      const observationRecoveryEpoch = scrollRecoveryEpoch;
      contentResizeFrame = requestAnimationFrame(() => {
        contentResizeFrame = 0;
        compensateLayoutAfterUpdate(
          observationNonce,
          observationScopeKey,
          observationInteractionEpoch,
          observationRecoveryEpoch,
        );
      });
    });
    for (const element of containerRef.querySelectorAll<HTMLElement>(
      '[data-message-id], [data-layout-observation]',
    )) {
      contentResizeObserver.observe(element);
    }
    if (!layoutStabilizer.hasAnchor) {
      rememberCurrentLayoutAnchor();
    }
  }

  function scheduleActivationScrollRestore() {
    const restoreNonce = ++activationRestoreNonce;

    const attemptRestore = () => {
      if (restoreNonce !== activationRestoreNonce) return;
      if (destroyed || !containerRef || !isActive) return;
      restorePanelScrollPosition(false);
    };

    void tick().then(async () => {
      if (restoreNonce !== activationRestoreNonce) return;
      if (persistedScrollAnchor?.messageId) {
        await revealRenderMessage(persistedScrollAnchor.messageId);
      }
      if (restoreNonce !== activationRestoreNonce) return;
      attemptRestore();
      requestAnimationFrame(() => {
        attemptRestore();
      });
    });
  }

  async function revealRenderMessage(messageId: string): Promise<boolean> {
    const scopeKey = currentHistoryLoadScopeKey();
    const windowNonce = renderWindowNonce;
    const targetIndex = safeRenderItems.findIndex((item) => item.message.id === messageId);
    if (targetIndex < 0) {
      return false;
    }
    const requiredLimit = safeRenderItems.length - targetIndex;
    if (requiredLimit > visibleRenderLimit) {
      visibleRenderLimit = requiredLimit;
      await tick();
      if (
        destroyed
        || windowNonce !== renderWindowNonce
        || scopeKey !== currentHistoryLoadScopeKey()
      ) {
        return false;
      }
    }
    return true;
  }

  function setContainerScrollPosition(
    nextTop: number,
    intent: ProgrammaticScrollIntent = 'restore',
  ) {
    if (!containerRef) return;
    const maxScrollTop = Math.max(0, containerRef.scrollHeight - containerRef.clientHeight);
    const clampedTop = Math.max(0, Math.min(nextTop, maxScrollTop));
    const previousTop = containerRef.scrollTop;
    const transaction = scrollCoordinator.begin(
      clampedTop,
      scrollInteractionEpoch,
      currentScrollScopeKey(),
      intent,
    );
    containerRef.scrollTop = clampedTop;
    scrollCoordinator.retarget(transaction.id, containerRef.scrollTop);
    scrollCoordinator.completeIfUnchanged(
      transaction.id,
      previousTop,
      containerRef.scrollTop,
    );
    lastObservedScrollTop = containerRef.scrollTop;
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
        offsetTop: rect.top - containerRect.top,
      };
    }
    const lastCandidate = candidates[candidates.length - 1];
    if (!lastCandidate) {
      return null;
    }
    const rect = lastCandidate.getBoundingClientRect();
    return {
      messageId: lastCandidate.dataset.messageId || null,
      offsetTop: rect.top - containerRect.top,
    };
  }

  function captureVisibleLayoutAnchor() {
    const anchor = captureVisibleAnchor();
    if (!anchor?.messageId) return null;
    return {
      messageId: anchor.messageId,
      offsetTop: anchor.offsetTop,
    };
  }

  function rememberCurrentLayoutAnchor(): void {
    if (shouldAutoScroll) {
      layoutStabilizer.clear();
      return;
    }
    layoutStabilizer.remember(captureVisibleLayoutAnchor(), {
      scopeKey: currentScrollScopeKey(),
      interactionEpoch: scrollInteractionEpoch,
      recoveryEpoch: scrollRecoveryEpoch,
    });
  }

  function syncPanelScrollState(
    scrollTop: number,
    autoScrollEnabled: boolean,
    persist = true,
    anchor?: ReturnType<typeof captureVisibleAnchor>,
  ) {
    const input: {
      scrollTop: number;
      autoScrollEnabled: boolean;
      anchor?: ReturnType<typeof captureVisibleAnchor>;
    } = { scrollTop, autoScrollEnabled };
    if (anchor !== undefined) input.anchor = anchor;
    updatePanelScrollState(panelKey, input, { persist });
  }

  function cancelScheduledScrollStateSync() {
    pendingScrollState = null;
    if (scrollStateFrame) {
      cancelAnimationFrame(scrollStateFrame);
      scrollStateFrame = 0;
    }
  }

  function scheduleScrollStateSync(scrollTop: number, autoScrollEnabled: boolean, persist = true) {
    pendingScrollState = {
      scrollTop,
      autoScrollEnabled,
      persist,
      panelKey,
      scopeKey: currentScrollScopeKey(),
      interactionEpoch: scrollInteractionEpoch,
      recoveryEpoch: scrollRecoveryEpoch,
    };
    if (scrollStateFrame) return;
    scrollStateFrame = requestAnimationFrame(() => {
      scrollStateFrame = 0;
      const next = pendingScrollState;
      pendingScrollState = null;
      if (!next || destroyed || !containerRef || !isActive) return;
      if (
        next.panelKey !== panelKey
        || next.scopeKey !== currentScrollScopeKey()
        || next.interactionEpoch !== scrollInteractionEpoch
        || next.recoveryEpoch !== scrollRecoveryEpoch
      ) return;
      syncPanelScrollState(
        next.scrollTop,
        next.autoScrollEnabled,
        next.persist,
        captureVisibleAnchor(),
      );
      rememberCurrentLayoutAnchor();
    });
  }

  function scrollPanelToBottom(persist = true) {
    if (!containerRef) return;
    cancelScheduledScrollStateSync();
    setContainerScrollPosition(
      containerRef.scrollHeight - containerRef.clientHeight,
      'follow-bottom',
    );
    layoutStabilizer.clear();
    syncPanelScrollState(containerRef.scrollTop, true, persist);
  }

  function restorePanelScrollPosition(persist = false) {
    if (!containerRef) return;
    cancelScheduledScrollStateSync();
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
        setContainerScrollPosition(
          containerRef.scrollTop + currentOffsetTop - anchor.offsetTop,
          'restore',
        );
        rememberCurrentLayoutAnchor();
        syncPanelScrollState(containerRef.scrollTop, false, persist, captureVisibleAnchor());
        return;
      }
    }
    setContainerScrollPosition(persistedScrollTop, 'restore');
    rememberCurrentLayoutAnchor();
    syncPanelScrollState(containerRef.scrollTop, false, persist, captureVisibleAnchor());
  }

  // 任何可见卡片内容或状态变化都触发滚动判断，覆盖文本、思考、工具和文件卡片。
  $effect(() => {
    const active = isActive;
    const _len = safeRenderMessages.length;
    const _sig = renderContentSignature;
    const _runtimeSig = runtimeLayoutSignature;
    const scheduleNonce = ++autoScrollScheduleNonce;
    void _len;
    void _sig;
    void _runtimeSig;
    if (!active || !shouldAutoScroll || !containerRef) return;
    tick().then(() => {
      if (
        scheduleNonce !== autoScrollScheduleNonce
        || !containerRef
        || !isActive
        || !shouldAutoScroll
      ) return;
      scrollPanelToBottom();
    });
  });

  $effect(() => {
    const active = isActive;
    const signature = `${messageElementSignature}:${runtimeLayoutSignature}`;
    void signature;
    const observationNonce = ++layoutObservationNonce;
    if (!active || !containerRef) {
      disconnectContentResizeObserver();
      return;
    }
    tick().then(() => {
      if (
        observationNonce !== layoutObservationNonce
        || destroyed
        || !isActive
        || !containerRef
      ) return;
      observeMessageLayoutChanges(observationNonce);
      compensateLayoutAfterUpdate(
        observationNonce,
        currentHistoryLoadScopeKey(),
        scrollInteractionEpoch,
        scrollRecoveryEpoch,
      );
    });
  });

  // 没有形成溢出时浏览器不会派发 scroll 事件；历史窗口仍必须能继续向前展开。
  // 必须在同一事务内连续加载，不能依赖加载状态切换后 effect 再次触发，
  // 因为“展开本地窗口”本身可能不改变 effect 依赖值。
  async function loadHistoryUntilScrollable(scopeKey: string): Promise<void> {
    while (
      !destroyed
      && isActive
      && scopeKey === currentHistoryLoadScopeKey()
      && containerRef
      && canLoadOlderHistory
      && messagesState.sessionHistory.historyLoadStatus === 'idle'
      && containerRef.scrollHeight <= containerRef.clientHeight + 1
    ) {
      const loaded = await requestOlderHistoryLoad();
      if (!loaded) return;
      await tick();
    }
  }

  $effect(() => {
    const active = isActive;
    const canLoad = canLoadOlderHistory;
    const itemCount = safeRenderItems.length;
    const historyLoadStatus = sessionHistory.historyLoadStatus;
    void itemCount;
    if (
      !active
      || !canLoad
      || (historyLoadStatus !== 'idle' && !hasHiddenLocalHistory)
      || !containerRef
    ) return;
    const scopeKey = currentHistoryLoadScopeKey();
    void tick().then(() => {
      if (
        destroyed
        || !isActive
        || scopeKey !== currentHistoryLoadScopeKey()
        || !containerRef
        || !canLoadOlderHistory
        || (
          messagesState.sessionHistory.historyLoadStatus !== 'idle'
          && !hasHiddenLocalHistory
        )
      ) return;
      if (containerRef.scrollHeight <= containerRef.clientHeight + 1) {
        void loadHistoryUntilScrollable(scopeKey);
      }
    });
  });

  // 面板可见性或会话作用域变化后恢复对应位置。作用域变化也必须触发恢复，
  // 因为同一个面板实例可能只更新 session 而不会经历重新挂载。
  let previousPanelRestoreScope = '';
  $effect(() => {
    const active = isActive;
    const scope = `${displayContext}\u0000${taskId || ''}\u0000${currentWorkspaceId}\u0000${messagesState.currentWorkspacePath || ''}\u0000${currentSessionId}`;
    const scopeChanged = scope !== previousPanelRestoreScope;
    if (scopeChanged) {
      previousPanelRestoreScope = scope;
      activationRestoreNonce += 1;
    }
    if (active && (scopeChanged || !wasActive) && containerRef) {
      scheduleActivationScrollRestore();
    }
    wasActive = active;
  });

  // 外部触发的消息定位（例如：目标面板点击历史计划，穿透定位到对应对话轮次）
  let handledMessageJumpNonce = 0;
  $effect(() => {
    const jumpNonce = messagesState.messageJump.nonce;
    const targetMessageId = messagesState.messageJump.messageId;
    const jumpScope = currentHistoryLoadScopeKey();
    if (!targetMessageId) return;
    if (jumpNonce === handledMessageJumpNonce) return;
    if (displayContext !== 'thread') return;
    if (!isActive) return;
    if (!containerRef) return;

    const existsInCurrentList = safeRenderItems.some((item) => item.message.id === targetMessageId);
    if (!existsInCurrentList) return;
    handledMessageJumpNonce = jumpNonce;

    void revealRenderMessage(targetMessageId).then(async (revealed) => {
      if (!revealed) return;
      await tick();
      if (
        !containerRef
        || jumpNonce !== messagesState.messageJump.nonce
        || jumpScope !== currentHistoryLoadScopeKey()
      ) return;
      const selectorSafeId = targetMessageId.replace(/"/g, '\\"');
      const targetElement = containerRef.querySelector(`[data-message-id="${selectorSafeId}"]`) as HTMLElement | null;
      if (!targetElement) return;

      const containerRect = containerRef.getBoundingClientRect();
      const elementRect = targetElement.getBoundingClientRect();
      const targetTop = containerRef.scrollTop
        + elementRect.top
        - containerRect.top
        - Math.max(0, (containerRef.clientHeight - elementRect.height) / 2);
      scrollToPositionFromNavigation(targetTop);
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

  function captureScrollSnapshot() {
    if (!containerRef) return null;
    return {
      panelKey,
      scopeKey: currentScrollScopeKey(),
      interactionEpoch: scrollInteractionEpoch,
      recoveryEpoch: scrollRecoveryEpoch,
      anchor: captureVisibleAnchor(),
    };
  }

  function restoreScrollSnapshot(
    snapshot: ReturnType<typeof captureScrollSnapshot>,
    persist = false,
  ) {
    if (destroyed || !snapshot || !containerRef) return;
    if (snapshot.panelKey !== panelKey || snapshot.scopeKey !== currentScrollScopeKey()) {
      return;
    }
    if (
      snapshot.interactionEpoch !== scrollInteractionEpoch
      || snapshot.recoveryEpoch !== scrollRecoveryEpoch
    ) {
      scheduleScrollStateSync(containerRef.scrollTop, shouldAutoScroll);
      return;
    }

    // 自动跟随底部时，历史窗口扩展只改变了视口上方内容；此时唯一正确的
    // 恢复目标仍是底部，不能用旧的可见消息锚点把视口拉回历史位置。
    if (shouldAutoScroll) {
      scrollPanelToBottom(persist);
      return;
    }

    let nextTop: number | null = null;
    const snapshotAnchor = snapshot.anchor;
    const messageId = snapshotAnchor?.messageId;
    if (messageId) {
      const selectorSafeId = messageId.replace(/"/g, '\\"');
      const targetElement = containerRef.querySelector(
        `[data-message-id="${selectorSafeId}"]`,
      ) as HTMLElement | null;
      if (targetElement) {
        const containerRect = containerRef.getBoundingClientRect();
        const elementRect = targetElement.getBoundingClientRect();
        nextTop = containerRef.scrollTop
          + elementRect.top
          - containerRect.top
          - snapshotAnchor.offsetTop;
      }
    }
    // 没有稳定的可见消息锚点时不能用总高度差猜测位置：消息高度、折叠状态和
    // 字体布局变化都可能让该差值与锚点上方的真实增量不同。保留浏览器当前
    // 位置，等待下一次明确的布局事务重新捕获锚点。
    if (nextTop === null) return;
    setContainerScrollPosition(nextTop, 'restore');
    rememberCurrentLayoutAnchor();
    syncPanelScrollState(containerRef.scrollTop, shouldAutoScroll, persist, captureVisibleAnchor());
  }

  async function revealPreviousRenderItems(
    snapshot: ReturnType<typeof captureScrollSnapshot> = captureScrollSnapshot(),
  ): Promise<boolean> {
    if (!hasHiddenLocalHistory) return false;
    const previousLimit = visibleRenderLimit;
    visibleRenderLimit = Math.min(safeRenderItems.length, visibleRenderLimit + RENDER_WINDOW_CHUNK);
    await tick();
    restoreScrollSnapshot(snapshot);
    return visibleRenderLimit > previousLimit;
  }

  async function loadOlderHistory(requestId: number): Promise<boolean> {
    const sessionId = (messagesState.currentSessionId || '').trim();
    const workspaceId = (messagesState.currentWorkspaceId || '').trim();
    const workspacePath = (messagesState.currentWorkspacePath || '').trim();
    if (hasHiddenLocalHistory) {
      return revealPreviousRenderItems();
    }
    if (displayContext !== 'thread' || !sessionId || hasActiveLocalTimelineTurn()) {
      return false;
    }
    const historyState = messagesState.sessionHistory;
    if (
      historyState.sessionId !== sessionId
      || (historyState.workspaceId || '') !== workspaceId
      || (historyState.workspacePath || '') !== workspacePath
      || historyState.historyLoadStatus === 'loading'
      || historyState.historyLoadStatus === 'exhausted'
      || !(historyState.canonicalHasMoreBefore && historyState.canonicalBeforeCursor)
    ) {
      return false;
    }
    const requestGeneration = historyLoadGeneration;
    const requestScopeKey = currentHistoryLoadScopeKey();
    const requestRevision = historyState.revision;
    const requestCanonicalBeforeCursor = historyState.canonicalBeforeCursor;
    setSessionHistoryState(sessionId, {
      workspaceId,
      workspacePath,
      historyLoadStatus: 'loading',
    });
    try {
      const response = await getAgentSessionMessages({
        sessionId,
        scope: workspaceId || workspacePath ? 'workspace' : 'personal',
        workspaceId,
        workspacePath,
        canonicalBeforeCursor: requestCanonicalBeforeCursor,
        limit: 50,
      });
      if (
        requestGeneration !== historyLoadGeneration
        || requestScopeKey !== currentHistoryLoadScopeKey()
      ) {
        settleOwnedHistoryRequest(
          sessionId,
          workspaceId,
          workspacePath,
          requestRevision,
          requestCanonicalBeforeCursor,
          requestId,
        );
        return false;
      }
      const turns = Array.isArray(response.canonicalTurns)
        ? response.canonicalTurns.map((turn, index) => (
          normalizeCanonicalTurnStrict(turn, `history.canonicalTurns[${index}]`)
        ))
        : [];
      // 请求期间用户可能已经改变阅读位置；快照必须在真正插入历史前捕获，不能写回请求开始时的旧位置。
      const snapshot = captureScrollSnapshot();
      const committed = commitOlderSessionHistoryPage({
        sessionId,
        workspaceId,
        workspacePath,
        revision: requestRevision,
        canonicalBeforeCursor: requestCanonicalBeforeCursor,
        turns,
        canonicalHasMoreBefore: response.canonicalHasMoreBefore === true,
        nextCanonicalBeforeCursor: typeof response.canonicalBeforeCursor === 'string'
          ? response.canonicalBeforeCursor
          : null,
      });
      if (!committed.accepted) {
        console.warn('[message-list] 更早的会话历史没有产生可提交的前进量', committed.reason);
        if (committed.reason === 'stale') {
          settleOwnedHistoryRequest(
            sessionId,
            workspaceId,
            workspacePath,
            requestRevision,
            requestCanonicalBeforeCursor,
            requestId,
          );
        }
        return false;
      }
      await tick();
      if (
        requestGeneration !== historyLoadGeneration
        || requestScopeKey !== currentHistoryLoadScopeKey()
      ) {
        settleOwnedHistoryRequest(
          sessionId,
          workspaceId,
          workspacePath,
          requestRevision,
          requestCanonicalBeforeCursor,
          requestId,
        );
        return false;
      }
      if (hasHiddenLocalHistory) {
        await revealPreviousRenderItems(snapshot);
        if (
          requestGeneration !== historyLoadGeneration
          || requestScopeKey !== currentHistoryLoadScopeKey()
        ) {
          settleOwnedHistoryRequest(
            sessionId,
            workspaceId,
            workspacePath,
            requestRevision,
            requestCanonicalBeforeCursor,
            requestId,
          );
          return false;
        }
        return true;
      }
      restoreScrollSnapshot(snapshot);
      return committed.addedTurnCount > 0;
    } catch (error) {
      if (
        requestGeneration !== historyLoadGeneration
        || requestScopeKey !== currentHistoryLoadScopeKey()
      ) {
        settleOwnedHistoryRequest(
          sessionId,
          workspaceId,
          workspacePath,
          requestRevision,
          requestCanonicalBeforeCursor,
          requestId,
        );
        return false;
      }
      console.error('[message-list] 加载更早的会话历史失败:', error);
      setSessionHistoryState(sessionId, {
        workspaceId,
        workspacePath,
        historyLoadStatus: 'error',
        expectedRevision: requestRevision,
        expectedCanonicalBeforeCursor: requestCanonicalBeforeCursor,
      });
      setCanonicalTimelineError(error);
      postBridgeMessage({ type: 'requestState' });
      return false;
    }
  }

  function requestOlderHistoryLoad(): Promise<boolean> {
    const scopeKey = currentHistoryLoadScopeKey();
    if (historyLoadRequest?.scopeKey === scopeKey) return historyLoadRequest.promise;
    const id = ++nextHistoryLoadRequestId;
    const promise = loadOlderHistory(id);
    const request = { id, scopeKey, promise };
    historyLoadRequest = request;
    void promise.then(() => {
      if (historyLoadRequest === request) historyLoadRequest = null;
    }, () => {
      if (historyLoadRequest === request) historyLoadRequest = null;
    });
    return promise;
  }

  function cancelScrollRecoveryForUserIntent(): void {
    scrollRecoveryEpoch += 1;
    layoutStabilizer.clear();
    activationRestoreNonce += 1;
    autoScrollScheduleNonce += 1;
    scrollCoordinator.cancel();
    if (contentResizeFrame) {
      cancelAnimationFrame(contentResizeFrame);
      contentResizeFrame = 0;
    }
    cancelScheduledScrollStateSync();
  }

  function canNestedScrollerConsumeWheel(target: EventTarget | null, deltaY: number): boolean {
    if (!(target instanceof Element) || deltaY === 0) return false;
    let element = target instanceof HTMLElement ? target : target.parentElement;
    while (element && element !== containerRef) {
      const style = getComputedStyle(element);
      if (canScrollableElementConsumeWheel(deltaY, element, style.overflowY)) return true;
      element = element.parentElement;
    }
    return false;
  }

  function handleWheelIntent(event: WheelEvent, node: HTMLDivElement): void {
    const nestedScrollerCanConsume = canNestedScrollerConsumeWheel(event.target, event.deltaY);
    if (!isMainMessageWheelInput({
      deltaY: event.deltaY,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      nestedScrollerCanConsume,
    })) return;
    // 只有主列表会实际消费这次滚轮输入时，才取消它的程序滚动恢复。
    if (event.target instanceof Element && !node.contains(event.target)) return;
    cancelScrollRecoveryForUserIntent();
    if (event.deltaY < 0) {
      // 滚动条已经在顶端时浏览器不会再派发 scroll，但这仍是明确的历史阅读意图。
      if (shouldAutoScroll) {
        updatePanelScrollState(panelKey, { autoScrollEnabled: false }, { persist: false });
      }
      if (node.scrollTop <= HISTORY_LOAD_THRESHOLD_PX) {
        void requestOlderHistoryLoad();
      }
    }
  }

  function handleKeydownIntent(event: KeyboardEvent): void {
    const scrollingUp = event.key === 'ArrowUp'
      || event.key === 'PageUp'
      || event.key === 'Home'
      || (event.key === ' ' && event.shiftKey);
    const scrollingDown = event.key === 'ArrowDown'
      || event.key === 'PageDown'
      || event.key === 'End'
      || (event.key === ' ' && !event.shiftKey);
    if (scrollingUp || scrollingDown) {
      cancelScrollRecoveryForUserIntent();
    }
  }

  function installScrollIntentHandlers(node: HTMLDivElement) {
    let pointerStart: { x: number; y: number; pointerType: string } | null = null;
    let touchStart: { x: number; y: number } | null = null;
    const isInteractiveTarget = (target: EventTarget | null): boolean => (
      target instanceof Element
      && Boolean(target.closest('button, a, input, textarea, select, [contenteditable="true"]'))
    );
    const onWheel = (event: WheelEvent) => handleWheelIntent(event, node);
    const onPointerDown = (event: PointerEvent) => {
      if (event.isPrimary === false) return;
      pointerStart = { x: event.clientX, y: event.clientY, pointerType: event.pointerType };
      if (!isInteractiveTarget(event.target)) {
        node.focus({ preventScroll: true });
      }
    };
    const onPointerMove = (event: PointerEvent) => {
      if (
        !pointerStart
        || event.isPrimary === false
        || (pointerStart.pointerType === 'mouse' && event.buttons === 0)
      ) return;
      const moved = Math.hypot(event.clientX - pointerStart.x, event.clientY - pointerStart.y);
      if (moved > 3) {
        pointerStart = null;
        cancelScrollRecoveryForUserIntent();
      }
    };
    const onPointerEnd = () => {
      pointerStart = null;
    };
    const onTouchStart = (event: TouchEvent) => {
      const touch = event.touches[0];
      if (touch) touchStart = { x: touch.clientX, y: touch.clientY };
    };
    const onTouchMove = (event: TouchEvent) => {
      const touch = event.touches[0];
      if (!touchStart || !touch) return;
      const moved = Math.hypot(touch.clientX - touchStart.x, touch.clientY - touchStart.y);
      if (moved > 3) {
        touchStart = null;
        cancelScrollRecoveryForUserIntent();
      }
    };
    const onTouchEnd = () => {
      touchStart = null;
    };
    const onKeydown = (event: KeyboardEvent) => handleKeydownIntent(event);

    node.addEventListener('wheel', onWheel, { passive: true });
    node.addEventListener('pointerdown', onPointerDown);
    node.addEventListener('pointermove', onPointerMove);
    node.addEventListener('pointerup', onPointerEnd);
    node.addEventListener('pointercancel', onPointerEnd);
    node.addEventListener('touchstart', onTouchStart, { passive: true });
    node.addEventListener('touchmove', onTouchMove, { passive: true });
    node.addEventListener('touchend', onTouchEnd, { passive: true });
    node.addEventListener('touchcancel', onTouchEnd, { passive: true });
    node.addEventListener('keydown', onKeydown);
    return {
      destroy() {
        node.removeEventListener('wheel', onWheel);
        node.removeEventListener('pointerdown', onPointerDown);
        node.removeEventListener('pointermove', onPointerMove);
        node.removeEventListener('pointerup', onPointerEnd);
        node.removeEventListener('pointercancel', onPointerEnd);
        node.removeEventListener('touchstart', onTouchStart);
        node.removeEventListener('touchmove', onTouchMove);
        node.removeEventListener('touchend', onTouchEnd);
        node.removeEventListener('touchcancel', onTouchEnd);
        node.removeEventListener('keydown', onKeydown);
      },
    };
  }

  // 检测用户是否手动滚动
  function handleScroll(event: Event) {
    const target = event.target as HTMLDivElement;
    if (target !== containerRef) return;
    const { scrollTop, scrollHeight, clientHeight } = target;
    const distanceFromBottom = scrollHeight - scrollTop - clientHeight;
    const isNearBottom = distanceFromBottom < 100;
    const scrollScopeKey = currentScrollScopeKey();
    const hadPendingProgrammaticScroll = scrollCoordinator.isPendingFor(
      scrollInteractionEpoch,
      scrollScopeKey,
    );
    const programmaticIntent = scrollCoordinator.consumeIntentIfMatches(
      scrollTop,
      scrollInteractionEpoch,
      scrollScopeKey,
    );
    const isProgrammaticScroll = programmaticIntent !== null;
    const isUnconfirmedProgrammaticScroll = hadPendingProgrammaticScroll && !isProgrammaticScroll;
    const userScrollDirection = deriveMessageScrollDirection(scrollTop, lastObservedScrollTop);
    if (isUnconfirmedProgrammaticScroll) {
      // 旧的应用 scroll 事件可能晚于新的目标写入到达。它没有携带来源，
      // 不能把当前事务取消或写入错误的位置；真实输入会先由 wheel/pointer/touch/keyboard
      // 监听器显式取消事务。
      lastObservedScrollTop = scrollTop;
      return;
    }
    const userScroll = !isProgrammaticScroll && userScrollDirection !== 'none';
    if (userScroll) {
      scrollInteractionEpoch += 1;
    }
    let nextAutoScroll = shouldAutoScroll;
    if (programmaticIntent === 'follow-bottom') {
      nextAutoScroll = true;
    } else if (programmaticIntent) {
      nextAutoScroll = false;
    } else if (userScrollDirection === 'up') {
      nextAutoScroll = false;
    } else if (isNearBottom) {
      nextAutoScroll = true;
    }
    lastObservedScrollTop = scrollTop;
    if (nextAutoScroll !== shouldAutoScroll) {
      updatePanelScrollState(panelKey, { autoScrollEnabled: nextAutoScroll }, { persist: false });
    }
    rememberCurrentLayoutAnchor();
    scheduleScrollStateSync(scrollTop, nextAutoScroll);
    if (!isProgrammaticScroll && userScrollDirection === 'up' && scrollTop <= HISTORY_LOAD_THRESHOLD_PX) {
      void requestOlderHistoryLoad();
    }
  }

  function scrollToPositionFromNavigation(nextTop: number): void {
    if (!containerRef) return;
    scrollInteractionEpoch += 1;
    activationRestoreNonce += 1;
    cancelScheduledScrollStateSync();
    setContainerScrollPosition(nextTop, 'navigation');
    syncPanelScrollState(containerRef.scrollTop, false, true, captureVisibleAnchor());
  }

  // 滚动到底部
  function scrollToBottom() {
    scrollInteractionEpoch += 1;
    activationRestoreNonce += 1;
    updatePanelScrollState(panelKey, { autoScrollEnabled: true }, { persist: false });
    scrollPanelToBottom(false);
  }

  onDestroy(() => {
    destroyed = true;
    activationRestoreNonce += 1;
    layoutObservationNonce += 1;
    autoScrollScheduleNonce += 1;
    historyLoadGeneration += 1;
    disconnectContentResizeObserver();
    cancelScheduledScrollStateSync();
    scrollCoordinator.destroy();
    layoutStabilizer.clear();
    const historyState = messagesState.sessionHistory;
    const sessionId = (messagesState.currentSessionId || '').trim();
    const workspaceId = (messagesState.currentWorkspaceId || '').trim();
    const workspacePath = (messagesState.currentWorkspacePath || '').trim();
    if (
      displayContext === 'thread'
      && historyLoadRequest?.scopeKey === currentHistoryLoadScopeKey()
      && historyState.historyLoadStatus === 'loading'
      && historyState.sessionId === sessionId
      && (historyState.workspaceId || '') === workspaceId
      && (historyState.workspacePath || '') === workspacePath
    ) {
      setSessionHistoryState(sessionId, {
        workspaceId,
        workspacePath,
        historyLoadStatus: 'idle',
        expectedRevision: historyState.revision,
        expectedCanonicalBeforeCursor: historyState.canonicalBeforeCursor,
      });
    }
    if (!containerRef) {
      return;
    }
    syncPanelScrollState(containerRef.scrollTop, shouldAutoScroll, true, captureVisibleAnchor());
  });
</script>

<div class="message-list-wrapper" class:has-turn-navigation={showTurnNavigation}>
  <TurnNavigationRail
    items={turnNavigationItems}
    container={containerRef}
    onRevealMessage={revealRenderMessage}
    onScrollToPosition={scrollToPositionFromNavigation}
  />
  <div
    class="message-list"
    bind:this={containerRef}
    role="log"
    tabindex="-1"
    use:installScrollIntentHandlers
    onscroll={handleScroll}
    data-panel-id={displayContext === 'thread' ? 'thread' : (taskId || 'task')}
    data-display-context={displayContext}
    data-conversation-display-mode={conversationDisplayMode}
    data-panel-active={isActive ? 'true' : 'false'}
  >
    {#if timelineRenderEntries.length === 0}
      <div class="empty-state">
        <div class:empty-icon-with-suggestions={showSuggestions} class="empty-icon">
          <Icon name={showSuggestions ? 'sparkles' : emptyIcon} size={showSuggestions ? 26 : 48} />
        </div>
        <p class="empty-text">{emptyTitle}</p>
        <p class="empty-hint">{emptyHint}</p>
        {#if showSuggestions}
          <div class="empty-suggestions">
            <div class="suggestions-header">
              <span class="suggestions-title">{i18n.t('messageList.suggestions.title')}</span>
              {#if suggestionItems.length > 0}
                <button
                  type="button"
                  class="suggestions-refresh"
                  onclick={rotateSuggestions}
                  disabled={!canRotateSuggestions}
                  title={i18n.t('messageList.suggestions.refresh')}
                >
                  <Icon
                    name="refresh"
                    size={14}
                    class={sessionSuggestions.generating ? 'suggestions-refresh-spinning' : ''}
                  />
                  <span>{i18n.t('messageList.suggestions.refresh')}</span>
                </button>
              {/if}
            </div>
            {#each suggestionItems as suggestion (suggestion.prompt)}
              <button type="button" class="suggestion-card" onclick={() => fillComposer(suggestion.prompt)}>
                <span class="suggestion-icon">
                  <Icon name={suggestionIcon(suggestion.category)} size={15} />
                </span>
                <span class="suggestion-copy">
                  <span class="suggestion-label">{suggestion.label}</span>
                  <span class="suggestion-text">{suggestion.prompt}</span>
                </span>
                <span class="suggestion-arrow"><Icon name="chevron-right" size={15} /></span>
              </button>
            {/each}
            {#each suggestionSkeletonSlots as slot (slot)}
              <div class="suggestion-card suggestion-card--skeleton" aria-hidden="true">
                <span class="skeleton-icon"></span>
                <span class="suggestion-copy">
                  <span class="skeleton-line skeleton-line--label"></span>
                  <span class="skeleton-line skeleton-line--text"></span>
                  <span class="skeleton-line skeleton-line--text skeleton-line--text-short"></span>
                </span>
              </div>
            {/each}
            {#if sessionSuggestions.loadingInitial}
              <span class="suggestions-meta" role="status">{i18n.t('messageList.suggestions.loading')}</span>
            {:else if sessionSuggestions.unavailable}
              <div class="suggestions-unavailable">
                <span>{i18n.t('messageList.suggestions.unavailable')}</span>
                <button type="button" class="suggestions-refresh" onclick={rotateSuggestions}>
                  <Icon name="refresh" size={14} />
                  <span>{i18n.t('messageList.suggestions.retry')}</span>
                </button>
              </div>
            {:else if suggestionItems.length > 0}
              <span class="suggestions-meta">
                {suggestionScope.workspaceId || suggestionScope.workspacePath
                  ? i18n.t('messageList.suggestions.meta.workspace')
                  : i18n.t('messageList.suggestions.meta.personal')}
              </span>
            {/if}
          </div>
        {/if}
        {#if canLoadOlderHistory || sessionHistory.historyLoadStatus === 'loading'}
          <button
            type="button"
            class="empty-history-load"
            onclick={() => requestOlderHistoryLoad()}
            disabled={!canLoadOlderHistory}
          >
            <Icon name={sessionHistory.historyLoadStatus === 'loading' ? 'loader' : 'chevron-up'} size={14} />
            <span>{sessionHistory.historyLoadStatus === 'loading' ? i18n.t('messageList.loadingOlder') : i18n.t('messageList.loadOlder')}</span>
          </button>
        {/if}
      </div>
    {:else}
      {#each renderEntries as entry (entry.key)}
        {#if entry.kind === 'turn'}
          <ConversationTurn
            turnId={entry.turnId}
            items={entry.items}
            {readOnly}
            {displayContext}
            runtimeActive={Boolean(entry.runtimeKey)}
            {elapsedSeconds}
            initialExpanded={Boolean(entry.runtimeKey)}
            {filePreviewScopeForItem}
            canEditMessage={(item) => canEditUserMessage(item.message)}
            editMessage={(item) => editUserMessage(item.message)}
            continueInterruptedSession={continueInterruptedSession}
          />
        {:else if entry.kind === 'message'}
          <MessageItem
            message={entry.item.message}
            {readOnly}
            {displayContext}
            filePreviewScope={filePreviewScopeForItem(entry.item)}
            canEdit={canEditUserMessage(entry.item.message)}
            onEdit={() => editUserMessage(entry.item.message)}
            onContinueInterrupted={continueInterruptedSession}
          />
        {:else}
          {#if entry.kind === 'runtime'}
            <div class="message-runtime-entry" data-layout-observation="runtime">
              <TurnRuntimeIndicator elapsedSeconds={elapsedSeconds} />
            </div>
          {:else}
            <div class="message-runtime-entry" data-layout-observation="runtime-summary">
              <TurnRuntimeSummary
                durationMs={normalizedRuntimeDurationMs}
                completedAt={normalizedRuntimeCompletedAt}
              />
            </div>
          {/if}
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
    scrollbar-gutter: stable;
    /* 消息列随中间面板自适应铺满，仅保留基础安全留白。 */
    padding-block: var(--space-4);
    padding-inline: var(--space-4);
    /* 阅读位置由应用消息锚点统一维护，避免浏览器锚点与应用恢复竞争。 */
    overflow-anchor: none;
  }

  .message-runtime-entry {
    flex: 0 0 auto;
  }

  /* 轮次导航轨道只占用左侧窄条，不限制消息列的横向自适应宽度。 */
  @container message-list (min-width: 640px) {
    .message-list-wrapper.has-turn-navigation .message-list {
      padding-left: calc(var(--space-4) + 8px);
    }
  }

  /* 纵向居中由全局 .empty-state 的 flex: 1 + justify-content: center 负责，
     这里不再声明固定高度，内容超过可视高度时才自然向下滚动。 */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1 1 auto;
    min-height: 0;
    width: 100%;
    margin: 0;
    box-sizing: border-box;
    text-align: center;
    color: var(--foreground-muted);
    /* 横向不再额外内缩，空状态内容宽度与输入框外框完全一致 */
    padding: var(--space-8) 0;
  }

  .empty-icon {
    width: var(--icon-2xl);
    height: var(--icon-2xl);
    margin-bottom: var(--space-4);
    opacity: 0.3;
    color: var(--foreground-muted);
  }

  .empty-icon-with-suggestions {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    margin-bottom: var(--space-3);
    /* 空状态顶部使用纯图标，避免再叠加一个与页面层级无关的卡片边框。 */
    border: 0;
    background: transparent;
    color: var(--primary);
    opacity: 1;
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
    /* 建议卡片保持原来的内容列宽；消息记录本身仍由 message-list 全宽自适应。 */
    width: min(100%, 768px);
    margin-top: var(--space-6);
    text-align: left;
  }

  .suggestions-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    /* 刷新按钮只在建议就绪后出现，预留其高度可避免加载完成时整块建议区上移。 */
    min-height: 28px;
    margin-bottom: var(--space-1);
  }

  .suggestions-title {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground-muted);
    text-align: left;
    letter-spacing: 0.01em;
  }

  .suggestions-refresh {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 28px;
    padding: 0 var(--space-2);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
    transition: color var(--transition-fast), background var(--transition-fast);
  }

  .suggestions-refresh:hover:not(:disabled) {
    background: var(--surface-hover, rgba(255,255,255,0.05));
    color: var(--foreground);
  }

  .suggestions-refresh:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 1px;
  }

  .suggestions-refresh:disabled {
    cursor: default;
    opacity: 0.55;
  }

  .suggestions-refresh :global(.suggestions-refresh-spinning) {
    animation: suggestions-refresh-spin 0.9s linear infinite;
  }

  @keyframes suggestions-refresh-spin {
    to { transform: rotate(360deg); }
  }

  .suggestion-card {
    display: flex;
    /* 图标与箭头相对文本块纵向居中，避免固定 margin 造成的视觉错位 */
    align-items: center;
    text-align: left;
    gap: var(--space-3);
    /* 与 .suggestion-copy 的固定高度一致，让加载骨架与真实卡片占据同一空间。 */
    min-height: 68px;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1, rgba(255,255,255,0.02));
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .suggestion-card:hover:not(.suggestion-card--skeleton) {
    background: var(--surface-hover, rgba(255,255,255,0.05));
    border-color: color-mix(in srgb, var(--primary) 34%, var(--border));
  }

  .suggestion-card:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 1px;
  }

  .suggestion-card--skeleton {
    cursor: default;
  }

  .suggestion-icon {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    flex: 0 0 30px;
    border-radius: 9px;
    background: color-mix(in srgb, var(--primary) 10%, transparent);
    color: var(--primary);
  }

  /* 真实内容按实际文字高度居中，避免单行描述因预留两行高度而视觉偏上。 */
  .suggestion-copy {
    display: flex;
    flex: 1;
    flex-direction: column;
    justify-content: center;
    min-width: 0;
  }

  /* 骨架屏保留固定高度，避免加载态与内容态切换时卡片跳动。 */
  .suggestion-card--skeleton .suggestion-copy {
    min-height: 52px;
  }

  .suggestion-label {
    display: block;
    margin-bottom: 2px;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    line-height: 1.5;
    /* 卡片是 button，全局按钮样式的 nowrap 会继承下来，需在文本层显式恢复换行。 */
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .suggestion-text {
    display: -webkit-box;
    overflow: hidden;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
    /* 覆盖全局按钮样式继承来的 nowrap，否则两行截断退化为单行溢出。 */
    white-space: normal;
    overflow-wrap: anywhere;
  }

  .skeleton-icon {
    width: 30px;
    height: 30px;
    flex: 0 0 30px;
    border-radius: 9px;
    background: var(--surface-2);
    animation: suggestion-skeleton-pulse 1.6s ease-in-out infinite;
  }

  .skeleton-line {
    display: block;
    height: 9px;
    border-radius: var(--radius-xs);
    background: var(--surface-2);
    animation: suggestion-skeleton-pulse 1.6s ease-in-out infinite;
  }

  .skeleton-line--label {
    width: 34%;
    margin-top: 2px;
    margin-bottom: 6px;
  }

  .skeleton-line--text {
    width: 82%;
    margin-bottom: 7px;
  }

  .skeleton-line--text-short {
    width: 54%;
    margin-bottom: 0;
  }

  @keyframes suggestion-skeleton-pulse {
    0%, 100% { opacity: 0.5; }
    50% { opacity: 1; }
  }

  @media (prefers-reduced-motion: reduce) {
    .skeleton-icon,
    .skeleton-line,
    .suggestions-refresh :global(.suggestions-refresh-spinning) {
      animation: none;
    }
  }

  .suggestion-arrow {
    display: grid;
    place-items: center;
    color: var(--foreground-muted);
    opacity: 0.65;
    transition: transform var(--transition-fast), color var(--transition-fast);
  }

  .suggestion-card:hover .suggestion-arrow {
    color: var(--primary);
    transform: translateX(2px);
  }

  .suggestions-meta {
    align-self: flex-start;
    margin-top: var(--space-1);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    opacity: 0.75;
  }

  .suggestions-unavailable {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    margin-top: var(--space-1);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
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
