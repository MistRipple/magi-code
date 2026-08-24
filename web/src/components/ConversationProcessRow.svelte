<script lang="ts">
  import type { TimelineRenderItem } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import { resolveConversationProcessLabel } from '../lib/conversation-disclosure';

  interface Props {
    item: TimelineRenderItem;
  }

  let { item }: Props = $props();

  const message = $derived(item.message);
  const isStreaming = $derived(message.isStreaming || message.metadata?.turnItemStatus === 'running');

  const label = $derived(resolveConversationProcessLabel(message, i18n.t.bind(i18n)));

</script>

<div class="conversation-process-row" class:streaming={isStreaming} title={label}>
  <span class="process-marker" aria-hidden="true"></span>
  <span class="process-label">{label}</span>
</div>

<style>
  .conversation-process-row {
    display: flex;
    align-items: flex-start;
    min-height: 26px;
    gap: 7px;
    padding: 1px 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.4;
  }

  .conversation-process-row.streaming {
    color: var(--foreground);
  }

  .process-marker {
    position: relative;
    flex: 0 0 12px;
    width: 12px;
    height: 18px;
  }

  .process-marker::before {
    content: '';
    position: absolute;
    top: 7px;
    left: 4px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.52;
  }

  .conversation-process-row.streaming .process-marker::before {
    opacity: 1;
    animation: processMarkerPulse 1.5s ease-in-out infinite;
  }

  .process-label {
    min-width: 0;
    overflow-wrap: anywhere;
    white-space: normal;
  }

  @keyframes processMarkerPulse {
    50% {
      opacity: 0.35;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .conversation-process-row.streaming .process-marker::before {
      animation: none;
    }
  }

  @media (max-width: 560px) {
    .conversation-process-row {
      min-height: 30px;
    }
  }
</style>
