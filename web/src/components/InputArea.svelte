<script lang="ts">
  import { vscode } from '../lib/vscode-bridge';
  import {
    addToast,
    getActiveInteractionType,
    getQueuedMessages,
    markQueuedMessageAsGuide,
    messagesState,
  } from '../stores/messages.svelte';
  import { getTaskGraphState, refreshTaskProjection } from '../stores/task-graph-store.svelte';
  import type { SessionIntakeResponseDto } from '../shared/rust-backend-types';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';
  import Icon from './Icon.svelte';
  import { generateId } from '../lib/utils';
  import { i18n } from '../stores/i18n.svelte';
  import { isTaskProjectionAcceptingIntake } from '../lib/task-projection-state';

  interface SelectedImage {
    id: string;
    dataUrl: string;
    name: string;
  }

  // 输入内容
  let inputValue = $state('');

  // 拖动调整大小相关
  let inputHeight = $state(120); // 默认高度增加到 120px
  const minHeight = 80;
  const maxHeight = 400;

  // 🔧 图片上传相关状态
  let selectedImages = $state<SelectedImage[]>([]);
  const MAX_IMAGES = 5;  // 最多支持 5 张图片
  const MAX_IMAGE_SIZE = 10 * 1024 * 1024;  // 单张图片最大 10MB

  // Intake 路由状态
  let intakeLoading = $state(false);
  let stopLoading = $state(false);

  const currentSessionId = $derived(messagesState.currentSessionId);
  const taskGraph = $derived(getTaskGraphState(currentSessionId));

  // 任务图运行中：将用户输入路由到 Intake API
  const shouldUseIntake = $derived.by(() => {
    const projection = taskGraph.projection;
    return isTaskProjectionAcceptingIntake(projection, taskGraph.rootTaskId);
  });
  const defaultIntakeContextTaskId = $derived.by(() => {
    const projection = taskGraph.projection;
    if (!projection) return null;
    const priorityStatuses = ['AwaitingApproval', 'Blocked', 'Repairing', 'Verifying', 'Running', 'Ready'];
    for (const status of priorityStatuses) {
      const task = projection.tasks.find((item) => item.kind !== 'Objective' && item.status === status);
      if (task) return task.task_id;
    }
    return projection.root_task?.task_id ?? taskGraph.rootTaskId ?? null;
  });
  const intakeContextTaskId = $derived(defaultIntakeContextTaskId);
  const shouldPauseTaskGraphFromComposer = $derived.by(() => {
    const projection = taskGraph.projection;
    const sessionId = currentSessionId?.trim();
    const rootTaskId = projection?.root_task.task_id ?? taskGraph.rootTaskId;
    if (!projection || !sessionId || !rootTaskId) return false;
    return projection.runner_status === 'running';
  });
  const sessionInputLocked = $derived.by(() => (
    messagesState.sessionHydrating || !currentSessionId?.trim()
  ));

  // 发送/停止态只认 store 内已经收敛好的处理状态，避免历史工具卡片把空闲会话抬回执行态。
  const isSending = $derived(
    messagesState.isProcessing
    || messagesState.backendProcessing,
  );
  const activeInteraction = $derived.by(() => getActiveInteractionType());
  const isInteractionBlocking = $derived.by(() => Boolean(activeInteraction));
  const queuedMessages = $derived.by(() => getQueuedMessages());
  const MAX_INPUT_CHARS = 10000;
  let inputTextareaEl = $state<HTMLTextAreaElement | null>(null);
  const sendButtonTitle = $derived.by(() => {
    if (isSending) {
      return i18n.t('input.followUp.queueTitle');
    }
    return i18n.t('input.send');
  });
  const sendDisabled = $derived.by(() => (
    sessionInputLocked || isInteractionBlocking || intakeLoading
  ));
  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (inputValue.trim().length > 0) return true;
    // 执行中补充指令不支持图片，避免"有内容可发送"与实际能力不一致
    if (isSending) return false;
    return selectedImages.length > 0;
  });

  function clearComposerState() {
    inputValue = '';
    selectedImages = [];
  }

  function resolveComposerRawContent(): string {
    if (typeof inputTextareaEl?.value === 'string') {
      return inputTextareaEl.value;
    }
    if (typeof document !== 'undefined') {
      const activeElement = document.activeElement;
      if (
        activeElement
        && typeof HTMLTextAreaElement !== 'undefined'
        && activeElement instanceof HTMLTextAreaElement
      ) {
        return activeElement.value;
      }
    }
    return inputValue;
  }

  function isNaturalContinueRequest(value: string | null): boolean {
    if (!value) return false;
    const text = value.trim().toLowerCase();
    if (!text) return false;
    return [
      '继续',
      '继续执行',
      '继续任务',
      '继续刚才的任务',
      '继续刚刚的任务',
      'resume',
      'continue',
    ].includes(text);
  }

  // 发送消息（支持图片附件）
  // 运行中再次发送不会打断当前轮，而是按当前 session 的队列/引导模式串行提交。
  async function sendMessage() {
    const rawContent = resolveComposerRawContent();
    const normalizedContent = rawContent.trim();
    // 允许只发送图片（无文字）或只发送文字。
    if ((!normalizedContent && selectedImages.length === 0) || isInteractionBlocking) return;
    if ((isSending || shouldUseIntake) && selectedImages.length > 0) {
      addToast('warning', i18n.t('input.noImageDuringExecution'));
      return;
    }

    const submissionText = normalizedContent
      ? rawContent
      : (selectedImages.length > 0 ? i18n.t('input.analyzeImages') : null);
    const submissionLength = submissionText?.length ?? 0;

    if (submissionLength > MAX_INPUT_CHARS) {
      addToast('warning', i18n.t('input.inputTooLong', { length: submissionLength, max: MAX_INPUT_CHARS }));
      return;
    }

    // 任务运行中默认走 Intake；继续意图必须交给 session turn 分类器恢复执行链。
    if (shouldUseIntake && submissionText && !isNaturalContinueRequest(submissionText)) {
      await sendIntake(submissionText);
      return;
    }

    const requestId = generateId();
    vscode.postMessage({
      type: 'executeTask',
      text: submissionText,
      requestId,
      deepTask: false,
      skillName: null,
      followUpMode: isSending ? 'queue' : undefined,
      images: selectedImages.map((img) => ({
        name: img.name,
        dataUrl: img.dataUrl,
      })),
    });
    clearComposerState();
  }

  async function sendIntake(message: string) {
    if (intakeLoading) return;
    intakeLoading = true;
    try {
      const client = new RustDaemonClient(resolveAgentBaseUrl());
      const response = await client.postIntake({
        sessionId: messagesState.currentSessionId,
        message,
        contextTaskId: intakeContextTaskId,
      });
      handleIntakeResponse(response);
      await refreshTaskProjection(messagesState.currentSessionId);
      clearComposerState();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      addToast('error', `Intake 失败: ${msg}`);
    } finally {
      intakeLoading = false;
    }
  }

  function handleIntakeResponse(response: SessionIntakeResponseDto) {
    switch (response.classification) {
      case 'decision_answer':
        if (response.resolved) {
          addToast('success', `已确认选择: ${response.chosenOption}`);
        } else {
          addToast('warning', response.reason || '没有待处理的决策任务');
        }
        break;
      case 'pause':
        addToast('info', '任务已停止，进度已保存');
        break;
      case 'replan':
        addToast('info', `已触发重规划，取消 ${response.cancelledTaskIds?.length ?? 0} 个任务`);
        break;
      case 'supplement_context':
        addToast('success', '补充上下文已接收');
        break;
      case 'append_task':
        addToast('success', '已追加新任务');
        break;
      case 'new_objective':
        addToast('info', response.note || '新目标请通过新 session 提交');
        break;
      case 'general_chat':
        addToast('info', response.note || '普通聊天消息暂不写入任务图');
        break;
      default:
        addToast('info', '输入已处理');
    }
  }

  function insertNewlineAtCursor() {
    const textarea = inputTextareaEl;
    if (!textarea) {
      inputValue += '\n';
      return;
    }
    const selectionStart = textarea.selectionStart ?? textarea.value.length;
    const selectionEnd = textarea.selectionEnd ?? selectionStart;
    textarea.setRangeText('\n', selectionStart, selectionEnd, 'end');
    inputValue = textarea.value;
  }

  function isEnterKey(event: KeyboardEvent): boolean {
    return event.key === 'Enter' || event.code === 'Enter' || event.code === 'NumpadEnter';
  }

  // 处理键盘事件
  function handleKeydown(event: KeyboardEvent) {
    if (isEnterKey(event)) {
      // 输入法组合态下回车只用于上屏，不能误触发发送
      if (event.isComposing || event.keyCode === 229) {
        return;
      }
      const isAltEnter = event.altKey
        || event.getModifierState?.('Alt');
      if (isAltEnter) {
        event.preventDefault();
        insertNewlineAtCursor();
        return;
      }
      if (event.metaKey || event.ctrlKey || event.shiftKey) {
        event.preventDefault();
        return;
      }
      event.preventDefault();
      sendMessage();
      return;
    }
  }

  // 任务图运行时，输入框停止入口与任务面板共用同一条可恢复停止链路。
  async function stopTask() {
    if (stopLoading) return;
    stopLoading = true;
    try {
      if (shouldPauseTaskGraphFromComposer) {
        const projection = taskGraph.projection;
        const sessionId = currentSessionId?.trim();
        const rootTaskId = projection?.root_task.task_id ?? taskGraph.rootTaskId;
        if (sessionId && rootTaskId) {
          const client = new RustDaemonClient(resolveAgentBaseUrl());
          await client.pauseTask({ taskId: rootTaskId, sessionId });
          await refreshTaskProjection(sessionId);
          addToast('info', '任务已停止，进度已保存');
        }
        return;
      }
      vscode.postMessage({ type: 'interruptTask' });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `停止失败: ${message}`);
    } finally {
      stopLoading = false;
    }
  }

  function guideQueuedMessage(queuedMessageId: string) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId) return;
    markQueuedMessageAsGuide(normalizedId);
    vscode.postMessage({
      type: 'guideQueuedMessage',
      queuedMessageId: normalizedId,
    });
  }

  // 拖动调整大小
  function startResize(event: MouseEvent) {
    const startY = event.clientY;
    const startHeight = inputHeight;

    function onMouseMove(e: MouseEvent) {
      const delta = startY - e.clientY;
      const newHeight = Math.min(maxHeight, Math.max(minHeight, startHeight + delta));
      inputHeight = newHeight;
    }

    function onMouseUp() {
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
    }

    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  }

  // 🔧 处理粘贴事件（支持图片粘贴）
  function handlePaste(event: ClipboardEvent) {
    const items = event.clipboardData?.items;
    if (!items) return;

    let hasImage = false;

    for (const item of items) {
      if (!item.type.startsWith('image/')) continue;
      hasImage = true;

      if (selectedImages.length >= MAX_IMAGES) {
        addToast('warning', i18n.t('input.maxImages', { max: MAX_IMAGES }));
        break;
      }

      const file = item.getAsFile();
      if (!file) continue;

      if (file.size > MAX_IMAGE_SIZE) {
        addToast('warning', i18n.t('input.imageTooLarge', { size: (file.size / 1024 / 1024).toFixed(1) }));
        continue;
      }

      // 读取图片为 DataURL
      const reader = new FileReader();
      reader.onload = (e) => {
        const dataUrl = e.target?.result as string;
        if (dataUrl) {
          selectedImages = [...selectedImages, {
            id: generateId(),
            dataUrl,
            name: file.name || i18n.t('input.pastedImage', { index: selectedImages.length + 1 }),
          }];
          addToast('success', i18n.t('input.imageAdded'));
        }
      };
      reader.onerror = () => {
        addToast('error', i18n.t('input.imageReadFailed'));
      };
      reader.readAsDataURL(file);
    }

    if (hasImage) {
      event.preventDefault();
    }
  }

  // 🔧 删除已选图片
  function removeImage(imageId: string) {
    selectedImages = selectedImages.filter(img => img.id !== imageId);
  }

  // 🔧 清空所有图片
  function clearAllImages() {
    selectedImages = [];
  }
</script>

<div class="ia-container">
  {#if queuedMessages.length > 0}
    <div class="ia-queue-panel">
      <div class="ia-queue-header">
        <span class="ia-queue-header-title">
          <Icon name="clock" size={12} />
          <span>{i18n.t('input.queue.banner')}</span>
        </span>
        <span class="ia-queue-header-count">{queuedMessages.length}</span>
      </div>
      <div class="ia-queue-list">
        {#each queuedMessages as queued, index (queued.id)}
          <div class="ia-queue-item">
            <span class="ia-queue-index">{index + 1}</span>
            <span class="ia-queue-mode" class:guide={queued.mode === 'guide'}>
              {queued.mode === 'guide' ? i18n.t('input.queue.modeGuide') : i18n.t('input.queue.modeQueue')}
            </span>
            <div class="ia-queue-content" title={queued.content}>{queued.content}</div>
            {#if queued.mode !== 'guide'}
              <button
                type="button"
                class="ia-queue-guide"
                onclick={() => guideQueuedMessage(queued.id)}
                title={i18n.t('messageItem.guideQueuedTitle')}
              >
                <Icon name="send" size={11} />
                <span>{i18n.t('messageItem.guideQueued')}</span>
              </button>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}

  <div class="ia-wrapper" style="min-height: {inputHeight}px">
    <!-- 拖动调整大小 -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="ia-resize" onmousedown={startResize}></div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <textarea
      bind:value={inputValue}
      bind:this={inputTextareaEl}
      class="ia-textarea"
      data-testid="input-textarea"
      class:has-images={selectedImages.length > 0}
      placeholder={selectedImages.length > 0
        ? i18n.t('input.placeholderWithImages')
        : i18n.t('input.placeholderDefault')}
      disabled={sessionInputLocked || isInteractionBlocking}
      onkeydown={handleKeydown}
      onpaste={handlePaste}
    ></textarea>

    <!-- 图片预览 -->
    {#if selectedImages.length > 0}
      <div class="ia-images">
        {#each selectedImages as img (img.id)}
          <div class="ia-img-item">
            <img src={img.dataUrl} alt={img.name} class="ia-img-thumb" />
            <button class="ia-img-remove" onclick={() => removeImage(img.id)} title={i18n.t('input.remove')}>
              <Icon name="close" size={10} />
            </button>
          </div>
        {/each}
        {#if selectedImages.length > 1}
          <button class="ia-img-clear" onclick={clearAllImages} title={i18n.t('input.clearAllImages')}>{i18n.t('input.clearImages')}</button>
        {/if}
      </div>
    {/if}

    <div class="ia-actions">
      <div class="ia-left" aria-hidden="true"></div>

      <div class="ia-right">
        {#if isSending}
          {#if hasContent}
            <button
              class="ia-send ready"
              data-testid="input-followup-send-button"
              onclick={sendMessage}
              disabled={sendDisabled}
              title={sendButtonTitle}
            >
              <Icon name="send" size={14} />
            </button>
          {/if}
          <button
            class="ia-send stop"
            data-testid="input-stop-button"
            onclick={stopTask}
            disabled={stopLoading}
            title={shouldPauseTaskGraphFromComposer ? '停止当前任务，保留进度' : i18n.t('input.stop')}
          >
            <Icon name={stopLoading ? 'loader' : 'stop'} size={14} class={stopLoading ? 'spinning' : ''} />
          </button>
        {:else if hasContent}
          <!-- 空闲且有内容：显示发送按钮 -->
          <button
            class="ia-send ready"
            data-testid="input-send-button"
            onclick={sendMessage}
            disabled={sendDisabled}
            title={sendButtonTitle}
          >
            <Icon name="send" size={14} />
          </button>
        {:else}
          <!-- 无内容 + 空闲：显示禁用的发送按钮 -->
          <button
            class="ia-send"
            disabled
            title={i18n.t('input.send')}
          >
            <Icon name="send" size={14} />
          </button>
        {/if}
      </div>
    </div>
  </div>

</div>

<style>
  /* ============================================
     InputArea - 输入区域
     设计参考: ChatGPT / Claude Desktop 简约输入框
     前缀: ia-
     ============================================ */
  .ia-container {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    flex-shrink: 0;
    padding: var(--space-3) var(--space-4) var(--space-4) var(--space-4);
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    position: relative;
  }

  .ia-wrapper {
    display: flex;
    flex-direction: column;
    max-height: 50vh;
    background: var(--vscode-input-background);
    border: 1px solid color-mix(in srgb, var(--border) 60%, transparent);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-sm);
    transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
    /* 不使用 overflow:hidden — 允许模型下拉菜单溢出显示 */
  }

  .ia-wrapper:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px var(--primary-muted);
  }

  /* 拖拽调整：视觉 2px 指示器，交互区域 10px */
  .ia-resize {
    height: 10px;
    flex-shrink: 0;
    cursor: ns-resize;
    background: transparent;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background var(--transition-fast);
    border-radius: var(--radius-lg) var(--radius-lg) 0 0;
  }

  .ia-resize::after {
    content: '';
    width: 28px;
    height: 2px;
    background: var(--border);
    border-radius: 1px;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .ia-resize:hover { background: color-mix(in srgb, var(--primary) 8%, transparent); }
  .ia-resize:hover::after { opacity: 0.8; }

  /* 文本框 */
  .ia-textarea {
    flex: 1;
    min-height: 36px;
    width: 100%;
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-sm);
    line-height: var(--leading-relaxed);
    resize: none;
    border: none;
    background: transparent;
    color: var(--foreground);
    outline: none;
    font-family: inherit;
  }

  .ia-textarea::placeholder { color: var(--foreground-muted); }
  .ia-textarea:disabled { opacity: 0.5; cursor: not-allowed; }
  .ia-textarea.has-images { min-height: 36px; }

  /* 操作栏 */
  .ia-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 4px var(--space-2);
    gap: var(--space-1);
    flex-shrink: 0;
    border-radius: 0 0 var(--radius-lg) var(--radius-lg);
  }

  .ia-left, .ia-right {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  /* 发送按钮：圆形 */
  .ia-send {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    background: var(--surface-2);
    border: none;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .ia-send.ready { background: var(--primary); color: white; }
  .ia-send.ready:hover { background: var(--primary-hover); transform: scale(1.08); }
  .ia-send:disabled { opacity: 0.35; cursor: not-allowed; }
  .ia-send.stop { background: var(--error); color: white; animation: ia-pulse 1.2s ease-in-out infinite; }
  @keyframes ia-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.65; } }

  /* 图片预览 */
  .ia-images {
    display: flex;
    flex-wrap: nowrap;
    flex-shrink: 0;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    max-height: 90px;
    overflow-x: auto;
    overflow-y: hidden;
    border-top: 1px solid var(--border-subtle);
  }

  .ia-img-item {
    position: relative;
    width: 52px;
    height: 52px;
    border-radius: var(--radius-sm);
    overflow: hidden;
    border: 1px solid var(--border);
  }

  .ia-img-thumb { width: 100%; height: 100%; object-fit: cover; }

  .ia-img-remove {
    position: absolute;
    top: 2px;
    right: 2px;
    width: 16px;
    height: 16px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    background: rgba(0, 0, 0, 0.6);
    border: none;
    border-radius: 50%;
    color: white;
    cursor: pointer;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .ia-img-item:hover .ia-img-remove { opacity: 1; }
  .ia-img-remove:hover { background: var(--destructive); }

  .ia-img-clear {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-1) var(--space-2);
    font-size: var(--text-xs);
    background: transparent;
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .ia-img-clear:hover { border-color: var(--destructive); color: var(--destructive); }

  .ia-queue-panel {
    border: 1px solid color-mix(in srgb, var(--border) 78%, transparent);
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--surface-1) 96%, transparent);
    padding: 7px 9px;
    display: flex;
    flex-direction: column;
    gap: 7px;
    box-shadow: inset 0 1px 0 color-mix(in srgb, var(--foreground-muted) 6%, transparent);
  }

  .ia-queue-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }

  .ia-queue-header-title {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: color-mix(in srgb, var(--foreground) 84%, transparent);
    font-size: 12px;
    font-weight: var(--font-medium);
    line-height: 1.2;
  }

  .ia-queue-header-count {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 20px;
    height: 20px;
    padding: 0 6px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--surface-hover) 72%, transparent);
    border: 1px solid color-mix(in srgb, var(--border) 65%, transparent);
    color: var(--foreground-muted);
    font-size: 11px;
    font-weight: var(--font-semibold);
  }

  .ia-queue-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 124px;
    overflow-y: auto;
  }

  .ia-queue-item {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto;
    align-items: start;
    gap: 8px;
    padding: 6px 8px;
    border-radius: var(--radius-sm);
    border: 1px solid color-mix(in srgb, var(--border-subtle) 70%, transparent);
    background: color-mix(in srgb, var(--surface-2) 40%, var(--surface-1));
    min-height: 32px;
  }

  .ia-queue-mode {
    display: inline-flex;
    align-items: center;
    height: 17px;
    margin-top: 1px;
    padding: 0 6px;
    border-radius: var(--radius-full);
    border: 1px solid color-mix(in srgb, var(--primary) 28%, transparent);
    background: color-mix(in srgb, var(--primary) 8%, transparent);
    color: var(--primary);
    font-size: 10px;
    font-weight: var(--font-semibold);
    line-height: 1;
    white-space: nowrap;
  }

  .ia-queue-mode.guide {
    border-color: color-mix(in srgb, var(--warning) 34%, transparent);
    background: color-mix(in srgb, var(--warning) 10%, transparent);
    color: var(--warning);
  }

  .ia-queue-index {
    width: 16px;
    height: 16px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    margin-top: 1px;
    font-size: 10px;
    line-height: 1;
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--surface-hover) 75%, transparent);
    border: 1px solid color-mix(in srgb, var(--border) 68%, transparent);
  }

  .ia-queue-content {
    font-size: 12px;
    line-height: 1.3;
    color: var(--foreground);
    white-space: normal;
    overflow: hidden;
    word-break: break-word;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  .ia-queue-guide {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    margin-top: 0;
    padding: 0 7px;
    border: 1px solid color-mix(in srgb, var(--primary) 35%, transparent);
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--primary) 8%, transparent);
    color: var(--primary);
    font-size: 10px;
    font-weight: var(--font-semibold);
    line-height: 1;
    cursor: pointer;
    opacity: 0;
    transform: translateX(3px);
    transition: opacity 120ms ease, transform 120ms ease, background 120ms ease;
  }

  .ia-queue-item:hover .ia-queue-guide,
  .ia-queue-item:focus-within .ia-queue-guide {
    opacity: 1;
    transform: translateX(0);
  }

  .ia-queue-guide:hover {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  @media (hover: none) {
    .ia-queue-guide {
      opacity: 1;
      transform: none;
    }
  }

</style>
