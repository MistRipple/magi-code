<script lang="ts">
  import type { PlaceholderState } from '../types/message';
  import type { IconName } from '../lib/icons';
  import WorkerBadge from './WorkerBadge.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    state: PlaceholderState;
  }
  let { state }: Props = $props();

  const stateConfig: Record<PlaceholderState, { text: string; icon: IconName }> = {
    pending: {
      text: '正在准备...',
      icon: 'loader',
    },
    received: {
      text: '已接收...',
      icon: 'check',
    },
    thinking: {
      text: '正在思考...',
      icon: 'brain',
    },
    connecting: {
      text: '连接模型中...',
      icon: 'globe',
    },
  };

  const config = $derived(stateConfig[state] || stateConfig.pending);
</script>

<div class="placeholder-message" data-state={state}>
  <div class="placeholder-header">
    <WorkerBadge worker="orchestrator" size="sm" />
    <div class="placeholder-status">
      <span class="status-icon" class:spinning={state === 'pending' || state === 'connecting'}>
        <Icon name={config.icon} size={14} />
      </span>
      <span class="status-text">{config.text}</span>
    </div>
  </div>

  <div class="placeholder-dots">
    <span class="dot"></span>
    <span class="dot"></span>
    <span class="dot"></span>
  </div>
</div>

<style>
  .placeholder-message {
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    border-left: 3px solid var(--color-orchestrator, var(--primary));
    animation: placeholderEnter 0.25s ease-out;
    margin-right: var(--space-2);
  }

  @keyframes placeholderEnter {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .placeholder-header {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-bottom: var(--space-3);
  }

  .placeholder-status {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .status-icon {
    display: flex;
    color: var(--info);
  }

  .status-icon.spinning {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .status-text {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    animation: textPulse 2s ease-in-out infinite;
  }

  @keyframes textPulse {
    0%, 100% { opacity: 0.6; }
    50% { opacity: 1; }
  }

  /* Loading dots styles (for pending/connecting states) */
  .placeholder-dots {
    display: flex;
    gap: 4px;
    padding: var(--space-2) 0;
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--info);
    animation: dotBounce 1.4s ease-in-out infinite;
  }

  .dot:nth-child(1) { animation-delay: 0s; }
  .dot:nth-child(2) { animation-delay: 0.2s; }
  .dot:nth-child(3) { animation-delay: 0.4s; }

  @keyframes dotBounce {
    0%, 80%, 100% {
      transform: translateY(0);
      opacity: 0.4;
    }
    40% {
      transform: translateY(-6px);
      opacity: 1;
    }
  }

  /* State-specific border colors */
  .placeholder-message[data-state="pending"] {
    border-left-color: var(--foreground-muted);
  }

  .placeholder-message[data-state="received"] {
    border-left-color: var(--info);
  }

  .placeholder-message[data-state="thinking"] {
    border-left-color: var(--primary);
    animation: placeholderEnter 0.25s ease-out, thinkingPulse 2s ease-in-out infinite;
  }

  .placeholder-message[data-state="connecting"] {
    border-left-color: var(--warning);
  }

  @keyframes thinkingPulse {
    0%, 100% {
      box-shadow: 0 0 0 0 rgba(59, 130, 246, 0);
    }
    50% {
      box-shadow: 0 0 0 4px rgba(59, 130, 246, 0.1);
    }
  }

  /* Reduced motion preference */
  @media (prefers-reduced-motion: reduce) {
    .placeholder-message,
    .status-icon.spinning,
    .status-text {
      animation: none;
    }
    .status-text { opacity: 1; }
  }
</style>
