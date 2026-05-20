<!--
  无语言（或显式 'text'）code fence 的轻量渲染容器。

  设计原因：完整的 CodeBlock 组件带 header（chevron / icon / 语言标签 / 复制按钮）+ 行号 + hljs 高亮，
  视觉重量很大。当模型回复连续输出多个无语言 code fence（典型场景：描述工具调用示例、
  贴命令输出、贴文件路径）时，每个块都被包成完整 CodeBlock 会形成密集的"CODE"卡片噪音。

  约定：调用方负责判断 language 是否为空或 'text'；本组件只负责渲染。
  与 CodeBlockRenderer / MdCodeBlock 两条入口路径共用，避免同功能多实现。
-->
<script lang="ts">
  interface Props {
    content: string;
  }

  let { content }: Props = $props();
</script>

<pre class="plain-text-block"><code>{content || ''}</code></pre>

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
