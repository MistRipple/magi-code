<script lang="ts">
  import { onMount } from 'svelte';
  import { marked, type Token, type Tokens } from 'marked';
  import hljs from 'highlight.js';
  import CodeBlock from './CodeBlock.svelte';

  // Props
  interface Props {
    content: string;
    isStreaming?: boolean;
  }
  let { content, isStreaming = false }: Props = $props();

  // 内容段落类型
  type ContentSegment =
    | { type: 'markdown'; html: string }
    | { type: 'code'; code: string; language: string };

  // 解析后的内容段落
  let segments = $state<ContentSegment[]>([]);

  // 配置 marked
  onMount(() => {
    marked.setOptions({
      breaks: true,
      gfm: true,
    });
  });

  // 解析内容为段落
  $effect(() => {
    if (!content) {
      segments = [];
      return;
    }

    try {
      // 使用 marked.lexer 解析 markdown 为 tokens
      const tokens = marked.lexer(content);
      const result: ContentSegment[] = [];
      let pendingTokens: Token[] = [];

      // 将待处理的 tokens 渲染为 HTML
      function flushPendingTokens() {
        if (pendingTokens.length > 0) {
          const html = marked.parser(pendingTokens as Token[]);
          if (html.trim()) {
            result.push({ type: 'markdown', html });
          }
          pendingTokens = [];
        }
      }

      // 遍历 tokens，提取代码块
      for (const token of tokens) {
        if (token.type === 'code') {
          const codeToken = token as Tokens.Code;
          const lang = (codeToken.lang || '').toLowerCase();

          // 代码块：先渲染之前的 markdown，再添加代码段落
          flushPendingTokens();
          result.push({
            type: 'code',
            code: codeToken.text,
            language: lang,
          });
        } else {
          // 非代码块：累积到待处理 tokens
          pendingTokens.push(token);
        }
      }

      // 处理剩余的 tokens
      flushPendingTokens();

      segments = result;
    } catch (error) {
      console.error('[MarkdownContent] 解析错误:', error);
      segments = [{ type: 'markdown', html: `<p>${content}</p>` }];
    }
  });

  // 代码高亮处理
  $effect(() => {
    if (segments.length > 0 && !isStreaming) {
      setTimeout(() => {
        const codeBlocks = document.querySelectorAll('.markdown-content pre code:not(.hljs)');
        codeBlocks.forEach((block) => {
          hljs.highlightElement(block as HTMLElement);
        });
      }, 0);
    }
  });
</script>

<div class="markdown-content" class:streaming={isStreaming}>
  {#each segments as segment, i (`segment-${i}-${segment.type}`)}
    {#if segment.type === 'markdown'}
      {@html segment.html}
    {:else if segment.type === 'code'}
      <CodeBlock
        code={segment.code}
        language={segment.language}
        showLineNumbers={segment.language !== 'mermaid'}
      />
    {/if}
  {/each}
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

  .markdown-content :global(img) {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
</style>
