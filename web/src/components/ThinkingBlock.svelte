<script lang="ts">
  import { untrack } from 'svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { ThinkingBlock as ThinkingGroup, ThinkingSegment } from '../types/message';
  import Icon from './Icon.svelte';
  import ToolCardMeta from './ToolCardMeta.svelte';
  import Disclosure from './Disclosure.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { resolveThinkingTitle } from '../lib/thinking-title';

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

  const title = $derived(resolveThinkingTitle(group, i18n.t.bind(i18n)));

  function isSegmentStreaming(segment: ThinkingSegment): boolean {
    return segment.status === 'pending' || segment.status === 'running';
  }

  const visualStatus = $derived(group.isStreaming ? 'running' : group.status === 'failed' ? 'error' : group.status === 'blocked' ? 'pending' : group.status === 'cancelled' ? 'cancelled' : 'success');
  const instanceId = $props.id();
  const contentId = `thinking-${instanceId}`;

  function toggle() {
    collapsed = !collapsed;
  }
</script>

<div
  class="message-card thinking-block"
  class:collapsed
  class:streaming={group.isStreaming}
  class:failed={group.status === 'failed'}
  class:interrupted={group.status === 'blocked' || group.status === 'cancelled'}
  data-thinking-group-id={group.groupId}
  data-thinking-segment-count={group.segments.length}
>
  <button class="thinking-header" onclick={toggle} aria-expanded={!collapsed} aria-controls={contentId}>
    <span class="thinking-icon">
      <Icon name="clock" size={14} />
    </span>

    <span class="thinking-title">{title}</span>
    <ToolCardMeta status={visualStatus} label={title} expanded={!collapsed} />
  </button>

  <Disclosure expanded={!collapsed}>
    <div class="thinking-content" id={contentId}>
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
  </Disclosure>
</div>

<style>
  .thinking-block {
    margin-top: var(--space-2);

    overflow: hidden;
  }

  .thinking-header:hover {
    background: var(--surface-hover);
  }

  .thinking-icon {
    display: flex;
    color: var(--foreground-muted);
  }

  .thinking-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .thinking-content {
    border-top: var(--magi-hairline) solid var(--border);
    background: transparent;
  }

  .thinking-segment {
    padding: var(--space-3);
  }

  .thinking-segment + .thinking-segment {
    border-top: var(--magi-hairline) solid color-mix(in srgb, var(--border) 72%, transparent);
  }

  .thinking-body {
    font-size: var(--text-sm);
    line-height: 1.6;
    color: var(--foreground-muted);
  }

</style>
