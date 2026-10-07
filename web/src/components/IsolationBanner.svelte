<script lang="ts">
  import { messagesState, addToast } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    discardSessionIsolation,
    getSessionIsolation,
    openIsolationMergeDialog,
  } from '../stores/session-isolation-store.svelte';
  import { isolationOriginBlockingSession } from '../lib/isolation-merge';
  import { resolveCurrentSessionTitle } from '../lib/session-title';
  import Icon from './Icon.svelte';

  const sessionId = $derived(messagesState.currentSessionId?.trim() || '');
  const isolation = $derived(sessionId ? getSessionIsolation(sessionId) : null);
  const binding = $derived({
    workspaceId: messagesState.currentWorkspaceId?.trim() || '',
    workspacePath: messagesState.currentWorkspacePath?.trim() || '',
  });
  const blockingSessionId = $derived(isolation ? isolationOriginBlockingSession(isolation.origin) : null);
  const blockingSessionTitle = $derived(
    blockingSessionId
      ? resolveCurrentSessionTitle({
          sessionId: blockingSessionId,
          workspaceId: messagesState.currentWorkspaceId,
          workspacePath: messagesState.currentWorkspacePath,
          workspaceSessions: messagesState.workspaceSessionProjection,
          workspaceSessionProjections: messagesState.workspaceSessionProjections,
          personalSessions: messagesState.personalSessionProjection.sessions,
        })
      : '',
  );
  const description = $derived.by(() => {
    if (!blockingSessionId) return i18n.t('isolation.banner.desc');
    return blockingSessionTitle
      ? i18n.t('isolation.banner.descAuto', { session: blockingSessionTitle })
      : i18n.t('isolation.banner.descAutoUnknown');
  });
  let busy = $state(false);

  async function discard(): Promise<void> {
    if (busy || !sessionId) return;
    if (!window.confirm(i18n.t('isolation.discard.confirm'))) return;
    busy = true;
    try {
      const result = await discardSessionIsolation(sessionId, binding, true);
      addToast(
        result.ok ? 'success' : 'error',
        result.ok ? i18n.t('isolation.discard.done') : result.error || i18n.t('isolation.discard.failed'),
      );
    } finally {
      busy = false;
    }
  }
</script>

{#if isolation}
  <div class="isolation-banner" role="status">
    <span class="isolation-banner-icon" aria-hidden="true"><Icon name="layers" size={14} /></span>
    <div class="isolation-banner-text">
      <div class="isolation-banner-title">{i18n.t('isolation.banner.title')}</div>
      <div class="isolation-banner-desc">{description}</div>
    </div>
    <div class="isolation-banner-actions">
      <button
        type="button"
        class="isolation-banner-btn primary"
        disabled={busy}
        onclick={() => openIsolationMergeDialog(sessionId, binding)}
      >
        <Icon name="git-merge" size={12} />
        <span>{i18n.t('isolation.banner.merge')}</span>
      </button>
      <button type="button" class="isolation-banner-btn" disabled={busy} onclick={() => void discard()}>
        <span>{i18n.t('isolation.banner.discard')}</span>
      </button>
    </div>
  </div>
{/if}

<style>
  .isolation-banner {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-3);
    margin: var(--space-1) var(--space-1) var(--space-2);
    padding: var(--space-2) var(--space-3);
    border: 1px solid color-mix(in srgb, var(--primary) 28%, var(--border-subtle));
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--primary) 8%, transparent);
  }

  .isolation-banner-icon {
    display: inline-flex;
    flex: 0 0 24px;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    color: var(--primary);
  }

  .isolation-banner-text {
    flex: 1 1 auto;
    min-width: 0;
  }

  .isolation-banner-title {
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .isolation-banner-desc {
    margin-top: 1px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.45;
  }

  .isolation-banner-actions {
    display: flex;
    flex: 0 0 auto;
    flex-direction: column;
    gap: 4px;
  }

  .isolation-banner-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    white-space: nowrap;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .isolation-banner-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
    color: var(--foreground);
  }

  .isolation-banner-btn.primary {
    background: var(--primary);
    color: var(--primary-foreground, #fff);
    font-weight: var(--font-semibold);
  }

  .isolation-banner-btn.primary:hover:not(:disabled) {
    background: var(--primary-hover, var(--primary));
    color: var(--primary-foreground, #fff);
  }

  .isolation-banner-btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
</style>
