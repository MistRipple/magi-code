<script lang="ts">
  import { setContext } from 'svelte';
  import {
    FILE_PREVIEW_SCOPE_CONTEXT,
    type FilePreviewScope,
    type FilePreviewScopeReader,
  } from '../lib/file-reference';
  import { MARKDOWN_IMAGE_CONTEXT, type MarkdownImageContext } from '../lib/markdown-image';
  import { preprocessMarkdown, splitStreamingMarkdown } from '../lib/markdown-utils';
  import type { Component } from 'svelte';
  import {
    loadMarkdownRenderer,
    type MarkdownRendererProps,
  } from '../lib/markdown-renderer-loader';

  // Props — 保持既有调用方式兼容，并为文件预览补充图片基准路径。
  interface Props {
    content: string;
    isStreaming?: boolean;
    filePreviewScope?: FilePreviewScope;
    baseFilePath?: string;
  }
  let {
    content,
    isStreaming = false,
    filePreviewScope = undefined,
    baseFilePath = undefined,
  }: Props = $props();

  let MarkdownRendererComponent = $state<Component<MarkdownRendererProps> | null>(null);

  $effect(() => {
    void loadMarkdownRenderer()
      .then((module) => {
        MarkdownRendererComponent = module.default;
      })
      .catch((error) => {
        console.error('[markdown] 渲染器加载失败', error);
      });
  });

  const readFilePreviewScope: FilePreviewScopeReader = () => filePreviewScope;
  setContext(FILE_PREVIEW_SCOPE_CONTEXT, readFilePreviewScope);
  const markdownImageContext: MarkdownImageContext = {
    get baseFilePath() {
      return baseFilePath;
    },
    readFilePreviewScope,
  };
  setContext(MARKDOWN_IMAGE_CONTEXT, markdownImageContext);

  // 流式与完成态共用同一结构：完成态就是“全部内容都已稳定、没有易变尾段”。
  // 稳定段渲染器在流式结束时保持同一个实例，只是收到完整内容并原地更新，
  // 不会因为切换分支而整体销毁重建（那会导致闪烁、滚动跳动和选区丢失）。
  const markdownParts = $derived.by(() => {
    const source = content || '';
    if (!isStreaming) {
      return { stable: preprocessMarkdown(source, false), volatile: '' };
    }
    const parts = splitStreamingMarkdown(source);
    return {
      stable: preprocessMarkdown(parts.stable, false),
      volatile: preprocessMarkdown(parts.volatile, true),
    };
  });
</script>

<div class="markdown-content">
  {#if MarkdownRendererComponent}
    {#if markdownParts.stable}
      <MarkdownRendererComponent source={markdownParts.stable} isStreaming={false} />
    {/if}
    {#if markdownParts.volatile}
      <MarkdownRendererComponent source={markdownParts.volatile} isStreaming={true} />
    {/if}
  {:else}
    <pre class="markdown-fallback">{content}</pre>
  {/if}
</div>

<style>
  .markdown-content {
    min-width: 0;
    max-width: 100%;
    overflow-wrap: anywhere;
    color: var(--foreground);
    text-wrap: pretty;
  }

  .markdown-fallback {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font: inherit;
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
    font-weight: var(--font-semibold);
  }

  .markdown-content :global(strong),
  .markdown-content :global(b) { font-weight: var(--font-bold); }

  /* 标题字体大小适配消息内容，不宜过大 */
  .markdown-content :global(h1) { font-size: var(--text-lg); }
  .markdown-content :global(h2) { font-size: var(--text-md); }
  .markdown-content :global(h3) { font-size: var(--text-base); }

  .markdown-content :global(ul),
  .markdown-content :global(ol) {
    margin: var(--spacing-sm) 0;
    padding-inline-start: var(--spacing-lg);
  }

  /* Three-digit ordered markers must stay inside the same column as cards and prose. */
  .markdown-content :global(ol) {
    padding-inline-start: max(var(--spacing-lg), 4.5ch);
  }

  .markdown-content :global(li::marker) {
    font-variant-numeric: tabular-nums;
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

  .markdown-content :global(pre:not(.code-pre):not(.plain-text-block)) {
    margin: var(--spacing-sm) 0;
    padding: var(--spacing-md);
    box-sizing: border-box;
    max-width: 100%;
    overflow-wrap: normal;
    overflow-x: auto;
  }

  .markdown-content :global(code:not(.code-text):not(.plain-text-code):not(.diff-line-code)) {
    font-family: var(--font-mono);
    font-size: max(var(--text-xs), 0.9em);
  }

  .markdown-content :global(table) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    width: 100%;
    border-collapse: collapse;
    margin: var(--spacing-sm) 0;
  }

  .markdown-content :global(th),
  .markdown-content :global(td) {
    padding: var(--spacing-sm);
    border: var(--magi-hairline) solid var(--border);
    text-align: left;
  }

  .markdown-content :global(th) {
    background: var(--code-bg);
    font-weight: var(--font-semibold);
  }

  .markdown-content :global(hr) {
    border: none;
    border-top: var(--magi-hairline) solid var(--border);
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

  .markdown-content :global(.md-link-external) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 17px;
    height: 17px;
    margin: 0 1px 0 2px;
    padding: 0;
    border: 0;
    border-radius: 3px;
    background: transparent;
    color: var(--foreground-muted);
    vertical-align: -0.125em;
    cursor: pointer;
    opacity: .68;
  }

  .markdown-content :global(.md-link-external:hover),
  .markdown-content :global(.md-link-external:focus-visible) {
    background: var(--surface-2);
    color: var(--foreground);
    opacity: 1;
  }

  /* 内联代码样式 */
  .markdown-content :global(:not(pre) > code) {
    background: var(--code-bg, rgba(0,0,0,0.2));
    padding: 1px 4px;
    border-radius: var(--radius-sm, 3px);
  }

  .markdown-content :global(.katex-display) {
    max-width: 100%;
    overflow-x: auto;
    overflow-y: hidden;
  }

  .markdown-content :global(img) {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
</style>
