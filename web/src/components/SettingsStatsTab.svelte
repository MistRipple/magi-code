<script lang="ts">
import { i18n } from '../stores/i18n.svelte';
import { getAgentColor } from '../lib/agent-colors';
  import Icon from './Icon.svelte';
  import type { ModelStatusMap } from '../types/message';

  let {
    totalInputTokens,
    totalOutputTokens,
    totalTokens,
    isRefreshing,
    refreshConnections,
    showResetConfirmDialog,
    modelStatuses,
    getWorkerStats,
    getStatusClass,
    getWorkerDisplayName,
    statusTexts,
  } = $props<{
    totalInputTokens: number;
    totalOutputTokens: number;
    totalTokens: number;
    isRefreshing: boolean;
    refreshConnections: () => void;
    showResetConfirmDialog: () => void;
    modelStatuses: ModelStatusMap;
    getWorkerStats: (worker: string) => any;
    getStatusClass: (status: string) => string;
    getWorkerDisplayName: (worker: string) => string;
    statusTexts: Record<string, () => string>;
  }>();

  function formatTokens(tokens: number | undefined): string {
    if (tokens === undefined || tokens === null) return '--';
    if (tokens >= 1000000) return `${(tokens / 1000000).toFixed(1)}M`;
    if (tokens >= 1000) return `${(tokens / 1000).toFixed(1)}K`;
    return String(tokens);
  }

</script>

<div class="apple-manager">
  <div class="apple-scroller-proxy">
    <div class="settings-section stats-section" style="border-bottom: none;">
      <div class="apple-dashboard-bar">
        <div class="stats-overview-inline">
          <div class="summary-item">
            <span class="summary-value">{formatTokens(totalInputTokens)}</span>
            <span class="summary-label">{i18n.t('settings.stats.inputTokens', { count: '' }).replace(/[:：]/g, '').trim()}</span>
          </div>
          <div class="summary-divider"></div>
          <div class="summary-item">
            <span class="summary-value">{formatTokens(totalOutputTokens)}</span>
            <span class="summary-label">{i18n.t('settings.stats.outputTokens', { count: '' }).replace(/[:：]/g, '').trim()}</span>
          </div>
          <div class="summary-divider"></div>
          <div class="summary-item primary">
            <span class="summary-value">{formatTokens(totalTokens)}</span>
            <span class="summary-label">{i18n.t('settings.stats.totalTokens', { count: '' }).replace(/[:：]/g, '').trim()}</span>
          </div>
        </div>

        <div class="settings-section-actions">
          <button class="apple-action-btn" class:saving={isRefreshing} onclick={refreshConnections} disabled={isRefreshing}>
            <Icon name="refresh" size={14} />
            {isRefreshing ? i18n.t('settings.stats.checking') : i18n.t('settings.stats.check')}
          </button>
          <button class="apple-action-btn danger" onclick={showResetConfirmDialog}>
            {i18n.t('settings.stats.resetTokens')}
          </button>
        </div>
      </div>

      <div class="apple-grid">
        {#each Object.keys(modelStatuses) as worker}
          {@const status = modelStatuses[worker]}
          {@const workerStats = getWorkerStats(worker)}
          {@const statusClass = getStatusClass(status?.status || 'checking')}
          {@const agentColorPair = getAgentColor(worker)}
          {@const modelLabel = workerStats?.resolvedModel
            || status?.model
            || (status?.status === 'not_configured'
              ? i18n.t('settings.stats.notConfigured')
              : status?.status === 'disabled'
                ? i18n.t('settings.stats.disabled')
                : i18n.t('settings.stats.unknownModel'))}
          <div class="apple-widget-card {statusClass}" data-worker={worker}>
            <div class="widget-header">
              <div class="brand-group">
                <div class="avatar-squircle" style="background: {agentColorPair.muted}; color: {agentColorPair.color}">
                  <Icon name="model" size={14} />
                </div>
                <div class="identity-stack">
                  <span class="widget-title">
                    {worker === 'orchestrator' ? i18n.t('settings.stats.orchestratorModel') : worker === 'auxiliary' ? i18n.t('settings.stats.auxiliaryModel') : getWorkerDisplayName(worker)}
                  </span>
                  {#if worker === 'orchestrator' || worker === 'auxiliary'}
                    <span class="apple-core-badge">CORE</span>
                  {/if}
                </div>
              </div>
              <div class="widget-status {statusClass}" title={(statusTexts[status?.status] || statusTexts['checking'])()}>
                <span class="apple-indicator {statusClass}"></span>
                <span class="status-text">{(statusTexts[status?.status] || statusTexts['checking'])()}</span>
              </div>
            </div>
            
            <div class="widget-body">
              {#if status?.error}
                <div class="error-text" title={status.error}>{status.error}</div>
              {:else}
                <div class="model-text" title={modelLabel}>{modelLabel}</div>
              {/if}
            </div>

            <div class="widget-metrics-grid">
              <div class="metric-block">
                <div class="metric-value">{workerStats?.totalExecutions ?? '--'}</div>
                <div class="metric-label">{i18n.t('settings.stats.calls', { count: '' }).replace(/[:：\s]/g, '')}</div>
              </div>
              <div class="metric-block">
                <div class="metric-value">{workerStats?.successRate != null ? `${Math.round(workerStats.successRate * 100)}%` : '--'}</div>
                <div class="metric-label">{i18n.t('settings.stats.successRate', { rate: '' }).replace(/[:：\s]/g, '')}</div>
              </div>
              <div class="metric-block">
                <div class="metric-value">{formatTokens(workerStats?.totalInputTokens ?? 0)}</div>
                <div class="metric-label">{i18n.t('settings.stats.input', { count: '' }).replace(/[:：\s]/g, '')}</div>
              </div>
              <div class="metric-block">
                <div class="metric-value">{formatTokens(workerStats?.totalOutputTokens ?? 0)}</div>
                <div class="metric-label">{i18n.t('settings.stats.output', { count: '' }).replace(/[:：\s]/g, '')}</div>
              </div>
            </div>
          </div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>


  .apple-dashboard-bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: rgba(var(--foreground-rgb), 0.04);
    border: 1px solid rgba(var(--foreground-rgb), 0.08);
    border-radius: 12px;
    padding: 14px 20px;
    margin-bottom: 24px;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.04);
  }

  .stats-overview-inline {
    display: flex;
    align-items: center;
    gap: 20px;
    flex-wrap: wrap;
  }

  .summary-divider {
    width: 1px;
    height: 24px;
    background: rgba(var(--foreground-rgb), 0.1);
  }

  .summary-item {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }

  .summary-value {
    font-size: 22px;
    font-weight: 700;
    color: var(--foreground);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.5px;
  }

  .summary-label {
    font-size: 10px;
    font-weight: 600;
    color: var(--foreground-muted);
    text-transform: uppercase;
  }



  .summary-item.primary .summary-value {
    color: var(--primary);
  }



  /* Apple Widget Card Style */
  .apple-widget-card {
    background: rgba(255, 255, 255, 0.92);
    border: 1px solid rgba(60, 60, 67, 0.16);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04), 0 6px 18px rgba(0, 0, 0, 0.05);
    border-radius: 12px;
    padding: 14px 18px 18px 16px;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    transition: background 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
    min-height: 103px;
  }

  .apple-widget-card:hover {
    border-color: rgba(60, 60, 67, 0.2);
    background: #ffffff;
  }

  :global(body.theme-dark) .apple-widget-card,
  :global(body.vscode-dark) .apple-widget-card,
  :global(:root.theme-dark) .apple-widget-card {
    background: rgba(255, 255, 255, 0.04);
    border-color: rgba(255, 255, 255, 0.14);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04), 0 6px 18px rgba(0, 0, 0, 0.05);
  }

  :global(body.theme-dark) .apple-widget-card:hover,
  :global(body.vscode-dark) .apple-widget-card:hover,
  :global(:root.theme-dark) .apple-widget-card:hover {
    border-color: rgba(255, 255, 255, 0.20);
    background: rgba(255, 255, 255, 0.07);
  }

  .apple-widget-card.error {
    background: rgba(var(--error-rgb, 255, 59, 48), 0.05);
    border-color: rgba(var(--error-rgb, 255, 59, 48), 0.2);
  }
  .apple-widget-card.disabled {
    opacity: 0.6;
    filter: grayscale(0.8);
  }

  .widget-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    margin-bottom: 4px;
  }

  .brand-group {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .avatar-squircle {
    width: 24px;
    height: 24px;
    border-radius: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .identity-stack {
    display: flex;
    flex-direction: column;
    justify-content: center;
    min-width: 0;
  }

  .widget-title {
    font-size: 13.5px;
    font-weight: 600;
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    letter-spacing: -0.2px;
  }

  .apple-core-badge {
    align-self: flex-start;
    font-size: 7.5px;
    font-weight: 700;
    padding: 1px 4px;
    background: rgba(var(--foreground-rgb), 0.1);
    color: var(--foreground-muted);
    border-radius: 4px;
    margin-top: 1px;
  }

  .widget-status {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 3px 6px;
    background: rgba(var(--foreground-rgb), 0.05);
    border-radius: 10px;
  }

  .status-text {
    font-size: 8.5px;
    font-weight: 600;
  }
  .widget-status.success .status-text { color: var(--success); }
  .widget-status.error .status-text { color: var(--error); }
  .widget-status.warning .status-text { color: var(--warning); }
  .widget-status.checking .status-text { color: var(--info); }
  .widget-status.disabled .status-text { color: var(--foreground-muted); }

  .widget-body {
    flex: 1;
    margin-bottom: 8px;
    display: flex;
    flex-direction: column;
    justify-content: flex-start;
  }

  .model-text {
    font-size: 11.5px;
    color: var(--foreground-muted);
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .error-text {
    font-size: 11px;
    color: var(--error);
    font-weight: 500;
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .widget-metrics-grid {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    margin-top: auto;
  }

  .metric-block {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
  }

  .metric-value {
    font-size: 14.5px;
    font-weight: 700;
    color: var(--foreground);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.3px;
    line-height: 1.1;
  }

  .metric-label {
    font-size: 8.5px;
    font-weight: 600;
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.2px;
  }

  @keyframes pulse {
    0% { opacity: 1; }
    50% { opacity: 0.5; }
    100% { opacity: 1; }
  }

  @media (max-width: 600px) {
    .stats-overview-panel {
      flex-wrap: wrap;
      gap: 12px;
    }
  }

</style>
