<script lang="ts">
  import Modal from './Modal.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import { loadViewDraft, saveViewDraft, clearViewDraft } from '../lib/view-drafts';
  import Disclosure from './Disclosure.svelte';
  import { onMount } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import type { StandardMessage } from '../shared/protocol/message-protocol';
  import { MessageCategory } from '../shared/protocol/message-protocol';
  import { ensureArray } from '../lib/utils';
  import Icon from './Icon.svelte';
  import { selectedComposerSourceFolderId } from '../stores/composer-workspace.svelte';
  import FileName from './FileName.svelte';
  import FileTypeIcon from './FileTypeIcon.svelte';
  import KnowledgeGraphPanel from './KnowledgeGraphPanel.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getState, messagesState } from '../stores/messages.svelte';
  import { openCodeTab } from '../stores/right-pane.svelte';
  import { requestOpenHtmlFileInBrowser } from '../lib/browser-navigation';
  import {
    RUNTIME_CONNECTION_EVENT,
    type AgentConnectionEventDetail,
    addAgentKnowledgeItem,
    addAgentKnowledgeRelation,
    clearAgentProjectKnowledge,
    deleteAgentKnowledgeItem,
    deleteAgentKnowledgeRelation,
    getAgentKnowledgeGraph,
    getAgentProjectKnowledge,
    listAgentKnowledgeRelations,
    isWebAgentMode,
    reindexAgentProjectKnowledge,
    updateAgentKnowledgeRelation,
    updateAgentKnowledgeItem,
    type AgentKnowledgeRelation,
    type AgentKnowledgeRelationDraft,
    type AgentKnowledgeRelationsResponse,
  } from '../web/agent-api';

  // 知识类型定义
  interface CodeIndex {
    files?: Array<{ path: string; lines?: number; size?: number }>;
    techStack?: string[];
    entryPoints?: string[];
  }

  type CodeIndexStatusValue = 'indexing' | 'indexed' | 'empty' | 'failed';
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
    return `${workspaceId}::${workspacePath}::${selectedComposerSourceFolderId(workspaceId, appState.currentSessionId)}`;
  });
  let currentTab = $state<'overview' | 'adr' | 'faq' | 'learning' | 'graph'>('overview');
  let isLoading = $state(false);
  let hasRequestedKnowledge = $state(false);
  let lastWorkspaceKey = $state('');
  let knowledgeWorkspaceGeneration = 0;
  let knowledgeLoadRequestSeq = 0;
  let activeKnowledgeLoadRequestId = $state('');
  let knowledgeLoadTimeout: ReturnType<typeof setTimeout> | null = null;
  let knowledgeIndexPollTimeout: ReturnType<typeof setTimeout> | null = null;
  let loadError = $state('');
  let codeIndex = $state<CodeIndex | null>(null);
  let codeIndexStatus = $state<CodeIndexStatus | null>(null);
  let graphPayload = $state<Record<string, unknown> | null>(null);
  let graphLoading = $state(false);
  let graphRequestSeq = 0;
  let knowledgeRelations = $state<AgentKnowledgeRelation[]>([]);
  let knowledgeRelationsMeta = $state<Pick<AgentKnowledgeRelationsResponse, 'totalRelations' | 'truncated'>>({ totalRelations: 0, truncated: false });
  let adrs = $state<ADR[]>([]);
  let faqs = $state<FAQ[]>([]);
  let learnings = $state<Learning[]>([]);
  let searchQuery = $state('');
  let showAllEntryPoints = $state(false);
  type EditableKnowledgeKind = 'adr' | 'faq' | 'learning';

  let expandedAdrId = $state<string | null>(null);
  let expandedFaqId = $state<string | null>(null);
  let expandedLearningId = $state<string | null>(null);
  let showClearConfirm = $state(false);
  let editorElement = $state<HTMLDivElement | null>(null);
  $effect(() => { editorElement?.scrollIntoView({ block: 'nearest' }); });
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
    const title = codeIndexStatus.status === 'indexing'
      ? i18n.t('knowledge.overview.indexing')
      : codeIndexStatus.status === 'failed'
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

  /** Electron 中 HTML 入口进入 Browser；其他文件和 Web 端进入文件视图。 */
  function previewEntryPoint(filepath: string) {
    if (!filepath) return;
    const previewScope = knowledgeWorkspaceScope(currentKnowledgeWorkspaceRequest(), true);
    if (requestOpenHtmlFileInBrowser(filepath, previewScope)) return;
    openCodeTab(null, filepath, {
      sessionId: '',
      sourceFolderId: selectedComposerSourceFolderId(messagesState.currentWorkspaceId || '', messagesState.currentSessionId),
      workspaceId: messagesState.currentWorkspaceId ?? undefined,
      workspacePath: messagesState.currentWorkspacePath || undefined,
    });
  }

  function openKnowledgeRecord(knowledgeId: string): void {
    const id = knowledgeId.trim();
    if (!id) return;
    searchQuery = '';
    if (adrs.some((item) => item.id === id)) {
      currentTab = 'adr';
      expandedAdrId = id;
    } else if (faqs.some((item) => item.id === id)) {
      currentTab = 'faq';
      expandedFaqId = id;
    } else if (learnings.some((item) => item.id === id)) {
      currentTab = 'learning';
      expandedLearningId = id;
    }
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

  let editorDraftKey = '';
  let editorBaseline = '';
  function restoreEditorDraft() {
    editorDraftKey = `knowledge:${currentWorkspaceKey}:${editorKind}:${editorId ?? 'new'}`;
    editorBaseline = JSON.stringify({ formTitle, formContent, formContext, formTags });
    ({ formTitle, formContent, formContext, formTags } = loadViewDraft(editorDraftKey, editorBaseline, { formTitle, formContent, formContext, formTags }));
  }
  $effect(() => {
    if (editorKind && editorDraftKey) saveViewDraft(editorDraftKey, editorBaseline, { formTitle, formContent, formContext, formTags });
  });

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
    restoreEditorDraft();
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
    restoreEditorDraft();
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
    restoreEditorDraft();
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
    restoreEditorDraft();
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
          await updateAgentKnowledgeItem(editorId, patch, knowledgeWorkspaceScope(request));
        } else {
          await addAgentKnowledgeItem({
            kind,
            title: kind === 'learning' ? undefined : title,
            content,
            tags,
            context: kind === 'learning' ? context : undefined,
          }, knowledgeWorkspaceScope(request));
        }
      } else {
        if (editorId) {
          const updatePayload: { type: string; [key: string]: unknown } = {
            type: 'updateKnowledgeItem',
            knowledgeId: editorId,
            ...knowledgeWorkspaceScope(request),
            content,
            tags,
          };
          if (kind === 'adr' || kind === 'faq') updatePayload.title = title;
          if (kind === 'learning') updatePayload.context = context;
          vscode.postMessage(updatePayload);
        } else {
          const addPayload: { type: string; [key: string]: unknown } = {
            type: 'addKnowledgeItem',
            ...knowledgeWorkspaceScope(request),
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
      clearViewDraft(editorDraftKey);
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
    graphRequestSeq += 1;
    graphLoading = false;
    codeIndex = null;
    codeIndexStatus = null;
    graphPayload = null;
    knowledgeRelations = [];
    knowledgeRelationsMeta = { totalRelations: 0, truncated: false };
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

  function clearKnowledgeLoadTimeout() {
    if (knowledgeLoadTimeout) {
      clearTimeout(knowledgeLoadTimeout);
      knowledgeLoadTimeout = null;
    }
  }

  function clearKnowledgeIndexPollTimeout() {
    if (knowledgeIndexPollTimeout) {
      clearTimeout(knowledgeIndexPollTimeout);
      knowledgeIndexPollTimeout = null;
    }
  }

  function scheduleKnowledgeIndexPoll() {
    clearKnowledgeIndexPollTimeout();
    if (!isKnowledgeActive || codeIndexStatus?.status !== 'indexing') {
      return;
    }
    const request = currentKnowledgeWorkspaceRequest();
    knowledgeIndexPollTimeout = setTimeout(() => {
      knowledgeIndexPollTimeout = null;
      if (!isCurrentKnowledgeWorkspaceRequest(request) || !isKnowledgeActive) {
        return;
      }
      requestKnowledgeLoad(false, true);
    }, 750);
  }

  function finishKnowledgeLoad() {
    activeKnowledgeLoadRequestId = '';
    clearKnowledgeLoadTimeout();
  }

  function scheduleKnowledgeLoadTimeout(request: KnowledgeLoadRequest) {
    clearKnowledgeLoadTimeout();
    knowledgeLoadTimeout = setTimeout(() => {
      if (!isCurrentKnowledgeLoadRequest(request) || activeKnowledgeLoadRequestId !== request.requestId) {
        return;
      }
      loadError = i18n.t('knowledge.toast.loadFailed');
      isLoading = false;
      finishKnowledgeLoad();
    }, 30_000);
  }

  function applyKnowledgePayload(payload: Record<string, unknown> | null | undefined): boolean {
    if (!payloadMatchesCurrentWorkspace(payload)) {
      return false;
    }
    const codeIndexPayload = payload?.codeIndex && typeof payload.codeIndex === 'object'
      ? payload.codeIndex as Record<string, unknown>
      : null;
    codeIndexStatus = normalizeCodeIndexStatus(payload?.codeIndexStatus);
    graphPayload = payload?.graph && typeof payload.graph === 'object' && !Array.isArray(payload.graph)
      ? payload.graph as Record<string, unknown>
      : null;
    applyKnowledgeRelationsPayload(payload);

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
    finishKnowledgeLoad();
    scheduleKnowledgeIndexPoll();
    return true;
  }

  function normalizeCodeIndexStatus(value: unknown): CodeIndexStatus | null {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const record = value as Record<string, unknown>;
    const status = typeof record.status === 'string' ? record.status : '';
    if (status !== 'indexing' && status !== 'indexed' && status !== 'empty' && status !== 'failed') return null;
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
    if (status.status === 'indexing') {
      return i18n.t('knowledge.overview.indexingHint');
    }
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
    sourceFolderId: string;
    workspaceId: string;
    workspacePath: string;
  }

  interface KnowledgeLoadRequest extends KnowledgeWorkspaceRequest {
    loadSeq: number;
    requestId: string;
  }

  function currentKnowledgeWorkspaceRequest(): KnowledgeWorkspaceRequest {
    const workspaceId = typeof appState.currentWorkspaceId === 'string' ? appState.currentWorkspaceId.trim() : '';
    const workspacePath = typeof appState.currentWorkspacePath === 'string' ? appState.currentWorkspacePath.trim() : '';
    return {
      workspaceKey: currentWorkspaceKey,
      sourceFolderId: selectedComposerSourceFolderId(workspaceId, appState.currentSessionId),
      workspaceGeneration: knowledgeWorkspaceGeneration,
      workspaceId,
      workspacePath,
    };
  }

  function hasKnowledgeWorkspaceScope(request: KnowledgeWorkspaceRequest = currentKnowledgeWorkspaceRequest()): boolean {
    return Boolean(request.workspaceId || request.workspacePath);
  }

  function knowledgeWorkspaceScope(request: KnowledgeWorkspaceRequest, includeSourceFolder = false): { scope: 'workspace'; workspaceId: string; workspacePath: string; sourceFolderId?: string } {
    return {
      scope: 'workspace',
      workspaceId: request.workspaceId,
      workspacePath: request.workspacePath,
      ...(includeSourceFolder && request.sourceFolderId ? { sourceFolderId: request.sourceFolderId } : {}),
    };
  }

  function nextKnowledgeLoadRequest(): KnowledgeLoadRequest {
    const loadSeq = ++knowledgeLoadRequestSeq;
    return {
      ...currentKnowledgeWorkspaceRequest(),
      loadSeq,
      requestId: `knowledge-load-${Date.now()}-${loadSeq}`,
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
    console.warn('[KnowledgePanel] load project knowledge failed:', error);
    loadError = i18n.t('knowledge.toast.loadFailed');
    isLoading = false;
    finishKnowledgeLoad();
  }

  function handleKnowledgeMutationError(error: unknown, messageKey: string, request: KnowledgeWorkspaceRequest) {
    if (!isCurrentKnowledgeWorkspaceRequest(request)) {
      return;
    }
    console.warn('[KnowledgePanel] project knowledge mutation failed:', error);
    loadError = i18n.t(messageKey);
    isLoading = false;
    finishKnowledgeLoad();
  }

  function resolvePayloadWorkspaceKey(payload: Record<string, unknown> | null | undefined): string {
    const workspaceId = typeof payload?.workspaceId === 'string' ? payload.workspaceId.trim() : '';
    const workspacePath = typeof payload?.workspacePath === 'string' ? payload.workspacePath.trim() : '';
    return workspaceId || workspacePath ? `${workspaceId}::${workspacePath}::${typeof payload?.sourceFolderId === 'string' ? payload.sourceFolderId : selectedComposerSourceFolderId(workspaceId, appState.currentSessionId)}` : '';
  }

  function payloadMatchesCurrentWorkspace(payload: Record<string, unknown> | null | undefined): boolean {
    return resolvePayloadWorkspaceKey(payload) === currentWorkspaceKey;
  }

  function refreshSettingsAfterKnowledgePayload() {
    vscode.postMessage({ type: 'loadSettingsBootstrap', force: true });
  }

  function applyKnowledgeRelationsPayload(payload: Record<string, unknown> | null | undefined): void {
    const rawRelations = Array.isArray(payload?.relations) ? payload.relations : [];
    knowledgeRelations = rawRelations.flatMap((value) => {
      if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
      const relation = value as Partial<AgentKnowledgeRelation>;
      if (typeof relation.relationId !== 'string' || !relation.source || !relation.target || typeof relation.kind !== 'string') {
        return [];
      }
      return [{
        ...relation,
        relationId: relation.relationId,
        source: relation.source,
        kind: relation.kind as AgentKnowledgeRelation['kind'],
        target: relation.target,
        origin: relation.origin ?? 'inferred',
        status: relation.status ?? 'candidate',
        evidence: ensureArray(relation.evidence).filter((item): item is string => typeof item === 'string'),
      } as AgentKnowledgeRelation];
    });
    const rawMeta = payload?.relationMeta && typeof payload.relationMeta === 'object' && !Array.isArray(payload.relationMeta)
      ? payload.relationMeta as Record<string, unknown>
      : {};
    const rawTotal = rawMeta.totalRelations ?? payload?.totalRelations;
    const totalRelations = typeof rawTotal === 'number' && Number.isFinite(rawTotal)
      ? Math.max(knowledgeRelations.length, Math.trunc(rawTotal))
      : knowledgeRelations.length;
    knowledgeRelationsMeta = {
      totalRelations,
      truncated: rawMeta.truncated === true || payload?.truncated === true || totalRelations > knowledgeRelations.length,
    };
  }

  async function fetchKnowledgeViaApi(request: KnowledgeLoadRequest, reindex: boolean) {
    if (reindex) {
      await reindexAgentProjectKnowledge(knowledgeWorkspaceScope(request, true));
    }
    const [res, relationPayload] = await Promise.all([
      getAgentProjectKnowledge(knowledgeWorkspaceScope(request, true)),
      listAgentKnowledgeRelations(knowledgeWorkspaceScope(request, true)),
    ]);
    if (!isCurrentKnowledgeLoadRequest(request)) {
      return;
    }
    if (!applyKnowledgePayload(res)) {
      isLoading = false;
      finishKnowledgeLoad();
      return;
    }
    knowledgeRelations = relationPayload.relations;
    knowledgeRelationsMeta = {
      totalRelations: relationPayload.totalRelations,
      truncated: relationPayload.truncated,
    };
    refreshSettingsAfterKnowledgePayload();
  }

  async function loadKnowledgeGraph(focus: string | null): Promise<void> {
    const request = currentKnowledgeWorkspaceRequest();
    if (!hasKnowledgeWorkspaceScope(request)) return;
    const requestSeq = ++graphRequestSeq;
    graphLoading = true;
    try {
      const payload = await getAgentKnowledgeGraph(
        {
          focus: focus?.trim() || undefined,
          depth: 1,
          direction: 'both',
          maxNodes: 120,
          maxEdges: 240,
        },
        knowledgeWorkspaceScope(request, true),
      );
      if (requestSeq !== graphRequestSeq || !isCurrentKnowledgeWorkspaceRequest(request)) return;
      if (payloadMatchesCurrentWorkspace(payload)) {
        graphPayload = payload;
      }
    } catch (error) {
      if (requestSeq === graphRequestSeq && isCurrentKnowledgeWorkspaceRequest(request)) {
        console.warn('[KnowledgePanel] load focused knowledge graph failed:', error);
      }
    } finally {
      if (requestSeq === graphRequestSeq && isCurrentKnowledgeWorkspaceRequest(request)) {
        graphLoading = false;
      }
    }
  }

  function requestKnowledgeLoad(reindex = false, background = false) {
    const request = nextKnowledgeLoadRequest();
    if (!hasKnowledgeWorkspaceScope(request)) {
      hasRequestedKnowledge = false;
      isLoading = false;
      loadError = '';
      clearKnowledgeContent();
      return;
    }
    clearKnowledgeIndexPollTimeout();
    hasRequestedKnowledge = true;
    if (!background) {
      isLoading = true;
    }
    activeKnowledgeLoadRequestId = request.requestId;
    scheduleKnowledgeLoadTimeout(request);
    loadError = '';
    if (isWebMode) {
      fetchKnowledgeViaApi(request, reindex)
        .catch((error) => handleKnowledgeLoadError(error, request));
    } else {
      vscode.postMessage({
        type: reindex ? 'reindexProjectKnowledge' : 'getProjectKnowledge',
        knowledgeRequestId: request.requestId,
        ...knowledgeWorkspaceScope(request),
      });
    }
  }

  function refresh() {
    requestKnowledgeLoad(true);
  }

  function toggleAdr(adr: ADR) {
    expandedAdrId = expandedAdrId === adr.id ? null : adr.id;
  }

  function toggleFaq(faq: FAQ) {
    expandedFaqId = expandedFaqId === faq.id ? null : faq.id;
  }

  function deleteAdr(id: string, e: Event) {
    e.stopPropagation();
    void deleteKnowledgeEntry(id, e.currentTarget as HTMLElement);
  }

  function deleteFaq(id: string, e: Event) {
    e.stopPropagation();
    void deleteKnowledgeEntry(id, e.currentTarget as HTMLElement);
  }

  function toggleLearning(learning: Learning) {
    expandedLearningId = expandedLearningId === learning.id ? null : learning.id;
  }

  function deleteLearning(id: string, e: Event) {
    e.stopPropagation();
    void deleteKnowledgeEntry(id, e.currentTarget as HTMLElement);
  }

  async function deleteKnowledgeEntry(id: string, anchor: HTMLElement) {
    const scope = currentWorkspaceKey;
    if (!(await confirmMessage(i18n.t('overlay.deleteKnowledgeBody'), { title: i18n.t('overlay.deleteKnowledge'), confirmLabel: i18n.t('overlay.deleteKnowledge'), tone: 'danger', anchor }))) return;
    if (scope !== currentWorkspaceKey) return;
    const request = currentKnowledgeWorkspaceRequest();
    if (isWebMode) {
      deleteAgentKnowledgeItem(id, knowledgeWorkspaceScope(request))
        .then(() => {
          if (isCurrentKnowledgeWorkspaceRequest(request)) {
            refresh();
          }
        })
        .catch((error) => handleKnowledgeMutationError(error, 'knowledge.toast.deleteFailed', request));
    } else {
      vscode.postMessage({ type: 'deleteKnowledgeItem', knowledgeId: id, ...knowledgeWorkspaceScope(request) });
    }
  }

  async function createKnowledgeRelation(relation: AgentKnowledgeRelationDraft): Promise<void> {
    const request = currentKnowledgeWorkspaceRequest();
    await addAgentKnowledgeRelation(relation, knowledgeWorkspaceScope(request, true));
    if (isCurrentKnowledgeWorkspaceRequest(request)) {
      requestKnowledgeLoad(false);
    }
  }

  async function editKnowledgeRelation(relation: AgentKnowledgeRelationDraft & { relationId: string }): Promise<void> {
    const request = currentKnowledgeWorkspaceRequest();
    await updateAgentKnowledgeRelation(relation, knowledgeWorkspaceScope(request, true));
    if (isCurrentKnowledgeWorkspaceRequest(request)) {
      requestKnowledgeLoad(false);
    }
  }

  async function removeKnowledgeRelation(relationId: string): Promise<void> {
    const request = currentKnowledgeWorkspaceRequest();
    await deleteAgentKnowledgeRelation(relationId, knowledgeWorkspaceScope(request, true));
    if (isCurrentKnowledgeWorkspaceRequest(request)) {
      requestKnowledgeLoad(false);
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
      clearAgentProjectKnowledge(knowledgeWorkspaceScope(request))
        .then(() => {
          if (isCurrentKnowledgeWorkspaceRequest(request)) {
            refresh();
          }
        })
        .catch((error) => handleKnowledgeMutationError(error, 'knowledge.toast.clearFailed', request));
    } else {
      vscode.postMessage({ type: 'clearProjectKnowledge', ...knowledgeWorkspaceScope(request) });
    }
  }

  $effect(() => {
    const nextWorkspaceKey = currentWorkspaceKey;
    if (lastWorkspaceKey === nextWorkspaceKey) {
      return;
    }
    lastWorkspaceKey = nextWorkspaceKey;
    showAllEntryPoints = false;
    knowledgeWorkspaceGeneration += 1;
    knowledgeLoadRequestSeq += 1;
    hasRequestedKnowledge = false;
    isLoading = false;
    finishKnowledgeLoad();
    clearKnowledgeIndexPollTimeout();
    loadError = '';
    clearKnowledgeContent();
    if (isKnowledgeActive) {
      requestKnowledgeLoad();
    }
  });

  $effect(() => {
    if (!isKnowledgeActive || hasRequestedKnowledge || !hasKnowledgeWorkspaceScope()) {
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
      const payloadRequestId = typeof payload.knowledgeRequestId === 'string' ? payload.knowledgeRequestId.trim() : '';
      if (payloadRequestId && payloadRequestId !== activeKnowledgeLoadRequestId) {
        return;
      }
      if (applyKnowledgePayload(payload)) {
        refreshSettingsAfterKnowledgePayload();
      } else if (!payloadRequestId || payloadRequestId === activeKnowledgeLoadRequestId) {
        loadError = i18n.t('knowledge.toast.loadFailed');
        isLoading = false;
        finishKnowledgeLoad();
      }
    });

    const onConnected = (event: Event) => {
      if ((event as CustomEvent<AgentConnectionEventDetail>).detail?.status === 'connected') {
        hasRequestedKnowledge = false;
        if (isKnowledgeActive && hasKnowledgeWorkspaceScope()) requestKnowledgeLoad();
      }
    };
    window.addEventListener(RUNTIME_CONNECTION_EVENT, onConnected);
    return () => {
      window.removeEventListener(RUNTIME_CONNECTION_EVENT, onConnected);
      clearKnowledgeLoadTimeout();
      clearKnowledgeIndexPollTimeout();
      unsubscribe();
    };
  });
</script>

<div class="panel-content-scrollable knowledge-panel">
  <div class="kp-toolbar">
    <nav class="kp-tabs-bar" aria-label={i18n.t('knowledge.navigation')}>
      <button class="kp-tab" aria-current={currentTab === 'overview' ? 'page' : undefined} class:active={currentTab === 'overview'} onclick={() => switchTab('overview')}>
        <Icon name="stats" size={13} />
        <span>{i18n.t('knowledge.tabs.overview')}</span>
      </button>
      <button class="kp-tab" aria-current={currentTab === 'adr' ? 'page' : undefined} class:active={currentTab === 'adr'} onclick={() => switchTab('adr')}>
        <Icon name="document" size={13} />
        <span>{i18n.t('knowledge.tabs.adr')}</span>
        {#if adrs.length > 0}
          <span class="kp-tab-count">{adrs.length}</span>
        {/if}
      </button>
      <button class="kp-tab" aria-current={currentTab === 'faq' ? 'page' : undefined} class:active={currentTab === 'faq'} onclick={() => switchTab('faq')}>
        <Icon name="question" size={13} />
        <span>{i18n.t('knowledge.tabs.faq')}</span>
        {#if faqs.length > 0}
          <span class="kp-tab-count">{faqs.length}</span>
        {/if}
      </button>
      <button class="kp-tab" aria-current={currentTab === 'learning' ? 'page' : undefined} class:active={currentTab === 'learning'} onclick={() => switchTab('learning')}>
        <Icon name="lightbulb" size={13} />
        <span>{i18n.t('knowledge.tabs.learning')}</span>
        {#if learnings.length > 0}
          <span class="kp-tab-count">{learnings.length}</span>
        {/if}
      </button>
      <button class="kp-tab" aria-current={currentTab === 'graph' ? 'page' : undefined} class:active={currentTab === 'graph'} onclick={() => switchTab('graph')}>
        <Icon name="git-branch" size={13} />
        <span>{i18n.t('knowledge.tabs.graph')}</span>
      </button>
    </nav>
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

  <!-- 搜索与新增操作分开，输入框独立获得焦点样式。 -->
  {#if currentTab !== 'overview' && currentTab !== 'graph'}
    <div class="kp-list-toolbar">
      <div class="kp-search-bar">
        <Icon name="search" size={13} />
        <input
          type="text"
          class="kp-search-input"
          placeholder={currentTab === 'adr' ? i18n.t('knowledge.search.adrPlaceholder') : currentTab === 'faq' ? i18n.t('knowledge.search.faqPlaceholder') : i18n.t('knowledge.search.learningPlaceholder')}
          aria-label={currentTab === 'adr' ? i18n.t('knowledge.search.adrPlaceholder') : currentTab === 'faq' ? i18n.t('knowledge.search.faqPlaceholder') : i18n.t('knowledge.search.learningPlaceholder')}
          bind:value={searchQuery}
        />
        {#if searchQuery}
          <button aria-label={i18n.t('knowledge.search.clear')} class="kp-search-clear" onclick={() => searchQuery = ''}>
            <Icon name="close" size={12} />
          </button>
        {/if}
      </div>
          <button class="btn btn--secondary btn--sm" onclick={() => openCreateEditor(currentTab as EditableKnowledgeKind)}>
        <Icon name="plus" size={12} />
        <span>{i18n.t('knowledge.actions.add')}</span>
      </button>
    </div>
  {/if}

  <!-- 清空确认弹窗 -->
  {#if showClearConfirm}
    <Modal title={i18n.t('knowledge.confirm.title')} size="sm" onClose={cancelClear}>
      <div class="kp-confirm-dialog" data-magi-surface="critical">
        <p class="kp-confirm-desc">{i18n.t('knowledge.confirm.desc', { fileCount, adrCount: adrs.length, faqCount: faqs.length, learningCount: learnings.length })}</p>
      </div>
      {#snippet footer()}
        <button class="btn btn--secondary btn--sm" onclick={cancelClear}>{i18n.t('knowledge.confirm.cancel')}</button>
        <button class="btn btn--error btn--sm" onclick={executeClear}>{i18n.t('knowledge.confirm.confirm')}</button>
      {/snippet}
    </Modal>
  {/if}

  <!-- 主内容区 -->
  <div class="kp-content">
    {#if editorKind}
      <div class="kp-editor-card" bind:this={editorElement}>
        <div class="kp-editor-title">{editorTitle()}</div>
        <p class="settings-hint" role="status">{i18n.t('overlay.draftSession')}</p>
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
          <button class="btn btn--secondary btn--sm" onclick={closeEditor} disabled={isSaving}>{i18n.t('knowledge.actions.cancel')}</button>
          <button class="btn btn--primary btn--sm" onclick={saveEditor} disabled={isSaving}>{i18n.t('knowledge.actions.save')}</button>
        </div>
      </div>
    {/if}

    {#if loadError}
      <div class="kp-load-error">
        <Icon name="warning" size={14} />
        <span>{loadError}</span>
      </div>
    {/if}

    {#if isLoading}
      <div class="kp-loading">
        <span class="kp-spinner"><Icon name="loader" size={20} /></span>
        <span>{i18n.t('knowledge.loading')}</span>
      </div>
    {:else if currentTab === 'overview'}
      <!-- 概览 -->
      <div class="kp-overview">
        <!-- 紧凑统计条 -->
        <div class="kp-stats-row">
          <div class="kp-stat">
            <span class="kp-stat-value">{fileCount.toLocaleString(i18n.locale)}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.files')}</span>
          </div>
          <div class="kp-stat">
            <span class="kp-stat-value">{totalLines.toLocaleString(i18n.locale)}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.lines')}</span>
          </div>
          <div class="kp-stat">
            <span class="kp-stat-value">{adrs.length}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.adr')}</span>
          </div>
          <div class="kp-stat">
            <span class="kp-stat-value">{faqs.length}</span>
            <span class="kp-stat-label">{i18n.t('knowledge.overview.faq')}</span>
          </div>
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
              {#each (showAllEntryPoints ? normalizedCodeIndex.entryPoints : normalizedCodeIndex.entryPoints.slice(0, 6)) as entry}
                <button
                  type="button"
                  class="kp-entry-item"
                  onclick={() => previewEntryPoint(entry)}
                  title={i18n.t('rightPane.previewFile', { path: entry })}
                >
                  <FileTypeIcon path={entry} size={14} />
                  <FileName name={entry} />
                </button>
              {/each}
            </div>
            {#if normalizedCodeIndex.entryPoints.length > 6}
              <button class="kp-section-link kp-entry-more" aria-expanded={showAllEntryPoints} onclick={() => showAllEntryPoints = !showAllEntryPoints}>
                {i18n.t(showAllEntryPoints ? 'knowledge.overview.showLess' : 'knowledge.overview.viewAll')}
                <span>({normalizedCodeIndex.entryPoints.length})</span>
              </button>
            {/if}
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
              <button type="button" class="kp-preview-item" onclick={() => openKnowledgeRecord(adr.id)}>
                <Icon name="document" size={14} />
                <span class="kp-preview-text">{adr.title}</span>
                {#if adrDate}
                  <span class="kp-preview-date">{adrDate}</span>
                {/if}
              </button>
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
              <button type="button" class="kp-preview-item" onclick={() => openKnowledgeRecord(learning.id)}>
                <Icon name="lightbulb" size={14} />
                <span class="kp-preview-text">{formatLearningTitle(learning.content)}</span>
                {#if learningDate}
                  <span class="kp-preview-date">{learningDate}</span>
                {/if}
              </button>
            {/each}
          </div>
        {/if}
      </div>

    {:else if currentTab === 'graph'}
      <KnowledgeGraphPanel
        payload={graphPayload as { status?: 'indexing' | 'ready' | 'empty' | 'failed'; reasonCode?: string | null; nodes?: unknown[]; edges?: unknown[]; stats?: Record<string, number>; truncated?: boolean } | null}
        knowledgeOptions={[
          ...adrs.map((item) => ({ id: item.id, label: item.title, kind: 'adr' })),
          ...faqs.map((item) => ({ id: item.id, label: item.question, kind: 'faq' })),
          ...learnings.map((item) => ({ id: item.id, label: formatLearningTitle(item.content), kind: 'learning' })),
        ]}
        relations={knowledgeRelations}
        relationMeta={knowledgeRelationsMeta}
        editable={hasKnowledgeWorkspaceScope()}
        graphLoading={graphLoading}
        onRefresh={refresh}
        onFocusNode={loadKnowledgeGraph}
        onOpenCode={previewEntryPoint}
        onOpenKnowledge={openKnowledgeRecord}
        onCreateRelation={createKnowledgeRelation}
        onUpdateRelation={editKnowledgeRelation}
        onDeleteRelation={removeKnowledgeRelation}
      />

    {:else if currentTab === 'adr'}
      <!-- ADR Tab -->
      <div class="kp-list">
        {#if filteredAdrs.length === 0}
          <div class="kp-empty">
            <Icon name="document" size={28} />
            <div class="kp-empty-title">{i18n.t(searchQuery.trim() ? 'knowledge.search.noResults' : 'knowledge.adr.emptyTitle')}</div>
            {#if searchQuery.trim()}
              <button class="kp-section-link" onclick={() => searchQuery = ''}>{i18n.t('knowledge.search.clear')}</button>
            {:else}
              <div class="kp-empty-hint">{i18n.t('knowledge.adr.emptyHint')}</div>
            {/if}
          </div>
        {:else}
          {#each filteredAdrs as adr (adr.id)}
            {@const isExpanded = expandedAdrId === adr.id}
            {@const adrDate = formatKnowledgeDate(adr.updatedAt)}
            {@const adrTags = visibleTags(adr.tags)}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header">
                <button type="button" class="kp-card-toggle" aria-expanded={isExpanded} onclick={() => toggleAdr(adr)}>
                  <div class="kp-card-main">
                    <span class="kp-card-title">{adr.title}</span>
                    {#if !isExpanded && adr.content}
                      <p class="kp-card-preview">{adr.content}</p>
                    {/if}
                    {#if adrDate}<span class="kp-card-date">{adrDate}</span>{/if}
                  </div>
                  <span class="kp-card-chevron"><Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} /></span>
                </button>
                <div class="kp-card-actions">
                  <button class="kp-card-action" title={i18n.t('knowledge.actions.edit')} onclick={(e) => editAdr(adr, e)}>
                    <Icon name="edit" size={14} />
                  </button>
                  <button class="kp-card-delete" title={i18n.t('knowledge.adr.deleteTitle')} onclick={(e) => deleteAdr(adr.id, e)}>
                    <Icon name="trash" size={14} />
                  </button>
                </div>
              </div>
              <Disclosure expanded={isExpanded}>
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
              </Disclosure>
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
            <div class="kp-empty-title">{i18n.t(searchQuery.trim() ? 'knowledge.search.noResults' : 'knowledge.faq.emptyTitle')}</div>
            {#if searchQuery.trim()}
              <button class="kp-section-link" onclick={() => searchQuery = ''}>{i18n.t('knowledge.search.clear')}</button>
            {:else}
              <div class="kp-empty-hint">{i18n.t('knowledge.faq.emptyHint')}</div>
            {/if}
          </div>
        {:else}
          {#each filteredFaqs as faq (faq.id)}
            {@const isExpanded = expandedFaqId === faq.id}
            {@const faqDate = formatKnowledgeDate(faq.updatedAt)}
            {@const faqTags = visibleTags(faq.tags)}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header">
                <button type="button" class="kp-card-toggle" aria-expanded={isExpanded} onclick={() => toggleFaq(faq)}>
                  <div class="kp-card-main">
                    <span class="kp-card-title">{faq.question}</span>
                    {#if !isExpanded && faq.answer}
                      <p class="kp-card-preview">{faq.answer}</p>
                    {/if}
                    {#if faqDate}<span class="kp-card-date">{faqDate}</span>{/if}
                  </div>
                  <span class="kp-card-chevron"><Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} /></span>
                </button>
                <div class="kp-card-actions">
                  <button class="kp-card-action" title={i18n.t('knowledge.actions.edit')} onclick={(e) => editFaq(faq, e)}>
                    <Icon name="edit" size={14} />
                  </button>
                  <button class="kp-card-delete" title={i18n.t('knowledge.faq.deleteTitle')} onclick={(e) => deleteFaq(faq.id, e)}>
                    <Icon name="trash" size={14} />
                  </button>
                </div>
              </div>
              <Disclosure expanded={isExpanded}>
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
              </Disclosure>
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
            <div class="kp-empty-title">{i18n.t(searchQuery.trim() ? 'knowledge.search.noResults' : 'knowledge.learning.emptyTitle')}</div>
            {#if searchQuery.trim()}
              <button class="kp-section-link" onclick={() => searchQuery = ''}>{i18n.t('knowledge.search.clear')}</button>
            {:else}
              <div class="kp-empty-hint">{i18n.t('knowledge.learning.emptyHint')}</div>
            {/if}
          </div>
        {:else}
          {#each filteredLearnings as learning (learning.id)}
            {@const isExpanded = expandedLearningId === learning.id}
            {@const learningSource = formatKnowledgeSource(learning.context)}
            {@const learningDate = formatKnowledgeDate(learning.updatedAt ?? learning.createdAt)}
            {@const learningTags = visibleTags(learning.tags)}
            <div class="kp-card" class:expanded={isExpanded}>
              <div class="kp-card-header">
                <button type="button" class="kp-card-toggle" aria-expanded={isExpanded} onclick={() => toggleLearning(learning)}>
                  <div class="kp-card-main">
                    <span class="kp-card-title">{formatLearningTitle(learning.content)}</span>
                    {#if !isExpanded && learningSource}
                      <p class="kp-card-preview">{learningSource}</p>
                    {/if}
                    {#if learningDate}<span class="kp-card-date">{learningDate}</span>{/if}
                  </div>
                  <span class="kp-card-chevron"><Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} /></span>
                </button>
                <div class="kp-card-actions">
                  <button class="kp-card-action" title={i18n.t('knowledge.actions.edit')} onclick={(e) => editLearning(learning, e)}>
                    <Icon name="edit" size={14} />
                  </button>
                  <button class="kp-card-delete" title={i18n.t('knowledge.learning.deleteTitle')} onclick={(e) => deleteLearning(learning.id, e)}>
                    <Icon name="trash" size={14} />
                  </button>
                </div>
              </div>
              <Disclosure expanded={isExpanded}>
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
              </Disclosure>
            </div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .knowledge-panel {
    container: knowledge-layout / inline-size;
    min-width: 0;
    gap: var(--space-4);
    overflow: hidden;
    background: transparent;
  }

  .kp-toolbar, .kp-list-toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
    flex-shrink: 0;
  }

  .kp-toolbar { border-bottom: var(--magi-hairline) solid var(--border); }
  .kp-tabs-bar {
    display: flex;
    flex: 1;
    min-width: 0;
    gap: var(--space-3);
    overflow-x: auto;
    scrollbar-width: thin;
  }

  .kp-tab {
    display: inline-flex;
    align-items: center;
    flex-shrink: 0;
    gap: var(--space-1);
    min-height: 36px;
    padding: 6px 2px;
    border: 0;
    border-radius: 0;
    box-shadow: inset 0 -2px transparent;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    white-space: nowrap;
    cursor: pointer;
  }
  .kp-tab:hover { color: var(--foreground); }
  .kp-tab.active {
    color: var(--foreground);
    box-shadow: inset 0 -2px var(--foreground);
    font-weight: var(--font-semibold);
  }
  .kp-tab-count {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .kp-tab-actions, .kp-card-actions { display: flex; flex-shrink: 0; gap: var(--space-1); }
  .kp-icon-btn, .kp-card-action, .kp-card-delete, .kp-search-clear {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }
  .kp-icon-btn:hover:not(:disabled), .kp-card-action:hover, .kp-search-clear:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
  .kp-icon-btn--danger:hover:not(:disabled), .kp-card-delete:hover {
    background: var(--error-muted);
    color: var(--error);
  }
  .kp-icon-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .kp-search-bar {
    display: flex;
    align-items: center;
    flex: 1;
    gap: var(--space-2);
    min-width: 0;
    min-height: 32px;
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border);
    background: var(--surface-1);
    color: var(--foreground-muted);
  }
  .kp-search-bar:focus-within { box-shadow: var(--magi-focus-shadow); }
  .kp-search-input {
    width: 100%;
    min-width: 0;
    border: 0;
    outline: 0;
    box-shadow: none;
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-sm);
  }
  .kp-search-input::placeholder { color: var(--foreground-muted); }
  .kp-search-clear { width: 24px; height: 24px; }
  .kp-editor-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
    margin-bottom: var(--space-4);
    padding: var(--space-4);
    border-radius: var(--radius-md);
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border);
  }
  .kp-editor-title { color: var(--foreground); font-size: var(--text-sm); font-weight: var(--font-semibold); }
  .kp-editor-field { display: flex; flex-direction: column; gap: var(--space-1); min-width: 0; color: var(--foreground-muted); font-size: var(--text-xs); }
  .kp-editor-input, .kp-editor-textarea {
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    padding: 8px;
    border: 0;
    border-radius: var(--radius-sm);
    outline: 0;
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border);
    background: var(--surface-1);
    color: var(--foreground);
    font-size: var(--text-sm);
  }
  .kp-editor-textarea { resize: vertical; min-height: 110px; }
  .kp-editor-input:focus-visible, .kp-editor-textarea:focus-visible { box-shadow: var(--magi-focus-shadow); }
  .kp-editor-actions { display: flex; justify-content: flex-end; flex-wrap: wrap; gap: var(--space-2); }
  .kp-editor-error { color: var(--error); font-size: var(--text-xs); overflow-wrap: anywhere; }
  .kp-load-error, .kp-index-notice {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-3);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .kp-load-error { margin-bottom: var(--space-4); background: var(--error-muted); color: var(--error); }
  .kp-index-notice { grid-column: 1 / -1; }
  .kp-index-notice.failed { background: var(--warning-muted); color: var(--warning); }
  .kp-index-notice-text { display: flex; flex-direction: column; gap: var(--space-1); min-width: 0; }
  .kp-index-notice-title { color: var(--foreground); font-weight: var(--font-medium); }
  .kp-index-notice-hint { line-height: var(--leading-normal); }
  .kp-content { flex: 1; min-width: 0; min-height: 0; overflow-y: auto; scrollbar-width: thin; scrollbar-color: var(--scrollbar-thumb) transparent; }
  .kp-loading, .kp-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    min-height: 200px;
    padding: var(--space-5);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    text-align: center;
    overflow-wrap: anywhere;
  }
  .kp-spinner {
    display: inline-flex;
    color: var(--primary);
    width: 20px;
    height: 20px;
    animation: kp-spin 1s linear infinite;
  }
  @keyframes kp-spin { to { transform: rotate(360deg); } }
  .kp-overview { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); align-items: start; gap: var(--space-6); }
  .kp-stats-row {
    display: grid;
    grid-column: 1 / -1;
    grid-template-columns: repeat(auto-fit, minmax(104px, 1fr));
    gap: var(--space-3);
    padding-bottom: var(--space-5);
    border-bottom: var(--magi-hairline) solid var(--border);
  }
  .kp-stat { display: flex; flex-direction: column; align-items: flex-start; gap: var(--space-1); min-width: 0; }
  .kp-stat-value { color: var(--foreground); font-size: var(--text-lg); font-weight: var(--font-semibold); font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
  .kp-stat-label { color: var(--foreground-muted); font-size: var(--text-xs); }
  .kp-section { display: flex; flex-direction: column; gap: var(--space-2); min-width: 0; }
  .kp-section-title { display: flex; align-items: center; flex-wrap: wrap; gap: var(--space-2); margin: 0 0 var(--space-1); color: var(--foreground); font-size: var(--text-sm); font-weight: var(--font-semibold); }
  .kp-section-link { margin-left: auto; padding: 2px 0; border: 0; background: transparent; color: var(--foreground-muted); font-size: var(--text-xs); font-weight: var(--font-normal); cursor: pointer; }
  .kp-section-link:hover { color: var(--foreground); text-decoration: underline; }
  .kp-empty .kp-section-link { margin-left: 0; }
  .kp-tech-grid, .kp-tag-list { display: flex; flex-wrap: wrap; gap: var(--space-2); min-width: 0; }
  .kp-tech-badge, .kp-tag { max-width: 100%; padding: 2px 7px; border-radius: var(--radius-sm); background: var(--surface-2); color: var(--foreground-muted); font-size: var(--text-xs); overflow-wrap: anywhere; }
  .kp-entry-list { display: flex; flex-direction: column; gap: var(--space-1); }
  .kp-entry-item, .kp-preview-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-width: 0;
    padding: 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .kp-entry-more { align-self: flex-start; margin-left: 8px; }
  .kp-entry-item { font-family: var(--font-mono); font-size: var(--text-xs); }
  .kp-entry-item:hover, .kp-preview-item:hover { background: var(--surface-hover); color: var(--foreground); }
  .kp-preview-text { flex: 1; min-width: 0; overflow: hidden; color: var(--foreground); text-overflow: ellipsis; white-space: nowrap; }
  .kp-preview-date, .kp-card-date { color: var(--foreground-muted); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .kp-preview-date { flex-shrink: 0; }
  .kp-list { display: flex; flex-direction: column; gap: var(--space-3); }
  .kp-empty-title { color: var(--foreground); font-weight: var(--font-medium); }
  .kp-empty-hint { font-size: var(--text-xs); line-height: var(--leading-normal); }
  .kp-card { min-width: 0; border-radius: var(--radius-md); box-shadow: inset 0 0 0 var(--magi-hairline) var(--border); }
  .kp-card.expanded { box-shadow: inset 0 0 0 var(--magi-hairline) color-mix(in srgb, var(--foreground) 28%, var(--border)); }
  /* 展开按钮跨整行，独立操作占中间轨道；箭头始终位于最右。 */
  .kp-card-header { display: grid; grid-template-columns: minmax(0, 1fr) auto 14px; align-items: start; gap: var(--space-2); padding: var(--space-3); }
  .kp-card-toggle { display: grid; grid-column: 1 / -1; grid-row: 1; grid-template-columns: subgrid; align-items: start; gap: var(--space-2); min-width: 0; padding: 0; border: 0; background: transparent; color: inherit; text-align: left; white-space: normal; cursor: pointer; }
  .kp-card-main { grid-column: 1; min-width: 0; }
  .kp-card-actions { grid-column: 2; grid-row: 1; z-index: 1; }
  .kp-card-title { display: -webkit-box; overflow: hidden; color: var(--foreground); font-size: var(--text-sm); font-weight: var(--font-medium); line-height: var(--leading-normal); overflow-wrap: anywhere; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
  .kp-card.expanded .kp-card-title { display: block; }
  .kp-card-preview { display: -webkit-box; overflow: hidden; margin: 4px 0 0; color: var(--foreground-muted); font-size: var(--text-xs); line-height: var(--leading-normal); overflow-wrap: anywhere; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
  .kp-card-date { display: block; margin-top: var(--space-1); }
  .kp-card-chevron { grid-column: 3; display: inline-flex; align-items: center; min-height: 28px; color: var(--foreground-muted); }
  .kp-card-body { min-width: 0; padding: var(--space-3); border-top: var(--magi-hairline) solid var(--border); }
  .kp-detail-block + .kp-detail-block, .kp-tag-list { margin-top: var(--space-3); }
  .kp-detail-block h5 { margin: 0 0 var(--space-2); color: var(--foreground); font-size: var(--text-xs); font-weight: var(--font-medium); }
  .kp-detail-block p { margin: 0; color: var(--foreground-muted); font-size: var(--text-sm); line-height: var(--leading-relaxed); white-space: pre-wrap; overflow-wrap: anywhere; }
  .kp-confirm-dialog { color: var(--foreground); }
  .kp-confirm-desc { margin: 0 0 var(--space-5); color: var(--foreground-muted); font-size: var(--text-sm); line-height: var(--leading-normal); }

  @container knowledge-layout (max-width: 640px) {
    .kp-overview { grid-template-columns: minmax(0, 1fr); gap: var(--space-5); }
  }
  @container knowledge-layout (max-width: 360px) {
    .kp-tabs-bar { gap: var(--space-2); }
    .kp-tab { font-size: var(--text-xs); }
    .kp-card-header { grid-template-columns: minmax(0, 1fr) 14px; }
    .kp-card-chevron { grid-column: 2; }
    .kp-card-actions { grid-column: 1 / -1; grid-row: 2; justify-self: end; }
    .kp-list-toolbar { flex-wrap: wrap; }
    .kp-search-bar { flex-basis: 100%; }
  }
</style>
