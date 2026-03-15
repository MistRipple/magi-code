<script lang="ts">
  import type { Message, PlaceholderState, Task } from '../types/message';
  import type { IconName } from '../lib/icons';
  import MarkdownContent from './MarkdownContent.svelte';
  import WorkerBadge from './WorkerBadge.svelte';
  import SubTaskSummaryCard from './SubTaskSummaryCard.svelte';
  import BlockRenderer from './BlockRenderer.svelte';
  import Icon from './Icon.svelte';
  import RetryRuntimeIndicator from './RetryRuntimeIndicator.svelte';
  import ErrorDetailPopover from './ErrorDetailPopover.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getState, retryRuntimeState } from '../stores/messages.svelte';
  import { normalizeWorkerSlot } from '../lib/message-classifier';
  import { ensureArray } from '../lib/utils';

  // Props
  interface Props {
    message: Message;
    readOnly?: boolean;
    /** 显示上下文：thread=主对话区, worker=Worker面板 */
    displayContext?: 'thread' | 'worker';
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

  const appState = getState();
  const tasks = $derived(ensureArray(appState.tasks) as Task[]);
  const workerRuntimeMap = $derived(appState.workerRuntime);
  const workerWaitResults = $derived(appState.workerWaitResults);

  // 派生状态
  const isUser = $derived(message.type === 'user_input');
  const isNotice = $derived(message.type === 'system-notice' || message.type === 'error');
  const interactionMeta = $derived(message.metadata?.interaction as { prompt?: string; type?: string } | undefined);
  const isInteraction = $derived(Boolean(interactionMeta));
  const isStreaming = $derived(message.isStreaming);
  const retryRuntime = $derived(retryRuntimeState.byMessageId.get(message.id));

  // 主角色判断：主对话区的主角色是 orchestrator，Worker 面板的主角色是具体 Worker（claude/codex/gemini）
  // 主角色消息使用 inline 模式（无卡片包裹），客角色消息使用 card 模式
  const isNativeSource = $derived(
    displayContext === 'thread'
      ? message.source === 'orchestrator'
      : message.source !== 'orchestrator' && message.source !== 'system'
  );

  // 占位消息相关派生状态
  const isPlaceholder = $derived(Boolean(message.metadata?.isPlaceholder));
  const placeholderState = $derived((message.metadata?.placeholderState || 'pending') as PlaceholderState);
  const wasPlaceholder = $derived(Boolean(message.metadata?.wasPlaceholder));
  const sendingAnimation = $derived(Boolean(message.metadata?.sendingAnimation));
  const isSupplementary = $derived(Boolean(message.metadata?.isSupplementary));

  // 过滤无效的 blocks
  const safeBlocks = $derived(
    (message.blocks || []).filter((b): b is import('../types/message').ContentBlock =>
      !!b && typeof b === 'object' && 'type' in b
    )
  );

  // 检查是否真的有可见内容（防止虽然有 blocks 但全是空字符串导致 UI 假死）
  const hasVisibleContent = $derived.by(() => {
    if (message.content && message.content.trim().length > 0) return true;
    if (safeBlocks.length === 0) return false;

    // 遍历 blocks，只要有一个包含实质内容就认为有可见内容
    for (const block of safeBlocks) {
      if (block.type === 'tool_call') return true;
      if (block.type === 'tool_result') return true;
      if (block.type === 'thinking' && block.thinking?.content && block.thinking.content.trim().length > 0) return true;
      if (block.type === 'text' && block.content && block.content.trim().length > 0) return true;
    }
    return false;
  });

  // 纯内容消息判断：只含 text/code blocks 的消息使用 inline 模式渲染，
  // 不需要卡片包裹——它们是模型输出的自然语言内容和代码片段，
  // 只有 tool_call / thinking / plan / file_change 等过程性内容才需要卡片。
  const isContentOnly = $derived.by(() => {
    if (safeBlocks.length === 0) return Boolean(message.content);
    return safeBlocks.every(b => b.type === 'text' || b.type === 'code');
  });

  // 最终是否使用 inline 模式：主角色始终 inline；
  // 非主角色仅在消息最终确定形态（非流式、非占位）且为纯内容时才 inline，
  // 避免流式过程中 blocks 类型变化导致 DOM 分支切换闪烁。
  const useInlineMode = $derived(isNativeSource || (!isStreaming && !isPlaceholder && isContentOnly));
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

  // 格式化时间戳
  function formatTime(timestamp: number): string {
    const date = new Date(timestamp);
    return date.toLocaleTimeString(i18n.locale, {
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  function formatElapsed(seconds: number): string {
    if (seconds < 60) return `${seconds}s`;
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}m ${s}s`;
  }

  // 获取 worker 信息（如果有）
  const worker = $derived(message.metadata?.worker || null);
  const badgeWorker = $derived(worker || (message.source === 'orchestrator' ? 'orchestrator' : message.source));

  // 子任务卡片消息，作为独立消息存在
  const isSubTaskCardOnly = $derived(message.type === 'task_card');
  const isWorkerSingleCard = $derived(isSubTaskCardOnly || message.type === 'instruction');

  type CardWorkerStatus = 'pending' | 'running' | 'completed' | 'failed' | 'stopped' | 'skipped';

  const subTaskId = $derived.by(() => {
    const metaId = typeof message.metadata?.subTaskId === 'string' ? message.metadata?.subTaskId : '';
    if (metaId) return metaId;
    const cardId = typeof (message.metadata?.subTaskCard as { id?: unknown } | undefined)?.id === 'string'
      ? (message.metadata?.subTaskCard as { id: string }).id
      : '';
    return cardId || null;
  });

  const subTaskFromTasks = $derived.by(() => {
    if (!subTaskId) return null;
    for (const task of tasks) {
      const found = (task.subTasks || []).find((st) => st.id === subTaskId);
      if (found) return found;
    }
    return null;
  });

  function mapSubTaskStatusToCard(status?: string): CardWorkerStatus | undefined {
    if (!status) return undefined;
    switch (status) {
      case 'running':
        return 'running';
      case 'in_progress':
        return 'running';
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'skipped':
        return 'skipped';
      case 'cancelled':
        return 'stopped';
      case 'blocked':
      case 'paused':
      case 'pending':
        return 'pending';
      default:
        return undefined;
    }
  }

  const subTaskStatusOverride = $derived.by(() => mapSubTaskStatusToCard(subTaskFromTasks?.status));
  const subTaskStartedAtOverride = $derived.by(() => subTaskFromTasks?.startedAt);

  const instructionWorkerName = $derived(normalizeWorkerSlot(message.metadata?.worker || message.metadata?.agent));

  // 任务说明消息（编排者派发给 Worker）
  const isInstruction = $derived(message.type === 'instruction');
  const instructionTargetWorker = $derived(
    (message.metadata?.worker || message.metadata?.agent) as string | undefined
  );
  const instructionCardPayload = $derived.by(() => ({
    title: message.content || '',
    description: typeof message.metadata?.description === 'string' ? message.metadata?.description : undefined,
    worker: instructionTargetWorker || (message.metadata?.assignedWorker as string | undefined),
  }));

  const cardWorker = $derived.by(() => {
    const rawWorker = (message.metadata?.subTaskCard as { worker?: unknown } | undefined)?.worker
      || message.metadata?.assignedWorker
      || instructionWorkerName
      || message.metadata?.worker;
    return normalizeWorkerSlot(rawWorker);
  });

  const workerRuntime = $derived.by(() => (cardWorker ? workerRuntimeMap[cardWorker] : null));
  const cardKey = $derived.by(() => {
    const meta = message.metadata as Record<string, unknown> | undefined;
    const rawRequestId = typeof meta?.requestId === 'string' ? meta.requestId.trim() : '';
    const rawMissionId = typeof meta?.missionId === 'string' ? meta.missionId.trim() : '';
    const scopeId = rawRequestId || rawMissionId;
    const rawAssignmentId = typeof meta?.assignmentId === 'string' ? meta.assignmentId.trim() : '';
    if (rawAssignmentId) {
      return scopeId ? `assign:${rawAssignmentId}@${scopeId}` : `assign:${rawAssignmentId}`;
    }
    const rawSubTaskId = typeof meta?.subTaskId === 'string' ? meta.subTaskId.trim() : '';
    if (rawSubTaskId) {
      return scopeId ? `assign:${rawSubTaskId}@${scopeId}` : `assign:${rawSubTaskId}`;
    }
    const rawCardId = typeof meta?.cardId === 'string' ? meta.cardId.trim() : '';
    if (rawCardId) return rawCardId;
    return '';
  });
  const workerWaitResult = $derived.by(() => (cardKey ? (workerWaitResults?.[cardKey] || null) : null));

  function mapRuntimeStatusToCard(status?: string | null): CardWorkerStatus | undefined {
    if (!status) return undefined;
    switch (status) {
      case 'running':
        return 'running';
      case 'blocked':
        return 'pending';
      case 'failed':
        return 'failed';
      case 'completed':
        return 'completed';
      default:
        return undefined;
    }
  }

  const runtimeStatusOverride = $derived.by(() => mapRuntimeStatusToCard(workerRuntime?.status));
  function mapWaitResultStatus(status?: string): CardWorkerStatus | undefined {
    switch (status) {
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'skipped':
        return 'skipped';
      case 'cancelled':
        return 'stopped';
      default:
        return undefined;
    }
  }

  const waitResultStatusOverride = $derived.by(() => {
    if (!workerWaitResult || !workerWaitResult.results || workerWaitResult.results.length === 0) return undefined;
    return mapWaitResultStatus(workerWaitResult.results[0]?.status);
  });

  // 消息自身携带的 subTaskCard.status — 后端 emitSubTaskCard 写入，随消息持久化
  // 作为比 runtimeStatusOverride 更优先的保护层，防止 Worker 级别的全局状态穿透已完成的旧卡片
  const metadataCardStatus = $derived.by(() => {
    const cardData = message.metadata?.subTaskCard as { status?: string, wait_status?: string } | undefined;
    // 如果已经固化了 wait_status，则优先返回 wait_status 对应的卡片状态
    if (cardData?.wait_status) {
      return mapWaitResultStatus(cardData.wait_status);
    }
    return mapSubTaskStatusToCard(cardData?.status);
  });

  const cardStatusOverride = $derived.by(() => {
    // 3 层优先级：subTask(tasks数组) > waitResult(全局缓存) > metadata(消息自身) > runtime(Worker级)
    return subTaskStatusOverride || waitResultStatusOverride || metadataCardStatus || runtimeStatusOverride;
  });
  const cardStartedAtOverride = $derived.by(() =>
    subTaskStartedAtOverride ?? (isInstruction ? message.timestamp : undefined)
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
    <div class="user-content">{message.content}</div>
    <div class="user-time">
      {#if isSupplementary}<span class="supplementary-tag">{i18n.t('messageItem.supplementaryTag')}</span>{/if}
      {formatTime(message.timestamp)}
    </div>
  </div>
<!-- Worker 单卡：任务/等待/结果统一渲染 -->
{:else if isWorkerSingleCard}
  <div class="message-item subtask-card-only" data-message-id={message.id} data-source={message.source}>
    <SubTaskSummaryCard
      card={(isSubTaskCardOnly ? (message.metadata?.subTaskCard as any) : instructionCardPayload) as any}
      {readOnly}
      messageTimestamp={message.timestamp}
      statusOverride={cardStatusOverride}
      startedAtOverride={cardStartedAtOverride}
      runtimeStatus={workerRuntime?.status}
      waitResult={workerWaitResult}
    />
  </div>
<!-- 助手消息：纯文本内容使用 inline 模式，结构化内容使用 card 模式 -->
{:else if useInlineMode}
  <!-- inline 模式：无卡片边框和 header，自然融入对话流 -->
  <div
    class="message-item assistant native"
    class:inline-guest={!isNativeSource}
    class:streaming={isStreaming}
    class:placeholder={isPlaceholder}
    class:was-placeholder={wasPlaceholder}
    data-message-id={message.id}
    data-source={message.source}
    data-interaction={isInteraction ? 'true' : 'false'}
    data-placeholder-state={isPlaceholder ? placeholderState : undefined}
  >
    <div class="message-content">
      {#if isPlaceholder}
        <div class="placeholder-content">
          {#if showStreamingIndicator}
            <div class="streaming-indicator-bottom">
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-elapsed-time">{formatElapsed(streamingElapsedSeconds)}</span>
            </div>
          {/if}
        </div>
      {:else}
        <!-- 非主角色的纯文本消息：在内容前显示来源标识 -->
        {#if !isNativeSource}
          <div class="inline-source-tag">
            <WorkerBadge worker={badgeWorker} size="sm" />
          </div>
        {/if}

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
        {:else if safeBlocks.length > 0}
          {#each safeBlocks as block, i (`${message.id}-block-${i}-${block.type}`)}
            <BlockRenderer {block} {isStreaming} {readOnly} />
          {/each}
        {:else if message.content}
          <MarkdownContent content={message.content} {isStreaming} />
        {/if}

          <!-- 只要处于流式接收状态，就应该在最底部渲染跳动的加载点，不论是否有内容 -->
          {#if isStreaming && showStreamingIndicator}
            <div class="streaming-indicator-bottom fallback" class:has-content={hasVisibleContent}>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-elapsed-time">{formatElapsed(streamingElapsedSeconds)}</span>
            </div>
          {/if}

          {#if retryRuntime}
            <RetryRuntimeIndicator runtime={retryRuntime} />
          {/if}

      {/if}
    </div>
  </div>
{:else}
  <!-- card 模式：含结构化内容（tool_call/thinking/code/plan 等）的消息，有卡片边框和 header -->
  <div
    class="message-item assistant"
    class:streaming={isStreaming}
    class:placeholder={isPlaceholder}
    class:was-placeholder={wasPlaceholder}
    class:no-visible-content={!hasVisibleContent}
    data-message-id={message.id}
    data-source={message.source}
    data-interaction={isInteraction ? 'true' : 'false'}
    data-placeholder-state={isPlaceholder ? placeholderState : undefined}
  >
    <!-- 当真正有内容时才展开 header，或者不处于 streaming 状态 -->
    {#if hasVisibleContent || !isStreaming || isPlaceholder}
      <div class="message-header" class:fade-in={hasVisibleContent && isStreaming}>
        <div class="message-source">
          <WorkerBadge worker={badgeWorker} size="sm" />
          {#if isPlaceholder}
            <span class="placeholder-status">
              {#if placeholderState === 'pending'}
                {i18n.t('messageItem.placeholder.pending')}
              {:else if placeholderState === 'received'}
                {i18n.t('messageItem.placeholder.received')}
              {:else if placeholderState === 'thinking'}
                {i18n.t('messageItem.placeholder.thinking')}
              {:else}
                {i18n.t('messageItem.placeholder.processing')}
              {/if}
            </span>
          {/if}
        </div>
        <div class="message-meta">
          <span class="message-time">{formatTime(message.timestamp)}</span>
          {#if !isStreaming && !isPlaceholder}
            <button class="copy-btn" onclick={handleCopy} title={i18n.t('messageItem.copyTitle')}>
              <Icon name="copy" size={12} />
            </button>
          {/if}
        </div>
      </div>
    {/if}

    <div class="message-content">
      {#if isPlaceholder}
        <div class="placeholder-content">
          {#if showStreamingIndicator}
            <div class="streaming-indicator-bottom">
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-elapsed-time">{formatElapsed(streamingElapsedSeconds)}</span>
            </div>
          {/if}
        </div>
      {:else}
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
        {:else if safeBlocks.length > 0}
          {#each safeBlocks as block, i (`${message.id}-block-${i}-${block.type}`)}
            <BlockRenderer {block} {isStreaming} {readOnly} />
          {/each}
        {:else if message.content}
          <MarkdownContent content={message.content} {isStreaming} />
        {/if}

          <!-- 与主角色消息保持一致：处于流式接收时在底部展示统一三点动画 -->
          {#if isStreaming && showStreamingIndicator}
            <div class="streaming-indicator-bottom fallback" class:has-content={hasVisibleContent}>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-dot"></span>
              <span class="streaming-elapsed-time">{formatElapsed(streamingElapsedSeconds)}</span>
            </div>
          {/if}

          {#if retryRuntime}
            <RetryRuntimeIndicator runtime={retryRuntime} />
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

  /* ===== SubTaskCard 独立样式（与 assistant 消息保持一致的间距） ===== */
  .message-item.subtask-card-only {
    padding: 0 var(--space-4);
    margin-right: var(--space-2);
  }

  /* ===== 助手消息样式（card 模式：客角色） ===== */
  .message-item.assistant {
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    border: 1px solid var(--border);
    margin-right: var(--space-2);
    transition: border-color 0.2s ease, box-shadow 0.2s ease;
    flex-shrink: 0;
    height: auto;
    overflow: visible;
  }

  /* ===== inline 模式样式（无卡片包裹） ===== */
  .message-item.assistant.native {
    background: transparent;
    border: none;
    padding: 0 var(--space-4);
    border-radius: 0;
  }

  /* 主角色连续消息紧凑间距（orchestrator 在主对话区、worker 在 Worker 面板） */
  .message-item.assistant.native:not(.inline-guest) {
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

  /* 移除无可见内容时的多余包装感（针对客角色卡片） */
  .message-item.assistant.streaming.no-visible-content:not(.placeholder) {
    border-color: transparent;
    background: transparent;
    box-shadow: none;
    padding-top: var(--space-2);
    padding-bottom: var(--space-2);
  }

  /* 当有内容时的加载指示器，增加上边距，和内容拉开距离 */
  .streaming-indicator-bottom.fallback.has-content {
    margin-top: var(--space-2);
    padding-top: var(--space-2);
  }

  /* 渐入动画，用于内容首次出现时平滑展开 Header */
  .message-header.fade-in {
    animation: contentFadeIn 0.3s ease-out;
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
  /* 消除流式渲染时首个 block（thinking/tool_call）的顶部 margin 产生的空白 */
  .message-content > :global(:first-child) {
    margin-top: 0;
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



  /* 流式消息底部加载指示器：统一的三个点动画 */
  .streaming-indicator-bottom {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: var(--space-2) 0;
    margin-top: var(--space-2);
  }

  .streaming-indicator-bottom.fallback {
    margin-top: 0;
    padding: var(--space-1) 0;
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

  .streaming-elapsed-time {
    margin-left: 4px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
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
    display: flex;
    align-items: center;
    padding: var(--space-1) 0;
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
