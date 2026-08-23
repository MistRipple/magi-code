<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import type { FilePreviewScope } from '../lib/file-reference';
  import { getBlockRenderer } from '../lib/block-registry';
  import type { ConversationPresentationRole } from '../lib/conversation-presentation';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
    filePreviewScope?: FilePreviewScope;
    presentationRole?: ConversationPresentationRole;
  }

  let {
    block,
    isStreaming = false,
    readOnly = false,
    filePreviewScope = undefined,
    presentationRole = 'process',
  }: Props = $props();

  // 🔧 防御性检查：确保 block 有效且有 type 属性
  const isValidBlock = $derived(block && typeof block === 'object' && 'type' in block);
  const Renderer = $derived(isValidBlock ? getBlockRenderer(block) : null);
</script>

{#if Renderer}
  <Renderer {block} {isStreaming} {readOnly} {filePreviewScope} {presentationRole} />
{/if}
