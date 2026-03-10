<script lang="ts">
  import type { RetryRuntimeState } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    runtime: RetryRuntimeState;
  }

  let { runtime }: Props = $props();
  let now = $state(Date.now());

  $effect(() => {
    if (runtime.phase !== 'scheduled') {
      return;
    }

    now = Date.now();
    const timer = setInterval(() => {
      now = Date.now();
    }, 1000);

    return () => {
      clearInterval(timer);
    };
  });

  const waitSeconds = $derived(
    runtime.phase === 'scheduled'
      ? Math.max(0, Math.ceil(((runtime.nextRetryAt ?? Date.now()) - now) / 1000))
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
    border: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
    background: var(--surface-secondary, rgba(128, 128, 128, 0.08));
  }

  .retry-runtime-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .retry-runtime-wait {
    margin-top: 4px;
    font-size: 12px;
    color: var(--text-secondary);
  }
</style>