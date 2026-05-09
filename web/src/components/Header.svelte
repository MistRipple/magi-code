<script lang="ts">
  import { addToast, getState, messagesState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import NotificationCenter from './NotificationCenter.svelte';
  import type { Session } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import { buildSessionMarkdownExport, downloadMarkdown } from '../lib/session-export';

  import type { Snippet } from 'svelte';
  interface Props {
    onOpenSettings?: () => void;
    children?: Snippet;
  }

  let { onOpenSettings, children }: Props = $props();
  const appState = getState();
  // 下拉菜单状态
  let dropdownOpen = $state(false);

  // P2-#12：局域网/隧道访问入口已收到 SettingsPanel 高级抽屉，主路径不再暴露。

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

  // 当前 workspace 的目录名（仅展示，便于在多 workspace 切换时确认上下文）
  const currentWorkspaceFolder = $derived.by(() => {
    const path = messagesState.currentWorkspacePath?.trim() ?? '';
    if (!path) return '';
    const cleaned = path.replace(/[\\/]+$/, '');
    const segments = cleaned.split(/[\\/]/).filter(Boolean);
    return segments.length > 0 ? segments[segments.length - 1] : cleaned;
  });

  // 当前会话是否为空（无消息），为空时禁止创建新会话
  const isCurrentSessionEmpty = $derived(
    ensureArray(appState.threadMessages).length === 0
  );
  const newSessionDisabled = $derived(isCurrentSessionEmpty || messagesState.sessionHydrating);
  const newSessionTitle = $derived(
    messagesState.sessionHydrating
      ? '正在创建新会话'
      : (isCurrentSessionEmpty ? i18n.t('header.currentSessionEmpty') : i18n.t('header.newSession'))
  );
  // 切换下拉菜单
  function toggleDropdown() {
    dropdownOpen = !dropdownOpen;
  }

  // 导出当前会话为 Markdown 文件
  function exportSession() {
    const payload = buildSessionMarkdownExport();
    if (!payload) {
      addToast('warning', i18n.t('header.exportEmpty'));
      return;
    }
    try {
      downloadMarkdown(payload);
      addToast('success', i18n.t('header.exportSuccess', { count: payload.messageCount }));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      addToast('error', i18n.t('header.exportFailed', { message }));
    }
  }
  const exportDisabled = $derived(
    !messagesState.currentSessionId || ensureArray(appState.threadMessages).length === 0,
  );

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
      addToast('info', `正在切换到会话“${pendingSwitchSessionName}”...`, undefined, {
        category: 'feedback',
        source: 'session-management',
        persistToCenter: false,
        countUnread: false,
        displayMode: 'toast',
        duration: 1800,
      });
      vscode.postMessage({
        type: 'switchSession',
        sessionId: pendingSwitchSessionId,
        workspaceId: messagesState.currentWorkspaceId || undefined,
        workspacePath: messagesState.currentWorkspacePath || undefined,
      });
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
    addToast('info', '正在创建新会话...', undefined, {
      category: 'feedback',
      source: 'session-management',
      persistToCenter: false,
      countUnread: false,
      displayMode: 'notification_center',
      duration: 1800,
    });
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
      addToast('info', `正在删除会话“${pendingDeleteSessionName}”...`, undefined, {
        category: 'feedback',
        source: 'session-management',
        persistToCenter: false,
        countUnread: false,
        displayMode: 'toast',
        duration: 1800,
      });
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
  {#if currentWorkspaceFolder}
    <div
      class="workspace-breadcrumb"
      title={messagesState.currentWorkspacePath || currentWorkspaceFolder}
      aria-label={i18n.t('header.workspaceBreadcrumbTitle', { path: messagesState.currentWorkspacePath || currentWorkspaceFolder })}
    >
      <Icon name="folder" size={12} />
      <span class="workspace-breadcrumb-name">{currentWorkspaceFolder}</span>
      <span class="workspace-breadcrumb-sep">/</span>
    </div>
  {/if}
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
          <button class="btn-icon btn-icon--sm" onclick={newSession} title={newSessionTitle} disabled={newSessionDisabled}>
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

  <div class="header-center">
    {@render children?.()}
  </div>

  <!-- 右侧操作按钮 -->
  <div class="header-actions">
    <button class="btn-icon btn-icon--sm" onclick={newSession} title={newSessionTitle} disabled={newSessionDisabled}>
      <Icon name="plus" size={14} />
    </button>
    <button
      class="btn-icon btn-icon--sm"
      onclick={exportSession}
      disabled={exportDisabled}
      title={i18n.t('header.exportSession')}
      aria-label={i18n.t('header.exportSession')}
    >
      <Icon name="download" size={14} />
    </button>
    <NotificationCenter />
    <button class="btn-icon btn-icon--sm" onclick={openSettings} title={i18n.t('header.settings')}>
      <Icon name="settings" size={14} />
    </button>
  </div>
</header>

<!-- 删除确认对话框 -->
{#if showDeleteConfirm}
  <Modal
    title={i18n.t('header.deleteSessionTitle')}
    onClose={closeDeleteConfirm}
    size="sm"
    closeOnBackdrop={true}
  >
    <p>{i18n.t('header.deleteSessionConfirm', { name: pendingDeleteSessionName })}</p>

    {#snippet footer()}
      <button class="modal-btn secondary" onclick={closeDeleteConfirm}>{i18n.t('header.cancel')}</button>
      <button class="modal-btn danger" onclick={confirmDelete}>{i18n.t('header.confirmDelete')}</button>
    {/snippet}
  </Modal>
{/if}

<!-- 切换会话确认对话框 -->
{#if showSwitchConfirm}
  <Modal
    title={i18n.t('header.switchSessionTitle')}
    onClose={closeSwitchConfirm}
    size="sm"
    closeOnBackdrop={true}
  >
    <p>{i18n.t('header.switchSessionConfirm', { name: pendingSwitchSessionName })}</p>

    {#snippet footer()}
      <button class="modal-btn secondary" onclick={closeSwitchConfirm}>{i18n.t('header.cancel')}</button>
      <button class="modal-btn primary" onclick={confirmSwitch}>{i18n.t('header.confirmSwitch')}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .header-bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    height: 48px;
    padding: 0 var(--space-4);
    background: var(--glass-bg);
    backdrop-filter: blur(14px);
    -webkit-backdrop-filter: blur(14px);
    flex-shrink: 0;
    position: sticky;
    top: 0;
    z-index: var(--z-modal);
    border-bottom: 1px solid color-mix(in srgb, var(--border) 78%, transparent);
  }

  .header-center {
    display: flex;
    flex: 1;
    justify-content: center;
  }

  .workspace-breadcrumb {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 8px;
    margin-right: 4px;
    color: var(--foreground-muted);
    font-size: 11px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--surface-2) 50%, transparent);
    flex-shrink: 0;
    cursor: default;
  }
  .workspace-breadcrumb-name {
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .workspace-breadcrumb-sep {
    color: var(--foreground-muted);
    opacity: 0.55;
  }
  @media (max-width: 720px) {
    .workspace-breadcrumb-name { max-width: 80px; }
  }

  .session-selector {
    position: relative;
    flex-shrink: 0;
    display: flex;
    justify-content: flex-start;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-shrink: 0;
    justify-content: flex-end;
  }

  /* 移动端：Header 变两行 */
  @media (max-width: 768px) {
    .header-bar {
      flex-wrap: wrap;
      height: auto;
      padding: var(--space-2) var(--space-3);
      gap: var(--space-1);
    }

    /* 第一行：左侧会话 + 右侧按钮 */
    .session-selector {
      flex: 1;
      min-width: 0;
    }

    .header-actions {
      flex: 0;
    }

    /* 第二行：Tab 栏占满整行 */
    .header-center {
      flex-basis: 100%;
      order: 3;
      justify-content: flex-start;
      overflow-x: auto;
      -webkit-overflow-scrolling: touch;
      scrollbar-width: none;
    }

    .header-center::-webkit-scrollbar {
      display: none;
    }

    .session-selector-name {
      max-width: 120px;
    }
  }

  .session-selector-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px 10px;
    background: transparent;
    border: none;
    border-radius: var(--radius-md);
    color: color-mix(in srgb, var(--foreground) 92%, var(--foreground-muted));
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .session-selector-btn:hover {
    background: color-mix(in srgb, var(--surface-hover) 72%, transparent);
    color: var(--foreground);
  }

  .session-selector-btn:focus-visible {
    outline-offset: 0;
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
    margin-top: 6px;
    min-width: 240px;
    max-width: 320px;
    background: color-mix(in srgb, var(--background) 92%, var(--surface-1));
    backdrop-filter: blur(16px);
    -webkit-backdrop-filter: blur(16px);
    border: 1px solid var(--border);
    border-radius: calc(var(--radius-lg) + 2px);
    box-shadow: var(--shadow-lg);
    z-index: var(--z-popover);
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
    max-height: 280px;
    overflow-y: auto;
    padding: 6px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .session-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    width: 100%;
    padding: 10px 12px;
    text-align: left;
    font-size: var(--text-sm);
    color: var(--foreground);
    background: transparent;
    border: none;
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .session-item:hover {
    background: color-mix(in srgb, var(--surface-hover) 82%, transparent);
  }

  .session-item.active {
    background: color-mix(in srgb, var(--surface-selected) 82%, transparent);
    color: var(--foreground);
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

  .session-item.active .session-name {
    font-weight: var(--font-semibold);
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
    opacity: 0.42;
    transition: all var(--transition-fast);
    flex-shrink: 0;
  }

  .session-item:hover .delete-btn {
    opacity: 1;
  }

  .session-item.active .delete-btn {
    opacity: 0.78;
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

  /* 下拉菜单背景遮罩 - 点击外部区域关闭 */
  .dropdown-backdrop {
    position: fixed;
    inset: 0;
    z-index: calc(var(--z-popover) - 1);
    background: transparent;
  }

  /* 使用全局 .btn-icon 样式，这里只覆盖必要的 */
</style>
