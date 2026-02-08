<script lang="ts">
  import type { Message, PlaceholderState } from '../types/message';
  import type { IconName } from '../lib/icons';
  import MarkdownContent from './MarkdownContent.svelte';
  import WorkerBadge from './WorkerBadge.svelte';
  import SubTaskSummaryCard from './SubTaskSummaryCard.svelte';
  import InstructionCard from './InstructionCard.svelte';
  import BlockRenderer from './BlockRenderer.svelte';
  import Icon from './Icon.svelte';

  // Props
  interface Props {
    message: Message;
    readOnly?: boolean;
  }
  let { message, readOnly = false }: Props = $props();

  // 派生状态
  const isUser = $derived(message.type === 'user_input');
  const isNotice = $derived(message.type === 'system-notice' || message.type === 'error');
  const interactionMeta = $derived(message.metadata?.interaction as { prompt?: string; type?: string } | undefined);
  const isInteraction = $derived(Boolean(interactionMeta));
  const isStreaming = $derived(message.isStreaming);

  // 占位消息相关派生状态
  const isPlaceholder = $derived(Boolean(message.metadata?.isPlaceholder));
  const placeholderState = $derived((message.metadata?.placeholderState || 'pending') as PlaceholderState);
  const wasPlaceholder = $derived(Boolean(message.metadata?.wasPlaceholder));
  const justCompleted = $derived(Boolean(message.metadata?.justCompleted));
  const sendingAnimation = $derived(Boolean(message.metadata?.sendingAnimation));

  // 过滤无效的 blocks
  const safeBlocks = $derived(
    (message.blocks || []).filter((b): b is import('../types/message').ContentBlock =>
      !!b && typeof b === 'object' && 'type' in b
    )
  );

  // 格式化时间戳
  function formatTime(timestamp: number): string {
    const date = new Date(timestamp);
    return date.toLocaleTimeString('zh-CN', {
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  // 获取 worker 信息（如果有）
  const worker = $derived(message.metadata?.worker || null);
  const badgeWorker = $derived(worker || (message.source === 'orchestrator' ? 'orchestrator' : message.source));

  // 子任务卡片消息，作为独立消息存在
  const isSubTaskCardOnly = $derived(message.type === 'task_card');

  // 任务说明消息（编排者派发给 Worker）
  const isInstruction = $derived(message.type === 'instruction');
  const instructionTargetWorker = $derived(
    (message.metadata?.worker || message.metadata?.agent) as string | undefined
  );

  // 通知类型和对应的图标/颜色（使用 Message 类型中的 noticeType）
  const noticeType = $derived(message.noticeType || 'info');
  const noticeIcons: Record<string, IconName> = {
    success: 'check-circle',
    error: 'x-circle',
    warning: 'alert-triangle',
    info: 'info'
  };
  const noticeColors: Record<string, string> = {
    success: 'var(--success)',
    error: 'var(--error)',
    warning: 'var(--warning)',
    info: 'var(--info)',
  };

  // 获取消息中的图片
  // 优先从 message.images，其次从 message.metadata?.images
  const messageImages = $derived(
    message.images || (message.metadata?.images as Array<{ dataUrl: string }>) || []
  );

  // 图片预览弹窗状态
  let showImagePreview = $state(false);
  let previewImageUrl = $state('');

  // 点击图片放大预览
  function openImagePreview(imageUrl: string) {
    previewImageUrl = imageUrl;
    showImagePreview = true;
  }

  // 关闭图片预览
  function closeImagePreview() {
    showImagePreview = false;
    previewImageUrl = '';
  }

  // 复制内容
  async function handleCopy() {
    if (!message.content) return;
    try {
      await navigator.clipboard.writeText(message.content);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }
</script>

<!-- 系统通知消息：居中显示 -->
{#if isNotice}
  <div class="system-notice {noticeType}">
    <span class="notice-icon" style="color: {noticeColors[noticeType] || noticeColors.info}">
      <Icon name={noticeIcons[noticeType] || 'info'} size={14} />
    </span>
    <span class="notice-text">{message.content}</span>
    <span class="notice-time">{formatTime(message.timestamp)}</span>
  </div>
<!-- 用户消息：简洁显示 -->
{:else if isUser}
  <div class="message-item user" class:sending={sendingAnimation} data-message-id={message.id} data-source="user">
    <!-- 用户上传的图片缩略图 -->
    {#if messageImages.length > 0}
      <div class="user-images">
        {#each messageImages as img, i (`${message.id}-img-${i}`)}
          <button class="user-image-thumb" onclick={() => openImagePreview(img.dataUrl)} type="button" title="点击放大">
            <img src={img.dataUrl} alt="附件图片 {i + 1}" />
          </button>
        {/each}
      </div>
    {/if}
    <div class="user-content">{message.content}</div>
    <div class="user-time">{formatTime(message.timestamp)}</div>
  </div>
<!-- subTaskCard 消息：直接显示卡片，不需要外层包裹 -->
{:else if isSubTaskCardOnly}
  <div class="message-item subtask-card-only" data-message-id={message.id} data-source={message.source}>
    <SubTaskSummaryCard card={message.metadata?.subTaskCard as any} {readOnly} />
  </div>
<!-- WORKER_INSTRUCTION 消息：任务说明卡片 -->
{:else if isInstruction}
  <div class="message-item instruction-card-only" data-message-id={message.id} data-source={message.source}>
    <InstructionCard
      content={message.content}
      targetWorker={instructionTargetWorker}
      isStreaming={isStreaming}
    />
  </div>
<!-- 助手消息：完整显示（包括占位状态） -->
{:else}
  <div
    class="message-item assistant"
    class:streaming={isStreaming}
    class:placeholder={isPlaceholder}
    class:was-placeholder={wasPlaceholder}
    class:just-completed={justCompleted}
    data-message-id={message.id}
    data-source={message.source}
    data-interaction={isInteraction ? 'true' : 'false'}
    data-placeholder-state={isPlaceholder ? placeholderState : undefined}
  >
    <div class="message-header">
      <div class="message-source">
        <!-- 只显示 WorkerBadge，不再重复显示 source-name -->
        <WorkerBadge worker={badgeWorker} size="sm" />
        <!-- 占位状态显示状态文字 -->
        {#if isPlaceholder}
          <span class="placeholder-status">
            {#if placeholderState === 'pending'}
              正在准备...
            {:else if placeholderState === 'received'}
              已接收...
            {:else if placeholderState === 'thinking'}
              正在思考...
            {:else}
              处理中...
            {/if}
          </span>
        {/if}
      </div>
      <div class="message-meta">
        <span class="message-time">{formatTime(message.timestamp)}</span>
        {#if !isStreaming && !isPlaceholder}
          <button class="copy-btn" onclick={handleCopy} title="复制内容">
            <Icon name="copy" size={12} />
          </button>
        {/if}
      </div>
    </div>

    <div class="message-content">
      {#if isPlaceholder}
        <!-- 占位状态：显示加载动画 -->
        <div class="placeholder-content">
          <div class="streaming-indicator-bottom">
            <span class="streaming-dot"></span>
            <span class="streaming-dot"></span>
            <span class="streaming-dot"></span>
          </div>
        </div>
      {:else}
        {#if isInteraction && interactionMeta?.prompt}
          <div class="interaction-inline">
            <Icon name="sparkles" size={14} />
            <span>{interactionMeta.prompt}</span>
          </div>
        {/if}

        {#if safeBlocks.length > 0}
            {#each safeBlocks as block, i (`${message.id}-block-${i}-${block.type}`)}
              <BlockRenderer {block} {isStreaming} {readOnly} />
            {/each}
          {:else if message.content}
            <MarkdownContent content={message.content} {isStreaming} />
          {/if}

          <!-- 流式消息底部加载指示器 -->
          {#if isStreaming}
            <div class="streaming-indicator-bottom">
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
            </div>
          {/if}
      {/if}
    </div>
  </div>
{/if}

<!-- 图片预览弹窗 -->
{#if showImagePreview}
  <button
    class="image-preview-overlay"
    onclick={closeImagePreview}
    onkeydown={(e) => e.key === 'Escape' && closeImagePreview()}
    type="button"
    aria-label="关闭图片预览"
  >
    <div class="image-preview-content" role="document">
      <span class="image-preview-close" aria-hidden="true">×</span>
      <img src={previewImageUrl} alt="图片预览" class="image-preview-img" />
    </div>
  </button>
{/if}

<style>
  /* ===== 系统通知样式（与HTML版本一致，简洁无背景） ===== */
  .system-notice {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 4px 12px;
    margin: 2px auto;
    font-size: var(--text-sm);
    width: fit-content;
    max-width: 90%;
  }
  .system-notice.info { color: var(--info); }
  .system-notice.success { color: var(--success); }
  .system-notice.warning { color: var(--warning); }
  .system-notice.error { color: var(--error); }
  .notice-icon {
    display: flex;
    flex-shrink: 0;
    width: 14px;
    height: 14px;
  }
  .notice-text {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .notice-time {
    font-size: 10px;
    opacity: 0.6;
    margin-left: 4px;
  }

  /* ===== 用户消息样式（简洁） ===== */
  .message-item.user {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    padding: var(--space-3) var(--space-4);
    margin-left: auto;
    max-width: 85%;
  }
  .user-content {
    background: var(--primary);
    color: white;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius-sm) var(--radius-lg);
    font-size: var(--text-base);
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
  }
  .user-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: var(--space-1);
  }

  /* ===== SubTaskCard 独立样式（与 assistant 消息保持一致的间距） ===== */
  .message-item.subtask-card-only {
    margin-right: var(--space-2);  /* 与 assistant 消息一致 */
  }

  /* ===== InstructionCard 独立样式（任务说明卡片） ===== */
  .message-item.instruction-card-only {
    margin-right: var(--space-2);  /* 与 assistant 消息一致 */
  }

  /* ===== 助手消息样式 ===== */
  .message-item.assistant {
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    margin-right: var(--space-2);  /* 减少右边距，配合 MessageList 的 padding-right 调整 */
    /* 平滑过渡：仅边框颜色和阴影，高度由内容自然撑开 */
    transition: border-color 0.2s ease, box-shadow 0.2s ease;
    /* 高度自适应：禁止压缩，确保高度随内容自动撑开 */
    flex-shrink: 0;
    height: auto;
    overflow: visible;
  }

  /* 流式消息卡片：高度完全由内容与动画驱动，避免占位感 */
  .message-item.assistant.streaming {
    min-height: 48px; /* 保持与 placeholder 高度一致，防止切换时塌陷 */
  }

  .interaction-inline {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    margin-bottom: var(--space-2);
    border-radius: var(--radius-md);
    background: var(--info-muted);
    border: 1px solid color-mix(in srgb, var(--info) 40%, transparent);
    font-size: var(--text-sm);
    color: var(--foreground);
  }
  .message-item.streaming { border-color: var(--info); }

  .message-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-3);
    font-size: var(--text-sm);
  }
  .message-source {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .message-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-left: auto;  /* 确保始终右对齐 */
  }
  .message-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }
  .copy-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    opacity: 0;
    transition: all var(--transition-fast);
  }
  .message-item:hover .copy-btn { opacity: 1; }
  .copy-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
  .message-content {
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
    overflow-wrap: break-word;
    font-size: var(--text-base);
    /* 确保内容区域高度由内容自然撑开 */
    min-height: 0;
    height: auto;
  }
  /* 移除流式消息的渐变遮罩，避免干扰视觉 */

  /* 占位→真实消息过渡动画 */
  .message-item.assistant.was-placeholder {
    animation: contentFadeIn 0.15s ease-out;
  }

  @keyframes contentFadeIn {
    from {
      opacity: 0.7;
    }
    to {
      opacity: 1;
    }
  }

  /* 完成动画：边框颜色过渡 */
  .message-item.assistant.just-completed {
    animation: completeBorderFade 0.3s ease-out;
  }

  @keyframes completeBorderFade {
    from {
      border-color: var(--info);
    }
    to {
      border-color: var(--border);
    }
  }

  /* 流式消息底部加载指示器：统一的三个点动画 */
  .streaming-indicator-bottom {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: var(--space-2) 0;
    margin-top: var(--space-2);
  }

  .streaming-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--info);
    opacity: 0.6;
    animation: streamingPulse 1.4s ease-in-out infinite;
  }

  .streaming-dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .streaming-dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes streamingPulse {
    0%, 80%, 100% {
      opacity: 0.4;
      transform: scale(1);
    }
    40% {
      opacity: 1;
      transform: scale(1.2);
    }
  }

  /* ===== 占位消息样式（统一在 assistant 卡片内） ===== */
  .message-item.assistant.placeholder {
    border-left: 3px solid var(--info);
  }

  .placeholder-status {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-style: italic;
  }

  .placeholder-content {
    min-height: 48px;
    display: flex;
    align-items: center;
    padding: var(--space-3) 0;
  }

  /* 占位消息的加载指示器：居左显示，无上边距 */
  .placeholder-content .streaming-indicator-bottom {
    margin-top: 0;
    padding: var(--space-2) 0;
  }

  /* ===== 用户消息图片缩略图 ===== */
  .user-images {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }

  .user-image-thumb {
    width: 80px;
    height: 80px;
    border-radius: 8px;
    overflow: hidden;
    cursor: pointer;
    border: 1px solid var(--border);
    background: var(--background);
    padding: 0;
    transition: transform 0.2s, box-shadow 0.2s;
  }

  .user-image-thumb:hover {
    transform: scale(1.05);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
  }

  .user-image-thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  /* ===== 图片预览弹窗 ===== */
  .image-preview-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.85);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 9999;
    cursor: pointer;
    border: none;
    padding: 0;
    margin: 0;
    width: 100vw;
    height: 100vh;
  }

  .image-preview-content {
    position: relative;
    max-width: 90vw;
    max-height: 90vh;
    cursor: default;
    pointer-events: none;
  }

  .image-preview-close {
    position: absolute;
    top: -40px;
    right: 0;
    background: transparent;
    border: none;
    color: white;
    font-size: 32px;
    width: 40px;
    height: 40px;
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0.8;
  }

  .image-preview-img {
    max-width: 90vw;
    max-height: 85vh;
    border-radius: 4px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
    pointer-events: auto;
  }
</style>
