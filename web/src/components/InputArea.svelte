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
  import { getTaskProjectionState, refreshTaskProjection } from '../stores/task-projection-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import {
    enhanceAgentPrompt,
    fetchAgentModelList,
    getAgentSettingsBootstrap,
    resolveAgentBaseUrl,
    saveAgentOrchestratorSessionConfig,
    fetchWorkspaceBranches,
    checkoutWorkspaceBranch,
    type AgentSettingsBootstrapSnapshot,
    settingsBootstrapMatchesCurrentWorkspace,
  } from '../web/agent-api';
  import Icon from './Icon.svelte';
  import { generateId } from '../lib/utils';
  import { i18n } from '../stores/i18n.svelte';
  import {
    type AccessProfile,
    isAccessProfile,
    readStoredAccessProfile,
    writeStoredAccessProfile,
  } from '../shared/access-profile';
  import {
    composerWorkspaceState,
    resolveComposerWorkspace,
    selectComposerDraftWorkspace,
    type ComposerWorkspaceOption,
  } from '../stores/composer-workspace.svelte';

  interface SelectedImage {
    id: string;
    dataUrl: string;
    name: string;
  }

  // 输入框可识别的 instruction skill。来源：bootstrap 中的 skillsConfig.instructionSkills，
  // 这一组才是 `/` 唤起的指令型技能，与 customTools（已注册到工具表）的语义不同。
  interface SkillOption {
    name: string;
    description: string;
  }

  type ReasoningEffort = 'low' | 'medium' | 'high' | 'xhigh';

  const reasoningOptions: Array<{
    value: ReasoningEffort;
    labelKey: string;
  }> = [
    { value: 'low', labelKey: 'input.mainModelPicker.reasoning.low' },
    { value: 'medium', labelKey: 'input.mainModelPicker.reasoning.medium' },
    { value: 'high', labelKey: 'input.mainModelPicker.reasoning.high' },
    { value: 'xhigh', labelKey: 'input.mainModelPicker.reasoning.xhigh' },
  ];

  const accessProfileOptions: Array<{
    value: AccessProfile;
    labelKey: string;
  }> = [
    {
      value: 'read_only',
      labelKey: 'input.access.readOnly',
    },
    {
      value: 'restricted',
      labelKey: 'input.access.restricted',
    },
    {
      value: 'full_access',
      labelKey: 'input.access.fullAccess',
    },
  ];

  // 输入内容
  let inputValue = $state('');

  // 斜杠唤起的技能选择面板状态
  let selectedSkill = $state<SkillOption | null>(null);
  let slashTriggerStart = $state<number | null>(null);
  let slashFilter = $state('');
  let slashHighlightIndex = $state(0);
  let slashListEl = $state<HTMLDivElement | null>(null);
  let slashTooltipTop = $state(0);
  // 技能详情预览（API 拉取 prompt.md / SKILL.md 内容，截断 200 字）。
  // 仅在 hover/高亮时按需懒拉，避免 bootstrap 体积膨胀；同一会话内复用。
  let skillPreviewCache = $state<Record<string, string>>({});
  let skillPreviewLoading = $state<Record<string, boolean>>({});

  // 拖动调整大小相关
  let inputHeight = $state(120); // 默认高度增加到 120px
  const minHeight = 80;
  const maxHeight = 400;

  // 🔧 图片上传相关状态
  let selectedImages = $state<SelectedImage[]>([]);
  let pendingImageReadCount = $state(0);
  let sendPreparing = $state(false);
  const MAX_IMAGES = 5;  // 最多支持 5 张图片
  const MAX_IMAGE_SIZE = 10 * 1024 * 1024;  // 单张图片最大 10MB
  const IMAGE_FILE_NAME_PATTERN = /\.(png|jpe?g|gif|webp|bmp|heic|heif)$/i;

  let stopLoading = $state(false);
  let enhanceLoading = $state(false);
  let enhanceOriginalPrompt = $state<string | null>(null);
  let enhanceResultPrompt = $state<string | null>(null);

  // 主线模型 picker：弹窗状态 + 模型列表惰性拉取。
  // 选中后只写当前会话 orchestrator 覆盖段；全局配置仍是新会话默认值和连接凭据来源。
  let pickerOpen = $state(false);
  let pickerLoading = $state(false);
  let pickerSavingModel = $state<string | null>(null);
  let pickerSavingReasoning = $state<ReasoningEffort | null>(null);
  let pickerModels = $state<string[]>([]);
  let pickerError = $state<string | null>(null);
  let pickerLoadedOnce = false;

  // Git 分支切换器：仅在工作区是 git 仓库时显示。分支信息为即时查询的瞬态状态，
  // 不进持久化 store；切换失败只展示稳定提示，原始错误保留在控制台日志。
  let branchPickerOpen = $state(false);
  let branchLoading = $state(false);
  let branchSwitching = $state<string | null>(null);
  let branches = $state<string[]>([]);
  let currentBranch = $state<string | null>(null);
  let branchError = $state<string | null>(null);
  let branchIsRepo = $state(false);
  let branchAdditions = $state(0);
  let branchDeletions = $state(0);
  let branchRequestSeq = 0;
  let workspacePickerOpen = $state(false);
  let accessProfilePickerOpen = $state(false);
  let selectedAccessProfile = $state<AccessProfile>('restricted');
  const currentPickerModel = $derived.by(() => readOrchestratorModel());
  const currentPickerReasoningEffort = $derived.by(() => readOrchestratorReasoningEffort());
  const currentPickerReasoningLabel = $derived.by(() => reasoningEffortLabel(currentPickerReasoningEffort));
  const currentAccessProfileOption = $derived.by(() => (
    accessProfileOptions.find((option) => option.value === selectedAccessProfile)
    ?? accessProfileOptions[1]
  ));
  const auxiliaryConfig = $derived.by(() => getAuxiliaryConfigSnapshot());
  const auxiliaryEnhanceReady = $derived.by(() => hasUsableModelConfig(auxiliaryConfig));
  const enhanceButtonTitle = $derived.by(() => (
    auxiliaryEnhanceReady ? i18n.t('input.enhance.title') : i18n.t('input.enhance.disabled')
  ));
  const hasEnhanceSnapshot = $derived.by(() => (
    enhanceOriginalPrompt !== null
    && enhanceResultPrompt !== null
  ));

  const currentSessionId = $derived(messagesState.currentSessionId);
  const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
  const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);
  const isDraftSession = $derived.by(() => !currentSessionId?.trim());
  const composerWorkspace = $derived.by(() => (
    resolveComposerWorkspace(currentWorkspaceId, currentWorkspacePath, isDraftSession)
  ));
  const workspaceOptions = $derived.by(() => composerWorkspaceState.workspaces);
  const taskProjection = $derived(getTaskProjectionState(currentSessionId, currentWorkspaceId));

  const shouldInterruptTaskProjectionFromComposer = $derived.by(() => {
    const projection = taskProjection.projection;
    const sessionId = currentSessionId?.trim();
    const rootTaskId = projection?.root_task.task_id ?? taskProjection.rootTaskId;
    if (!projection || !sessionId || !rootTaskId) return false;
    return projection.runner_status === 'running';
  });
  const sessionInputLocked = $derived.by(() => (
    messagesState.sessionHydrating
  ));

  // 发送/停止态只认 store 内已经收敛好的 isProcessing：
  // 该字段已收敛为 backendProcessing | pendingRequests 的单一事实源，
  // 不再叠加 orchestrator runtimeState / canonical projection / activeMessageIds，
  // 避免历史会话里的陈旧工具卡片把空闲会话抬回执行态。
  const isSending = $derived(messagesState.isProcessing);
  const activeInteraction = $derived.by(() => getActiveInteractionType());
  const isInteractionBlocking = $derived.by(() => Boolean(activeInteraction));
  const queuedMessages = $derived.by(() => getQueuedMessages());
  const MAX_INPUT_CHARS = 10000;
  let inputTextareaEl = $state<HTMLDivElement | null>(null);
  let isComposing = $state(false);
  let pendingCaretOffset = $state<number | null>(null);
  const sendButtonTitle = $derived.by(() => {
    if (isSending) {
      return i18n.t('input.followUp.queueTitle');
    }
    return i18n.t('input.send');
  });
  const sendDisabled = $derived.by(() => (
    sessionInputLocked || isInteractionBlocking || sendPreparing || pendingImageReadCount > 0
  ));
  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (inputValue.trim().length > 0) return true;
    // 执行中补充指令不支持图片，避免"有内容可发送"与实际能力不一致
    if (isSending) return false;
    return selectedImages.length > 0 || pendingImageReadCount > 0;
  });

  // bootstrap 是全局缓存，新会话/设置变更都会同步更新这里，所以输入框可以直接派生。
  const availableSkills = $derived.by<SkillOption[]>(() => {
    const snapshot = messagesState.settingsBootstrapSnapshot as
      | { skillsConfig?: Record<string, unknown> }
      | null;
    const cfg = (snapshot?.skillsConfig ?? {}) as Record<string, unknown>;
    const raw = Array.isArray(cfg.instructionSkills) ? cfg.instructionSkills : [];
    const out: SkillOption[] = [];
    for (const entry of raw) {
      if (!entry || typeof entry !== 'object') continue;
      const obj = entry as Record<string, unknown>;
      const name = (() => {
        for (const key of ['name', 'skillName', 'skillId', 'fullName']) {
          const val = obj[key];
          if (typeof val === 'string' && val.trim()) return val.trim();
        }
        return '';
      })();
      if (!name) continue;
      const description = typeof obj.description === 'string' ? obj.description : '';
      out.push({ name, description });
    }
    return out;
  });

  function fuzzyMatch(text: string, query: string): boolean {
    if (!query) return true;
    let qi = 0;
    for (let i = 0; i < text.length && qi < query.length; i++) {
      if (text[i] === query[qi]) qi++;
    }
    return qi === query.length;
  }

  const filteredSkills = $derived.by<SkillOption[]>(() => {
    if (slashTriggerStart === null) return [];
    const filter = slashFilter.trim().toLowerCase();
    const list = availableSkills;
    if (!filter) return list;
    return list.filter((skill) => {
      const name = skill.name.toLowerCase();
      const desc = skill.description.toLowerCase();
      return fuzzyMatch(name, filter) || name.includes(filter) || desc.includes(filter);
    });
  });

  const slashMenuOpen = $derived(slashTriggerStart !== null && filteredSkills.length > 0);

  // 跟随高亮项更新 tooltip 的纵向偏移；列表与 tooltip 都挂在 popover 上，
  // 鼠标 hover / 键盘上下键改动 slashHighlightIndex 都会触发这次重算。
  $effect(() => {
    void slashHighlightIndex;
    void filteredSkills;
    if (!slashMenuOpen) return;
    queueMicrotask(() => {
      const list = slashListEl;
      if (!list) return;
      const items = list.querySelectorAll<HTMLElement>('.ia-slash-item');
      const active = items[slashHighlightIndex];
      if (!active) return;
      slashTooltipTop = active.offsetTop;
      active.scrollIntoView({ block: 'nearest' });
    });
  });

  // 高亮项变化时，按需从后端拉取技能正文预览（prompt.md/SKILL.md 前 200 字）。
  // 缓存 key 为技能名；空字符串视为 "已尝试但内容为空"，避免重复请求。
  $effect(() => {
    if (!slashMenuOpen) return;
    const active = filteredSkills[slashHighlightIndex];
    if (!active) return;
    const skillName = active.name;
    if (skillPreviewCache[skillName] !== undefined) return;
    if (skillPreviewLoading[skillName]) return;
    skillPreviewLoading[skillName] = true;
    const base = resolveAgentBaseUrl();
    const url = `${base.replace(/\/$/, '')}/api/settings/skills/instruction-preview?skillId=${encodeURIComponent(skillName)}`;
    fetch(url)
      .then(async (resp) => {
        if (!resp.ok) {
          skillPreviewCache[skillName] = '';
          return;
        }
        const data = await resp.json();
        const preview = typeof data?.preview === 'string' ? data.preview : '';
        skillPreviewCache[skillName] = preview;
      })
      .catch(() => {
        skillPreviewCache[skillName] = '';
      })
      .finally(() => {
        skillPreviewLoading[skillName] = false;
      });
  });

  function clearComposerState() {
    inputValue = '';
    selectedImages = [];
    selectedSkill = null;
    clearEnhanceSnapshot();
    closeSlashMenu();
  }

  let imageReadWaiters: Array<() => void> = [];

  function notifyImageReadWaiters() {
    if (pendingImageReadCount > 0 || imageReadWaiters.length === 0) return;
    const waiters = imageReadWaiters;
    imageReadWaiters = [];
    for (const resolve of waiters) resolve();
  }

  function beginImageRead() {
    pendingImageReadCount += 1;
  }

  function finishImageRead() {
    pendingImageReadCount = Math.max(0, pendingImageReadCount - 1);
    notifyImageReadWaiters();
  }

  function waitForPendingImageReads(): Promise<void> {
    if (pendingImageReadCount === 0) return Promise.resolve();
    return new Promise((resolve) => {
      imageReadWaiters = [...imageReadWaiters, resolve];
    });
  }

  function isClipboardImageFile(file: File, hintedType = ''): boolean {
    const mediaType = (file.type || hintedType).toLowerCase();
    if (mediaType.startsWith('image/')) return true;
    return IMAGE_FILE_NAME_PATTERN.test(file.name);
  }

  function clipboardFileKey(file: File): string {
    return [
      file.name,
      file.type,
      file.size,
      file.lastModified,
    ].join(':');
  }

  function collectClipboardImageFiles(data: DataTransfer | null | undefined): File[] {
    if (!data) return [];
    const files: File[] = [];
    const seen = new Set<string>();
    const addFile = (file: File | null, hintedType = '') => {
      if (!file || !isClipboardImageFile(file, hintedType)) return;
      const key = clipboardFileKey(file);
      if (seen.has(key)) return;
      seen.add(key);
      files.push(file);
    };

    for (const item of Array.from(data.items ?? [])) {
      if (item.kind !== 'file') continue;
      addFile(item.getAsFile(), item.type);
    }
    for (const file of Array.from(data.files ?? [])) {
      addFile(file);
    }
    return files;
  }

  function readImageFileIntoComposer(file: File) {
    beginImageRead();
    const reader = new FileReader();
    const imageName = file.name || i18n.t('input.pastedImage', {
      index: selectedImages.length + pendingImageReadCount,
    });
    reader.onload = (event) => {
      const dataUrl = event.target?.result;
      if (typeof dataUrl !== 'string' || dataUrl.length === 0) return;
      selectedImages = [...selectedImages, {
        id: generateId(),
        dataUrl,
        name: imageName,
      }];
      addToast('success', i18n.t('input.imageAdded'));
    };
    reader.onerror = () => {
      addToast('error', i18n.t('input.imageReadFailed'));
    };
    reader.onloadend = finishImageRead;
    try {
      reader.readAsDataURL(file);
    } catch {
      finishImageRead();
      addToast('error', i18n.t('input.imageReadFailed'));
    }
  }

  // contenteditable 编辑器辅助：以 inputValue 为唯一事实，DOM 仅作为渲染层。
  // 渲染策略：保留原始 markdown 标记符号（**、`、# 等），用 span 包裹做样式高亮，
  // 这样 textContent 与 inputValue 1:1 对齐，光标偏移可直接复用。
  function escapeHtml(input: string): string {
    return input
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }

  function buildHighlightedHtml(raw: string): string {
    if (!raw) return '';
    const inlineTokenRe = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\*[^*\n]+\*)/g;
    const renderInline = (segment: string) =>
      segment.replace(inlineTokenRe, (match) => {
        if (match.startsWith('**')) return `<span class="md-bold">${match}</span>`;
        if (match.startsWith('`')) return `<span class="md-code">${match}</span>`;
        return `<span class="md-italic">${match}</span>`;
      });
    return raw
      .split('\n')
      .map((line) => {
        const escaped = escapeHtml(line);
        const headingMatch = escaped.match(/^(#{1,6} )(.*)$/);
        const quoteMatch = escaped.match(/^(&gt; )(.*)$/);
        const listMatch = escaped.match(/^([-*] )(.*)$/);
        let prefix = '';
        let rest = escaped;
        if (headingMatch) {
          prefix = `<span class="md-heading">${headingMatch[1]}</span>`;
          rest = headingMatch[2];
        } else if (quoteMatch) {
          prefix = `<span class="md-quote">${quoteMatch[1]}</span>`;
          rest = quoteMatch[2];
        } else if (listMatch) {
          prefix = `<span class="md-list-marker">${listMatch[1]}</span>`;
          rest = listMatch[2];
        }
        return prefix + renderInline(rest);
      })
      .join('\n');
  }

  // 浏览器在 contenteditable 中可能插入 <br>/<div>；这里统一抽出纯文本，
  // 让换行只通过 \n 表达，配合 CSS white-space: pre-wrap 渲染。
  function extractEditorText(root: Node): string {
    let result = '';
    const blockTags = new Set(['DIV', 'P', 'LI', 'BLOCKQUOTE', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6']);
    function walk(node: Node) {
      if (node.nodeType === Node.TEXT_NODE) {
        result += node.nodeValue ?? '';
        return;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      const el = node as HTMLElement;
      if (el.tagName === 'BR') {
        result += '\n';
        return;
      }
      const isBlock = blockTags.has(el.tagName);
      if (isBlock && result.length > 0 && !result.endsWith('\n')) {
        result += '\n';
      }
      for (const child of Array.from(el.childNodes)) walk(child);
    }
    for (const child of Array.from(root.childNodes)) walk(child);
    return result;
  }

  function readEditorText(): string {
    if (!inputTextareaEl) return inputValue;
    return extractEditorText(inputTextareaEl);
  }

  function getEditorCaretOffset(): number {
    if (!inputTextareaEl) return 0;
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return 0;
    const range = selection.getRangeAt(0);
    if (!inputTextareaEl.contains(range.endContainer)) return inputValue.length;
    const pre = range.cloneRange();
    pre.selectNodeContents(inputTextareaEl);
    pre.setEnd(range.endContainer, range.endOffset);
    return pre.toString().length;
  }

  function setEditorCaretOffset(offset: number) {
    if (!inputTextareaEl) return;
    const selection = window.getSelection();
    if (!selection) return;
    const clamped = Math.max(0, offset);
    const range = document.createRange();
    let remaining = clamped;
    const walker = document.createTreeWalker(inputTextareaEl, NodeFilter.SHOW_TEXT);
    let lastTextNode: Text | null = null;
    let node = walker.nextNode() as Text | null;
    while (node) {
      lastTextNode = node;
      if (remaining <= node.data.length) {
        range.setStart(node, remaining);
        range.collapse(true);
        selection.removeAllRanges();
        selection.addRange(range);
        return;
      }
      remaining -= node.data.length;
      node = walker.nextNode() as Text | null;
    }
    if (lastTextNode) {
      range.setStart(lastTextNode, lastTextNode.data.length);
    } else {
      range.setStart(inputTextareaEl, inputTextareaEl.childNodes.length);
    }
    range.collapse(true);
    selection.removeAllRanges();
    selection.addRange(range);
  }

  function focusEditor() {
    inputTextareaEl?.focus();
  }

  // 当 inputValue 由外部驱动（技能选择、enhance 等）变化时，
  // 与 DOM 比对一次，必要时重渲染并恢复 pendingCaretOffset。
  $effect(() => {
    const value = inputValue;
    if (!inputTextareaEl) return;
    if (isComposing) return;
    const current = extractEditorText(inputTextareaEl);
    if (current === value) {
      if (pendingCaretOffset !== null) {
        const target = pendingCaretOffset;
        pendingCaretOffset = null;
        queueMicrotask(() => setEditorCaretOffset(target));
      }
      return;
    }
    inputTextareaEl.innerHTML = buildHighlightedHtml(value);
    const target = pendingCaretOffset ?? value.length;
    pendingCaretOffset = null;
    queueMicrotask(() => setEditorCaretOffset(target));
  });


  function closeSlashMenu() {
    slashTriggerStart = null;
    slashFilter = '';
    slashHighlightIndex = 0;
  }

  // 仅在光标前是行首或空白时认定 `/` 是触发字符，避免 URL/路径里的斜杠误触。
  function recomputeSlashState() {
    if (!inputTextareaEl) {
      closeSlashMenu();
      return;
    }
    const cursor = getEditorCaretOffset();
    const value = readEditorText();
    if (cursor === 0) {
      closeSlashMenu();
      return;
    }
    let i = cursor - 1;
    let triggerAt: number | null = null;
    while (i >= 0) {
      const ch = value[i];
      if (ch === '/') {
        const prev = i > 0 ? value[i - 1] : '';
        const isLineStart = i === 0 || prev === '\n' || prev === ' ' || prev === '\t';
        triggerAt = isLineStart ? i : null;
        break;
      }
      if (ch === ' ' || ch === '\n' || ch === '\t') break;
      i--;
    }
    if (triggerAt === null) {
      closeSlashMenu();
      return;
    }
    slashTriggerStart = triggerAt;
    slashFilter = value.slice(triggerAt + 1, cursor);
    if (slashHighlightIndex >= filteredSkills.length) {
      slashHighlightIndex = 0;
    }
  }

  function commitSkill(skill: SkillOption) {
    selectedSkill = skill;
    if (inputTextareaEl && slashTriggerStart !== null) {
      const cursor = getEditorCaretOffset();
      const value = readEditorText();
      const before = value.slice(0, slashTriggerStart);
      const after = value.slice(cursor);
      pendingCaretOffset = before.length;
      inputValue = `${before}${after}`;
      queueMicrotask(focusEditor);
    }
    closeSlashMenu();
  }

  function removeSelectedSkill() {
    selectedSkill = null;
    queueMicrotask(focusEditor);
  }

  function handleComposerInput() {
    if (isComposing) return;
    if (!inputTextareaEl) return;
    const offset = getEditorCaretOffset();
    const text = readEditorText();
    inputTextareaEl.innerHTML = buildHighlightedHtml(text);
    setEditorCaretOffset(offset);
    inputValue = text;
    recomputeSlashState();
  }

  function handleComposerSelectionChange() {
    if (slashTriggerStart !== null) recomputeSlashState();
  }

  function handleCompositionStart() {
    isComposing = true;
  }

  function handleCompositionEnd() {
    isComposing = false;
    handleComposerInput();
  }

  function selectAccessProfile(profile: AccessProfile) {
    selectedAccessProfile = profile;
    accessProfilePickerOpen = false;
    writeStoredAccessProfile(profile);
  }

  function workspaceBinding(workspace: ComposerWorkspaceOption | null): { workspaceId?: string; workspacePath?: string } {
    if (!workspace) return {};
    return {
      workspaceId: workspace.workspaceId,
      workspacePath: workspace.rootPath,
    };
  }

  function workspaceKey(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return '';
    return `${workspace.workspaceId}::${workspace.rootPath}`;
  }

  function composerWorkspaceLabel(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return i18n.t('input.workspace.select');
    return workspace.name || workspace.rootPath;
  }

  function composerWorkspaceTitle(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return i18n.t('input.workspace.required');
    return `${i18n.t('input.workspace.title')}: ${workspace.rootPath}`;
  }

  function workspaceButtonLabel(workspace: ComposerWorkspaceOption | null): string {
    if (workspace) return composerWorkspaceLabel(workspace);
    return isDraftSession ? i18n.t('input.workspace.select') : i18n.t('input.workspace.title');
  }

  function workspaceButtonTitle(workspace: ComposerWorkspaceOption | null): string {
    if (isDraftSession) return composerWorkspaceTitle(workspace);
    if (!workspace) return i18n.t('input.workspace.lockedPending');
    return i18n.t('input.workspace.locked', { name: composerWorkspaceLabel(workspace) });
  }

  function selectWorkspace(workspaceId: string) {
    if (!isDraftSession || sessionInputLocked || isInteractionBlocking) return;
    const workspace = selectComposerDraftWorkspace(workspaceId);
    if (!workspace) return;
    workspacePickerOpen = false;
    void refreshBranchState();
  }

  function resolveSubmissionWorkspace(): ComposerWorkspaceOption | null {
    return resolveComposerWorkspace(currentWorkspaceId, currentWorkspacePath, isDraftSession);
  }

  onMount(() => {
    selectedAccessProfile = readStoredAccessProfile();

    function handleFillComposer(event: Event) {
      const text = (event as CustomEvent<{ text?: string }>).detail?.text;
      if (typeof text !== 'string' || !text.trim()) return;
      clearEnhanceSnapshot();
      pendingCaretOffset = text.length;
      inputValue = text;
      queueMicrotask(focusEditor);
    }
    function handleSetAccessProfile(event: Event) {
      const profile = (event as CustomEvent<{ profile?: unknown }>).detail?.profile;
      if (!isAccessProfile(profile)) return;
      selectAccessProfile(profile);
      addToast('success', i18n.t('input.access.switched', {
        mode: i18n.t(accessProfileOptions.find((option) => option.value === profile)?.labelKey ?? 'input.access.restricted'),
      }));
    }
    function handlePickerOutsidePointerDown(event: PointerEvent) {
      const target = event.target;
      if (workspacePickerOpen && !(target instanceof Element && target.closest('.ia-workspace-wrap'))) {
        workspacePickerOpen = false;
      }
      if (accessProfilePickerOpen && !(target instanceof Element && target.closest('.ia-access-wrap'))) {
        accessProfilePickerOpen = false;
      }
    }
    window.addEventListener('magi:fillComposer', handleFillComposer as EventListener);
    window.addEventListener('magi:setAccessProfile', handleSetAccessProfile as EventListener);
    document.addEventListener('pointerdown', handlePickerOutsidePointerDown, true);
    return () => {
      window.removeEventListener('magi:fillComposer', handleFillComposer as EventListener);
      window.removeEventListener('magi:setAccessProfile', handleSetAccessProfile as EventListener);
      document.removeEventListener('pointerdown', handlePickerOutsidePointerDown, true);
    };
  });

  // 分支状态随 composer 工作区 reactive 重查：草稿态允许用户先选工作区再首发，
  // 因此不能只读后端当前绑定。监听 composerWorkspace 可避免草稿态显示旧分支。
  // path 为空则不查（等 hydrate / 工作区列表），非空才查。
  $effect(() => {
    const workspacePath = composerWorkspace?.rootPath;
    // 读 currentSessionId 建立依赖：切会话也重查（分支状态与工作树绑定）。
    void currentSessionId;
    if (typeof workspacePath !== 'string' || !workspacePath.trim()) {
      branchRequestSeq += 1;
      branchIsRepo = false;
      branchLoading = false;
      branchError = null;
      return;
    }
    branchLoading = false;
    branchError = null;
    void refreshBranchState();
  });

  function resolveComposerRawContent(): string {
    if (inputTextareaEl) {
      return extractEditorText(inputTextareaEl);
    }
    return inputValue;
  }

  // 发送消息（支持图片附件）。
  // 空闲时直接执行；正在响应时自动进入排队，由 bridge 在当前轮结束后逐条提交。
  async function sendMessage() {
    if (sendPreparing) return;
    sendPreparing = true;
    try {
      await waitForPendingImageReads();
      const rawContent = resolveComposerRawContent();
      const normalizedContent = rawContent.trim();
      if ((!normalizedContent && selectedImages.length === 0) || sessionInputLocked || isInteractionBlocking) return;
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

      const targetWorkspace = resolveSubmissionWorkspace();
      if (!targetWorkspace) {
        addToast('warning', i18n.t('input.workspace.required'));
        return;
      }

      const requestId = generateId();
      vscode.postMessage({
        type: 'executeTask',
        text: submissionText,
        requestId,
        workspaceId: targetWorkspace.workspaceId,
        workspacePath: targetWorkspace.rootPath,
        sessionId: isDraftSession ? '' : (messagesState.currentSessionId || ''),
        skillName: selectedSkill?.name ?? null,
        accessProfile: selectedAccessProfile,
        followUpMode: isSending ? 'queue' : undefined,
        images: selectedImages.map((img) => ({
          name: img.name,
          dataUrl: img.dataUrl,
        })),
      });
      clearComposerState();
    } finally {
      sendPreparing = false;
    }
  }

  function insertNewlineAtCursor() {
    if (!inputTextareaEl) {
      inputValue += '\n';
      return;
    }
    const offset = getEditorCaretOffset();
    const value = readEditorText();
    pendingCaretOffset = offset + 1;
    inputValue = `${value.slice(0, offset)}\n${value.slice(offset)}`;
  }

  function isEnterKey(event: KeyboardEvent): boolean {
    return event.key === 'Enter' || event.code === 'Enter' || event.code === 'NumpadEnter';
  }

  // 处理键盘事件
  function handleKeydown(event: KeyboardEvent) {
    if (slashMenuOpen) {
      // 斜杠菜单展开时优先处理导航；输入法组合态下不拦截，交给 IME 完成上屏。
      if (!event.isComposing && event.keyCode !== 229) {
        if (event.key === 'ArrowDown') {
          event.preventDefault();
          slashHighlightIndex = (slashHighlightIndex + 1) % filteredSkills.length;
          return;
        }
        if (event.key === 'ArrowUp') {
          event.preventDefault();
          slashHighlightIndex = (slashHighlightIndex - 1 + filteredSkills.length) % filteredSkills.length;
          return;
        }
        if (event.key === 'Escape') {
          event.preventDefault();
          closeSlashMenu();
          return;
        }
        if (event.key === 'Tab' || isEnterKey(event)) {
          event.preventDefault();
          const chosen = filteredSkills[slashHighlightIndex] ?? filteredSkills[0];
          if (chosen) commitSkill(chosen);
          return;
        }
      }
    }
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

  // 任务投影运行时，输入框停止入口与任务面板共用同一条可恢复中断链路。
  async function stopTask() {
    if (stopLoading) return;
    stopLoading = true;
    try {
      if (shouldInterruptTaskProjectionFromComposer) {
        const projection = taskProjection.projection;
        const sessionId = currentSessionId?.trim();
        const rootTaskId = projection?.root_task.task_id ?? taskProjection.rootTaskId;
        if (sessionId && rootTaskId) {
          const client = new RustDaemonClient(resolveAgentBaseUrl());
          await client.interruptTask({
            taskId: rootTaskId,
            sessionId,
            workspaceId: currentWorkspaceId?.trim() || undefined,
            workspacePath: currentWorkspacePath?.trim() || undefined,
          });
          await refreshTaskProjection(
            sessionId,
            currentWorkspaceId?.trim() || undefined,
            currentWorkspacePath?.trim() || undefined,
          );
          addToast('info', i18n.t('input.stopTaskSaved'));
        }
        return;
      }
      vscode.postMessage({ type: 'interruptTask' });
    } catch (err) {
      console.warn('[InputArea] stop task failed:', err);
      addToast('error', i18n.t('input.stopFailed'));
    } finally {
      stopLoading = false;
    }
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
    clearEnhanceSnapshot();
    pendingCaretOffset = text.length;
    inputValue = text;
    queueMicrotask(focusEditor);
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

  // 🔧 处理粘贴事件（支持图片粘贴 + 纯文本插入）
  function handlePaste(event: ClipboardEvent) {
    const imageFiles = collectClipboardImageFiles(event.clipboardData);
    if (imageFiles.length > 0) {
      event.preventDefault();
      for (const file of imageFiles) {
        if (selectedImages.length + pendingImageReadCount >= MAX_IMAGES) {
          addToast('warning', i18n.t('input.maxImages', { max: MAX_IMAGES }));
          break;
        }
        if (file.size > MAX_IMAGE_SIZE) {
          addToast('warning', i18n.t('input.imageTooLarge', { size: (file.size / 1024 / 1024).toFixed(1) }));
          continue;
        }
        readImageFileIntoComposer(file);
      }
      return;
    }

    // 纯文本路径：阻止浏览器把 HTML 粘进 contenteditable，统一按 \n 文本插入
    const text = event.clipboardData?.getData('text/plain');
    if (typeof text !== 'string' || text.length === 0) return;
    event.preventDefault();
    if (!inputTextareaEl) {
      pendingCaretOffset = (inputValue.length + text.length);
      inputValue = inputValue + text;
      return;
    }
    const offset = getEditorCaretOffset();
    const current = readEditorText();
    pendingCaretOffset = offset + text.length;
    inputValue = `${current.slice(0, offset)}${text}${current.slice(offset)}`;
  }

  // 🔧 删除已选图片
  function removeImage(imageId: string) {
    selectedImages = selectedImages.filter(img => img.id !== imageId);
  }

  // 🔧 清空所有图片
  function clearAllImages() {
    selectedImages = [];
  }

  function getOrchestratorConfigSnapshot(): Record<string, unknown> | null {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      return null;
    }
    const orchestratorConfig = snapshot?.orchestratorConfig;
    if (!orchestratorConfig || typeof orchestratorConfig !== 'object' || Array.isArray(orchestratorConfig)) {
      return null;
    }
    return orchestratorConfig as Record<string, unknown>;
  }

  function getEffectiveOrchestratorConfigSnapshot(): Record<string, unknown> | null {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      return null;
    }
    const effectiveConfig = snapshot?.effectiveOrchestratorConfig;
    if (effectiveConfig && typeof effectiveConfig === 'object' && !Array.isArray(effectiveConfig)) {
      return effectiveConfig as Record<string, unknown>;
    }
    return getOrchestratorConfigSnapshot();
  }

  function getOrchestratorSessionConfigSnapshot(): Record<string, unknown> {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      return {};
    }
    const sessionConfig = snapshot?.orchestratorSessionConfig;
    if (!sessionConfig || typeof sessionConfig !== 'object' || Array.isArray(sessionConfig)) {
      return {};
    }
    return sessionConfig as Record<string, unknown>;
  }

  function readOrchestratorModel(): string {
    const config = getEffectiveOrchestratorConfigSnapshot();
    const model = config?.model;
    return typeof model === 'string' ? model.trim() : '';
  }

  function normalizeReasoningEffort(value: unknown): ReasoningEffort | null {
    return value === 'low' || value === 'medium' || value === 'high' || value === 'xhigh'
      ? value
      : null;
  }

  function readOrchestratorReasoningEffort(): ReasoningEffort | null {
    const config = getEffectiveOrchestratorConfigSnapshot();
    return normalizeReasoningEffort(config?.reasoningEffort);
  }

  function reasoningEffortLabel(value: ReasoningEffort | null): string {
    if (!value) return '';
    const match = reasoningOptions.find((option) => option.value === value);
    return match ? i18n.t(match.labelKey) : '';
  }

  function objectRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {};
  }

  function getAuxiliaryConfigSnapshot(): Record<string, unknown> | null {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      return null;
    }
    const auxiliaryConfig = snapshot?.auxiliaryConfig;
    if (!auxiliaryConfig || typeof auxiliaryConfig !== 'object' || Array.isArray(auxiliaryConfig)) {
      return null;
    }
    return auxiliaryConfig as Record<string, unknown>;
  }

  function hasUsableModelConfig(config: Record<string, unknown> | null): boolean {
    if (!config) {
      return false;
    }
    const baseUrl = typeof config.baseUrl === 'string' ? config.baseUrl.trim() : '';
    const apiKey = typeof config.apiKey === 'string' ? config.apiKey.trim() : '';
    const model = typeof config.model === 'string' ? config.model.trim() : '';
    return Boolean(baseUrl && apiKey && model);
  }

  function clearEnhanceSnapshot() {
    enhanceOriginalPrompt = null;
    enhanceResultPrompt = null;
  }

  function applyLocalOrchestratorSessionConfig(
    sessionConfig: Record<string, unknown>,
    effectiveConfig: Record<string, unknown>,
  ) {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!snapshot || !settingsBootstrapMatchesCurrentWorkspace(snapshot)) return;
    messagesState.settingsBootstrapSnapshot = {
      ...snapshot,
      orchestratorSessionConfig: { ...sessionConfig },
      effectiveOrchestratorConfig: { ...effectiveConfig },
    } as AgentSettingsBootstrapSnapshot;
  }

  async function refreshPickerSettingsSnapshot() {
    const latest = await getAgentSettingsBootstrap({ scope: 'core', accessProfile: selectedAccessProfile });
    if (!settingsBootstrapMatchesCurrentWorkspace(latest)) {
      return;
    }
    messagesState.settingsBootstrapSnapshot = latest;
  }

  // 主线模型 picker：打开 / 关闭 + 模型列表惰性拉取。
  // 模型列表读取全局 orchestrator 连接配置；保存只写当前会话覆盖段。
  async function togglePicker() {
    if (pickerOpen) {
      pickerOpen = false;
      return;
    }
    pickerOpen = true;
    if (!pickerLoadedOnce && !pickerLoading) {
      await loadPickerModels();
    }
  }
  async function loadPickerModels() {
    const orchestratorConfig = getOrchestratorConfigSnapshot();
    if (!orchestratorConfig) {
      pickerError = i18n.t('input.modelPickerNotReady');
      pickerLoading = false;
      return;
    }
    pickerLoading = true;
    pickerError = null;
    try {
      const payload = await fetchAgentModelList(
        orchestratorConfig as Record<string, unknown>,
        'orch',
      );
      pickerModels = Array.isArray(payload.models) ? payload.models : [];
      pickerLoadedOnce = true;
    } catch (error) {
      console.warn('[InputArea] 拉取主线模型列表失败:', error);
      pickerError = i18n.t('input.modelListLoadFailed');
    } finally {
      pickerLoading = false;
    }
  }
  async function selectPickerModel(model: string) {
    const normalizedModel = model.trim();
    if (!normalizedModel) return;
    if (normalizedModel === currentPickerModel) {
      pickerOpen = false;
      return;
    }
    const sessionId = currentSessionId?.trim() || '';
    if (!sessionId) {
      pickerError = i18n.t('input.modelPickerSessionRequired');
      return;
    }

    pickerSavingModel = normalizedModel;
    pickerError = null;
    const nextSessionConfig = {
      ...getOrchestratorSessionConfigSnapshot(),
      model: normalizedModel,
    };
    try {
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, {
        sessionId,
        workspaceId: currentWorkspaceId?.trim() || undefined,
        workspacePath: currentWorkspacePath?.trim() || undefined,
      });
      applyLocalOrchestratorSessionConfig(
        objectRecord(saved.orchestratorSessionConfig),
        objectRecord(saved.effectiveOrchestratorConfig),
      );
      try {
        await refreshPickerSettingsSnapshot();
      } catch (error) {
        console.warn('[InputArea] 切换主线模型后刷新设置快照失败:', error);
        addToast('warning', i18n.t('input.modelSavedSyncPending'));
      }
      addToast('success', i18n.t('input.modelSwitched', { model: normalizedModel }));
      pickerOpen = false;
    } catch (error) {
      console.warn('[InputArea] 保存主线模型失败:', error);
      pickerError = i18n.t('input.modelSaveFailed');
      addToast('error', pickerError);
    } finally {
      pickerSavingModel = null;
    }
  }

  async function selectPickerReasoningEffort(value: ReasoningEffort) {
    const sessionId = currentSessionId?.trim() || '';
    if (!sessionId) {
      pickerError = i18n.t('input.modelPickerSessionRequired');
      return;
    }
    if (value === currentPickerReasoningEffort) {
      return;
    }
    pickerSavingReasoning = value;
    pickerError = null;
    const nextSessionConfig = {
      ...getOrchestratorSessionConfigSnapshot(),
      reasoningEffort: value,
    };
    try {
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, {
        sessionId,
        workspaceId: currentWorkspaceId?.trim() || undefined,
        workspacePath: currentWorkspacePath?.trim() || undefined,
      });
      applyLocalOrchestratorSessionConfig(
        objectRecord(saved.orchestratorSessionConfig),
        objectRecord(saved.effectiveOrchestratorConfig),
      );
      addToast('success', i18n.t('input.reasoningSwitched', { level: reasoningEffortLabel(value) }));
    } catch (error) {
      console.warn('[InputArea] 保存主线思考强度失败:', error);
      pickerError = i18n.t('input.reasoningSaveFailed');
      addToast('error', pickerError);
    } finally {
      pickerSavingReasoning = null;
    }
  }

  // 初次拉取分支状态：决定左下角分支入口是否显示，以及当前分支文案。
  function applyBranchResult(result: { isRepo: boolean; currentBranch: string | null; branches: string[]; additions: number; deletions: number }) {
    branchIsRepo = result.isRepo;
    currentBranch = result.currentBranch;
    branches = result.branches;
    branchAdditions = result.additions ?? 0;
    branchDeletions = result.deletions ?? 0;
  }

  async function refreshBranchState() {
    const requestSeq = ++branchRequestSeq;
    const requestWorkspace = composerWorkspace;
    const requestWorkspaceKey = workspaceKey(requestWorkspace);
    try {
      const result = await fetchWorkspaceBranches(workspaceBinding(requestWorkspace));
      if (requestSeq !== branchRequestSeq || workspaceKey(composerWorkspace) !== requestWorkspaceKey) {
        return;
      }
      applyBranchResult(result);
    } catch (error) {
      if (requestSeq !== branchRequestSeq || workspaceKey(composerWorkspace) !== requestWorkspaceKey) {
        return;
      }
      // 拉取失败时静默隐藏入口，不打扰用户（git 能力是增强项，非核心链路）。
      branchIsRepo = false;
      console.warn('[InputArea] 拉取工作区分支失败:', error);
    }
  }

  async function toggleBranchPicker() {
    if (branchPickerOpen) {
      branchPickerOpen = false;
      return;
    }
    branchPickerOpen = true;
    if (!branchLoading) {
      await loadBranches();
    }
  }

  async function loadBranches() {
    const requestSeq = ++branchRequestSeq;
    const requestWorkspace = composerWorkspace;
    const requestWorkspaceKey = workspaceKey(requestWorkspace);
    branchLoading = true;
    branchError = null;
    try {
      const result = await fetchWorkspaceBranches(workspaceBinding(requestWorkspace));
      if (requestSeq !== branchRequestSeq || workspaceKey(composerWorkspace) !== requestWorkspaceKey) {
        return;
      }
      applyBranchResult(result);
    } catch (error) {
      if (requestSeq !== branchRequestSeq || workspaceKey(composerWorkspace) !== requestWorkspaceKey) {
        return;
      }
      console.warn('[InputArea] 读取工作区分支失败:', error);
      branchError = i18n.t('input.branch.loadFailed');
    } finally {
      if (requestSeq === branchRequestSeq) {
        branchLoading = false;
      }
    }
  }

  async function selectBranch(branch: string) {
    const target = branch.trim();
    if (!target) return;
    // 任务进行中禁止切换：切走工作树会破坏运行中 agent 的文件一致性。
    if (sessionInputLocked || isInteractionBlocking) return;
    if (target === currentBranch) {
      branchPickerOpen = false;
      return;
    }
    branchSwitching = target;
    branchError = null;
    const requestWorkspace = composerWorkspace;
    const requestWorkspaceKey = workspaceKey(requestWorkspace);
    try {
      const result = await checkoutWorkspaceBranch(target, workspaceBinding(requestWorkspace));
      if (workspaceKey(composerWorkspace) !== requestWorkspaceKey) {
        return;
      }
      if (result.ok) {
        currentBranch = result.currentBranch ?? target;
        addToast('success', i18n.t('input.branch.switched', { branch: currentBranch }));
        branchPickerOpen = false;
        // 切换后工作区改动行数可能变化（如非冲突改动跟随切换），重新拉取以刷新计数。
        void refreshBranchState();
        // 广播工作区内容变更：文件树 / RightPane 等视图据此刷新，避免停留在旧分支内容。
        window.dispatchEvent(new CustomEvent('magi:workspaceContentChanged', {
          detail: { reason: 'branchSwitched', branch: currentBranch },
        }));
      } else {
        console.warn('[InputArea] 切换工作区分支被拒绝:', result.error);
        branchError = i18n.t('input.branch.switchFailed');
        addToast('error', i18n.t('input.branch.switchFailed'));
      }
    } catch (error) {
      console.warn('[InputArea] 切换工作区分支失败:', error);
      branchError = i18n.t('input.branch.switchFailed');
      addToast('error', i18n.t('input.branch.switchFailed'));
    } finally {
      branchSwitching = null;
    }
  }

  // 设计原则：只做一次确定性还原；任何解析失败都退回原文，避免吞掉用户实际想要的内容。
  function unwrapEnhancedPromptPayload(raw: string): string {
    let text = raw.trim();
    if (!text) return text;
    const fenceMatch = text.match(/^```(?:json|markdown|md|text)?\s*\n?([\s\S]*?)\n?```$/i);
    if (fenceMatch) {
      text = fenceMatch[1].trim();
    }
    if ((text.startsWith('{') && text.endsWith('}')) || (text.startsWith('[') && text.endsWith(']'))) {
      try {
        const parsed = JSON.parse(text);
        const candidate = extractEnhancedContent(parsed);
        if (candidate) text = candidate;
      } catch { /* 解析失败保持原样 */ }
    }
    return text.trim();
  }

  function extractEnhancedContent(value: unknown): string | null {
    if (typeof value === 'string') return value;
    if (!value || typeof value !== 'object') return null;
    const obj = value as Record<string, unknown>;
    const keys = ['enhancedPrompt', 'enhanced_prompt', 'content', 'text', 'prompt', 'result', 'output'];
    for (const key of keys) {
      const inner = obj[key];
      if (typeof inner === 'string' && inner.trim()) return inner;
      if (inner && typeof inner === 'object') {
        const nested = extractEnhancedContent(inner);
        if (nested) return nested;
      }
    }
    return null;
  }

  // Prompt enhance：调用后端模型重写当前 textarea 文本
  // 这里固定走辅助模型，不占用主线模型配额；如果存在选中的技能上下文，一并传给后端增强。
  async function enhancePromptHandler() {
    const draft = resolveComposerRawContent();
    const normalizedDraft = draft.trim();
    if (enhanceLoading || !normalizedDraft || !auxiliaryEnhanceReady) return;
    enhanceLoading = true;
    try {
      const result = await enhanceAgentPrompt({
        prompt: normalizedDraft,
        skillName: selectedSkill?.name?.trim() || null,
        skillDescription: selectedSkill?.description?.trim() || null,
      });
      const next = unwrapEnhancedPromptPayload(result?.enhancedPrompt ?? '');
      if (!next) {
        if (result?.error) {
          console.warn('[InputArea] 提示词优化返回错误:', result.error);
        }
        addToast('warning', i18n.t('input.enhance.empty'));
        return;
      }
      enhanceOriginalPrompt = draft;
      enhanceResultPrompt = next;
      inputValue = next;
      pendingCaretOffset = next.length;
      queueMicrotask(focusEditor);
      addToast('success', i18n.t('input.enhance.success'));
    } catch (error) {
      console.warn('[InputArea] 提示词优化失败:', error);
      addToast('error', i18n.t('input.enhance.failed'));
    } finally {
      enhanceLoading = false;
    }
  }

  function restoreEnhancedPrompt() {
    if (!enhanceOriginalPrompt || !enhanceResultPrompt) return;
    inputValue = enhanceOriginalPrompt;
    pendingCaretOffset = enhanceOriginalPrompt.length;
    clearEnhanceSnapshot();
    queueMicrotask(focusEditor);
    addToast('info', i18n.t('input.enhance.restored'));
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

    <!-- 已选技能：显示为可移除的 chip，避免污染用户输入正文 -->
    {#if selectedSkill}
      <div class="ia-skill-chip-row">
        <span class="ia-skill-chip" title={selectedSkill.description}>
          <span class="ia-skill-chip-label">/{selectedSkill.name}</span>
          {#if selectedSkill.description}
            <span class="ia-skill-chip-desc">{selectedSkill.description}</span>
          {/if}
          <button
            type="button"
            class="ia-skill-chip-remove"
            onclick={removeSelectedSkill}
            title={i18n.t('input.removeSkill')}
            aria-label={i18n.t('input.removeSkill')}
          >
            <Icon name="close" size={10} />
          </button>
        </span>
      </div>
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      bind:this={inputTextareaEl}
      class="ia-textarea"
      data-testid="input-textarea"
      class:has-images={selectedImages.length > 0}
      class:is-empty={!inputValue}
      contenteditable={!(sessionInputLocked || isInteractionBlocking)}
      role="textbox"
      tabindex={sessionInputLocked || isInteractionBlocking ? -1 : 0}
      aria-multiline="true"
      aria-disabled={sessionInputLocked || isInteractionBlocking}
      data-placeholder={selectedSkill
        ? i18n.t('input.placeholderWithSkill', { skillName: selectedSkill.name })
        : selectedImages.length > 0
          ? i18n.t('input.placeholderWithImages')
          : i18n.t('input.placeholderDefault')}
      onkeydown={handleKeydown}
      oninput={handleComposerInput}
      onkeyup={handleComposerSelectionChange}
      onclick={handleComposerSelectionChange}
      onblur={() => queueMicrotask(closeSlashMenu)}
      oncompositionstart={handleCompositionStart}
      oncompositionend={handleCompositionEnd}
      onpaste={handlePaste}
    ></div>

    {#if slashMenuOpen}
      <div class="ia-slash-popover" role="listbox" aria-label={i18n.t('input.useSkill')}>
        <div class="ia-slash-list" bind:this={slashListEl}>
          {#each filteredSkills as skill, index (skill.name)}
            <button
              type="button"
              role="option"
              aria-selected={index === slashHighlightIndex}
              class="ia-slash-item"
              class:active={index === slashHighlightIndex}
              onmouseenter={() => (slashHighlightIndex = index)}
              onmousedown={(e) => { e.preventDefault(); commitSkill(skill); }}
            >
              <span class="ia-slash-item-label">{skill.name}</span>
            </button>
          {/each}
        </div>
        {#if filteredSkills[slashHighlightIndex]}
          {@const activeName = filteredSkills[slashHighlightIndex].name}
          {@const previewText = skillPreviewCache[activeName]}
          {#if previewText !== undefined && previewText !== ''}
            <div class="ia-slash-tooltip" style="top: {slashTooltipTop}px">
              {previewText}
            </div>
          {:else if skillPreviewLoading[activeName]}
            <div class="ia-slash-tooltip ia-slash-tooltip-loading" style="top: {slashTooltipTop}px">
              …
            </div>
          {/if}
        {/if}
      </div>
    {/if}

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
        <div class="ia-picker-wrap ia-workspace-wrap">
          <button
            type="button"
            class="ia-workspace-btn"
            class:active={workspacePickerOpen}
            class:configured={composerWorkspace !== null}
            class:locked={!isDraftSession}
            onclick={() => {
              if (isDraftSession && workspaceOptions.length > 0) {
                workspacePickerOpen = !workspacePickerOpen;
              }
            }}
            disabled={workspaceOptions.length === 0 || sessionInputLocked || isInteractionBlocking}
            title={workspaceButtonTitle(composerWorkspace)}
            aria-expanded={workspacePickerOpen}
            aria-disabled={!isDraftSession || sessionInputLocked || isInteractionBlocking}
          >
            <Icon name="folder" size={12} />
            <span class="ia-workspace-btn-label">{workspaceButtonLabel(composerWorkspace)}</span>
          </button>
          {#if workspacePickerOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-popover-backdrop" onclick={() => (workspacePickerOpen = false)}></div>
            <div class="ia-picker-popover ia-workspace-popover" role="menu">
              <div class="ia-picker-header">{i18n.t('input.workspace.title')}</div>
              {#if workspaceOptions.length === 0}
                <div class="ia-picker-status">{i18n.t('input.workspace.empty')}</div>
              {:else}
                <div class="ia-picker-list">
                  {#each workspaceOptions as workspace (workspace.workspaceId)}
                    <button
                      type="button"
                      class="ia-picker-item"
                      class:selected={composerWorkspace?.workspaceId === workspace.workspaceId}
                      onclick={() => selectWorkspace(workspace.workspaceId)}
                    >
                      <span class="ia-picker-item-label">{workspace.name}</span>
                      <span class="ia-picker-item-desc">{workspace.rootPath}</span>
                    </button>
                  {/each}
                </div>
              {/if}
            </div>
          {/if}
        </div>
        {#if branchIsRepo}
          <div class="ia-picker-wrap">
            <button
              type="button"
              class="ia-branch-btn"
              class:active={branchPickerOpen}
              onclick={toggleBranchPicker}
              disabled={branchSwitching !== null || sessionInputLocked || isInteractionBlocking}
              title={`${i18n.t('input.branch.title')}: ${currentBranch || '—'}`}
              aria-expanded={branchPickerOpen}
            >
              <span class="ia-branch-btn-label">{currentBranch || '—'}</span>
              {#if branchAdditions > 0 || branchDeletions > 0}
                <span class="ia-branch-diffstat">
                  <span class="ia-branch-add">+{branchAdditions}</span>
                  <span class="ia-branch-del">-{branchDeletions}</span>
                </span>
              {/if}
            </button>
            {#if branchPickerOpen}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="ia-popover-backdrop" onclick={() => (branchPickerOpen = false)}></div>
              <div class="ia-picker-popover ia-branch-popover" role="menu">
                <div class="ia-picker-header">{i18n.t('input.branch.title')}</div>
                {#if branchLoading}
                  <div class="ia-picker-status">{i18n.t('input.branch.loading')}</div>
                {:else if branchError}
                  <div class="ia-picker-status ia-picker-status-error">
                    {branchError}
                    <button
                      type="button"
                      class="ia-picker-retry"
                      onclick={() => { branchError = null; loadBranches(); }}
                    >{i18n.t('input.branch.retry')}</button>
                  </div>
                {:else if branches.length === 0}
                  <div class="ia-picker-status">{i18n.t('input.branch.empty')}</div>
                {:else}
                  <div class="ia-picker-list">
                    {#each branches as branch (branch)}
                      <button
                        type="button"
                        class="ia-picker-item"
                        class:selected={currentBranch === branch}
                        onclick={() => void selectBranch(branch)}
                        disabled={branchSwitching !== null}
                      >
                        <span class="ia-picker-item-label">{branch}</span>
                        {#if branchSwitching === branch}
                          <span class="ia-picker-item-desc">{i18n.t('input.branch.switching')}</span>
                        {:else if currentBranch === branch}
                          <span class="ia-picker-item-desc">{i18n.t('input.branch.current')}</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/if}
      </div>

      <div class="ia-right">
        <div class="ia-picker-wrap ia-access-wrap">
          <button
            type="button"
            class="ia-picker-btn ia-access-btn"
            class:active={accessProfilePickerOpen}
            onclick={() => (accessProfilePickerOpen = !accessProfilePickerOpen)}
            disabled={sessionInputLocked || isInteractionBlocking}
            title={`${i18n.t('input.access.title')}: ${i18n.t(currentAccessProfileOption.labelKey)}`}
            aria-expanded={accessProfilePickerOpen}
            aria-label={i18n.t('input.access.title')}
          >
            <span class="ia-access-btn-label">{i18n.t(currentAccessProfileOption.labelKey)}</span>
          </button>
          {#if accessProfilePickerOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-popover-backdrop" onclick={() => (accessProfilePickerOpen = false)}></div>
            <div class="ia-picker-popover ia-access-popover" role="menu">
              <div class="ia-picker-header">{i18n.t('input.access.title')}</div>
              <div class="ia-picker-list">
                {#each accessProfileOptions as option (option.value)}
                  <button
                    type="button"
                    class="ia-picker-item"
                    class:selected={selectedAccessProfile === option.value}
                    onclick={() => selectAccessProfile(option.value)}
                  >
                    <span class="ia-picker-item-label">{i18n.t(option.labelKey)}</span>
                  </button>
                {/each}
              </div>
            </div>
          {/if}
        </div>
        <div class="ia-picker-wrap ia-model-wrap">
          <button
            type="button"
            class="ia-picker-btn ia-model-btn"
            class:active={pickerOpen}
            class:configured={currentPickerModel !== ''}
            onclick={togglePicker}
            disabled={sessionInputLocked || isInteractionBlocking || pickerSavingModel !== null || pickerSavingReasoning !== null}
            title={currentPickerModel
              ? i18n.t('input.mainModelPicker.titleConfigured', { model: currentPickerModel })
              : i18n.t('input.mainModelPicker.titleEmpty')}
            aria-expanded={pickerOpen}
          >
            <Icon name={pickerSavingModel || pickerSavingReasoning ? 'loader' : 'circleOutline'} size={12} class={pickerSavingModel || pickerSavingReasoning ? 'spinning' : ''} />
            <span class="ia-picker-btn-label">{currentPickerModel || i18n.t('input.mainModelPicker.buttonEmpty')}</span>
            {#if currentPickerReasoningLabel}
              <span class="ia-model-effort">{currentPickerReasoningLabel}</span>
            {/if}
            <Icon name="chevron-down" size={10} />
          </button>
          {#if pickerOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-popover-backdrop" onclick={() => (pickerOpen = false)}></div>
            <div class="ia-session-model-popover" role="menu">
              <div class="ia-effort-section">
                <div class="ia-picker-header">{i18n.t('input.mainModelPicker.reasoning.header')}</div>
                <div class="ia-effort-strip">
                  {#each reasoningOptions as option (option.labelKey)}
                    <button
                      type="button"
                      class="ia-effort-option"
                      class:selected={currentPickerReasoningEffort === option.value}
                      onclick={() => void selectPickerReasoningEffort(option.value)}
                      disabled={pickerSavingReasoning !== null || pickerSavingModel !== null}
                    >
                      <span>{i18n.t(option.labelKey)}</span>
                      {#if pickerSavingReasoning === option.value}
                        <Icon name="loader" size={12} class="spinning" />
                      {/if}
                    </button>
                  {/each}
                </div>
              </div>
              <div class="ia-picker-divider"></div>
              <div class="ia-model-list-section">
                <div class="ia-section-header-row">
                  <div class="ia-picker-header">{i18n.t('input.mainModelPicker.header')}</div>
                </div>
                {#if pickerLoading}
                  <div class="ia-picker-status">{i18n.t('input.mainModelPicker.loading')}</div>
                {:else if pickerError}
                  <div class="ia-picker-status ia-picker-status-error">
                    {pickerError}
                    <button
                      type="button"
                      class="ia-picker-retry"
                      onclick={() => { pickerError = null; loadPickerModels(); }}
                    >{i18n.t('input.mainModelPicker.retry')}</button>
                  </div>
                {:else if pickerModels.length === 0}
                  <div class="ia-picker-status">{i18n.t('input.mainModelPicker.empty')}</div>
                {:else}
                  <div class="ia-picker-list">
                    {#each pickerModels as model (model)}
                      <button
                        type="button"
                        class="ia-picker-item ia-picker-row"
                        class:selected={currentPickerModel === model}
                        onclick={() => void selectPickerModel(model)}
                        disabled={pickerSavingModel !== null || pickerSavingReasoning !== null}
                      >
                        <span class="ia-picker-item-label">{model}</span>
                        {#if pickerSavingModel === model}
                          <Icon name="loader" size={12} class="spinning" />
                        {:else if currentPickerModel === model}
                          <span class="ia-picker-check">✓</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>
          {/if}
        </div>
        <button
          type="button"
          class="ia-enhance"
          class:loading={enhanceLoading}
          onclick={enhancePromptHandler}
          disabled={enhanceLoading || !inputValue.trim() || sessionInputLocked || isInteractionBlocking || !auxiliaryEnhanceReady}
          title={enhanceButtonTitle}
          aria-label={enhanceButtonTitle}
        >
          <Icon name={enhanceLoading ? 'loader' : 'enhance'} size={14} class={enhanceLoading ? 'spinning' : ''} />
        </button>
        {#if hasEnhanceSnapshot}
          <button
            type="button"
            class="ia-enhance ia-enhance-restore"
            onclick={restoreEnhancedPrompt}
            disabled={sessionInputLocked || isInteractionBlocking}
            title={i18n.t('input.enhance.restore')}
            aria-label={i18n.t('input.enhance.restore')}
          >
            <Icon name="undo" size={14} />
          </button>
        {/if}
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
            title={shouldInterruptTaskProjectionFromComposer ? i18n.t('input.stopTaskTitle') : i18n.t('input.stop')}
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
    position: relative;
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
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    overflow-y: auto;
    cursor: text;
  }

  .ia-textarea.is-empty::before {
    content: attr(data-placeholder);
    color: var(--foreground-muted);
    pointer-events: none;
    display: block;
  }
  .ia-textarea[aria-disabled="true"] { opacity: 0.5; cursor: not-allowed; }
  .ia-textarea.has-images { min-height: 36px; }

  .ia-textarea :global(.md-bold) { font-weight: 600; }
  .ia-textarea :global(.md-italic) { font-style: italic; }
  .ia-textarea :global(.md-code) {
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 0.92em;
    background: color-mix(in srgb, var(--foreground) 8%, transparent);
    border-radius: 3px;
    padding: 0 3px;
  }
  .ia-textarea :global(.md-heading) {
    font-weight: 600;
    color: var(--primary, currentColor);
  }
  .ia-textarea :global(.md-quote) {
    color: var(--foreground-muted);
  }
  .ia-textarea :global(.md-list-marker) {
    color: var(--primary, currentColor);
    font-weight: 500;
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
    min-width: 0;
  }

  .ia-left, .ia-right {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }

  .ia-right {
    flex-wrap: nowrap;
    justify-content: flex-end;
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
    justify-content: center;
    height: 24px;
    width: 24px;
    padding: 0;
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
  .ia-enhance-restore {
    background: color-mix(in srgb, var(--primary) 10%, transparent);
    border-color: color-mix(in srgb, var(--primary) 32%, transparent);
    color: var(--primary);
  }
  .ia-enhance-restore:hover:not(:disabled) {
    background: color-mix(in srgb, var(--primary) 16%, transparent);
    border-color: color-mix(in srgb, var(--primary) 46%, transparent);
  }

  .ia-popover-backdrop {
    position: fixed;
    inset: 0;
    background: transparent;
    z-index: 30;
  }

  /* 主线模型 picker：右下角，向上展开 */
  .ia-picker-wrap {
    position: relative;
    display: inline-flex;
    min-width: 0;
  }
  .ia-access-btn-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: 112px;
  }
  .ia-access-popover .ia-picker-item-label {
    white-space: nowrap;
    word-break: keep-all;
  }
  .ia-workspace-wrap {
    max-width: 190px;
  }
  .ia-workspace-popover {
    right: auto;
    left: 0;
  }
  .ia-picker-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    max-width: 180px;
    min-width: 0;
    padding: 0 8px;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: 11px;
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .ia-picker-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
  }
  .ia-picker-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .ia-picker-btn.active {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    border-color: color-mix(in srgb, var(--primary) 42%, transparent);
    color: var(--primary);
  }
  .ia-picker-btn.configured {
    background: color-mix(in srgb, var(--primary) 18%, transparent);
    border-color: color-mix(in srgb, var(--primary) 55%, transparent);
    color: var(--primary);
  }
  .ia-picker-btn-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: 130px;
  }
  .ia-model-btn {
    max-width: 230px;
    background: color-mix(in srgb, var(--primary) 16%, transparent);
    border-color: color-mix(in srgb, var(--primary) 50%, transparent);
    color: color-mix(in srgb, var(--primary) 62%, white);
    gap: 6px;
  }
  .ia-model-btn .ia-picker-btn-label {
    max-width: 132px;
    color: color-mix(in srgb, var(--primary) 44%, white);
  }
  .ia-model-effort {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    height: 16px;
    padding: 0 6px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--primary) 28%, transparent);
    color: color-mix(in srgb, var(--primary) 54%, white);
    font-size: 10px;
    line-height: 16px;
    white-space: nowrap;
  }
  .ia-workspace-btn,
  .ia-branch-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    max-width: 180px;
    min-width: 0;
    padding: 0 8px;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: 11px;
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .ia-workspace-btn:hover:not(:disabled),
  .ia-branch-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
  }
  .ia-workspace-btn:disabled,
  .ia-branch-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .ia-workspace-btn.active,
  .ia-branch-btn.active {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    border-color: color-mix(in srgb, var(--primary) 42%, transparent);
    color: var(--primary);
  }
  .ia-workspace-btn.configured {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 36%, transparent);
    color: var(--primary);
  }
  .ia-workspace-btn.locked {
    cursor: default;
  }
  .ia-workspace-btn-label,
  .ia-branch-btn-label {
    flex: 0 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: 120px;
  }
  .ia-branch-diffstat {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .ia-branch-add { color: var(--success, #2ea043); }
  .ia-branch-del { color: var(--danger, #f85149); }
  /* 分支 picker 位于左下角，popover 锚定到左侧（覆盖模型 picker 的 right:0）。 */
  .ia-branch-popover {
    right: auto;
    left: 0;
  }
  .ia-picker-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    right: 0;
    z-index: 31;
    width: 280px;
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
  .ia-session-model-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    right: 0;
    z-index: 31;
    display: flex;
    flex-direction: column;
    width: min(280px, calc(100vw - 24px));
    max-height: 420px;
    padding: 8px;
    background: color-mix(in srgb, var(--background) 100%, white 8%);
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    border: 1px solid color-mix(in srgb, var(--border) 70%, var(--foreground) 30%);
    border-radius: var(--radius-md);
    box-shadow: 0 16px 44px rgba(0, 0, 0, 0.5), 0 2px 8px rgba(0, 0, 0, 0.22);
  }
  .ia-effort-section {
    flex: 0 0 auto;
  }
  .ia-effort-strip {
    display: flex;
    gap: 4px;
    padding: 2px 0 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .ia-effort-strip::-webkit-scrollbar {
    display: none;
  }
  .ia-effort-option {
    flex: 1 0 auto;
    min-width: 42px;
    height: 26px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0 6px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm, 6px);
    color: var(--foreground);
    font-size: 12px;
    cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast), color var(--transition-fast);
  }
  .ia-effort-option:hover:not(:disabled) {
    background: color-mix(in srgb, var(--primary) 10%, transparent);
  }
  .ia-effort-option.selected {
    background: color-mix(in srgb, var(--primary) 16%, transparent);
    border-color: color-mix(in srgb, var(--primary) 34%, transparent);
    color: var(--primary);
  }
  .ia-effort-option:disabled {
    cursor: wait;
    opacity: 0.72;
  }
  .ia-model-list-section {
    min-height: 0;
    overflow-y: auto;
  }
  .ia-model-list-section::-webkit-scrollbar {
    width: 8px;
  }
  .ia-model-list-section::-webkit-scrollbar-thumb {
    background: var(--border);
    border-radius: 8px;
  }
  .ia-section-header-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .ia-section-header-row .ia-picker-header {
    margin-bottom: 0;
    border-bottom: none;
  }
  .ia-picker-popover.ia-access-popover {
    box-sizing: border-box;
    width: max-content;
    min-width: 112px;
    max-width: 132px;
  }
  .ia-picker-header {
    font-size: 11px;
    color: var(--foreground-muted);
    padding: 2px 6px 6px;
    border-bottom: 1px dashed var(--border-subtle);
    margin-bottom: 4px;
  }
  .ia-picker-item {
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
  .ia-picker-item:hover {
    background: color-mix(in srgb, var(--primary) 10%, transparent);
  }
  .ia-picker-item:disabled {
    cursor: wait;
    opacity: 0.72;
  }
  .ia-picker-item.selected {
    background: color-mix(in srgb, var(--primary) 16%, transparent);
    color: var(--primary);
  }
  .ia-picker-item.ia-picker-row {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .ia-picker-item-label {
    font-size: 12px;
    font-weight: var(--font-medium, 500);
    word-break: break-all;
  }
  .ia-picker-row .ia-picker-item-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    word-break: normal;
    min-width: 0;
  }
  .ia-picker-check {
    flex: 0 0 auto;
    color: var(--primary);
    font-size: 13px;
    line-height: 1;
  }
  .ia-picker-divider {
    height: 1px;
    margin: 5px 6px;
    background: var(--border-subtle);
  }
  .ia-picker-item-desc {
    font-size: 11px;
    color: var(--foreground-muted);
    line-height: 1.4;
  }
  .ia-picker-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-top: 4px;
    margin-top: 4px;
    border-top: 1px dashed var(--border-subtle);
  }
  .ia-picker-status {
    font-size: 11px;
    color: var(--foreground-muted);
    padding: 8px 6px;
    text-align: center;
  }
  .ia-picker-status-error {
    color: var(--error);
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: center;
  }
  .ia-picker-retry {
    align-self: center;
    padding: 2px 10px;
    font-size: 11px;
    background: transparent;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .ia-picker-retry:hover {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
  }

  /* 斜杠技能选择：chip + 列表 + 预览 */
  .ia-skill-chip-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px var(--space-3) 0;
  }
  .ia-skill-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    padding: 3px 6px 3px 10px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--primary) 16%, transparent);
    border: 1px solid color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
    font-size: 12px;
    line-height: 1.2;
  }
  .ia-skill-chip-label {
    font-weight: var(--font-medium, 500);
    white-space: nowrap;
  }
  .ia-skill-chip-desc {
    color: color-mix(in srgb, var(--primary) 72%, var(--foreground-muted));
    font-size: 11px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ia-skill-chip-remove {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    padding: 0;
    border: none;
    border-radius: var(--radius-full);
    background: transparent;
    color: inherit;
    cursor: pointer;
    transition: background var(--transition-fast);
  }
  .ia-skill-chip-remove:hover {
    background: color-mix(in srgb, var(--primary) 24%, transparent);
  }

  .ia-slash-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 8px;
    z-index: 31;
    width: 240px;
    max-height: 320px;
    padding: 6px;
    background: color-mix(in srgb, var(--background) 100%, white 6%);
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    border: 1px solid color-mix(in srgb, var(--border) 80%, var(--foreground) 20%);
    border-radius: var(--radius-md);
    box-shadow: 0 14px 40px rgba(0, 0, 0, 0.5), 0 2px 8px rgba(0, 0, 0, 0.25);
  }
  .ia-slash-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: 308px;
    overflow-y: auto;
  }
  .ia-slash-item {
    display: flex;
    align-items: center;
    width: 100%;
    padding: 7px 10px;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm, 6px);
    cursor: pointer;
    text-align: left;
    color: var(--foreground);
    font-size: 13px;
    transition: background var(--transition-fast);
  }
  .ia-slash-item.active,
  .ia-slash-item:hover {
    background: color-mix(in srgb, var(--foreground) 10%, transparent);
    color: var(--foreground);
  }
  .ia-slash-item-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* 鼠标 hover / 键盘选择时跟随展示的描述卡片，跟随 .active 项的 offsetTop */
  .ia-slash-tooltip {
    position: absolute;
    left: calc(100% + 8px);
    max-width: 360px;
    min-width: 220px;
    padding: 10px 12px;
    background: color-mix(in srgb, var(--background) 100%, var(--foreground) 4%);
    color: var(--foreground);
    border: 1px solid color-mix(in srgb, var(--border) 80%, var(--foreground) 20%);
    border-radius: var(--radius-md);
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    box-shadow: 0 12px 32px color-mix(in srgb, var(--foreground) 18%, transparent);
    pointer-events: none;
  }
  .ia-slash-tooltip-loading {
    color: var(--foreground-muted);
    font-style: italic;
    min-width: 80px;
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

  @media (max-width: 640px) {
    .ia-container {
      padding: var(--space-2) 10px calc(var(--space-2) + env(safe-area-inset-bottom));
    }

    .ia-wrapper {
      max-height: min(46vh, 340px);
      border-radius: var(--radius-lg);
    }

    .ia-actions {
      flex-wrap: nowrap;
      align-items: center;
      justify-content: space-between;
      gap: 4px;
      padding: 4px 6px;
    }

    .ia-left {
      flex: 0 1 min(156px, 42vw);
      width: auto;
      min-width: 0;
    }

    .ia-left:empty {
      display: none;
    }

    .ia-right {
      flex: 1 1 auto;
      width: auto;
      min-width: 0;
      flex-wrap: nowrap;
      justify-content: flex-end;
      gap: 4px;
    }

    .ia-workspace-wrap,
    .ia-left > .ia-picker-wrap {
      flex: 1 1 0;
      min-width: 0;
    }

    .ia-workspace-btn,
    .ia-branch-btn {
      width: 100%;
      max-width: 100%;
    }

    .ia-workspace-btn-label,
    .ia-branch-btn-label {
      min-width: 34px;
      max-width: 100%;
    }

    .ia-access-wrap {
      flex: 0 1 auto;
      max-width: 86px;
    }

    .ia-model-wrap {
      flex: 0 1 auto;
      max-width: min(144px, 40vw);
    }

    .ia-access-wrap .ia-picker-btn,
    .ia-model-wrap .ia-picker-btn {
      width: auto;
      max-width: 100%;
    }

    .ia-access-btn-label {
      max-width: 100%;
    }

    .ia-picker-btn-label {
      max-width: 100%;
      min-width: 0;
    }

    .ia-enhance {
      flex: 0 0 24px;
    }

    .ia-send {
      flex: 0 0 28px;
    }

    .ia-picker-popover {
      width: min(280px, calc(100vw - 24px));
      max-height: min(360px, 56vh);
    }
  }

</style>
