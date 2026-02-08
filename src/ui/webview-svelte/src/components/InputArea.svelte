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
  import { generateId, ensureArray } from '../lib/utils';

  // 技能类型
  interface InstructionSkill {
    name: string;
    description?: string;
    userInvocable?: boolean;
  }

  // 输入内容
  let inputValue = $state('');

  // 模式和模型选择
  let selectedModel = $state('');
  const interactionMode = $derived.by(() => getInteractionMode());
  const requestedInteractionMode = $derived.by(() => getRequestedInteractionMode());
  const isModeSyncing = $derived.by(() => isInteractionModeSyncing());

  // 模型下拉状态
  let modelDropdownOpen = $state(false);
  const modelOptions = [
    { value: '', label: '自动' },
    { value: 'claude', label: 'Claude' },
    { value: 'codex', label: 'Codex' },
    { value: 'gemini', label: 'Gemini' },
  ] as const;
  const selectedModelLabel = $derived(
    modelOptions.find(o => o.value === selectedModel)?.label || '自动'
  );

  function selectModel(value: string) {
    selectedModel = value;
    modelDropdownOpen = false;
  }

  // 技能下拉列表状态
  let skillDropdownOpen = $state(false);
  let skillsConfig = $state<any>(null);
  let skillSearchQuery = $state('');
  // 已选中的技能（徽章卡片）
  let selectedSkill = $state<InstructionSkill | null>(null);

  const instructionSkills = $derived.by(() => {
    return ensureArray<InstructionSkill>(skillsConfig?.instructionSkills)
      .filter(s => s.userInvocable !== false);
  });

  const filteredSkills = $derived.by(() => {
    if (!skillSearchQuery.trim()) return instructionSkills;
    const q = skillSearchQuery.toLowerCase();
    return instructionSkills.filter(s =>
      (s.name || '').toLowerCase().includes(q) ||
      (s.description || '').toLowerCase().includes(q)
    );
  });

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

  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (selectedSkill) return true;
    if (inputValue.trim().length > 0) return true;
    // 执行中补充指令不支持图片，避免"有内容可发送"与实际能力不一致
    if (isSending) return false;
    return selectedImages.length > 0;
  });

  // P1-1: 限频机制 - 执行中 1 秒/条，空闲 300ms/条
  let lastSendTime = $state(0);
  const RATE_LIMIT_IDLE = 300;      // 空闲状态：300ms
  const RATE_LIMIT_PROCESSING = 1000;  // 执行中：1 秒

  // 发送消息（支持图片附件）
  // 执行中发送输入 = 补充指令（默认在下一决策点生效）
  function sendMessage() {
    if (isModeSyncing) {
      addToast('warning', '交互模式切换尚未完成，请稍候再发送');
      return;
    }

    const content = inputValue.trim();
    // 允许只发送图片（无文字）或只发送文字，或只发送已选技能
    // 执行中允许发送，后端将执行"打断并重启"
    if ((!content && !selectedSkill && selectedImages.length === 0) || isInteractionBlocking) return;

    // P1-1: 限频检查
    const now = Date.now();
    const minInterval = isSending ? RATE_LIMIT_PROCESSING : RATE_LIMIT_IDLE;
    if (now - lastSendTime < minInterval) {
      addToast('warning', '发送过快，请稍后再试');
      return;
    }
    lastSendTime = now;

    // 拼接技能前缀：将徽章转换为 /skillName 斜杠命令
    const finalPrompt = selectedSkill
      ? `/${selectedSkill.name} ${content}`.trim()
      : content;

    if (finalPrompt.length > MAX_INPUT_CHARS) {
      addToast('warning', `输入内容过长（${finalPrompt.length} 字符），请控制在 ${MAX_INPUT_CHARS} 字符以内`);
      return;
    }

    // 根据是否正在执行，区分发送新任务还是追加补充指令
    if (isSending) {
      // 执行中：发送补充指令（后端在下一决策点注入）
      // 注意：执行中暂不支持图片
      if (selectedImages.length > 0) {
        addToast('warning', '执行中暂不支持发送图片，请先停止当前任务');
        return;
      }
      vscode.postMessage({
        type: 'appendMessage',
        taskId: '',  // 后端自动关联当前任务
        content: finalPrompt,
      });
    } else {
      // 空闲状态：发送新任务
      const requestId = generateId();
      vscode.postMessage({
        type: 'executeTask',
        prompt: finalPrompt || '请分析这些图片',
        mode: interactionMode,
        agent: selectedModel || undefined,
        requestId,
        images: selectedImages.map(img => ({ dataUrl: img.dataUrl })),
      });
    }

    // 清理输入状态
    inputValue = '';
    selectedImages = [];
    selectedSkill = null;
  }

  // 处理键盘事件
  function handleKeydown(event: KeyboardEvent) {
    if ((event.key === 'Enter' && event.metaKey) || (event.key === 'Enter' && event.ctrlKey)) {
      event.preventDefault();
      sendMessage();
    }
    // 输入框为空时按 Backspace 删除技能徽章
    if (event.key === 'Backspace' && !inputValue && selectedSkill) {
      event.preventDefault();
      selectedSkill = null;
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

  // 打开/关闭技能下拉列表
  function toggleSkillDropdown() {
    if (skillDropdownOpen) {
      skillDropdownOpen = false;
      skillSearchQuery = '';
      return;
    }
    // 打开时请求加载技能列表
    vscode.postMessage({ type: 'loadSkillsConfig' });
    skillDropdownOpen = true;
    skillSearchQuery = '';
  }

  function closeSkillDropdown() {
    skillDropdownOpen = false;
    skillSearchQuery = '';
  }

  // 选中技能：设置徽章，不修改输入文本
  function selectSkill(skill: InstructionSkill) {
    selectedSkill = skill;
    closeSkillDropdown();
    // 聚焦输入框
    requestAnimationFrame(() => {
      const textarea = document.querySelector('.ia-textarea') as HTMLTextAreaElement;
      if (textarea) {
        textarea.focus();
      }
    });
  }

  // 清除技能徽章
  function clearSkillBadge() {
    selectedSkill = null;
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

      // 提示词增强响应
      if (standard.data.dataType === 'promptEnhanced') {
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
      }

      // 技能配置加载响应
      if (standard.data.dataType === 'skillsConfigLoaded') {
        const payload = standard.data.payload as { config?: any };
        skillsConfig = payload?.config || null;
      }
    });
    return () => unsubscribe();
  });
</script>

<div class="ia-container">
  <div class="ia-wrapper" style="height: {inputHeight}px">
    <!-- 拖动调整大小 -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="ia-resize" onmousedown={startResize}></div>

    <!-- 技能徽章 -->
    {#if selectedSkill}
      <div class="ia-skill-badge-bar">
        <span class="ia-skill-badge">
          <Icon name="skill" size={11} />
          <span class="ia-skill-badge-name">/{selectedSkill.name}</span>
          <button class="ia-skill-badge-remove" onclick={clearSkillBadge} title="移除技能">
            <Icon name="close" size={9} />
          </button>
        </span>
      </div>
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <textarea
      bind:value={inputValue}
      class="ia-textarea"
      class:has-images={selectedImages.length > 0}
      class:has-badge={!!selectedSkill}
      placeholder={selectedSkill
        ? `描述 ${selectedSkill.name} 的参数...`
        : selectedImages.length > 0
          ? "添加描述（可选）..."
          : "描述你的任务... (⌘+V 粘贴图片)"}
      disabled={isInteractionBlocking}
      onkeydown={handleKeydown}
      onpaste={handlePaste}
    ></textarea>

    <!-- 图片预览 -->
    {#if selectedImages.length > 0}
      <div class="ia-images">
        {#each selectedImages as img (img.id)}
          <div class="ia-img-item">
            <img src={img.dataUrl} alt={img.name} class="ia-img-thumb" />
            <button class="ia-img-remove" onclick={() => removeImage(img.id)} title="移除">
              <Icon name="close" size={10} />
            </button>
          </div>
        {/each}
        {#if selectedImages.length > 1}
          <button class="ia-img-clear" onclick={clearAllImages} title="清空所有图片">清空</button>
        {/if}
      </div>
    {/if}

    <div class="ia-actions">
      <div class="ia-left">
        <!-- 技能下拉选择器 -->
        <div class="ia-skill-wrap">
          <button class="ia-icon-btn" onclick={toggleSkillDropdown} title="使用技能">
            <Icon name="skill" size={14} />
          </button>
          {#if skillDropdownOpen}
            <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
            <div class="ia-skill-backdrop" role="presentation" onclick={closeSkillDropdown}></div>
            <div class="ia-skill-menu">
              <div class="ia-skill-search">
                <input
                  type="text"
                  bind:value={skillSearchQuery}
                  placeholder="搜索技能..."
                  class="ia-skill-search-input"
                />
              </div>
              <div class="ia-skill-list">
                {#if filteredSkills.length === 0}
                  <div class="ia-skill-empty">暂无可用技能</div>
                {:else}
                  {#each filteredSkills as skill (skill.name)}
                    <button
                      class="ia-skill-item"
                      onclick={() => selectSkill(skill)}
                      title={skill.description || skill.name}
                    >
                      <span class="ia-skill-name">/{skill.name}</span>
                      {#if skill.description}
                        <span class="ia-skill-desc">{skill.description}</span>
                      {/if}
                    </button>
                  {/each}
                {/if}
              </div>
            </div>
          {/if}
        </div>

        <!-- 模型选择器（自定义下拉） -->
        <div class="ia-model-wrap">
          <button class="ia-model-btn" onclick={() => modelDropdownOpen = !modelDropdownOpen} title="选择模型">
            <span class="ia-model-label">{selectedModelLabel}</span>
            <Icon name="chevron-down" size={10} />
          </button>
          {#if modelDropdownOpen}
            <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
            <div class="ia-model-backdrop" role="presentation" onclick={() => modelDropdownOpen = false}></div>
            <div class="ia-model-menu">
              {#each modelOptions as opt}
                <button
                  class="ia-model-item"
                  class:selected={selectedModel === opt.value}
                  onclick={() => selectModel(opt.value)}
                >{opt.label}</button>
              {/each}
            </div>
          {/if}
        </div>

        <!-- 模式开关（滑块 Toggle） -->
        <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
        <div
          class="ia-toggle"
          class:auto={interactionMode === 'auto'}
          class:syncing={isModeSyncing}
          role="switch"
          aria-checked={interactionMode === 'auto'}
          tabindex="0"
          onclick={() => setMode(interactionMode === 'ask' ? 'auto' : 'ask')}
          onkeydown={(e) => e.key === 'Enter' && setMode(interactionMode === 'ask' ? 'auto' : 'ask')}
          title={isModeSyncing ? '模式切换中…' : (interactionMode === 'ask' ? '当前: Ask 模式' : '当前: Auto 模式')}
        >
          <span class="ia-toggle-label ask" class:active={interactionMode === 'ask'}>Ask</span>
          <span class="ia-toggle-label auto" class:active={interactionMode === 'auto'}>Auto</span>
          <span class="ia-toggle-thumb" class:syncing={isModeSyncing}></span>
        </div>
      </div>

      <div class="ia-right">
        <!-- 增强：纯图标 -->
        <button
          class="ia-icon-btn ia-enhance"
          class:enhancing={isEnhancing}
          onclick={enhancePrompt}
          title={isEnhancing ? '增强中...' : '增强提示 (AI 优化)'}
          disabled={!inputValue.trim() || isEnhancing}
        >
          <span class:spinning={isEnhancing}>
            <Icon name={isEnhancing ? 'loader' : 'enhance'} size={14} />
          </span>
        </button>

        <!-- 发送 / 停止 -->
        {#if isSending}
          <button class="ia-send stop" onclick={stopTask} title="停止">
            <Icon name="stop" size={14} />
          </button>
          {#if hasContent}
            <button
              class="ia-send ready"
              onclick={sendMessage}
              disabled={isInteractionBlocking || isModeSyncing}
              title={isModeSyncing ? '模式切换中' : '打断并重启 (⌘+Enter)'}
            >
              <Icon name="send" size={14} />
            </button>
          {/if}
        {:else}
          <button
            class="ia-send"
            class:ready={hasContent && !isInteractionBlocking && !isModeSyncing}
            onclick={sendMessage}
            disabled={!hasContent || isInteractionBlocking || isModeSyncing}
            title={isModeSyncing
              ? '模式切换中'
              : (isInteractionBlocking ? `等待：${activeInteraction}` : '发送 (⌘+Enter)')}
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
    flex-shrink: 0;
    padding: var(--space-2) var(--space-3);
    background: var(--background);
  }

  .ia-wrapper {
    display: flex;
    flex-direction: column;
    background: var(--vscode-input-background);
    border: 1px solid var(--vscode-input-border, var(--border));
    border-radius: var(--radius-lg);
    transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
    /* 不使用 overflow:hidden — 允许模型下拉菜单溢出显示 */
  }

  .ia-wrapper:focus-within {
    border-color: var(--primary);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--primary) 15%, transparent);
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
  .ia-textarea.has-badge { padding-top: 2px; }

  /* 技能徽章栏 */
  .ia-skill-badge-bar {
    display: flex;
    align-items: center;
    padding: 6px var(--space-2) 0;
    flex-shrink: 0;
  }

  .ia-skill-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 4px 2px 6px;
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--primary) 25%, transparent);
    border-radius: var(--radius-sm);
    font-size: 11px;
    font-weight: var(--font-medium);
    color: var(--primary);
    line-height: 1;
    max-width: 100%;
  }

  .ia-skill-badge-name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .ia-skill-badge-remove {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-full);
    color: var(--primary);
    cursor: pointer;
    opacity: 0.6;
    transition: opacity var(--transition-fast), background var(--transition-fast);
    flex-shrink: 0;
  }
  .ia-skill-badge-remove:hover { opacity: 1; background: color-mix(in srgb, var(--primary) 15%, transparent); }

  /* 操作栏 */
  .ia-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 4px var(--space-2);
    gap: var(--space-1);
    border-radius: 0 0 var(--radius-lg) var(--radius-lg);
  }

  .ia-left, .ia-right {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  /* 通用图标按钮：26px 圆形 */
  .ia-icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .ia-icon-btn:hover { background: var(--surface-hover); color: var(--foreground); }
  .ia-icon-btn:disabled { opacity: 0.35; cursor: not-allowed; }

  /* 增强按钮特殊状态 */
  .ia-enhance.enhancing { color: var(--info); }
  .ia-enhance .spinning { animation: ia-spin 1s linear infinite; display: flex; }
  @keyframes ia-spin { to { transform: rotate(360deg); } }

  /* 技能下拉选择器 */
  .ia-skill-wrap {
    position: relative;
  }

  .ia-skill-backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
  }

  .ia-skill-menu {
    position: absolute;
    bottom: calc(100% + 4px);
    left: 0;
    width: 240px;
    max-height: 280px;
    background: var(--vscode-input-background, var(--surface-1));
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-lg);
    z-index: 51;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .ia-skill-search {
    flex-shrink: 0;
    padding: 6px;
    border-bottom: 1px solid var(--border);
  }

  .ia-skill-search-input {
    width: 100%;
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    font-size: 11px;
    outline: none;
  }
  .ia-skill-search-input:focus { border-color: var(--primary); }
  .ia-skill-search-input::placeholder { color: var(--foreground-muted); }

  .ia-skill-list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 3px;
    display: flex;
    flex-direction: column;
  }

  .ia-skill-item {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 6px 8px;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: left;
    color: var(--foreground);
    transition: background var(--transition-fast);
  }
  .ia-skill-item:hover { background: var(--surface-hover); }

  .ia-skill-name {
    font-size: 12px;
    font-weight: var(--font-medium);
    color: var(--primary);
  }

  .ia-skill-desc {
    font-size: 10px;
    color: var(--foreground-muted);
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 1;
    line-clamp: 1;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .ia-skill-empty {
    padding: var(--space-3);
    text-align: center;
    color: var(--foreground-muted);
    font-size: 11px;
  }

  /* 模型选择器（自定义下拉） */
  .ia-model-wrap {
    position: relative;
  }

  .ia-model-btn {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 26px;
    padding: 0 6px;
    font-size: 11px;
    font-weight: var(--font-medium);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    color: var(--foreground);
    cursor: pointer;
    transition: border-color var(--transition-fast);
    white-space: nowrap;
  }
  .ia-model-btn:hover { border-color: var(--primary); }

  .ia-model-label { pointer-events: none; }

  .ia-model-backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
  }

  .ia-model-menu {
    position: absolute;
    bottom: calc(100% + 4px);
    left: 0;
    min-width: 90px;
    background: var(--vscode-input-background, var(--surface-1));
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-lg);
    z-index: 51;
    padding: 3px;
    display: flex;
    flex-direction: column;
  }

  .ia-model-item {
    display: flex;
    align-items: center;
    height: 28px;
    padding: 0 var(--space-2);
    font-size: 11px;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground);
    cursor: pointer;
    transition: background var(--transition-fast);
    white-space: nowrap;
  }
  .ia-model-item:hover { background: var(--surface-hover); }
  .ia-model-item.selected { color: var(--primary); font-weight: var(--font-semibold); }

  /* 模式开关（滑块 Toggle） */
  .ia-toggle {
    position: relative;
    display: inline-flex;
    align-items: center;
    width: 72px;
    height: 24px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    cursor: pointer;
    user-select: none;
    flex-shrink: 0;
    transition: border-color var(--transition-fast);
  }
  .ia-toggle:hover { border-color: var(--foreground-muted); }
  .ia-toggle.syncing { border-color: var(--warning); }

  .ia-toggle-label {
    position: relative;
    z-index: 1;
    flex: 1;
    text-align: center;
    font-size: 10px;
    font-weight: var(--font-semibold);
    letter-spacing: 0.02em;
    color: var(--foreground-muted);
    transition: color var(--transition-fast);
    pointer-events: none;
    line-height: 22px;
  }
  .ia-toggle-label.active { color: white; }

  .ia-toggle-thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: calc(50% - 3px);
    height: calc(100% - 4px);
    background: var(--primary);
    border-radius: var(--radius-full);
    transition: transform 0.2s cubic-bezier(0.4, 0, 0.2, 1);
    pointer-events: none;
  }
  .ia-toggle.auto .ia-toggle-thumb {
    transform: translateX(calc(100% + 2px));
  }
  .ia-toggle-thumb.syncing {
    background: var(--warning);
    animation: ia-thumb-pulse 0.8s ease-in-out infinite;
  }
  @keyframes ia-thumb-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
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
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
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
</style>
