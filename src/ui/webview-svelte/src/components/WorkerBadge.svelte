<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { vscode } from '../lib/vscode-bridge';

  type WorkerType = 'orchestrator' | 'coder' | 'reviewer' | 'planner' | 'debugger' | 'claude' | 'codex' | 'gemini' | 'default';
  type WorkerStatus = 'idle' | 'running' | 'completed' | 'failed';

  interface Props {
    worker: string;
    status?: WorkerStatus;
    size?: 'sm' | 'md' | 'lg';
    showStatus?: boolean;
  }

  let {
    worker,
    status = 'idle',
    size = 'sm',
    showStatus = false
  }: Props = $props();

  // Worker 类型配置 - 使用 Icon 组件替代 emoji
  const workerConfig: Record<WorkerType, { colorVar: string; icon: IconName; label: string }> = {
    orchestrator: { colorVar: '--color-orchestrator', icon: 'target', label: '协调者' },
    coder: { colorVar: '--color-codex', icon: 'code', label: '编码者' },
    reviewer: { colorVar: '--color-claude', icon: 'search', label: '审查者' },
    planner: { colorVar: '--color-gemini', icon: 'list', label: '规划者' },
    debugger: { colorVar: '--color-claude', icon: 'bug', label: '调试者' },
    claude: { colorVar: '--color-claude', icon: 'brain', label: 'Claude' },
    codex: { colorVar: '--color-codex', icon: 'zap', label: 'Codex' },
    gemini: { colorVar: '--color-gemini', icon: 'sparkles', label: 'Gemini' },
    default: { colorVar: '--foreground-muted', icon: 'bot', label: 'Agent' }
  };

  // 状态配置
  const statusConfig: Record<WorkerStatus, { color: string; text: string }> = {
    idle: { color: 'var(--foreground-muted)', text: '空闲' },
    running: { color: 'var(--info)', text: '运行中' },
    completed: { color: 'var(--success)', text: '完成' },
    failed: { color: 'var(--error)', text: '失败' }
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
    const lowerWorker = worker.toLowerCase();
    for (const [key, value] of Object.entries(workerConfig)) {
      if (lowerWorker.includes(key)) {
        return value;
      }
    }
    return workerConfig.default;
  });

  const statusInfo = $derived(statusConfig[status]);
</script>

<span
  class="worker-badge size-{size} worker-{worker.toLowerCase()}"
  style="--worker-color: var({config.colorVar})"
  title="{config.label}{showStatus ? ` - ${statusInfo.text}` : ''}"
>
  <span class="worker-icon">
    <Icon name={config.icon} size={size === 'sm' ? 10 : size === 'md' ? 12 : 14} />
  </span>
  <span class="worker-name">{worker}</span>
  {#if showStatus}
    <span class="worker-status" style="color: {statusInfo.color}">
      {#if status === 'running'}
        <span class="status-dot running"></span>
      {/if}
      {statusInfo.text}
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
    text-transform: uppercase;
    letter-spacing: 0.5px;
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
