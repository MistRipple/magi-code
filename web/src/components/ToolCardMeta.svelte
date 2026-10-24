<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  // Presentation only: the caller owns the canonical outcome and disclosure state.
  let { status, label = '', expanded, duration, children }: {
    status?: string;
    label?: string;
    expanded?: boolean;
    duration?: number;
    children?: Snippet;
  } = $props();
  const statusLabel = $derived(label || (status === 'unconfirmed' ? i18n.t('toolCall.status.unconfirmed') : status === 'degraded' ? i18n.t('toolCall.agentSpawn.degraded') : status ? i18n.t(`terminalSession.status.${status}`) : ''));
  const elapsed = $derived(typeof duration === 'number' && Number.isFinite(duration) && duration >= 0
    ? `${(duration / 1000).toFixed(1)} s` : '');
</script>

<span class="tool-card-meta">
  {@render children?.()}
  {#if elapsed}<span class="tool-card-duration" title={`${i18n.t('toolCall.duration')} ${elapsed}`}>{elapsed}</span>{/if}
  <span class="tool-card-status-slot">
    {#if status}<span class="tool-status status-{status}" role="img" aria-label={statusLabel} title={statusLabel}><span class="status-dot" class:pulsing={status === 'running' || status === 'pending'}></span></span>{/if}
  </span>
  <span class="tool-card-chevron-slot" aria-hidden="true">
    {#if expanded !== undefined}<span class="tool-card-chevron" class:expanded><Icon name="chevron-right" size={12} /></span>{/if}
  </span>
</span>

<style>
  .tool-card-meta { display:inline-flex; align-items:center; justify-content:flex-end; gap:8px; flex:0 0 auto; margin-left:auto; color:var(--foreground-muted); font-size:var(--text-xs); font-weight:var(--font-normal); line-height:1; font-variant-numeric:tabular-nums; }
  .tool-card-status-slot,.tool-card-chevron-slot { display:inline-flex; align-items:center; justify-content:center; width:14px; height:18px; flex:0 0 14px; }
  .tool-status { display:inline-flex; align-items:center; justify-content:center; width:14px; height:18px; }
  .status-dot { display:block; flex:none; width:7px; height:7px; border-radius:50%; background:currentColor; }
  .status-pending,.status-unconfirmed,.status-degraded { color:var(--warning); }
  .status-running { color:var(--info); }
  .status-success { color:var(--success); }
  .status-error { color:var(--error); }
  .status-cancelled { color:var(--foreground-muted); }
  .pulsing { animation:tool-card-pulse 1.5s ease-in-out infinite; }
  .tool-card-chevron { display:flex; align-items:center; justify-content:center; color:var(--foreground-muted); opacity:.65; transition:transform var(--transition-fast); }
  .tool-card-chevron.expanded { transform:rotate(90deg); opacity:1; }
  .tool-card-duration { white-space:nowrap; }
  @keyframes tool-card-pulse { 50% { opacity:.35; } }
  @container(max-width:360px) { .tool-card-duration { display:none; } }
  @media(max-width:480px) { .tool-card-duration { display:none; } }
  @media(prefers-reduced-motion:reduce) { .pulsing { animation:none; } .tool-card-chevron { transition:none; } }
</style>
