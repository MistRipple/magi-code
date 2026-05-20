<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import CodeBlock from './CodeBlock.svelte';
  import PlainTextBlock from './PlainTextBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block, isStreaming = false, readOnly = false }: Props = $props();
  const normalizedLanguage = $derived((block.language || '').trim().toLowerCase());
  // 无语言或显式 'text' → 退化到 PlainTextBlock 轻量渲染；
  // 与 MdCodeBlock 共用同一判断，避免双路径重复实现。
  const usePlainTextRenderer = $derived(!normalizedLanguage || normalizedLanguage === 'text');
</script>

{#if usePlainTextRenderer}
  <PlainTextBlock content={block.content || ''} />
{:else}
  <CodeBlock
    code={block.content || ''}
    language={block.language || ''}
    showLineNumbers={true}
    {isStreaming}
    showCopyButton={!readOnly}
  />
{/if}
