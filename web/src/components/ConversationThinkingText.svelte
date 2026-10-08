<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { Message, ThinkingSegment } from '../types/message';
  import MarkdownContent from './MarkdownContent.svelte';

  interface Props {
    message: Message;
    filePreviewScope?: FilePreviewScope;
  }

  let { message, filePreviewScope = undefined }: Props = $props();

  // 摘要风格的思考正文：阶段标题已经说明「思考中 / 已思考」，这里不再套一层带标题、
  // 边框和折叠按钮的卡片，只把思考文字按过程正文的样子缩进显示。
  const segments = $derived.by<ThinkingSegment[]>(() => {
    const result: ThinkingSegment[] = [];
    for (const block of message.blocks || []) {
      if (block && typeof block === 'object' && block.type === 'thinking' && block.thinking) {
        result.push(...block.thinking.segments.filter((segment) => segment.content.trim()));
      }
    }
    return result;
  });
  const fallbackText = $derived(
    segments.length === 0 && typeof message.content === 'string' ? message.content.trim() : '',
  );

  function segmentStreaming(segment: ThinkingSegment): boolean {
    return segment.status === 'pending' || segment.status === 'running';
  }
</script>

{#if segments.length > 0 || fallbackText}
  <div class="conversation-thinking-text">
    {#each segments as segment (segment.segmentId)}
      <div class="conversation-thinking-segment">
        <MarkdownContent
          content={segment.content.trim()}
          isStreaming={segmentStreaming(segment)}
          {filePreviewScope}
        />
      </div>
    {/each}
    {#if fallbackText}
      <div class="conversation-thinking-segment">
        <MarkdownContent content={fallbackText} isStreaming={false} {filePreviewScope} />
      </div>
    {/if}
  </div>
{/if}

<style>
  .conversation-thinking-text {
    min-width: 0;
    padding: 2px 0 4px;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.6;
  }

  .conversation-thinking-segment + .conversation-thinking-segment {
    margin-top: var(--space-2);
  }
</style>
