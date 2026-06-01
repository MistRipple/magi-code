<script lang="ts">
  import { setContext } from 'svelte';
  import {
    FILE_PREVIEW_SCOPE_CONTEXT,
    type FilePreviewScope,
    type FilePreviewScopeReader,
  } from '../lib/file-reference';
  import { preprocessMarkdown, splitStreamingMarkdown } from '../lib/markdown-utils';
  import MarkdownRenderer from './MarkdownRenderer.svelte';

  // Props — 对外接口保持不变
  interface Props {
    content: string;
    isStreaming?: boolean;
    filePreviewScope?: FilePreviewScope;
  }
  let { content, isStreaming = false, filePreviewScope = undefined }: Props = $props();

  const readFilePreviewScope: FilePreviewScopeReader = () => filePreviewScope;
  setContext(FILE_PREVIEW_SCOPE_CONTEXT, readFilePreviewScope);

  const markdownParts = $derived.by(() => {
    const source = content || '';
    if (!isStreaming) {
      return {
        isSplit: false,
        completed: preprocessMarkdown(source, false),
        stable: '',
        volatile: '',
      };
    }
    const parts = splitStreamingMarkdown(source);
    return {
      isSplit: true,
      completed: '',
      stable: preprocessMarkdown(parts.stable, false),
      volatile: preprocessMarkdown(parts.volatile, true),
    };
  });
</script>

<div class="markdown-content">
  {#if markdownParts.isSplit}
    {#if markdownParts.stable}
      <MarkdownRenderer source={markdownParts.stable} isStreaming={false} />
    {/if}
    {#if markdownParts.volatile}
      <MarkdownRenderer source={markdownParts.volatile} isStreaming={true} />
    {/if}
  {:else}
    <MarkdownRenderer source={markdownParts.completed} isStreaming={false} />
  {/if}
</div>

<style>
  .markdown-content {
    color: var(--foreground);
  }

  /* Markdown 元素样式 */
  .markdown-content :global(p) {
    margin: 0 0 var(--spacing-sm) 0;
  }

  .markdown-content :global(p:last-child) {
    margin-bottom: 0;
  }

  .markdown-content :global(h1),
  .markdown-content :global(h2),
  .markdown-content :global(h3),
  .markdown-content :global(h4) {
    margin: var(--spacing-md) 0 var(--spacing-sm) 0;
    font-weight: 600;
  }

  /* 标题字体大小适配消息内容，不宜过大 */
  .markdown-content :global(h1) { font-size: var(--text-lg); }
  .markdown-content :global(h2) { font-size: var(--text-md); }
  .markdown-content :global(h3) { font-size: var(--text-base); }

  .markdown-content :global(ul),
  .markdown-content :global(ol) {
    margin: var(--spacing-sm) 0;
    padding-left: var(--spacing-lg);
  }

  .markdown-content :global(li) {
    margin: var(--spacing-xs) 0;
  }

  .markdown-content :global(blockquote) {
    margin: var(--spacing-sm) 0;
    padding: var(--spacing-sm) var(--spacing-md);
    border-left: 3px solid var(--primary);
    background: var(--code-bg);
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  }

  .markdown-content :global(pre) {
    margin: var(--spacing-sm) 0;
    padding: var(--spacing-md);
    overflow-x: auto;
  }

  .markdown-content :global(code) {
    font-family: var(--font-mono);
    font-size: 0.9em;
  }

  .markdown-content :global(table) {
    width: 100%;
    border-collapse: collapse;
    margin: var(--spacing-sm) 0;
  }

  .markdown-content :global(th),
  .markdown-content :global(td) {
    padding: var(--spacing-sm);
    border: 1px solid var(--border);
    text-align: left;
  }

  .markdown-content :global(th) {
    background: var(--code-bg);
    font-weight: 600;
  }

  .markdown-content :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: var(--spacing-md) 0;
  }

  /* 链接样式 */
  .markdown-content :global(a.md-link) {
    color: var(--primary);
    text-decoration: none;
    cursor: pointer;
  }

  .markdown-content :global(a.md-link:hover) {
    text-decoration: underline;
  }

  /* 内联代码样式 */
  .markdown-content :global(:not(pre) > code) {
    background: var(--code-bg, rgba(0,0,0,0.2));
    padding: 1px 4px;
    border-radius: var(--radius-sm, 3px);
  }

  .markdown-content :global(img) {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
</style>
