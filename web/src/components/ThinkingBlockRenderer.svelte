<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import ThinkingBlock from './ThinkingBlock.svelte';

  interface Props {
    block: ContentBlock;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, filePreviewScope = undefined }: Props = $props();

  const group = $derived.by(() => {
    if (!block.thinking) {
      throw new Error(`[ThinkingBlockRenderer] block ${block.id} 缺少 ThinkingGroup`);
    }
    return block.thinking;
  });
</script>

<ThinkingBlock
  {group}
  {filePreviewScope}
/>
