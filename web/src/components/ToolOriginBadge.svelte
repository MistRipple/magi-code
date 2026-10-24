<script lang="ts">
  /**
   * 工具卡片上的来源标签：这次工具调用不是 Magi 自己的 agent 发起的。
   * Magi 自己的调用不显示标签；GPT Web 网页里的模型经 MCP 回调的工具显示「ChatGPT」。
   */
  import type { ToolOrigin } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';

  let { origin }: { origin?: ToolOrigin } = $props();
</script>

{#if origin}
  <span class="tool-origin" data-testid="tool-origin" data-origin={origin} title={i18n.t(`toolOrigin.${origin}.title`)}>
    {i18n.t(`toolOrigin.${origin}`)}
  </span>
{/if}

<style>
  .tool-origin {
    flex: 0 0 auto;
    padding: 0 6px;
    border: var(--magi-hairline) solid var(--border);
    border-radius: 9px;
    font-size: var(--text-xs);
    line-height: 16px;
    color: var(--foreground-muted);
    white-space: nowrap;
  }
</style>
