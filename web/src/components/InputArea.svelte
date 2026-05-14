<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import {
    addToast,
    getActiveInteractionType,
    getQueuedMessages,
    messagesState,
    removeQueuedMessage,
  } from '../stores/messages.svelte';
  import { getTaskGraphState, refreshTaskProjection } from '../stores/task-graph-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { enhanceAgentPrompt, resolveAgentBaseUrl } from '../web/agent-api';
  import { categoryLabel, listTaskTemplates, type ResolvedTemplate } from '../lib/task-templates';
  import Icon from './Icon.svelte';
  import { generateId } from '../lib/utils';
  import { i18n } from '../stores/i18n.svelte';

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

  let stopLoading = $state(false);
  let enhanceLoading = $state(false);
  let templatesOpen = $state(false);
  const templates = $derived<ResolvedTemplate[]>(templatesOpen ? listTaskTemplates() : []);
  const groupedTemplates = $derived.by(() => {
    if (!templatesOpen) return [] as Array<{ category: ResolvedTemplate['category']; label: string; items: ResolvedTemplate[] }>;
    const buckets = new Map<ResolvedTemplate['category'], ResolvedTemplate[]>();
    for (const tpl of templates) {
      const list = buckets.get(tpl.category) ?? [];
      list.push(tpl);
      buckets.set(tpl.category, list);
    }
    return Array.from(buckets.entries()).map(([category, items]) => ({
      category,
      label: categoryLabel(category),
      items,
    }));
  });

  const currentSessionId = $derived(messagesState.currentSessionId);
  const taskGraph = $derived(getTaskGraphState(currentSessionId));

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
    sessionInputLocked || isInteractionBlocking
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

  onMount(() => {
    function handleFillComposer(event: Event) {
      const text = (event as CustomEvent<{ text?: string }>).detail?.text;
      if (typeof text !== 'string' || !text.trim()) return;
      inputValue = text;
      queueMicrotask(() => {
        const el = inputTextareaEl;
        if (!el) return;
        el.focus();
        const cursor = text.length;
        try { el.setSelectionRange(cursor, cursor); } catch { /* ignore */ }
      });
    }
    window.addEventListener('magi:fillComposer', handleFillComposer as EventListener);
    return () => window.removeEventListener('magi:fillComposer', handleFillComposer as EventListener);
  });

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

  // 发送消息（支持图片附件）。
  // 空闲时直接执行；正在响应时自动进入排队，由 bridge 在当前轮结束后逐条提交。
  async function sendMessage() {
    const rawContent = resolveComposerRawContent();
    const normalizedContent = rawContent.trim();
    if ((!normalizedContent && selectedImages.length === 0) || isInteractionBlocking) return;
    if (isSending && selectedImages.length > 0) {
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

    const requestId = generateId();
    vscode.postMessage({
      type: 'executeTask',
      text: submissionText,
      requestId,
      skillName: null,
      followUpMode: isSending ? 'queue' : undefined,
      images: selectedImages.map((img) => ({
        name: img.name,
        dataUrl: img.dataUrl,
      })),
    });
    clearComposerState();
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
    vscode.postMessage({
      type: 'guideQueuedMessage',
      queuedMessageId: normalizedId,
    });
  }

  function deleteQueuedMessage(queuedMessageId: string) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId) return;
    removeQueuedMessage(normalizedId);
  }

  // 修改：取出排队消息内容回填到输入框，并从队列移除；用户重新点击发送后会按当前会话状态再次进入排队。
  function editQueuedMessage(queuedMessageId: string) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId) return;
    const target = messagesState.queuedMessages.find((message) => message.id === normalizedId);
    if (!target) return;
    const text = (target.text ?? target.content ?? '').toString();
    removeQueuedMessage(normalizedId);
    inputValue = text;
    queueMicrotask(() => {
      const el = inputTextareaEl;
      if (!el) return;
      el.focus();
      const cursor = text.length;
      try { el.setSelectionRange(cursor, cursor); } catch { /* ignore */ }
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

  function toggleTemplates() {
    templatesOpen = !templatesOpen;
  }
  function applyTemplate(tpl: ResolvedTemplate) {
    const prompt = (tpl.prompt || '').trim();
    if (!prompt) {
      templatesOpen = false;
      return;
    }
    inputValue = prompt;
    templatesOpen = false;
    queueMicrotask(() => {
      const el = inputTextareaEl;
      if (!el) return;
      el.focus();
      const cursor = prompt.length;
      try { el.setSelectionRange(cursor, cursor); } catch { /* ignore */ }
    });
  }

  // Prompt enhance：调用后端模型重写当前 textarea 文本
  async function enhancePromptHandler() {
    const draft = inputValue.trim();
    if (enhanceLoading || !draft) return;
    enhanceLoading = true;
    try {
      const result = await enhanceAgentPrompt(draft);
      const next = (result?.enhancedPrompt ?? '').trim();
      if (!next) {
        addToast('warning', result?.error || i18n.t('input.enhance.empty'));
        return;
      }
      inputValue = next;
      queueMicrotask(() => {
        const el = inputTextareaEl;
        if (!el) return;
        el.focus();
        const cursor = next.length;
        try { el.setSelectionRange(cursor, cursor); } catch { /* ignore */ }
      });
      addToast('success', i18n.t('input.enhance.success'));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      addToast('error', i18n.t('input.enhance.failed', { message }));
    } finally {
      enhanceLoading = false;
    }
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
            <div class="ia-queue-content" title={queued.content}>{queued.content}</div>
            <div class="ia-queue-actions">
              <button
                type="button"
                class="ia-queue-action"
                onclick={() => guideQueuedMessage(queued.id)}
                title={i18n.t('messageItem.guideQueuedTitle')}
                aria-label={i18n.t('messageItem.guideQueued')}
              >
                <Icon name="send" size={12} />
              </button>
              <button
                type="button"
                class="ia-queue-action"
                onclick={() => editQueuedMessage(queued.id)}
                title={i18n.t('input.queue.edit')}
                aria-label={i18n.t('input.queue.edit')}
              >
                <Icon name="edit" size={12} />
              </button>
              <button
                type="button"
                class="ia-queue-action danger"
                onclick={() => deleteQueuedMessage(queued.id)}
                title={i18n.t('input.queue.delete')}
                aria-label={i18n.t('input.queue.delete')}
              >
                <Icon name="trash" size={12} />
              </button>
            </div>
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
      <div class="ia-left">
        <div class="ia-templates-wrap">
          <button
            type="button"
            class="ia-enhance"
            class:active={templatesOpen}
            onclick={toggleTemplates}
            disabled={sessionInputLocked || isInteractionBlocking}
            title={i18n.t('input.templates.title')}
            aria-label={i18n.t('input.templates.title')}
            aria-expanded={templatesOpen}
          >
            <Icon name="lightbulb" size={14} />
            <span>{i18n.t('input.templates.label')}</span>
          </button>
          {#if templatesOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-templates-backdrop" onclick={() => (templatesOpen = false)}></div>
            <div class="ia-templates-popover" role="menu">
              <div class="ia-templates-header">{i18n.t('input.templates.heading')}</div>
              {#each groupedTemplates as group (group.category)}
                <div class="ia-templates-group">
                  <div class="ia-templates-group-label">{group.label}</div>
                  {#each group.items as tpl (tpl.id)}
                    <button
                      type="button"
                      class="ia-templates-item"
                      onclick={() => applyTemplate(tpl)}
                      title={tpl.description}
                    >
                      <span class="ia-templates-item-label">{tpl.label}</span>
                      <span class="ia-templates-item-desc">{tpl.description}</span>
                    </button>
                  {/each}
                </div>
              {/each}
            </div>
          {/if}
        </div>
        <button
          type="button"
          class="ia-enhance"
          class:loading={enhanceLoading}
          onclick={enhancePromptHandler}
          disabled={enhanceLoading || !inputValue.trim() || sessionInputLocked || isInteractionBlocking}
          title={i18n.t('input.enhance.title')}
          aria-label={i18n.t('input.enhance.title')}
        >
          <Icon name={enhanceLoading ? 'loader' : 'enhance'} size={14} class={enhanceLoading ? 'spinning' : ''} />
          <span>{i18n.t('input.enhance.label')}</span>
        </button>
      </div>

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

  .ia-enhance {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 8px;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: 11px;
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .ia-enhance:hover:not(:disabled) {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
  }
  .ia-enhance:disabled { opacity: 0.4; cursor: not-allowed; }
  .ia-enhance.loading { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 50%, transparent); }
  .ia-enhance.active {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    border-color: color-mix(in srgb, var(--primary) 42%, transparent);
    color: var(--primary);
  }

  .ia-templates-wrap {
    position: relative;
    display: inline-flex;
  }
  .ia-templates-backdrop {
    position: fixed;
    inset: 0;
    background: transparent;
    z-index: 30;
  }
  .ia-templates-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
    z-index: 31;
    width: 320px;
    max-height: 360px;
    overflow-y: auto;
    padding: 8px;
    background: color-mix(in srgb, var(--background) 100%, white 8%);
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    border: 1px solid color-mix(in srgb, var(--border) 80%, var(--foreground) 20%);
    border-radius: var(--radius-md);
    box-shadow: 0 14px 40px rgba(0, 0, 0, 0.45), 0 2px 8px rgba(0, 0, 0, 0.22);
  }
  .ia-templates-header {
    font-size: 11px;
    color: var(--foreground-muted);
    padding: 2px 6px 6px;
  }
  .ia-templates-group {
    padding: 4px 0;
  }
  .ia-templates-group + .ia-templates-group {
    border-top: 1px dashed var(--border-subtle);
    margin-top: 4px;
  }
  .ia-templates-group-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--foreground-muted);
    padding: 4px 6px 2px;
  }
  .ia-templates-item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    width: 100%;
    padding: 6px 8px;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm, 6px);
    cursor: pointer;
    text-align: left;
    color: var(--foreground);
    transition: background var(--transition-fast);
  }
  .ia-templates-item:hover {
    background: color-mix(in srgb, var(--primary) 10%, transparent);
  }
  .ia-templates-item-label {
    font-size: 12px;
    font-weight: var(--font-medium, 500);
  }
  .ia-templates-item-desc {
    font-size: 11px;
    color: var(--foreground-muted);
    line-height: 1.4;
  }

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
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: start;
    gap: 8px;
    padding: 6px 8px;
    border-radius: var(--radius-sm);
    border: 1px solid color-mix(in srgb, var(--border-subtle) 70%, transparent);
    background: color-mix(in srgb, var(--surface-2) 40%, var(--surface-1));
    min-height: 32px;
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

  .ia-queue-actions {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-top: 0;
    opacity: 0;
    transform: translateX(3px);
    transition: opacity 120ms ease, transform 120ms ease;
  }

  .ia-queue-item:hover .ia-queue-actions,
  .ia-queue-item:focus-within .ia-queue-actions {
    opacity: 1;
    transform: translateX(0);
  }

  .ia-queue-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--surface-1) 92%, transparent);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease, border-color 120ms ease;
  }

  .ia-queue-action:hover {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    border-color: color-mix(in srgb, var(--primary) 40%, transparent);
    color: var(--primary);
  }

  .ia-queue-action.danger:hover {
    background: color-mix(in srgb, var(--error) 14%, transparent);
    border-color: color-mix(in srgb, var(--error) 45%, transparent);
    color: var(--error);
  }

  @media (hover: none) {
    .ia-queue-actions {
      opacity: 1;
      transform: none;
    }
  }

</style>
