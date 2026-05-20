<!--
  Markdown 代码块 renderer — 包装 CodeBlock / PlainTextBlock。
  接收 @humanspeak/svelte-markdown 传入的 lang + text props。
  通过 context 获取父级 MarkdownContent 的 isStreaming 状态。

  分支判断与 CodeBlockRenderer 共用：无语言（或显式 'text'）→ 轻量 PlainTextBlock；
  有具体语言 → 完整 CodeBlock（带 header / 行号 / hljs 高亮）。
-->
<script lang="ts">
  import { getContext } from 'svelte';
  import CodeBlock from '../CodeBlock.svelte';
  import PlainTextBlock from '../PlainTextBlock.svelte';

  interface Props {
    lang: string;
    text: string;
  }
  const { lang, text }: Props = $props();

  const streamingCtx = getContext<{ readonly isStreaming: boolean }>('markdown-streaming');
  const isStreaming = $derived(streamingCtx?.isStreaming ?? false);

  const normalizedLanguage = $derived((lang || '').trim().toLowerCase());
  const usePlainTextRenderer = $derived(!normalizedLanguage || normalizedLanguage === 'text');
</script>

{#if usePlainTextRenderer}
  <PlainTextBlock content={text} />
{:else}
  <CodeBlock
    code={text}
    language={lang}
    showLineNumbers={lang !== 'mermaid'}
    {isStreaming}
  />
{/if}
