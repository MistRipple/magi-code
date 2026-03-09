<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import {
    markAllNotificationsRead,
    clearAllNotifications,
    removeNotification,
    type Notification,
  } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  const appState = getState();

  let panelOpen = $state(false);
  let activeFilter = $state<'all' | 'incident' | 'audit'>('all');

  const notifications = $derived(appState.notifications as Notification[]);
  const unreadCount = $derived(appState.unreadNotificationCount as number);
  const filteredNotifications = $derived(
    activeFilter === 'all'
      ? notifications
      : notifications.filter((notif) => notif.category === activeFilter)
  );

  function togglePanel() {
    panelOpen = !panelOpen;
    if (panelOpen) {
      markAllNotificationsRead();
    }
  }

  function closePanel() {
    panelOpen = false;
  }

  function handleClearAll() {
    clearAllNotifications();
  }

  function handleRemove(id: string) {
    removeNotification(id);
  }

  function formatTime(timestamp: number): string {
    const d = new Date(timestamp);
    return d.toLocaleTimeString(i18n.locale, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  }

  function getCategoryLabel(category: 'incident' | 'audit'): string {
    if (category === 'incident') return i18n.t('notification.categoryIncident');
    return i18n.t('notification.categoryAudit');
  }

  function getTypeIcon(type: string): 'check' | 'close' | 'warning' | 'info' {
    switch (type) {
      case 'success': return 'check';
      case 'error': return 'close';
      case 'warning': return 'warning';
      default: return 'info';
    }
  }
</script>

<div class="notification-center">
  <button class="btn-icon btn-icon--sm notification-btn" onclick={togglePanel} title={i18n.t('notification.buttonTitle')}>
    <Icon name="bell" size={14} />
    {#if unreadCount > 0}
      <span class="badge">{unreadCount > 99 ? '99+' : unreadCount}</span>
    {/if}
  </button>

  {#if panelOpen}
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="panel-backdrop" onclick={closePanel} role="presentation"></div>
    <div class="notification-panel">
      <div class="panel-header">
        <span class="panel-title">{i18n.t('notification.title')}</span>
        <div class="panel-actions">
          {#if notifications.length > 0}
            <button class="btn-text" onclick={handleClearAll} title={i18n.t('notification.clearAllTitle')}>{i18n.t('notification.clearAll')}</button>
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
          <button class="filter-btn" class:active={activeFilter === 'incident'} onclick={() => activeFilter = 'incident'}>
            {i18n.t('notification.filterIncident')}
          </button>
          <button class="filter-btn" class:active={activeFilter === 'audit'} onclick={() => activeFilter = 'audit'}>
            {i18n.t('notification.filterAudit')}
          </button>
        </div>
      {/if}
      <div class="notification-list">
        {#if filteredNotifications.length === 0}
          <div class="empty-state">
            <Icon name="bell" size={24} />
            <span>{activeFilter === 'all' ? i18n.t('notification.empty') : i18n.t('notification.emptyFiltered')}</span>
          </div>
        {:else}
          {#each filteredNotifications as notif (notif.id)}
            <div class="notification-item type-{notif.type}">
              <div class="notif-icon">
                <Icon name={getTypeIcon(notif.type)} size={14} />
              </div>
              <div class="notif-content">
                {#if notif.title}
                  <div class="notif-title">{notif.title}</div>
                {/if}
                <div class="notif-message">{notif.message}</div>
                <div class="notif-meta">
                  <span class="notif-category">{getCategoryLabel(notif.category)}</span>
                  <span class="notif-time">{formatTime(notif.timestamp)}</span>
                </div>
              </div>
              <button class="notif-remove" onclick={() => handleRemove(notif.id)} title={i18n.t('notification.removeTitle')}>
                <Icon name="close" size={10} />
              </button>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .notification-center {
    position: relative;
  }

  .notification-btn {
    position: relative;
  }

  .badge {
    position: absolute;
    top: -2px;
    right: -2px;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    font-size: 9px;
    font-weight: var(--font-bold, 700);
    line-height: 14px;
    text-align: center;
    color: #fff;
    background: var(--error, #e45454);
    border-radius: 7px;
    pointer-events: none;
  }

  .panel-backdrop {
    position: fixed;
    inset: 0;
    z-index: calc(var(--z-dropdown, 100) - 1);
    background: transparent;
  }

  .notification-panel {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: var(--space-2, 4px);
    width: 320px;
    max-height: 400px;
    background: var(--vscode-dropdown-background, #3c3c3c);
    border: 1px solid var(--border, #454545);
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
    border-bottom: 1px solid var(--border, #454545);
    flex-shrink: 0;
  }

  .panel-title {
    font-size: var(--text-xs, 11px);
    font-weight: var(--font-semibold, 600);
    color: var(--foreground-muted, #999);
    text-transform: uppercase;
    letter-spacing: 0.5px;
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
    border-bottom: 1px solid var(--border, #454545);
    flex-shrink: 0;
  }

  .filter-btn {
    border: 1px solid var(--border, #454545);
    background: var(--surface, rgba(255,255,255,0.03));
    color: var(--foreground-muted, #999);
    border-radius: 999px;
    font-size: var(--text-xs, 11px);
    padding: 3px 8px;
    cursor: pointer;
    transition: all var(--transition-fast, 0.15s);
  }

  .filter-btn:hover {
    color: var(--foreground, #ccc);
    border-color: var(--foreground-muted, #999);
  }

  .filter-btn.active {
    color: var(--foreground, #ddd);
    background: var(--surface-hover, rgba(255,255,255,0.08));
    border-color: var(--foreground-muted, #999);
  }

  .btn-text {
    background: transparent;
    border: none;
    color: var(--foreground-muted, #999);
    font-size: var(--text-xs, 11px);
    cursor: pointer;
    padding: 2px 6px;
    border-radius: var(--radius-sm, 4px);
    transition: all var(--transition-fast, 0.15s);
  }

  .btn-text:hover {
    background: var(--surface-hover, rgba(255,255,255,0.06));
    color: var(--foreground, #ccc);
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
    color: var(--foreground-muted, #999);
    font-size: var(--text-sm, 13px);
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
    background: var(--surface-hover, rgba(255,255,255,0.06));
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
    color: var(--foreground, #ccc);
    margin-bottom: 2px;
    word-break: break-word;
  }

  .notif-message {
    font-size: var(--text-sm, 13px);
    color: var(--foreground-muted, #999);
    line-height: var(--leading-normal, 1.5);
    word-break: break-word;
  }

  .notif-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2, 4px);
    margin-top: 4px;
  }

  .notif-category {
    font-size: 10px;
    color: var(--foreground-muted, #999);
    border: 1px solid var(--border, #454545);
    border-radius: 999px;
    padding: 1px 6px;
    text-transform: uppercase;
    letter-spacing: 0.3px;
  }

  .notif-time {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted, #777);
  }

  .notif-remove {
    flex-shrink: 0;
    width: 18px;
    height: 18px;
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground-muted, #999);
    cursor: pointer;
    border-radius: var(--radius-sm, 4px);
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0;
    transition: all var(--transition-fast, 0.15s);
  }

  .notification-item:hover .notif-remove {
    opacity: 1;
  }

  .notif-remove:hover {
    background: var(--surface-hover, rgba(255,255,255,0.06));
    color: var(--foreground, #ccc);
  }
</style>
