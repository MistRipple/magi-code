<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import SelectField from './SelectField.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import type { StandardMessage } from '../shared/protocol/message-protocol';
  import { getAgentExecutionStats, type AgentExecutionStatsPayload } from '../web/agent-api';
  import { projectSessionStats, type SessionStatsScope } from '../lib/session-stats';
  import { anchorStatsPanel } from '../lib/anchored-stats-panel';

  let { sessionId, running = false }: { sessionId: string; running?: boolean } = $props();
  let scope = $state<SessionStatsScope>('all');
  let open = $state(false);
  let expanded = $state<'tokens' | 'performance' | null>(null);
  let snapshot = $state<AgentExecutionStatsPayload | null>(null);
  let loading = $state(false);
  let failed = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let panel = $state<HTMLDivElement>();
  let refresh = $state(() => {});
  const id = `session-stats-${Math.random().toString(36).slice(2)}`;
  const stats = $derived(projectSessionStats(snapshot?.items ?? [], scope));
  const empty = $derived(snapshot != null && stats.calls === 0);
  const tokensKnown = $derived(empty || stats.tokenObserved > 0);
  const scopes = $derived([
    { value: 'all', label: i18n.t('input.stats.all') },
    { value: 'orchestrator', label: i18n.t('input.stats.main') },
    { value: 'worker', label: i18n.t('input.stats.worker') },
    { value: 'auxiliary', label: i18n.t('input.stats.auxiliary') },
  ]);
  const decimal = (value: number | null) => value == null ? '—' : value.toLocaleString(i18n.locale, { maximumFractionDigits: 1, minimumFractionDigits: 1 });
  const exact = (value: number) => value.toLocaleString(i18n.locale);
  const count = (value: number) => snapshot ? exact(value) : '—';
  const compact = (value: number) => value >= 1e6 ? `${decimal(value / 1e6)}M` : value >= 1000 ? `${decimal(value / 1000)}K` : exact(value);
  const percent = (value: number | null) => value == null ? i18n.t('input.stats.unprovided') : `${decimal(value * 100)}%`;
  const seconds = (value: number | null) => value == null ? i18n.t('input.stats.unprovided') : i18n.t('input.stats.seconds', { value: decimal(value / 1000) });

  function close(returnFocus = false) { open = false; if (returnFocus) trigger?.focus(); }
  function toggle(event: MouseEvent) {
    event.stopPropagation();
    open = !open;
    if (open) refresh();
  }
  function keydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && open) { event.preventDefault(); event.stopPropagation(); close(true); }
  }

  $effect(() => {
    const session = sessionId;
    snapshot = null; failed = false; loading = false; scope = 'all'; expanded = null; open = false;
    let disposed = false;
    let inFlight = false;
    let dirty = false;
    let timer: ReturnType<typeof setTimeout>;
    async function load() {
      if (!session || disposed) return;
      if (inFlight) { dirty = true; return; }
      inFlight = true; loading = true;
      try {
        const result = await getAgentExecutionStats(session);
        if (!disposed) { snapshot = result; failed = false; }
      } catch { if (!disposed) failed = true; }
      finally {
        inFlight = false;
        if (!disposed) {
          loading = false;
          if (dirty) { dirty = false; schedule(); }
        }
      }
    }
    function schedule() { clearTimeout(timer); timer = setTimeout(() => void load(), 150); }
    refresh = schedule;
    const changed = (event: Event) => {
      const changedSession = (event as CustomEvent<{ sessionId?: string }>).detail?.sessionId;
      if (!changedSession || changedSession === session) schedule();
    };
    const visible = () => { if (document.visibilityState === 'visible') schedule(); };
    window.addEventListener('magi:usageChanged', changed);
    window.addEventListener('focus', schedule);
    document.addEventListener('visibilitychange', visible);
    const unsubscribe = vscode.onMessage(message => {
      if (message.type !== 'unifiedMessage') return;
      const standard = message.message as StandardMessage | undefined;
      if (standard?.data?.dataType === 'sessionBootstrapLoaded') schedule();
    });
    void load();
    return () => {
      disposed = true; clearTimeout(timer); unsubscribe();
      window.removeEventListener('magi:usageChanged', changed);
      window.removeEventListener('focus', schedule);
      document.removeEventListener('visibilitychange', visible);
    };
  });

  onMount(() => {
    const clickAway = (event: MouseEvent) => {
      const path = event.composedPath();
      if (trigger && path.includes(trigger) || panel && path.includes(panel)) return;
      close();
    };
    document.addEventListener('click', clickAway, true);
    document.addEventListener('keydown', keydown);
    return () => {
      document.removeEventListener('click', clickAway, true);
      document.removeEventListener('keydown', keydown);
    };
  });
</script>

<button type="button" class="session-stats-trigger" class:active={open} bind:this={trigger}
  onclick={toggle} onkeydown={keydown} title={i18n.t('input.stats.title')} aria-label={i18n.t('input.stats.trigger', { speed: decimal(stats.tps), cache: percent(stats.cache.tokenHitRate) })}
  aria-expanded={open} aria-controls={id} aria-haspopup="dialog" data-testid="session-stats-trigger">
  <Icon name="stats" size={16} />
  <span class="stats-summary"><bdi>{decimal(stats.tps)} tok/s</bdi><span aria-hidden="true">·</span><span>{i18n.t('input.stats.cacheShort', { value: snapshot ? percent(stats.cache.tokenHitRate) : '—' })}</span></span>
  {#if running}<span class="stats-update-dot" aria-hidden="true"></span>{/if}
</button>
{#if open && trigger}
  <div class="session-stats-popover" popover="manual" id={id} role="dialog" aria-label={i18n.t('input.stats.title')} tabindex="-1"
    bind:this={panel} onkeydown={keydown} data-magi-surface="popover" use:anchorStatsPanel={trigger} aria-busy={loading}>
    <h2 class="composer-popover-title">{i18n.t('input.stats.title')}</h2>
    <div class="stats-subhead"><span>{i18n.t('input.stats.current')}</span><SelectField label={i18n.t('input.stats.scope')} bind:value={scope} options={scopes} /></div>
    {#if failed}<div class="stats-error" role="status"><span>{i18n.t('input.stats.loadFailed')}</span><button class="btn btn--sm btn--secondary" onclick={refresh}>{i18n.t('input.stats.retry')}</button></div>{/if}
    <div class="metric-grid">
        <div class="metric"><div class="metric-label"><Icon name="clock" size={13} /><span>{i18n.t('input.stats.ttft')} <small>TTFT</small></span></div><div class="metric-value" class:metric-value--placeholder={stats.ttftMs == null}>{decimal(stats.ttftMs == null ? null : stats.ttftMs / 1000)}{#if stats.ttftMs != null}<span>{i18n.t('input.stats.secondUnit')}</span>{/if}</div><span class="metric-caption">{i18n.t('input.stats.ttftSamples', { count: count(stats.ttftCount) })}</span></div>
        <div class="metric"><div class="metric-label"><Icon name="zap" size={13} /><span>{i18n.t('input.stats.tps')} <small>TPS</small></span></div><div class="metric-value" class:metric-value--placeholder={stats.tps == null}>{decimal(stats.tps)}{#if stats.tps != null}<span>tok/s</span>{/if}</div><span class="metric-caption">{stats.tpsCount ? i18n.t('input.stats.weighted') : i18n.t('input.stats.noSpeed')}</span></div>
        <div class="metric"><div class="metric-label"><Icon name="database" size={13} /><span>{i18n.t('input.stats.cache')}</span></div><div class="metric-value" class:metric-value--placeholder={stats.cache.tokenHitRate == null} class:metric-value--status={stats.cache.tokenHitRate == null && !stats.cache.observedCallCount}>{stats.cache.tokenHitRate == null ? (stats.cache.observedCallCount ? '—' : i18n.t('input.stats.unobserved')) : percent(stats.cache.tokenHitRate)}</div><span class="metric-caption">{i18n.t('input.stats.cacheBasis')}</span></div>
        <div class="metric"><div class="metric-label"><Icon name="send" size={13} /><span>{i18n.t('input.stats.output')}</span></div><div class="metric-value" class:metric-value--placeholder={!tokensKnown}>{tokensKnown ? compact(stats.output) : '—'}{#if tokensKnown}<span>tok</span>{/if}</div><span class="metric-caption">{i18n.t('input.stats.tokenCoverage', { observed: count(stats.tokenObserved), count: count(stats.calls) })}</span></div>
    </div>
      <div class="stats-details">
        <button class="disclosure" aria-expanded={expanded === 'tokens'} aria-controls={`${id}-tokens`} onclick={() => expanded = expanded === 'tokens' ? null : 'tokens'}><Icon name="database" size={14} /><span>{i18n.t('input.stats.tokenDetails')}</span><Icon name={expanded === 'tokens' ? 'chevron-up' : 'chevron-down'} size={13} /></button>
        {#if expanded === 'tokens'}<div id={`${id}-tokens`} class="detail-body"><dl>
          <div><dt>{i18n.t('input.stats.input')}</dt><dd>{tokensKnown ? `${exact(stats.input)} tok` : i18n.t('input.stats.unprovided')}</dd></div>
          <div><dt>{i18n.t('input.stats.read')}</dt><dd>{(empty || stats.cache.observedCallCount) ? `${exact(stats.cache.readTokens)} tok` : i18n.t('input.stats.unprovided')}</dd></div>
          <div><dt>{i18n.t('input.stats.uncached')}</dt><dd>{(empty || stats.cache.observedCallCount) ? `${exact(Math.max(0, stats.cache.inputTokens - stats.cache.readTokens))} tok` : i18n.t('input.stats.unprovided')}</dd></div>
          <div><dt>{i18n.t('input.stats.write')}</dt><dd>{(empty || stats.writeObserved) ? `${exact(stats.cache.writeTokens)} tok` : i18n.t('input.stats.unprovided')}</dd></div>
          <div><dt>{i18n.t('input.stats.output')}</dt><dd>{tokensKnown ? `${exact(stats.output)} tok` : i18n.t('input.stats.unprovided')}</dd></div>
          <div><dt>{i18n.t('input.stats.requestRate')}</dt><dd>{percent(stats.cache.requestHitRate)}</dd></div>
          <div><dt>{i18n.t('input.stats.hitRequests')}</dt><dd>{count(stats.cache.hitCallCount)} / {count(stats.cache.observedCallCount)}</dd></div>
        </dl></div>{/if}
        <button class="disclosure" aria-expanded={expanded === 'performance'} aria-controls={`${id}-performance`} onclick={() => expanded = expanded === 'performance' ? null : 'performance'}><Icon name="clock" size={14} /><span>{i18n.t('input.stats.performanceDetails')}</span><Icon name={expanded === 'performance' ? 'chevron-up' : 'chevron-down'} size={13} /></button>
        {#if expanded === 'performance'}<div id={`${id}-performance`} class="detail-body"><dl>
          <div><dt>{i18n.t('input.stats.ttftCoverage')}</dt><dd>{count(stats.ttftCount)} / {count(stats.calls)}</dd></div>
          <div><dt>{i18n.t('input.stats.tpsCoverage')}</dt><dd>{count(stats.tpsCount)} / {count(stats.calls)}</dd></div>
          <div><dt>{i18n.t('input.stats.lastTtft')}</dt><dd>{seconds(stats.lastTtftMs)}</dd></div>
          <div><dt>{i18n.t('input.stats.lastTps')}</dt><dd>{stats.lastTps == null ? i18n.t('input.stats.unprovided') : `${decimal(stats.lastTps)} tok/s`}</dd></div>
        </dl></div>{/if}
      </div>
      <div class="observation-footer"><Icon name="info" size={12} /><span>{i18n.t('input.stats.observed', { observed: count(stats.cache.observedCallCount), count: count(stats.calls) })}</span></div>
    {#if loading && !snapshot}
      <div class="stats-empty stats-empty--compact" role="status"><Icon name="loader" size={18} /><span>{i18n.t('input.stats.loading')}</span></div>
    {:else if failed && !snapshot}
      <div class="stats-empty stats-empty--compact" role="status"><Icon name="info" size={18} /><span>{i18n.t('input.stats.unprovided')}</span></div>
    {/if}
    {#if running}<p class="detail-note" role="status">{i18n.t('input.stats.pending')}</p>{/if}
  </div>
{/if}

<style>
  .session-stats-trigger { position: relative; display: inline-flex; align-items: center; justify-content: center; gap: 6px; min-inline-size: 28px; min-block-size: 28px; padding: 4px; border: 0; border-radius: var(--radius-full); background: transparent; color: var(--foreground-muted); cursor: pointer; font: inherit; font-size: var(--text-xs); flex: none; }
  .session-stats-trigger:hover, .session-stats-trigger.active { color: var(--foreground); background: var(--surface-hover); }
  button:active { background: var(--surface-active); }
  button:focus-visible { outline: 2px solid var(--primary); outline-offset: 2px; }
  .stats-summary { display: inline-flex; align-items: baseline; gap: 6px; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .stats-update-dot { position: absolute; inset-block-start: 2px; inset-inline-end: 1px; inline-size: 4px; block-size: 4px; border-radius: 50%; background: var(--primary); }
  .session-stats-popover { position: fixed; z-index: var(--z-popover); display: flex; flex-direction: column; box-sizing: border-box; overflow: auto; overscroll-behavior: contain; min-inline-size: 0; padding: var(--space-4); border-radius: calc(var(--radius-sm) + var(--space-4)); background: var(--background); color: var(--foreground); box-shadow: inset 0 0 0 var(--magi-hairline) var(--magi-border-faint), var(--shadow-lg); font-size: var(--text-xs); line-height: 1.5; font-variant-numeric: tabular-nums; }
  .stats-subhead { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 8px; padding-block: 6px 16px; color: var(--foreground-muted); }
  .stats-subhead :global(.select-field) { max-inline-size: 60%; }
  .metric-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 20px 16px; padding-block: 2px 18px; }
  .metric { min-inline-size: 0; }
  .metric-label { display: flex; align-items: baseline; gap: 5px; color: var(--foreground-muted); font-size: 12px; line-height: 1.4; }
  .metric-label :global(svg) { flex: none; align-self: center; }
  small { font-size: var(--text-xs); }
  .metric-value { display: flex; flex-wrap: wrap; align-items: baseline; gap: 5px; margin-block: 5px 2px; font-size: 24px; line-height: 1.3; font-weight: var(--font-medium); overflow-wrap: anywhere; font-variant-numeric: tabular-nums; }
  .metric-value > span { font-size: var(--text-xs); color: var(--foreground-muted); font-weight: var(--font-normal); }
  .metric-value--placeholder { color: var(--foreground-muted); font-size: 21px; line-height: 1.45; font-weight: var(--font-normal); }
  .metric-value--status { font-size: 15px; line-height: 1.5; font-weight: var(--font-normal); letter-spacing: .01em; }
  .metric-caption, .detail-note { color: var(--foreground-muted); font-size: 12px; line-height: 1.45; overflow-wrap: anywhere; }
  .detail-note { margin-block: 10px 0; }
  .stats-details, .observation-footer { border-block-start: var(--magi-hairline) solid var(--border); }
  .disclosure { display: flex; align-items: center; gap: 7px; box-sizing: border-box; inline-size: calc(100% + 12px); min-block-size: 44px; margin-inline: -6px; padding: 7px 6px; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); font: inherit; text-align: start; cursor: pointer; touch-action: manipulation; }
  .disclosure[aria-expanded='true'] { color: var(--foreground); }
  .disclosure:active { background: var(--surface-active); }
  .disclosure:focus-visible { outline-offset: -2px; }
  @media (hover: hover) { .disclosure:hover { color: var(--foreground); background: var(--surface-hover); } }
  .disclosure > span { flex: 1; min-inline-size: 0; }
  .detail-body { padding-block-end: 14px; }
  dl { display: grid; gap: 8px; margin: 0; }
  dl > div { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
  dt { min-inline-size: 0; color: var(--foreground-muted); overflow-wrap: anywhere; }
  dd { margin: 0; text-align: end; overflow-wrap: anywhere; direction: ltr; unicode-bidi: isolate; }
  .observation-footer { display: flex; align-items: baseline; gap: 5px; padding-block-start: 10px; color: var(--foreground-muted); font-size: var(--text-xs); }
  .observation-footer :global(svg) { flex: none; }
  .stats-empty { display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 24px 8px; color: var(--foreground-muted); text-align: center; }
  .stats-empty--compact { flex-direction: row; justify-content: center; padding: 12px 0 0; border-block-start: var(--magi-hairline) solid var(--border); font-size: var(--text-xs); }
  .stats-error { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; color: var(--error); }
  @container magi-composer (max-width: 800px) { .stats-summary { display: none; } }
  @media (hover: none), (pointer: coarse) {
    .session-stats-trigger { min-inline-size: 44px; min-block-size: 44px; padding: 14px; touch-action: manipulation; }
    .stats-summary { display: none; }
  }
  @media (prefers-reduced-motion: no-preference) { .session-stats-popover { animation: stats-reveal 150ms ease-out; } }
  @keyframes stats-reveal { from { opacity: 0; } to { opacity: 1; } }
</style>
