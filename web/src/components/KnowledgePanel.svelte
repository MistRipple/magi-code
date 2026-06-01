<script lang="ts">
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import type { StandardMessage } from '../shared/protocol/message-protocol';
  import { MessageCategory } from '../shared/protocol/message-protocol';
  import { ensureArray } from '../lib/utils';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getState, messagesState } from '../stores/messages.svelte';
  import { openCodeTab } from '../stores/right-pane.svelte';
  import {
    addAgentKnowledgeItem,
    clearAgentProjectKnowledge,
    deleteAgentKnowledgeItem,
    getAgentProjectKnowledge,
    isWebAgentMode,
    updateAgentKnowledgeItem,
  } from '../web/agent-api';

  // 知识类型定义
  interface CodeIndex {
    files?: Array<{ path: string; lines?: number; size?: number }>;
    techStack?: string[];
    entryPoints?: string[];
  }

  type CodeIndexStatusValue = 'indexed' | 'empty' | 'failed';
  type CodeIndexReasonCode =
    | 'workspace_missing'
    | 'workspace_not_directory'
    | 'workspace_unreadable'
    | 'no_indexable_files';

  interface CodeIndexStatus {
    status: CodeIndexStatusValue;
    reasonCode?: CodeIndexReasonCode;
  }

  interface ADR {
    id: string;
    title: string;
    content?: string;
    tags?: string[];
    updatedAt?: number;
  }

  interface FAQ {
    id: string;
    question: string;
    answer?: string;
    tags?: string[];
    updatedAt?: number;
  }

  interface Learning {
    id: string;
    content: string;
    context?: string;
    createdAt?: number;
    updatedAt?: number;
    tags?: string[];
  }

  // 状态
  const appState = getState();
  const isWebMode = isWebAgentMode();
  const isKnowledgeActive = $derived(appState.currentTopTab === 'knowledge');
  const currentWorkspaceKey = $derived.by(() => {
    const workspaceId = typeof appState.currentWorkspaceId === 'string' ? appState.currentWorkspaceId.trim() : '';
    const workspacePath = typeof appState.currentWorkspacePath === 'string' ? appState.currentWorkspacePath.trim() : '';
    return `${workspaceId}::${workspacePath}`;
  });
  let currentTab = $state<'overview' | 'adr' | 'faq' | 'learning'>('overview');
  let isLoading = $state(false);
  let hasRequestedKnowledge = $state(false);
  let lastWorkspaceKey = $state('');
  let knowledgeWorkspaceGeneration = 0;
  let knowledgeLoadRequestSeq = 0;
  let loadError = $state('');
  let codeIndex = $state<CodeIndex | null>(null);
  let codeIndexStatus = $state<CodeIndexStatus | null>(null);
  let adrs = $state<ADR[]>([]);
  let faqs = $state<FAQ[]>([]);
  let learnings = $state<Learning[]>([]);
  let searchQuery = $state('');
  type EditableKnowledgeKind = 'adr' | 'faq' | 'learning';

  let expandedAdrId = $state<string | null>(null);
  let expandedFaqId = $state<string | null>(null);
  let expandedLearningId = $state<string | null>(null);
  let showClearConfirm = $state(false);
  let editorKind = $state<EditableKnowledgeKind | null>(null);
  let editorId = $state<string | null>(null);
  let formTitle = $state('');
  let formContent = $state('');
  let formContext = $state('');
  let formTags = $state('');
  let formError = $state('');
  let isSaving = $state(false);

  // 统计信息
  const normalizedCodeIndex = $derived(codeIndex
    ? {
        ...codeIndex,
        files: ensureArray(codeIndex.files) as NonNullable<CodeIndex['files']>,
        techStack: ensureArray(codeIndex.techStack) as string[],
        entryPoints: ensureArray(codeIndex.entryPoints) as string[]
      }
    : null
  );
  const fileCount = $derived(normalizedCodeIndex?.files.length || 0);
  const totalLines = $derived(
    normalizedCodeIndex?.files.reduce((sum, f) => sum + (Number(f.lines) || 0), 0) || 0
  );
  const hasKnowledgeContent = $derived(
    fileCount > 0 || adrs.length > 0 || faqs.length > 0 || learnings.length > 0
  );
  const codeIndexNotice = $derived.by(() => {
    if (!codeIndexStatus || codeIndexStatus.status === 'indexed') return null;
    const title = codeIndexStatus.status === 'failed'
      ? i18n.t('knowledge.overview.indexFailed')
      : i18n.t('knowledge.overview.indexEmpty');
    const hint = codeIndexNoticeHint(codeIndexStatus);
    return { title, hint, status: codeIndexStatus.status };
  });

  // 过滤后的 ADR 列表（安全过滤，跳过无效数据）
  const filteredAdrs = $derived.by(() => {
    let result = adrs.filter(adr => adr.title && typeof adr.title === 'string');
    if (searchQuery.trim()) {
      const query = searchQuery.toLowerCase();
      result = result.filter(adr =>
        adr.title.toLowerCase().includes(query) ||
        adr.content?.toLowerCase().includes(query) ||
        adr.tags?.some(t => t.toLowerCase().includes(query))
      );
    }
    return result;
  });

  // 过滤后的 FAQ 列表（安全过滤，跳过无效数据）
  const filteredFaqs = $derived.by(() => {
    let result = faqs.filter(faq => faq.question && typeof faq.question === 'string');
    if (!searchQuery.trim()) return result;
    const query = searchQuery.toLowerCase();
    return result.filter(faq =>
      faq.question.toLowerCase().includes(query) ||
      faq.answer?.toLowerCase().includes(query) ||
      faq.tags?.some(t => t.toLowerCase().includes(query))
    );
  });

  // 过滤后的 Learning 列表
  const filteredLearnings = $derived.by(() => {
    let result = learnings.filter(l => l.content && typeof l.content === 'string');
    if (!searchQuery.trim()) return result;
    const query = searchQuery.toLowerCase();
    return result.filter(l =>
      l.content.toLowerCase().includes(query) ||
      l.context?.toLowerCase().includes(query) ||
      l.tags?.some(t => t.toLowerCase().includes(query))
    );
  });

  function switchTab(tabId: typeof currentTab) {
    currentTab = tabId;
    expandedAdrId = null;
    expandedFaqId = null;
    expandedLearningId = null;
    closeEditor();
  }

  /**
   * 知识库入口文件点击 → 推到右侧 RightPane 的 code tab。
   * 这里只携带 filepath；RightPane 会异步拉取文件源码进行渲染，
   * 文件为空时显示空态 + "在编辑器中打开" 按钮。
   */
  function previewEntryPoint(filepath: string) {
    if (!filepath) return;
    openCodeTab(messagesState.currentSessionId, filepath, {
      sessionId: messagesState.currentSessionId ?? undefined,
      workspaceId: messagesState.currentWorkspaceId ?? undefined,
      workspacePath: messagesState.currentWorkspacePath || undefined,
    });
  }

  function splitTags(value: string): string[] {
    const tags: string[] = [];
    for (const raw of value.split(',')) {
      const tag = raw.trim();
      if (tag && !tags.includes(tag)) {
        tags.push(tag);
      }
      if (tags.length >= 8) break;
    }
    return tags;
  }

  function joinTags(tags?: string[]): string {
    return ensureArray(tags).join(', ');
  }

  function parseKnowledgeTimestamp(value: unknown): number | undefined {
    if (typeof value === 'number' && Number.isFinite(value)) return value;
    if (typeof value === 'string' && value.trim()) {
      const parsed = Number(value);
      if (Number.isFinite(parsed)) return parsed;
    }
    return undefined;
  }

  function formatKnowledgeDate(value: number | undefined): string {
    if (!value) return '';
    const date = new Date(value);
    if (!Number.isFinite(date.getTime())) return '';
    const currentYear = new Date().getFullYear();
    const options: Intl.DateTimeFormatOptions = date.getFullYear() === currentYear
      ? { month: 'numeric', day: 'numeric' }
      : { year: 'numeric', month: 'numeric', day: 'numeric' };
    return new Intl.DateTimeFormat(i18n.locale, options).format(date);
  }

  function isGeneratedSourceRef(value: string | undefined): boolean {
    const normalized = value?.trim() ?? '';
    return normalized.startsWith('session:')
      || normalized.startsWith('session-')
      || normalized.startsWith('task:')
      || normalized.startsWith('task-');
  }

  function formatKnowledgeSource(value: string | undefined): string {
    const normalized = value?.trim() ?? '';
    if (!normalized) return '';
    if (normalized.startsWith('session:') || normalized.startsWith('session-')) {
      return i18n.t('knowledge.source.autoSession');
    }
    if (normalized.startsWith('task:') || normalized.startsWith('task-')) {
      return i18n.t('knowledge.source.autoTask');
    }
    return normalized;
  }

  function visibleTags(tags?: string[]): string[] {
    const hidden = new Set(['auto', 'learning', 'adr', 'faq', 'codeindex']);
    return ensureArray(tags)
      .map((tag) => String(tag).trim())
      .filter((tag) => tag && !hidden.has(tag.toLowerCase()));
  }

  function formatLearningTitle(content: string): string {
    const normalized = content.trim();
    return normalized.length > 80 ? `${normalized.slice(0, 80)}...` : normalized;
  }

  function closeEditor() {
    editorKind = null;
    editorId = null;
    formTitle = '';
    formContent = '';
    formContext = '';
    formTags = '';
    formError = '';
    isSaving = false;
  }

  function openCreateEditor(kind: EditableKnowledgeKind) {
    editorKind = kind;
    editorId = null;
    formTitle = '';
    formContent = '';
    formContext = '';
    formTags = '';
    formError = '';
  }

  function editAdr(adr: ADR, e: Event) {
    e.stopPropagation();
    editorKind = 'adr';
    editorId = adr.id;
    formTitle = adr.title || '';
    formContent = adr.content || '';
    formContext = '';
    formTags = joinTags(visibleTags(adr.tags));
    formError = '';
  }

  function editFaq(faq: FAQ, e: Event) {
    e.stopPropagation();
    editorKind = 'faq';
    editorId = faq.id;
    formTitle = faq.question || '';
    formContent = faq.answer || '';
    formContext = '';
    formTags = joinTags(visibleTags(faq.tags));
    formError = '';
  }

  function editLearning(learning: Learning, e: Event) {
    e.stopPropagation();
    editorKind = 'learning';
    editorId = learning.id;
    formTitle = '';
    formContent = learning.content || '';
    formContext = isGeneratedSourceRef(learning.context) ? '' : learning.context || '';
    formTags = joinTags(visibleTags(learning.tags));
    formError = '';
  }

  function editorTitle(): string {
    if (editorKind === 'adr') return editorId ? i18n.t('knowledge.adr.editTitle') : i18n.t('knowledge.adr.addTitle');
    if (editorKind === 'faq') return editorId ? i18n.t('knowledge.faq.editTitle') : i18n.t('knowledge.faq.addTitle');
    if (editorKind === 'learning') return editorId ? i18n.t('knowledge.learning.editTitle') : i18n.t('knowledge.learning.addTitle');
    return '';
  }

  async function saveEditor() {
    if (!editorKind || isSaving) return;
    const request = currentKnowledgeWorkspaceRequest();
    const title = formTitle.trim();
    const content = formContent.trim();
    const context = formContext.trim();
    const tags = splitTags(formTags);

    if ((editorKind === 'adr' || editorKind === 'faq') && !title) {
      formError = i18n.t('knowledge.form.titleRequired');
      return;
    }
    if (!content) {
      formError = i18n.t('knowledge.form.contentRequired');
      return;
    }
    if (editorKind === 'learning' && content.length < 12) {
      formError = i18n.t('knowledge.form.learningTooShort');
      return;
    }

    isSaving = true;
    formError = '';
    try {
      const kind = editorKind;
      if (isWebMode) {
        if (editorId) {
          const patch: { title?: string; content?: string; tags?: string[]; context?: string } = {
            content,
            tags,
          };
          if (kind === 'adr' || kind === 'faq') {
            patch.title = title;
          }
          if (kind === 'learning') {
            patch.context = context;
          }
          await updateAgentKnowledgeItem(editorId, patch);
        } else {
          await addAgentKnowledgeItem({
            kind,
            title: kind === 'learning' ? undefined : title,
            content,
            tags,
            context: kind === 'learning' ? context : undefined,
          });
        }
      } else {
        if (editorId) {
          const updatePayload: { type: string; [key: string]: unknown } = {
            type: 'updateKnowledgeItem',
            knowledgeId: editorId,
            content,
            tags,
          };
          if (kind === 'adr' || kind === 'faq') updatePayload.title = title;
          if (kind === 'learning') updatePayload.context = context;
          vscode.postMessage(updatePayload);
        } else {
          const addPayload: { type: string; [key: string]: unknown } = {
            type: 'addKnowledgeItem',
            kind,
            content,
            tags,
          };
          if (kind === 'adr' || kind === 'faq') addPayload.title = title;
          if (kind === 'learning') addPayload.context = context;
          vscode.postMessage(addPayload);
        }
      }
      if (!isCurrentKnowledgeWorkspaceRequest(request)) {
        return;
      }
      closeEditor();
      refresh();
    } catch (error) {
      if (!isCurrentKnowledgeWorkspaceRequest(request)) {
        return;
      }
      console.warn('[KnowledgePanel] save knowledge item failed:', error);
      formError = i18n.t('knowledge.form.saveFailed');
    } finally {
      if (isCurrentKnowledgeWorkspaceRequest(request)) {
        isSaving = false;
      }
    }
  }

  function clearKnowledgeContent() {
    codeIndex = null;
    codeIndexStatus = null;
    adrs = [];
    faqs = [];
    learnings = [];
    searchQuery = '';
    expandedAdrId = null;
    expandedFaqId = null;
    expandedLearningId = null;
    showClearConfirm = false;
    closeEditor();
  }

  function applyKnowledgePayload(payload: Record<string, unknown> | null | undefined): boolean {
    if (!payloadMatchesCurrentWorkspace(payload)) {
      return false;
    }
    const codeIndexPayload = payload?.codeIndex && typeof payload.codeIndex === 'object'
      ? payload.codeIndex as Record<string, unknown>
      : null;
    codeIndexStatus = normalizeCodeIndexStatus(payload?.codeIndexStatus);

    const items = ensureArray(payload?.items) as Array<Record<string, unknown>>;
    adrs = items
      .filter((item) => item.kind === 'adr')
      .map((item) => ({
        id: String(item.id ?? ''),
        title: typeof item.title === 'string' ? item.title : '',
        content: typeof item.content === 'string' ? item.content : '',
        tags: ensureArray(item.tags) as string[],
        updatedAt: parseKnowledgeTimestamp(item.updatedAt),
      }));
    faqs = items
      .filter((item) => item.kind === 'faq')
      .map((item) => ({
        id: String(item.id ?? ''),
        question: typeof item.title === 'string' ? item.title : '',
        answer: typeof item.content === 'string' ? item.content : '',
        tags: ensureArray(item.tags) as string[],
        updatedAt: parseKnowledgeTimestamp(item.updatedAt),
      }));
    learnings = items
      .filter((item) => item.kind === 'learning')
      .map((item) => ({
        id: String(item.id ?? ''),
        content: typeof item.content === 'string' ? item.content : '',
        context: typeof item.context === 'string' ? item.context : undefined,
        createdAt: parseKnowledgeTimestamp(item.createdAt),
        updatedAt: parseKnowledgeTimestamp(item.updatedAt),
        tags: ensureArray(item.tags) as string[],
      }));
    codeIndex = codeIndexPayload
      ? {
          ...codeIndexPayload,
          files: ensureArray(codeIndexPayload.files) as NonNullable<CodeIndex['files']>,
          techStack: ensureArray(codeIndexPayload.techStack) as string[],
          entryPoints: ensureArray(codeIndexPayload.entryPoints) as string[]
        }
      : null;
    loadError = '';
    isLoading = false;
    return true;
  }

  function normalizeCodeIndexStatus(value: unknown): CodeIndexStatus | null {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const record = value as Record<string, unknown>;
    const status = typeof record.status === 'string' ? record.status : '';
    if (status !== 'indexed' && status !== 'empty' && status !== 'failed') return null;
    const reasonCode = normalizeCodeIndexReasonCode(record.reasonCode);
    return { status, reasonCode };
  }

  function normalizeCodeIndexReasonCode(value: unknown): CodeIndexReasonCode | undefined {
    if (typeof value !== 'string') return undefined;
    if (
      value === 'workspace_missing' ||
      value === 'workspace_not_directory' ||
      value === 'workspace_unreadable' ||
      value === 'no_indexable_files'
    ) {
      return value;
    }
    return undefined;
  }

  function codeIndexNoticeHint(status: CodeIndexStatus): string {
    if (status.reasonCode === 'no_indexable_files') {
      return i18n.t('knowledge.overview.indexEmptyNoIndexableFiles');
    }
    if (status.reasonCode === 'workspace_missing') {
      return i18n.t('knowledge.overview.indexFailedWorkspaceMissing');
    }
    if (status.reasonCode === 'workspace_not_directory') {
      return i18n.t('knowledge.overview.indexFailedWorkspaceNotDirectory');
    }
    if (status.reasonCode === 'workspace_unreadable') {
      return i18n.t('knowledge.overview.indexFailedWorkspaceUnreadable');
    }
    return status.status === 'failed'
      ? i18n.t('knowledge.overview.indexFailedHint')
      : i18n.t('knowledge.overview.indexEmptyHint');
  }

  interface KnowledgeWorkspaceRequest {
    workspaceKey: string;
    workspaceGeneration: number;
  }

  interface KnowledgeLoadRequest extends KnowledgeWorkspaceRequest {
    loadSeq: number;
  }

  function currentKnowledgeWorkspaceRequest(): KnowledgeWorkspaceRequest {
    return {
      workspaceKey: currentWorkspaceKey,
      workspaceGeneration: knowledgeWorkspaceGeneration,
    };
  }

  function nextKnowledgeLoadRequest(): KnowledgeLoadRequest {
    return {
      ...currentKnowledgeWorkspaceRequest(),
      loadSeq: ++knowledgeLoadRequestSeq,
    };
  }

  function isCurrentKnowledgeWorkspaceRequest(request: KnowledgeWorkspaceRequest): boolean {
    return request.workspaceGeneration === knowledgeWorkspaceGeneration
      && request.workspaceKey === currentWorkspaceKey;
  }

  function isCurrentKnowledgeLoadRequest(request: KnowledgeLoadRequest): boolean {
    return request.loadSeq === knowledgeLoadRequestSeq && isCurrentKnowledgeWorkspaceRequest(request);
  }

  function handleKnowledgeLoadError(error: unknown, request: KnowledgeLoadRequest) {
    if (!isCurrentKnowledgeLoadRequest(request)) {
      return;
    }
    clearKnowledgeContent();
    console.warn('[KnowledgePanel] load project knowledge failed:', error);
    loadError = i18n.t('knowledge.toast.loadFailed');
    isLoading = false;
  }

  function handleKnowledgeMutationError(error: unknown, messageKey: string, request: KnowledgeWorkspaceRequest) {
    if (!isCurrentKnowledgeWorkspaceRequest(request)) {
      return;
    }
    console.warn('[KnowledgePanel] project knowledge mutation failed:', error);
    loadError = i18n.t(messageKey);
    isLoading = false;
  }

  function resolvePayloadWorkspaceKey(payload: Record<string, unknown> | null | undefined): string {
    const workspaceId = typeof payload?.workspaceId === 'string' ? payload.workspaceId.trim() : '';
    const workspacePath = typeof payload?.workspacePath === 'string' ? payload.workspacePath.trim() : '';
    return workspaceId || workspacePath ? `${workspaceId}::${workspacePath}` : '';
  }

  function payloadMatchesCurrentWorkspace(payload: Record<string, unknown> | null | undefined): boolean {
    return resolvePayloadWorkspaceKey(payload) === currentWorkspaceKey;
  }

  function refreshSettingsAfterKnowledgePayload() {
    vscode.postMessage({ type: 'loadSettingsBootstrap', force: true });
  }

  async function fetchKnowledgeViaApi(request: KnowledgeLoadRequest) {
    const res = await getAgentProjectKnowledge();
    if (!isCurrentKnowledgeLoadRequest(request)) {
      return;
    }
    if (!applyKnowledgePayload(res)) {
      isLoading = false;
      return;
    }
    refreshSettingsAfterKnowledgePayload();
  }

  function requestKnowledgeLoad() {
    const request = nextKnowledgeLoadRequest();
    hasRequestedKnowledge = true;
    isLoading = true;
    loadError = '';
    if (isWebMode) {
      fetchKnowledgeViaApi(request)
        .catch((error) => handleKnowledgeLoadError(error, request));
    } else {
      vscode.postMessage({ type: 'getProjectKnowledge' });
    }
  }

  function refresh() {
    requestKnowledgeLoad();
  }

  function toggleAdr(adr: ADR) {
    expandedAdrId = expandedAdrId === adr.id ? null : adr.id;
  }

  function toggleFaq(faq: FAQ) {
    expandedFaqId = expandedFaqId === faq.id ? null : faq.id;
  }

  function deleteAdr(id: string, e: Event) {
    e.stopPropagation();
    deleteKnowledgeEntry(id);
  }

  function deleteFaq(id: string, e: Event) {
    e.stopPropagation();
    deleteKnowledgeEntry(id);
  }

  function toggleLearning(learning: Learning) {
    expandedLearningId = expandedLearningId === learning.id ? null : learning.id;
  }

  function deleteLearning(id: string, e: Event) {
    e.stopPropagation();
    deleteKnowledgeEntry(id);
  }

  function deleteKnowledgeEntry(id: string) {
    const request = currentKnowledgeWorkspaceRequest();
    if (isWebMode) {
      deleteAgentKnowledgeItem(id)
        .then(() => {
          if (isCurrentKnowledgeWorkspaceRequest(request)) {
            refresh();
          }
        })
        .catch((error) => handleKnowledgeMutationError(error, 'knowledge.toast.deleteFailed', request));
    } else {
      vscode.postMessage({ type: 'deleteKnowledgeItem', knowledgeId: id });
    }
  }

  function confirmClear() {
    showClearConfirm = true;
  }

  function cancelClear() {
    showClearConfirm = false;
  }

  function executeClear() {
    const request = currentKnowledgeWorkspaceRequest();
    showClearConfirm = false;
    isLoading = true;
    if (isWebMode) {
      clearAgentProjectKnowledge()
        .then(() => {
          if (isCurrentKnowledgeWorkspaceRequest(request)) {
            refresh();
          }
        })
        .catch((error) => handleKnowledgeMutationError(error, 'knowledge.toast.clearFailed', request));
    } else {
      vscode.postMessage({ type: 'clearProjectKnowledge' });
    }
  }

  $effect(() => {
    const nextWorkspaceKey = currentWorkspaceKey;
    if (lastWorkspaceKey === nextWorkspaceKey) {
      return;
    }
    lastWorkspaceKey = nextWorkspaceKey;
    knowledgeWorkspaceGeneration += 1;
    knowledgeLoadRequestSeq += 1;
    hasRequestedKnowledge = false;
    isLoading = false;
    loadError = '';
    clearKnowledgeContent();
    if (isKnowledgeActive) {
      requestKnowledgeLoad();
    }
  });

  $effect(() => {
    if (!isKnowledgeActive || hasRequestedKnowledge) {
      return;
    }
    requestKnowledgeLoad();
  });

  // 监听来自扩展的消息
  onMount(() => {
    const unsubscribe = vscode.onMessage((msg) => {
      if (msg.type !== 'unifiedMessage') return;
      const standard = msg.message as StandardMessage;
      if (!standard || standard.category !== MessageCategory.DATA || !standard.data) return;
      if (standard.data.dataType !== 'projectKnowledgeLoaded') return;

      const payload = standard.data.payload as Record<string, unknown>;
      if (applyKnowledgePayload(payload)) {
        refreshSettingsAfterKnowledgePayload();
      }
    });

    return () => unsubscribe();
  });
</script>

<div class="panel-content-scrollable knowledge-panel">
  <!-- 头部：Tab 栏 -->
  <div class="kp-tabs-bar">
    <button class="kp-tab" class:active={currentTab === 'overview'} onclick={() => switchTab('overview')}>
      <Icon name="stats" size={13} />
      <span>{i18n.t('knowledge.tabs.overview')}</span>
    </button>
    <button class="kp-tab" class:active={currentTab === 'adr'} onclick={() => switchTab('adr')}>
      <Icon name="document" size={13} />
      <span>{i18n.t('knowledge.tabs.adr')}</span>
      {#if adrs.length > 0}
        <span class="kp-tab-count">{adrs.length}</span>
      {/if}
    </button>
    <button class="kp-tab" class:active={currentTab === 'faq'} onclick={() => switchTab('faq')}>
      <Icon name="question" size={13} />
      <span>{i18n.t('knowledge.tabs.faq')}</span>
      {#if faqs.length > 0}
        <span class="kp-tab-count">{faqs.length}</span>
      {/if}
    </button>
    <button class="kp-tab" class:active={currentTab === 'learning'} onclick={() => switchTab('learning')}>
      <Icon name="lightbulb" size={13} />
      <span>{i18n.t('knowledge.tabs.learning')}</span>
      {#if learnings.length > 0}
        <span class="kp-tab-count">{learnings.length}</span>
      {/if}
    </button>
    <div class="kp-tab-actions">
      <button class="kp-icon-btn" onclick={refresh} disabled={isLoading} title={i18n.t('knowledge.actions.refreshTitle')}>
        <Icon name="refresh" size={14} class={isLoading ? 'spinning' : ''} />
      </button>
      <button
        class="kp-icon-btn kp-icon-btn--danger"
        onclick={confirmClear}
        disabled={isLoading || !hasKnowledgeContent}
        title={i18n.t('knowledge.actions.clearTitle')}
      >
        <Icon name="delete" size={14} />
      </button>
    </div>
  </div>

  <!-- 搜索栏（Tab 下方独立行） -->
  {#if currentTab !== 'overview'}
    <div class="kp-search-bar">
      <Icon name="search" size={13} />
      <input
        type="text"
        class="kp-search-input"
        placeholder={currentTab === 'adr' ? i18n.t('knowledge.search.adrPlaceholder') : currentTab === 'faq' ? i18n.t('knowledge.search.faqPlaceholder') : i18n.t('knowledge.search.learningPlaceholder')}
        bind:value={searchQuery}
      />
      {#if searchQuery}
        <button class="kp-search-clear" onclick={() => searchQuery = ''}>
          <Icon name="close" size={12} />
        </button>
      {/if}
      <button class="kp-add-btn" onclick={() => openCreateEditor(currentTab as EditableKnowledgeKind)}>
        <Icon name="plus" size={12} />
        <span>{i18n.t('knowledge.actions.add')}</span>
      </button>
    </div>
  {/if}

  {#if editorKind}
    <div class="kp-editor-card">
      <div class="kp-editor-title">{editorTitle()}</div>
      {#if editorKind !== 'learning'}
        <label class="kp-editor-field">
          <span>{editorKind === 'faq' ? i18n.t('knowledge.form.question') : i18n.t('knowledge.form.title')}</span>
          <input class="kp-editor-input" bind:value={formTitle} />
        </label>
      {/if}
      <label class="kp-editor-field">
        <span>{editorKind === 'faq' ? i18n.t('knowledge.form.answer') : i18n.t('knowledge.form.content')}</span>
        <textarea class="kp-editor-textarea" bind:value={formContent} rows="4"></textarea>
      </label>
      {#if editorKind === 'learning'}
        <label class="kp-editor-field">
          <span>{i18n.t('knowledge.form.context')}</span>
          <input class="kp-editor-input" bind:value={formContext} />
        </label>
      {/if}
      <label class="kp-editor-field">
        <span>{i18n.t('knowledge.form.tags')}</span>
        <input class="kp-editor-input" bind:value={formTags} placeholder={i18n.t('knowledge.form.tagsPlaceholder')} />
      </label>
      {#if formError}
        <div class="kp-editor-error">{formError}</div>
      {/if}
      <div class="kp-editor-actions">
        <button class="kp-editor-btn" onclick={closeEditor} disabled={isSaving}>{i18n.t('knowledge.actions.cancel')}</button>
        <button class="kp-editor-btn kp-editor-btn--primary" onclick={saveEditor} disabled={isSaving}>{i18n.t('knowledge.actions.save')}</button>
      </div>
    </div>
  {/if}

  {#if loadError}
    <div class="kp-load-error">
      <Icon name="warning" size={14} />
      <span>{loadError}</span>
    </div>
  {/if}

  <!-- 清空确认弹窗 -->
  {#if showClearConfirm}
    <div class="kp-confirm-overlay" role="dialog">
      <div class="kp-confirm-dialog">
        <div class="kp-confirm-icon">
          <Icon name="warning" size={24} />
        </div>
        <div class="kp-confirm-title">{i18n.t('knowledge.confirm.title')}</div>
        <p class="kp-confirm-desc">
          {i18n.t('knowledge.confirm.desc', { fileCount, adrCount: adrs.length, faqCount: faqs.length, learningCount: learnings.length })}
        </p>
        <div class="kp-confirm-actions">
          <button class="kp-confirm-btn kp-confirm-btn--cancel" onclick={cancelClear}>{i18n.t('knowledge.confirm.cancel')}</button>
          <button class="kp-confirm-btn kp-confirm-btn--danger" onclick={executeClear}>{i18n.t('knowledge.confirm.confirm')}</button>
        </div>
      </div>
    </div>
  {/if}

  <!-- 主内容区 -->
  <div class="kp-content">
    {#if isLoading}
      <div class="kp-loading">
        <div class="kp-spinner"></div>
        <span>{i18n.t('knowledge.loading')}</span>
      </div>
    {:else if currentTab === 'overview'}
      <!-- 概览 -->
      <div class="kp-overview">
        <!-- 紧凑统计条 -->
        <div class="kp-stats-row">
          <div class="kp-stat">
            <span class="kp-stat-value">{fileCount.toLocaleString()}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.files')}</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{totalLines.toLocaleString()}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.lines')}</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{adrs.length}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.adr')}</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{faqs.length}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.faq')}</span>
          </div>
          <div class="kp-stat-divider"></div>
          <div class="kp-stat">
            <span class="kp-stat-value">{learnings.length}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.tabs.learning')}</span>
          </div>
        </div>

        {#if codeIndexNotice}
          <div class="kp-index-notice" class:failed={codeIndexNotice.status === 'failed'}>
            <Icon name={codeIndexNotice.status === 'failed' ? 'warning' : 'info'} size={14} />
            <div class="kp-index-notice-text">
              <span class="kp-index-notice-title">{codeIndexNotice.title}</span>
              <span class="kp-index-notice-hint">{codeIndexNotice.hint}</span>
            </div>
          </div>
        {/if}

        {#if normalizedCodeIndex?.techStack && normalizedCodeIndex.techStack.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="code" size={13} />
              <span>{i18n.t('knowledge.overview.techStack')}</span>
            </h4>
            <div class="kp-tech-grid">
              {#each normalizedCodeIndex.techStack as tech}
                <span class="kp-tech-badge">{tech}</span>
              {/each}
            </div>
          </div>
        {/if}

        {#if normalizedCodeIndex?.entryPoints && normalizedCodeIndex.entryPoints.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="target" size={13} />
              <span>{i18n.t('knowledge.overview.entryPoints')}</span>
            </h4>
            <div class="kp-entry-list">
              {#each normalizedCodeIndex.entryPoints as entry}
                <button
                  type="button"
                  class="kp-entry-item kp-entry-item--clickable"
                  onclick={() => previewEntryPoint(entry)}
                  title={i18n.t('rightPane.previewFile', { path: entry })}
                >
                  <Icon name="file-text" size={12} />
                  <span>{entry}</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}

        <!-- 最近的 ADR 预览 -->
        {#if adrs.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="document" size={13} />
              <span>{i18n.t('knowledge.overview.recentDecisions')}</span>
              <button class="kp-section-link" onclick={() => switchTab('adr')}>{i18n.t('knowledge.overview.viewAll')}</button>
            </h4>
            {#each adrs.slice(0, 3) as adr (adr.id)}
              {@const adrDate = formatKnowledgeDate(adr.updatedAt)}
              <div class="kp-preview-item">
                <span class="kp-preview-dot default"></span>
                <span class="kp-preview-text">{adr.title}</span>
                {#if adrDate}
                  <span class="kp-preview-date">{adrDate}</span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        {#if learnings.length > 0}
          <div class="kp-section">
            <h4 class="kp-section-title">
              <Icon name="lightbulb" size={13} />
              <span>{i18n.t('knowledge.overview.recentLearnings')}</span>
              <button class="kp-section-link" onclick={() => switchTab('learning')}>{i18n.t('knowledge.overview.viewAll')}</button>
            </h4>
            {#each learnings.slice(0, 3) as learning (learning.id)}
              {@const learningDate = formatKnowledgeDate(learning.updatedAt ?? learning.createdAt)}
              <div class="kp-preview-item">
                <span class="kp-preview-dot learning"></span>
                <span class="kp-preview-text">{formatLearningTitle(learning.content)}</span>
                {#if learningDate}
                  <span class="kp-preview-date">{learningDate}</span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </div>

    {:else if currentTab === 'adr'}
      <!-- ADR Tab -->
      <div class="kp-list">
        {#if filteredAdrs.length === 0}
          <div class="kp-empty">
            <Icon name="document" size={28} />
            <div class="kp-empty-title">{i18n.t('knowledge.adr.emptyTitle')}</div>
            <div class="kp-empty-hint">{i18n.t('knowledge.adr.emptyHint')}</div>
          </div>
        {:else}
          {#each filteredAdrs as adr (adr.id)}
            {@const isExpanded = expandedAdrId === adr.id}
            {@const adrDate = formatKnowledgeDate(adr.updatedAt)}
            {@const adrTags = visibleTags(adr.tags)}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header" role="button" tabindex="0" onclick={() => toggleAdr(adr)} onkeydown={(e) => e.key === 'Enter' && toggleAdr(adr)}>
                <span class="kp-card-indicator default"></span>
                <div class="kp-card-main">
                  <span class="kp-card-title">{adr.title}</span>
                  {#if !isExpanded && adr.content}
                    <p class="kp-card-preview">{adr.content}</p>
                  {/if}
                </div>
                <div class="kp-card-meta">
                  <button class="kp-card-action" title={i18n.t('knowledge.actions.edit')} onclick={(e) => editAdr(adr, e)}>
                    <Icon name="edit" size={12} />
                  </button>
                  <button class="kp-card-delete" title={i18n.t('knowledge.adr.deleteTitle')} onclick={(e) => deleteAdr(adr.id, e)}>
                    <Icon name="trash" size={12} />
                  </button>
                  {#if adrDate}
                    <span class="kp-category-badge">{adrDate}</span>
                  {/if}
                  <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
                </div>
              </div>
              {#if isExpanded}
                <div class="kp-card-body">
                  {#if adr.content}
                    <div class="kp-detail-block">
                      <p>{adr.content}</p>
                    </div>
                  {/if}
                  {#if adrTags.length > 0}
                    <div class="kp-tag-list">
                      {#each adrTags as tag}
                        <span class="kp-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

    {:else if currentTab === 'faq'}
      <!-- FAQ Tab -->
      <div class="kp-list">
        {#if filteredFaqs.length === 0}
          <div class="kp-empty">
            <Icon name="question" size={28} />
            <div class="kp-empty-title">{i18n.t('knowledge.faq.emptyTitle')}</div>
            <div class="kp-empty-hint">{i18n.t('knowledge.faq.emptyHint')}</div>
          </div>
        {:else}
          {#each filteredFaqs as faq (faq.id)}
            {@const isExpanded = expandedFaqId === faq.id}
            {@const faqDate = formatKnowledgeDate(faq.updatedAt)}
            {@const faqTags = visibleTags(faq.tags)}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header" role="button" tabindex="0" onclick={() => toggleFaq(faq)} onkeydown={(e) => e.key === 'Enter' && toggleFaq(faq)}>
                <span class="kp-card-indicator faq"></span>
                <div class="kp-card-main">
                  <span class="kp-card-title">{faq.question}</span>
                  {#if !isExpanded && faq.answer}
                    <p class="kp-card-preview">{faq.answer}</p>
                  {/if}
                </div>
                <div class="kp-card-meta">
                  <button class="kp-card-action" title={i18n.t('knowledge.actions.edit')} onclick={(e) => editFaq(faq, e)}>
                    <Icon name="edit" size={12} />
                  </button>
                  <button class="kp-card-delete" title={i18n.t('knowledge.faq.deleteTitle')} onclick={(e) => deleteFaq(faq.id, e)}>
                    <Icon name="trash" size={12} />
                  </button>
                  {#if faqDate}
                    <span class="kp-category-badge">{faqDate}</span>
                  {/if}
                  <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
                </div>
              </div>
              {#if isExpanded}
                <div class="kp-card-body">
                  {#if faq.answer}
                    <div class="kp-detail-block">
                      <p>{faq.answer}</p>
                    </div>
                  {/if}
                  {#if faqTags.length > 0}
                    <div class="kp-tag-list">
                      {#each faqTags as tag}
                        <span class="kp-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>

    {:else if currentTab === 'learning'}
      <!-- Learning Tab -->
      <div class="kp-list">
        {#if filteredLearnings.length === 0}
          <div class="kp-empty">
            <Icon name="lightbulb" size={28} />
            <div class="kp-empty-title">{i18n.t('knowledge.learning.emptyTitle')}</div>
            <div class="kp-empty-hint">{i18n.t('knowledge.learning.emptyHint')}</div>
          </div>
        {:else}
          {#each filteredLearnings as learning (learning.id)}
            {@const isExpanded = expandedLearningId === learning.id}
            {@const learningSource = formatKnowledgeSource(learning.context)}
            {@const learningDate = formatKnowledgeDate(learning.updatedAt ?? learning.createdAt)}
            {@const learningTags = visibleTags(learning.tags)}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header" role="button" tabindex="0" onclick={() => toggleLearning(learning)} onkeydown={(e) => e.key === 'Enter' && toggleLearning(learning)}>
                <span class="kp-card-indicator learning"></span>
                <div class="kp-card-main">
                  <span class="kp-card-title">{formatLearningTitle(learning.content)}</span>
                  {#if !isExpanded && learningSource}
                    <p class="kp-card-preview">{learningSource}</p>
                  {/if}
                </div>
                <div class="kp-card-meta">
                  <button class="kp-card-action" title={i18n.t('knowledge.actions.edit')} onclick={(e) => editLearning(learning, e)}>
                    <Icon name="edit" size={12} />
                  </button>
                  <button class="kp-card-delete" title={i18n.t('knowledge.learning.deleteTitle')} onclick={(e) => deleteLearning(learning.id, e)}>
                    <Icon name="trash" size={12} />
                  </button>
                  {#if learningDate}
                    <span class="kp-category-badge">{learningDate}</span>
                  {/if}
                  <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
                </div>
              </div>
              {#if isExpanded}
                <div class="kp-card-body">
                  <div class="kp-detail-block">
                    <p>{learning.content}</p>
                  </div>
                  {#if learningSource}
                    <div class="kp-detail-block">
                      <h5>{i18n.t('knowledge.source.label')}</h5>
                      <p>{learningSource}</p>
                    </div>
                  {/if}
                  {#if learningTags.length > 0}
                    <div class="kp-tag-list">
                      {#each learningTags as tag}
                        <span class="kp-tag">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  /* ============================================
     KnowledgePanel - 知识库面板
     设计参考: Linear/Notion 知识管理界面
     适配 VSCode 侧边栏窄面板约束
     ============================================ */

  .knowledge-panel {
    /* panel-content-scrollable 已经包含了 padding, flex, overflow */
    background: transparent;
    padding-top: 0;
  }

  /* ---- Tab 栏（Apple HIG 分段式胶囊风格） ---- */
  .kp-tabs-bar {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-3) 0;
    border-bottom: none;
    position: sticky;
    top: 0;
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    z-index: var(--z-sticky);
    margin-bottom: var(--space-2);
  }

  .kp-tab {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 6px 12px;
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-radius: var(--radius-full);
    cursor: pointer;
    transition: all var(--transition-fast);
    white-space: nowrap;
    height: 28px;
  }

  .kp-tab:hover {
    color: var(--foreground);
    background: var(--surface-hover);
  }

  .kp-tab.active {
    color: var(--foreground);
    background: var(--surface-active);
    font-weight: var(--font-semibold);
  }

  .kp-tab-count {
    font-size: 10px;
    font-weight: var(--font-medium);
    min-width: 18px;
    height: 18px;
    line-height: 18px;
    text-align: center;
    padding: 0 5px;
    background: color-mix(in srgb, var(--foreground) 10%, transparent);
    color: var(--foreground-muted);
    border-radius: var(--radius-full);
  }

  .kp-tab.active .kp-tab-count {
    background: var(--foreground);
    color: var(--background);
  }

  .kp-tab-actions {
    display: flex;
    gap: var(--space-1);
    margin-left: auto;
  }

  .kp-icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-icon-btn:hover:not(:disabled) {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .kp-icon-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .kp-icon-btn--danger:hover:not(:disabled) {
    background: var(--error-muted);
    color: var(--error);
  }

  :global(.spinning) {
    animation: kp-spin 1s linear infinite;
  }

  @keyframes kp-spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ---- 搜索栏 ---- */
  .kp-search-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border);
    color: var(--foreground-muted);
  }

  .kp-search-input {
    flex: 1;
    border: none;
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-sm);
    outline: none;
    min-width: 0;
  }

  .kp-search-input::placeholder {
    color: var(--foreground-muted);
    opacity: 0.6;
  }

  .kp-search-clear {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    background: var(--surface-3);
    border: none;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-search-clear:hover {
    background: var(--surface-active);
    color: var(--foreground);
  }

  .kp-add-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    background: var(--surface-1);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-add-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
    border-color: var(--foreground-muted);
  }

  .kp-editor-card {
    margin: 0 var(--space-3) var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .kp-editor-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .kp-editor-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .kp-editor-input,
  .kp-editor-textarea {
    width: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    padding: 6px 8px;
    outline: none;
  }

  .kp-editor-textarea {
    resize: vertical;
    min-height: 86px;
  }

  .kp-editor-input:focus,
  .kp-editor-textarea:focus {
    border-color: var(--primary);
  }

  .kp-editor-error {
    color: var(--error);
    font-size: var(--text-xs);
  }

  .kp-load-error {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border: 1px solid color-mix(in srgb, var(--error) 26%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--error-muted);
    color: var(--error);
    font-size: var(--text-xs);
  }

  .kp-editor-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .kp-editor-btn {
    padding: 5px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .kp-editor-btn--primary {
    background: var(--primary);
    border-color: var(--primary);
    color: white;
  }

  .kp-editor-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }


  /* ---- 主内容区 ---- */
  .kp-content {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-3);
  }

  .kp-loading {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    height: 200px;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .kp-spinner {
    width: 20px;
    height: 20px;
    border: 2px solid var(--border);
    border-top-color: var(--primary);
    border-radius: var(--radius-full);
    animation: kp-spin 1s linear infinite;
  }

  /* ---- 概览 ---- */
  .kp-overview {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  .kp-stats-row {
    display: flex;
    align-items: center;
    justify-content: space-around;
    padding: var(--space-3) var(--space-2);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .kp-stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }

  .kp-stat-value {
    font-size: var(--text-md);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    font-variant-numeric: tabular-nums;
  }

  .kp-stat-label {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .kp-stat-divider {
    width: 1px;
    height: 24px;
    background: var(--border);
  }

  .kp-index-notice {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid color-mix(in srgb, var(--primary) 22%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-1));
    color: var(--foreground-muted);
  }

  .kp-index-notice.failed {
    border-color: color-mix(in srgb, var(--warning) 28%, var(--border));
    background: color-mix(in srgb, var(--warning) 9%, var(--surface-1));
  }

  .kp-index-notice-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .kp-index-notice-title {
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
  }

  .kp-index-notice-hint {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
  }

  .kp-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .kp-section-title {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin: 0;
  }

  .kp-section-link {
    margin-left: auto;
    font-size: var(--text-xs);
    font-weight: var(--font-normal);
    color: var(--primary);
    background: none;
    border: none;
    cursor: pointer;
    padding: 0;
  }

  .kp-section-link:hover {
    text-decoration: underline;
  }

  .kp-tech-grid {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .kp-tech-badge {
    font-size: var(--text-xs);
    padding: 2px 8px;
    background: var(--surface-2);
    color: var(--foreground);
    border-radius: var(--radius-full);
    border: 1px solid var(--border-subtle);
  }

  .kp-entry-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .kp-entry-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-1);
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
  }

  /* 入口文件可点击变体：复用 .kp-entry-item 视觉，叠加 button reset + hover 反馈 */
  .kp-entry-item--clickable {
    width: 100%;
    border: none;
    text-align: left;
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }
  .kp-entry-item--clickable:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
  .kp-entry-item--clickable:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  /* 概览预览条目 */
  .kp-preview-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-sm);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
  }

  .kp-preview-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .kp-preview-dot.default { background: var(--foreground-muted); opacity: 0.4; }
  .kp-preview-dot.learning { background: var(--warning); opacity: 0.75; }

  .kp-preview-text {
    flex: 1;
    min-width: 0;
    color: var(--foreground);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .kp-preview-date {
    flex: 0 0 auto;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-variant-numeric: tabular-nums;
  }

  /* ---- 列表 ---- */
  .kp-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .kp-empty {
    /* 复用全局 .empty-state 模式，这里只保留特有调整 */
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: var(--space-8) var(--space-4);
    color: var(--foreground-muted);
    text-align: center;
    opacity: 0.7;
  }

  .kp-empty-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    opacity: 0.8;
  }

  .kp-empty-hint {
    font-size: var(--text-xs);
    opacity: 0.6;
  }

  /* ---- 卡片（ADR / FAQ 条目） ---- */
  .kp-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    overflow: hidden;
    transition: border-color var(--transition-fast);
  }

  .kp-card:hover {
    border-color: var(--foreground-muted);
  }

  .kp-card.expanded {
    border-color: var(--primary);
  }

  .kp-card-header {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-3);
    cursor: pointer;
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    color: inherit;
  }

  .kp-card-header:hover .kp-card-delete,
  .kp-card-header:hover .kp-card-action {
    opacity: 1;
  }

  .kp-card-indicator {
    width: 3px;
    min-height: 20px;
    border-radius: 2px;
    flex-shrink: 0;
    margin-top: 2px;
  }

  .kp-card-indicator.default { background: var(--border); }
  .kp-card-indicator.faq { background: var(--color-gemini); }
  .kp-card-indicator.learning { background: var(--warning); }

  .kp-card-main {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
  }

  .kp-card-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    display: -webkit-box;
    line-height: var(--leading-normal);
    overflow: hidden;
    text-overflow: ellipsis;
    overflow-wrap: anywhere;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .kp-card-preview {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin: 4px 0 0;
    line-height: var(--leading-normal);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .kp-card-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: 0 0 auto;
    color: var(--foreground-muted);
    min-width: max-content;
    margin-left: auto;
  }

  .kp-card-delete,
  .kp-card-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    opacity: 0.7;
    flex-shrink: 0;
    transition: all var(--transition-fast);
  }

  .kp-card-action:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .kp-card-delete:hover {
    background: var(--error-muted);
    color: var(--error);
  }

  /* ---- 卡片展开详情 ---- */
  .kp-card-body {
    padding: 0 var(--space-3) var(--space-3) calc(var(--space-3) + 3px + var(--space-2));
    border-top: 1px solid var(--border);
  }

  .kp-detail-block {
    margin-top: var(--space-3);
  }

  .kp-detail-block h5 {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin: 0 0 var(--space-2) 0;
    text-transform: uppercase;
    letter-spacing: 0.3px;
    opacity: 0.7;
  }

  .kp-detail-block p {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin: 0;
    line-height: var(--leading-relaxed);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .kp-tag-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }

  .kp-tag {
    font-size: var(--text-2xs);
    padding: 1px 6px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
  }

  /* ---- 分类徽章 ---- */
  .kp-category-badge {
    font-size: 10px;
    padding: 1px 6px;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  /* ---- 确认弹窗 ---- */
  .kp-confirm-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--overlay);
    z-index: var(--z-modal);
  }

  .kp-confirm-dialog {
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-5);
    max-width: 300px;
    width: 90%;
    box-shadow: var(--shadow-lg);
    text-align: center;
  }

  .kp-confirm-icon {
    color: var(--warning);
    margin-bottom: var(--space-3);
  }

  .kp-confirm-title {
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .kp-confirm-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin: 0 0 var(--space-5) 0;
    line-height: var(--leading-normal);
  }

  .kp-confirm-actions {
    display: flex;
    justify-content: center;
    gap: var(--space-3);
  }

  .kp-confirm-btn {
    padding: var(--space-2) var(--space-5);
    font-size: var(--text-sm);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .kp-confirm-btn--cancel {
    background: transparent;
    color: var(--foreground-muted);
  }

  .kp-confirm-btn--cancel:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .kp-confirm-btn--danger {
    background: var(--error);
    border-color: var(--error);
    color: white;
  }

  .kp-confirm-btn--danger:hover {
    opacity: 0.9;
  }
</style>
