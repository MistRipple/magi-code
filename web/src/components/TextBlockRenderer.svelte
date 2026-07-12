<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import MarkdownContent from './MarkdownContent.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, isStreaming = false, filePreviewScope = undefined }: Props = $props();
</script>

<MarkdownContent content={block.content || ''} {isStreaming} {filePreviewScope} />
