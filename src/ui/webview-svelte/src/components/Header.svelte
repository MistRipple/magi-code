<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import type { Session } from '../types/message';

  interface Props {
    onOpenSettings?: () => void;
  }

  let { onOpenSettings }: Props = $props();

  const appState = getState();

  // 下拉菜单状态
  let dropdownOpen = $state(false);

  // 获取当前会话名称
  const currentSessionName = $derived(() => {
    if (!appState.currentSessionId) return '新会话';
    const session = (ensureArray(appState.sessions) as Session[]).find(s => s.id === appState.currentSessionId);
    return session?.name || '会话';
  });

  // 切换下拉菜单
  function toggleDropdown() {
    dropdownOpen = !dropdownOpen;
  }

  // 选择会话
  function selectSession(sessionId: string) {
    vscode.postMessage({ type: 'switchSession', sessionId });
    dropdownOpen = false;
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

  // 删除会话
  function deleteSession(sessionId: string, event: MouseEvent) {
    event.stopPropagation();
    if (confirm('确定要删除这个会话吗？')) {
      vscode.postMessage({ type: 'closeSession', sessionId });
    }
  }

  // 格式化日期
  function formatDate(date: string | number | Date): string {
    const d = new Date(date);
    return d.toLocaleDateString('zh-CN', {
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
      <span class="session-selector-name">{currentSessionName()}</span>
      <Icon name="chevronDown" size={12} class="session-selector-chevron" />
    </button>

    {#if dropdownOpen}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="session-dropdown">
        <div class="session-dropdown-header">
          <span class="session-dropdown-title">会话历史</span>
          <button class="btn-icon btn-icon--sm" onclick={newSession} title="新建会话">
            <Icon name="plus" size={14} />
          </button>
        </div>
        <div class="session-list">
          {#if (ensureArray(appState.sessions) as Session[]).length === 0}
            <div class="session-dropdown-empty">
              <Icon name="chat" size={24} />
              <span>暂无会话历史</span>
            </div>
          {:else}
            {#each ensureArray(appState.sessions) as session (session.id)}
              {@const s = session as Session}
              <div
                class="session-item"
                class:active={s.id === appState.currentSessionId}
                role="button"
                tabindex="0"
                onclick={() => selectSession(s.id)}
                onkeydown={(e) => e.key === 'Enter' && selectSession(s.id)}
              >
                <div class="session-info">
                  <span class="session-name">{s.name || '未命名会话'}</span>
                  <div class="session-meta">
                    <span class="session-count">{s.messageCount ?? 0} 条消息</span>
                    <span class="session-date">{formatDate(s.updatedAt || s.createdAt)}</span>
                  </div>
                </div>
                <button
                  class="delete-btn"
                  onclick={(e) => deleteSession(s.id, e)}
                  title="删除会话"
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
    <button class="btn-icon btn-icon--sm" onclick={newSession} title="新建会话">
      <Icon name="plus" size={14} />
    </button>
    <button class="btn-icon btn-icon--sm" onclick={openSettings} title="设置">
      <Icon name="settings" size={14} />
    </button>
  </div>
</header>

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
    max-width: 150px;
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
    background: var(--vscode-dropdown-background, #3c3c3c);
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

  /* 使用全局 .btn-icon 样式，这里只覆盖必要的 */
</style>
