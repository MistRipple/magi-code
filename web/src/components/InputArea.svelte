<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import {
    addToast,
    clearTaskComposerDraft,
    getActiveInteractionType,
    getQueuedMessages,
    markQueuedMessageAsGuide,
    messagesState,
    type TaskComposerDraft,
  } from '../stores/messages.svelte';
  import { getTaskGraphState, refreshTaskProjection, selectTaskGraphTask } from '../stores/task-graph-store.svelte';
  import type { StandardMessage } from '../shared/protocol/message-protocol';
  import { MessageCategory } from '../shared/protocol/message-protocol';
  import type { SessionIntakeResponseDto, TaskDto } from '../shared/rust-backend-types';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';
  import Icon from './Icon.svelte';
  import { generateId, ensureArray } from '../lib/utils';
  import { i18n } from '../stores/i18n.svelte';
  import { getTaskDisplayGoal, getTaskDisplayTitle, getTaskKindLabel, getTaskStatusLabel } from '../lib/task-labels';
  import { isTaskProjectionAcceptingIntake } from '../lib/task-projection-state';

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

  interface IntakeTaskOption {
    taskId: string;
    label: string;
    title: string;
  }

  // 输入内容
  let inputValue = $state('');
  let appliedTaskComposerDraft = $state<TaskComposerDraft | null>(null);

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
  // 增强按钮状态
  let isEnhancing = $state(false);

  // 🔧 图片上传相关状态
  let selectedImages = $state<SelectedImage[]>([]);
  const MAX_IMAGES = 5;  // 最多支持 5 张图片
  const MAX_IMAGE_SIZE = 10 * 1024 * 1024;  // 单张图片最大 10MB

  // Intake 路由状态
  let intakeLoading = $state(false);
  let stopLoading = $state(false);

  const currentSessionId = $derived(messagesState.currentSessionId);
  const taskGraph = $derived(getTaskGraphState(currentSessionId));

  // 深度模式任务图运行中：将用户输入路由到 Intake API
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
  const intakeTaskOptions = $derived.by((): IntakeTaskOption[] => {
    const projection = taskGraph.projection;
    if (!projection) return [];
    const seen = new Set<string>();
    return projection.tasks
      .filter((task) => {
        if (task.status === 'Cancelled') return false;
        if (seen.has(task.task_id)) return false;
        seen.add(task.task_id);
        return task.kind === 'Objective' || task.parent_task_id || task.task_id === projection.root_task.task_id;
      })
      .map((task) => ({
        taskId: task.task_id,
        label: formatIntakeTaskOptionLabel(task),
        title: `${getTaskDisplayTitle(task)}\n${getTaskDisplayGoal(task) || task.task_id}`,
      }));
  });
  const intakeContextTaskId = $derived.by(() => {
    const available = new Set(intakeTaskOptions.map((option) => option.taskId));
    const selectedTaskId = taskGraph.selectedTaskId?.trim();
    if (selectedTaskId && available.has(selectedTaskId)) {
      return selectedTaskId;
    }
    return defaultIntakeContextTaskId;
  });
  const showIntakeTaskTargetBar = $derived(shouldUseIntake && intakeTaskOptions.length > 0);
  const selectedIntakeTaskOption = $derived.by(() => (
    intakeTaskOptions.find((option) => option.taskId === intakeContextTaskId) ?? null
  ));
  const shouldPauseTaskGraphFromComposer = $derived.by(() => {
    const projection = taskGraph.projection;
    const sessionId = currentSessionId?.trim();
    const rootTaskId = projection?.root_task.task_id ?? taskGraph.rootTaskId;
    if (!projection || !sessionId || !rootTaskId) return false;
    return projection.runner_status === 'running' || projection.runner_status === 'blocked';
  });

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
    isInteractionBlocking || intakeLoading
  ));
  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (selectedSkill) return true;
    if (inputValue.trim().length > 0) return true;
    // 执行中补充指令不支持图片，避免"有内容可发送"与实际能力不一致
    if (isSending) return false;
    return selectedImages.length > 0;
  });

  function clearComposerState() {
    inputValue = '';
    selectedImages = [];
    selectedSkill = null;
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

  function formatIntakeTaskOptionLabel(task: TaskDto): string {
    return `${getTaskKindLabel(task.kind)} · ${getTaskDisplayTitle(task)} · ${getTaskStatusLabel(task.status)}`;
  }

  function selectIntakeTaskTarget(event: Event) {
    const select = event.currentTarget as HTMLSelectElement | null;
    selectTaskGraphTask(currentSessionId, select?.value ?? null);
  }

  // 发送消息（支持图片附件）
  // 运行中再次发送不会打断当前轮，而是按当前 session 的队列/引导模式串行提交。
  async function sendMessage() {
    const rawContent = inputValue;
    const normalizedContent = rawContent.trim();
    // 允许只发送图片（无文字）或只发送文字，或只发送已选技能
    if ((!normalizedContent && !selectedSkill && selectedImages.length === 0) || isInteractionBlocking) return;
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

    // 深度任务运行中默认走 Intake；继续意图必须交给 session turn 分类器恢复执行链。
    if (shouldUseIntake && submissionText && !isNaturalContinueRequest(submissionText)) {
      await sendIntake(submissionText);
      return;
    }

    const requestId = generateId();
    vscode.postMessage({
      type: 'executeTask',
      text: submissionText,
      requestId,
      deepTask: deepTaskEnabled,
      skillName: selectedSkill?.name ?? null,
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
        addToast('info', '任务已暂停');
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
    // 输入框为空时按 Backspace 删除技能徽章
    if (event.key === 'Backspace' && !inputValue && selectedSkill) {
      event.preventDefault();
      selectedSkill = null;
    }
  }

  // 任务图运行时，输入框停止入口与任务面板共用同一条暂停链路。
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
          addToast('info', '任务链已暂停');
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

  function focusInputTextareaToEnd() {
    requestAnimationFrame(() => {
      inputTextareaEl?.focus();
      const length = inputTextareaEl?.value.length || 0;
      inputTextareaEl?.setSelectionRange(length, length);
    });
  }

  $effect(() => {
    const draft = messagesState.taskComposerDraft;
    if (!draft || draft === appliedTaskComposerDraft) return;
    appliedTaskComposerDraft = draft;
    inputValue = draft.text;
    if (draft.taskId) {
      selectTaskGraphTask(currentSessionId, draft.taskId);
    }
    clearTaskComposerDraft();
    focusInputTextareaToEnd();
  });

  // 增强提示词 - 直接替换输入框内容
  function enhancePrompt() {
    const content = inputValue.trim();
    if (!content || isEnhancing) return;
    isEnhancing = true;
    vscode.postMessage({ type: 'enhancePrompt', prompt: content });
  }

  // 切换深度任务模式
  function toggleDeepTask() {
    deepTaskEnabled = !deepTaskEnabled;
    vscode.postMessage({ type: 'updateSetting', key: 'deepTask', value: deepTaskEnabled });
    addToast('info', deepTaskEnabled
      ? i18n.t('input.deepModeEnabled')
      : i18n.t('input.deepModeDisabled'));
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
            addToast('success', i18n.t('input.promptEnhanced'));
          }
        }
      }

      if (standard.data.dataType === 'settingsBootstrapLoaded') {
        const payload = standard.data.payload as {
          skillsConfig?: any;
          runtimeSettings?: { deepTask?: boolean };
        };
        skillsConfig = payload?.skillsConfig || null;
        if (typeof payload?.runtimeSettings?.deepTask === 'boolean') {
          deepTaskEnabled = payload.runtimeSettings.deepTask;
        }
      }
    });
    vscode.postMessage({ type: 'loadSettingsBootstrap', force: false });
    return () => {
      unsubscribe();
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

    {#if showIntakeTaskTargetBar}
      <div class="ia-task-target-bar">
        <span class="ia-task-target-label">
          <Icon name="target" size={11} />
          <span>目标任务</span>
        </span>
        <select
          class="ia-task-target-select"
          value={intakeContextTaskId ?? ''}
          onchange={selectIntakeTaskTarget}
          title={selectedIntakeTaskOption?.title || '目标任务'}
          aria-label="目标任务"
        >
          {#each intakeTaskOptions as option (option.taskId)}
            <option value={option.taskId}>{option.label}</option>
          {/each}
        </select>
      </div>
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <textarea
      bind:value={inputValue}
      bind:this={inputTextareaEl}
      class="ia-textarea"
      data-testid="input-textarea"
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

        <!-- 深度任务模式开关 -->
        <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
        <button
          class="ia-deep-btn"
          class:active={deepTaskEnabled}
          onclick={toggleDeepTask}
          title={deepTaskEnabled
            ? i18n.t('input.deepModeActive')
            : i18n.t('input.deepModeInactive')}
        >
          <Icon name="infinity" size={12} />
          <span class="ia-deep-label">{i18n.t('input.deepLabel')}</span>
        </button>

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
            title={shouldPauseTaskGraphFromComposer ? '暂停当前任务链' : i18n.t('input.stop')}
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
    background: var(--surface-1);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    font-size: 11px;
    font-weight: var(--font-medium);
    color: var(--foreground);
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
    color: var(--foreground-muted);
    cursor: pointer;
    transition: opacity var(--transition-fast), background var(--transition-fast);
    flex-shrink: 0;
  }
  .ia-skill-badge-remove:hover { color: var(--error); background: var(--error-muted); }

  .ia-task-target-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 6px var(--space-2) 0;
    flex-shrink: 0;
  }

  .ia-task-target-label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .ia-task-target-select {
    min-width: 0;
    flex: 1;
    height: 28px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    color: var(--foreground);
    font: inherit;
    font-size: var(--text-xs);
    outline: none;
  }

  .ia-task-target-select:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 2px var(--primary-muted);
  }

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
    bottom: calc(100% + 8px);
    left: 0;
    width: 260px;
    max-height: 320px;
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-xl);
    z-index: var(--z-dropdown);
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
    gap: 2px;
    padding: 8px 10px;
    background: transparent;
    border: none;
    border-radius: var(--radius-md);
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

  /* 深度任务模式按钮 */
  .ia-deep-btn {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 24px;
    padding: 0 8px;
    font-size: 10px;
    font-weight: var(--font-semibold);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    user-select: none;
    flex-shrink: 0;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .ia-deep-btn:hover { border-color: var(--foreground-muted); color: var(--foreground); }

  .ia-deep-btn.active {
    background: color-mix(in srgb, var(--primary) 15%, transparent);
    border-color: var(--primary);
    color: var(--primary);
  }

  .ia-deep-btn.active:hover {
    background: color-mix(in srgb, var(--primary) 22%, transparent);
  }

  .ia-deep-label { pointer-events: none; }

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
