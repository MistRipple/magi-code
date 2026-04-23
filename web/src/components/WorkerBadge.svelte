<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { vscode } from '../lib/vscode-bridge';
  import { i18n } from '../stores/i18n.svelte';
  import { resolveAgentDisplayLabel } from '../lib/agent-colors';

  type WorkerStatus = 'idle' | 'running' | 'completed' | 'failed';

  interface Props {
    worker: string;
    label?: string;
    status?: WorkerStatus;
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

  // Worker 类型配置 — 系统角色预设 + 通用 fallback
  const builtInConfig: Record<string, { colorVar: string; icon: IconName; labelKey?: string; label?: string }> = {
    orchestrator: { colorVar: '--color-orchestrator', icon: 'target', labelKey: 'workerBadge.role.orchestrator' },
    auxiliary: { colorVar: '--color-auxiliary', icon: 'wrench', label: 'Auxiliary' },
  };
  const defaultConfig = { colorVar: '--foreground-muted', icon: 'bot' as IconName, labelKey: undefined as string | undefined, label: 'Agent' };

  // 状态配置
  const statusConfig: Record<WorkerStatus, { color: string; textKey: string }> = {
    idle: { color: 'var(--foreground-muted)', textKey: 'workerBadge.status.idle' },
    running: { color: 'var(--info)', textKey: 'workerBadge.status.running' },
    completed: { color: 'var(--success)', textKey: 'workerBadge.status.completed' },
    failed: { color: 'var(--error)', textKey: 'workerBadge.status.failed' }
  };

  // 获取 worker 配置
  const config = $derived.by(() => {
    if (!worker || typeof worker !== 'string') {
      vscode.postMessage({
        type: 'uiError',
        component: 'WorkerBadge',
        detail: { worker, status, size },
        stack: new Error('WorkerBadge: invalid worker').stack,
      });
      throw new Error('WorkerBadge: invalid worker');
    }
    // 精确匹配内置角色名
    if (builtInConfig[worker]) {
      return builtInConfig[worker];
    }
    // 模糊匹配
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
  class="worker-badge size-{size} worker-{worker.toLowerCase()}"
  style="--worker-color: var({config.colorVar})"
  title="{displayWorkerName}{showStatus ? ` - ${i18n.t(statusInfo.textKey)}` : ''}"
>
  <span class="worker-icon">
    <Icon name={config.icon} size={size === 'sm' ? 10 : size === 'md' ? 12 : 14} />
  </span>
  <span class="worker-name">{displayWorkerName}</span>
  {#if showStatus}
    <span class="worker-status" style="color: {statusInfo.color}">
      {#if status === 'running'}
        <span class="status-dot running"></span>
      {/if}
      {i18n.t(statusInfo.textKey)}
    </span>
  {/if}
</span>

<style>
  .worker-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--worker-color) 15%, transparent);
    border: 1px solid color-mix(in srgb, var(--worker-color) 30%, transparent);
    font-size: var(--text-xs);
    font-weight: 500;
    white-space: nowrap;
  }

  .size-sm { padding: 1px 6px; font-size: 11px; }
  .size-md { padding: 2px 8px; font-size: var(--text-xs); }
  .size-lg { padding: 4px 10px; font-size: var(--text-sm); }

  .worker-icon { font-size: 0.9em; }
  
  .worker-name {
    color: var(--worker-color);
  }

  .worker-status {
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
