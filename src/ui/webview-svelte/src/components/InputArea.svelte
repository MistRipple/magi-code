<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import {
    addToast,
    getActiveInteractionType,
    getInteractionMode,
    getRequestedInteractionMode,
    isInteractionModeSyncing,
    requestInteractionMode,
    messagesState,
  } from '../stores/messages.svelte';
  import type { StandardMessage } from '../../../../protocol/message-protocol';
  import { MessageCategory } from '../../../../protocol/message-protocol';
  import Icon from './Icon.svelte';
  import { generateId } from '../lib/utils';

  // 输入内容
  let inputValue = $state('');

  // 模式和模型选择
  let selectedModel = $state('');
  const interactionMode = $derived.by(() => getInteractionMode());
  const requestedInteractionMode = $derived.by(() => getRequestedInteractionMode());
  const isModeSyncing = $derived.by(() => isInteractionModeSyncing());

  // 拖动调整大小相关
  let inputHeight = $state(120); // 默认高度增加到 120px
  const minHeight = 80;
  const maxHeight = 400;

  // 增强按钮状态
  let isEnhancing = $state(false);

  // 🔧 图片上传相关状态
  let selectedImages = $state<Array<{ id: string; dataUrl: string; name: string }>>([]);
  const MAX_IMAGES = 5;  // 最多支持 5 张图片
  const MAX_IMAGE_SIZE = 10 * 1024 * 1024;  // 单张图片最大 10MB

  // 🔧 修复响应式：直接访问 messagesState 属性确保正确追踪
  const isSending = $derived(messagesState.isProcessing);
  const activeInteraction = $derived.by(() => getActiveInteractionType());
  const isInteractionBlocking = $derived.by(() => Boolean(activeInteraction));
  const MAX_INPUT_CHARS = 10000;

  // P0-2: 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (inputValue.trim().length > 0) return true;
    // 执行中补充指令不支持图片，避免“有内容可发送”与实际能力不一致
    if (isSending) return false;
    return selectedImages.length > 0;
  });
  const showStopButton = $derived(isSending && !hasContent);

  // P1-1: 限频机制 - 执行中 1 秒/条，空闲 300ms/条
  let lastSendTime = $state(0);
  const RATE_LIMIT_IDLE = 300;      // 空闲状态：300ms
  const RATE_LIMIT_PROCESSING = 1000;  // 执行中：1 秒

  // 发送消息（支持图片附件）
  // 执行中发送输入 = 打断当前执行并按新输入重新开始
  function sendMessage() {
    if (isModeSyncing) {
      addToast('warning', '交互模式切换尚未完成，请稍候再发送');
      return;
    }

    const content = inputValue.trim();
    // 允许只发送图片（无文字）或只发送文字
    // 执行中允许发送，后端将执行“打断并重启”
    if ((!content && selectedImages.length === 0) || isInteractionBlocking) return;

    // P1-1: 限频检查
    const now = Date.now();
    const minInterval = isSending ? RATE_LIMIT_PROCESSING : RATE_LIMIT_IDLE;
    if (now - lastSendTime < minInterval) {
      addToast('warning', '发送过快，请稍后再试');
      return;
    }
    lastSendTime = now;

    if (content.length > MAX_INPUT_CHARS) {
      addToast('warning', `输入内容过长（${content.length} 字符），请控制在 ${MAX_INPUT_CHARS} 字符以内`);
      return;
    }

    // 根据是否正在执行，区分发送新任务还是打断重启
    if (isSending) {
      // 执行中：发送重启输入（后端立即中断并重启）
      // 注意：执行中暂不支持图片
      if (selectedImages.length > 0) {
        addToast('warning', '执行中暂不支持发送图片，请先停止当前任务');
        return;
      }
      vscode.postMessage({
        type: 'appendMessage',
        taskId: '',  // 后端自动关联当前任务
        content: content,
      });
    } else {
      // 空闲状态：发送新任务
      const requestId = generateId();
      vscode.postMessage({
        type: 'executeTask',
        prompt: content || '请分析这些图片',
        mode: interactionMode,
        agent: selectedModel || undefined,
        requestId,
        images: selectedImages.map(img => ({ dataUrl: img.dataUrl })),
      });
    }

    // 清理输入状态
    inputValue = '';
    selectedImages = [];
  }

  // 处理键盘事件
  function handleKeydown(event: KeyboardEvent) {
    if ((event.key === 'Enter' && event.metaKey) || (event.key === 'Enter' && event.ctrlKey)) {
      event.preventDefault();
      sendMessage();
    }
  }

  // 停止任务
  function stopTask() {
    vscode.postMessage({ type: 'interruptTask' });
  }

  // 增强提示词 - 直接替换输入框内容
  function enhancePrompt() {
    const content = inputValue.trim();
    if (!content || isEnhancing) return;
    isEnhancing = true;
    vscode.postMessage({ type: 'enhancePrompt', prompt: content });
  }

  // 切换模式
  function setMode(mode: 'ask' | 'auto') {
    if (isModeSyncing && requestedInteractionMode === mode) {
      return;
    }
    requestInteractionMode(mode);
    vscode.postMessage({ type: 'setInteractionMode', mode });
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

  // 打开技能弹窗
  function openSkillPopup() {
    window.dispatchEvent(new CustomEvent('openSkillPopup'));
  }

  // 🔧 处理粘贴事件（支持图片粘贴）
  function handlePaste(event: ClipboardEvent) {
    const items = event.clipboardData?.items;
    if (!items) return;

    for (const item of items) {
      if (item.type.startsWith('image/')) {
        event.preventDefault();  // 阻止默认粘贴行为

        if (selectedImages.length >= MAX_IMAGES) {
          addToast('warning', `最多支持 ${MAX_IMAGES} 张图片`);
          return;
        }

        const file = item.getAsFile();
        if (!file) continue;

        if (file.size > MAX_IMAGE_SIZE) {
          addToast('warning', `图片过大（${(file.size / 1024 / 1024).toFixed(1)}MB），请控制在 10MB 以内`);
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
              name: file.name || `粘贴图片_${selectedImages.length + 1}`,
            }];
            addToast('success', '图片已添加');
          }
        };
        reader.onerror = () => {
          addToast('error', '图片读取失败');
        };
        reader.readAsDataURL(file);
        break;  // 一次只处理一张图片
      }
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

  onMount(() => {
    const unsubscribe = vscode.onMessage((msg) => {
      if (msg.type !== 'unifiedMessage') return;
      const standard = msg.message as StandardMessage;
      if (!standard || standard.category !== MessageCategory.DATA || !standard.data) return;
      if (standard.data.dataType !== 'promptEnhanced') return;

      const payload = standard.data.payload as { enhancedPrompt?: string; error?: string };
      isEnhancing = false;
      if (payload?.error) {
        addToast('error', payload.error);
      } else {
        const enhancedPrompt = typeof payload?.enhancedPrompt === 'string' ? payload.enhancedPrompt : '';
        if (enhancedPrompt) {
          inputValue = enhancedPrompt;
          addToast('success', '提示词已增强');
        }
      }
    });
    return () => unsubscribe();
  });
</script>

<div class="input-container">
  <div class="input-wrapper" style="height: {inputHeight}px">
    <!-- 拖动调整大小的条 -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="input-resize-bar" onmousedown={startResize}></div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- P0-1: 执行中仍可输入，仅在需要用户确认时禁用 -->
    <textarea
      bind:value={inputValue}
      class="input-box"
      class:has-images={selectedImages.length > 0}
      placeholder={selectedImages.length > 0 ? "添加描述（可选）..." : "描述你的任务... (Ctrl+V 粘贴图片)"}
      disabled={isInteractionBlocking}
      onkeydown={handleKeydown}
      onpaste={handlePaste}
    ></textarea>

    <!-- 🔧 图片预览区域 -->
    {#if selectedImages.length > 0}
      <div class="image-preview-area">
        {#each selectedImages as img (img.id)}
          <div class="image-preview-item">
            <img src={img.dataUrl} alt={img.name} class="preview-thumbnail" />
            <button
              class="remove-image-btn"
              onclick={() => removeImage(img.id)}
              title="移除图片"
            >
              <Icon name="close" size={12} />
            </button>
          </div>
        {/each}
        {#if selectedImages.length > 1}
          <button class="clear-all-images-btn" onclick={clearAllImages} title="清空所有图片">
            清空
          </button>
        {/if}
      </div>
    {/if}

    <div class="input-actions">
      <div class="input-actions-left">
        <!-- 技能按钮 -->
        <button class="icon-btn" onclick={openSkillPopup} title="使用 Skill">
          <Icon name="skill" size={16} />
        </button>

        <!-- 模型选择器 -->
        <select class="model-selector" bind:value={selectedModel} title="选择模型">
          <option value="">自动</option>
          <option value="claude">Claude</option>
          <option value="codex">Codex</option>
          <option value="gemini">Gemini</option>
        </select>

        <!-- 模式切换 -->
        <div class="mode-toggle" class:syncing={isModeSyncing}>
          <button
            class="mode-toggle-option"
            class:active={interactionMode === 'ask'}
            class:pending={requestedInteractionMode === 'ask' && isModeSyncing}
            onclick={() => setMode('ask')}
            disabled={isModeSyncing && requestedInteractionMode !== 'ask'}
            title={isModeSyncing && requestedInteractionMode === 'ask' ? '正在切换到 Ask…' : 'Ask'}
          >Ask</button>
          <button
            class="mode-toggle-option"
            class:active={interactionMode === 'auto'}
            class:pending={requestedInteractionMode === 'auto' && isModeSyncing}
            onclick={() => setMode('auto')}
            disabled={isModeSyncing && requestedInteractionMode !== 'auto'}
            title={isModeSyncing && requestedInteractionMode === 'auto' ? '正在切换到 Auto…' : 'Auto'}
          >Auto</button>
        </div>
      </div>

      <div class="input-actions-right">
        <!-- 增强按钮 -->
        <button
          class="enhance-btn"
          class:enhancing={isEnhancing}
          onclick={enhancePrompt}
          title="增强提示 (AI 优化)"
          disabled={!inputValue.trim() || isEnhancing}
        >
          <span class="enhance-icon" class:spinning={isEnhancing}>
            <Icon name={isEnhancing ? 'loader' : 'enhance'} size={12} />
          </span>
          <span class="enhance-text">{isEnhancing ? '增强中...' : '增强'}</span>
        </button>

        <!-- 按钮双态逻辑 -->
        <!-- 执行中有内容=发送（打断重启），执行中无内容=停止 -->
        <!-- 空闲时始终显示发送按钮 -->
        {#if showStopButton}
          <button class="send-btn stop" onclick={stopTask} title="停止">
            <Icon name="stop" size={14} />
          </button>
        {:else}
          <button
            class="send-btn"
            class:ready={hasContent && !isInteractionBlocking && !isModeSyncing}
            onclick={sendMessage}
            disabled={!hasContent || isInteractionBlocking || isModeSyncing}
            title={isModeSyncing
              ? '交互模式切换中，请稍候'
              : (isInteractionBlocking ? `等待处理：${activeInteraction}` : (isSending ? '打断并重启执行 (Cmd+Enter)' : '发送 (Cmd+Enter)'))}
          >
            <Icon name="send" size={14} />
          </button>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .input-container {
    flex-shrink: 0;
    padding: var(--space-3) var(--space-4);
    background: var(--background);
    border-top: 1px solid var(--border);
  }

  .input-wrapper {
    display: flex;
    flex-direction: column;
    /* 🔧 使用 VS Code 输入框背景色，自动适配浅色/深色主题 */
    background: var(--vscode-input-background);
    border: 1px solid var(--vscode-input-border, var(--border));
    border-radius: var(--radius-lg);
    transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
    overflow: hidden;
  }

  .input-wrapper:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 2px rgba(59, 130, 246, 0.1);
  }

  .input-resize-bar {
    height: 8px;
    cursor: ns-resize;
    background: transparent;
    border-bottom: 1px solid var(--border-subtle);
    transition: background var(--transition-fast);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .input-resize-bar::after {
    content: '';
    width: 32px;
    height: 3px;
    background: var(--border);
    border-radius: 2px;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .input-resize-bar:hover { background: var(--surface-hover); }
  .input-resize-bar:hover::after { opacity: 1; }

  .input-box {
    flex: 1;
    width: 100%;
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-base);
    line-height: var(--leading-relaxed);
    resize: none;
    border: none;
    background: transparent;
    color: var(--foreground);
    outline: none;
    font-family: inherit;
  }

  .input-box::placeholder { color: var(--foreground-muted); }
  .input-box:disabled { opacity: 0.5; cursor: not-allowed; }

  .input-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-2) var(--space-3);
    /* 🔧 移除分层效果，与输入区域保持纯色一致 */
    background: transparent;
    gap: var(--space-2);
  }

  .input-actions-left, .input-actions-right {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  /* 🔧 统一按钮高度为 28px */
  .model-selector {
    height: 28px;
    padding: 0 var(--space-3);
    font-size: var(--text-xs);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground);
    cursor: pointer;
  }
  .model-selector:focus { outline: none; border-color: var(--primary); }

  .mode-toggle {
    display: flex;
    height: 28px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    overflow: hidden;
    border: 1px solid var(--border);
  }

  .mode-toggle-option {
    display: flex;
    align-items: center;
    padding: 0 var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .mode-toggle.syncing {
    border-color: var(--warning);
  }
  .mode-toggle-option.active { background: var(--primary); color: white; }
  .mode-toggle-option.pending {
    position: relative;
    background: color-mix(in srgb, var(--warning) 22%, transparent);
    color: var(--warning);
  }
  .mode-toggle-option.pending::after {
    content: '';
    position: absolute;
    right: 4px;
    top: 50%;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: currentColor;
    transform: translateY(-50%);
  }
  .mode-toggle-option:hover:not(.active):not(:disabled) { background: var(--surface-hover); color: var(--foreground); }
  .mode-toggle-option:disabled { opacity: 0.6; cursor: not-allowed; }

  /* 增强按钮 - 统一高度 28px */
  .enhance-btn {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    height: 28px;
    padding: 0 var(--space-3);
    font-size: var(--text-xs);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .enhance-btn:hover:not(:disabled) { background: var(--surface-hover); color: var(--foreground); border-color: var(--primary); }
  .enhance-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .enhance-btn.enhancing { border-color: var(--info); color: var(--info); }
  .enhance-icon { display: flex; }
  .enhance-icon.spinning { animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .enhance-text { font-weight: var(--font-medium); }

  /* 发送按钮 - 统一高度 28px */
  .send-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .send-btn.ready { background: var(--primary); border-color: var(--primary); color: white; }
  .send-btn.ready:hover { background: var(--primary-hover); transform: scale(1.05); }
  .send-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .send-btn.stop { background: var(--error); border-color: var(--error); color: white; animation: pulse 1s ease-in-out infinite; }
  @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.7; } }

  /* 图标按钮 - 统一高度 28px */
  .icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .icon-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
    border-color: var(--primary);
  }

  /* 🔧 图片预览区域样式 */
  .image-preview-area {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-top: 1px solid var(--border-subtle);
    background: var(--surface-1);
  }

  .image-preview-item {
    position: relative;
    width: 60px;
    height: 60px;
    border-radius: var(--radius-sm);
    overflow: hidden;
    border: 1px solid var(--border);
  }

  .preview-thumbnail {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .remove-image-btn {
    position: absolute;
    top: 2px;
    right: 2px;
    width: 18px;
    height: 18px;
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

  .image-preview-item:hover .remove-image-btn {
    opacity: 1;
  }

  .remove-image-btn:hover {
    background: var(--destructive);
  }

  .clear-all-images-btn {
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

  .clear-all-images-btn:hover {
    border-color: var(--destructive);
    color: var(--destructive);
  }

  .input-box.has-images {
    min-height: 40px;
  }
</style>
