<script lang="ts">
  import type { PlaceholderState } from '../types/message';
  import type { IconName } from '../lib/icons';
  import WorkerBadge from './WorkerBadge.svelte';
  import StreamingIndicator from './StreamingIndicator.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    state: PlaceholderState;
  }
  let { state }: Props = $props();

  // 状态配置（符合 message-response-flow-design.md 规范）
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
  };

  const config = $derived(stateConfig[state] || stateConfig.pending);
</script>

<div class="placeholder-message" data-state={state}>
  <div class="placeholder-header">
    <WorkerBadge worker="orchestrator" size="sm" />
    <div class="placeholder-status">
      <span class="status-icon" class:spinning={state === 'pending'}>
        <Icon name={config.icon} size={14} />
      </span>
      <span class="status-text">{config.text}</span>
    </div>
  </div>

  <!-- 复用 StreamingIndicator 组件，统一跳动点样式 -->
  <div class="placeholder-indicator">
    <StreamingIndicator />
  </div>
</div>

<style>
  .placeholder-message {
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    border-left: 3px solid var(--color-orchestrator, var(--primary));
    animation: fadeSlideIn 0.2s ease-out;
    margin-right: var(--space-2);
  }

  /* 动画名称统一为 fadeSlideIn（符合设计文档） */
  @keyframes fadeSlideIn {
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

  /* StreamingIndicator 容器样式 */
  .placeholder-indicator {
    padding: var(--space-2) 0;
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
    animation: fadeSlideIn 0.2s ease-out;
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
