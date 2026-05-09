<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { vscode } from '../lib/vscode-bridge';
  import { i18n } from '../stores/i18n.svelte';
  import { resolveAgentDisplayLabel } from '../lib/agent-colors';

  type ExecutorStatus = 'idle' | 'running' | 'completed' | 'failed';

  interface Props {
    worker: string;
    label?: string;
    status?: ExecutorStatus;
    size?: 'sm' | 'md' | 'lg';
    showStatus?: boolean;
  }

  let {
    worker,
    label = '',
    status = 'idle',
    size = 'sm',
    showStatus = false
  }: Props = $props();
  const currentLocale = $derived(i18n.locale);

  const builtInConfig: Record<string, { colorVar: string; icon: IconName; labelKey?: string; label?: string }> = {
    orchestrator: { colorVar: '--color-orchestrator', icon: 'target', labelKey: 'workerBadge.role.orchestrator' },
    auxiliary: { colorVar: '--color-auxiliary', icon: 'wrench', label: 'Auxiliary' },
  };
  const defaultConfig = { colorVar: '--foreground-muted', icon: 'bot' as IconName, labelKey: undefined as string | undefined, label: 'Agent' };

  const statusConfig: Record<ExecutorStatus, { color: string; textKey: string }> = {
    idle: { color: 'var(--foreground-muted)', textKey: 'workerBadge.status.idle' },
    running: { color: 'var(--info)', textKey: 'workerBadge.status.running' },
    completed: { color: 'var(--success)', textKey: 'workerBadge.status.completed' },
    failed: { color: 'var(--error)', textKey: 'workerBadge.status.failed' }
  };

  const config = $derived.by(() => {
    if (!worker || typeof worker !== 'string') {
      vscode.postMessage({
        type: 'uiError',
        component: 'ExecutorBadge',
        detail: { worker, status, size },
        stack: new Error('ExecutorBadge: invalid worker').stack,
      });
      throw new Error('ExecutorBadge: invalid worker');
    }
    if (builtInConfig[worker]) {
      return builtInConfig[worker];
    }
    const lowerWorker = worker.toLowerCase();
    for (const [key, value] of Object.entries(builtInConfig)) {
      if (lowerWorker.includes(key)) {
        return value;
      }
    }
    return defaultConfig;
  });

  const statusInfo = $derived(statusConfig[status]);
  const displayWorkerName = $derived.by(() => {
    void currentLocale;
    const explicitLabel = typeof label === 'string' ? label.trim() : '';
    if (explicitLabel) {
      return explicitLabel;
    }
    return resolveAgentDisplayLabel(worker, (key) => i18n.t(key));
  });
</script>

<span
  class="executor-badge size-{size} executor-{worker.toLowerCase()}"
  style="--executor-color: var({config.colorVar})"
  title="{displayWorkerName}{showStatus ? ` - ${i18n.t(statusInfo.textKey)}` : ''}"
>
  <span class="executor-icon">
    <Icon name={config.icon} size={size === 'sm' ? 10 : size === 'md' ? 12 : 14} />
  </span>
  <span class="executor-name">{displayWorkerName}</span>
  {#if showStatus}
    <span class="executor-status" style="color: {statusInfo.color}">
      {#if status === 'running'}
        <span class="status-dot running"></span>
      {/if}
      {i18n.t(statusInfo.textKey)}
    </span>
  {/if}
</span>

<style>
  .executor-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--executor-color) 15%, transparent);
    border: 1px solid color-mix(in srgb, var(--executor-color) 30%, transparent);
    font-size: var(--text-xs);
    font-weight: 500;
    white-space: nowrap;
  }

  .size-sm { padding: 1px 6px; font-size: 11px; }
  .size-md { padding: 2px 8px; font-size: var(--text-xs); }
  .size-lg { padding: 4px 10px; font-size: var(--text-sm); }

  .executor-icon { font-size: 0.9em; }

  .executor-name {
    color: var(--executor-color);
  }

  .executor-status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 0.9em;
    opacity: 0.8;
  }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }

  .status-dot.running {
    animation: pulse 1.5s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.5; transform: scale(0.8); }
  }
</style>
