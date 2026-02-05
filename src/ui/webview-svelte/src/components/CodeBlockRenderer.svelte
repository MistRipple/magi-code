<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import CodeBlock from './CodeBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block, isStreaming = false, readOnly = false }: Props = $props();
</script>

<CodeBlock
  code={block.content || ''}
  language={block.language || ''}
  showLineNumbers={true}
  {isStreaming}
  showCopyButton={!readOnly}
/>
