<script lang="ts">
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { messagesState, isPersistedSessionId } from '../stores/messages.svelte';
  import { listPluginJobs, createPluginJob, cancelPluginJob } from '../web/plugin-api';
  import type { PluginManifest, PluginJobSchedule } from '../shared/app-server-protocol.generated';
  import SelectField from './SelectField.svelte';
  let { manifest, active }: { manifest: PluginManifest; active: boolean } = $props();
  let jobs = $state<PluginJobSchedule[]>([]);
  let command = $state('');
  let text = $state('');
  let delay = $state(1);
  let interval = $state(0);
  let maxCalls = $state(0);
  let maxOutput = $state(2048);
  let timeout = $state(60);
  let busy = $state(false);
  let error = $state('');
  let loading = $state(true);
  let disposed = false;
  let reading = false;
  const session = $derived(messagesState.currentSessionId);
  const canCreate = $derived(active && !!command && isPersistedSessionId(session));
  $effect(() => { if (!command) command = manifest.contributions.commands?.[0]?.id ?? ''; });
  const options = $derived((manifest.contributions.commands ?? []).map(c => ({ value: c.id, label: c.title })));
  async function reload() {
    if (reading) return;
    reading = true;
    try { const result = await listPluginJobs(manifest.id); if (!disposed) { jobs = result; error = ''; } }
    catch (e) { if (!disposed) error = e instanceof Error ? e.message : String(e); }
    finally { reading = false; if (!disposed) loading = false; }
  }
  onMount(() => { void reload(); const timer = setInterval(() => { if (!busy && !document.hidden) void reload(); }, 2000); return () => { disposed = true; clearInterval(timer); }; });
  async function create(event: SubmitEvent) {
    event.preventDefault();
    if (!canCreate || !session || busy) return;
    busy = true; error = '';
    try {
      await createPluginJob(manifest.id, { commandId: command, sessionId: session, text, runAt: Date.now() + delay * 60000, intervalMs: interval ? interval * 60000 : null, budget: { maxModelCalls: maxCalls, maxOutputTokens: maxOutput, timeoutMs: timeout * 1000 } });
      await reload();
    } catch (e) { error = e instanceof Error ? e.message : String(e); }
    finally { busy = false; }
  }
  async function cancel(job: PluginJobSchedule) {
    busy = true; error = '';
    try { await cancelPluginJob(manifest.id, job.id); await reload(); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
    finally { busy = false; }
  }
</script>

<section class="settings-section" data-testid="plugin-jobs">
  <header class="settings-section-header"><div><h4 class="settings-section-title">{i18n.t('settings.plugins.jobs.title')}</h4><p class="settings-section-desc">{i18n.t('settings.plugins.jobs.description')}</p></div></header>
  <form onsubmit={create} class="job-form">
    <SelectField bind:value={command} {options} label={i18n.t('settings.plugins.jobs.command')} disabled={!active || busy} />
    <label>{i18n.t('settings.plugins.jobs.input')}<textarea class="form-input" rows="2" maxlength="65536" bind:value={text} disabled={busy}></textarea></label>
    <div class="job-fields">
      <label>{i18n.t('settings.plugins.jobs.delay')}<input class="form-input" type="number" min="0" max="525600" required bind:value={delay} disabled={busy} /></label>
      <label>{i18n.t('settings.plugins.jobs.interval')}<input class="form-input" type="number" min="0" max="525600" required bind:value={interval} disabled={busy} /></label>
      <label>{i18n.t('settings.plugins.jobs.maxCalls')}<input class="form-input" type="number" min="0" max="100" required bind:value={maxCalls} disabled={busy} /></label>
      <label>{i18n.t('settings.plugins.jobs.maxOutput')}<input class="form-input" type="number" min="1" max="32768" required bind:value={maxOutput} disabled={busy} /></label>
      <label>{i18n.t('settings.plugins.jobs.timeout')}<input class="form-input" type="number" min="1" max="120" required bind:value={timeout} disabled={busy} /></label>
    </div>
    <p class="settings-hint">{i18n.t(canCreate ? 'settings.plugins.jobs.currentSession' : 'settings.plugins.jobs.selectSession')}</p>
    <button class="btn btn--sm btn--primary" type="submit" disabled={!canCreate || busy}>{i18n.t('settings.plugins.jobs.create')}</button>
  </form>
  {#if error}<p class="settings-notice settings-notice--error" role="alert">{error}</p>{/if}
  <div class="settings-rows" aria-live="polite">
    {#each jobs as job (job.id)}
      <div class="settings-row">
        <div class="settings-row-copy">
          <span class="settings-row-title">{manifest.contributions.commands?.find(c => c.id === job.request.commandId)?.title ?? job.request.commandId}</span>
          <span class="settings-row-desc">{i18n.t(job.enabled ? 'settings.plugins.jobs.enabled' : 'settings.plugins.jobs.disabled')} · {new Date(job.nextRunAt).toLocaleString()}</span>
          <span class="settings-row-desc">{i18n.t('settings.plugins.jobs.targetSession', { session: job.request.sessionId })}</span>
          <span class="settings-row-desc">{i18n.t('settings.plugins.jobs.budget', { calls: job.request.budget.maxModelCalls, output: job.request.budget.maxOutputTokens, timeout: Math.round(job.request.budget.timeoutMs / 1000) })}</span>
          {#if job.lastOutcome}<span class="settings-row-desc">{i18n.t('settings.plugins.jobs.result', { result: job.lastOutcome })}</span>{/if}
        </div>
        {#if job.enabled || job.pendingRequestId}<button class="btn btn--sm btn--secondary" type="button" disabled={busy} onclick={() => cancel(job)}>{i18n.t('settings.plugins.jobs.cancel')}</button>{/if}
      </div>
    {:else}<div class="settings-empty">{i18n.t(loading ? 'common.loading' : 'settings.plugins.jobs.empty')}</div>{/each}
  </div>
</section>
<style>
  .job-form { display: grid; gap: var(--space-3); min-inline-size: 0; }
  .job-form label { display: grid; gap: var(--space-2); font-size: var(--text-xs); color: var(--foreground-muted); min-inline-size: 0; }
  .job-fields { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 160px), 1fr)); gap: var(--space-3); }
  .job-form .btn { justify-self: start; }
</style>
