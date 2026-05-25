<script lang="ts">
  import type { ContentBlock, Message, PlaceholderState } from '../types/message';
  import type { IconName } from '../lib/icons';
  import MarkdownContent from './MarkdownContent.svelte';
  import ExecutorBadge from './ExecutorBadge.svelte';
  import BlockRenderer from './BlockRenderer.svelte';
  import Icon from './Icon.svelte';
  import RetryRuntimeIndicator from './RetryRuntimeIndicator.svelte';
  import ErrorDetailPopover from './ErrorDetailPopover.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { retryRuntimeState } from '../stores/messages.svelte';
  import { getAgentColor } from '../lib/agent-colors';
  import { formatDuration, formatElapsed as formatElapsedMmSs } from '../lib/utils';
  import { isRuntimeInternalTool } from '../shared/tool-visibility';

  // Props
  interface Props {
    message: Message;
    readOnly?: boolean;
    /** 显示上下文：thread=主对话区, worker=Worker面板 */
    displayContext?: 'thread' | 'task';
    /** 是否允许渲染底部流式三点（仅最后一条流式消息启用） */
    showStreamingIndicator?: boolean;
    /** 当前面板流式计时（秒） */
    streamingElapsedSeconds?: number;
  }
  let {
    message,
    readOnly = false,
    displayContext = 'thread',
    showStreamingIndicator = true,
    streamingElapsedSeconds = 0,
  }: Props = $props();

  // 派生状态
  const isUser = $derived(message.type === 'user_input');
  const isNotice = $derived(message.type === 'system-notice');
  const interactionMeta = $derived(message.metadata?.interaction as {
    prompt?: string;
    type?: string;
    requestId?: string;
    options?: Array<{ value: string; label: string; isDefault?: boolean }>;
  } | undefined);
  const isInteraction = $derived(Boolean(interactionMeta));
  const isStreaming = $derived(message.isStreaming);
  const retryRuntime = $derived(retryRuntimeState.byMessageId.get(message.id));

  // 主角色判断只决定来源标签；普通正文必须保持无外层面板。
  const isNativeSource = $derived(
    displayContext === 'thread'
      ? message.source === 'orchestrator'
      : message.source !== 'orchestrator' && message.source !== 'system'
  );

  // 占位消息相关派生状态
  const isPlaceholder = $derived(Boolean(message.metadata?.isPlaceholder));
  const placeholderState = $derived((message.metadata?.placeholderState || 'pending') as PlaceholderState);
  const sendingAnimation = $derived(Boolean(message.metadata?.sendingAnimation));
  const isSupplementary = $derived(Boolean(message.metadata?.isSupplementary));

  // 动态 agent 颜色：为消息左侧色带和流式动画提供颜色
  const agentColorStyle = $derived.by(() => {
    const source = message.source;
    if (!source) return '';
    const { color } = getAgentColor(source);
    return `border-left-color: ${color}; --stream-accent: ${color};`;
  });
  const safeBlocks = $derived(
    (message.blocks || []).filter((b): b is ContentBlock =>
      !!b && typeof b === 'object' && 'type' in b
    )
  );
  // 主线不承载运行时内部工具；这些 block 只能作为任务详情/运行时日志的投影。
  const presentationBlocks = $derived(
    safeBlocks.filter((block) => {
      if (block.type !== 'tool_call' && block.type !== 'tool_result') {
        return true;
      }
      const toolName = typeof block.toolCall?.name === 'string' ? block.toolCall.name : '';
      return !isRuntimeInternalTool(toolName);
    })
  );
  // 检查是否真的有可见内容（防止虽然有 blocks 但全是空字符串导致 UI 假死）
  const hasVisibleContent = $derived.by(() => {
    if (message.content && message.content.trim().length > 0) return true;
    if (presentationBlocks.length === 0) return false;

    // 遍历 blocks，只要有一个包含实质内容就认为有可见内容
    for (const block of presentationBlocks) {
      if (block.type === 'tool_call') return true;
      if (block.type === 'tool_result') return true;
      if (block.type === 'file_change') return true;
      if (block.type === 'plan') return true;
      if (block.type === 'thinking') {
        const thinkingText = block.thinking?.content || block.content || '';
        if (thinkingText.trim().length > 0) return true;
      }
      if (block.type === 'text' && block.content && block.content.trim().length > 0) return true;
    }
    return false;
  });

  const messagePhase = $derived.by(() => (
    typeof message.metadata?.phase === 'string' ? message.metadata.phase.trim() : ''
  ));
  const systemSectionType = $derived.by(() => {
    const extra = message.metadata?.extra as { type?: unknown } | undefined;
    return typeof extra?.type === 'string' ? extra.type.trim() : '';
  });
  const isSystemSection = $derived.by(() => (
    displayContext === 'thread'
    && message.source === 'orchestrator'
    && messagePhase === 'system_section'
    && !isStreaming
    && !isPlaceholder
  ));
  const systemSectionSummary = $derived.by(() => {
    if (!isSystemSection || typeof message.content !== 'string') {
      return '';
    }
    const firstLine = message.content
      .split(/\r?\n/)
      .map((line) => line.trim())
      .find((line) => line.length > 0) || '';
    return firstLine.replace(/^\[system\]\s*/i, '').trim();
  });
  const shouldCollapseSystemSection = $derived.by(() => {
    if (!isSystemSection || typeof message.content !== 'string') {
      return false;
    }
    const normalized = message.content.trim();
    if (!normalized) {
      return false;
    }
    const nonEmptyLines = normalized
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line.length > 0).length;
    return nonEmptyLines >= 3 || normalized.length >= 140;
  });

  function resolveBlockRenderKey(
    block: import('../types/message').ContentBlock,
  ): string {
    const id = typeof block.id === 'string' ? block.id.trim() : '';
    if (!id) {
      throw new Error(`[MessageItem] block 缺少 id: message=${message.id} type=${block.type}`);
    }
    return `${message.id}:${id}`;
  }

  // 格式化时间戳
  function formatTime(timestamp: number): string {
    const date = new Date(timestamp);
    return date.toLocaleTimeString(i18n.locale, {
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  function formatElapsed(seconds: number): string {
    return formatElapsedMmSs(seconds);
  }

  function formatDurationMs(durationMs: number): string {
    const normalizedMs = Math.max(0, durationMs);
    if (normalizedMs > 0 && normalizedMs < 1000) {
      return '<1s';
    }
    return formatDuration(normalizedMs);
  }

  // 获取 worker 信息（如果有）
  const worker = $derived(message.metadata?.worker || null);
  const laneTitle = $derived.by(() => {
    const value = message.metadata?.laneTitle;
    return typeof value === 'string' ? value.trim() : '';
  });
  const messageTypeRequiresCardShell = $derived.by(() => {
    switch (message.type) {
      case 'interaction':
      case 'error':
        return true;
      default:
        return false;
    }
  });
  const usesCardShell = $derived.by(() => (
    isInteraction
    || shouldCollapseSystemSection
    || messageTypeRequiresCardShell
  ));
  const badgeWorker = $derived(worker || (message.source === 'orchestrator' ? 'orchestrator' : message.source));
  const turnItemKind = $derived.by(() => {
    const value = message.metadata?.turnItemKind;
    return typeof value === 'string' ? value.trim() : '';
  });
  const workerBadgeLabel = $derived.by(() => (
    turnItemKind.startsWith('worker_') && laneTitle
      ? laneTitle
      : ''
  ));
  const responseDurationMs = $derived.by(() => {
    const value = message.metadata?.responseDurationMs;
    return typeof value === 'number' && Number.isFinite(value) && value >= 0
      ? value
      : null;
  });
  const showResponseDuration = $derived.by(() => (
    displayContext === 'thread'
    && !isStreaming
    && !isPlaceholder
    && !isSystemSection
    && responseDurationMs !== null
  ));

  // 子任务卡片消息，作为独立消息存在
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

</script>

<!-- 系统通知消息：居中显示（必须有实际文本内容才渲染） -->
{#if isNotice && message.content && message.content.trim()}
  <div class="system-notice {noticeType}">
    <span class="notice-icon" style="color: {noticeColors[noticeType] || noticeColors.info}">
      <Icon name={noticeIcons[noticeType] || 'info'} size={14} />
    </span>
    <span class="notice-text">
      <ErrorDetailPopover text={message.content} maxInlineChars={96} />
    </span>
    <span class="notice-time">{formatTime(message.timestamp)}</span>
  </div>
<!-- 用户消息：简洁显示 -->
{:else if isUser}
  <div class="message-item user" class:sending={sendingAnimation} class:supplementary={isSupplementary} data-message-id={message.id} data-source="user">
    <!-- 用户上传的图片缩略图 -->
    {#if messageImages.length > 0}
      <div class="user-images">
        {#each messageImages as img, i (`${message.id}-img-${i}`)}
          <button class="user-image-thumb" onclick={() => openImagePreview(img.dataUrl)} type="button" title={i18n.t('messageItem.imageClickTitle')}>
            <img src={img.dataUrl} alt={i18n.t('messageItem.imageAlt', { index: i + 1 })} />
          </button>
        {/each}
      </div>
    {/if}
    <div class="user-row">
      <div class="user-content">
        <div class="user-plain-content">
          <MarkdownContent content={message.content || ''} isStreaming={false} />
        </div>
      </div>
    </div>
    <div class="user-time">
      {#if isSupplementary}<span class="supplementary-tag">{i18n.t('messageItem.supplementaryTag')}</span>{/if}
      {formatTime(message.timestamp)}
    </div>
  </div>
<!-- 助手消息：单一纯流式渲染路径，收到什么立即展示，不做模式切换 -->
{:else}
  <div
    class="message-item assistant"
    class:native={isNativeSource}
    class:inline-guest={!isNativeSource}
    class:card-shell={usesCardShell}
    class:plain-shell={!usesCardShell}
    class:streaming={isStreaming}
    class:placeholder={isPlaceholder}
    class:no-visible-content={!hasVisibleContent && !isNativeSource}
    data-message-id={message.id}
    data-source={message.source}
    data-interaction={isInteraction ? 'true' : 'false'}
    data-placeholder-state={isPlaceholder ? placeholderState : undefined}
    style={agentColorStyle}
  >
    <!-- 非主角色：来源标识固定渲染，占位/有内容共用同一节点，避免首 token 漂移 -->
    {#if !isNativeSource}
      <div class="inline-source-tag">
        <ExecutorBadge worker={badgeWorker} label={workerBadgeLabel} size="sm" />
      </div>
    {/if}

    <div class="message-content">
      {#if !isPlaceholder}
        {#if isInteraction && interactionMeta?.prompt}
          <div class="interaction-inline">
            <Icon name="sparkles" size={14} />
            <span>{interactionMeta.prompt}</span>
          </div>
        {/if}

        {#if shouldCollapseSystemSection && safeBlocks.length === 0 && message.content}
          <details class="system-section-fold" data-system-section-type={systemSectionType || undefined}>
            <summary class="system-section-summary">
              <span class="system-section-badge">{i18n.t('messageItem.systemSectionBadge')}</span>
              <span class="system-section-title">{systemSectionSummary || i18n.t('messageItem.systemSectionTitleFallback')}</span>
              <span class="system-section-hint">{i18n.t('messageItem.systemSectionExpandHint')}</span>
            </summary>
            <div class="system-section-content">
              <MarkdownContent content={message.content} isStreaming={false} />
            </div>
          </details>
        {:else if presentationBlocks.length > 0}
          {#each presentationBlocks as block, i (resolveBlockRenderKey(block))}
            {@const blockIsStreaming = block.type === 'thinking'
              ? (isStreaming && i === presentationBlocks.length - 1)
              : isStreaming}
            <BlockRenderer {block} isStreaming={blockIsStreaming} {readOnly} />
          {/each}
        {:else if message.content}
          <MarkdownContent content={message.content} {isStreaming} />
        {/if}

        {#if showResponseDuration}
          <div class="message-runtime-footer completed" class:has-content={hasVisibleContent}>
            <span class="message-runtime-text">
              {i18n.t('messageItem.responseDurationLabel')} {formatDurationMs(responseDurationMs ?? 0)}
            </span>
          </div>
        {/if}

        {#if retryRuntime}
          <RetryRuntimeIndicator runtime={retryRuntime} />
        {/if}
      {/if}

      <!-- 底部计时器槽位：父级 MessageList 把 showStreamingIndicator 锚到当前 turn 内
           displayOrder 最大的非 user_input render item，使指示器始终落在 turn 最末尾卡片的底部；
           不再叠加 isStreaming 判断，避免锚点 message 自身已完成时指示器消失。 -->
      {#if showStreamingIndicator}
        <div class="streaming-indicator-bottom">
          <span class="streaming-dot"></span>
          <span class="streaming-dot"></span>
          <span class="streaming-dot"></span>
          <span class="streaming-elapsed-time">{formatElapsed(streamingElapsedSeconds)}</span>
        </div>
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
    aria-label={i18n.t('messageItem.imagePreviewClose')}
  >
    <div class="image-preview-content" role="document">
      <span class="image-preview-close" aria-hidden="true">×</span>
      <img src={previewImageUrl} alt={i18n.t('messageItem.imagePreviewAlt')} class="image-preview-img" />
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
    min-width: 0;
    flex: 1;
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

  .user-row {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
    max-width: 100%;
  }

  .user-content {
    background: var(--primary);
    color: white;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-lg) var(--radius-lg) var(--radius-sm) var(--radius-lg);
    font-size: var(--text-base);
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
    min-width: 0;
  }

  .user-plain-content {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  /* 用户气泡是固定蓝底，markdown 内部所有文本/标记/代码块都强制白色，
     不随主题变化——避免浅色模式下文字与蓝底对比度过低。 */
  .user-content :global(.markdown-content),
  .user-content :global(.markdown-content *) {
    color: inherit;
  }
  .user-content :global(.markdown-content code),
  .user-content :global(.markdown-content pre) {
    background: rgba(255, 255, 255, 0.18);
    color: inherit;
  }
  .user-content :global(.markdown-content a) {
    color: inherit;
    text-decoration: underline;
  }

  .user-time {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: var(--space-1);
    display: flex;
    align-items: center;
    gap: var(--space-2);
    justify-content: flex-end;
  }

  /* ===== 补充指令标签 ===== */
  .supplementary-tag {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: var(--radius-sm);
    background: var(--foreground-muted);
    color: var(--background);
    opacity: 0.7;
  }
  .message-item.user.supplementary .user-content {
    background: var(--primary);
    opacity: 0.85;
  }

  /* ===== 助手消息基础样式：默认纯正文，不带卡片壳 ===== */
  .message-item.assistant {
    display: flex;
    flex-direction: column;
    padding: 0 var(--space-4);
    margin-right: var(--space-2);
    flex-shrink: 0;
    height: auto;
    overflow: visible;
  }

  .message-item.assistant.plain-shell {
    border: none;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
  }

  /* ===== 外层卡片壳只用于整条交互/错误等消息；thinking / 工具 / worker card 由各自 block 组件渲染 ===== */
  .message-item.assistant.card-shell {
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    border-left-width: 3px;
    transition: border-color 0.2s ease, box-shadow 0.2s ease;
  }

  /* 主角色连续消息紧凑间距（orchestrator 在主对话区，子代理在任务详情中） */
  .message-item.assistant.plain-shell:not(.inline-guest) {
    margin-top: calc(-1 * var(--space-2));
  }

  /* 非主角色的 inline 来源标识：轻量 badge，不打断阅读流 */
  .inline-source-tag {
    margin-bottom: var(--space-2);
    opacity: 0.7;
  }

  /* 流式消息卡片：高度完全由内容与动画驱动，避免占位感 */
  .message-item.assistant.streaming {
    min-height: 0;
  }

  /* 移除无可见内容时的多余包装感（针对特殊卡片） */
  .message-item.assistant.card-shell.streaming.no-visible-content:not(.placeholder) {
    border-color: transparent;
    background: transparent;
    box-shadow: none;
    padding-top: var(--space-2);
    padding-bottom: var(--space-2);
  }

  /* 底部运行态区域在 streaming 与完成态之间复用同一容器，避免完成瞬间高度跳变。 */
  .message-runtime-footer.has-content {
    margin-top: var(--space-3);
    padding-top: var(--space-2);
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
  .interaction-actions {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-2);
  }
  .interaction-action-btn {
    min-width: 88px;
    padding: 6px 12px;
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--foreground);
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease, opacity 120ms ease;
  }
  .interaction-action-btn:hover:not(:disabled) {
    border-color: var(--primary);
    background: color-mix(in srgb, var(--primary) 10%, var(--surface));
  }
  .interaction-action-btn.is-primary {
    border-color: color-mix(in srgb, var(--primary) 55%, transparent);
    background: color-mix(in srgb, var(--primary) 18%, var(--surface));
  }
  .interaction-action-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .system-section-fold {
    margin: var(--space-1) 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface) 88%, var(--foreground) 12%);
    overflow: hidden;
  }
  .system-section-summary {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    list-style: none;
    user-select: none;
    font-size: var(--text-sm);
  }
  .system-section-summary::-webkit-details-marker {
    display: none;
  }
  .system-section-badge {
    flex-shrink: 0;
    font-size: 11px;
    line-height: 1;
    color: var(--info);
    border: 1px solid color-mix(in srgb, var(--info) 45%, transparent);
    border-radius: 999px;
    padding: 3px 8px;
  }
  .system-section-title {
    flex: 1;
    min-width: 0;
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .system-section-hint {
    flex-shrink: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }
  .system-section-fold[open] .system-section-summary {
    border-bottom: 1px solid var(--border);
  }
  .system-section-content {
    padding: var(--space-2) var(--space-3) var(--space-3);
  }
  /* 流式时不再变更边框颜色，避免 streaming 状态快速切换导致边框线闪烁 */

  .message-content {
    line-height: var(--leading-relaxed);
    word-wrap: break-word;
    overflow-wrap: break-word;
    font-size: var(--text-base);
    /* 核心性能屏障：防止底部打字机变高时，触发 List/App 一路向上重新算高度（Reflow） */
    contain: layout;
    /* 确保内容区域高度由内容自然撑开 */
    min-height: 0;
    height: auto;
  }
  /* 消除流式渲染时首个 block（thinking/tool_call）的顶部 margin 产生的空白 */
  .message-content > :global(:first-child) {
    margin-top: 0;
  }

  /* 流式消息底部加载指示器：始终位于消息内容末尾，使整 turn 末尾卡片的底部承载计时器 */
  .streaming-indicator-bottom,
  .message-runtime-footer {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: var(--space-2) 0;
  }

  .streaming-indicator-bottom {
    margin-top: var(--space-2);
    padding: var(--space-1) 0;
  }

  .message-runtime-footer {
    min-height: calc(var(--text-xs) * 1.4 + var(--space-2));
    margin-top: 0;
    padding: var(--space-1) 0;
  }

  .streaming-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--foreground-muted);
    opacity: 0.72;
    animation: streamingBounce 1.4s ease-in-out infinite;
  }

  .streaming-dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .streaming-dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  .streaming-elapsed-time {
    margin-left: 4px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }

  .message-runtime-text {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
    font-variant-numeric: tabular-nums;
  }

  @keyframes streamingBounce {
    0%, 80%, 100% {
      opacity: 0.45;
      transform: translateY(0);
    }
    40% {
      opacity: 1;
      transform: translateY(-3px);
    }
  }

  /* ===== 占位消息样式：只保留裸态流式指示器，不给整条消息套卡片 ===== */
  .message-item.assistant.placeholder {
    border-left: none;
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
    z-index: var(--z-modal);
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
