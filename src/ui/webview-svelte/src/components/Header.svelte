<script lang="ts">
  import { getState, messagesState, setCurrentSessionId } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import NotificationCenter from './NotificationCenter.svelte';
  import LanAccessPanel from './LanAccessPanel.svelte';
  import type { Session } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    onOpenSettings?: () => void;
  }

  let { onOpenSettings }: Props = $props();
  const appState = getState();

  // 下拉菜单状态
  let dropdownOpen = $state(false);

  // 局域网访问面板状态
  let showLanPanel = $state(false);

  // 删除确认对话框状态
  let showDeleteConfirm = $state(false);
  let pendingDeleteSessionId = $state<string | null>(null);
  let pendingDeleteSessionName = $state('');

  // 切换会话确认对话框状态
  let showSwitchConfirm = $state(false);
  let pendingSwitchSessionId = $state<string | null>(null);
  let pendingSwitchSessionName = $state('');

  // 🔧 修复响应式：直接使用 messagesState 对象属性
  // 获取当前会话名称
  const currentSessionName = $derived.by(() => {
    if (!messagesState.currentSessionId) return i18n.t('header.defaultSessionName');
    const session = (ensureArray(messagesState.sessions) as Session[]).find(s => s.id === messagesState.currentSessionId);
    return session?.name || i18n.t('header.sessionFallbackName');
  });

  // 🔧 修复响应式：会话列表
  const sessions = $derived(ensureArray(messagesState.sessions) as Session[]);

  // 当前会话是否为空（无消息），为空时禁止创建新会话
  const isCurrentSessionEmpty = $derived(
    ensureArray(appState.threadMessages).length === 0
  );

  // 切换下拉菜单
  function toggleDropdown() {
    dropdownOpen = !dropdownOpen;
  }

  // 点击会话项 - 如果是当前会话则忽略，否则弹出确认
  function handleSessionClick(sessionId: string, sessionName: string) {
    // 如果点击的就是当前会话，直接关闭下拉菜单，不做任何操作
    if (sessionId === messagesState.currentSessionId) {
      dropdownOpen = false;
      return;
    }
    // 弹出切换确认对话框
    pendingSwitchSessionId = sessionId;
    pendingSwitchSessionName = sessionName || i18n.t('header.unnamedSession');
    showSwitchConfirm = true;
  }

  // 确认切换会话
  function confirmSwitch() {
    if (pendingSwitchSessionId) {
      setCurrentSessionId(pendingSwitchSessionId);
      vscode.postMessage({ type: 'switchSession', sessionId: pendingSwitchSessionId });
    }
    closeSwitchConfirm();
    dropdownOpen = false;
  }

  // 取消切换
  function closeSwitchConfirm() {
    showSwitchConfirm = false;
    pendingSwitchSessionId = null;
    pendingSwitchSessionName = '';
  }

  // 新建会话
  function newSession() {
    vscode.postMessage({ type: 'newSession' });
    dropdownOpen = false;
  }

  // 打开设置
  function openSettings() {
    onOpenSettings?.();
  }

  // 点击删除按钮 - 显示插件内置确认弹窗
  function handleDeleteClick(sessionId: string, sessionName: string, event: MouseEvent) {
    event.stopPropagation();
    pendingDeleteSessionId = sessionId;
    pendingDeleteSessionName = sessionName || i18n.t('header.unnamedSession');
    showDeleteConfirm = true;
  }

  // 确认删除
  function confirmDelete() {
    if (pendingDeleteSessionId) {
      // 直接删除，无需后端再确认
      vscode.postMessage({ type: 'deleteSession', sessionId: pendingDeleteSessionId, requireConfirm: false });
    }
    closeDeleteConfirm();
  }

  // 取消删除
  function closeDeleteConfirm() {
    showDeleteConfirm = false;
    pendingDeleteSessionId = null;
    pendingDeleteSessionName = '';
  }

  // 格式化日期
  function formatDate(date: string | number | Date): string {
    const d = new Date(date);
    return d.toLocaleDateString(i18n.locale, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }

</script>

<header class="header-bar">
  <!-- 会话选择器 -->
  <div class="session-selector">
    <button class="session-selector-btn" onclick={toggleDropdown}>
      <Icon name="chat" size={14} class="session-selector-icon" />
      <span class="session-selector-name">{currentSessionName}</span>
      <Icon name="chevronDown" size={12} class="session-selector-chevron" />
    </button>

    {#if dropdownOpen}
      <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
      <!-- 点击外部区域关闭下拉菜单的遮罩层 -->
      <div class="dropdown-backdrop" onclick={() => dropdownOpen = false} role="presentation"></div>
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="session-dropdown">
        <div class="session-dropdown-header">
          <span class="session-dropdown-title">{i18n.t('header.sessionHistory')}</span>
          <button class="btn-icon btn-icon--sm" onclick={newSession} title={isCurrentSessionEmpty ? i18n.t('header.currentSessionEmpty') : i18n.t('header.newSession')} disabled={isCurrentSessionEmpty}>
            <Icon name="plus" size={14} />
          </button>
        </div>
        <div class="session-list">
          {#if sessions.length === 0}
            <div class="session-dropdown-empty">
              <Icon name="chat" size={24} />
              <span>{i18n.t('header.noSessionHistory')}</span>
            </div>
          {:else}
            {#each sessions as session (session.id)}
              <div
                class="session-item"
                class:active={session.id === messagesState.currentSessionId}
                role="button"
                tabindex="0"
                onclick={() => handleSessionClick(session.id, session.name || '')}
                onkeydown={(e) => e.key === 'Enter' && handleSessionClick(session.id, session.name || '')}
              >
                <div class="session-info">
                  <span class="session-name">{session.name || i18n.t('header.unnamedSession')}</span>
                  <div class="session-meta">
                    <span class="session-count">{i18n.t('header.messageCount', { count: session.messageCount ?? 0 })}</span>
                    <span class="session-date">{formatDate(session.updatedAt || session.createdAt)}</span>
                  </div>
                </div>
                <button
                  class="delete-btn"
                  onclick={(e) => handleDeleteClick(session.id, session.name || '', e)}
                  title={i18n.t('header.deleteSession')}
                >
                  <Icon name="delete" size={14} />
                </button>
              </div>
            {/each}
          {/if}
        </div>
      </div>
    {/if}
  </div>

  <!-- 右侧操作按钮 -->
  <div class="header-actions">
    <button class="btn-icon btn-icon--sm" onclick={newSession} title={isCurrentSessionEmpty ? i18n.t('header.currentSessionEmpty') : i18n.t('header.newSession')} disabled={isCurrentSessionEmpty}>
      <Icon name="plus" size={14} />
    </button>
    <div class="lan-access-wrapper">
      <button
        class="btn-icon btn-icon--sm lan-access-trigger"
        class:active={showLanPanel}
        onclick={() => { showLanPanel = !showLanPanel; }}
        title={i18n.t('lanAccess.title')}
      >
        <Icon name="qrcode" size={14} />
      </button>
      <LanAccessPanel visible={showLanPanel} onClose={() => { showLanPanel = false; }} />
    </div>
    <NotificationCenter />
    <button class="btn-icon btn-icon--sm" onclick={openSettings} title={i18n.t('header.settings')}>
      <Icon name="settings" size={14} />
    </button>
  </div>
</header>

<!-- 删除确认对话框 -->
{#if showDeleteConfirm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay" role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3 class="modal-title">{i18n.t('header.deleteSessionTitle')}</h3>
        <button class="modal-close" onclick={closeDeleteConfirm}>×</button>
      </div>
      <div class="modal-body">
        <p>{i18n.t('header.deleteSessionConfirm', { name: pendingDeleteSessionName })}</p>
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" onclick={closeDeleteConfirm}>{i18n.t('header.cancel')}</button>
        <button class="modal-btn danger" onclick={confirmDelete}>{i18n.t('header.confirmDelete')}</button>
      </div>
    </div>
  </div>
{/if}

<!-- 切换会话确认对话框 -->
{#if showSwitchConfirm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay" role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3 class="modal-title">{i18n.t('header.switchSessionTitle')}</h3>
        <button class="modal-close" onclick={closeSwitchConfirm}>×</button>
      </div>
      <div class="modal-body">
        <p>{i18n.t('header.switchSessionConfirm', { name: pendingSwitchSessionName })}</p>
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" onclick={closeSwitchConfirm}>{i18n.t('header.cancel')}</button>
        <button class="modal-btn primary" onclick={confirmSwitch}>{i18n.t('header.confirmSwitch')}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .header-bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    height: 40px;
    padding: 0 var(--space-4);
    border-bottom: 1px solid var(--border);
    background: var(--background);
    flex-shrink: 0;
  }

  .session-selector {
    position: relative;
  }

  .session-selector-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    color: var(--foreground);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .session-selector-btn:hover {
    background: var(--surface-hover);
    border-color: var(--border);
  }

  .session-selector-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    max-width: 180px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .session-dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    margin-top: var(--space-2);
    min-width: 220px;
    max-width: 320px;
    background: var(--dropdown-bg);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-lg);
    z-index: var(--z-dropdown);
    overflow: hidden;
  }

  .session-dropdown-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
  }

  .session-dropdown-title {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .session-list {
    max-height: 200px;
    overflow-y: auto;
    padding: var(--space-2) 0;
  }

  .session-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    width: 100%;
    padding: var(--space-3) var(--space-4);
    text-align: left;
    font-size: var(--text-sm);
    color: var(--foreground);
    background: transparent;
    border: none;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .session-item:hover {
    background: var(--surface-hover);
  }

  .session-item.active {
    background: var(--surface-selected);
    color: var(--primary);
  }

  .session-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }

  .session-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .session-meta {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .session-count {
    color: var(--foreground-muted);
  }

  .session-date {
    color: var(--foreground-muted);
  }

  .delete-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    opacity: 0.5;
    transition: all var(--transition-fast);
    flex-shrink: 0;
  }

  .session-item:hover .delete-btn {
    opacity: 1;
  }

  .delete-btn:hover {
    background: var(--error-muted);
    color: var(--error);
  }

  .session-dropdown-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-5);
    color: var(--foreground-muted);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .lan-access-wrapper {
    position: relative;
  }

  .lan-access-trigger.active {
    color: var(--accent, var(--vscode-textLink-foreground, #3794ff));
  }

  /* 下拉菜单背景遮罩 - 点击外部区域关闭 */
  .dropdown-backdrop {
    position: fixed;
    inset: 0;
    z-index: calc(var(--z-dropdown) - 1);
    background: transparent;
  }

  /* 使用全局 .btn-icon 样式，这里只覆盖必要的 */
</style>
