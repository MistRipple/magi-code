<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    deleteSafeguardAudit,
    getSafeguardAuditPage,
    type SafeguardAuditEntry,
  } from '../web/agent-api';

  let { onClose, onCountChange } = $props<{
    onClose: () => void;
    /** 删除后把最新总数交回设置页，页面上的「已记录 N 条」随之更新。 */
    onCountChange: (count: number) => void;
  }>();

  let entries = $state<SafeguardAuditEntry[]>([]);
  let total = $state(0);
  let nextBefore = $state<number | null>(null);
  let loading = $state(true);
  let loadError = $state('');

  async function load(before?: number | null): Promise<void> {
    loading = true;
    loadError = '';
    try {
      const page = await getSafeguardAuditPage(before);
      entries = before == null ? page.entries : [...entries, ...page.entries];
      total = page.total;
      nextBefore = page.nextBefore;
    } catch (error) {
      loadError = error instanceof Error ? error.message : String(error);
    } finally {
      loading = false;
    }
  }

  onMount(() => { void load(); });

  let deleting = $state(false);

  async function remove(selection: { eventIds: string[] } | { all: true }): Promise<void> {
    deleting = true;
    try {
      const result = await deleteSafeguardAudit(selection);
      total = result.total;
      onCountChange(result.total);
      if ('all' in selection) {
        entries = [];
        nextBefore = null;
      } else {
        const removed = new Set(selection.eventIds);
        entries = entries.filter((entry) => !removed.has(entry.eventId));
      }
    } catch (error) {
      addToast('error', error instanceof Error ? error.message : String(error));
    } finally {
      deleting = false;
    }
  }

  async function clearAll(): Promise<void> {
    if (!(await confirmMessage(i18n.t('settings.safeguard.audit.clearConfirm', { count: total }), { tone: 'danger' }))) {
      return;
    }
    await remove({ all: true });
  }

  function decisionLabel(decision: string | null): string {
    switch (decision) {
      case 'hard_block':
        return i18n.t('settings.safeguard.action.hardBlock');
      case 'audit_only':
        return i18n.t('settings.safeguard.action.auditOnly');
      case 'require_approval_in_restricted':
        return i18n.t('settings.safeguard.action.requireApproval');
      default:
        return decision ?? '-';
    }
  }

  function formatTime(millis: number): string {
    return new Date(millis).toLocaleString(i18n.locale, { hour12: false });
  }
</script>

<Modal title={i18n.t('settings.safeguard.audit.title')} size="lg" {onClose} closeOnBackdrop>
  <p class="audit-note">{i18n.t('settings.safeguard.audit.note')}</p>

  {#if loadError}
    <p class="audit-error">{i18n.t('settings.safeguard.audit.loadFailed', { error: loadError })}</p>
  {/if}

  {#if entries.length === 0 && !loading && !loadError}
    <p class="audit-empty">{i18n.t('settings.safeguard.audit.empty')}</p>
  {:else}
    <ul class="audit-list">
      {#each entries as entry (entry.eventId)}
        <li class="audit-item">
          <div class="audit-item-head">
            <span class="audit-decision" data-decision={entry.decision}>{decisionLabel(entry.decision)}</span>
            <span class="audit-tool">{entry.toolName ?? '-'}</span>
            <time class="audit-time">{formatTime(entry.occurredAt)}</time>
            <button
              type="button"
              class="btn-icon btn-icon--sm btn-icon--danger"
              title={i18n.t('settings.safeguard.audit.delete')}
              aria-label={i18n.t('settings.safeguard.audit.delete')}
              disabled={deleting}
              onclick={() => void remove({ eventIds: [entry.eventId] })}
            ><Icon name="delete" size={12} /></button>
          </div>
          <div class="audit-rules">
            {#each entry.matchedRules as rule}
              <code title={i18n.t(`settings.safeguard.category.${rule.category}`)}>{rule.pattern}</code>
            {/each}
          </div>
          {#if entry.sessionId}
            <div class="audit-session">
              {i18n.t('settings.safeguard.audit.session')}：{entry.sessionTitle || i18n.t('settings.safeguard.audit.sessionGone')}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if loading}
    <div class="audit-status">{i18n.t('common.loading')}</div>
  {/if}

  <div class="audit-footer">
    <span class="audit-count">{i18n.t('settings.safeguard.audit.shown', { shown: entries.length, total })}</span>
    <span class="audit-footer-actions">
      {#if nextBefore !== null && !loading}
        <button type="button" class="btn btn--sm" onclick={() => void load(nextBefore)}>
          {i18n.t('settings.safeguard.audit.loadMore')}
        </button>
      {/if}
      {#if total > 0}
        <button type="button" class="btn btn--sm btn--danger" disabled={deleting} onclick={() => void clearAll()}>
          {i18n.t('settings.safeguard.audit.clearAll')}
        </button>
      {/if}
    </span>
  </div>
</Modal>

<style>
  .audit-note { margin: 0 0 var(--space-3); color: var(--foreground-muted); font-size: var(--text-xs); line-height: 1.5; }
  .audit-error { color: var(--danger); font-size: var(--text-sm); }
  .audit-empty, .audit-status { padding: var(--space-6) 0; text-align: center; color: var(--foreground-muted); font-size: var(--text-sm); }
  .audit-list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--space-2); max-height: min(55vh, 460px); overflow-y: auto; }
  .audit-item { padding: var(--space-2) var(--space-3); border: 1px solid var(--border); border-radius: var(--radius-md); display: flex; flex-direction: column; gap: 4px; }
  .audit-item-head { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
  .audit-decision { flex-shrink: 0; padding: 1px 8px; border-radius: 10px; font-size: 11px; background: color-mix(in srgb, var(--foreground) 10%, transparent); }
  .audit-decision[data-decision='hard_block'] { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .audit-decision[data-decision='audit_only'] { color: var(--foreground-muted); }
  .audit-tool { font-family: var(--font-mono, monospace); font-size: var(--text-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .audit-time { margin-left: auto; flex-shrink: 0; color: var(--foreground-muted); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .audit-rules { display: flex; flex-wrap: wrap; gap: 4px; }
  .audit-rules code { padding: 0 6px; border-radius: 4px; font-size: 12px; background: color-mix(in srgb, var(--foreground) 8%, transparent); }
  .audit-session { color: var(--foreground-muted); font-size: var(--text-xs); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .audit-footer { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); margin-top: var(--space-3); }
  .audit-footer-actions { display: flex; gap: var(--space-2); }
  .audit-count { color: var(--foreground-muted); font-size: var(--text-xs); }
</style>
