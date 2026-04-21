<script lang="ts">
  import type { ContentBlock } from '../types/message';
  import CodeBlock from './CodeBlock.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block, isStreaming = false, readOnly = false }: Props = $props();
  const normalizedLanguage = $derived((block.language || '').trim().toLowerCase());
  const usePlainTextRenderer = $derived(!normalizedLanguage || normalizedLanguage === 'text');
</script>

{#if usePlainTextRenderer}
  <pre class="plain-text-block"><code>{block.content || ''}</code></pre>
{:else}
  <CodeBlock
    code={block.content || ''}
    language={block.language || ''}
    showLineNumbers={true}
    {isStreaming}
    showCopyButton={!readOnly}
  />
{/if}

<style>
  .plain-text-block {
    margin: var(--spacing-sm) 0;
    padding: var(--space-3);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--assistant-message-bg) 88%, var(--surface) 12%);
    border: 1px solid var(--border);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: var(--font-family-mono, 'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace);
    font-size: var(--text-sm);
    line-height: 1.6;
  }

  .plain-text-block code {
    font-family: inherit;
  }
</style>
