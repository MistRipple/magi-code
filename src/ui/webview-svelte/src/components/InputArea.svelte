<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { getState, addThreadMessage, addToast, getActiveInteractionType, addPendingRequest } from '../stores/messages.svelte';
  import type { StandardMessage } from '../../../../protocol/message-protocol';
  import { MessageCategory } from '../../../../protocol/message-protocol';
  import Icon from './Icon.svelte';
  import { generateId } from '../lib/utils';

  const appState = getState();

  // 输入内容
  let inputValue = $state('');

  // 模式和模型选择
  let selectedModel = $state('');
  let interactionMode = $state<'ask' | 'auto'>('auto');

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

  // 是否正在发送
  const isSending = $derived(appState.isProcessing);
  const activeInteraction = $derived(getActiveInteractionType());
  const isInteractionBlocking = $derived(Boolean(activeInteraction));
  const MAX_INPUT_CHARS = 10000;

  // 发送消息（支持图片附件）
  function sendMessage() {
    const content = inputValue.trim();
    // 允许只发送图片（无文字）或只发送文字
    if ((!content && selectedImages.length === 0) || isSending || isInteractionBlocking) return;
    if (content.length > MAX_INPUT_CHARS) {
      addToast('warning', `输入内容过长（${content.length} 字符），请控制在 ${MAX_INPUT_CHARS} 字符以内`);
      return;
    }

    const requestId = generateId();
    // 🔧 乐观更新：立即设置处理状态，用户无需等待后端响应即可看到 Loading 动画
    addPendingRequest(requestId);

    // 构建用户消息（包含图片预览信息）
    const displayContent = selectedImages.length > 0
      ? `${content}${content ? '\n' : ''}[附件: ${selectedImages.length} 张图片]`
      : content;

    addThreadMessage({
      id: generateId(),
      role: 'user',
      source: 'orchestrator',
      content: displayContent,
      timestamp: Date.now(),
      isStreaming: false,
      isComplete: true,
    });

    // 发送到后端（包含图片数据）
    vscode.postMessage({
      type: 'executeTask',
      prompt: content || '请分析这些图片',  // 无文字时使用默认提示
      mode: interactionMode,
      agent: selectedModel || undefined,
      requestId,
      images: selectedImages.map(img => ({ dataUrl: img.dataUrl })),
    });

    inputValue = '';
    selectedImages = [];  // 清空已选图片
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
    interactionMode = mode;
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
    <textarea
      bind:value={inputValue}
      class="input-box"
      class:has-images={selectedImages.length > 0}
      placeholder={selectedImages.length > 0 ? "添加描述（可选）..." : "描述你的任务... (Ctrl+V 粘贴图片)"}
      disabled={isSending}
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
        <div class="mode-toggle">
          <button
            class="mode-toggle-option"
            class:active={interactionMode === 'ask'}
            onclick={() => setMode('ask')}
          >Ask</button>
          <button
            class="mode-toggle-option"
            class:active={interactionMode === 'auto'}
            onclick={() => setMode('auto')}
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

        <!-- 发送/停止按钮 -->
        {#if isSending}
          <button class="send-btn stop" onclick={stopTask} title="停止">
            <Icon name="stop" size={14} />
          </button>
        {:else}
          <button
            class="send-btn"
            class:ready={(inputValue.trim() || selectedImages.length > 0) && !isInteractionBlocking}
            onclick={sendMessage}
            disabled={(!inputValue.trim() && selectedImages.length === 0) || isInteractionBlocking}
            title={isInteractionBlocking ? `等待处理：${activeInteraction}` : '发送 (Cmd+Enter)'}
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
  .mode-toggle-option.active { background: var(--primary); color: white; }
  .mode-toggle-option:hover:not(.active) { background: var(--surface-hover); color: var(--foreground); }

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
