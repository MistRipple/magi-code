<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { getState, addThreadMessage } from '../stores/messages.svelte';
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

  // 是否正在发送
  const isSending = $derived(appState.isProcessing);

  // 发送消息
  function sendMessage() {
    const content = inputValue.trim();
    if (!content || isSending) return;

    addThreadMessage({
      id: generateId(),
      role: 'user',
      source: 'orchestrator',
      content,
      timestamp: Date.now(),
      isStreaming: false,
      isComplete: true,
    });

    vscode.postMessage({
      type: 'executeTask',
      prompt: content,
      mode: interactionMode,
      agent: selectedModel || undefined,
      requestId: generateId(),
    });

    inputValue = '';
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

  // 增强提示词
  async function enhancePrompt() {
    const content = inputValue.trim();
    if (!content || isEnhancing) return;
    isEnhancing = true;
    vscode.postMessage({ type: 'enhancePrompt', content });
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

  onMount(() => {
    // 监听增强结果
    const handler = (event: MessageEvent) => {
      const msg = event.data;
      if (msg.type === 'promptEnhanced') {
        const enhancedPrompt = typeof msg.enhancedPrompt === 'string' ? msg.enhancedPrompt : '';
        if (enhancedPrompt) {
          inputValue = enhancedPrompt;
        }
        isEnhancing = false;
      }
    };
    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  });
</script>

<div class="input-container">
  <div class="input-wrapper" style="height: {inputHeight}px">
    <!-- 拖动调整大小的条 -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="input-resize-bar" onmousedown={startResize}></div>

    <textarea
      bind:value={inputValue}
      class="input-box"
      placeholder="描述你的任务..."
      disabled={isSending}
      onkeydown={handleKeydown}
    ></textarea>

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
            class:ready={inputValue.trim()}
            onclick={sendMessage}
            disabled={!inputValue.trim()}
            title="发送 (Cmd+Enter)"
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
</style>
