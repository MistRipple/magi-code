<!--
  Markdown 代码块 renderer — 包装现有 CodeBlock 组件
  接收 @humanspeak/svelte-markdown 传入的 lang + text props
  通过 context 获取父级 MarkdownContent 的 isStreaming 状态
-->
<script lang="ts">
  import { getContext } from 'svelte';
  import CodeBlock from '../CodeBlock.svelte';

  interface Props {
    lang: string;
    text: string;
  }
  const { lang, text }: Props = $props();

  // 从 MarkdownContent 的 context 中获取 isStreaming 状态
  const streamingCtx = getContext<{ readonly isStreaming: boolean }>('markdown-streaming');
  const isStreaming = $derived(streamingCtx?.isStreaming ?? false);
</script>

<CodeBlock
  code={text}
  language={lang}
  showLineNumbers={lang !== 'mermaid'}
  {isStreaming}
/>
