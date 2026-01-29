<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { getState } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';

  const appState = getState();

  // 输入内容
  let inputValue = $state('');

  // 模式和模型选择
  let selectedModel = $state('');
  let interactionMode = $state<'ask' | 'auto'>('auto');

  // 拖动调整大小相关
  let inputHeight = $state(80);
  const minHeight = 60;
  const maxHeight = 300;

  // 是否正在发送
  const isSending = $derived(appState.isProcessing);

  // 发送消息
  function sendMessage() {
    const content = inputValue.trim();
    if (!content || isSending) return;

    vscode.postMessage({
      type: 'sendMessage',
      content,
      model: selectedModel || undefined,
      mode: interactionMode,
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
    vscode.postMessage({ type: 'stopTask' });
  }

  // 打开技能弹窗
  function openSkillPopup() {
    vscode.postMessage({ type: 'openSkillPopup' });
  }

  // 增强提示词
  function enhancePrompt() {
    const content = inputValue.trim();
    if (!content) return;
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

  onMount(() => {
    // 监听增强结果
    window.addEventListener('message', (event) => {
      const msg = event.data;
      if (msg.type === 'enhancedPrompt' && msg.content) {
        inputValue = msg.content;
      }
    });
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
          <Icon name="skill" size={14} />
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
        <button class="enhance-btn" onclick={enhancePrompt} title="增强提示 (AI 优化)" disabled={!inputValue.trim()}>
          <Icon name="enhance" size={12} />
          <span class="enhance-text">增强</span>
        </button>

        <!-- 发送/停止按钮 -->
        {#if isSending}
          <button class="send-btn stop" onclick={stopTask} title="停止">
            <Icon name="stop" size={14} />
          </button>
        {:else}
          <button class="send-btn" onclick={sendMessage} disabled={!inputValue.trim()} title="发送 (Cmd+Enter)">
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
    padding: var(--space-4);
    background: var(--background);
  }

  .input-wrapper {
    display: flex;
    flex-direction: column;
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    transition: border-color var(--transition-fast);
    overflow: hidden;
  }

  .input-wrapper:focus-within {
    border-color: var(--primary);
  }

  .input-resize-bar {
    height: 6px;
    cursor: ns-resize;
    background: transparent;
    border-bottom: 1px solid var(--border-subtle);
    transition: background var(--transition-fast);
  }

  .input-resize-bar:hover {
    background: var(--surface-hover);
  }

  .input-box {
    flex: 1;
    width: 100%;
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    resize: none;
    border: none;
    background: transparent;
    color: var(--foreground);
    outline: none;
  }

  .input-box::placeholder {
    color: var(--foreground-muted);
  }

  .input-box:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .input-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-2) var(--space-3);
    border-top: 1px solid var(--border-subtle);
    background: var(--surface-1);
  }

  .input-actions-left,
  .input-actions-right {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: var(--btn-height-md);
    height: var(--btn-height-md);
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .icon-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .model-selector {
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground);
    cursor: pointer;
  }

  .model-selector:focus {
    outline: none;
    border-color: var(--primary);
  }

  .mode-toggle {
    display: flex;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }

  .mode-toggle-option {
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .mode-toggle-option.active {
    background: var(--primary);
    color: white;
  }

  .mode-toggle-option:hover:not(.active) {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .enhance-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .enhance-btn:hover:not(:disabled) {
    background: var(--surface-hover);
    color: var(--foreground);
    border-color: var(--primary);
  }

  .enhance-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .enhance-text {
    font-weight: var(--font-medium);
  }

  .send-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: var(--btn-height-lg);
    height: var(--btn-height-lg);
    padding: 0;
    background: var(--primary);
    border: none;
    border-radius: var(--radius-md);
    color: white;
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .send-btn:hover:not(:disabled) {
    background: var(--primary-hover);
  }

  .send-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .send-btn.stop {
    background: var(--error);
  }

  .send-btn.stop:hover {
    opacity: 0.9;
  }
</style>
