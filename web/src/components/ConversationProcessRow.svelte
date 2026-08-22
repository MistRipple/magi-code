<script lang="ts">
  import type { TimelineRenderItem } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import type { IconName } from '../lib/icons';
  import Icon from './Icon.svelte';

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

  const iconName = $derived.by((): IconName => {
    if (message.type === 'thinking') return 'clock';
    if (message.type === 'system-notice') return 'info';
    if (message.type === 'task_card' || message.type === 'plan') return 'list';
    return 'terminal';
  });
</script>

<div class="conversation-process-row" class:streaming={isStreaming} title={label}>
  <span class="process-icon"><Icon name={iconName} size={15} /></span>
  <span class="process-label">{label}</span>
</div>

<style>
  .conversation-process-row {
    display: flex;
    align-items: flex-start;
    min-height: 32px;
    gap: 9px;
    padding: 3px 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .conversation-process-row.streaming {
    color: var(--foreground);
  }

  .process-icon {
    display: inline-flex;
    flex: 0 0 auto;
    margin-top: 3px;
    color: var(--primary);
  }

  .conversation-process-row:not(.streaming) .process-icon {
    color: var(--foreground-muted);
  }

  .process-label {
    display: -webkit-box;
    min-width: 0;
    overflow: hidden;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }
</style>
