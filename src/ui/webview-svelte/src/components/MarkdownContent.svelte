<script lang="ts">
  import { onMount } from 'svelte';
  import { marked, type Token, type Tokens } from 'marked';
  import CodeBlock from './CodeBlock.svelte';
  import { preprocessMarkdown } from '../lib/markdown-utils';
  import { vscode } from '../lib/vscode-bridge';

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

  // 渲染控制：使用引用对象存储最新内容，彻底解决闭包旧值问题
  // 字符串是值传递，对象是引用传递。定时器读取 contentRef.val 永远是新的。
  const contentRef = { val: '' };

  // 节流控制
  let lastRenderTime = 0;
  let renderTimer: ReturnType<typeof setTimeout> | undefined;

  // 参考 Augment 的自定义 renderer 方案：
  // 通过 marked.use() 配置自定义 renderer，控制链接、图片等元素的 HTML 输出
  // 而非 Augment 的全量 Token 组件化（改造量过大），用 renderer 覆盖达到同等效果
  const renderer: Parameters<typeof marked.use>[0]['renderer'] = {
    // 链接：在 webview 中通过 postMessage 打开，避免直接导航
    link({ href, title, tokens }) {
      const text = this.parser.parseInline(tokens);
      const safeHref = escapeAttr(href || '');
      const titleAttr = title ? ` title="${escapeAttr(title)}"` : '';
      return `<a href="${safeHref}" class="md-link" data-href="${safeHref}"${titleAttr}>${text}</a>`;
    },
    // 图片：限制 src、添加 loading=lazy
    image({ href, title, text }) {
      const safeHref = escapeAttr(href || '');
      const safAlt = escapeAttr(text || '');
      const titleAttr = title ? ` title="${escapeAttr(title)}"` : '';
      return `<img src="${safeHref}" alt="${safAlt}"${titleAttr} loading="lazy" />`;
    },
  };

  function escapeAttr(s: string): string {
    return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }

  // 初始化 marked 配置（模块级，确保首次渲染也生效）
  marked.setOptions({ breaks: true, gfm: true });
  marked.use({ renderer });

  let containerEl: HTMLDivElement;

  onMount(() => {
    // 事件委托：处理 markdown 中链接的点击（参考 Augment 的 os() 链接分发）
    function handleLinkClick(e: MouseEvent) {
      const target = (e.target as HTMLElement).closest('a.md-link') as HTMLAnchorElement | null;
      if (!target) return;
      e.preventDefault();
      const href = target.getAttribute('data-href') || target.href;
      if (href) {
        vscode.postMessage({ type: 'openLink', url: href });
      }
    }
    containerEl?.addEventListener('click', handleLinkClick);

    return () => {
      if (renderTimer) clearTimeout(renderTimer);
      containerEl?.removeEventListener('click', handleLinkClick);
    };
  });

  function doRender() {
    const text = contentRef.val;
    if (!text) {
      segments = [];
      return;
    }

    try {
      const contentToParse = preprocessMarkdown(text, isStreaming);
      const tokens = marked.lexer(contentToParse);
      const result: ContentSegment[] = [];
      let pendingTokens: Token[] = [];

      function flushPendingTokens() {
        if (pendingTokens.length > 0) {
          const html = marked.parser(pendingTokens as Token[]);
          if (html.trim()) {
            result.push({ type: 'markdown', html });
          }
          pendingTokens = [];
        }
      }

      for (const token of tokens) {
        if (token.type === 'code') {
          const codeToken = token as Tokens.Code;
          const isFenced = /^ {0,3}(`{3,}|~{3,})/.test(token.raw);

          if (isFenced) {
            const lang = (codeToken.lang || '').toLowerCase();
            flushPendingTokens();
            result.push({
              type: 'code',
              code: codeToken.text,
              language: lang,
            });
          } else {
            pendingTokens.push(token);
          }
        } else {
          pendingTokens.push(token);
        }
      }

      flushPendingTokens();
      segments = result;
    } catch (error) {
      console.error('[MarkdownContent] 解析错误:', error);
      segments = [{ type: 'markdown', html: `<p>${text}</p>` }];
    }
  }

  // 统一响应逻辑
  $effect(() => {
    // 1. 同步最新内容到引用对象 (同步操作，极快)
    contentRef.val = content || '';

    // 2. 决策渲染时机
    if (!isStreaming || !contentRef.val) {
      // 非流式或空内容：立即渲染
      if (renderTimer) {
        clearTimeout(renderTimer);
        renderTimer = undefined;
      }
      doRender();
      return;
    }

    // 流式：节流控制
    const now = Date.now();
    const dynamicDelay = Math.min(100, 32 + Math.floor(contentRef.val.length / 500) * 5);

    // 如果已经有定时器在跑，说明已经安排了下一次渲染，无需操作
    // 定时器执行时会读取 contentRef.val 的最新值
    if (!renderTimer) {
      if (now - lastRenderTime >= dynamicDelay) {
        // 距离上次渲染已够久，立即执行
        doRender();
        lastRenderTime = now;
      } else {
        // 还没到时间，安排延后执行
        renderTimer = setTimeout(() => {
          doRender();
          lastRenderTime = Date.now();
          renderTimer = undefined;
        }, dynamicDelay - (now - lastRenderTime));
      }
    }
  });
</script>

<div class="markdown-content" class:streaming={isStreaming} bind:this={containerEl}>
  {#each segments as segment, i (`segment-${i}-${segment.type}`)}
    {#if segment.type === 'markdown'}
      {@html segment.html}
    {:else if segment.type === 'code'}
      <CodeBlock
        code={segment.code}
        language={segment.language}
        showLineNumbers={segment.language !== 'mermaid'}
        isStreaming={isStreaming}
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
