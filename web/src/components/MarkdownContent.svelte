<script lang="ts">
  import { setContext } from 'svelte';
  import SvelteMarkdown from '@humanspeak/svelte-markdown';
  import { preprocessMarkdown } from '../lib/markdown-utils';
  import MdCodeBlock from './renderers/MdCodeBlock.svelte';
  import MdLink from './renderers/MdLink.svelte';
  import MdImage from './renderers/MdImage.svelte';

  // Props — 对外接口保持不变
  interface Props {
    content: string;
    isStreaming?: boolean;
  }
  let { content, isStreaming = false }: Props = $props();

  // 通过 context 向子 renderer 组件传递 isStreaming 状态
  // 使用 getter 确保子组件总能获取最新值
  setContext('markdown-streaming', { get isStreaming() { return isStreaming; } });

  // 预处理后的 Markdown 源文本：$derived 同步派生，上游推多快就渲染多快。
  //
  // 历史：曾按消息长度阶梯降频（>5000 字 150ms / 2000 字 100ms / 500 字 50ms /
  // 短消息 16ms）以规避 marked AST 全量重解析的"O(N²) 算力黑洞"。但该方案让
  // 流式内容呈现为"块状蹦字"（最差 6.7fps），用户感知就是打字机。
  // 现代浏览器 + 新版 svelte-markdown + marked 解析 5000 字开销已降至个位数 ms，
  // 不再需要节流；如果未来真的复现长消息卡顿，应该上 Web Worker 或增量 parse，
  // 而不是把延迟丢回用户视野。
  const processedSource = $derived(preprocessMarkdown(content || '', isStreaming));

  // 自定义 renderer：覆盖代码块、链接、图片
  const renderers = {
    code: MdCodeBlock,
    link: MdLink,
    image: MdImage,
  };

  // marked 选项
  const options = {
    breaks: true,
    gfm: true,
  };
</script>

<div class="markdown-content">
  <SvelteMarkdown
    source={processedSource}
    {renderers}
    {options}
  />
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

