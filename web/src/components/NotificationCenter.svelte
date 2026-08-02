<script lang="ts">
  import {
    getNotifications,
    getUnreadNotificationCount,
    getNotificationCenterStatus,
    loadNotifications,
    markAllNotificationsRead,
    clearAllNotifications,
    removeNotification,
    resolveNotification,
    messagesState,
    type Notification,
  } from '../stores/messages.svelte';
  import { showFeedback } from '../lib/notifications';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    open: boolean;
    onOpenChange: (open: boolean) => void;
  }

  let { open, onOpenChange }: Props = $props();
  let activeFilter = $state<'all' | 'unresolved' | 'resolved'>('all');
  let wasOpen = $state(false);
  let expandedNotificationIds = $state<Set<string>>(new Set());

  const notifications = $derived.by(() => getNotifications() as Notification[]);
  const unreadCount = $derived.by(() => getUnreadNotificationCount() as number);
  const notificationStatus = $derived.by(() => getNotificationCenterStatus());
  const hasWorkspace = $derived(Boolean(messagesState.currentWorkspaceId?.trim()));
  const filteredNotifications = $derived.by(() => (
    activeFilter === 'all'
      ? notifications
      : notifications.filter((notif) => (
        activeFilter === 'resolved' ? notif.resolved : !notif.resolved
      ))
  ));

  $effect(() => {
    if (open && !wasOpen && messagesState.bootstrapped && hasWorkspace) {
      if (unreadCount > 0) {
        markAllNotificationsRead();
      } else {
        loadNotifications();
      }
    }
    wasOpen = open;
  });

  function closePanel() {
    onOpenChange(false);
  }

  function handleClearAll() {
    if (!messagesState.bootstrapped || !hasWorkspace) {
      return;
    }
    clearAllNotifications();
  }

  function handleRemove(id: string) {
    if (!messagesState.bootstrapped || !hasWorkspace) {
      return;
    }
    removeNotification(id);
  }

  function handleResolve(id: string) {
    if (!messagesState.bootstrapped || !hasWorkspace) {
      return;
    }
    resolveNotification(id);
  }

  function formatTime(timestamp: number): string {
    const d = new Date(timestamp);
    return d.toLocaleString(i18n.locale, {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  }

  function getScopeLabel(scope: Notification['scope']): string {
    return i18n.t(`notification.scope.${scope}`);
  }

  function getTypeIcon(type: string): 'check' | 'close' | 'warning' | 'info' {
    switch (type) {
      case 'success': return 'check';
      case 'error': return 'close';
      case 'warning': return 'warning';
      default: return 'info';
    }
  }

  function hasDiagnosticDetails(notification: Notification): boolean {
    return Boolean(
      (notification.detail && notification.detail !== notification.message)
      || notification.errorCode
      || notification.failureStage
      || notification.taskId
      || notification.requestId
      || notification.source,
    );
  }

  function toggleNotificationDetails(id: string): void {
    const next = new Set(expandedNotificationIds);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    expandedNotificationIds = next;
  }

  function diagnosticText(notification: Notification): string {
    return [
      notification.title,
      notification.message,
      notification.detail && notification.detail !== notification.message
        ? notification.detail
        : undefined,
      notification.errorCode
        ? `${i18n.t('notification.errorCode')}: ${notification.errorCode}`
        : undefined,
      notification.failureStage
        ? `${i18n.t('notification.failureStage')}: ${notification.failureStage}`
        : undefined,
      notification.taskId
        ? `${i18n.t('notification.taskId')}: ${notification.taskId}`
        : undefined,
      notification.requestId
        ? `${i18n.t('notification.requestId')}: ${notification.requestId}`
        : undefined,
      notification.source
        ? `${i18n.t('notification.source')}: ${notification.source}`
        : undefined,
      `${i18n.t('notification.time')}: ${new Date(notification.timestamp).toISOString()}`,
    ].filter((value): value is string => Boolean(value)).join('\n');
  }

  async function copyDiagnostic(notification: Notification): Promise<void> {
    try {
      await navigator.clipboard.writeText(diagnosticText(notification));
      showFeedback('success', i18n.t('notification.copySuccess'), {
        presentation: 'toast',
        source: 'notification-center',
      });
    } catch (error) {
      console.warn('[NotificationCenter] 复制诊断信息失败:', error);
      showFeedback('error', i18n.t('notification.copyFailed'), {
        presentation: 'toast',
        source: 'notification-center',
      });
    }
  }
</script>

<div class="notification-center">
  {#if open}
    <div class="notification-panel" data-magi-surface="popover">
      <div class="panel-header">
        <span class="panel-title">{i18n.t('notification.title')}</span>
        <div class="panel-actions">
          {#if notifications.length > 0}
            <button
              class="btn-text"
              onclick={handleClearAll}
              title={i18n.t('notification.clearAllTitle')}
              disabled={notificationStatus.isLoading}
            >
              {i18n.t('notification.clearAll')}
            </button>
          {/if}
          <button class="btn-icon btn-icon--xs" onclick={closePanel} title={i18n.t('notification.closeTitle')}>
            <Icon name="close" size={12} />
          </button>
        </div>
      </div>
      {#if notifications.length > 0}
        <div class="panel-filter">
          <button class="filter-btn" class:active={activeFilter === 'all'} onclick={() => activeFilter = 'all'}>
            {i18n.t('notification.filterAll')}
          </button>
          <button class="filter-btn" class:active={activeFilter === 'unresolved'} onclick={() => activeFilter = 'unresolved'}>
            {i18n.t('notification.filterUnresolved')}
          </button>
          <button class="filter-btn" class:active={activeFilter === 'resolved'} onclick={() => activeFilter = 'resolved'}>
            {i18n.t('notification.filterResolved')}
          </button>
        </div>
      {/if}
      <div class="notification-list">
        {#if notificationStatus.isLoading && filteredNotifications.length === 0}
          <div class="empty-state">
            <Icon name="refresh" size={24} class="spin" />
            <span>{i18n.t('notification.loading')}</span>
          </div>
        {:else if notificationStatus.error && filteredNotifications.length === 0}
          <div class="empty-state empty-state--error">
            <Icon name="warning" size={24} />
            <span>{i18n.t('notification.operationFailed')}</span>
          </div>
        {:else if filteredNotifications.length === 0}
          <div class="empty-state">
            <Icon name="bell" size={24} />
            <span>{activeFilter === 'all' ? i18n.t('notification.empty') : i18n.t('notification.emptyFiltered')}</span>
          </div>
        {:else}
          {#if notificationStatus.error}
            <div class="inline-error">
              <Icon name="warning" size={12} />
              <span>{i18n.t('notification.operationFailed')}</span>
            </div>
          {/if}
          {#each filteredNotifications as notif (notif.id)}
            <div class="notification-item type-{notif.type}" class:resolved={notif.resolved}>
              <div class="notif-icon">
                <Icon name={getTypeIcon(notif.type)} size={14} />
              </div>
              <div class="notif-content">
                {#if notif.title}
                  <div class="notif-title">{notif.title}</div>
                {/if}
                <div class="notif-message">{notif.message}</div>
                <div class="notif-meta">
                  <span class="notif-scope">{getScopeLabel(notif.scope)}</span>
                  {#if notif.occurrenceCount > 1}
                    <span class="notif-count">{i18n.t('notification.occurrences', { count: notif.occurrenceCount })}</span>
                  {/if}
                  <span class="notif-time" title={new Date(notif.timestamp).toISOString()}>
                    {formatTime(notif.timestamp)}
                  </span>
                </div>
                {#if expandedNotificationIds.has(notif.id)}
                  <div class="notif-details">
                    {#if notif.detail && notif.detail !== notif.message}
                      <pre>{notif.detail}</pre>
                    {/if}
                    <dl>
                      {#if notif.errorCode}
                        <div><dt>{i18n.t('notification.errorCode')}</dt><dd>{notif.errorCode}</dd></div>
                      {/if}
                      {#if notif.failureStage}
                        <div><dt>{i18n.t('notification.failureStage')}</dt><dd>{notif.failureStage}</dd></div>
                      {/if}
                      {#if notif.taskId}
                        <div><dt>{i18n.t('notification.taskId')}</dt><dd>{notif.taskId}</dd></div>
                      {/if}
                      {#if notif.requestId}
                        <div><dt>{i18n.t('notification.requestId')}</dt><dd>{notif.requestId}</dd></div>
                      {/if}
                      {#if notif.source}
                        <div><dt>{i18n.t('notification.source')}</dt><dd>{notif.source}</dd></div>
                      {/if}
                    </dl>
                  </div>
                {/if}
              </div>
              <div class="notif-actions">
                {#if hasDiagnosticDetails(notif)}
                  <button
                    class="notif-action"
                    onclick={() => toggleNotificationDetails(notif.id)}
                    title={expandedNotificationIds.has(notif.id)
                      ? i18n.t('notification.collapseDetailsTitle')
                      : i18n.t('notification.expandDetailsTitle')}
                    aria-expanded={expandedNotificationIds.has(notif.id)}
                  >
                    <Icon name={expandedNotificationIds.has(notif.id) ? 'chevron-down' : 'chevron-right'} size={10} />
                  </button>
                {/if}
                <button
                  class="notif-action"
                  onclick={() => copyDiagnostic(notif)}
                  title={i18n.t('notification.copyTitle')}
                >
                  <Icon name="copy" size={10} />
                </button>
                {#if !notif.resolved}
                  <button
                    class="notif-action"
                    onclick={() => handleResolve(notif.id)}
                    title={i18n.t('notification.resolveTitle')}
                    disabled={notificationStatus.isLoading}
                  >
                    <Icon name="check" size={11} />
                  </button>
                {/if}
                <button
                  class="notif-action"
                  onclick={() => handleRemove(notif.id)}
                  title={i18n.t('notification.removeTitle')}
                  disabled={notificationStatus.isLoading}
                >
                  <Icon name="close" size={10} />
                </button>
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .notification-center {
    display: contents;
  }

  .notification-panel {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: var(--space-2, 4px);
    width: min(480px, calc(100vw - 24px));
    max-height: min(420px, calc(100vh - 72px));
    background: var(--dropdown-bg);
    border: 1px solid var(--border);
    border-radius: var(--radius-md, 6px);
    box-shadow: var(--shadow-lg, 0 8px 24px rgba(0,0,0,0.3));
    z-index: var(--z-dropdown, 100);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-3, 8px) var(--space-4, 12px);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .panel-title {
    font-size: var(--text-xs, 11px);
    font-weight: var(--font-semibold, 600);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0;
  }

  .panel-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2, 4px);
  }

  .panel-filter {
    display: flex;
    align-items: center;
    gap: var(--space-2, 4px);
    padding: var(--space-2, 6px) var(--space-3, 8px);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .filter-btn {
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground-muted);
    border-radius: 999px;
    font-size: var(--text-xs, 11px);
    padding: 3px 8px;
    cursor: pointer;
    transition: all var(--transition-fast, 0.15s);
  }

  .filter-btn:hover {
    color: var(--foreground);
    border-color: var(--foreground-muted);
  }

  .filter-btn.active {
    color: var(--foreground);
    background: var(--surface-hover);
    border-color: var(--foreground-muted);
  }

  .btn-text {
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    font-size: var(--text-xs, 11px);
    cursor: pointer;
    padding: 2px 6px;
    border-radius: var(--radius-sm, 4px);
    transition: all var(--transition-fast, 0.15s);
  }

  .btn-text:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .btn-text:disabled {
    cursor: default;
    opacity: 0.45;
  }

  .notification-list {
    overflow-y: auto;
    flex: 1;
    padding: var(--space-2, 4px) 0;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3, 8px);
    padding: var(--space-6, 24px);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
    width: 100%;
    box-sizing: border-box;
  }

  .empty-state--error {
    color: var(--error);
  }

  .inline-error {
    display: flex;
    align-items: center;
    gap: var(--space-2, 4px);
    padding: var(--space-2, 6px) var(--space-4, 12px);
    color: var(--error);
    font-size: var(--text-xs, 11px);
    border-bottom: 1px solid var(--border);
  }

  .spin {
    animation: notification-spin 0.9s linear infinite;
  }

  @keyframes notification-spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  .notification-item {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3, 8px);
    padding: var(--space-3, 8px) var(--space-4, 12px);
    transition: background var(--transition-fast, 0.15s);
    position: relative;
  }

  .notification-item:hover {
    background: var(--surface-hover);
  }

  .notification-item.resolved {
    opacity: 0.68;
  }

  .notif-icon {
    flex-shrink: 0;
    width: 18px;
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-top: 1px;
  }

  .type-success .notif-icon { color: var(--success, #4caf50); }
  .type-error .notif-icon { color: var(--error, #e45454); }
  .type-warning .notif-icon { color: var(--warning, #ffb74d); }
  .type-info .notif-icon { color: var(--primary, #007acc); }

  .notif-content {
    flex: 1;
    min-width: 0;
  }

  .notif-title {
    font-size: var(--text-sm, 13px);
    font-weight: var(--font-semibold, 600);
    color: var(--foreground);
    margin-bottom: 2px;
    word-break: break-word;
  }

  .notif-message {
    font-size: var(--text-sm, 13px);
    color: var(--foreground-muted);
    line-height: var(--leading-normal, 1.5);
    word-break: break-word;
    white-space: pre-wrap;
  }

  .notif-details {
    margin-top: var(--space-3, 8px);
    padding-top: var(--space-3, 8px);
    border-top: 1px solid var(--border);
  }

  .notif-details pre {
    margin: 0 0 var(--space-3, 8px);
    max-height: 180px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    line-height: 1.5;
    color: var(--foreground);
  }

  .notif-details dl {
    display: grid;
    gap: 4px;
    margin: 0;
  }

  .notif-details dl div {
    display: grid;
    grid-template-columns: 82px minmax(0, 1fr);
    gap: var(--space-2, 6px);
    font-size: var(--text-xs, 11px);
    line-height: 1.45;
  }

  .notif-details dt {
    color: var(--foreground-muted);
  }

  .notif-details dd {
    margin: 0;
    color: var(--foreground);
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }

  .notif-meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2, 4px);
    margin-top: 4px;
  }

  .notif-scope,
  .notif-count {
    font-size: 10px;
    color: var(--foreground-muted);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 4px);
    padding: 1px 6px;
  }

  .notif-time {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
  }

  .notif-actions {
    display: flex;
    flex-shrink: 0;
    gap: 2px;
  }

  .notif-action {
    flex-shrink: 0;
    width: 18px;
    height: 18px;
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm, 4px);
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0.65;
    transition: all var(--transition-fast, 0.15s);
  }

  .notification-item:hover .notif-action {
    opacity: 1;
  }

  .notif-action:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .notif-action:disabled {
    cursor: default;
    opacity: 0.45;
  }
</style>
