<script lang="ts">
  import type { TimelineRenderItem } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    item: TimelineRenderItem;
  }

  let { item }: Props = $props();

  const message = $derived(item.message);
  const isStreaming = $derived(message.isStreaming || message.metadata?.turnItemStatus === 'running');

  function plainText(value: string): string {
    return value
      .replace(/```[\s\S]*?```/gu, ' ')
      .replace(/[`*_>#-]/gu, ' ')
      .replace(/\s+/gu, ' ')
      .trim();
  }

  const detailText = $derived.by(() => {
    const directContent = typeof message.content === 'string' ? plainText(message.content) : '';
    if (directContent) return directContent;
    for (const block of message.blocks || []) {
      if (!block || typeof block !== 'object') continue;
      if (typeof block.content === 'string') {
        const content = plainText(block.content);
        if (content) return content;
      }
      if (block.type === 'thinking') {
        const content = block.thinking?.segments
          .map((segment) => plainText(segment.content))
          .filter(Boolean)
          .join(' ');
        if (content) return content;
      }
    }
    return '';
  });

  const label = $derived.by(() => {
    if (detailText) return detailText;
    const title = typeof message.metadata?.title === 'string' ? message.metadata.title.trim() : '';
    if (title) return title;
    if (message.type === 'thinking') return i18n.t('messageList.turnDisclosure.thinking');
    if (message.type === 'system-notice') return i18n.t('messageList.turnDisclosure.systemEvent');
    return i18n.t('messageList.turnDisclosure.processEvent');
  });

</script>

<div class="conversation-process-row" class:streaming={isStreaming} title={label}>
  <span class="process-marker" aria-hidden="true"></span>
  <span class="process-label">{label}</span>
</div>

<style>
  .conversation-process-row {
    display: flex;
    align-items: flex-start;
    min-height: 32px;
    gap: 7px;
    padding: 3px 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .conversation-process-row.streaming {
    color: var(--foreground);
  }

  .process-marker {
    position: relative;
    flex: 0 0 12px;
    width: 12px;
    height: 20px;
  }

  .process-marker::before {
    content: '';
    position: absolute;
    top: 8px;
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
</style>
