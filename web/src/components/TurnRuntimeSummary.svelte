<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { formatDuration, formatCompletedTime } from '../lib/utils';

  interface Props {
    durationMs: number;
    completedAt?: number | null;
    hasContent?: boolean;
    /** 摘要模式的轮次标题已经显示耗时时关闭，避免同一数字在标题和底部各出现一次。 */
    showDuration?: boolean;
  }

  let {
    durationMs,
    completedAt = null,
    hasContent = false,
    showDuration = true,
  }: Props = $props();

  const normalizedDurationMs = $derived(Math.max(0, durationMs));
  const normalizedCompletedAt = $derived(
    typeof completedAt === 'number' && Number.isFinite(completedAt) && completedAt >= 0
      && Number.isFinite(new Date(completedAt).getTime())
      ? completedAt
      : null,
  );
  const durationLabel = $derived(
    formatDuration(normalizedDurationMs, (key, vars) => i18n.t(key, vars)),
  );
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (完成时间支持键盘聚焦查看，无需增加操作按钮) -->
<div class="message-runtime-footer completed" class:has-content={hasContent}
  role="group" tabindex={normalizedCompletedAt !== null ? 0 : undefined}>
  <span class="message-runtime-text ui-meta">
    {#if showDuration}
      <span>{i18n.t('messageItem.responseDurationLabel')}{durationLabel}</span>
      {#if normalizedCompletedAt !== null}
        <time class="message-completed-time" datetime={new Date(normalizedCompletedAt).toISOString()}>{formatCompletedTime(normalizedCompletedAt, (key, vars) => i18n.t(key, vars))}</time>
      {/if}
    {:else if normalizedCompletedAt !== null}
      <time class="message-completed-time" datetime={new Date(normalizedCompletedAt).toISOString()}>{formatCompletedTime(normalizedCompletedAt, (key, vars) => i18n.t(key, vars))}</time>
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
    display: flex;
    flex-wrap: wrap;
    column-gap: var(--space-3);
    row-gap: var(--space-1);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
    font-variant-numeric: tabular-nums;
  }

  .message-runtime-text > * { white-space: nowrap; }

  .message-completed-time {
    opacity: 0;
    transition: opacity 140ms ease-out;
  }

  .message-runtime-footer:hover .message-completed-time,
  .message-runtime-footer:focus-within .message-completed-time,
  :global(.message-item.assistant:hover) .message-completed-time,
  :global(.message-item.assistant:focus-within) .message-completed-time {
    opacity: 1;
  }

  .message-runtime-footer:focus-visible {
    outline: 0;
    border-radius: var(--radius-sm);
    box-shadow: inset 0 0 0 1px var(--border);
  }

  @media (hover: none) {
    .message-completed-time { opacity: 1; }
  }

  @media (prefers-reduced-motion: reduce) {
    .message-completed-time { transition: none; }
  }
</style>
