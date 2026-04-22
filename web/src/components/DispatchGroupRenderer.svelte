<script lang="ts">
  import type { ContentBlock, DispatchGroupLane } from '../types/message';
  import {
    buildDispatchLaneCardData,
    resolveDispatchLaneMessageTimestamp,
  } from '../lib/worker-card-view-model';
  import { i18n } from '../stores/i18n.svelte';
  import SubTaskSummaryCard from './SubTaskSummaryCard.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block, isStreaming = false, readOnly = false }: Props = $props();

  const lanes = $derived.by(() => (
    Array.isArray(block.lanes)
      ? block.lanes.filter((lane): lane is DispatchGroupLane => Boolean(lane && typeof lane === 'object' && typeof lane.laneId === 'string'))
      : []
  ));

  const fallbackTimestamp = $derived.by(() => Date.now());
  const summaryText = $derived.by(() => (
    typeof block.summaryText === 'string' ? block.summaryText.trim() : ''
  ));
  const placeholderStatus = $derived.by(() => {
    switch (block.status) {
      case 'running':
        return { label: i18n.t('subTaskSummaryCard.status.running'), tone: 'running' };
      case 'completed':
        return { label: i18n.t('subTaskSummaryCard.status.completed'), tone: 'success' };
      case 'failed':
        return { label: i18n.t('subTaskSummaryCard.status.failed'), tone: 'danger' };
      case 'cancelled':
        return { label: i18n.t('subTaskSummaryCard.status.cancelled'), tone: 'danger' };
      case 'pending':
      default:
        return { label: i18n.t('subTaskSummaryCard.status.pending'), tone: 'pending' };
    }
  });
  const placeholderText = $derived.by(() => (
    summaryText
    || ((isStreaming || block.status === 'running' || block.status === 'pending')
      ? i18n.t('messageItem.placeholder.processing')
      : i18n.t('provider.subTaskFallbackTitle'))
  ));
  const showPlaceholderActivity = $derived(isStreaming || block.status === 'running' || block.status === 'pending');

</script>

{#if lanes.length > 0}
  <div class="dispatch-group" data-dispatch-wave-id={block.dispatchWaveId}>
    {#each lanes as lane (`${block.dispatchWaveId || 'dispatch'}:${lane.laneId}`)}
      <SubTaskSummaryCard
        card={buildDispatchLaneCardData(lane, block.dispatchWaveId)}
        {readOnly}
        compact={false}
        messageTimestamp={resolveDispatchLaneMessageTimestamp(lane, fallbackTimestamp)}
        startedAtOverride={lane.startedAt}
        runtimeStatus={lane.status}
      />
    {/each}
  </div>
{:else}
  <div class="dispatch-group" data-dispatch-wave-id={block.dispatchWaveId}>
    <div class="dispatch-group-placeholder" data-status={placeholderStatus.tone}>
      <div class={`dispatch-group-placeholder__status dispatch-group-placeholder__status--${placeholderStatus.tone}`}>
        {placeholderStatus.label}
      </div>
      <p class="dispatch-group-placeholder__summary">{placeholderText}</p>
      {#if showPlaceholderActivity}
        <div class="dispatch-group-placeholder__activity" aria-hidden="true">
          <span class="dispatch-group-placeholder__dot"></span>
          <span class="dispatch-group-placeholder__dot"></span>
          <span class="dispatch-group-placeholder__dot"></span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .dispatch-group {
    display: flex;
    flex-direction: column;
    gap: 12px;
    width: 100%;
  }

  .dispatch-group-placeholder {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
  }

  .dispatch-group-placeholder__status {
    display: inline-flex;
    align-items: center;
    width: fit-content;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  .dispatch-group-placeholder__status--pending {
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
  }

  .dispatch-group-placeholder__status--running {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  .dispatch-group-placeholder__status--success {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }

  .dispatch-group-placeholder__status--danger {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 14%, transparent);
  }

  .dispatch-group-placeholder__summary {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .dispatch-group-placeholder__activity {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .dispatch-group-placeholder__dot {
    width: 6px;
    height: 6px;
    border-radius: 999px;
    background: var(--foreground-muted);
    opacity: 0.72;
    animation: dispatchPlaceholderBounce 1.4s ease-in-out infinite;
  }

  .dispatch-group-placeholder__dot:nth-child(2) {
    animation-delay: 0.18s;
  }

  .dispatch-group-placeholder__dot:nth-child(3) {
    animation-delay: 0.36s;
  }

  @keyframes dispatchPlaceholderBounce {
    0%, 80%, 100% {
      transform: translateY(0);
      opacity: 0.45;
    }
    40% {
      transform: translateY(-3px);
      opacity: 1;
    }
  }
</style>
