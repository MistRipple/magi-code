<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { i18n } from '../stores/i18n.svelte';

  // Props
  interface Props {
    content: string;
    isStreaming?: boolean;
    initialExpanded?: boolean;
  }

  let {
    content,
    isStreaming = false,
    initialExpanded
  }: Props = $props();

  // 折叠状态只由初始配置决定；流式输出期间也允许用户手动展开/折叠
  let collapsed = $state(untrack(() => !(initialExpanded ?? false)));

  const thinkingContent = $derived((content ?? '').trim());

  // 单一标题文案：流式时显示「思考中...」，完成后显示「思考已完成」。
  // 之前并存「固定标题 + 内容摘要」两层，信息冗余且摘要在长思考输出里读起来割裂，
  // 现在按状态收敛到一行——读者只关心"还在思考 / 已经思考完"两种状态。
  const title = $derived(
    isStreaming
      ? i18n.t('thinkingBlock.streamingTitle')
      : i18n.t('thinkingBlock.completedTitle'),
  );

  function toggle() {
    collapsed = !collapsed;
  }
</script>

<div
  class="thinking-block"
  class:collapsed
  class:streaming={isStreaming}
>
  <button class="thinking-header" onclick={toggle}>
    <span class="chevron">
      <Icon name="chevron-right" size={12} />
    </span>

    <span class="thinking-icon">
      <Icon name="clock" size={14} />
    </span>

    <span class="thinking-title">{title}</span>
  </button>

  {#if !collapsed}
    <div class="thinking-content">
      <div class="thinking-body">
        <MarkdownContent content={thinkingContent} {isStreaming} />
      </div>
    </div>
  {/if}
</div>

<style>
  .thinking-block {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin-top: var(--space-2);
    background: rgba(139, 92, 246, 0.05);
    overflow: hidden;
  }

  .thinking-block.streaming {
    border-color: #a855f7;
    box-shadow: 0 0 0 1px rgba(168, 85, 247, 0.2);
  }

  /* header 高度/padding/字号/accent 条/chevron 等共享规范见 styles/tool-card.css；
     ThinkingBlock 特有：hover 用紫色 brand 而非通用 surface-hover */
  .thinking-header:hover {
    background: rgba(139, 92, 246, 0.1);
  }

  /* icon 保留紫色 brand 作为思考类型识别色（accent 条已用同色，形成视觉呼应） */
  .thinking-icon {
    display: flex;
    color: #a855f7;
  }

  .thinking-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .thinking-content {
    padding: var(--space-3);
    border-top: 1px solid var(--border);
    background: rgba(139, 92, 246, 0.02);
    /* 🔧 移除固定高度限制，让内容自然撑开 */
    /* max-height: 400px; */
    /* overflow-y: auto; */
    animation: expandContent 0.2s ease-out;
  }

  @keyframes expandContent {
    from { opacity: 0; transform: translateY(-8px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .thinking-body {
    font-size: var(--text-sm);
    line-height: 1.6;
    color: var(--foreground-muted);
  }

  /* 流式动画 */
  .streaming .thinking-icon {
    animation: spin 2s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
