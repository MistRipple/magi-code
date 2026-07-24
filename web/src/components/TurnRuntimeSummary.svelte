<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { formatDuration, formatTraceableTime } from '../lib/utils';

  interface Props {
    durationMs: number;
    completedAt?: number | null;
    hasContent?: boolean;
  }

  let {
    durationMs,
    completedAt = null,
    hasContent = false,
  }: Props = $props();

  const normalizedDurationMs = $derived(Math.max(0, durationMs));
  const normalizedCompletedAt = $derived(
    typeof completedAt === 'number' && Number.isFinite(completedAt) && completedAt >= 0
      ? completedAt
      : null,
  );
  const durationLabel = $derived(
    normalizedDurationMs > 0 && normalizedDurationMs < 1000
      ? '<1s'
      : formatDuration(normalizedDurationMs),
  );
</script>

<div class="message-runtime-footer completed" class:has-content={hasContent}>
  <span class="message-runtime-text">
    {i18n.t('messageItem.responseDurationLabel')} {durationLabel}
    {#if normalizedCompletedAt !== null}
      · {formatTraceableTime(normalizedCompletedAt)}
    {/if}
  </span>
</div>

<style>
  .message-runtime-footer {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: calc(var(--text-xs) * 1.4 + var(--space-2));
    margin-top: 0;
    padding: var(--space-1) 0;
  }

  .message-runtime-footer.has-content {
    margin-top: var(--space-3);
    padding-top: var(--space-2);
  }

  .message-runtime-text {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
    font-variant-numeric: tabular-nums;
  }
</style>
