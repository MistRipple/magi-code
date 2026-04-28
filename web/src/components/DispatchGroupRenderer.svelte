<script lang="ts">
  import type { ContentBlock, DispatchGroupLane } from '../types/message';
  import {
    buildDispatchLaneCardData,
    mergeDispatchLanesByWorkerTab,
    resolveDispatchLaneMessageTimestamp,
  } from '../lib/worker-card-view-model';
  import SubTaskSummaryCard from './SubTaskSummaryCard.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block, readOnly = false }: Props = $props();

  const lanes = $derived.by(() => (
    mergeDispatchLanesByWorkerTab(
      Array.isArray(block.lanes)
        ? block.lanes.filter((lane): lane is DispatchGroupLane => Boolean(lane && typeof lane === 'object' && typeof lane.laneId === 'string'))
        : [],
    )
  ));

  const fallbackTimestamp = $derived.by(() => Date.now());
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
{/if}

<style>
  .dispatch-group {
    display: flex;
    flex-direction: column;
    gap: 12px;
    width: 100%;
  }
</style>
