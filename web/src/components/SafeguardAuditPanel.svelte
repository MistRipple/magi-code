<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    deleteSafeguardAudit,
    getSafeguardAuditPage,
    type SafeguardAuditEntry,
  } from '../web/agent-api';

  let { persistenceHealthy, onCountChange } = $props<{
    persistenceHealthy: boolean;
    /** 加载与删除后把最新总数交回设置页，标签上的数量随之更新。 */
    onCountChange: (count: number) => void;
  }>();

  let entries = $state<SafeguardAuditEntry[]>([]);
  let total = $state(0);
  let nextBefore = $state<number | null>(null);
  let loading = $state(true);
  let loadError = $state('');
  let deleting = $state(false);

  async function load(before?: number | null): Promise<void> {
    loading = true;
    loadError = '';
    try {
      const page = await getSafeguardAuditPage(before);
      entries = before == null ? page.entries : [...entries, ...page.entries];
      total = page.total;
      nextBefore = page.nextBefore;
      onCountChange(page.total);
    } catch (error) {
      loadError = error instanceof Error ? error.message : String(error);
    } finally {
      loading = false;
    }
  }

  onMount(() => { void load(); });

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

  function accessLabel(profile: string | null): string {
    switch (profile) {
      case 'ReadOnly':
        return i18n.t('input.access.readOnly');
      case 'Restricted':
        return i18n.t('input.access.restricted');
      case 'FullAccess':
        return i18n.t('input.access.fullAccess');
      default:
        return profile ?? '';
    }
  }

  function formatTime(millis: number): string {
    return new Date(millis).toLocaleString(i18n.locale, { hour12: false });
  }
</script>

<div class="audit-panel">
  <div class="settings-section-desc">{i18n.t('settings.safeguard.audit.note')}</div>
  {#if !persistenceHealthy}
    <div class="audit-warning">{i18n.t('settings.safeguard.auditUnavailable')}</div>
  {/if}

  <div class="audit-toolbar">
    <span class="audit-count">{i18n.t('settings.safeguard.audit.shown', { shown: entries.length, total })}</span>
    <span class="audit-toolbar-actions">
      <button type="button" class="btn btn--secondary btn--sm" disabled={loading || deleting} onclick={() => void load()}>
        <Icon name="refresh" size={13} />
        {i18n.t('settings.safeguard.audit.refresh')}
      </button>
      <button type="button" class="btn btn--danger btn--sm" disabled={total === 0 || loading || deleting} onclick={() => void clearAll()}>
        {i18n.t('settings.safeguard.audit.clearAll')}
      </button>
    </span>
  </div>

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
          <div class="audit-meta">
            {#if entry.accessProfile}<span>{accessLabel(entry.accessProfile)}</span>{/if}
            {#if entry.sessionId}
              <span>{i18n.t('settings.safeguard.audit.session')}：{entry.sessionTitle || i18n.t('settings.safeguard.audit.sessionGone')}</span>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if loading}
    <div class="audit-status">{i18n.t('common.loading')}</div>
  {:else if nextBefore !== null}
    <div class="audit-more">
      <button type="button" class="btn btn--secondary btn--sm" onclick={() => void load(nextBefore)}>
        {i18n.t('settings.safeguard.audit.loadMore')}
      </button>
    </div>
  {/if}
</div>

<style>
  .audit-panel { display: flex; flex-direction: column; gap: var(--space-3); }
  .audit-warning { padding: 8px 10px; border: 1px solid color-mix(in srgb, var(--danger) 45%, var(--border)); border-radius: 8px; color: var(--danger); font-size: var(--text-xs); }
  .audit-toolbar { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); flex-wrap: wrap; }
  .audit-toolbar-actions { display: flex; gap: var(--space-2); }
  .audit-count { color: var(--foreground-muted); font-size: var(--text-xs); }
  .audit-error { margin: 0; color: var(--danger); font-size: var(--text-sm); }
  .audit-empty, .audit-status { padding: var(--space-6) 0; margin: 0; text-align: center; color: var(--foreground-muted); font-size: var(--text-sm); }
  .audit-more { display: flex; justify-content: center; }
  .audit-list { list-style: none; margin: 0; padding: 0; border: 1px solid var(--border); border-radius: var(--radius-md); overflow: hidden; }
  .audit-item { padding: var(--space-2) var(--space-3); display: flex; flex-direction: column; gap: 4px; }
  .audit-item + .audit-item { border-top: 1px solid var(--border-subtle, var(--border)); }
  .audit-item-head { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
  .audit-decision { flex-shrink: 0; padding: 1px 8px; border-radius: 10px; font-size: 11px; background: color-mix(in srgb, var(--foreground) 10%, transparent); }
  .audit-decision[data-decision='hard_block'] { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .audit-decision[data-decision='audit_only'] { color: var(--foreground-muted); }
  .audit-tool { min-width: 0; font-family: var(--font-mono, monospace); font-size: var(--text-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .audit-time { margin-left: auto; flex-shrink: 0; color: var(--foreground-muted); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .audit-rules { display: flex; flex-wrap: wrap; gap: 4px; }
  .audit-rules code { padding: 0 6px; border-radius: 4px; font-size: 12px; background: color-mix(in srgb, var(--foreground) 8%, transparent); }
  .audit-meta { display: flex; flex-wrap: wrap; gap: var(--space-3); color: var(--foreground-muted); font-size: var(--text-xs); }
</style>
