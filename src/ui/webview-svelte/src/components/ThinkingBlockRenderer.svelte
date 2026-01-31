<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import ThinkingBlock from './ThinkingBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
  }

  let { block, isStreaming = false }: Props = $props();
  const thinkingContent = $derived(block.thinking?.content || block.content || '');
  const isComplete = $derived(block.thinking?.isComplete ?? !isStreaming);
</script>

<ThinkingBlock
  thinking={[{ content: thinkingContent }]}
  isStreaming={!isComplete}
  initialExpanded={false}
/>
