<script lang="ts">
  import type { RetryRuntimeState } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import { uiClockState, retainUiClock } from '../stores/ui-clock.svelte';

  interface Props {
    runtime: RetryRuntimeState;
  }

  let { runtime }: Props = $props();
  const now = $derived(uiClockState.now);

  $effect(() => {
    if (runtime.phase !== 'scheduled') {
      return;
    }

    return retainUiClock();
  });

  const waitSeconds = $derived(
    runtime.phase === 'scheduled'
      ? Math.max(0, Math.ceil(((runtime.nextRetryAt ?? now) - now) / 1000))
      : 0
  );
</script>

<div class="retry-runtime-indicator" data-phase={runtime.phase}>
  <div class="retry-runtime-title">
    {#if runtime.phase === 'scheduled'}
      {i18n.t('messageItem.retry.scheduledTitle', { attempt: runtime.attempt, maxAttempts: runtime.maxAttempts })}
    {:else}
      {i18n.t('messageItem.retry.startedTitle', { attempt: runtime.attempt, maxAttempts: runtime.maxAttempts })}
    {/if}
  </div>

  {#if runtime.phase === 'scheduled'}
    <div class="retry-runtime-wait">
      {i18n.t('messageItem.retry.scheduledWait', { seconds: waitSeconds })}
    </div>
  {/if}
</div>

<style>
  .retry-runtime-indicator {
    margin-top: 10px;
    padding: 10px 12px;
    border-radius: 10px;
    border: 1px solid var(--border);
    background: var(--surface-2);
  }

  .retry-runtime-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--foreground);
  }

  .retry-runtime-wait {
    margin-top: 4px;
    font-size: 12px;
    color: var(--foreground-muted);
  }
</style>
