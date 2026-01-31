<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import { getBlockRenderer } from '../lib/block-registry';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
  }

  let { block, isStreaming = false }: Props = $props();

  // 🔧 防御性检查：确保 block 有效且有 type 属性
  const isValidBlock = $derived(block && typeof block === 'object' && 'type' in block);
  const Renderer = $derived(isValidBlock ? getBlockRenderer(block) : null);
</script>

{#if Renderer}
  <Renderer {block} {isStreaming} />
{/if}
