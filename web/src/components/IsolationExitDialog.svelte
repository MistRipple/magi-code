<script lang="ts">
  import type { IsolationMergeEntryDto } from '../shared/rust-backend-types';
  import { addToast } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    closeIsolationExitDialog,
    discardSessionIsolation,
    getSessionIsolation,
    isolationExitDialogState,
    loadIsolationMergePlan,
    openIsolationMergeDialog,
  } from '../stores/session-isolation-store.svelte';
  import { isolationOriginBlockingSession, summarizeMergePlan } from '../lib/isolation-merge';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  const PREVIEW_LIMIT = 6;

  let loading = $state(true);
  let working = $state(false);
  let loadError = $state('');
  let actionError = $state('');
  let entries = $state<IsolationMergeEntryDto[]>([]);
  let loadedFor = '';

  const summary = $derived(summarizeMergePlan(entries));
  /** 真正还没进主工作区的改动：已一致的文件丢弃也不会损失任何内容。 */
  const pending = $derived(entries.filter((entry) => entry.state !== 'already_applied'));
  const automatic = $derived(
    Boolean(
      isolationExitDialogState.sessionId
      && getSessionIsolation(isolationExitDialogState.sessionId)
      && isolationOriginBlockingSession(getSessionIsolation(isolationExitDialogState.sessionId)!.origin),
    ),
  );

  async function load(sessionId: string): Promise<void> {
    loading = true;
    loadError = '';
    actionError = '';
    try {
      const plan = await loadIsolationMergePlan(sessionId, isolationExitDialogState.binding);
      if (isolationExitDialogState.sessionId !== sessionId) return;
      entries = plan.entries;
    } catch (error) {
      loadError = error instanceof Error && error.message ? error.message : i18n.t('isolation.merge.loadFailed');
      entries = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (!isolationExitDialogState.open) {
      loadedFor = '';
      return;
    }
    const sessionId = isolationExitDialogState.sessionId;
    if (sessionId && loadedFor !== sessionId) {
      loadedFor = sessionId;
      void load(sessionId);
    }
  });

  async function exitIsolation(): Promise<void> {
    if (working) return;
    working = true;
    actionError = '';
    try {
      const result = await discardSessionIsolation(
        isolationExitDialogState.sessionId,
        isolationExitDialogState.binding,
        true,
      );
      if (result.ok) {
        closeIsolationExitDialog();
        addToast('success', i18n.t('isolation.exit.done'));
      } else {
        actionError = result.error || i18n.t('isolation.exit.failed');
      }
    } finally {
      working = false;
    }
  }

  function mergeThenExit(): void {
    const { sessionId, binding } = isolationExitDialogState;
    closeIsolationExitDialog();
    openIsolationMergeDialog(sessionId, binding, { exitAfterMerge: true });
  }
</script>

{#if isolationExitDialogState.open}
  <Modal
    title={i18n.t('isolation.exit.title')}
    size="md"
    onClose={closeIsolationExitDialog}
    closeOnBackdrop={!working}
  >
    {#if loading}
      <div class="exit-status">{i18n.t('isolation.exit.checking')}</div>
    {:else if loadError}
      <p class="exit-text exit-text--error">{i18n.t('isolation.exit.checkFailed', { error: loadError })}</p>
    {:else if pending.length === 0}
      <p class="exit-text">{i18n.t('isolation.exit.clean')}</p>
    {:else}
      <p class="exit-text">
        {i18n.t('isolation.exit.pending', {
          count: pending.length,
          clean: summary.clean,
          conflict: summary.conflict,
        })}
      </p>
      <ul class="exit-files">
        {#each pending.slice(0, PREVIEW_LIMIT) as entry (entry.path)}
          <li title={entry.path}>
            <Icon name={entry.state === 'conflict' ? 'alert-triangle' : 'file-edit'} size={12} />
            <span>{entry.path}</span>
          </li>
        {/each}
        {#if pending.length > PREVIEW_LIMIT}
          <li class="exit-files-more">{i18n.t('isolation.exit.more', { count: pending.length - PREVIEW_LIMIT })}</li>
        {/if}
      </ul>
    {/if}
    {#if automatic && !loading}
      <p class="exit-hint">{i18n.t('isolation.exit.autoHint')}</p>
    {/if}
    {#if actionError}
      <p class="exit-text exit-text--error">{actionError}</p>
    {/if}

    {#snippet footer()}
      <div class="exit-footer">
        <button type="button" class="exit-btn" disabled={working} onclick={closeIsolationExitDialog}>
          {i18n.t('isolation.exit.keep')}
        </button>
        {#if !loading}
          {#if loadError}
            <button type="button" class="exit-btn exit-btn--danger" disabled={working} onclick={() => void exitIsolation()}>
              {working ? i18n.t('isolation.exit.working') : i18n.t('isolation.exit.forceDiscard')}
            </button>
          {:else if pending.length === 0}
            <button type="button" class="exit-btn exit-btn--primary" disabled={working} onclick={() => void exitIsolation()}>
              {working ? i18n.t('isolation.exit.working') : i18n.t('isolation.exit.confirm')}
            </button>
          {:else}
            <button type="button" class="exit-btn exit-btn--danger" disabled={working} onclick={() => void exitIsolation()}>
              {working ? i18n.t('isolation.exit.working') : i18n.t('isolation.exit.discard')}
            </button>
            <button type="button" class="exit-btn exit-btn--primary" disabled={working} onclick={mergeThenExit}>
              {i18n.t('isolation.exit.merge')}
            </button>
          {/if}
        {/if}
      </div>
    {/snippet}
  </Modal>
{/if}

<style>
  .exit-status {
    padding: var(--space-6) var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    text-align: center;
  }

  .exit-text {
    margin: 0 0 var(--space-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: 1.6;
  }

  .exit-text--error {
    color: var(--error);
  }

  .exit-hint {
    margin: var(--space-2) 0 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .exit-files {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0 0 var(--space-2);
    padding: var(--space-2);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--foreground) 4%, transparent);
    list-style: none;
    font-size: var(--text-xs);
  }

  .exit-files li {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    color: var(--foreground-muted);
  }

  .exit-files li span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .exit-files-more {
    padding-left: 18px;
  }

  .exit-footer {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .exit-btn {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-sm);
    white-space: nowrap;
  }

  .exit-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
    color: var(--foreground);
  }

  .exit-btn--primary,
  .exit-btn--primary:hover:not(:disabled) {
    background: var(--primary);
    color: var(--primary-foreground, #fff);
    font-weight: var(--font-semibold);
  }

  .exit-btn--danger {
    color: var(--error);
  }

  .exit-btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
</style>
