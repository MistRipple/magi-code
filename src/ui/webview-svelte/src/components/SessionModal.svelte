<script lang="ts">
  import { getState, setCurrentSessionId } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';

  // Props
  interface Props {
    isOpen: boolean;
    onClose: () => void;
  }

  let { isOpen, onClose }: Props = $props();

  const appState = getState();

  // 会话排序（最近的在前）
  const sortedSessions = $derived(
    [...ensureArray(appState.sessions)].sort((a, b) =>
      new Date(b.updatedAt || b.createdAt).getTime() -
      new Date(a.updatedAt || a.createdAt).getTime()
    )
  );

  function selectSession(sessionId: string) {
    setCurrentSessionId(sessionId);
    vscode.postMessage({ type: 'switchSession', sessionId });
    onClose();
  }

  function createNewSession() {
    vscode.postMessage({ type: 'newSession' });
    onClose();
  }

  function deleteSession(sessionId: string, event: MouseEvent) {
    event.stopPropagation();
    if (confirm('确定要删除这个会话吗？')) {
      vscode.postMessage({ type: 'closeSession', sessionId });
    }
  }

  function formatDate(date: string | number | Date): string {
    const d = new Date(date);
    return d.toLocaleDateString('zh-CN', {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      onClose();
    }
  }
</script>

{#if isOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions a11y_interactive_supports_focus -->
  <div class="modal-backdrop" onclick={onClose} onkeydown={handleKeydown} role="dialog" tabindex="-1">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions a11y_no_noninteractive_element_interactions -->
    <div class="modal-content" onclick={(e) => e.stopPropagation()} role="document">
      <div class="modal-header">
        <h3 class="modal-title">会话管理</h3>
        <button class="close-btn" onclick={onClose} title="关闭">
          <Icon name="close" size={16} />
        </button>
      </div>

      <div class="modal-body">
        <button class="new-session-btn" onclick={createNewSession}>
          <Icon name="plus" size={14} />
          新建会话
        </button>

        <div class="session-list">
          {#each sortedSessions as session (session.id)}
            <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
            <div
              class="session-item"
              class:active={session.id === appState.currentSessionId}
              onclick={() => selectSession(session.id)}
              role="button"
              tabindex="0"
            >
              <div class="session-info">
                <span class="session-name">{session.name || '未命名会话'}</span>
                <span class="session-date">{formatDate(session.updatedAt || session.createdAt)}</span>
              </div>
              <button
                class="delete-btn"
                onclick={(e) => deleteSession(session.id, e)}
                title="删除会话"
              >
                <Icon name="delete" size={12} />
              </button>
            </div>
          {/each}
        </div>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: var(--overlay);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

  @keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }

  .modal-content {
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    width: 90%;
    max-width: 400px;
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: var(--shadow-xl);
    animation: slideUp var(--duration-normal) var(--ease-out);
  }

  @keyframes slideUp { from { opacity: 0; transform: translateY(16px); } to { opacity: 1; transform: translateY(0); } }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4);
    border-bottom: 1px solid var(--border);
  }

  .modal-title {
    font-size: var(--text-md);
    font-weight: var(--font-semibold);
    margin: 0;
    color: var(--foreground);
  }

  .close-btn {
    width: var(--btn-height-md);
    height: var(--btn-height-md);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .close-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .modal-body {
    padding: var(--space-4);
    overflow-y: auto;
  }

  .new-session-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    width: 100%;
    height: var(--btn-height-lg);
    padding: 0 var(--space-4);
    margin-bottom: var(--space-4);
    background: var(--primary);
    color: white;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    border: none;
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .new-session-btn:hover {
    background: var(--primary-hover);
  }

  .session-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .session-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .session-item:hover {
    background: var(--surface-hover);
  }

  .session-item.active {
    border-color: var(--primary);
    background: var(--surface-selected);
  }

  .session-info {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .session-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .session-date {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .delete-btn {
    width: var(--btn-height-xs);
    height: var(--btn-height-xs);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    opacity: 0;
    transition: all var(--transition-fast);
  }

  .session-item:hover .delete-btn {
    opacity: 1;
  }

  .delete-btn:hover {
    background: var(--error-muted);
    color: var(--error);
  }
</style>
