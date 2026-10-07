<script lang="ts">
  import type {
    IsolationConflictResolutionDto,
    IsolationMergeEntryDto,
  } from '../shared/rust-backend-types';
  import { addToast } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    applyIsolationMerge,
    closeIsolationMergeDialog,
    isolationMergeDialogState,
    loadIsolationMergePlan,
  } from '../stores/session-isolation-store.svelte';
  import {
    buildMergeRequest,
    conflictLabelKey,
    summarizeMergePlan,
  } from '../lib/isolation-merge';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let loading = $state(true);
  let applying = $state(false);
  let loadError = $state('');
  let entries = $state<IsolationMergeEntryDto[]>([]);
  let excluded = $state<Set<string>>(new Set());
  let resolutions = $state<Record<string, IsolationConflictResolutionDto>>({});
  let loadedFor = '';

  const summary = $derived(summarizeMergePlan(entries));
  const request = $derived(buildMergeRequest(entries, { excluded, resolutions }));
  const mergeableCount = $derived(request.paths.length);

  async function load(sessionId: string): Promise<void> {
    loading = true;
    loadError = '';
    try {
      const plan = await loadIsolationMergePlan(sessionId, isolationMergeDialogState.binding);
      if (isolationMergeDialogState.sessionId !== sessionId) return;
      entries = plan.entries;
      excluded = new Set();
      resolutions = {};
    } catch (error) {
      loadError = error instanceof Error && error.message ? error.message : i18n.t('isolation.merge.loadFailed');
      entries = [];
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (!isolationMergeDialogState.open) {
      loadedFor = '';
      return;
    }
    const sessionId = isolationMergeDialogState.sessionId;
    if (sessionId && loadedFor !== sessionId) {
      loadedFor = sessionId;
      void load(sessionId);
    }
  });

  function toggleIncluded(path: string): void {
    const next = new Set(excluded);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    excluded = next;
  }

  function resolve(path: string, resolution: IsolationConflictResolutionDto): void {
    resolutions = { ...resolutions, [path]: resolution };
  }

  async function apply(): Promise<void> {
    if (applying) return;
    applying = true;
    try {
      const outcome = await applyIsolationMerge(
        isolationMergeDialogState.sessionId,
        isolationMergeDialogState.binding,
        { paths: request.paths, resolutions: request.resolutions },
      );
      const applied = outcome.applied.length;
      const left = outcome.unresolved.length + outcome.failed.length;
      if (outcome.failed.length > 0) {
        addToast(
          'error',
          i18n.t('isolation.merge.partialFailed', {
            count: outcome.failed.length,
            detail: outcome.failed[0].error,
          }),
        );
      } else {
        addToast(
          'success',
          left > 0
            ? i18n.t('isolation.merge.doneWithLeft', { applied, left })
            : i18n.t('isolation.merge.done', { applied }),
        );
      }
      if (left === 0) {
        closeIsolationMergeDialog();
      } else {
        await load(isolationMergeDialogState.sessionId);
      }
    } catch (error) {
      addToast('error', error instanceof Error && error.message ? error.message : i18n.t('isolation.merge.failed'));
    } finally {
      applying = false;
    }
  }

  function actionIcon(action: IsolationMergeEntryDto['action']): 'file-plus' | 'file-edit' | 'file-minus' {
    if (action === 'add') return 'file-plus';
    if (action === 'delete') return 'file-minus';
    return 'file-edit';
  }
</script>

{#if isolationMergeDialogState.open}
  <Modal
    title={i18n.t('isolation.merge.title')}
    size="lg"
    onClose={closeIsolationMergeDialog}
    closeOnBackdrop={!applying}
    bodyClass="isolation-merge-body"
  >
    {#if loading}
      <div class="merge-status">{i18n.t('isolation.merge.loading')}</div>
    {:else if loadError}
      <div class="merge-status merge-status--error">{loadError}</div>
    {:else if entries.length === 0}
      <div class="merge-status">{i18n.t('isolation.merge.empty')}</div>
    {:else}
      <div class="merge-summary">
        {i18n.t('isolation.merge.summary', {
          clean: summary.clean,
          same: summary.alreadyApplied,
          conflict: summary.conflict,
        })}
      </div>
      <ul class="merge-list">
        {#each entries as entry (entry.path)}
          <li class="merge-row" class:conflict={entry.state === 'conflict'}>
            <div class="merge-row-main">
              {#if entry.state === 'conflict'}
                <span class="merge-mark merge-mark--conflict" aria-hidden="true">
                  <Icon name="alert-triangle" size={12} />
                </span>
              {:else}
                <input
                  type="checkbox"
                  class="merge-check"
                  checked={!excluded.has(entry.path)}
                  disabled={applying}
                  aria-label={entry.path}
                  onchange={() => toggleIncluded(entry.path)}
                />
              {/if}
              <span class="merge-action merge-action--{entry.action}" title={i18n.t(`isolation.merge.action.${entry.action}`)}>
                <Icon name={actionIcon(entry.action)} size={12} />
              </span>
              <span class="merge-path" title={entry.path}>{entry.path}</span>
              <span
                class="merge-state merge-state--{entry.state}"
              >
                {entry.state === 'clean'
                  ? i18n.t('isolation.merge.state.clean')
                  : entry.state === 'already_applied'
                    ? i18n.t('isolation.merge.state.alreadyApplied')
                    : i18n.t('isolation.merge.state.conflict')}
              </span>
            </div>
            {#if entry.state === 'conflict'}
              <div class="merge-conflict">
                <span class="merge-conflict-reason">{i18n.t(conflictLabelKey(entry.conflict))}</span>
                {#if entry.conflict !== 'unsupported'}
                  <div class="merge-resolutions" role="radiogroup" aria-label={i18n.t('isolation.merge.resolve.label')}>
                    <button
                      type="button"
                      class="merge-choice"
                      class:selected={resolutions[entry.path] === 'use_session'}
                      role="radio"
                      aria-checked={resolutions[entry.path] === 'use_session'}
                      disabled={applying}
                      onclick={() => resolve(entry.path, 'use_session')}
                    >{i18n.t('isolation.merge.resolve.useSession')}</button>
                    <button
                      type="button"
                      class="merge-choice"
                      class:selected={resolutions[entry.path] === 'keep_source'}
                      role="radio"
                      aria-checked={resolutions[entry.path] === 'keep_source'}
                      disabled={applying}
                      onclick={() => resolve(entry.path, 'keep_source')}
                    >{i18n.t('isolation.merge.resolve.keepSource')}</button>
                  </div>
                {/if}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    {#snippet footer()}
      <div class="merge-footer">
        <button type="button" class="merge-btn" disabled={applying} onclick={closeIsolationMergeDialog}>
          {i18n.t('isolation.merge.cancel')}
        </button>
        <button
          type="button"
          class="merge-btn merge-btn--primary"
          disabled={applying || loading || mergeableCount === 0}
          onclick={() => void apply()}
        >
          {applying ? i18n.t('isolation.merge.applying') : i18n.t('isolation.merge.apply')}
        </button>
      </div>
    {/snippet}
  </Modal>
{/if}

<style>
  .merge-status {
    padding: var(--space-6) var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    text-align: center;
  }

  .merge-status--error {
    color: var(--error);
  }

  .merge-summary {
    padding: 0 0 var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .merge-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: min(52vh, 420px);
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
  }

  .merge-row {
    padding: 6px var(--space-2);
    border-radius: var(--radius-md);
  }

  .merge-row:hover {
    background: color-mix(in srgb, var(--foreground) 4%, transparent);
  }

  .merge-row.conflict {
    background: color-mix(in srgb, var(--warning) 8%, transparent);
  }

  .merge-row-main {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .merge-check {
    flex: 0 0 auto;
    margin: 0;
    accent-color: var(--primary);
  }

  .merge-mark {
    display: inline-flex;
    flex: 0 0 13px;
    align-items: center;
    justify-content: center;
  }

  .merge-mark--conflict {
    color: var(--warning);
  }

  .merge-action {
    display: inline-flex;
    flex: 0 0 auto;
    color: var(--foreground-muted);
  }

  .merge-action--add {
    color: var(--success);
  }

  .merge-action--delete {
    color: var(--error);
  }

  .merge-path {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-family: var(--font-mono, monospace);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .merge-state {
    flex: 0 0 auto;
    padding: 0 8px;
    border-radius: 999px;
    font-size: 10px;
    line-height: 18px;
  }

  .merge-state--clean {
    background: color-mix(in srgb, var(--success) 14%, transparent);
    color: var(--success);
  }

  .merge-state--already_applied {
    background: color-mix(in srgb, var(--foreground) 8%, transparent);
    color: var(--foreground-muted);
  }

  .merge-state--conflict {
    background: color-mix(in srgb, var(--warning) 16%, transparent);
    color: var(--warning);
  }

  .merge-conflict {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    margin: 6px 0 0 calc(13px + var(--space-2));
  }

  .merge-conflict-reason {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .merge-resolutions {
    display: inline-flex;
    gap: 4px;
  }

  .merge-choice {
    height: 24px;
    padding: 0 10px;
    border: 1px solid var(--border-subtle);
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: background var(--transition-fast), border-color var(--transition-fast), color var(--transition-fast);
  }

  .merge-choice:hover:not(:disabled) {
    color: var(--foreground);
  }

  .merge-choice.selected {
    border-color: color-mix(in srgb, var(--primary) 45%, transparent);
    background: var(--primary-muted);
    color: var(--primary);
    font-weight: var(--font-semibold);
  }

  .merge-footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .merge-btn {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-sm);
  }

  .merge-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
    color: var(--foreground);
  }

  .merge-btn--primary {
    background: var(--primary);
    color: var(--primary-foreground, #fff);
    font-weight: var(--font-semibold);
  }

  .merge-btn--primary:hover:not(:disabled) {
    background: var(--primary-hover, var(--primary));
    color: var(--primary-foreground, #fff);
  }

  .merge-btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
</style>
