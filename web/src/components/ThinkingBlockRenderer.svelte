<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import ThinkingBlock from './ThinkingBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
  }

  let { block, isStreaming = false }: Props = $props();

  // 🔧 修复：确保 thinking 内容在流式期间也能正确获取
  const thinkingContent = $derived(block.thinking?.content || block.content || '');

  const isComplete = $derived(block.thinking?.isComplete ?? !isStreaming);
  const shouldShowStreamingState = $derived(isStreaming && !isComplete);
</script>

<ThinkingBlock
  content={thinkingContent}
  isStreaming={shouldShowStreamingState}
/>
