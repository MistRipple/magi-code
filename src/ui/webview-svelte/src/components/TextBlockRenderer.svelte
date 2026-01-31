<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import MarkdownContent from './MarkdownContent.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
  }

  let { block, isStreaming = false }: Props = $props();
</script>

<MarkdownContent content={block.content || ''} {isStreaming} />
