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
    | { type: 'markdown'; key: string; html: string }
    | { type: 'code'; key: string; code: string; language: string };

  // 解析后的内容段落
  let segments = $state<ContentSegment[]>([]);

  // 渲染控制：使用引用对象存储最新内容，彻底解决闭包旧值问题
  // 字符串是值传递，对象是引用传递。定时器读取 contentRef.val 永远是新的。
  const contentRef = { val: '' };

  // 单模式流式渲染：始终走 markdown 解析，只把调度收敛为逐帧刷新。
  let renderFrame: number | undefined;
  const streamState = {
    lastRaw: '',
    stableRaw: '',
    tailRaw: '',
    stableSegments: [] as ContentSegment[],
  };
  const MIN_STREAMING_BOUNDARY = 32;
  const TARGET_TAIL_LENGTH = 96;
  const MIN_TAIL_REMAINDER = 16;

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
      if (renderFrame !== undefined) cancelAnimationFrame(renderFrame);
      containerEl?.removeEventListener('click', handleLinkClick);
    };
  });

  function resetStreamingState() {
    streamState.lastRaw = '';
    streamState.stableRaw = '';
    streamState.tailRaw = '';
    streamState.stableSegments = [];
  }

  function parseContentSegments(text: string, streamingParse: boolean, keyPrefix: string, keyStart = 0): ContentSegment[] {
    if (!text) {
      return [];
    }

    const contentToParse = preprocessMarkdown(text, streamingParse);
    const tokens = marked.lexer(contentToParse);
    const result: ContentSegment[] = [];
    let pendingTokens: Token[] = [];
    let segmentIndex = keyStart;

    function flushPendingTokens() {
      if (pendingTokens.length > 0) {
        const html = marked.parser(pendingTokens as Token[]);
        if (html.trim()) {
          result.push({ type: 'markdown', key: `${keyPrefix}-md-${segmentIndex}`, html });
          segmentIndex += 1;
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
            key: `${keyPrefix}-code-${segmentIndex}`,
            code: codeToken.text,
            language: lang,
          });
          segmentIndex += 1;
        } else {
          pendingTokens.push(token);
        }
      } else {
        pendingTokens.push(token);
      }
    }

    flushPendingTokens();
    return result;
  }

  function findSafeStreamingBoundary(text: string): number {
    if (!text) {
      return 0;
    }

    const lines = text.split('\n');
    let offset = 0;
    let inFence = false;
    let fenceMarker = '';
    let safeBoundary = 0;
    let softBoundary = 0;

    for (const line of lines) {
      const lineStart = offset;
      const lineWithNewlineLength = line.length + 1;
      const lineEnd = Math.min(text.length, lineStart + lineWithNewlineLength);
      const trimmed = line.trim();
      const fenceMatch = line.match(/^ {0,3}(`{3,}|~{3,})/);

      if (fenceMatch) {
        const marker = fenceMatch[1];
        const markerChar = marker[0];
        if (!inFence) {
          inFence = true;
          fenceMarker = markerChar;
        } else if (fenceMarker === markerChar) {
          inFence = false;
          fenceMarker = '';
          safeBoundary = lineEnd;
        }
      }

      if (!inFence) {
        if (trimmed.length === 0) {
          safeBoundary = lineEnd;
        } else {
          const punctuationMatches = Array.from(line.matchAll(/[。！？.!?](?=(?:["'”’」』）》】\]\)]|\s|$))/g));
          if (punctuationMatches.length > 0) {
            const lastMatch = punctuationMatches[punctuationMatches.length - 1];
            safeBoundary = lineStart + lastMatch.index + lastMatch[0].length;
          }
          const softMatches = Array.from(line.matchAll(/(?:\s+|[，,、；;：:])(?=\S|$)/g));
          if (softMatches.length > 0) {
            const lastSoftMatch = softMatches[softMatches.length - 1];
            softBoundary = lineStart + lastSoftMatch.index + lastSoftMatch[0].length;
          }
        }
      }

      offset = lineEnd;
    }

    if (!inFence) {
      const preferredBoundaryLimit = text.length - MIN_TAIL_REMAINDER;
      const stableBoundaryTooFarBehind = safeBoundary > 0 && (text.length - safeBoundary) > TARGET_TAIL_LENGTH;
      if ((safeBoundary === 0 || stableBoundaryTooFarBehind)
        && softBoundary >= MIN_STREAMING_BOUNDARY
        && softBoundary <= preferredBoundaryLimit) {
        safeBoundary = softBoundary;
      }
    }

    return Math.max(0, Math.min(safeBoundary, text.length));
  }

  function renderStreamingMarkdown() {
    const raw = contentRef.val;
    if (!raw) {
      resetStreamingState();
      segments = [];
      return;
    }

    const isAppendOnly = streamState.lastRaw && raw.startsWith(streamState.lastRaw);
    if (!isAppendOnly) {
      resetStreamingState();
      streamState.tailRaw = raw;
    } else {
      streamState.tailRaw += raw.slice(streamState.lastRaw.length);
    }

    streamState.lastRaw = raw;

    const safeBoundary = findSafeStreamingBoundary(streamState.tailRaw);
    if (safeBoundary > 0) {
      const stableChunk = streamState.tailRaw.slice(0, safeBoundary);
      const stableChunkSegments = parseContentSegments(
        stableChunk,
        false,
        'stable',
        streamState.stableSegments.length,
      );
      if (stableChunkSegments.length > 0) {
        streamState.stableSegments = streamState.stableSegments.concat(stableChunkSegments);
      }
      streamState.stableRaw += stableChunk;
      streamState.tailRaw = streamState.tailRaw.slice(safeBoundary);
    }

    const tailSegments = parseContentSegments(streamState.tailRaw, true, 'tail');
    segments = streamState.stableSegments.concat(tailSegments);
  }

  function renderMarkdown() {
    const text = contentRef.val;
    if (!text) {
      segments = [];
      return;
    }

    try {
      resetStreamingState();
      segments = parseContentSegments(text, false, 'full');
    } catch (error) {
      console.error('[MarkdownContent] 解析错误:', error);
      segments = [{ type: 'markdown', key: 'fallback-md-0', html: `<p>${escapeAttr(text)}</p>` }];
    }
  }

  // 统一响应逻辑
  $effect(() => {
    // 1. 同步最新内容到引用对象 (同步操作，极快)
    contentRef.val = content || '';

    // 2. 决策渲染时机
    if (!isStreaming || !contentRef.val) {
      if (renderFrame !== undefined) {
        cancelAnimationFrame(renderFrame);
        renderFrame = undefined;
      }
      renderMarkdown();
      return;
    }

    if (renderFrame === undefined) {
      renderFrame = requestAnimationFrame(() => {
        renderStreamingMarkdown();
        renderFrame = undefined;
      });
    }
  });
</script>

<div class="markdown-content" class:streaming={isStreaming} bind:this={containerEl}>
  {#each segments as segment (segment.key)}
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
