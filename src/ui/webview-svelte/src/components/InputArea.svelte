<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import {
    addToast,
    getActiveInteractionType,
    getInteractionMode,
    getQueuedMessages,
    isInteractionModeSyncing,
    requestInteractionMode,
    messagesState,
  } from '../stores/messages.svelte';
  import type { StandardMessage } from '../../../../protocol/message-protocol';
  import { MessageCategory } from '../../../../protocol/message-protocol';
  import Icon from './Icon.svelte';
  import { generateId, ensureArray } from '../lib/utils';
  import { i18n } from '../stores/i18n.svelte';

  // 技能类型
  interface InstructionSkill {
    name: string;
    fullName?: string;
    description?: string;
    userInvocable?: boolean;
  }

  interface SelectedImage {
    id: string;
    dataUrl: string;
    name: string;
  }

  // 输入内容
  let inputValue = $state('');

  // 模式选择
  const interactionMode = $derived.by(() => getInteractionMode());
  const isModeSyncing = $derived.by(() => isInteractionModeSyncing());

  type InteractionPreset = 'fast' | 'stable' | 'review' | 'custom';
  type BuiltinPreset = Exclude<InteractionPreset, 'custom'>;
  interface PresetConfig {
    mode: 'ask' | 'auto';
    deepTask: boolean;
    labelKey: string;
    descriptionKey: string;
    icon: 'zap' | 'shield' | 'check';
  }

  const PRESET_CONFIGS: Record<BuiltinPreset, PresetConfig> = {
    fast: {
      mode: 'auto',
      deepTask: false,
      labelKey: 'input.preset.fast.name',
      descriptionKey: 'input.preset.fast.desc',
      icon: 'zap',
    },
    stable: {
      mode: 'auto',
      deepTask: true,
      labelKey: 'input.preset.stable.name',
      descriptionKey: 'input.preset.stable.desc',
      icon: 'shield',
    },
    review: {
      mode: 'ask',
      deepTask: true,
      labelKey: 'input.preset.review.name',
      descriptionKey: 'input.preset.review.desc',
      icon: 'check',
    },
  };
  const PRESET_OPTIONS: Array<{ key: BuiltinPreset; config: PresetConfig }> = [
    { key: 'fast', config: PRESET_CONFIGS.fast },
    { key: 'stable', config: PRESET_CONFIGS.stable },
    { key: 'review', config: PRESET_CONFIGS.review },
  ];

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
      (s.fullName || '').toLowerCase().includes(q) ||
      (s.description || '').toLowerCase().includes(q)
    );
  });

  // 拖动调整大小相关
  let inputHeight = $state(120); // 默认高度增加到 120px
  const minHeight = 80;
  const maxHeight = 400;

  // 深度任务模式
  let deepTaskEnabled = $state(false);
  const currentPreset = $derived.by(() => resolvePreset(interactionMode, deepTaskEnabled));

  // 增强按钮状态
  let isEnhancing = $state(false);

  // 🔧 图片上传相关状态
  let selectedImages = $state<SelectedImage[]>([]);
  const MAX_IMAGES = 5;  // 最多支持 5 张图片
  const MAX_IMAGE_SIZE = 10 * 1024 * 1024;  // 单张图片最大 10MB

  // 🔧 修复响应式：直接访问 messagesState 属性确保正确追踪
  const isSending = $derived(messagesState.isProcessing);
  const activeInteraction = $derived.by(() => getActiveInteractionType());
  const isInteractionBlocking = $derived.by(() => Boolean(activeInteraction));
  const queuedMessages = $derived.by(() => getQueuedMessages());
  const MAX_INPUT_CHARS = 10000;
  let activeQueueMenu = $state<{ queueId: string; left: number; top: number } | null>(null);
  let inputTextareaEl = $state<HTMLTextAreaElement | null>(null);
  const activeQueueMenuTarget = $derived.by(() => {
    if (!activeQueueMenu) return null;
    return queuedMessages.find((item) => item.id === activeQueueMenu.queueId) || null;
  });

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
  // 执行中发送输入 = 暂存队列（当前轮结束后 FIFO 自动续跑）
  function sendMessage() {
    if (isModeSyncing) {
      addToast('warning', i18n.t('input.modeSyncNotReady'));
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
      addToast('warning', i18n.t('input.sendTooFast'));
      return;
    }
    lastSendTime = now;

    // 拼接技能前缀：将徽章转换为 /skillName 斜杠命令
    const finalPrompt = selectedSkill
      ? `/${selectedSkill.name} ${content}`.trim()
      : content;

    if (finalPrompt.length > MAX_INPUT_CHARS) {
      addToast('warning', i18n.t('input.inputTooLong', { length: finalPrompt.length, max: MAX_INPUT_CHARS }));
      return;
    }

    // 根据是否正在执行，区分发送新任务还是加入暂存队列
    if (isSending) {
      // 执行中：加入暂存队列（后端在当前轮结束后自动续跑）
      // 注意：执行中暂不支持图片
      if (selectedImages.length > 0) {
        addToast('warning', i18n.t('input.noImageDuringExecution'));
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
        prompt: finalPrompt || i18n.t('input.analyzeImages'),
        mode: interactionMode,
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

  function closeQueueMenu() {
    activeQueueMenu = null;
  }

  function focusInputTextareaToEnd() {
    requestAnimationFrame(() => {
      inputTextareaEl?.focus();
      const length = inputTextareaEl?.value.length || 0;
      inputTextareaEl?.setSelectionRange(length, length);
    });
  }

  function toggleQueueMenu(queueId: string, event: MouseEvent) {
    if (activeQueueMenu?.queueId === queueId) {
      closeQueueMenu();
      return;
    }
    const trigger = event.currentTarget as HTMLElement | null;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    const menuWidth = 132;
    const menuHeight = 72;
    let left = rect.right + 8;
    if (left + menuWidth > window.innerWidth - 8) {
      left = Math.max(8, rect.left - menuWidth - 8);
    }
    let top = rect.top;
    if (top + menuHeight > window.innerHeight - 8) {
      top = Math.max(8, window.innerHeight - menuHeight - 8);
    }
    activeQueueMenu = { queueId, left, top };
  }

  function startEditQueuedMessage(queueId: string) {
    const target = queuedMessages.find((item) => item.id === queueId);
    if (!target) return;
    inputValue = target.content;
    selectedSkill = null;
    selectedImages = [];
    closeSkillDropdown();
    vscode.postMessage({
      type: 'deleteQueuedMessage',
      queueId,
    });
    closeQueueMenu();
    focusInputTextareaToEnd();
  }

  function deleteQueuedMessage(queueId: string) {
    vscode.postMessage({
      type: 'deleteQueuedMessage',
      queueId,
    });
    closeQueueMenu();
  }

  // 增强提示词 - 直接替换输入框内容
  function enhancePrompt() {
    const content = inputValue.trim();
    if (!content || isEnhancing) return;
    isEnhancing = true;
    vscode.postMessage({ type: 'enhancePrompt', prompt: content });
  }

  function resolvePreset(mode: 'ask' | 'auto', deepTask: boolean): InteractionPreset {
    if (mode === 'auto' && !deepTask) return 'fast';
    if (mode === 'auto' && deepTask) return 'stable';
    if (mode === 'ask' && deepTask) return 'review';
    return 'custom';
  }

  function applyPreset(preset: BuiltinPreset) {
    if (isModeSyncing) {
      addToast('warning', i18n.t('input.modeSyncNotReady'));
      return;
    }

    const target = PRESET_CONFIGS[preset];
    const needModeChange = interactionMode !== target.mode;
    const needDeepTaskChange = deepTaskEnabled !== target.deepTask;
    if (!needModeChange && !needDeepTaskChange) {
      return;
    }

    if (needModeChange) {
      requestInteractionMode(target.mode);
      vscode.postMessage({ type: 'setInteractionMode', mode: target.mode });
    }

    if (needDeepTaskChange) {
      deepTaskEnabled = target.deepTask;
      vscode.postMessage({ type: 'updateSetting', key: 'deepTask', value: target.deepTask });
    }

    addToast('info', i18n.t('input.presetApplied', { preset: i18n.t(target.labelKey) }));
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
    focusInputTextareaToEnd();
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
          addToast('warning', i18n.t('input.maxImages', { max: MAX_IMAGES }));
          return;
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
    const handleViewportChanged = () => {
      closeQueueMenu();
    };
    window.addEventListener('resize', handleViewportChanged);
    window.addEventListener('scroll', handleViewportChanged, true);
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
            addToast('success', i18n.t('input.promptEnhanced'));
          }
        }
      }

      // 技能配置加载响应
      if (standard.data.dataType === 'skillsConfigLoaded') {
        const payload = standard.data.payload as { config?: any };
        skillsConfig = payload?.config || null;
      }

      // 深度任务状态同步
      if (standard.data.dataType === 'deepTaskChanged') {
        const payload = standard.data.payload as { enabled?: boolean };
        if (typeof payload?.enabled === 'boolean') {
          deepTaskEnabled = payload.enabled;
        }
      }
    });
    // 初始化时请求 deepTask 状态
    vscode.postMessage({ type: 'getDeepTaskState' });
    return () => {
      unsubscribe();
      window.removeEventListener('resize', handleViewportChanged);
      window.removeEventListener('scroll', handleViewportChanged, true);
    };
  });
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
            <div class="ia-queue-ops">
              <button
                class="ia-queue-menu-trigger"
                onclick={(event) => toggleQueueMenu(queued.id, event)}
                title={i18n.t('input.queue.more')}
                aria-label={i18n.t('input.queue.more')}
              >
                <Icon name="more-horizontal" size={12} />
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

    <!-- 技能徽章 -->
    {#if selectedSkill}
      <div class="ia-skill-badge-bar">
        <span class="ia-skill-badge">
          <Icon name="skill" size={11} />
          <span class="ia-skill-badge-name">/{selectedSkill.name}</span>
          <button class="ia-skill-badge-remove" onclick={clearSkillBadge} title={i18n.t('input.removeSkill')}>
            <Icon name="close" size={9} />
          </button>
        </span>
      </div>
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <textarea
      bind:value={inputValue}
      bind:this={inputTextareaEl}
      class="ia-textarea"
      class:has-images={selectedImages.length > 0}
      class:has-badge={!!selectedSkill}
      placeholder={selectedSkill
        ? i18n.t('input.placeholderWithSkill', { skillName: selectedSkill.name })
        : selectedImages.length > 0
          ? i18n.t('input.placeholderWithImages')
          : i18n.t('input.placeholderDefault')}
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
        <!-- 技能下拉选择器 -->
        <div class="ia-skill-wrap">
          <button
            class="ia-icon-btn"
            onclick={toggleSkillDropdown}
            title={i18n.t('input.useSkill')}
          >
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
                  placeholder={i18n.t('input.searchSkill')}
                  class="ia-skill-search-input"
                />
              </div>
              <div class="ia-skill-list">
                {#if filteredSkills.length === 0}
                  <div class="ia-skill-empty">{i18n.t('input.noSkills')}</div>
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

        <div class="ia-preset-wrap">
          <div class="ia-preset" role="group" aria-label={i18n.t('input.preset.groupLabel')}>
            {#each PRESET_OPTIONS as option (option.key)}
              <button
                class="ia-preset-btn"
                class:active={currentPreset === option.key}
                onclick={() => applyPreset(option.key)}
                title={i18n.t(option.config.descriptionKey)}
                disabled={isModeSyncing}
              >
                <Icon name={option.config.icon} size={10} />
                <span>{i18n.t(option.config.labelKey)}</span>
              </button>
            {/each}
          </div>
          {#if currentPreset === 'custom'}
            <span class="ia-preset-meta">{i18n.t('input.preset.custom.desc')}</span>
          {/if}
        </div>
      </div>

      <div class="ia-right">
        <!-- 增强：纯图标 -->
        <button
          class="ia-icon-btn ia-enhance"
          class:enhancing={isEnhancing}
          onclick={enhancePrompt}
          title={isEnhancing ? i18n.t('input.enhancing') : i18n.t('input.enhancePrompt')}
          disabled={!inputValue.trim() || isEnhancing}
        >
          <span class:spinning={isEnhancing}>
            <Icon name={isEnhancing ? 'loader' : 'enhance'} size={14} />
          </span>
        </button>

        {#if hasContent}
          <!-- 有内容：显示发送按钮 -->
          <button
            class="ia-send ready"
            onclick={sendMessage}
            disabled={isInteractionBlocking || isModeSyncing}
            title={isModeSyncing ? i18n.t('input.modeSwitchingShort') : (isSending ? i18n.t('input.sendSupplementary') : i18n.t('input.send'))}
          >
            <Icon name="send" size={14} />
          </button>
        {:else if isSending}
          <!-- 无内容 + 运行中：显示停止按钮 -->
          <button class="ia-send stop" onclick={stopTask} title={i18n.t('input.stop')}>
            <Icon name="stop" size={14} />
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

  {#if activeQueueMenu && activeQueueMenuTarget}
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
    <div class="ia-queue-menu-backdrop" role="presentation" onclick={closeQueueMenu}></div>
    <div
      class="ia-queue-floating-menu"
      style="left: {activeQueueMenu.left}px; top: {activeQueueMenu.top}px;"
    >
      <button class="ia-queue-menu-item" onclick={() => startEditQueuedMessage(activeQueueMenu.queueId)}>
        <Icon name="edit" size={10} />
        <span>{i18n.t('input.queue.edit')}</span>
      </button>
      <button class="ia-queue-menu-item danger" onclick={() => deleteQueuedMessage(activeQueueMenu.queueId)}>
        <Icon name="trash" size={10} />
        <span>{i18n.t('input.queue.delete')}</span>
      </button>
    </div>
  {/if}
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
    padding: var(--space-2) var(--space-3);
    background: var(--background);
    position: relative;
  }

  .ia-wrapper {
    display: flex;
    flex-direction: column;
    max-height: 50vh;
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
    flex-shrink: 0;
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

  /* 执行预设（Fast / Stable / Review） */
  .ia-preset-wrap {
    display: inline-flex;
    flex-direction: column;
    gap: 2px;
  }

  .ia-preset {
    display: inline-flex;
    align-items: center;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    overflow: hidden;
    background: var(--surface-2);
  }

  .ia-preset-btn {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 24px;
    padding: 0 8px;
    font-size: 10px;
    font-weight: var(--font-semibold);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
    white-space: nowrap;
  }

  .ia-preset-btn + .ia-preset-btn {
    border-left: 1px solid var(--border-subtle);
  }

  .ia-preset-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .ia-preset-btn.active {
    background: color-mix(in srgb, var(--primary) 18%, transparent);
    color: var(--primary);
  }

  .ia-preset-btn:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }

  .ia-preset-meta {
    font-size: 10px;
    line-height: 1.2;
    color: var(--warning);
    padding-left: 2px;
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
  }

  .ia-queue-ops {
    flex-shrink: 0;
  }

  .ia-queue-menu-trigger {
    width: 22px;
    height: 22px;
    border: 1px solid transparent;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    margin-top: -1px;
  }

  .ia-queue-menu-trigger:hover {
    background: var(--surface-hover);
    border-color: color-mix(in srgb, var(--border) 80%, transparent);
    color: var(--foreground);
  }

  .ia-queue-menu-backdrop {
    position: fixed;
    inset: 0;
    z-index: 58;
    background: transparent;
  }

  .ia-queue-floating-menu {
    position: fixed;
    min-width: 132px;
    padding: 4px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    background: var(--vscode-input-background, var(--surface-1));
    box-shadow: var(--shadow-md);
    display: flex;
    flex-direction: column;
    gap: 2px;
    z-index: 59;
  }

  .ia-queue-menu-item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    border: none;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    padding: 4px 6px;
    font-size: 12px;
    text-align: left;
  }

  .ia-queue-menu-item:hover {
    background: var(--surface-hover);
  }

  .ia-queue-menu-item.danger {
    color: var(--error);
  }

</style>
