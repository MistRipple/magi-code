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

  // 输入框可识别的 instruction skill。来源：bootstrap 中的 skillsConfig.instructionSkills，
  // 这一组才是 `/` 唤起的指令型技能，与 customTools（已注册到工具表）的语义不同。
  interface SkillOption {
    name: string;
    description: string;
  }

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
  const taskProjection = $derived(getTaskProjectionState(currentSessionId));

  const shouldInterruptTaskProjectionFromComposer = $derived.by(() => {
    const projection = taskProjection.projection;
    const sessionId = currentSessionId?.trim();
    const rootTaskId = projection?.root_task.task_id ?? taskProjection.rootTaskId;
    if (!projection || !sessionId || !rootTaskId) return false;
    return projection.runner_status === 'running';
  });
  const sessionInputLocked = $derived.by(() => (
    messagesState.sessionHydrating || !currentSessionId?.trim()
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
    sessionInputLocked || isInteractionBlocking
  ));
  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (inputValue.trim().length > 0) return true;
    // 执行中补充指令不支持图片，避免"有内容可发送"与实际能力不一致
    if (isSending) return false;
    return selectedImages.length > 0;
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
    closeSlashMenu();
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

  // 当 inputValue 由外部驱动（模板、技能选择、enhance 等）变化时，
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

  onMount(() => {
    function handleFillComposer(event: Event) {
      const text = (event as CustomEvent<{ text?: string }>).detail?.text;
      if (typeof text !== 'string' || !text.trim()) return;
      pendingCaretOffset = text.length;
      inputValue = text;
      queueMicrotask(focusEditor);
    }
    window.addEventListener('magi:fillComposer', handleFillComposer as EventListener);
    return () => window.removeEventListener('magi:fillComposer', handleFillComposer as EventListener);
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
      skillName: selectedSkill?.name ?? null,
      followUpMode: isSending ? 'queue' : undefined,
      images: selectedImages.map((img) => ({
        name: img.name,
        dataUrl: img.dataUrl,
      })),
    });
    clearComposerState();
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
          await client.interruptTask({ taskId: rootTaskId, sessionId });
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
    const items = event.clipboardData?.items;
    let hasImage = false;

    if (items) {
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
    }

    if (hasImage) {
      event.preventDefault();
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

  function toggleTemplates() {
    templatesOpen = !templatesOpen;
  }
  function applyTemplate(tpl: ResolvedTemplate) {
    const prompt = (tpl.prompt || '').trim();
    if (!prompt) {
      templatesOpen = false;
      return;
    }
    pendingCaretOffset = prompt.length;
    inputValue = prompt;
    templatesOpen = false;
    queueMicrotask(focusEditor);
  }

  // 通用清洗：模型偶尔会把改写结果包成 ```json ... ``` 或对象字面量，这里在前端兜底剥壳。
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
  async function enhancePromptHandler() {
    const draft = inputValue.trim();
    if (enhanceLoading || !draft) return;
    enhanceLoading = true;
    try {
      const result = await enhanceAgentPrompt(draft);
      const next = unwrapEnhancedPromptPayload(result?.enhancedPrompt ?? '');
      if (!next) {
        addToast('warning', result?.error || i18n.t('input.enhance.empty'));
        return;
      }
      inputValue = next;
      pendingCaretOffset = next.length;
      queueMicrotask(focusEditor);
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
            title={shouldInterruptTaskProjectionFromComposer ? '停止当前任务，保留进度' : i18n.t('input.stop')}
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

</style>
