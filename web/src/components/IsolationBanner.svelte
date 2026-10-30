<script lang="ts">
  import { messagesState } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    getSessionIsolation,
    openIsolationExitDialog,
    openIsolationMergeDialog,
  } from '../stores/session-isolation-store.svelte';
  import { isolationOriginBlockingSession } from '../lib/isolation-merge';
  import { resolveCurrentSessionTitle } from '../lib/session-title';
  import Icon from './Icon.svelte';
  import { selectedComposerSourceFolderId } from '../stores/composer-workspace.svelte';

  const sessionId = $derived(messagesState.currentSessionId?.trim() || '');
  const isolation = $derived(sessionId ? getSessionIsolation(sessionId) : null);
  const binding = $derived({
    workspaceId: messagesState.currentWorkspaceId?.trim() || '',
    sourceFolderId: selectedComposerSourceFolderId(messagesState.currentWorkspaceId || '', sessionId),
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
        class="btn btn--primary btn--sm"
        onclick={() => openIsolationMergeDialog(sessionId, binding)}
      >
        <Icon name="git-merge" size={12} />
        <span>{i18n.t('isolation.banner.merge')}</span>
      </button>
      <button type="button" class="btn btn--secondary btn--sm" onclick={() => openIsolationExitDialog(sessionId, binding)}>
        <span>{i18n.t('isolation.banner.exit')}</span>
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
    border: var(--magi-hairline) solid color-mix(in srgb, var(--primary) 28%, var(--border-subtle));
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

</style>
