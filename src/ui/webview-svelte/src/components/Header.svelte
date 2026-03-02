<script lang="ts">
  import { messagesState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import type { Session } from '../types/message';

  interface Props {
    onOpenSettings?: () => void;
  }

  let { onOpenSettings }: Props = $props();

  // 下拉菜单状态
  let dropdownOpen = $state(false);

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
    if (!messagesState.currentSessionId) return '新会话';
    const session = (ensureArray(messagesState.sessions) as Session[]).find(s => s.id === messagesState.currentSessionId);
    return session?.name || '会话';
  });

  // 🔧 修复响应式：会话列表
  const sessions = $derived(ensureArray(messagesState.sessions) as Session[]);

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
    pendingSwitchSessionName = sessionName || '未命名会话';
    showSwitchConfirm = true;
  }

  // 确认切换会话
  function confirmSwitch() {
    if (pendingSwitchSessionId) {
      const currentMessages = collectCurrentSessionSnapshot();
      vscode.postMessage({ type: 'switchSession', sessionId: pendingSwitchSessionId, currentMessages });
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
    const currentMessages = collectCurrentSessionSnapshot();
    vscode.postMessage({ type: 'newSession', currentMessages });
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
    pendingDeleteSessionName = sessionName || '未命名会话';
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
    return d.toLocaleDateString('zh-CN', {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  type PersistableMessage = Record<string, unknown> & {
    id?: string;
    timestamp?: number;
  };

  function collectCurrentSessionSnapshot(): PersistableMessage[] {
    if (!messagesState.currentSessionId) return [];

    const merged: PersistableMessage[] = [];
    const seen = new Set<string>();

    for (const message of ensureArray<PersistableMessage>(messagesState.threadMessages as unknown as PersistableMessage[])) {
      const id = typeof message?.id === 'string' ? message.id.trim() : '';
      if (!id || seen.has(id)) continue;
      seen.add(id);
      merged.push(message);
    }

    const workers = ['claude', 'codex', 'gemini'] as const;
    for (const worker of workers) {
      for (const message of ensureArray<PersistableMessage>(messagesState.agentOutputs[worker] as unknown as PersistableMessage[])) {
        const id = typeof message?.id === 'string' ? message.id.trim() : '';
        if (!id || seen.has(id)) continue;
        seen.add(id);
        merged.push({
          ...message,
          agent: worker,
          source: worker,
        });
      }
    }

    merged.sort((a, b) => {
      const ta = typeof a?.timestamp === 'number' ? a.timestamp : 0;
      const tb = typeof b?.timestamp === 'number' ? b.timestamp : 0;
      return ta - tb;
    });

    return merged;
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
          <span class="session-dropdown-title">会话历史</span>
          <button class="btn-icon btn-icon--sm" onclick={newSession} title="新建会话">
            <Icon name="plus" size={14} />
          </button>
        </div>
        <div class="session-list">
          {#if sessions.length === 0}
            <div class="session-dropdown-empty">
              <Icon name="chat" size={24} />
              <span>暂无会话历史</span>
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
                  <span class="session-name">{session.name || '未命名会话'}</span>
                  <div class="session-meta">
                    <span class="session-count">{session.messageCount ?? 0} 条消息</span>
                    <span class="session-date">{formatDate(session.updatedAt || session.createdAt)}</span>
                  </div>
                </div>
                <button
                  class="delete-btn"
                  onclick={(e) => handleDeleteClick(session.id, session.name || '', e)}
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

<!-- 删除确认对话框 -->
{#if showDeleteConfirm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay" role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3 class="modal-title">删除会话</h3>
        <button class="modal-close" onclick={closeDeleteConfirm}>×</button>
      </div>
      <div class="modal-body">
        <p>确定要删除会话 "<strong>{pendingDeleteSessionName}</strong>" 吗？此操作不可撤销。</p>
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" onclick={closeDeleteConfirm}>取消</button>
        <button class="modal-btn danger" onclick={confirmDelete}>确定删除</button>
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
        <h3 class="modal-title">切换会话</h3>
        <button class="modal-close" onclick={closeSwitchConfirm}>×</button>
      </div>
      <div class="modal-body">
        <p>确定要切换到会话 "<strong>{pendingSwitchSessionName}</strong>" 吗？</p>
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" onclick={closeSwitchConfirm}>取消</button>
        <button class="modal-btn primary" onclick={confirmSwitch}>确定切换</button>
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

  /* 下拉菜单背景遮罩 - 点击外部区域关闭 */
  .dropdown-backdrop {
    position: fixed;
    inset: 0;
    z-index: calc(var(--z-dropdown) - 1);
    background: transparent;
  }

  /* 使用全局 .btn-icon 样式，这里只覆盖必要的 */
</style>
