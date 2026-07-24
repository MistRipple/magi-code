<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { formatElapsed } from '../lib/utils';

  interface Props {
    elapsedSeconds: number;
  }

  let { elapsedSeconds }: Props = $props();
  const elapsedLabel = $derived(formatElapsed(Math.max(0, elapsedSeconds)));
</script>

<div class="turn-runtime-indicator" aria-label={i18n.t('runtimeState.status.running')} role="status">
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-dot"></span>
  <span class="turn-runtime-elapsed-time">{elapsedLabel}</span>
</div>

<style>
  .turn-runtime-indicator {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 24px;
    margin-top: var(--space-2);
    padding: var(--space-1) 0;
    color: var(--foreground-muted);
  }

  .turn-runtime-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--foreground-muted);
    opacity: 0.72;
    animation: turnRuntimeBounce 1.4s ease-in-out infinite;
  }

  .turn-runtime-dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .turn-runtime-dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  .turn-runtime-elapsed-time {
    margin-left: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }

  @keyframes turnRuntimeBounce {
    0%, 80%, 100% {
      opacity: 0.45;
      transform: translateY(0);
    }
    40% {
      opacity: 1;
      transform: translateY(-3px);
    }
  }
</style>
