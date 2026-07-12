<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import ThinkingBlock from './ThinkingBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, isStreaming = false, filePreviewScope = undefined }: Props = $props();

  const thinkingContent = $derived(block.thinking?.content || block.content || '');

  const isComplete = $derived(block.thinking?.isComplete ?? !isStreaming);
  const shouldShowStreamingState = $derived(isStreaming && !isComplete);
</script>

<ThinkingBlock
  content={thinkingContent}
  isStreaming={shouldShowStreamingState}
  {filePreviewScope}
/>
