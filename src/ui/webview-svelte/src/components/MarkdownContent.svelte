<script lang="ts">
  import { onMount } from 'svelte';
  import { marked } from 'marked';
  import hljs from 'highlight.js';

  // Props
  interface Props {
    content: string;
    isStreaming?: boolean;
  }
  let { content, isStreaming = false }: Props = $props();

  // 渲染后的 HTML
  let renderedHtml = $state('');

  // 配置 marked
  onMount(() => {
    marked.setOptions({
      breaks: true,
      gfm: true,
    });
  });

  // 响应式渲染 Markdown
  $effect(() => {
    // 依赖 content 变化
    if (content) {
      try {
        // 同步解析 Markdown
        const result = marked.parse(content, { async: false });
        renderedHtml = typeof result === 'string' ? result : '';
      } catch (error) {
        console.error('[MarkdownContent] 解析错误:', error);
        renderedHtml = `<p>${content}</p>`;
      }
    } else {
      renderedHtml = '';
    }
  });

  // 代码高亮处理
  $effect(() => {
    // 当 renderedHtml 变化后，对代码块进行高亮
    if (renderedHtml && !isStreaming) {
      // 使用 tick 或 setTimeout 确保 DOM 更新后执行
      setTimeout(() => {
        const codeBlocks = document.querySelectorAll('pre code:not(.hljs)');
        codeBlocks.forEach((block) => {
          hljs.highlightElement(block as HTMLElement);
        });
      }, 0);
    }
  });
</script>

<div class="markdown-content" class:streaming={isStreaming}>
  {@html renderedHtml}
</div>

<style>
  .markdown-content {
    color: var(--foreground);
  }

  /* 流式状态下禁用某些动画以提高性能 */
  .markdown-content.streaming :global(*) {
    animation: none !important;
    transition: none !important;
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

  .markdown-content :global(h1) { font-size: 1.5em; }
  .markdown-content :global(h2) { font-size: 1.3em; }
  .markdown-content :global(h3) { font-size: 1.1em; }

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

  .markdown-content :global(img) {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
</style>

