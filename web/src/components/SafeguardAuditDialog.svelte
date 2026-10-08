<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getSafeguardAuditPage, type SafeguardAuditEntry } from '../web/agent-api';

  let { onClose } = $props<{ onClose: () => void }>();

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
    {#if nextBefore !== null && !loading}
      <button type="button" class="btn btn--sm" onclick={() => void load(nextBefore)}>
        {i18n.t('settings.safeguard.audit.loadMore')}
      </button>
    {/if}
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
  .audit-count { color: var(--foreground-muted); font-size: var(--text-xs); }
</style>
