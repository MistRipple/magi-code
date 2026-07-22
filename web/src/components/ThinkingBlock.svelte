<script lang="ts">
  import { untrack } from 'svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { ThinkingBlock as ThinkingGroup, ThinkingSegment } from '../types/message';
  import Icon from './Icon.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    group: ThinkingGroup;
    initialExpanded?: boolean;
    filePreviewScope?: FilePreviewScope;
  }

  let {
    group,
    initialExpanded,
    filePreviewScope = undefined,
  }: Props = $props();

  // 折叠状态只由初始配置决定；流式输出期间也允许用户手动展开/折叠
  let collapsed = $state(untrack(() => !(initialExpanded ?? false)));

  const title = $derived.by(() => {
    if (group.status === 'failed') return i18n.t('thinkingBlock.failedTitle');
    if (group.status === 'blocked' || group.status === 'cancelled') {
      return i18n.t('thinkingBlock.interruptedTitle');
    }
    return group.isStreaming
      ? i18n.t('thinkingBlock.streamingTitle')
      : i18n.t('thinkingBlock.completedTitle');
  });

  function isSegmentStreaming(segment: ThinkingSegment): boolean {
    return segment.status === 'pending' || segment.status === 'running';
  }

  function toggle() {
    collapsed = !collapsed;
  }
</script>

<div
  class="thinking-block"
  class:collapsed
  class:streaming={group.isStreaming}
  class:failed={group.status === 'failed'}
  class:interrupted={group.status === 'blocked' || group.status === 'cancelled'}
  data-thinking-group-id={group.groupId}
  data-thinking-segment-count={group.segments.length}
>
  <button class="thinking-header" onclick={toggle}>
    <span class="chevron">
      <Icon name="chevron-right" size={12} />
    </span>

    <span class="thinking-icon">
      <Icon name="clock" size={14} />
    </span>

    <span class="thinking-title">{title}</span>
  </button>

  {#if !collapsed}
    <div class="thinking-content">
      {#each group.segments as segment (segment.segmentId)}
        <div class="thinking-segment" data-thinking-segment-id={segment.segmentId}>
          {#if segment.content.trim()}
            <div class="thinking-body">
              <MarkdownContent
                content={segment.content.trim()}
                isStreaming={isSegmentStreaming(segment)}
                {filePreviewScope}
              />
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .thinking-block {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin-top: var(--space-2);
    background: rgba(139, 92, 246, 0.05);
    overflow: hidden;
  }

  .thinking-block.streaming {
    border-color: #a855f7;
    box-shadow: 0 0 0 1px rgba(168, 85, 247, 0.2);
  }

  .thinking-block.failed {
    border-color: color-mix(in srgb, var(--error) 55%, var(--border));
  }

  .thinking-block.interrupted {
    border-color: color-mix(in srgb, var(--warning) 55%, var(--border));
  }

  /* header 高度/padding/字号/accent 条/chevron 等共享规范见 styles/tool-card.css；
     ThinkingBlock 特有：hover 用紫色 brand 而非通用 surface-hover */
  .thinking-header:hover {
    background: rgba(139, 92, 246, 0.1);
  }

  /* icon 保留紫色 brand 作为思考类型识别色（accent 条已用同色，形成视觉呼应） */
  .thinking-icon {
    display: flex;
    color: #a855f7;
  }

  .thinking-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .thinking-content {
    border-top: 1px solid var(--border);
    background: rgba(139, 92, 246, 0.02);
    animation: expandContent 0.2s ease-out;
  }

  .thinking-segment {
    padding: var(--space-3);
  }

  .thinking-segment + .thinking-segment {
    border-top: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
  }

  @keyframes expandContent {
    from { opacity: 0; transform: translateY(-8px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .thinking-body {
    font-size: var(--text-sm);
    line-height: 1.6;
    color: var(--foreground-muted);
  }

  /* 流式动画 */
  .streaming .thinking-icon {
    animation: spin 2s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
