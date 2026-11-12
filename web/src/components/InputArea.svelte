<script lang="ts">

  import FileName from './FileName.svelte';
  import PluginConversationView from './PluginConversationView.svelte';
  import { conversationViews } from '../lib/plugin-conversation-views';
  import FileTypeIcon from './FileTypeIcon.svelte';
  import { onMount, tick, type Component } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { turnStoreState } from '../stores/turn-store.svelte';
  import { isLocalOptimisticTurn } from '../stores/turn-reducer';
  import { isCanonicalTerminalStatus } from '../shared/protocol/canonical-turn';
  import {
    addToast,
    cancelTurnEditing,
    getActiveInteractionType,
    getQueuedMessages,
    isPersistedSessionId,
    messagesState,
    onDraftSessionAdopted,
  } from '../stores/messages.svelte';
  import type { MessageBrowserNodeSelection, QueuedMessage } from '../types/message';
  import { getAgentRunState } from '../stores/agent-run-store.svelte';
  import { activeAgentRun } from '../stores/active-agent-run.svelte';
  import {
    enhanceAgentPrompt,
    getAgentMainModelCatalog,
    getAgentSettingsBootstrap,
    isReferenceableBrowserAnnotation,
    saveAgentModelContextWindow,
    saveAgentOrchestratorSessionConfig,
    resolveAgentModelReasoning,
    resolveAgentPath,
    updateBrowserAnnotationComment,
    type BrowserAnnotationSnapshot,
    type ResolvedAgentPath,
    type AgentSettingsBootstrapSnapshot,
    settingsBootstrapMatchesCurrentWorkspace,
  } from '../web/agent-api';
  import type { AgentBindingOverride } from '../web/agent-binding-context';
  import Icon from './Icon.svelte';
  import UserQuestionPanel from './UserQuestionPanel.svelte';
  import GoalRunDrawers from './GoalRunDrawers.svelte';
  import { userQuestionState } from '../stores/user-question-store.svelte';
  import Modal from './Modal.svelte';
  import { loadViewDraft, saveViewDraft, clearViewDraft } from '../lib/view-drafts';
  import ContextUsageRing from './ContextUsageRing.svelte';
  import SessionStats from './SessionStats.svelte';
  import { projectSessionContextBudget } from '../lib/context-usage-ring';
  import { resolveUserMessageCommandLabel } from '../lib/user-message-command';
  import GitContextControl from './GitContextControl.svelte';
  import SessionIsolationChip from './SessionIsolationChip.svelte';
  import { generateId } from '../lib/utils';
  import { i18n } from '../stores/i18n.svelte';
  import {
    type AccessProfile,
    ACCESS_PROFILE_STORAGE_KEY,
    isAccessProfile,
    readStoredAccessProfile,
    writeStoredAccessProfile,
  } from '../shared/access-profile';
  import {
    composerWorkspaceState,
    selectedComposerSourceFolderId,
    composerSourceFolderForPath,
    selectComposerSourceFolder,
    resolveComposerWorkspace,
    setComposerHasUnsavedInput,
    type ComposerWorkspaceOption,
  } from '../stores/composer-workspace.svelte';
  import { openWorkspaceFolderPicker, requestProjectCreation } from '../stores/workspace-onboarding.svelte';
  import {
    navigateSession,
    sessionNavigationState,
    waitForSessionNavigation,
  } from '../shared/session-navigation.svelte';
  import ImageAttachmentTray from './ImageAttachmentTray.svelte';
  import { registerPluginComposer, notifyPluginComposerChanged, observeComposerSubmission } from '../lib/plugin-composer';
  import type { PluginComposerTextInput } from '../shared/app-server-protocol.generated';
  import { MAX_COMPOSER_TEXT_CHARS } from '../lib/composer-policy';
  import { materializeSession } from '../web/agent-api';
  import { mainModelContextWindowKey, type MainModelCatalogGroup } from '../shared/main-model-sources';
  import {
    resolveOrchestratorModel,
    resolveOrchestratorReasoningEffort,
    withOrchestratorReasoningEffort,
    type OrchestratorReasoningEffort,
  } from '../shared/orchestrator-session-config';
  import {
    buildComposerActions,
    filterSlashCommands,
    resolveSlashTrigger,
    type ComposerAction,
    type ComposerSessionCommand,
    type ComposerPluginCommandOption,
    type ComposerSkillOption,
  } from '../lib/composer-actions';
  import { pluginProjection, pluginRegistryRevision, trackPluginProjection } from '../stores/plugin-registry.svelte';
  import {
    composerAttachmentTotal,
    MAX_COMPOSER_IMAGE_BYTES,
    MAX_COMPOSER_IMAGES,
    queuedMessageEditable,
    selectDroppedImages,
    sessionCommandDisabledReason,
    summarizeQueuedMessage,
  } from '../lib/composer-policy';
  import { isDesktopRuntime } from '../lib/desktop-updater';
  import {
    addComposerContextReference,
    MAX_COMPOSER_CONTEXT_REFERENCES,
    toSessionContextReferencePayload,
    type ComposerContextReference,
    type ComposerContextReferenceKind,
  } from '../lib/composer-context-references';
  import {
    DESKTOP_CONTEXT_DROP_EVENT,
    normalizeDesktopDropPaths,
    resolveDesktopDroppedPath,
  } from '../lib/desktop-file-drop';
  import { rightPaneState, type BrowserTabPayload } from '../stores/right-pane.svelte';

  interface SelectedImage {
    id: string;
    dataUrl: string;
    name: string;
    size?: number;
  }

  type SelectedBrowserAnnotation = Pick<
    BrowserAnnotationSnapshot,
    | 'annotationId'
    | 'browserSessionId'
    | 'tabId'
    | 'sequence'
    | 'kind'
    | 'comment'
    | 'status'
    | 'screenshotArtifactId'
  >;

  interface QueuedMessageEdit {
    sourceFolderId?: string;
    queueId: string;
    revision: number;
    sessionId: string;
    workspaceId: string;
    workspacePath: string;
  }

  interface ComposerSubmissionDraft {
    sourceFolderId?: string;
    queuedEdit?: QueuedMessageEdit | null;
    accessProfile?: AccessProfile;
    text: string;
    images: SelectedImage[];
    contextReferences: ComposerContextReference[];
    browserAnnotations: SelectedBrowserAnnotation[];
    browserNodeSelections: MessageBrowserNodeSelection[];
    goalMode: boolean;
    sessionCommand: ComposerSessionCommand | null;
    skill: SkillOption | null;
    pluginCommand: ComposerPluginCommandOption | null;
  }

  // 输入框的指令技能来自本地技能配置与 daemon 已就绪的插件技能投影，
  // 这一组才是 `/` 唤起的指令型技能，与 customTools（已注册到工具表）的语义不同。
  type SkillOption = ComposerSkillOption;

  type ReasoningEffort = OrchestratorReasoningEffort;
  type ModelReasoningCapability = import('../shared/rust-backend-types').ModelReasoningCapability;

  // 当前会话最早一个待回答的选择题；回答后下一个自然顶上来。
  const pendingUserQuestion = $derived(
    userQuestionState.sessionId === (messagesState.currentSessionId?.trim() || '')
      ? userQuestionState.pending[0] ?? null
      : null,
  );
  const pendingUserQuestionCount = $derived(
    userQuestionState.sessionId === (messagesState.currentSessionId?.trim() || '')
      ? userQuestionState.pending.length
      : 0,
  );
  const accessProfileOptions: Array<{
    value: AccessProfile;
    labelKey: string;
    descriptionKey: string;
    icon: 'eye' | 'shield' | 'zap';
  }> = [
    {
      value: 'read_only',
      labelKey: 'input.access.readOnly',
      descriptionKey: 'input.access.readOnlyDescription',
      icon: 'eye',
    },
    {
      value: 'restricted',
      labelKey: 'input.access.restricted',
      descriptionKey: 'input.access.restrictedDescription',
      icon: 'shield',
    },
    {
      value: 'full_access',
      labelKey: 'input.access.fullAccess',
      descriptionKey: 'input.access.fullAccessDescription',
      icon: 'zap',
    },
  ];

  // 输入内容
  let inputValue = $state('');
  // 斜杠快捷引用：Goal 决定持续推进生命周期，Skill 决定本轮执行方法，两者可同时引用。
  let selectedGoalMode = $state(false);
  let selectedSessionCommand = $state<ComposerSessionCommand | null>(null);
  let selectedSkill = $state<SkillOption | null>(null);
  let selectedPluginCommand = $state<ComposerPluginCommandOption | null>(null);
  let selectedContextReferences = $state<ComposerContextReference[]>([]);
  let selectedBrowserAnnotations = $state<SelectedBrowserAnnotation[]>([]);
  let selectedBrowserNodeSelections = $state<MessageBrowserNodeSelection[]>([]);
  let editingBrowserAnnotation = $state<SelectedBrowserAnnotation | null>(null);
  let editingBrowserAnnotationComment = $state('');
  let browserAnnotationSaving = $state(false);
  let addMenuOpen = $state(false);
  let contextPickerOpen = $state(false);
  let WebFolderPickerComponent = $state<Component<{
    title?: string;
    includeFiles?: boolean;
    onSelect: (selection: { pathRef: string; displayPath: string; name: string; isDirectory: boolean }) => void;
    onCancel: () => void;
    disabled?: boolean;
  }> | null>(null);
  let contextPickerLoad: Promise<void> | null = null;
  let slashTriggerStart = $state<number | null>(null);
  let slashFilter = $state('');
  let slashHighlightIndex = $state(0);
  let slashListEl = $state<HTMLDivElement | null>(null);

  function loadContextPicker(): Promise<void> {
    if (WebFolderPickerComponent) return Promise.resolve();
    contextPickerLoad ??= import('../web/WebFolderPicker.svelte')
      .then((module) => {
        WebFolderPickerComponent = module.default;
      })
      .finally(() => {
        contextPickerLoad = null;
      });
    return contextPickerLoad;
  }

  $effect(() => {
    if (!contextPickerOpen) return;
    void loadContextPicker().catch((error) => {
      console.error('[InputArea] 上下文目录选择器加载失败:', error);
      contextPickerOpen = false;
      addToast('error', i18n.t('app.featureLoadFailed'));
    });
  });

  // 🔧 图片上传相关状态
  let selectedImages = $state<SelectedImage[]>([]);
  let pendingImageReadCount = $state(0);
  let sendPreparing = $state(false);
  // 提交与 Bridge 结果通过 requestId 成对收口。输入区只拥有草稿，Bridge 只拥有
  // 网络生命周期，不能由任一侧猜测另一个状态，避免失败时永久丢失用户上下文。
  const submittedComposerDrafts = new Map<string, ComposerSubmissionDraft>();
  // 输入区组件在会话切换时不会销毁。草稿必须按会话隔离，否则新会话会继承
  // 上一个会话尚未发送的图片、标记或文本，表现为内容串会话和“新会话自带附件”。
  const scopedComposerDrafts = new Map<string, ComposerSubmissionDraft>();
  const IMAGE_FILE_NAME_PATTERN = /\.(png|jpe?g|gif|webp|avif|bmp|ico|svg|heic|heif)$/i;
  const GENERATED_CLIPBOARD_IMAGE_NAME_PATTERN = /^(clipboard|image|pasted[-_ ]?image)\.[^.]+$/i;
  const CLIPBOARD_IMAGE_TYPE_PRIORITY = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'];

  let enhanceLoading = $state(false);
  let enhanceOriginalPrompt = $state<string | null>(null);
  let enhanceResultPrompt = $state<string | null>(null);
  let enhanceAbortController: AbortController | null = null;
  let enhanceRequestSeq = 0;

  // 主线模型 picker：弹窗状态 + 模型列表惰性拉取。
  // 选中后只写当前会话 orchestrator 覆盖段；全局配置仍是新会话默认值和连接凭据来源。
  let pickerOpen = $state(false);
  let pickerView = $state<'effort' | 'models'>('effort');
  let pickerCapabilities = $state<Record<string, ModelReasoningCapability>>({});
  let sliderDraft = $state<number | null>(null);
  let reasoningAdjusted = $state(false);
  let fallbackReasoningCapability = $state<ModelReasoningCapability | null>(null);
  let reasoningReadStatus = $state<'loading' | 'ready' | 'failed'>('loading');
  let reasoningRetry = $state(0);
  let pickerLoading = $state(false);
  let pickerSavingModel = $state<string | null>(null);
  let pickerSavingReasoning = $state<ReasoningEffort | 'default' | null>(null);
  let pickerModelGroups = $state<MainModelCatalogGroup[]>([]);
  let modelSearch = $state('');
  let workspaceSearch = $state('');
  const filteredPickerModelGroups = $derived(pickerModelGroups.map((group) => ({
    ...group,
    models: group.models.filter((model) =>
      `${group.name} ${model.displayName} ${model.id}`.toLocaleLowerCase().includes(modelSearch.trim().toLocaleLowerCase())),
  })).filter((group) => group.models.length > 0));
  const pickerModelCount = $derived(pickerModelGroups.reduce((count, group) => count + group.models.length, 0));
  let pickerError = $state<string | null>(null);
  let pickerLoadedOnce = false;
  let pickerModelsConfigKey = '';
  // 已拉取列表对应的插件注册表修订。插件激活/停用/升级会增删其数据源，用户数据源配置不变；
  // 此时保留当前列表、后台重拉，避免窗口重新获得焦点（也会递增修订）时列表闪空。
  let pickerPluginRevision = -1;
  let pickerLoadPromise: Promise<void> | null = null;

  let settingsBootstrapRefreshKey = '';
  let settingsBootstrapRefreshSeq = 0;
  let workspacePickerOpen = $state(false);
  let sourceFolderSearch = $state('');
  let workspaceTrigger = $state<HTMLButtonElement | null>(null);
  let workspacePopover = $state<HTMLDivElement | null>(null);
  let sourceFolderPickerOpen = $state(false);
  let sourceFolderTrigger = $state<HTMLButtonElement | null>(null);
  let sourceFolderPopover = $state<HTMLDivElement | null>(null);
  const workspacePopoverId = $props.id();
  const sourceFolderPopoverId = `${workspacePopoverId}-source-folder`;

  function fitContextPopover(node: HTMLElement) {
    let frame = 0;
    const fit = () => {
      node.style.translate = 'none';
      const rect = node.getBoundingClientRect();
      const viewport = window.visualViewport;
      const start = (viewport?.offsetLeft || 0) + 12;
      const end = (viewport?.offsetLeft || 0) + (viewport?.width || window.innerWidth) - 12;
      node.style.translate = `${rect.left < start ? start - rect.left : rect.right > end ? end - rect.right : 0}px 0`;
    };
    // 等工具栏响应式布局完成，再按最终位置约束弹层。
    const scheduleFit = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(fit);
    };
    const observer = new ResizeObserver(scheduleFit);
    observer.observe(node);
    const toolbar = node.closest('.ia-actions');
    if (toolbar) observer.observe(toolbar);
    window.addEventListener('resize', scheduleFit);
    window.visualViewport?.addEventListener('resize', scheduleFit);
    window.visualViewport?.addEventListener('scroll', scheduleFit);
    scheduleFit();
    return { destroy() {
      cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener('resize', scheduleFit);
      window.visualViewport?.removeEventListener('resize', scheduleFit);
      window.visualViewport?.removeEventListener('scroll', scheduleFit);
    } };
  }

  function closeWorkspacePicker(restoreFocus = true): void {
    workspacePickerOpen = false;
    sourceFolderSearch = '';
    if (restoreFocus) workspaceTrigger?.focus();
  }

  async function openSourceFolderPicker(last = false): Promise<void> {
    if (!composerWorkspace || (composerWorkspace.sourceFolders?.length || 0) <= 1 || sessionInputLocked || isInteractionBlocking || editingQueuedMessage || editingTurn) return;
    sourceFolderSearch = '';
    sourceFolderPickerOpen = true;
    workspacePickerOpen = false;
    await tick();
    if (!sourceFolderPickerOpen) return;
    const options = sourceFolderPopover?.querySelectorAll<HTMLButtonElement>('[role="option"]:not(:disabled)');
    const selected = sourceFolderPopover?.querySelector<HTMLButtonElement>('[aria-selected="true"]:not(:disabled)');
    if (last) options?.[options.length - 1]?.focus();
    else (selected || options?.[0])?.focus();
  }

  function closeSourceFolderPicker(restoreFocus = true): void {
    sourceFolderPickerOpen = false;
    sourceFolderSearch = '';
    if (restoreFocus) sourceFolderTrigger?.focus();
  }

  async function openWorkspacePicker(last = false): Promise<void> {
    if (!isDraftSession || sessionInputLocked || isInteractionBlocking) return;
    sourceFolderPickerOpen = false;
    workspacePickerOpen = true;
    await tick();
    if (!workspacePickerOpen) return;
    const options = workspacePopover?.querySelectorAll<HTMLButtonElement>('[role="option"]:not(:disabled)');
    const selected = workspacePopover?.querySelector<HTMLButtonElement>('[aria-selected="true"]:not(:disabled)');
    if (last) options?.[options.length - 1]?.focus();
    else (selected || options?.[0])?.focus();
  }

  function handleContextPickerKeys(event: KeyboardEvent, popover: HTMLDivElement | null, closePicker: () => void): void {
    if (event.isComposing || event.keyCode === 229) return;
    if (event.key === 'Escape') {
      event.preventDefault(); event.stopPropagation(); closePicker(); return;
    }
    const fromSearch = event.target instanceof HTMLInputElement;
    if (fromSearch && !['ArrowDown', 'ArrowUp'].includes(event.key)) return;
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
    const target = event.target as HTMLElement;
    const list = target.closest('[role="listbox"]') || (fromSearch ? target.nextElementSibling : popover);
    const options = Array.from(list?.querySelectorAll<HTMLButtonElement>('[role="option"]:not(:disabled)') || []);
    if (!options.length) return;
    event.preventDefault();
    const index = options.indexOf(target as HTMLButtonElement);
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
      : event.key === 'ArrowDown' ? (index + 1) % options.length
      : (index <= 0 ? options.length : index) - 1;
    options[next].focus();
  }
  let accessProfilePickerOpen = $state(false);
  let selectedAccessProfile = $state<AccessProfile>('restricted');
  const currentPickerModel = $derived.by(() => readOrchestratorModel());

  const currentPickerSourceId = $derived.by(() => readOrchestratorSourceId());
  const mainModelReady = $derived.by(() => currentPickerModel.trim().length > 0 && currentPickerSourceId.trim().length > 0);
  const currentPickerGroup = $derived(pickerModelGroups.find((group) => group.sourceId === currentPickerSourceId));
  const currentPickerEntry = $derived(currentPickerGroup?.models.find((entry) => entry.id === currentPickerModel));
  const currentPickerLabel = $derived(currentPickerEntry?.displayName || currentPickerModel);
  const currentPickerSourceError = $derived(
    typeof messagesState.settingsBootstrapSnapshot?.orchestratorModelError === 'string'
      && messagesState.settingsBootstrapSnapshot.orchestratorModelError.length > 0,
  );
  const currentPickerCapabilityKey = $derived(`${currentPickerSourceId}\u001f${currentPickerModel}`);
  const projectedReasoningCapability = $derived(
    pickerCapabilities[currentPickerCapabilityKey]
      ?? (getEffectiveOrchestratorConfigSnapshot()?.model === currentPickerModel ? messagesState.settingsBootstrapSnapshot?.orchestratorReasoningCapability : null)
      ?? null,
  );
  const currentReasoningCapability = $derived(fallbackReasoningCapability ?? projectedReasoningCapability);
  $effect(() => {
    const projected = projectedReasoningCapability;
    const model = currentPickerModel;
    const config = currentPickerSourceId ? { sourceId: currentPickerSourceId } : null;

    void reasoningRetry;
    fallbackReasoningCapability = null;
    reasoningReadStatus = projected ? 'ready' : 'loading';
    if ((projected && projected.source !== 'unknown') || !model || !config) return;
    let cancelled = false;
    void resolveAgentModelReasoning(config, model).then(value => {
      if (!cancelled) { fallbackReasoningCapability = value; reasoningReadStatus = 'ready'; }
    }).catch(() => { if (!cancelled) reasoningReadStatus = 'failed'; });
    return () => { cancelled = true; };
  });
  const reasoningOptions = $derived(currentReasoningCapability?.supportedEfforts ?? []);
  const currentPickerReasoningEffort = $derived(
    reasoningOptions.includes(readOrchestratorReasoningEffort()!) ? readOrchestratorReasoningEffort() : null,
  );
  const reasoningIndex = $derived(Math.max(0, sliderDraft ?? reasoningOptions.indexOf(currentPickerReasoningEffort!)));
  const displayedReasoning = $derived(sliderDraft === null ? currentPickerReasoningEffort : reasoningOptions[reasoningIndex] ?? null);
  $effect(() => { void currentSessionId; reasoningAdjusted = false; });
  $effect(() => {
    void currentPickerModel;
    void currentSessionId;
    void pickerOpen;
    sliderDraft = null;
  });
  const currentPickerReasoningLabel = $derived.by(() => reasoningEffortLabel(currentPickerReasoningEffort));
  // 上下文用量圆环数据：直接取 orchestrator runtime 快照里的 budgetState。
  // 无活动会话或快照缺失时为 null，圆环组件会渲染占位态。
  const contextBudgetState = $derived.by(() => (
    messagesState.orchestratorRuntimeState?.runtimeSnapshot?.budgetState ?? null
  ));
  const configuredContextWindow = $derived.by(() => {
    const model = currentPickerModel.trim().toLowerCase();
    if (!model) return null;
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) return null;
    const sourceKey = currentPickerSourceId
      ? mainModelContextWindowKey(currentPickerSourceId, model)
      : '';
    const value = (sourceKey ? snapshot?.modelContextWindows?.[sourceKey] : undefined)
      ?? snapshot?.modelContextWindows?.[model];
    return typeof value === 'number' && Number.isFinite(value) && value > 0
      ? Math.floor(value)
      : null;
  });
  const contextBudgetView = $derived.by(() => {
    return projectSessionContextBudget({
      budget: contextBudgetState,
      tokenLimit: configuredContextWindow,
    });
  });
  const auxiliaryConfig = $derived.by(() => getAuxiliaryConfigSnapshot());
  const currentAccessProfileOption = $derived(
    accessProfileOptions.find(option => option.value === selectedAccessProfile) ?? accessProfileOptions[1]
  );
  const auxiliaryEnhanceReady = $derived.by(() => hasUsableModelConfig(auxiliaryConfig));
  const enhanceButtonTitle = $derived.by(() => (
    auxiliaryEnhanceReady ? i18n.t('input.enhance.title') : i18n.t('input.enhance.disabled')
  ));
  const hasEnhanceSnapshot = $derived.by(() => (
    enhanceOriginalPrompt !== null
    && enhanceResultPrompt !== null
  ));

  const currentSessionId = $derived(messagesState.currentSessionId);
  const stopLoading = $derived(messagesState.interruptPendingSessions.has(currentSessionId?.trim() || ''));
  const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
  const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);
  const selectedSourceFolderId = $derived(selectedComposerSourceFolderId(currentWorkspaceId || '', currentSessionId));
  const isDraftSession = $derived.by(() => !currentSessionId?.trim());
  const isPersonalSession = $derived.by(() => (
    !currentWorkspaceId?.trim()
    && !currentWorkspacePath?.trim()
  ));
  const persistedSessionId = $derived.by(() => (
    isPersistedSessionId(currentSessionId) ? currentSessionId?.trim() || '' : ''
  ));
  let composerReferenceScopeKey = $state('');
  const composerWorkspace = $derived.by(() => (
    isPersonalSession
      ? null
      : resolveComposerWorkspace(currentWorkspaceId, currentWorkspacePath)
  ));
  const workspaceOptions = $derived.by(() => composerWorkspaceState.workspaces);
  const filteredSourceFolders = $derived((composerWorkspace?.sourceFolders || []).filter(folder =>
    `${folder.name || ''} ${folder.path}`.toLocaleLowerCase().includes(sourceFolderSearch.trim().toLocaleLowerCase())));
  const selectedSourceFolder = $derived(composerWorkspace?.sourceFolders?.find((folder) => folder.sourceFolderId === selectedSourceFolderId));
  const filteredWorkspaces = $derived(workspaceOptions.filter(workspace => `${workspace.name} ${workspace.rootPath}`.toLocaleLowerCase().includes(workspaceSearch.trim().toLocaleLowerCase())));
  $effect(() => { if (!pickerOpen || pickerModelCount <= 10) modelSearch = ''; });
  $effect(() => { if (!workspacePickerOpen || workspaceOptions.length <= 10) workspaceSearch = ''; });
  $effect(() => { if (!sourceFolderPickerOpen || (composerWorkspace?.sourceFolders?.length || 0) <= 10) sourceFolderSearch = ''; });
  const agentRunState = $derived(getAgentRunState(currentSessionId, currentWorkspaceId));

  function currentComposerReferenceScopeKey(): string {
    // 作用域必须由已提交的工作区绑定和会话共同决定。直接读取 messagesState，
    // 避免把会话切换期间的派生值缓存成旧作用域；路径也参与键值，覆盖仅有路径的工作区。
    return [
      messagesState.currentWorkspaceId?.trim() || '',
      messagesState.currentWorkspacePath?.trim() || '',
      messagesState.currentSessionId?.trim() || '',
    ].join('\u0000');
  }

  $effect(() => onDraftSessionAdopted((workspaceId, sessionId) => {
    // 首条消息的接纳是同一会话的绑定交接，期间继续输入的草稿也随之保留。
    const previousScopeKey = composerReferenceScopeKey;
    const draftDirectory = selectedComposerSourceFolderId(workspaceId, '');
    selectComposerSourceFolder(workspaceId, sessionId, draftDirectory);
    composerReferenceScopeKey = [workspaceId, messagesState.currentWorkspacePath?.trim() || '', sessionId].join('\u0000');
    const draft = scopedComposerDrafts.get(previousScopeKey);
    if (draft) scopedComposerDrafts.set(composerReferenceScopeKey, draft);
    scopedComposerDrafts.delete(previousScopeKey);
  }));

  $effect(() => {
    // 这些直接读取是作用域切换的唯一响应式依赖。InputArea 在导航时保持挂载，
    // 不能依赖组件重建来清空 contenteditable 的旧 DOM。
    void messagesState.currentWorkspaceId;
    void messagesState.currentWorkspacePath;
    void messagesState.currentSessionId;
    const nextScopeKey = currentComposerReferenceScopeKey();
    if (!composerReferenceScopeKey) {
      composerReferenceScopeKey = nextScopeKey;
      return;
    }
    if (nextScopeKey === composerReferenceScopeKey) return;

    // InputArea 在会话切换时不会销毁。先归档旧作用域的草稿，再清空当前渲染状态，
    // 避免新会话继承旧会话的图片、标记或文本，同时保留用户尚未发送的输入。
    const previousScopeKey = composerReferenceScopeKey;
    // 编辑暂时借用输入区，原草稿已在进入编辑时保存；导航不能用编辑内容覆盖它。
    if (!loadedEditingTurnId) {
      if (composerHasDraft()) {
        scopedComposerDrafts.set(previousScopeKey, captureComposerSubmissionDraft(resolveComposerRawContent()));
      } else {
        scopedComposerDrafts.delete(previousScopeKey);
      }
    }

    composerReferenceScopeKey = nextScopeKey;
    clearComposerState({ preserveScopedDraft: true });
    const scopedDraft = scopedComposerDrafts.get(nextScopeKey);
    if (scopedDraft) {
      restoreComposerSubmissionDraft(scopedDraft);
    }
    invalidateEnhanceState();
    addMenuOpen = false;
    contextPickerOpen = false;
    closeSlashMenu();
  });

  const shouldInterruptAgentRunFromComposer = $derived.by(() => {
    const projection = agentRunState.projection;
    const sessionId = currentSessionId?.trim();
    const rootTaskId = projection?.root_task.task_id ?? agentRunState.rootTaskId;
    if (!projection || !sessionId || !rootTaskId) return false;
    return projection.runner_status === 'running';
  });
  const interruptTargetTurnId = $derived.by(() => {
    const turns = turnStoreState.reducer.turns.filter((turn) => (
      turn.sessionId === messagesState.currentSessionId
      && !isCanonicalTerminalStatus(turn.status)
      && !isLocalOptimisticTurn(turn)
    ));
    return turns.length === 1 ? turns[0].turnId : null;
  });
  let queueEditPending = $state(false);
  let queueEditConflict = $state('');
  let editingQueuedMessage = $state<QueuedMessageEdit | null>(null);
  const sessionInputLocked = $derived.by(() => (
    queueEditPending || sessionNavigationState.pending !== null || messagesState.sessionHydrating
  ));

  const isSending = $derived.by(() => messagesState.isProcessing || shouldInterruptAgentRunFromComposer);
  let resumeSubmittingSessionId = $state('');
  const resumeSubmitting = $derived(Boolean(currentSessionId) && resumeSubmittingSessionId === currentSessionId);
  $effect(() => {
    if (resumeSubmittingSessionId && resumeSubmittingSessionId !== currentSessionId) resumeSubmittingSessionId = '';
  });
  // 停止后的“继续推进”只有一种动作：任何停止（失败、中断、用户主动停止）都由 daemon 判定
  // 能否继续并随读模型下发，发送按钮据此显示，Web 不再从轮次或任务状态自行推断。
  const canContinue = $derived(activeAgentRun.continuation !== null);
  const continueSubmitting = $derived(resumeSubmitting);
  const continueTitle = $derived(i18n.t(continueSubmitting ? 'input.continuing' : 'input.continue'));
  onMount(() => {
    const handleContinueStatus = (event: Event) => {
      const detail = (event as CustomEvent<{ sessionId: string; status: string }>).detail;
      if (detail?.sessionId !== messagesState.currentSessionId) return;
      resumeSubmittingSessionId = detail.status === 'pending' ? detail.sessionId : '';
    };
    window.addEventListener('magi:interruptedRecoveryContinueStatus', handleContinueStatus);
    return () => window.removeEventListener('magi:interruptedRecoveryContinueStatus', handleContinueStatus);
  });
  function continueSession(): void {
    if (!canContinue || continueSubmitting || isSending || sessionInputLocked
      || isInteractionBlocking || composerHasDraft()) return;
    vscode.postMessage({ type: 'continueTask', sessionId: currentSessionId });
  }

  const activeInteraction = $derived.by(() => getActiveInteractionType());
  const isInteractionBlocking = $derived.by(() => Boolean(activeInteraction));
  const queuedMessages = $derived.by(() => getQueuedMessages());
  // 叠层层级：紧挨输入框的是 1，离得越远越大（越窄）。自下而上：目标、计划、排队、提问。
  let drawersCount = $state(0);
  const queueDockLevel = $derived(drawersCount + 1);
  const questionDockLevel = $derived(drawersCount + (queuedMessages.length > 0 ? 1 : 0) + 1);
  const MAX_INPUT_CHARS = MAX_COMPOSER_TEXT_CHARS;
  let inputTextareaEl = $state<HTMLDivElement | null>(null);
  let isComposing = $state(false);
  // Safari/Chromium 在 compositionend 后偶尔会再补发一次 Enter；这段
  // 短暂保护期只吞掉该重复事件，不改变后续正常发送。
  let compositionEndedAt = $state(0);
  let pendingCaretOffset = $state<number | null>(null);
  const sendButtonTitle = $derived.by(() => {
    if (sendPreparing) {
      return i18n.t('input.sending');
    }
    if (editingQueuedMessage) return i18n.t('input.queue.save');
    if (isSending) {
      return i18n.t('input.followUp.queueTitle');
    }
    if (!mainModelReady) {
      return i18n.t('input.mainModelRequired');
    }
    return i18n.t('input.send');
  });
  // 首条消息确认 sessionId 前不能追加提交，否则会被识别成另一条新会话请求。
  const sendDisabled = $derived.by(() => (
    sessionInputLocked || (!editingQueuedMessage && isInteractionBlocking) || sendPreparing || pendingImageReadCount > 0 || (!editingQueuedMessage && !mainModelReady)
    || (isDraftSession && isSending)
  ));

  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (inputValue.trim().length > 0) return true;
    return selectedImages.length > 0
      || pendingImageReadCount > 0
      || selectedContextReferences.length > 0
      || selectedBrowserAnnotations.length > 0
      || Boolean(selectedGoalMode || selectedSessionCommand || selectedSkill || selectedPluginCommand || selectedPluginCommand)
      || selectedBrowserNodeSelections.length > 0;
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
      const skillId = typeof obj.skillId === 'string' && obj.skillId.trim()
        ? obj.skillId.trim()
        : '';
      if (!skillId) continue;
      const name = typeof obj.name === 'string' && obj.name.trim()
        ? obj.name.trim()
        : skillId;
      const description = typeof obj.description === 'string' ? obj.description : '';
      out.push({ skillId, name, description });
    }
    for (const manifest of pluginProjection().manifests) {
      for (const skill of manifest.contributions.skills ?? []) {
        out.push({ skillId: `plugin/${manifest.id}/${skill.id}`, name: skill.title, description: skill.description });
      }
    }
    return out;
  });

  // 压缩命令与附件互斥：草稿会话没有可压缩的历史，带附件时 daemon 会拒绝，这里提前说明而不是让它消失。
  const attachmentTotal = $derived(composerAttachmentTotal({
    images: selectedImages.length + pendingImageReadCount,
    contextReferences: selectedContextReferences.length,
    browserAnnotations: selectedBrowserAnnotations.length,
    browserNodeSelections: selectedBrowserNodeSelections.length,
  }));
  const compactDisabledReason = $derived(sessionCommandDisabledReason({
    sessionCommandsAvailable: !isDraftSession,
    attachmentTotal,
  }));

  // 命令入口只来自 daemon 的激活投影，并随插件注册表变化一起刷新。
  $effect(() => {
    trackPluginProjection();
  });
  const availablePluginCommands = $derived<ComposerPluginCommandOption[]>(
    pluginProjection().commands
      .filter((entry) => entry.id.startsWith('plugin/'))
      .map((entry) => ({ pluginCommandId: entry.id, name: entry.title, description: entry.description })),
  );
  // 已选命令失效（插件停用）时撤销选择，避免提交已不可用的身份。
  $effect(() => {
    if (!pluginProjection().loaded) return;
    if (selectedSkill?.skillId.startsWith('plugin/') && !availableSkills.some(skill => skill.skillId === selectedSkill?.skillId)) {
      selectedSkill = null;
    }
    if (selectedPluginCommand && !availablePluginCommands.some((command) => command.pluginCommandId === selectedPluginCommand?.pluginCommandId)) {
      selectedPluginCommand = null;
    }
  });

  const composerActions = $derived.by<ComposerAction[]>(() => buildComposerActions(
    availableSkills,
    {
      goal: {
        name: i18n.t('input.goalMode.name'),
        description: i18n.t('input.goalMode.description'),
      },
      compact: {
        name: i18n.t('input.compact.name'),
        description: i18n.t('input.compact.description'),
      },
      context: {
        name: i18n.t('input.add.context'),
        description: i18n.t('input.add.contextDescription'),
      },
    },
    {
      sessionCommandDisabledReason: compactDisabledReason,
      pluginCommands: availablePluginCommands,
    },
  ));

  const filteredSlashCommands = $derived.by<Array<Exclude<ComposerAction, { kind: 'resource' }>>>(() => {
    if (slashTriggerStart === null) return [];
    return filterSlashCommands(composerActions, slashFilter).filter((command) => (
      command.kind === 'goal'
        ? !selectedGoalMode
        : command.kind === 'command'
          ? command.id !== selectedSessionCommand
        : command.kind === 'skill'
          ? command.id !== selectedSkill?.skillId
          : command.id !== selectedPluginCommand?.pluginCommandId
    ));
  });

  const slashMenuOpen = $derived(slashTriggerStart !== null && filteredSlashCommands.length > 0);

  // 鼠标 hover 或键盘导航切换高亮项时，确保当前选项始终处于可见区域。
  $effect(() => {
    void slashHighlightIndex;
    void filteredSlashCommands;
    if (!slashMenuOpen) return;
    queueMicrotask(() => {
      const list = slashListEl;
      if (!list) return;
      const items = list.querySelectorAll<HTMLElement>('.ia-slash-item');
      const active = items[slashHighlightIndex];
      if (!active) return;
      active.scrollIntoView({ block: 'nearest' });
    });
  });

  let composerUndoEpoch = 0;

  function clearComposerState(options: { preserveScopedDraft?: boolean } = {}) {
    composerUndoEpoch += 1;
    if (!options.preserveScopedDraft && composerReferenceScopeKey) {
      scopedComposerDrafts.delete(composerReferenceScopeKey);
    }
    editingQueuedMessage = null;
    sourceFolderPickerOpen = false;
    sourceFolderSearch = '';
    queueEditConflict = '';
    inputValue = '';
    selectedImages = [];
    selectedContextReferences = [];
    selectedBrowserAnnotations = [];
    selectedBrowserNodeSelections = [];
    selectedGoalMode = false;
    selectedSessionCommand = null;
    selectedSkill = null;
    selectedPluginCommand = null;
    addMenuOpen = false;
    contextPickerOpen = false;
    invalidateEnhanceState();
    closeSlashMenu();
  }

  function composerHasDraft(): boolean {
    return Boolean(
      editingQueuedMessage || inputValue.trim()
      || selectedImages.length > 0
      || selectedContextReferences.length > 0
      || selectedBrowserAnnotations.length > 0
      || selectedBrowserNodeSelections.length > 0
      || selectedGoalMode
      || selectedSessionCommand !== null
      || selectedSkill,
    );
  }

  function currentBrowserTabIdentity(): { browserSessionId: string; tabId: string } | null {
    const pane = rightPaneState.perSession[rightPaneState.activeScopeKey];
    const activeTabId = pane?.activeTabId;
    if (!activeTabId) return null;
    const activeTab = pane.openTabs.find((tab) => tab.id === activeTabId);
    if (!activeTab || activeTab.kind !== 'browser') return null;
    const payload = activeTab.payload as BrowserTabPayload;
    const browserSessionId = typeof payload.browserSessionId === 'string'
      ? payload.browserSessionId.trim()
      : '';
    const tabId = typeof payload.tabId === 'string' ? payload.tabId.trim() : '';
    return browserSessionId && tabId ? { browserSessionId, tabId } : null;
  }

  function browserNodeSelectionKey(selection: MessageBrowserNodeSelection): string {
    return JSON.stringify([
      selection.browserSessionId,
      selection.tabId,
      selection.surfaceId,
      selection.navigationRevision,
      selection.frameId ?? null,
      selection.backendDomNodeId,
      selection.domNodeId ?? null,
    ]);
  }

  function normalizeBrowserNodeSelection(value: unknown): MessageBrowserNodeSelection | null {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const source = value as Record<string, unknown>;
    const browserSessionId = typeof source.browserSessionId === 'string'
      ? source.browserSessionId.trim()
      : '';
    const tabId = typeof source.tabId === 'string' ? source.tabId.trim() : '';
    const surfaceId = typeof source.surfaceId === 'string' ? source.surfaceId.trim() : '';
    const navigationRevision = Number.isSafeInteger(source.navigationRevision)
      && Number(source.navigationRevision) >= 0
      ? Number(source.navigationRevision)
      : null;
    const backendDomNodeId = Number.isSafeInteger(source.backendDomNodeId)
      && Number(source.backendDomNodeId) > 0
      ? Number(source.backendDomNodeId)
      : null;
    const domNodeId = source.domNodeId === undefined || source.domNodeId === null
      ? null
      : Number.isSafeInteger(source.domNodeId) && Number(source.domNodeId) > 0
        ? Number(source.domNodeId)
        : null;
    const domNodeIdIsValid = source.domNodeId === undefined
      || source.domNodeId === null
      || domNodeId !== null;
    const frameId = source.frameId === undefined || source.frameId === null
      ? null
      : typeof source.frameId === 'string'
        ? source.frameId.trim() || null
        : undefined;
    const nodeName = typeof source.nodeName === 'string' ? source.nodeName.trim() : '';
    const url = typeof source.url === 'string' ? source.url.trim() : '';
    const title = typeof source.title === 'string' ? source.title.trim() : '';
    const textExcerpt = typeof source.textExcerpt === 'string' ? source.textExcerpt.trim() : '';
    const outerHtml = typeof source.outerHtml === 'string' ? source.outerHtml : '';
    if (
      !browserSessionId
      || !tabId
      || !surfaceId
      || navigationRevision === null
      || backendDomNodeId === null
      || !domNodeIdIsValid
      || frameId === undefined
      || !nodeName
      || typeof source.url !== 'string'
      || typeof source.title !== 'string'
      || typeof source.textExcerpt !== 'string'
      || typeof source.outerHtml !== 'string'
      || typeof source.outerHtmlTruncated !== 'boolean'
    ) return null;
    if (!source.attributes || typeof source.attributes !== 'object' || Array.isArray(source.attributes)) return null;
    const attributes: Record<string, string> = {};
    for (const [key, attribute] of Object.entries(source.attributes as Record<string, unknown>)) {
      if (typeof attribute !== 'string') return null;
      attributes[key] = attribute;
    }
    let bounds: MessageBrowserNodeSelection['bounds'] = null;
    if (source.bounds !== undefined && source.bounds !== null) {
      if (typeof source.bounds !== 'object' || Array.isArray(source.bounds)) return null;
      const candidate = source.bounds as Record<string, unknown>;
      if (![candidate.x, candidate.y, candidate.width, candidate.height]
        .every((item) => typeof item === 'number' && Number.isFinite(item))) return null;
      bounds = {
        x: Number(candidate.x),
        y: Number(candidate.y),
        width: Math.max(0, Number(candidate.width)),
        height: Math.max(0, Number(candidate.height)),
      };
    }
    const optionalString = (field: string): string | null => {
      const candidate = source[field];
      if (candidate === undefined || candidate === null) return null;
      return typeof candidate === 'string' ? candidate.trim() || null : null;
    };
    if (
      (source.ariaRole !== undefined && source.ariaRole !== null && typeof source.ariaRole !== 'string')
      || (source.ariaName !== undefined && source.ariaName !== null && typeof source.ariaName !== 'string')
    ) return null;
    return {
      browserSessionId,
      tabId,
      surfaceId,
      navigationRevision,
      url,
      title,
      frameId,
      backendDomNodeId,
      domNodeId,
      nodeName,
      attributes,
      textExcerpt,
      outerHtml,
      outerHtmlTruncated: source.outerHtmlTruncated,
      ariaRole: optionalString('ariaRole'),
      ariaName: optionalString('ariaName'),
      bounds,
    };
  }

  function cloneBrowserNodeSelection(selection: MessageBrowserNodeSelection): MessageBrowserNodeSelection {
    return {
      ...selection,
      attributes: { ...selection.attributes },
      bounds: selection.bounds ? { ...selection.bounds } : selection.bounds,
    };
  }

  function captureComposerSubmissionDraft(text: string): ComposerSubmissionDraft {
    return {
      sourceFolderId: selectedSourceFolderId || undefined,
      queuedEdit: editingQueuedMessage ? { ...editingQueuedMessage } : null,
      accessProfile: selectedAccessProfile,
      text,
      images: selectedImages.map((image) => ({ ...image })),
      contextReferences: selectedContextReferences.map((reference) => ({ ...reference })),
      browserAnnotations: selectedBrowserAnnotations.map((annotation) => ({ ...annotation })),
      browserNodeSelections: selectedBrowserNodeSelections.map(cloneBrowserNodeSelection),
      goalMode: selectedGoalMode,
      sessionCommand: selectedSessionCommand,
      skill: selectedSkill ? { ...selectedSkill } : null,
      pluginCommand: selectedPluginCommand ? { ...selectedPluginCommand } : null,
    };
  }

  function composerIsPristineForSubmissionRecovery(): boolean {
    return !editingTurn && !editingQueuedMessage
      && !resolveComposerRawContent().trim()
      && selectedImages.length === 0
      && selectedContextReferences.length === 0
      && selectedBrowserAnnotations.length === 0
      && selectedBrowserNodeSelections.length === 0
      && !selectedGoalMode
      && selectedSessionCommand === null
      && selectedSkill === null
      && selectedPluginCommand === null;
  }

  function restoreComposerSubmissionDraft(draft: ComposerSubmissionDraft): void {
    invalidateEnhanceState();
    editingQueuedMessage = draft.queuedEdit ? { ...draft.queuedEdit } : null;
    selectComposerSourceFolder(currentWorkspaceId || '', currentSessionId, draft.sourceFolderId || '');
    queueEditConflict = '';
    if (draft.accessProfile) selectedAccessProfile = draft.accessProfile;
    inputValue = draft.text;
    pendingCaretOffset = draft.text.length;
    selectedImages = draft.images.map((image) => ({ ...image }));
    selectedContextReferences = draft.contextReferences.map((reference) => ({ ...reference }));
    selectedBrowserAnnotations = draft.browserAnnotations.map((annotation) => ({ ...annotation }));
    selectedBrowserNodeSelections = draft.browserNodeSelections.map(cloneBrowserNodeSelection);
    selectedGoalMode = draft.goalMode;
    selectedSessionCommand = draft.sessionCommand;
    selectedSkill = draft.skill ? { ...draft.skill } : null;
    selectedPluginCommand = draft.pluginCommand ? { ...draft.pluginCommand } : null;
    queueMicrotask(focusEditor);
  }

  let loadedEditingTurnId = '';
  let loadedEditingScopeKey = '';
  const editingTurn = $derived(messagesState.editingTurn);

  $effect(() => {
    setComposerHasUnsavedInput(
      inputValue.trim().length > 0
      || selectedImages.length > 0
      || pendingImageReadCount > 0
      || selectedContextReferences.length > 0
      || selectedBrowserAnnotations.length > 0
      || selectedBrowserNodeSelections.length > 0
      || selectedGoalMode
      || selectedSessionCommand !== null
      || selectedSkill !== null
      || selectedPluginCommand !== null
      || editingTurn !== null,
    );
  });

  $effect(() => {
    const draft = editingTurn;
    if (!draft) {
      if (loadedEditingTurnId) {
        const previousScopeKey = loadedEditingScopeKey;
        loadedEditingTurnId = '';
        loadedEditingScopeKey = '';
        // 导航已经恢复了目标会话的草稿，此时不能再清空新的输入区。
        if (previousScopeKey !== currentComposerReferenceScopeKey()) return;
        clearComposerState({ preserveScopedDraft: true });
        const previousDraft = scopedComposerDrafts.get(previousScopeKey);
        if (previousDraft) restoreComposerSubmissionDraft(previousDraft);
      }
      return;
    }
    if (draft.turnId === loadedEditingTurnId) return;
    const scopeKey = currentComposerReferenceScopeKey();
    if (!loadedEditingTurnId) {
      if (composerHasDraft()) {
        scopedComposerDrafts.set(scopeKey, captureComposerSubmissionDraft(resolveComposerRawContent()));
      } else {
        scopedComposerDrafts.delete(scopeKey);
      }
    }
    loadedEditingTurnId = draft.turnId;
    loadedEditingScopeKey = scopeKey;
    invalidateEnhanceState();
    editingQueuedMessage = null;
    queueEditConflict = '';
    inputValue = draft.text;
    pendingCaretOffset = draft.text.length;
    selectedImages = draft.images.map((image) => ({
      id: generateId(),
      dataUrl: image.dataUrl,
      name: image.name || i18n.t('input.pastedImage', { index: 1 }),
    }));
    selectedContextReferences = draft.contextReferences.map((reference) => ({
      id: `${reference.kind}:${reference.pathRef || reference.path}`,
      ...reference,
    }));
    selectedBrowserAnnotations = draft.browserAnnotationRefs.map((reference, index) => ({
      ...reference,
      sequence: reference.sequence ?? index + 1,
      screenshotArtifactId: reference.screenshotArtifactId ?? null,
      status: 'active',
    }));
    selectedBrowserNodeSelections = draft.browserNodeSelections.map((selection) => ({
      ...selection,
      attributes: { ...selection.attributes },
      bounds: selection.bounds ? { ...selection.bounds } : selection.bounds,
    }));
    selectedGoalMode = draft.goalMode;
    selectedSessionCommand = null;
    selectedSkill = draft.skillName
      ? (availableSkills.find((skill) => skill.skillId === draft.skillName) || null)
      : null;
    queueMicrotask(focusEditor);
  });

  function cancelMessageEditing(): void {
    if (editingQueuedMessage) clearComposerState();
    else cancelTurnEditing();
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

  function clipboardImageTypePriority(file: File): number {
    const priority = CLIPBOARD_IMAGE_TYPE_PRIORITY.indexOf(file.type.toLowerCase());
    return priority >= 0 ? priority : CLIPBOARD_IMAGE_TYPE_PRIORITY.length;
  }

  function collapseGeneratedClipboardRepresentations(files: File[]): File[] {
    const generatedFiles = files.filter((file) => (
      GENERATED_CLIPBOARD_IMAGE_NAME_PATTERN.test(file.name)
    ));
    if (generatedFiles.length <= 1) return files;

    const generatedBaseNames = new Set(generatedFiles.map((file) => (
      file.name.replace(/\.[^.]+$/, '').toLowerCase()
    )));
    if (generatedBaseNames.size !== 1) return files;
    const generatedMediaTypes = new Set(generatedFiles.map((file) => file.type.toLowerCase()));
    if (generatedMediaTypes.size <= 1) return files;

    const preferred = [...generatedFiles].sort((left, right) => (
      clipboardImageTypePriority(left) - clipboardImageTypePriority(right)
    ))[0];
    return [
      ...files.filter((file) => !GENERATED_CLIPBOARD_IMAGE_NAME_PATTERN.test(file.name)),
      preferred,
    ];
  }

  function collectClipboardImageFiles(data: DataTransfer | null | undefined): File[] {
    if (!data) return [];
    const files = Array.from(data.files ?? []).filter((file) => isClipboardImageFile(file));
    if (files.length > 0) {
      return collapseGeneratedClipboardRepresentations(files);
    }

    const itemFiles = Array.from(data.items ?? [])
      .filter((item) => item.kind === 'file')
      .map((item) => ({ file: item.getAsFile(), hintedType: item.type }))
      .filter((entry): entry is { file: File; hintedType: string } => (
        entry.file !== null && isClipboardImageFile(entry.file, entry.hintedType)
      ))
      .map((entry) => entry.file);
    return collapseGeneratedClipboardRepresentations(itemFiles);
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
      addImageToComposer({ dataUrl, name: imageName, size: file.size });
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

  /** 粘贴和拖入共用：按张数 / 体积上限筛选，被丢弃的原因逐条说明。 */
  function addImageFiles(files: File[]): void {
    const selection = selectDroppedImages(files, selectedImages.length + pendingImageReadCount);
    for (const index of selection.accepted) {
      readImageFileIntoComposer(files[index]);
    }
    if (selection.overLimit > 0) {
      addToast('warning', i18n.t('input.maxImages', { max: MAX_COMPOSER_IMAGES }));
    }
    for (const file of selection.tooLarge) {
      addToast('warning', i18n.t('input.imageTooLarge', {
        size: (file.size / 1024 / 1024).toFixed(1),
      }));
    }
    if (selection.ignored > 0 && selection.accepted.length === 0) {
      addToast('warning', i18n.t('input.drop.imagesOnly'));
    }
  }

  // 压缩命令不能带附件：用户主动添加附件时让命令让位并说明，附件不丢。
  function dropSessionCommandForAttachment(): void {
    if (selectedSessionCommand === null && selectedPluginCommand === null) return;
    selectedSessionCommand = null;
    selectedPluginCommand = null;
    addToast('info', i18n.t('input.compact.removedForAttachment'), undefined, { forceVisible: true });
  }

  function activeModeLabel(): string | null {
    if (selectedGoalMode) return i18n.t('input.goalMode.name');
    if (selectedSessionCommand) return i18n.t('input.compact.name');
    if (selectedPluginCommand) return selectedPluginCommand.name;
    if (selectedSkill) return selectedSkill.name;
    return null;
  }

  // goal / 压缩 / skill 三选一：选新的会替换旧的，替换时明说，避免芯片悄悄换掉。
  function noteModeReplaced(previous: string | null, next: string): void {
    if (previous && previous !== next) {
      addToast('info', i18n.t('input.mode.replaced', { from: previous, to: next }), undefined, {
        forceVisible: true,
      });
    }
  }

  function notifyActionDisabled(action: ComposerAction): void {
    if (action.kind !== 'command' || !action.disabledReason) return;
    addToast(
      'info',
      i18n.t(`input.compact.disabled.${action.disabledReason === 'draft-session' ? 'draft' : 'attachments'}`),
      undefined,
      { forceVisible: true },
    );
  }

  function addImageToComposer(image: { dataUrl: string; name: string; size: number }): boolean {
    if (selectedImages.length >= MAX_COMPOSER_IMAGES) {
      addToast('warning', i18n.t('input.maxImages', { max: MAX_COMPOSER_IMAGES }));
      return false;
    }
    if (image.size > MAX_COMPOSER_IMAGE_BYTES) {
      addToast('warning', i18n.t('input.imageTooLarge', {
        size: (image.size / 1024 / 1024).toFixed(1),
      }));
      return false;
    }
    if (!image.dataUrl.startsWith('data:image/')) {
      addToast('error', i18n.t('input.imageReadFailed'));
      return false;
    }
    dropSessionCommandForAttachment();
    selectedImages = [...selectedImages, {
      id: generateId(),
      dataUrl: image.dataUrl,
      name: image.name,
      size: image.size,
    }];
    return true;
  }

  function startBrowserAnnotationEditing(annotation: SelectedBrowserAnnotation): void {
    editingBrowserAnnotation = annotation;
    editingBrowserAnnotationComment = loadViewDraft(`annotation:${annotation.annotationId}`, annotation.comment, annotation.comment);
  }

  $effect(() => {
    if (editingBrowserAnnotation) saveViewDraft(`annotation:${editingBrowserAnnotation.annotationId}`, editingBrowserAnnotation.comment, editingBrowserAnnotationComment);
  });

  function closeBrowserAnnotationEditing(): void {
    if (browserAnnotationSaving) return;
    editingBrowserAnnotation = null;
    editingBrowserAnnotationComment = '';
  }

  async function saveBrowserAnnotationEditing(): Promise<void> {
    const annotation = editingBrowserAnnotation;
    const comment = editingBrowserAnnotationComment.trim();
    if (!annotation || !comment || browserAnnotationSaving) return;
    browserAnnotationSaving = true;
    try {
      const updated = await updateBrowserAnnotationComment(annotation.annotationId, comment);
      selectedBrowserAnnotations = selectedBrowserAnnotations.map((item) => (
        item.annotationId === updated.annotationId ? updated : item
      ));
      window.dispatchEvent(new CustomEvent('magi:browserAnnotationUpdated', { detail: updated }));
      clearViewDraft(`annotation:${annotation.annotationId}`);
      editingBrowserAnnotation = null;
      editingBrowserAnnotationComment = '';
      addToast('success', i18n.t('browser.annotation.updated'));
    } catch (cause) {
      addToast('error', cause instanceof Error ? cause.message : String(cause));
    } finally {
      browserAnnotationSaving = false;
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
    const html = raw
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
    // Chromium 需要末尾的空行占位才能把光标留在换行之后，否则后续输入会回到上一行。
    return raw.endsWith('\n') ? `${html}<br data-editor-tail="">` : html;
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
        if (el.hasAttribute('data-editor-tail')) return;
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

  function getEditorSelectionOffsets(): { start: number; end: number } {
    const fallback = { start: inputValue.length, end: inputValue.length };
    if (!inputTextareaEl) return fallback;
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0) return fallback;
    const range = selection.getRangeAt(0);
    if (!inputTextareaEl.contains(range.startContainer)
      || !inputTextareaEl.contains(range.endContainer)) return fallback;
    const pre = range.cloneRange();
    pre.selectNodeContents(inputTextareaEl);
    pre.setEnd(range.startContainer, range.startOffset);
    const start = pre.toString().length;
    pre.setEnd(range.endContainer, range.endOffset);
    return { start, end: pre.toString().length };
  }

  function getEditorCaretOffset(): number {
    return getEditorSelectionOffsets().end;
  }

  function insertEditorText(text: string): void {
    const value = readEditorText();
    const { start, end } = getEditorSelectionOffsets();
    pendingCaretOffset = start + text.length;
    inputValue = `${value.slice(0, start)}${text}${value.slice(end)}`;
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
  function recomputeSlashState(
    value = readEditorText(),
    cursor = getEditorCaretOffset(),
  ) {
    if (!inputTextareaEl) {
      closeSlashMenu();
      return;
    }
    const trigger = resolveSlashTrigger(value, cursor);
    if (!trigger) {
      closeSlashMenu();
      return;
    }
    slashTriggerStart = trigger.triggerStart;
    slashFilter = trigger.filter;
    if (slashHighlightIndex >= filteredSlashCommands.length) {
      slashHighlightIndex = 0;
    }
  }

  function commitSlashCommand(command: Exclude<ComposerAction, { kind: 'resource' }>) {
    if (command.kind === 'command' && command.disabledReason) {
      notifyActionDisabled(command);
      return;
    }
    const previousMode = activeModeLabel();
    noteModeReplaced(previousMode, command.name);
    selectedPluginCommand = null;
    if (command.kind === 'goal') {
      selectedGoalMode = true;
      selectedSessionCommand = null;
      selectedSkill = null;
    } else if (command.kind === 'command') {
      selectedGoalMode = false;
      selectedSessionCommand = command.id;
      selectedSkill = null;
    } else if (command.kind === 'plugin-command') {
      selectedGoalMode = false;
      selectedSessionCommand = null;
      selectedSkill = null;
      selectedPluginCommand = command.pluginCommand;
    } else {
      selectedGoalMode = false;
      selectedSessionCommand = null;
      selectedSkill = command.skill;
    }
    if (inputTextareaEl && slashTriggerStart !== null) {
      invalidateEnhanceState();
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

  function removeGoalMode() {
    selectedGoalMode = false;
    queueMicrotask(focusEditor);
  }

  function removeSessionCommand() {
    selectedSessionCommand = null;
    queueMicrotask(focusEditor);
  }

  function removeSelectedSkill() {
    selectedSkill = null;
    queueMicrotask(focusEditor);
  }

  function closeAddMenu() {
    addMenuOpen = false;
  }

  function applyAddMenuAction(action: ComposerAction) {
    if (action.kind === 'resource') {
      closeAddMenu();
      contextPickerOpen = true;
      return;
    }
    if (action.kind === 'command' && action.disabledReason) {
      notifyActionDisabled(action);
      return;
    }
    const previousMode = activeModeLabel();
    if (previousMode !== action.name) noteModeReplaced(previousMode, action.name);
    const previousPluginCommand = selectedPluginCommand;
    selectedPluginCommand = null;
    if (action.kind === 'goal') {
      selectedGoalMode = !selectedGoalMode;
      selectedSessionCommand = null;
      selectedSkill = null;
    } else if (action.kind === 'command') {
      selectedGoalMode = false;
      selectedSessionCommand = selectedSessionCommand === action.id ? null : action.id;
      selectedSkill = null;
    } else if (action.kind === 'plugin-command') {
      selectedGoalMode = false;
      selectedSessionCommand = null;
      selectedSkill = null;
      selectedPluginCommand = previousPluginCommand?.pluginCommandId === action.pluginCommand.pluginCommandId
        ? null
        : action.pluginCommand;
    } else {
      selectedGoalMode = false;
      selectedSessionCommand = null;
      selectedSkill = selectedSkill?.skillId === action.skill.skillId ? null : action.skill;
    }
    closeAddMenu();
    queueMicrotask(focusEditor);
  }

  function handleContextReferenceSelected(
    selection: { pathRef: string; displayPath: string; name: string; isDirectory: boolean },
  ) {
    addContextReference({
      kind: selection.isDirectory ? 'directory' : 'file',
      path: selection.displayPath,
      pathRef: selection.pathRef,
      sourceFolderId: composerSourceFolderForPath(currentWorkspaceId || '', selection.displayPath),
      name: selection.name,
    });
    contextPickerOpen = false;
    queueMicrotask(focusEditor);
  }

  function addContextReference(input: {
    kind: ComposerContextReferenceKind;
    path: string;
    pathRef?: string;
    sourceFolderId?: string;
    name: string;
  }): boolean {
    const next = addComposerContextReference(selectedContextReferences, input);
    if (next === selectedContextReferences) {
      if (selectedContextReferences.length >= MAX_COMPOSER_CONTEXT_REFERENCES) {
        addToast('warning', i18n.t('input.add.contextLimit', {
          max: MAX_COMPOSER_CONTEXT_REFERENCES,
        }));
      }
      return false;
    } else {
      dropSessionCommandForAttachment();
      selectedContextReferences = next;
    }
    return true;
  }

  async function handleDesktopContextDrop(event: Event): Promise<void> {
    const paths = (event as CustomEvent<{ paths?: unknown }>).detail?.paths;
    if (!Array.isArray(paths)) return;
    const dropScopeKey = currentComposerReferenceScopeKey();
    const remaining = MAX_COMPOSER_CONTEXT_REFERENCES - selectedContextReferences.length;
    if (remaining <= 0) {
      addToast('warning', i18n.t('input.add.contextLimit', {
        max: MAX_COMPOSER_CONTEXT_REFERENCES,
      }));
      return;
    }
    const droppedPaths = normalizeDesktopDropPaths(
      paths.filter((path): path is string => typeof path === 'string'),
      selectedContextReferences.map((reference) => reference.path),
    );
    for (const path of droppedPaths) {
      if (currentComposerReferenceScopeKey() !== dropScopeKey) return;
      if (selectedContextReferences.length >= MAX_COMPOSER_CONTEXT_REFERENCES) {
        addToast('warning', i18n.t('input.add.contextLimit', {
          max: MAX_COMPOSER_CONTEXT_REFERENCES,
        }));
        break;
      }
      try {
        const result: ResolvedAgentPath = await resolveAgentPath(path);
        if (currentComposerReferenceScopeKey() !== dropScopeKey) return;
        const reference = resolveDesktopDroppedPath(path, result);
        if (reference) addContextReference(reference);
      } catch (error) {
        console.warn(`[InputArea] 无法添加拖入的上下文路径(${path}):`, error);
        addToast('error', i18n.t('input.add.contextDropFailed', {
          name: path.split(/[\\/]/u).filter(Boolean).pop() || path,
        }));
      }
    }
    queueMicrotask(focusEditor);
  }

  function removeAnnotationReference(annotation: SelectedBrowserAnnotation) {
    const scope = currentComposerReferenceScopeKey();
    const epoch = composerUndoEpoch;
    selectedBrowserAnnotations = selectedBrowserAnnotations.filter((item) => item.annotationId !== annotation.annotationId);
    addToast('info', i18n.t('overlay.annotationRemoved'), undefined, { forceVisible: true, action: { label: i18n.t('overlay.undo'), run: () => {
      if (composerUndoEpoch !== epoch || currentComposerReferenceScopeKey() !== scope || selectedBrowserAnnotations.some((item) => item.annotationId === annotation.annotationId)) return;
      selectedBrowserAnnotations = [...selectedBrowserAnnotations, annotation];
    } } });
  }

  function removeContextReference(referenceId: string) {
    const removed = selectedContextReferences.find((reference) => reference.id === referenceId);
    const scope = currentComposerReferenceScopeKey();
    const epoch = composerUndoEpoch;
    selectedContextReferences = selectedContextReferences.filter((reference) => (
      reference.id !== referenceId
    ));
    if (removed) addToast('info', i18n.t('overlay.contextRemoved'), undefined, { forceVisible: true, action: { label: i18n.t('overlay.undo'), run: () => {
      if (composerUndoEpoch !== epoch || currentComposerReferenceScopeKey() !== scope || selectedContextReferences.some((reference) => reference.id === removed.id)) return;
      addContextReference(removed);
    } } });
    queueMicrotask(focusEditor);
  }

  function handleComposerInput() {
    if (isComposing) return;
    if (!inputTextareaEl) return;
    invalidateEnhanceState();
    const text = readEditorText();
    inputValue = text;
    // 原生 input 事件内立即替换 contenteditable 的文本节点会把光标重置到开头。
    // 等当前输入事件完成后，再基于稳定的 DOM/selection 同步高亮和快捷命令状态。
    queueMicrotask(() => {
      if (!inputTextareaEl || isComposing) return;
      const currentText = readEditorText();
      const offset = getEditorCaretOffset();
      const highlightedHtml = buildHighlightedHtml(currentText);
      if (inputTextareaEl.innerHTML !== highlightedHtml) {
        inputTextareaEl.innerHTML = highlightedHtml;
        setEditorCaretOffset(offset);
      }
      if (inputValue !== currentText) inputValue = currentText;
      recomputeSlashState(currentText, offset);
    });
  }

  function handleComposerSelectionChange() {
    recomputeSlashState();
  }

  function handleCompositionStart() {
    isComposing = true;
    compositionEndedAt = 0;
  }

  function handleCompositionEnd() {
    isComposing = false;
    compositionEndedAt = Date.now();
    handleComposerInput();
  }

  function selectAccessProfile(profile: AccessProfile) {
    selectedAccessProfile = profile;
    accessProfilePickerOpen = false;
    writeStoredAccessProfile(profile);
  }

  function workspaceNavigationPath(workspace: ComposerWorkspaceOption): string {
    // 路径作为主要目录展示提示，项目/会话归属由 daemon 的 workspaceId 确认。
    return workspace.rootPath.trim();
  }

  function composerWorkspaceLabel(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return i18n.t('input.workspace.unbound');
    return workspace.name || workspace.rootPath;
  }

  function composerWorkspaceTitle(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return i18n.t('input.workspace.personalHint');
    return `${i18n.t('input.workspace.title')}: ${composerWorkspaceLabel(workspace)}\n${workspace.rootPath}`;
  }

  function workspaceButtonLabel(workspace: ComposerWorkspaceOption | null): string {
    if (workspace) return composerWorkspaceLabel(workspace);
    if (isPersonalSession) return composerWorkspaceLabel(null);
    return isDraftSession ? i18n.t('input.workspace.select') : i18n.t('input.workspace.title');
  }

  function workspaceButtonTitle(workspace: ComposerWorkspaceOption | null): string {
    if (isPersonalSession) return composerWorkspaceTitle(null);
    if (isDraftSession) return composerWorkspaceTitle(workspace);
    if (!workspace) return i18n.t('input.workspace.lockedPending');
    return i18n.t('input.workspace.locked', { name: composerWorkspaceLabel(workspace) });
  }

  function selectWorkspace(workspaceId: string) {
    if (!isDraftSession || sessionInputLocked || isInteractionBlocking) return;
    if (!workspaceId) {
      workspacePickerOpen = false;
      navigateSession({ kind: 'draft', scope: 'personal' });
      return;
    }
    const workspace = composerWorkspaceState.workspaces.find((candidate) => candidate.workspaceId === workspaceId) ?? null;
    if (!workspace) return;
    workspacePickerOpen = false;
    navigateSession({
      kind: 'draft',
      scope: 'workspace',
      workspaceId: workspace.workspaceId,
      workspacePath: workspaceNavigationPath(workspace),
    });
  }

  function useExistingWorkspaceFolder(): void {
    if (!isDraftSession || sessionInputLocked || isInteractionBlocking) return;
    workspacePickerOpen = false;
    openWorkspaceFolderPicker('composer');
  }

  function createProjectFromComposer(): void {
    if (!isDraftSession || sessionInputLocked || isInteractionBlocking) return;
    closeWorkspacePicker();
    requestProjectCreation(workspaceTrigger);
  }

  function selectSourceFolder(sourceFolderId: string): void {
    if (!composerWorkspace || sessionInputLocked || isInteractionBlocking || editingQueuedMessage || editingTurn) return;
    selectComposerSourceFolder(composerWorkspace.workspaceId, currentSessionId, sourceFolderId);
    closeSourceFolderPicker();
  }

  function resolveSubmissionWorkspace(): ComposerWorkspaceOption | null {
    if (!currentWorkspaceId?.trim() && !currentWorkspacePath?.trim()) {
      return null;
    }
    return resolveComposerWorkspace(currentWorkspaceId, currentWorkspacePath);
  }

  onMount(() => {
    selectedAccessProfile = readStoredAccessProfile();
    const releasePluginComposer = registerPluginComposer({
      sessionId: () => currentSessionId?.trim() || '',
      add(images) {
        if (sessionInputLocked || sendPreparing || editingQueuedMessage || editingTurn
          || selectedImages.length + pendingImageReadCount + images.length > MAX_COMPOSER_IMAGES) return null;
        const added = images.map((image) => ({ ...image, id: generateId() }));
        dropSessionCommandForAttachment();
        selectedImages = [...selectedImages, ...added];
        return added.map((image) => image.id);
      },
      has: (id) => selectedImages.some((image) => image.id === id),
      writable: () => Boolean(currentSessionId?.trim()) && !sessionInputLocked && !sendPreparing && !editingQueuedMessage && !editingTurn
        && selectedImages.length + pendingImageReadCount < MAX_COMPOSER_IMAGES,
      textWritable: () => Boolean(currentSessionId?.trim()) && !isDraftSession && !sessionInputLocked && !sendPreparing
        && !isComposing && !isInteractionBlocking && !pendingImageReadCount && composerIsPristineForSubmissionRecovery(),
      beginRead(count) {
        if (sessionInputLocked || sendPreparing || editingQueuedMessage || editingTurn
          || selectedImages.length + pendingImageReadCount + count > MAX_COMPOSER_IMAGES) return null;
        for (let i = 0; i < count; i++) beginImageRead();
        let finished = false;
        return () => {
          if (finished) return;
          finished = true;
          for (let i = 0; i < count; i++) finishImageRead();
        };
      },
      remove(id) {
        if (sessionInputLocked || sendPreparing || editingQueuedMessage || editingTurn) return false;
        if (!selectedImages.some((image) => image.id === id)) return false;
        selectedImages = selectedImages.filter((image) => image.id !== id);
        return true;
      },
      focus: () => queueMicrotask(focusEditor),
      notify: (level, message) => addToast(level, message, undefined, { forceVisible: true }),
      async submitText(input, admit) {
        const observation = observeComposerSubmission(input.submissionId);
        try {
          const posted = await sendMessage(null, { input, admit });
          if (posted !== input.submissionId) throw new Error(i18n.t('input.pluginTextUnavailable'));
          return await observation.result;
        } finally { observation.dispose(); }
      },
    });

    function handleFillComposer(event: Event) {
      const text = (event as CustomEvent<{ text?: string }>).detail?.text;
      if (typeof text !== 'string' || !text.trim()) return;
      invalidateEnhanceState();
      pendingCaretOffset = text.length;
      inputValue = text;
      queueMicrotask(focusEditor);
    }
    function handleSetAccessProfile(event: Event) {
      const profile = (event as CustomEvent<{ profile?: unknown }>).detail?.profile;
      if (!isAccessProfile(profile)) return;
      selectAccessProfile(profile);
    }
    function handleBrowserAnnotationCreated(event: Event) {
      const annotation = (event as CustomEvent<BrowserAnnotationSnapshot>).detail;
      if (
        !annotation?.annotationId
        || !isReferenceableBrowserAnnotation(annotation.status)
      ) return;
      if (selectedBrowserAnnotations.some((item) => item.annotationId === annotation.annotationId)) return;
      if (selectedBrowserAnnotations.length >= 20) {
        addToast('warning', i18n.t('browser.annotation.limit'));
        return;
      }
      dropSessionCommandForAttachment();
      selectedBrowserAnnotations = [...selectedBrowserAnnotations, annotation];
    }
    function handleBrowserAnnotationUpdated(event: Event) {
      const annotation = (event as CustomEvent<BrowserAnnotationSnapshot>).detail;
      if (!annotation?.annotationId) return;
      selectedBrowserAnnotations = selectedBrowserAnnotations.map((item) => (
        item.annotationId === annotation.annotationId ? annotation : item
      ));
    }
    function handleBrowserNodeSelected(event: Event) {
      const selection = normalizeBrowserNodeSelection((event as CustomEvent).detail);
      const currentBrowserTab = currentBrowserTabIdentity();
      if (
        !selection
        || !currentBrowserTab
        || selection.browserSessionId !== currentBrowserTab.browserSessionId
        || selection.tabId !== currentBrowserTab.tabId
      ) return;
      const selectionKey = browserNodeSelectionKey(selection);
      if (selectedBrowserNodeSelections.some((item) => browserNodeSelectionKey(item) === selectionKey)) return;
      if (selectedBrowserNodeSelections.length >= 20) {
        addToast('warning', i18n.t('browser.nodeSelection.limit'));
        return;
      }
      dropSessionCommandForAttachment();
      selectedBrowserNodeSelections = [...selectedBrowserNodeSelections, cloneBrowserNodeSelection(selection)];
      queueMicrotask(focusEditor);
    }
    function handleBrowserNodeSelectionInvalidated(event: Event) {
      const detail = (event as CustomEvent<{
        browserSessionId?: unknown;
        tabId?: unknown;
      }>).detail;
      const browserSessionId = typeof detail?.browserSessionId === 'string' ? detail.browserSessionId.trim() : '';
      const tabId = typeof detail?.tabId === 'string' ? detail.tabId.trim() : '';
      if (!browserSessionId || !tabId) return;
      selectedBrowserNodeSelections = selectedBrowserNodeSelections.filter((selection) => (
        selection.browserSessionId !== browserSessionId || selection.tabId !== tabId
      ));
    }
    function handleBrowserScreenshotCaptured(event: Event) {
      const detail = (event as CustomEvent<{
        dataUrl?: unknown;
        name?: unknown;
        size?: unknown;
      }>).detail;
      if (
        typeof detail?.dataUrl !== 'string'
        || typeof detail.name !== 'string'
        || typeof detail.size !== 'number'
      ) return;
      if (selectedImages.length + pendingImageReadCount >= MAX_COMPOSER_IMAGES) {
        addToast('warning', i18n.t('input.maxImages', { max: MAX_COMPOSER_IMAGES }));
        return;
      }
      if (addImageToComposer({ dataUrl: detail.dataUrl, name: detail.name, size: detail.size })) {
        queueMicrotask(focusEditor);
      }
    }
    function handleSessionTurnSubmissionSettled(event: Event) {
      const detail = (event as CustomEvent<{ requestId?: unknown; status?: unknown }>).detail;
      const requestId = typeof detail?.requestId === 'string' ? detail.requestId.trim() : '';
      if (!requestId) return;
      const draft = submittedComposerDrafts.get(requestId);
      submittedComposerDrafts.delete(requestId);
      if (detail.status !== 'failed' || !draft || !composerIsPristineForSubmissionRecovery()) return;
      restoreComposerSubmissionDraft(draft);
    }
    function handlePickerOutsidePointerDown(event: PointerEvent) {
      const target = event.target;
      if (sourceFolderPickerOpen && !(target instanceof Element && target.closest('.ia-source-folder-wrap'))) closeSourceFolderPicker(false);
      if (workspacePickerOpen && !(target instanceof Element && target.closest('.ia-workspace-wrap'))) {
        workspacePickerOpen = false;
      }
      if (accessProfilePickerOpen && !(target instanceof Element && target.closest('.ia-access-wrap'))) {
        accessProfilePickerOpen = false;
      }
      if (addMenuOpen && !(target instanceof Element && target.closest('.ia-add-wrap'))) {
        addMenuOpen = false;
      }
      if (pickerOpen && !(target instanceof Element && target.closest('.ia-model-wrap'))) {
        pickerOpen = false;
      }
      if (slashMenuOpen && !(target instanceof Element && target.closest('.ia-textarea, .ia-slash-popover'))) {
        closeSlashMenu();
      }
    }
    function handleStoredAccessProfileChange(event: StorageEvent) {
      if (event.key !== ACCESS_PROFILE_STORAGE_KEY) return;
      const nextProfile = readStoredAccessProfile();
      if (nextProfile === selectedAccessProfile) return;
      selectedAccessProfile = nextProfile;
      void getAgentSettingsBootstrap({ bootstrapScope: 'core', accessProfile: nextProfile })
        .then((latest) => {
          if (settingsBootstrapMatchesCurrentWorkspace(latest)) {
            messagesState.settingsBootstrapSnapshot = latest;
          }
        })
        .catch((error) => {
          console.warn('[InputArea] 跨窗口同步访问模式后刷新设置快照失败:', error);
        });
    }
    window.addEventListener('magi:fillComposer', handleFillComposer as EventListener);
    window.addEventListener('magi:setAccessProfile', handleSetAccessProfile as EventListener);
    window.addEventListener('magi:browserAnnotationCreated', handleBrowserAnnotationCreated as EventListener);
    window.addEventListener('magi:browserAnnotationUpdated', handleBrowserAnnotationUpdated as EventListener);
    window.addEventListener('magi:browserNodeSelected', handleBrowserNodeSelected as EventListener);
    window.addEventListener('magi:browserNodeSelectionInvalidated', handleBrowserNodeSelectionInvalidated as EventListener);
    window.addEventListener('magi:browserScreenshotCaptured', handleBrowserScreenshotCaptured as EventListener);
    window.addEventListener('magi:sessionTurnSubmissionSettled', handleSessionTurnSubmissionSettled as EventListener);
    window.addEventListener(DESKTOP_CONTEXT_DROP_EVENT, handleDesktopContextDrop as EventListener);
    window.addEventListener('storage', handleStoredAccessProfileChange);
    document.addEventListener('pointerdown', handlePickerOutsidePointerDown, true);
    return () => {
      releasePluginComposer();
      setComposerHasUnsavedInput(false);
      window.removeEventListener('magi:fillComposer', handleFillComposer as EventListener);
      window.removeEventListener('magi:setAccessProfile', handleSetAccessProfile as EventListener);
      window.removeEventListener('magi:browserAnnotationCreated', handleBrowserAnnotationCreated as EventListener);
      window.removeEventListener('magi:browserAnnotationUpdated', handleBrowserAnnotationUpdated as EventListener);
      window.removeEventListener('magi:browserNodeSelected', handleBrowserNodeSelected as EventListener);
      window.removeEventListener('magi:browserNodeSelectionInvalidated', handleBrowserNodeSelectionInvalidated as EventListener);
      window.removeEventListener('magi:browserScreenshotCaptured', handleBrowserScreenshotCaptured as EventListener);
      window.removeEventListener('magi:sessionTurnSubmissionSettled', handleSessionTurnSubmissionSettled as EventListener);
      window.removeEventListener(DESKTOP_CONTEXT_DROP_EVENT, handleDesktopContextDrop as EventListener);
      window.removeEventListener('storage', handleStoredAccessProfileChange);
      document.removeEventListener('pointerdown', handlePickerOutsidePointerDown, true);
    };
  });

  $effect(() => {
    void currentSessionId; void selectedImages; void pendingImageReadCount;
    void sessionInputLocked; void sendPreparing; void editingQueuedMessage; void editingTurn;
    void inputValue; void selectedContextReferences; void selectedBrowserAnnotations; void selectedBrowserNodeSelections;
    void selectedGoalMode; void selectedSkill; void selectedSessionCommand; void selectedPluginCommand;
    void isComposing; void isInteractionBlocking;
    notifyPluginComposerChanged();
  });

  $effect(() => {
    const orchestratorConfig = getOrchestratorConfigSnapshot();
    const configKey = orchestratorModelListConfigKey(orchestratorConfig);
    if (!configKey) {
      pickerModelGroups = [];
      pickerCapabilities = {};
      pickerModelsConfigKey = '';
      pickerLoadedOnce = false;
      pickerError = null;
      return;
    }
    if (pickerModelsConfigKey && pickerModelsConfigKey !== configKey) {
      pickerModelGroups = [];
      pickerCapabilities = {};
      pickerModelsConfigKey = '';
      pickerLoadedOnce = false;
      pickerError = null;
    }
    const pluginRevision = pluginRegistryRevision();
    if (pickerLoadedOnce && pickerPluginRevision !== pluginRevision) pickerLoadedOnce = false;
    if (!pickerLoadedOnce && !pickerLoading) {
      void loadPickerModels();
    }
  });

  // 新建会话/切换会话后，URL 与 session store 会先变更，settings bootstrap 可能仍是
  // 旧 session 绑定。输入区的模型按钮依赖 session 级有效配置，必须在绑定不匹配时刷新。
  $effect(() => {
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const sessionId = currentSessionId?.trim() || '';
    const scope = workspaceId || workspacePath ? 'workspace' : 'personal';
    const refreshKey = `${scope}|${workspaceId}|${workspacePath}|${sessionId}|${selectedAccessProfile}`;
    if (
      messagesState.settingsBootstrapSnapshot
      && settingsBootstrapMatchesCurrentWorkspace(messagesState.settingsBootstrapSnapshot)
    ) {
      settingsBootstrapRefreshKey = refreshKey;
      return;
    }
    if (settingsBootstrapRefreshKey === refreshKey) return;
    settingsBootstrapRefreshKey = refreshKey;
    const seq = ++settingsBootstrapRefreshSeq;
    getAgentSettingsBootstrap({ bootstrapScope: 'core', accessProfile: selectedAccessProfile })
      .then((latest) => {
        if (seq !== settingsBootstrapRefreshSeq) return;
        if (!settingsBootstrapMatchesCurrentWorkspace(latest)) return;
        messagesState.settingsBootstrapSnapshot = latest;
      })
      .catch((error) => {
        console.warn('[InputArea] 会话绑定变化后刷新设置快照失败:', error);
      });
  });

  function resolveComposerRawContent(): string {
    if (inputTextareaEl) {
      return extractEditorText(inputTextareaEl);
    }
    return inputValue;
  }

  // 发送消息（支持图片附件）。
  // daemon 持久接纳投递意图；替代输入等待原执行结算后才启动。
  async function sendMessage(interruptTurnId: string | null = null, plugin?: { input: PluginComposerTextInput; admit(): Promise<void> }) {
    if (sendPreparing) return;
    sendPreparing = true;
    try {
      await waitForPendingImageReads();
      if (plugin) {
        await plugin.admit();
        if (currentSessionId !== plugin.input.sessionId || isDraftSession || isComposing
          || sessionInputLocked || isInteractionBlocking || pendingImageReadCount) throw new Error(i18n.t('input.pluginTextUnavailable'));
        if (!composerIsPristineForSubmissionRecovery()) throw new Error(i18n.t('input.pluginTextDraftConflict'));
      }
      const rawContent = plugin ? plugin.input.text : resolveComposerRawContent();
      const normalizedContent = rawContent.trim();
      if (editingQueuedMessage) { await saveQueuedMessage(rawContent); return; }
      // 配置期间普通问题、补充方案和暂停说明都进入主对话；只有面板明确操作才提交控制动作。


      if (
        (!normalizedContent && selectedSessionCommand === null && selectedPluginCommand === null && selectedImages.length === 0 && selectedContextReferences.length === 0 && selectedBrowserAnnotations.length === 0 && selectedBrowserNodeSelections.length === 0)
        || sessionInputLocked
        || (isInteractionBlocking && !interruptTurnId)
        || (isDraftSession && isSending)
      ) return;
      if (selectedSessionCommand !== null && attachmentTotal > 0) {
        addToast('warning', i18n.t('input.compact.disabled.attachments'));
        return;
      }
      const submissionText = normalizedContent
        ? rawContent
        : selectedImages.length > 0
          ? i18n.t('input.analyzeImages')
          : selectedContextReferences.length > 0
            ? i18n.t('input.analyzeReferences')
            : selectedBrowserAnnotations.length > 0
              ? i18n.t('browser.annotation.context')
            : selectedBrowserNodeSelections.length > 0
              ? i18n.t('browser.nodeSelection.context')
            : null;
      const submissionLength = submissionText?.length ?? 0;

      if (submissionLength > MAX_INPUT_CHARS) {
        addToast('warning', i18n.t('input.inputTooLong', { length: submissionLength, max: MAX_INPUT_CHARS }));
        return;
      }

      const targetWorkspace = resolveSubmissionWorkspace();
      if (targetWorkspace?.sourceFolders?.length && !selectedSourceFolder) {
        addToast('warning', i18n.t('input.sourceFolderUnavailable'));
        return;
      }

      const orchestratorSessionConfig = resolveTurnOrchestratorSessionConfigPayload();
      if (!orchestratorSessionConfig) {
        return;
      }

      const requestId = plugin?.input.submissionId ?? generateId();
      const replaceTurnId = editingTurn?.sessionId === messagesState.currentSessionId
        ? editingTurn.turnId
        : null;
      if (interruptTurnId && (replaceTurnId || selectedSessionCommand || selectedPluginCommand)) return;
      if (!replaceTurnId && !plugin) {
        submittedComposerDrafts.set(requestId, captureComposerSubmissionDraft(rawContent));
      }
      vscode.postMessage({
        type: 'executeTask',
        sourceFolderId: targetWorkspace ? selectedSourceFolderId || undefined : undefined,
        text: submissionText,
        requestId,
        ...(plugin ? { userMessageId: `plugin-input-${plugin.input.submissionId}` } : {}),
        workspaceId: targetWorkspace?.workspaceId || '',
        workspacePath: targetWorkspace ? workspaceNavigationPath(targetWorkspace) : '',
        sessionId: isDraftSession ? '' : (messagesState.currentSessionId || ''),
        skillName: selectedSkill?.skillId ?? null,
        goalMode: selectedGoalMode,
        command: selectedSessionCommand,
        pluginCommandId: selectedPluginCommand?.pluginCommandId ?? null,
        accessProfile: selectedAccessProfile,
        ...(isDraftSession ? { orchestratorSessionConfig } : {}),
        replaceTurnId,
        interruptTurnId,
        images: selectedImages.map((img) => ({
          name: img.name,
          dataUrl: img.dataUrl,
        })),
        contextReferences: toSessionContextReferencePayload(selectedContextReferences),
        browserAnnotationRefs: selectedBrowserAnnotations.map((annotation) => annotation.annotationId),
        browserNodeSelections: selectedBrowserNodeSelections.map(cloneBrowserNodeSelection),
      });
      if (!replaceTurnId && !plugin) {
        clearComposerState();
      }
      return requestId;
    } finally {
      sendPreparing = false;
    }
  }

  function insertNewlineAtCursor() {
    invalidateEnhanceState();
    insertEditorText('\n');
  }

  function isEnterKey(event: KeyboardEvent): boolean {
    return event.key === 'Enter' || event.code === 'Enter' || event.code === 'NumpadEnter';
  }

  function isNewlineShortcut(event: KeyboardEvent): boolean {
    if (!isEnterKey(event) || event.metaKey || event.ctrlKey) {
      return false;
    }
    // Shift+Enter 是统一主快捷键；保留既有的 Option/Alt+Enter，避免 macOS 用户升级后失去原有操作。
    return (event.shiftKey && !event.altKey) || (event.altKey && !event.shiftKey);
  }

  // 处理键盘事件
  function handleKeydown(event: KeyboardEvent) {
    // IME 组合态优先于快捷键。
    if (isComposing || event.isComposing || event.keyCode === 229) return;
    if (isNewlineShortcut(event)) {
      event.preventDefault();
      closeSlashMenu();
      insertNewlineAtCursor();
      return;
    }
    if (isEnterKey(event) && compositionEndedAt > 0 && Date.now() - compositionEndedAt < 500) {
      event.preventDefault();
      return;
    }
    if (slashMenuOpen) {
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        slashHighlightIndex = (slashHighlightIndex + 1) % filteredSlashCommands.length;
        return;
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault();
        slashHighlightIndex = (slashHighlightIndex - 1 + filteredSlashCommands.length) % filteredSlashCommands.length;
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        closeSlashMenu();
        return;
      }
      if (event.key === 'Tab' || isEnterKey(event)) {
        event.preventDefault();
        const chosen = filteredSlashCommands[slashHighlightIndex] ?? filteredSlashCommands[0];
        if (chosen) commitSlashCommand(chosen);
        return;
      }
    }
    if (isEnterKey(event)) {
      if (event.metaKey || event.ctrlKey || event.shiftKey) {
        event.preventDefault();
        return;
      }
      event.preventDefault();
      sendMessage();
      return;
    }
  }

  // 代理运行运行时，输入框停止入口与目标面板共用同一条可恢复中断链路。
  function stopTask() {
    if (stopLoading || isDraftSession) return;
    vscode.postMessage({ type: 'interruptTask' });
  }

  function deleteQueuedMessage(queuedMessageId: string, expectedRevision?: number) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId || typeof expectedRevision !== 'number' || !Number.isSafeInteger(expectedRevision) || expectedRevision < 1) return;
    vscode.postMessage({
      type: 'removeQueuedMessage',
      queuedMessageId: normalizedId,
      expectedRevision,
    });
  }

  function guideQueuedMessageTitle(queued: QueuedMessage): string {
    switch (queued.guide?.status) {
      case 'ready': return i18n.t('input.queue.guideTitle');
      case 'no_active_turn': return i18n.t('input.queue.guideNoActiveTurn');
      case 'input_closed': return i18n.t('input.queue.guideInputClosed');
      case 'unsupported_input': return i18n.t('input.queue.guideUnsupportedInput');
      case 'unsupported_executor': return i18n.t('input.queue.guideUnsupportedExecutor');
      default: return i18n.t('input.queue.guideUnavailable');
    }
  }

  function guideQueuedMessage(queued: QueuedMessage) {
    if (queued.guide?.status !== 'ready' || !queued.guide.targetTurnId) return;
    vscode.postMessage({
      type: 'guideQueuedMessage',
      queuedMessageId: queued.id,
      expectedTurnId: queued.guide.targetTurnId,
      expectedRevision: queued.revision,
    });
  }

  // 草稿只保存视图编辑状态；原 Input 一直留在 daemon，保存时以原 revision 竞争领取。
  function editQueuedMessage(queuedMessageId: string) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId || queueEditPending) return;
    const target = messagesState.queuedMessages.find((message) => message.id === normalizedId);
    if (!target || typeof target.revision !== 'number' || target.revision < 1) return;
    if (!queuedMessageEditable(target)) {
      addToast('info', i18n.t('input.queue.editBlocked'), undefined, { forceVisible: true });
      return;
    }
    if (composerHasDraft()) {
      addToast('warning', i18n.t('input.queue.editNeedsEmptyComposer'));
      return;
    }
    let references: ComposerContextReference[] = [];
    for (const reference of target.contextReferences ?? []) {
      references = addComposerContextReference(references, reference);
    }
    const skillId = target.skillName?.trim() ?? '';
    const restoredSkill = skillId
      ? availableSkills.find((skill) => skill.skillId === skillId)
        ?? { skillId, name: skillId, description: '' }
      : null;
    const draft: ComposerSubmissionDraft = {
      accessProfile: isAccessProfile(target.accessProfile) ? target.accessProfile : selectedAccessProfile,
      queuedEdit: { queueId: target.id, revision: target.revision, sessionId: messagesState.currentSessionId || '',
        sourceFolderId: target.sourceFolderId,
        workspaceId: target.workspaceId || '', workspacePath: target.workspacePath || '' },
      text: (target.text ?? target.content ?? '').toString(),
      sourceFolderId: target.sourceFolderId,
      images: (target.images ?? []).map((image) => ({
        id: generateId(),
        dataUrl: image.dataUrl,
        name: image.name,
        size: Math.floor((image.dataUrl.length * 3) / 4),
      })),
      contextReferences: references,
      browserAnnotations: [],
      browserNodeSelections: (target.browserNodeSelections ?? []).map(cloneBrowserNodeSelection),
      goalMode: target.goalMode === true,
      sessionCommand: target.command === 'compact' ? 'compact' : null,
      skill: restoredSkill,
      pluginCommand: target.pluginCommandId
        ? availablePluginCommands.find((command) => command.pluginCommandId === target.pluginCommandId)
          ?? { pluginCommandId: target.pluginCommandId, name: target.pluginCommandId, description: '' }
        : null,
    };
    restoreComposerSubmissionDraft(draft);
  }

  async function saveQueuedMessage(text: string): Promise<void> {
    const target = editingQueuedMessage;
    if (!target || target.sessionId !== messagesState.currentSessionId || sessionInputLocked) return;
    if (text.length > MAX_INPUT_CHARS) {
      addToast('warning', i18n.t('input.inputTooLong', { length: text.length, max: MAX_INPUT_CHARS }));
      return;
    }
    const originScope = currentComposerReferenceScopeKey();
    const editRequestId = generateId();
    queueEditPending = true;
    queueEditConflict = '';
    let onUpdated: (event: Event) => void = () => {};
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const result = await new Promise<{ updated: boolean; errorMessage?: string }>((resolve) => {
        onUpdated = (event: Event) => {
          const detail = (event as CustomEvent<{ requestId: string; sessionId: string; updated: boolean; errorMessage?: string }>).detail;
          if (detail?.requestId === editRequestId && detail.sessionId === target.sessionId) {
            resolve({ updated: detail.updated === true, errorMessage: detail.errorMessage });
          }
        };
        window.addEventListener('magi:queuedMessageUpdated', onUpdated);
        timer = setTimeout(() => {
          resolve({ updated: false, errorMessage: i18n.t('input.queue.saveUnknown') });
        }, 120_000);
        vscode.postMessage({ type: 'updateQueuedMessage', queuedMessageId: target.queueId,
          expectedRevision: target.revision, editRequestId, request: {
            scope: target.workspaceId || target.workspacePath ? 'workspace' : 'personal',
            sessionId: target.sessionId, workspaceId: target.workspaceId || null, workspacePath: target.workspacePath || null,
            sourceFolderId: target.sourceFolderId || null,
            text, skillName: selectedSkill?.skillId ?? null, goalMode: selectedGoalMode, command: selectedSessionCommand,
            pluginCommandId: selectedPluginCommand?.pluginCommandId ?? null,
            accessProfile: selectedAccessProfile, locale: i18n.locale,
            images: selectedImages.map(image => ({ name: image.name, dataUrl: image.dataUrl })),
            contextReferences: toSessionContextReferencePayload(selectedContextReferences),
            browserAnnotationRefs: selectedBrowserAnnotations.map(annotation => annotation.annotationId),
            browserNodeSelections: selectedBrowserNodeSelections.map(cloneBrowserNodeSelection),
          } });
      });
      if (!result.updated) {
        if (currentComposerReferenceScopeKey() === originScope) {
          queueEditConflict = result.errorMessage || i18n.t('input.queue.saveConflict');
        }
        return;
      }
      if (currentComposerReferenceScopeKey() === originScope) clearComposerState();
      else scopedComposerDrafts.delete(originScope);
    } finally {
      if (timer) clearTimeout(timer);
      window.removeEventListener('magi:queuedMessageUpdated', onUpdated);
      queueEditPending = false;
    }
  }

  // 🔧 处理粘贴事件（支持图片粘贴 + 纯文本插入）
  function handlePaste(event: ClipboardEvent) {
    const imageFiles = collectClipboardImageFiles(event.clipboardData);
    if (imageFiles.length > 0) {
      event.preventDefault();
      addImageFiles(imageFiles);
      return;
    }

    // 纯文本路径：阻止浏览器把 HTML 粘进 contenteditable，统一按 \n 文本插入
    const text = event.clipboardData?.getData('text/plain');
    if (typeof text !== 'string' || text.length === 0) return;
    event.preventDefault();
    invalidateEnhanceState();
    insertEditorText(text);
  }

  // 拖入图片：浏览器端直接作为图片附件。桌面端的拖入由原生通道按「文件引用」处理，
  // 这里不拦截，避免同一次拖放被两条路径各处理一遍。
  let imageDragActive = $state(false);

  function dragCarriesFiles(event: DragEvent): boolean {
    return !isDesktopRuntime() && Array.from(event.dataTransfer?.types ?? []).includes('Files');
  }

  function handleImageDragOver(event: DragEvent) {
    if (!dragCarriesFiles(event) || sessionInputLocked || isInteractionBlocking) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
    imageDragActive = true;
  }

  function handleImageDragLeave(event: DragEvent) {
    const related = event.relatedTarget;
    if (related instanceof Node && (event.currentTarget as Node | null)?.contains(related)) return;
    imageDragActive = false;
  }

  function handleImageDrop(event: DragEvent) {
    imageDragActive = false;
    if (!dragCarriesFiles(event) || sessionInputLocked || isInteractionBlocking) return;
    event.preventDefault();
    addImageFiles(Array.from(event.dataTransfer?.files ?? []));
    queueMicrotask(focusEditor);
  }

  // 🔧 删除已选图片
  function removeImage(imageId: string) {
    const removed = selectedImages.find((image) => image.id === imageId);
    const scope = currentComposerReferenceScopeKey();
    const epoch = composerUndoEpoch;
    selectedImages = selectedImages.filter(img => img.id !== imageId);
    if (removed) addToast('info', i18n.t('overlay.imageRemoved'), undefined, { forceVisible: true, action: { label: i18n.t('overlay.undo'), run: () => {
      if (composerUndoEpoch !== epoch || currentComposerReferenceScopeKey() !== scope || selectedImages.some((image) => image.id === removed.id)) return;
      if (selectedImages.length + pendingImageReadCount >= MAX_COMPOSER_IMAGES) { addToast('warning', i18n.t('input.maxImages', { max: MAX_COMPOSER_IMAGES })); return; }
      dropSessionCommandForAttachment();
      selectedImages = [...selectedImages, removed];
    } } });
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

  function getCurrentOrchestratorSessionConfigSnapshot(): Record<string, unknown> {
    if (isDraftSession) {
      return messagesState.draftOrchestratorSessionConfig;
    }
    return getOrchestratorSessionConfigSnapshot();
  }

  function buildTurnOrchestratorSessionConfigPayload(model: string): Record<string, unknown> {
    const config = getCurrentOrchestratorSessionConfigSnapshot();
    return withOrchestratorReasoningEffort(
      config,
      readOrchestratorReasoningEffort(),
      { model },
    );
  }

  function resolveTurnOrchestratorSessionConfigPayload(): Record<string, unknown> | null {
    const model = readOrchestratorModel();
    if (!model || !readOrchestratorSourceId()) {
      pickerOpen = true;
      pickerView = 'models';
      if (!pickerLoadedOnce && !pickerLoading) {
        void loadPickerModels();
      }
      addToast('warning', i18n.t('input.mainModelRequired'));
      return null;
    }
    const nextConfig = buildTurnOrchestratorSessionConfigPayload(model);
    if (isDraftSession) {
      messagesState.draftOrchestratorSessionConfig = nextConfig;
    }
    return nextConfig;
  }

  function readOrchestratorModel(): string {
    return resolveOrchestratorModel(
      getCurrentOrchestratorSessionConfigSnapshot(),
      getEffectiveOrchestratorConfigSnapshot(),
    );
  }

  function readOrchestratorSourceId(): string {
    const selected = getCurrentOrchestratorSessionConfigSnapshot().sourceId;
    if (typeof selected === 'string' && selected.trim()) return selected.trim();
    const effective = getEffectiveOrchestratorConfigSnapshot()?.sourceId;
    return typeof effective === 'string' ? effective.trim() : '';
  }

  function readOrchestratorReasoningEffort(): ReasoningEffort | null {
    return resolveOrchestratorReasoningEffort(
      getCurrentOrchestratorSessionConfigSnapshot(),
      getEffectiveOrchestratorConfigSnapshot(),
    );
  }

  function reasoningEffortLabel(value: ReasoningEffort | null): string {
    return value ? i18n.t(`input.mainModelPicker.reasoning.${value}`) : '';
  }

  function objectRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {};
  }

  // 主对话目录由 daemon 合并“用户数据源”和“插件在本次运行中贡献的数据源”。
  // key 只描述用户数据源：它变了，已拉取的列表属于旧配置，必须清空重拉。
  // 没有用户数据源不等于没有可选模型（插件可以单独提供），所以空数组同样发起请求；
  // 只有设置快照尚未就绪（config 为空）时才不请求。插件侧变化见 pickerPluginRevision。
  function orchestratorModelListConfigKey(config: Record<string, unknown> | null): string {
    if (!config) return '';
    const sources = Array.isArray(config.sources) ? config.sources : [];
    return JSON.stringify({ revision: Number(config.revision) || 0, sources });
  }

  function applyDraftOrchestratorSessionPatch(patch: Record<string, unknown>) {
    messagesState.draftOrchestratorSessionConfig = {
      ...messagesState.draftOrchestratorSessionConfig,
      ...patch,
    };
  }

  function getAuxiliaryConfigSnapshot(): Record<string, unknown> | null {
    const snapshot = messagesState.settingsBootstrapSnapshot;
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
    const model = typeof config.model === 'string' ? config.model.trim() : '';
    return Boolean(baseUrl && model);
  }

  function clearEnhanceSnapshot() {
    enhanceOriginalPrompt = null;
    enhanceResultPrompt = null;
  }

  function invalidateEnhanceState() {
    enhanceRequestSeq += 1;
    enhanceAbortController?.abort();
    enhanceAbortController = null;
    clearEnhanceSnapshot();
  }

  function applyLocalOrchestratorSessionConfig(
    sessionConfig: Record<string, unknown>,
    effectiveConfig: Record<string, unknown>,
    sessionId: string,
    capability: ModelReasoningCapability | null,
  ) {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (currentSessionId?.trim() !== sessionId || !snapshot || !settingsBootstrapMatchesCurrentWorkspace(snapshot)) return;
    messagesState.settingsBootstrapSnapshot = {
      ...snapshot,
      orchestratorReasoningCapability: capability,
      orchestratorModelError: undefined,
      orchestratorSessionDefaults: { ...sessionConfig },
      orchestratorSessionConfig: { ...sessionConfig },
      effectiveOrchestratorConfig: { ...effectiveConfig },
    } as AgentSettingsBootstrapSnapshot;
  }

  function applyOrchestratorSessionDefaults(defaults: Record<string, unknown>) {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (snapshot && settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      const currentSessionConfig = objectRecord(snapshot.orchestratorSessionConfig);
      const currentModel = resolveOrchestratorModel(currentSessionConfig, undefined);
      const shouldInitialize = !currentModel && (isDraftSession || !currentSessionConfig.model);
      const resolvedSessionConfig = { ...currentSessionConfig, ...defaults };
      messagesState.settingsBootstrapSnapshot = {
        ...snapshot,
        orchestratorSessionDefaults: { ...defaults },
        ...(shouldInitialize ? {
          orchestratorSessionConfig: { ...resolvedSessionConfig },
          effectiveOrchestratorConfig: {
            ...snapshot.orchestratorConfig,
            ...resolvedSessionConfig,
          },
        } : {}),
      } as AgentSettingsBootstrapSnapshot;
    }
    if (isDraftSession) {
      const draftConfig = messagesState.draftOrchestratorSessionConfig;
      const draftModel = resolveOrchestratorModel(draftConfig, undefined);
      messagesState.draftOrchestratorSessionConfig = draftModel
        ? { ...defaults, ...draftConfig }
        : { ...draftConfig, ...defaults };
    }
  }

  // 主线模型 picker：打开 / 关闭 + 模型列表惰性拉取。
  // 模型列表读取全局 orchestrator 连接配置；保存只写当前会话覆盖段。
  async function togglePicker() {
    if (pickerOpen) {
      pickerOpen = false;
      return;
    }
    pickerOpen = true;
    pickerView = currentPickerModel ? 'effort' : 'models';
    if (!pickerLoadedOnce && !pickerLoading) {
      await loadPickerModels();
    } else {
      await refreshPickerModels();
    }
  }

  async function loadPickerModels() {
    if (pickerLoadPromise) {
      await pickerLoadPromise;
      return;
    }
    const loadPromise = (async () => {
      const orchestratorConfig = getOrchestratorConfigSnapshot();
      const configKey = orchestratorModelListConfigKey(orchestratorConfig);
      if (pickerLoadedOnce && pickerModelsConfigKey === configKey && pickerModelCount > 0) {
        return;
      }
      const pluginRevisionAtLoad = pluginRegistryRevision();
      pickerLoading = true;
      pickerError = null;
      try {
        if (!orchestratorConfig) throw new Error(i18n.t('input.modelListLoadFailed'));
        const payload = await getAgentMainModelCatalog();
        if (orchestratorModelListConfigKey(getOrchestratorConfigSnapshot()) !== configKey) return;
        pickerModelGroups = Array.isArray(payload.groups) ? payload.groups : [];
        pickerCapabilities = Object.fromEntries(pickerModelGroups.flatMap((group) => group.models.map((model) => [
          `${group.sourceId}\u001f${model.id}`, model.reasoningCapability,
        ])));
        const defaults = objectRecord(messagesState.settingsBootstrapSnapshot?.orchestratorSessionDefaults);
        if (!String(defaults.sourceId ?? '').trim()) {
          const firstGroup = pickerModelGroups.find((group) => group.models.length > 0);
          const firstModel = firstGroup?.models[0];
          if (firstGroup && firstModel) applyOrchestratorSessionDefaults({ sourceId: firstGroup.sourceId, model: firstModel.id, reasoningEffort: null });
        }
        const selectedSourceId = readOrchestratorSourceId();
        const selectedModel = readOrchestratorModel();
        const selectedModelIsAvailable = pickerModelGroups.some((group) =>
          group.sourceId === selectedSourceId && group.models.some((model) => model.id === selectedModel));
        const snapshot = messagesState.settingsBootstrapSnapshot;
        if (selectedModelIsAvailable && snapshot && settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
          messagesState.settingsBootstrapSnapshot = { ...snapshot, orchestratorModelError: undefined };
        }
        pickerModelsConfigKey = configKey;
        pickerPluginRevision = pluginRevisionAtLoad;
        pickerLoadedOnce = true;
      } catch (error) {
        if (orchestratorModelListConfigKey(getOrchestratorConfigSnapshot()) !== configKey) return;
        pickerModelsConfigKey = configKey;
        pickerPluginRevision = pluginRevisionAtLoad;
        pickerLoadedOnce = true;
        console.warn('[InputArea] 拉取主线模型列表失败:', error);
        pickerError = i18n.t('input.modelListLoadFailed');
      } finally {
        pickerLoading = false;
      }
    })();
    pickerLoadPromise = loadPromise;
    try {
      await loadPromise;
    } finally {
      if (pickerLoadPromise === loadPromise) {
        pickerLoadPromise = null;
      }
    }
  }
  async function refreshPickerModels() {
    if (pickerLoading) {
      const pending = pickerLoadPromise;
      if (pending) await pending;
      if (pickerLoading) return;
    }
    pickerLoadedOnce = false;
    await loadPickerModels();
  }
  async function selectPickerModel(model: string, sourceId: string) {
    if (pickerSavingModel || pickerSavingReasoning) return;
    const normalizedModel = model.trim();
    if (!normalizedModel) return;
    if (normalizedModel === currentPickerModel && sourceId === currentPickerSourceId) {
      pickerView = 'effort';
      return;
    }
    const sessionId = currentSessionId?.trim() || '';
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const binding: AgentBindingOverride = workspaceId || workspacePath
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
    const targetCapability = pickerCapabilities[`${sourceId}\u001f${normalizedModel}`];
    const oldEffort = readOrchestratorReasoningEffort();
    const reasoningEffort = targetCapability?.supportedEfforts.includes(oldEffort!)
      ? oldEffort : null;
    const adjusted = oldEffort !== null && reasoningEffort === null;
    if (!sessionId) {
      messagesState.draftOrchestratorSessionConfig = withOrchestratorReasoningEffort(
        messagesState.draftOrchestratorSessionConfig,
        reasoningEffort,
        { sourceId, model: normalizedModel },
      );
      pickerError = null;
      pickerView = 'effort';
      reasoningAdjusted = adjusted;
      return;
    }
    pickerSavingModel = `${sourceId}\u001f${normalizedModel}`;
    pickerError = null;
    const nextSessionConfig = withOrchestratorReasoningEffort(
      getOrchestratorSessionConfigSnapshot(),
      reasoningEffort,
      { sourceId, model: normalizedModel },
    );
    try {
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, binding);
      applyLocalOrchestratorSessionConfig(
        objectRecord(saved.orchestratorSessionConfig),
        objectRecord(saved.effectiveOrchestratorConfig),
        sessionId,
        (saved.orchestratorReasoningCapability ?? null) as ModelReasoningCapability | null,
      );
      try {
        await refreshPickerSettingsSnapshot();
      } catch (error) {
        console.warn('[InputArea] 切换主线模型后刷新设置快照失败:', error);
        addToast('warning', i18n.t('input.modelSavedSyncPending'));
      }
      pickerView = 'effort';
      reasoningAdjusted = adjusted;
    } catch (error) {
      console.warn('[InputArea] 保存主线模型失败:', error);
      pickerError = i18n.t('input.modelSaveFailed');
      addToast('error', pickerError);
    } finally {
      pickerSavingModel = null;
    }
  }

  /** 草稿会话先落成真实会话并切换过去；已经是真实会话则原样返回它的 ID。 */
  async function materializeDraftSession(): Promise<string> {
    const existing = currentSessionId?.trim() || '';
    if (existing) return existing;
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const materialized = await materializeSession(
      workspaceId || null,
      workspaceId ? workspacePath : undefined,
    );
    const sessionId = materialized.sessionId;
    const navigation = navigateSession(workspaceId
      ? { kind: 'session', scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { kind: 'session', scope: 'personal', sessionId });
    if (!navigation) throw new Error(i18n.t('web.workbenchActionFailed', { action: i18n.t('header.newSession') }));
    await waitForSessionNavigation(navigation);
    return sessionId;
  }

  async function selectPickerReasoningEffort(value: ReasoningEffort | null) {
    if (pickerSavingReasoning || pickerSavingModel || (value !== null && !reasoningOptions.includes(value))) return;
    reasoningAdjusted = false;
    const focusedSlider = document.activeElement instanceof HTMLInputElement
      && document.activeElement.matches('.ia-reasoning-slider input') ? document.activeElement : null;
    const sessionId = currentSessionId?.trim() || '';
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const binding: AgentBindingOverride = workspaceId || workspacePath
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
    if (!sessionId) {
      applyDraftOrchestratorSessionPatch({ reasoningEffort: value });
      sliderDraft = null;
      pickerError = null;
      return;
    }
    if (value === currentPickerReasoningEffort) {
      sliderDraft = null;
      return;
    }
    pickerSavingReasoning = value ?? 'default';
    pickerError = null;
    const nextSessionConfig = {
      ...getOrchestratorSessionConfigSnapshot(),
      reasoningEffort: value,
    };
    try {
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, binding);
      applyLocalOrchestratorSessionConfig(
        objectRecord(saved.orchestratorSessionConfig),
        objectRecord(saved.effectiveOrchestratorConfig),
        sessionId,
        (saved.orchestratorReasoningCapability ?? null) as ModelReasoningCapability | null,
      );
    } catch (error) {
      console.warn('[InputArea] 保存主线思考强度失败:', error);
      pickerError = i18n.t('input.reasoningSaveFailed');
      addToast('error', pickerError);
    } finally {
      pickerSavingReasoning = null;
      sliderDraft = null;
      if (focusedSlider) void tick().then(() => {
        if (focusedSlider.isConnected && document.activeElement === document.body) focusedSlider.focus({ preventScroll: true });
      });
    }
  }

  function commitUnselectedReasoningSlider(event: Event) {
    if (currentPickerReasoningEffort !== null) return;
    if (event instanceof KeyboardEvent && !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'].includes(event.key)) return;
    const value = reasoningOptions[Number((event.currentTarget as HTMLInputElement).value)];
    if (value) void selectPickerReasoningEffort(value);
  }

  async function saveCurrentModelContextWindow(contextWindowTokens: number): Promise<void> {
    const model = currentPickerModel.trim();
    if (!model) {
      throw new Error(i18n.t('input.contextRing.modelRequired'));
    }
    const saved = await saveAgentModelContextWindow(model, contextWindowTokens, currentPickerSourceId);
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (snapshot && settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      messagesState.settingsBootstrapSnapshot = {
        ...snapshot,
        modelContextWindows: objectRecord(saved.modelContextWindows) as Record<string, number>,
      } as AgentSettingsBootstrapSnapshot;
    }
  }

  // Prompt enhance：调用后端模型重写当前 textarea 文本
  // 这里固定走辅助模型，不占用主线模型配额；如果存在选中的技能上下文，一并传给后端增强。
  async function enhancePromptHandler() {
    const draft = resolveComposerRawContent();
    const normalizedDraft = draft.trim();
    if (enhanceLoading || !normalizedDraft || !auxiliaryEnhanceReady) return;
    const requestSeq = ++enhanceRequestSeq;
    const requestScopeKey = currentComposerReferenceScopeKey();
    const abortController = new AbortController();
    enhanceAbortController = abortController;
    enhanceLoading = true;
    try {
      const result = await enhanceAgentPrompt({
        prompt: normalizedDraft,
        skillName: selectedSkill?.skillId?.trim() || null,
        skillDescription: selectedSkill?.description?.trim() || null,
        locale: i18n.locale,
      }, abortController.signal);
      if (requestSeq !== enhanceRequestSeq || requestScopeKey !== currentComposerReferenceScopeKey()) return;
      const next = result?.enhancedPrompt?.trim() || '';
      if (!next) {
        if (result?.error) {
          console.warn('[InputArea] 提示词优化返回错误:', result.error);
        }
        addToast('warning', i18n.t('input.enhance.empty'));
        return;
      }
      if (next.length > MAX_INPUT_CHARS) {
        addToast('warning', i18n.t('input.enhance.tooLong', { max: MAX_INPUT_CHARS }));
        return;
      }
      enhanceOriginalPrompt = draft;
      enhanceResultPrompt = next;
      inputValue = next;
      pendingCaretOffset = next.length;
      queueMicrotask(focusEditor);
    } catch (error) {
      if (abortController.signal.aborted || requestSeq !== enhanceRequestSeq) return;
      console.warn('[InputArea] 提示词优化失败:', error);
      addToast('error', i18n.t('input.enhance.failed'));
    } finally {
      if (enhanceAbortController === abortController) enhanceAbortController = null;
      enhanceLoading = false;
    }
  }

  function restoreEnhancedPrompt() {
    if (!enhanceOriginalPrompt || !enhanceResultPrompt) return;
    inputValue = enhanceOriginalPrompt;
    pendingCaretOffset = enhanceOriginalPrompt.length;
    clearEnhanceSnapshot();
    queueMicrotask(focusEditor);
  }
async function refreshPickerSettingsSnapshot() {
    const latest = await getAgentSettingsBootstrap({ bootstrapScope: 'core', accessProfile: selectedAccessProfile });
    if (!settingsBootstrapMatchesCurrentWorkspace(latest)) {
      return;
    }
    messagesState.settingsBootstrapSnapshot = latest;
  }
</script>

<div class="ia-container">
  <!-- 停靠叠层，自上而下：提问卡片、排队面板、计划抽屉、目标抽屉，最后是输入框；越靠近输入框的卡片越宽。样式见全局 .dock-card。 -->
  <!-- 模型向用户提出的选择题：叠层最上方、最窄的一张卡。 -->
  {#if pendingUserQuestion}
    {#key pendingUserQuestion.questionId}
      <UserQuestionPanel pending={pendingUserQuestion} remaining={pendingUserQuestionCount - 1} level={questionDockLevel} />
    {/key}
  {/if}
  {#if queuedMessages.length > 0}
    <div class="ia-queue-panel dock-card" style="--dock-level: {queueDockLevel}">
      <div class="ia-queue-header dock-header">
        <span class="dock-lead" style="--dock-tone: var(--foreground-muted)"><Icon name="hourglass" size={13} /></span>
        <span class="ia-queue-header-title dock-title">
          {i18n.t('input.queue.header', { count: queuedMessages.length })}
        </span>
      </div>
      <div class="ia-queue-list">
        {#each queuedMessages as queued (queued.id)}
          {@const guideAvailable = queued.guide?.status === 'ready' && Boolean(queued.guide.targetTurnId)}
          {@const attachments = summarizeQueuedMessage(queued)}
          {@const commandLabel = resolveUserMessageCommandLabel({
            sessionCommand: queued.command,
            goalMode: queued.goalMode,
            skillName: queued.skillName,
            skillTitle: availableSkills.find(skill => skill.skillId === queued.skillName)?.name,
          })}
          <div class="ia-queue-item dock-row">
            <span class="ia-queue-index" aria-hidden="true"></span>
            <div class="ia-queue-content" title={queued.content}>
              {#if queued.pluginMessage}
                <span class="ia-queue-badge">{i18n.t(queued.pluginMessage.delivery === 'context' ? 'input.queue.pluginContext' : 'input.queue.pluginMessage', { name: queued.pluginMessage.pluginName })}</span>
              {/if}
              {#if queued.interruptTurnId}
                <span class="ia-queue-badge">{i18n.t('input.followUp.replacing')}</span>
              {/if}
              {#if commandLabel}<span class="ia-queue-command">{commandLabel}</span>{/if}
              {#if attachments.images > 0}
                <span class="ia-queue-badge">{i18n.t('input.queue.badge.images', { count: attachments.images })}</span>
              {/if}
              {#if attachments.references > 0}
                <span class="ia-queue-badge">{i18n.t('input.queue.badge.references', { count: attachments.references })}</span>
              {/if}
              {#if attachments.annotations > 0}
                <span class="ia-queue-badge">{i18n.t('input.queue.badge.annotations', { count: attachments.annotations })}</span>
              {/if}
              {queued.content}
              {#if queued.interruptionError}
                <span class="ia-queue-badge" title={queued.interruptionError}>{queued.interruptionError}</span>
              {/if}
            </div>
            <div class="ia-queue-actions">
              {#if queued.interruptTurnId && queued.interruptionError}
                <button type="button" class="ia-queue-action btn btn--secondary btn--sm"
                  onclick={() => vscode.postMessage({ type: 'retryQueuedMessage', queuedMessageId: queued.id, expectedRevision: queued.revision })}
                >{i18n.t('input.followUp.retry')}</button>
              {/if}
              {#if !queued.interruptTurnId && !queued.pluginMessage}
              <button
                type="button"
                class="ia-queue-action ia-queue-guide btn btn--secondary btn--sm"
                disabled={!guideAvailable}
                onclick={() => guideQueuedMessage(queued)}
                title={guideQueuedMessageTitle(queued)}
                aria-label={guideQueuedMessageTitle(queued)}
              >
                <Icon name="corner-down-right" size={12} />
                <span>{i18n.t('input.queue.guide')}</span>
              </button>
              <button
                type="button"
                class="ia-queue-action dock-icon-btn"
                disabled={!queuedMessageEditable(queued)}
                onclick={() => editQueuedMessage(queued.id)}
                title={i18n.t('input.queue.edit')}
                aria-label={i18n.t('input.queue.edit')}
              >
                <Icon name="edit" size={12} />
              </button>
              {/if}
              <button
                type="button"
                class="ia-queue-action dock-icon-btn dock-icon-btn--danger"
                onclick={() => deleteQueuedMessage(queued.id, queued.revision)}
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
  <GoalRunDrawers bind:count={drawersCount} />


  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="ia-wrapper"
    class:drag-active={imageDragActive}
    ondragover={handleImageDragOver}
    ondragleave={handleImageDragLeave}
    ondrop={handleImageDrop}
  >
    {#if imageDragActive}
      <div class="ia-drop-hint" aria-hidden="true">{i18n.t('input.drop.hint')}</div>
    {/if}
    {#if editingTurn || editingQueuedMessage}
      <div class="ia-editing-bar" role="status" data-testid="input-editing-bar">
        <span>{i18n.t(editingQueuedMessage ? 'input.queue.editing' : 'input.editingPreviousMessage')}</span>
        {#if queueEditConflict}<span class="ia-editing-conflict" role="alert">{queueEditConflict}</span>{/if}
        <button type="button" onclick={cancelMessageEditing} disabled={queueEditPending}>{i18n.t('input.cancelEditing')}</button>
      </div>
    {/if}

    <!-- 快捷引用保持结构化状态，不把 /goal 或 Skill 名称注入用户正文。 -->
    {#if selectedContextReferences.length > 0 || selectedBrowserAnnotations.length > 0 || selectedBrowserNodeSelections.length > 0 || selectedGoalMode || selectedSessionCommand || selectedSkill}
      <div class="ia-reference-chip-row">
        {#each selectedContextReferences as reference (reference.id)}
          <span class="ia-reference-chip ia-context-reference-chip" title={reference.path}>
            {#if reference.kind === 'directory'}
              <Icon name="folder" size={14} />
            {:else}
              <FileTypeIcon path={reference.name} size={14} />
            {/if}
            <span class="ia-reference-chip-label"><FileName name={reference.name} preserveExtension={reference.kind !== 'directory'} /></span>
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={() => removeContextReference(reference.id)}
              title={i18n.t('input.add.removeContext')}
              aria-label={i18n.t('input.add.removeContext')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/each}
        {#each selectedBrowserAnnotations as annotation (annotation.annotationId)}
          <span class="ia-reference-chip ia-browser-annotation-chip" title={annotation.comment}>
            <span class="ia-browser-annotation-number">{annotation.sequence}</span>
            <span class="ia-reference-chip-label">{annotation.comment}</span>
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={() => startBrowserAnnotationEditing(annotation)}
              title={i18n.t('browser.annotation.edit')}
              aria-label={i18n.t('browser.annotation.edit')}
            >
              <Icon name="edit" size={10} />
            </button>
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={() => removeAnnotationReference(annotation)}
              title={i18n.t('browser.annotation.remove')}
              aria-label={i18n.t('browser.annotation.remove')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/each}
        {#each selectedBrowserNodeSelections as selection, selectionIndex (browserNodeSelectionKey(selection))}
          <span class="ia-reference-chip ia-browser-node-selection-chip" title={selection.url}>
            <span class="ia-browser-annotation-number">{selectionIndex + 1}</span>
            <span class="ia-reference-chip-label">{selection.ariaRole || selection.nodeName}{selection.ariaName ? `: ${selection.ariaName}` : ''}</span>
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={() => { selectedBrowserNodeSelections = selectedBrowserNodeSelections.filter((_, index) => index !== selectionIndex); }}
              title={i18n.t('browser.nodeSelection.remove')}
              aria-label={i18n.t('browser.nodeSelection.remove')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/each}
        {#if selectedGoalMode}
          <span class="ia-reference-chip ia-reference-chip-goal" title={i18n.t('input.goalMode.description')}>
            <Icon name="infinity" size={11} />
            <span class="ia-reference-chip-label">/goal</span>
            <span class="ia-reference-chip-desc">{i18n.t('input.goalMode.name')}</span>
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={removeGoalMode}
              title={i18n.t('input.removeGoalMode')}
              aria-label={i18n.t('input.removeGoalMode')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/if}
        {#if selectedSessionCommand}
          <span class="ia-reference-chip ia-reference-chip-goal" title={i18n.t('input.compact.description')}>
            <Icon name="refresh" size={11} />
            <span class="ia-reference-chip-label">/compact</span>
            <span class="ia-reference-chip-desc">{i18n.t('input.compact.name')}</span>
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={removeSessionCommand}
              title={i18n.t('input.removeCompact')}
              aria-label={i18n.t('input.removeCompact')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/if}
        {#if selectedPluginCommand}
          <span class="ia-reference-chip ia-reference-chip-goal" title={selectedPluginCommand.description}>
            <Icon name="tool" size={11} />
            <span class="ia-reference-chip-label">/{selectedPluginCommand.name}</span>
            {#if selectedPluginCommand.description}<span class="ia-reference-chip-desc">{selectedPluginCommand.description}</span>{/if}
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={() => { selectedPluginCommand = null; queueMicrotask(focusEditor); }}
              title={i18n.t('input.removePluginCommand')}
              aria-label={i18n.t('input.removePluginCommand')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/if}
        {#if selectedSkill}
          <span class="ia-skill-chip" title={selectedSkill.description}>
            <Icon name="skill" size={11} />
            <span class="ia-reference-chip-label">/{selectedSkill.name}</span>
            {#if selectedSkill.description}
              <span class="ia-reference-chip-desc">{selectedSkill.description}</span>
            {/if}
            <button
              type="button"
              class="ia-reference-chip-remove"
              onclick={removeSelectedSkill}
              title={i18n.t('input.removeSkill')}
              aria-label={i18n.t('input.removeSkill')}
            >
              <Icon name="close" size={10} />
            </button>
          </span>
        {/if}
      </div>
    {/if}


    {#if selectedImages.length > 0 || pendingImageReadCount > 0}
      <ImageAttachmentTray
        images={selectedImages}
        pendingCount={pendingImageReadCount}
        disabled={sessionInputLocked || isInteractionBlocking}
        onRemove={removeImage}
        onClear={clearAllImages}
      />
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      bind:this={inputTextareaEl}
      class="ia-textarea"
      data-testid="input-textarea"
      class:has-images={selectedImages.length > 0}
      class:is-empty={!inputValue}
      contenteditable={!(sessionInputLocked || (isInteractionBlocking && !editingQueuedMessage))}
      role="textbox"
      tabindex={sessionInputLocked || (isInteractionBlocking && !editingQueuedMessage) ? -1 : 0}
      aria-multiline="true"
      aria-disabled={sessionInputLocked || (isInteractionBlocking && !editingQueuedMessage)}
      data-placeholder={selectedImages.length > 0
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
      <div class="ia-slash-popover" data-magi-surface="popover" role="listbox" aria-label={i18n.t('input.slash.label')}>
        <div class="ia-slash-list" bind:this={slashListEl}>
          {#each filteredSlashCommands as command, index (`${command.kind}:${command.id}`)}
            {#if index === 0 || filteredSlashCommands[index - 1]?.kind !== command.kind}
              <div class="ia-slash-group-label">
                {command.kind === 'goal'
                  ? i18n.t('input.slash.modeGroup')
                  : command.kind === 'command'
                    ? i18n.t('input.slash.commandGroup')
                  : command.kind === 'plugin-command'
                    ? i18n.t('input.slash.pluginCommandGroup')
                    : i18n.t('input.slash.skillGroup')}
              </div>
            {/if}
            <button
              type="button"
              role="option"
              aria-selected={index === slashHighlightIndex}
              class="ia-slash-item"
              class:active={index === slashHighlightIndex}
              class:disabled={command.kind === 'command' && Boolean(command.disabledReason)}
              aria-disabled={command.kind === 'command' && Boolean(command.disabledReason)}
              onmouseenter={() => (slashHighlightIndex = index)}
              onmousedown={(e) => { e.preventDefault(); commitSlashCommand(command); }}
            >
              <span class="ia-slash-item-icon" class:goal={command.kind === 'goal'}>
                <Icon name={command.kind === 'goal' ? 'infinity' : command.kind === 'command' ? 'refresh' : command.kind === 'plugin-command' ? 'tool' : 'skill'} size={12} />
              </span>
              <span class="ia-slash-item-content">
                <span class="ia-slash-item-label">/{command.kind === 'goal' ? 'goal' : command.kind === 'command' ? command.id : command.name}</span>
                {#if command.kind === 'command' && command.disabledReason}
                  <span class="ia-slash-item-description ia-slash-item-reason">
                    {i18n.t(`input.compact.disabled.${command.disabledReason === 'draft-session' ? 'draft' : 'attachments'}`)}
                  </span>
                {:else if command.description}
                  <span class="ia-slash-item-description">{command.description}</span>
                {/if}
              </span>
            </button>
          {/each}
        </div>
      </div>
    {/if}

    <div class="ia-actions">
      <div class="ia-left">
        <div class="ia-picker-wrap ia-add-wrap">
          <button
            type="button"
            class="ia-add-btn"
            class:active={addMenuOpen}
            onclick={() => (addMenuOpen = !addMenuOpen)}
            disabled={sessionInputLocked || isInteractionBlocking}
            title={i18n.t('input.add.title')}
            aria-label={i18n.t('input.add.title')}
            aria-expanded={addMenuOpen}
          >
            <Icon name="plus" size={15} />
          </button>
          {#if addMenuOpen}
            <div class="ia-picker-popover ia-add-popover" data-magi-surface="popover" role="menu">
              {#each composerActions as action, index (`${action.kind}:${action.id}`)}
                {#if index === 0 || composerActions[index - 1]?.kind !== action.kind}
                  <div class="ia-add-group-label">
                    {action.kind === 'resource'
                      ? i18n.t('input.add.resourceGroup')
                      : action.kind === 'goal'
                        ? i18n.t('input.slash.modeGroup')
                        : action.kind === 'command'
                          ? i18n.t('input.slash.commandGroup')
                        : action.kind === 'plugin-command'
                          ? i18n.t('input.slash.pluginCommandGroup')
                          : i18n.t('input.slash.skillGroup')}
                  </div>
                {/if}
                <button
                  type="button"
                  class="ia-add-item"
                  class:disabled={action.kind === 'command' && Boolean(action.disabledReason)}
                  aria-disabled={action.kind === 'command' && Boolean(action.disabledReason)}
                  class:selected={action.kind === 'goal'
                    ? selectedGoalMode
                    : action.kind === 'command'
                      ? selectedSessionCommand === action.id
                    : action.kind === 'skill'
                      ? selectedSkill?.skillId === action.skill.skillId
                    : action.kind === 'plugin-command'
                      ? selectedPluginCommand?.pluginCommandId === action.pluginCommand.pluginCommandId
                    : false}
                  onclick={() => applyAddMenuAction(action)}
                  role="menuitem"
                >
                  <span class="ia-add-item-icon" class:goal={action.kind === 'goal'}>
                    <Icon
                      name={action.kind === 'resource'
                        ? 'folder'
                      : action.kind === 'goal'
                          ? 'infinity'
                          : action.kind === 'command'
                            ? 'refresh'
                          : action.kind === 'plugin-command'
                            ? 'tool'
                          : 'skill'}
                      size={13}
                    />
                  </span>
                  <span class="ia-add-item-content">
                    <span class="ia-add-item-label">{action.name}</span>
                    {#if action.kind === 'command' && action.disabledReason}
                      <span class="ia-add-item-description">
                        {i18n.t(`input.compact.disabled.${action.disabledReason === 'draft-session' ? 'draft' : 'attachments'}`)}
                      </span>
                    {:else if action.description}
                      <span class="ia-add-item-description">{action.description}</span>
                    {/if}
                  </span>
                </button>
              {/each}
            </div>
          {/if}
        </div>
        {#if currentSessionId}
          {#each conversationViews(pluginProjection().manifests, 'composerLeft') as item (item.pluginId + ':' + item.view.id)}
            <PluginConversationView pluginId={item.pluginId} entry={item.view.entry} label={item.view.title}
              placement="composerLeft" context={{ sessionId: currentSessionId }} />
          {/each}
        {/if}
        <div class="ia-picker-wrap ia-workspace-wrap">
          <button
            type="button"
            class="ia-workspace-btn"
            bind:this={workspaceTrigger}
            aria-haspopup="dialog"
            class:active={workspacePickerOpen}
            class:configured={composerWorkspace !== null}
            class:locked={!isDraftSession}
            onclick={() => {
              if (workspacePickerOpen) closeWorkspacePicker(false);
              else void openWorkspacePicker();
            }}
            onkeydown={(event) => {
              if (event.key === 'Escape' && workspacePickerOpen) {
                event.preventDefault(); event.stopPropagation(); closeWorkspacePicker();
              } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                event.preventDefault(); void openWorkspacePicker(event.key === 'ArrowUp');
              }
            }}
            disabled={sessionInputLocked || isInteractionBlocking}
            title={workspaceButtonTitle(composerWorkspace)}
            aria-label={workspaceButtonTitle(composerWorkspace)}
            aria-expanded={workspacePickerOpen}
            aria-controls={workspacePopoverId}
            aria-disabled={!isDraftSession || sessionInputLocked || isInteractionBlocking}
          >
            <Icon name="grid" size={16} />
            <span class="ia-workspace-btn-label">{workspaceButtonLabel(composerWorkspace)}</span>
          </button>
          {#if workspacePickerOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-popover-backdrop" onclick={() => closeWorkspacePicker(false)}></div>
            <div id={workspacePopoverId} class="ia-picker-popover ia-workspace-popover" data-magi-surface="popover" role="dialog" tabindex="-1" aria-label={i18n.t('input.workspace.title')}
              bind:this={workspacePopover} use:fitContextPopover onkeydown={(event) => handleContextPickerKeys(event, workspacePopover, closeWorkspacePicker)}>
              {#if isDraftSession}
                <button type="button" class="ia-picker-item" class:selected={isPersonalSession}
                  onclick={() => selectWorkspace('')}>
                  <span class="ia-picker-item-label">{i18n.t('input.workspace.unbound')}</span>
                </button>
                {#if workspaceOptions.length > 10}<input type="search" class="form-input" bind:value={workspaceSearch} placeholder={i18n.t('selection.search')} aria-label={i18n.t('selection.searchFor', { label: i18n.t('input.workspace.title') })} />{/if}
                {#if workspaceOptions.length === 0}
                  <div class="ia-picker-status">{i18n.t('input.workspace.empty')}</div>
                {:else}
                  <div class="ia-picker-list" role="listbox" aria-label={i18n.t('input.workspace.title')}>
                    {#each filteredWorkspaces as workspace (workspace.workspaceId)}
                      <button
                        type="button"
                        class="ia-picker-item"
                        class:selected={composerWorkspace?.workspaceId === workspace.workspaceId}
                        role="option" aria-selected={composerWorkspace?.workspaceId === workspace.workspaceId}
                        onclick={() => selectWorkspace(workspace.workspaceId)}
                      >
                        <span class="ia-picker-item-label" title={workspace.name}>{workspace.name}</span>
                        <span class="ia-picker-item-desc" dir="ltr" title={workspace.rootPath}>{workspace.rootPath}</span>
                      </button>
                    {:else}
                      <div class="ia-picker-status" role="status">{i18n.t('selection.empty')}</div>
                    {/each}
                  </div>
                {/if}
              {/if}
              {#if isDraftSession}
                <div class="ia-picker-divider"></div>
                <button type="button" class="ia-picker-item ia-picker-row ia-workspace-action" onclick={createProjectFromComposer}>
                  <span class="ia-workspace-action-label"><Icon name="plus" size={16} /><span class="ia-picker-item-label">{i18n.t('web.createProjectTitle')}</span></span>
                </button>
                <button
                  type="button"
                  class="ia-picker-item ia-picker-row ia-workspace-action"
                  onclick={useExistingWorkspaceFolder}
                >
                  <span class="ia-workspace-action-label">
                    <Icon name="folder" size={13} />
                    <span class="ia-picker-item-label">{i18n.t('input.workspace.useExistingFolder')}</span>
                  </span>
                  <Icon name="chevron-right" size={11} />
                </button>
              {/if}
            </div>
          {/if}
        </div>
        {#if composerWorkspace && (composerWorkspace.sourceFolders?.length || 0) > 1}
          <div class="ia-picker-wrap ia-source-folder-wrap">
            <button type="button" class="ia-workspace-btn" class:active={sourceFolderPickerOpen}
              bind:this={sourceFolderTrigger}
              aria-haspopup="dialog" aria-expanded={sourceFolderPickerOpen} aria-controls={sourceFolderPopoverId}
              aria-label={`${i18n.t('input.sourceFolderTitle')}: ${selectedSourceFolder?.path || i18n.t('input.sourceFolderUnavailable')}`}
              title={`${i18n.t('input.sourceFolderTitle')}: ${selectedSourceFolder?.path || i18n.t('input.sourceFolderUnavailable')}`}
              disabled={sessionInputLocked || isInteractionBlocking || Boolean(editingQueuedMessage || editingTurn)}
              onkeydown={(event) => {
                if (event.key === 'Escape' && sourceFolderPickerOpen) {
                  event.preventDefault(); event.stopPropagation(); closeSourceFolderPicker();
                } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                  event.preventDefault(); void openSourceFolderPicker(event.key === 'ArrowUp');
                }
              }}
              onclick={() => { if (sourceFolderPickerOpen) closeSourceFolderPicker(false); else void openSourceFolderPicker(); }}>
              <Icon name="folder" size={16} />
              <bdi class="ia-workspace-btn-label">{selectedSourceFolder?.name || selectedSourceFolder?.path.split(/[\\/]/).filter(Boolean).at(-1) || i18n.t('input.sourceFolderTitle')}</bdi>
            </button>
            {#if sourceFolderPickerOpen}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="ia-popover-backdrop" onclick={() => closeSourceFolderPicker(false)}></div>
              <div id={sourceFolderPopoverId} class="ia-picker-popover ia-source-folder-popover" data-magi-surface="popover" role="dialog" tabindex="-1"
                aria-label={i18n.t('input.sourceFolderTitle')} bind:this={sourceFolderPopover}
                use:fitContextPopover onkeydown={(event) => handleContextPickerKeys(event, sourceFolderPopover, closeSourceFolderPicker)}>
                {#if (composerWorkspace.sourceFolders?.length || 0) > 10}
                  <input type="search" class="form-input" bind:value={sourceFolderSearch}
                    placeholder={i18n.t('selection.search')}
                    aria-label={i18n.t('selection.searchFor', { label: i18n.t('input.sourceFolderTitle') })} />
                {/if}
                <div role="listbox" aria-label={i18n.t('input.sourceFolderTitle')}>
                  {#each filteredSourceFolders as folder (folder.sourceFolderId)}
                    <button type="button" class="ia-picker-item" class:selected={selectedSourceFolderId === folder.sourceFolderId}
                      role="option" aria-selected={selectedSourceFolderId === folder.sourceFolderId}
                      disabled={sessionInputLocked || isInteractionBlocking || Boolean(editingQueuedMessage || editingTurn)}
                      onclick={() => selectSourceFolder(folder.sourceFolderId)}>
                      <span class="ia-picker-item-label" title={folder.name || folder.path.split(/[\\/]/).filter(Boolean).at(-1)}><bdi>{folder.name || folder.path.split(/[\\/]/).filter(Boolean).at(-1)}</bdi>{#if folder.isPrimary} · {i18n.t('input.primarySourceFolder')}{/if}</span>
                      <span class="ia-picker-item-desc" dir="ltr" title={folder.path}>{folder.path}</span>
                    </button>
                  {:else}
                    <div class="ia-picker-status" role="status">{i18n.t('selection.empty')}</div>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
        {/if}
        <GitContextControl
          workspace={composerWorkspace}
          sessionId={persistedSessionId}
          disabled={sessionInputLocked || isInteractionBlocking}
        />
        <SessionIsolationChip
          workspace={composerWorkspace}
          sessionId={persistedSessionId}
          disabled={sessionInputLocked || isInteractionBlocking}
          ensureSession={materializeDraftSession}
        />
      </div>

      <div class="ia-right">
        <div class="ia-runtime-controls">
          <div class="ia-picker-wrap ia-access-wrap">
            <button
              type="button"
              class="ia-picker-btn ia-access-btn ia-access-btn--{selectedAccessProfile}"
              class:active={accessProfilePickerOpen}
              onclick={() => (accessProfilePickerOpen = !accessProfilePickerOpen)}
              disabled={sessionInputLocked || isInteractionBlocking}
              title={`${i18n.t('input.access.title')}: ${i18n.t(currentAccessProfileOption.labelKey)}。${i18n.t(currentAccessProfileOption.descriptionKey)}`}
              aria-haspopup="listbox"
              aria-expanded={accessProfilePickerOpen}
              aria-label={`${i18n.t('input.access.title')}: ${i18n.t(currentAccessProfileOption.labelKey)}`}
            >
              <Icon name={currentAccessProfileOption.icon} size={14} />
              <span class="ia-access-btn-label">{i18n.t(currentAccessProfileOption.labelKey)}</span>
            </button>
            {#if accessProfilePickerOpen}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="ia-popover-backdrop" onclick={() => (accessProfilePickerOpen = false)}></div>
              <div class="ia-picker-popover ia-access-popover" data-magi-surface="popover" role="group" aria-label={i18n.t('input.access.title')}>
                <div class="ia-picker-header composer-popover-title">{i18n.t('input.access.title')}</div>
                <div class="ia-picker-list" role="listbox" aria-label={i18n.t('input.access.title')}>
                  {#each accessProfileOptions as option (option.value)}
                    <button
                      type="button"
                      class="ia-picker-item ia-access-option ia-access-option--{option.value}"
                      class:selected={selectedAccessProfile === option.value}
                      onclick={() => selectAccessProfile(option.value)}
                      role="option"
                      aria-selected={selectedAccessProfile === option.value}
                      title={i18n.t(option.descriptionKey)}
                    >
                      <span class="ia-access-option-icon"><Icon name={option.icon} size={14} /></span>
                      <span class="ia-access-option-copy">
                        <span class="ia-access-option-heading">
                          <span class="ia-picker-item-label">{i18n.t(option.labelKey)}</span>
                          {#if option.value === 'restricted'}
                            <span class="ia-access-recommended">{i18n.t('input.access.recommended')}</span>
                          {/if}
                        </span>
                        <span class="ia-picker-item-desc">{i18n.t(option.descriptionKey)}</span>
                      </span>
                    </button>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
          <span class="ia-toolbar-divider" aria-hidden="true"></span>
          <SessionStats sessionId={persistedSessionId} running={isSending} />
          <ContextUsageRing
            model={currentPickerModel}
            configuredTokenLimit={configuredContextWindow}
            usageRatio={contextBudgetView?.usageRatio ?? null}
            tokenUsed={contextBudgetView?.tokenUsed ?? null}
            remainingTokens={contextBudgetView?.remainingTokens ?? null}
            tokenLimit={contextBudgetView?.tokenLimit ?? null}
            warningLevel={contextBudgetView?.warningLevel ?? null}
            lastCompactionReason={contextBudgetView?.lastCompactionReason ?? null}
            originalTokenEstimate={contextBudgetView?.originalTokenEstimate ?? null}
            compactedTokenEstimate={contextBudgetView?.compactedTokenEstimate ?? null}
            measurement={contextBudgetView?.measurement ?? null}
            contextBreakdown={contextBudgetView?.contextBreakdown ?? null}
            responseReserveTokens={contextBudgetView?.responseReserveTokens ?? null}
            recoveryBufferTokens={contextBudgetView?.recoveryBufferTokens ?? null}
            proactiveThresholdTokens={contextBudgetView?.proactiveThresholdTokens ?? null}
            onSaveContextWindow={saveCurrentModelContextWindow}
          />
        </div>
        <div class="ia-submit-controls">
          <span class="ia-toolbar-divider" aria-hidden="true"></span>
          <div class="ia-picker-wrap ia-model-wrap">
          <button
            type="button"
            class="ia-picker-btn ia-model-btn"
            aria-haspopup="listbox"
            class:active={pickerOpen}
            class:configured={currentPickerModel !== ''}
            onclick={togglePicker}
            disabled={sessionInputLocked || isInteractionBlocking || pickerSavingModel !== null || pickerSavingReasoning !== null}
            title={currentPickerLabel
              ? i18n.t('input.mainModelPicker.titleConfigured', { model: currentPickerLabel })
              : i18n.t('input.mainModelPicker.titleEmpty')}
            aria-expanded={pickerOpen}
          >
            <span class="ia-picker-btn-label">{currentPickerLabel || i18n.t('input.mainModelPicker.buttonEmpty')}</span>

            {#if currentPickerReasoningLabel}
              <span class="ia-model-effort">{currentPickerReasoningLabel}</span>
            {/if}
            <Icon name="chevron-down" size={10} />
          </button>
          {#if pickerOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-popover-backdrop" onclick={() => (pickerOpen = false)}></div>
            <div class="ia-session-model-popover" data-magi-surface="popover" role="group" aria-label={i18n.t('input.mainModelPicker.header')}>
              {#if pickerView === 'effort'}
                <div class="ia-model-summary">
                  <div class="ia-effort-summary-header">
                    <span class="ia-effort-current" aria-live="polite">{#key displayedReasoning}<span class="ia-effort-value">{reasoningEffortLabel(displayedReasoning) || i18n.t('input.mainModelPicker.reasoning.providerDefault')}</span>{/key}</span>
                  </div>
                  <button type="button" class="ia-summary-model" aria-label={i18n.t('input.mainModelPicker.chooseModel')} onclick={() => { pickerView = 'models'; modelSearch = ''; }}>
                    <span>{currentPickerLabel || i18n.t('input.mainModelPicker.buttonEmpty')}</span><Icon name="chevron-right" size={13} />
                  </button>
                  {#if reasoningOptions.length > 1}
                    <div class="ia-reasoning-slider" style:--effort-progress={`${reasoningIndex / (reasoningOptions.length - 1) * 100}%`}>
                      <div class="ia-reasoning-control">
                      <div class="ia-reasoning-track" aria-hidden="true"><span class="ia-reasoning-fill"></span><span class="ia-reasoning-thumb"></span></div>
                      <input type="range" class="control-range" min="0" max={reasoningOptions.length - 1} step="1" value={reasoningIndex} aria-label={i18n.t('input.mainModelPicker.reasoning.header')} aria-valuetext={reasoningEffortLabel(displayedReasoning)} disabled={pickerSavingReasoning !== null || pickerSavingModel !== null} oninput={(event) => { sliderDraft = Number(event.currentTarget.value); }} onchange={(event) => void selectPickerReasoningEffort(reasoningOptions[Number(event.currentTarget.value)])} onpointerup={commitUnselectedReasoningSlider} onkeyup={commitUnselectedReasoningSlider} />
                      </div>
                      <div class="ia-reasoning-ticks" aria-hidden="true">{#each reasoningOptions as effort}<span class:active={effort === displayedReasoning}></span>{/each}</div>
                    </div>
                  {/if}

                    {#if reasoningOptions.length === 1}
                      <div class="ia-reasoning-actions">
                        <button type="button" class="btn btn--sm btn--secondary" aria-pressed={currentPickerReasoningEffort === reasoningOptions[0]} disabled={pickerSavingReasoning !== null || pickerSavingModel !== null} onclick={() => void selectPickerReasoningEffort(reasoningOptions[0])}>{reasoningEffortLabel(reasoningOptions[0])}</button>
                      </div>
                    {/if}
                    {#if currentPickerSourceError}<p class="ia-picker-status-error" role="alert">{i18n.t('input.mainModelSourceUnavailable')}</p>{/if}
                    {#if !currentReasoningCapability && reasoningReadStatus === 'loading'}<p class="ia-reasoning-hint" role="status">{i18n.t('settings.model.reasoning.loading')}</p>
                    {:else if !currentReasoningCapability && reasoningReadStatus === 'failed'}<p class="ia-reasoning-hint" role="alert">{i18n.t('settings.model.reasoning.failed')}</p><button type="button" class="btn btn--sm btn--secondary" onclick={() => reasoningRetry++}>{i18n.t('input.mainModelPicker.retry')}</button>
                    {:else if !reasoningOptions.length}<p class="ia-reasoning-hint">{i18n.t(currentReasoningCapability?.source === 'unknown' ? 'input.mainModelPicker.reasoning.unknown' : 'input.mainModelPicker.reasoning.unsupported')}</p>
                    {/if}
                    {#if reasoningAdjusted}<p class="ia-reasoning-hint" role="status">{i18n.t('settings.model.reasoning.adjusted')}</p>{/if}

                  {#if pickerError}<p class="ia-picker-status-error" role="alert">{pickerError}</p>{/if}
                </div>
              {:else}
              <div class="ia-model-list-section">
                <div class="ia-section-header-row">
                  <div class="ia-picker-header composer-popover-title">{i18n.t('input.mainModelPicker.header')}</div>
                  <button
                    type="button"
                    class="ia-picker-refresh"
                    onclick={() => void refreshPickerModels()}
                    disabled={pickerLoading || pickerSavingModel !== null || pickerSavingReasoning !== null}
                    title={i18n.t('input.mainModelPicker.refresh')}
                    aria-label={i18n.t('input.mainModelPicker.refresh')}
                  >
                    <Icon name="refresh" size={13} class={pickerLoading ? 'spinning' : ''} />
                  </button>
                </div>
                {#if currentPickerSourceError}<p class="ia-picker-status-error" role="alert">{i18n.t('input.mainModelSourceUnavailable')}</p>{/if}
                {#if pickerModelCount > 10}
                  <input type="search" class="form-input" bind:value={modelSearch} placeholder={i18n.t('selection.search')} aria-label={i18n.t('selection.searchFor', { label: i18n.t('input.mainModelPicker.header') })} />
                {/if}
                {#if pickerLoading}
                  <div class="ia-picker-status">{i18n.t('input.mainModelPicker.loading')}</div>
                {:else if pickerError}
                  <div class="ia-picker-status ia-picker-status-error">
                    {pickerError}
                    <button
                      type="button"
                      class="btn btn--secondary btn--sm ia-picker-retry"
                      onclick={() => { pickerError = null; loadPickerModels(); }}
                    >{i18n.t('input.mainModelPicker.retry')}</button>
                  </div>
                {:else if pickerModelCount === 0}
                  <div class="ia-picker-status">{i18n.t('input.mainModelPicker.empty')}</div>
                {:else}
                  {#if filteredPickerModelGroups.length === 0}
                    <div class="ia-picker-status" role="status">{i18n.t('selection.empty')}</div>
                  {/if}
                  <div class="ia-picker-list" role="listbox" aria-label={i18n.t('input.mainModelPicker.header')}>
                    {#each filteredPickerModelGroups as group (group.sourceId)}
                      <div class="ia-picker-group-heading">{group.name}{#if group.kind === 'plugin'}<span class="ia-picker-group-origin">{i18n.t('input.mainModelPicker.pluginSource')}</span>{/if}</div>
                      {#each group.models as model (`${group.sourceId}:${model.id}`)}
                        {@const selected = currentPickerSourceId === group.sourceId && currentPickerModel === model.id}
                        {@const selectionKey = `${group.sourceId}\u001f${model.id}`}
                        <button
                          type="button"
                          class="ia-picker-item ia-picker-row"
                          class:selected
                          role="option" aria-selected={selected}
                          title={`${group.name} · ${model.id}`}
                          data-model-id={model.id}
                          onclick={() => void selectPickerModel(model.id, group.sourceId)}
                          disabled={pickerSavingModel !== null || pickerSavingReasoning !== null}
                        >
                          <span class="ia-picker-item-label">{model.displayName || model.id}</span>
                          {#if pickerSavingModel === selectionKey}
                            <Icon name="loader" size={12} class="spinning" />
                          {:else if selected}
                            <span class="ia-picker-check"><Icon name="check" size={13} /></span>
                          {/if}
                        </button>
                      {/each}
                    {/each}
                  </div>

                {/if}


              </div>
              {/if}
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
          {#if editingQueuedMessage}
          <button
            class="ia-send ready"
            data-testid="input-queue-save-button"
            onclick={() => sendMessage()}
            disabled={sendDisabled || !hasContent}
            title={sendButtonTitle}
            aria-label={sendButtonTitle}
          >
            <Icon name={sendPreparing ? 'loader' : 'check'} size={14} class={sendPreparing ? 'spinning' : ''} />
          </button>
          <!-- 输入框有内容时一定是发送；只有没有内容时才可能是继续（包括继续正在提交的瞬间）。 -->
          {:else if !hasContent && !editingTurn && (continueSubmitting || (!isSending && canContinue))}
          <button
            type="button"
            class="ia-send ready"
            data-testid="input-continue-button"
            data-continue-action={activeAgentRun.continuation}
            onclick={continueSession}
            disabled={continueSubmitting || sessionInputLocked || isInteractionBlocking}
            title={continueTitle}
            aria-label={continueTitle}
          >
            <Icon name={continueSubmitting ? 'loader' : 'play'} size={14} class={continueSubmitting ? 'spinning' : ''} />
          </button>
          {:else if isSending && hasContent}
            <button
              type="button"
              class="ia-queue-action btn btn--secondary btn--sm"
              data-testid="input-replace-button"
              onclick={() => interruptTargetTurnId && sendMessage(interruptTargetTurnId)}
              disabled={sessionInputLocked || sendPreparing || pendingImageReadCount > 0 || !mainModelReady || !interruptTargetTurnId || Boolean(editingTurn || editingQueuedMessage || selectedSessionCommand || selectedPluginCommand)}
              title={i18n.t('input.followUp.replaceTitle')}
            >{i18n.t('input.followUp.replace')}</button>
            <button
              class="ia-send ready"
              data-testid="input-followup-send-button"
              onclick={() => sendMessage()}
              disabled={sendDisabled}
              title={sendButtonTitle}
              aria-label={sendButtonTitle}
            >
              <Icon name={sendPreparing ? 'loader' : 'send'} size={14} class={sendPreparing ? 'spinning' : ''} />
            </button>
          {:else if isSending}
          <button
            class="ia-send stop"
            data-testid="input-stop-button"
            onclick={stopTask}
            disabled={stopLoading || isDraftSession}
            aria-busy={stopLoading}
            aria-label={stopLoading ? i18n.t('input.stopping') : i18n.t('input.stop')}
            title={stopLoading ? i18n.t('input.stopping') : shouldInterruptAgentRunFromComposer ? i18n.t('input.stopTaskTitle') : i18n.t('input.stop')}
          >
            <Icon name={stopLoading ? 'loader' : 'stop'} size={14} class={stopLoading ? 'spinning' : ''} />
          </button>
          {:else if hasContent}
          <!-- 空闲且有内容：显示发送按钮 -->
          <button
            class="ia-send ready"
            data-testid="input-send-button"
            onclick={() => sendMessage()}
            disabled={sendDisabled}
            title={sendButtonTitle}
            aria-label={sendButtonTitle}
          >
            <Icon name={sendPreparing ? 'loader' : 'send'} size={14} class={sendPreparing ? 'spinning' : ''} />
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

</div>

{#if contextPickerOpen}
  <Modal
    onClose={() => (contextPickerOpen = false)}
    closeOnBackdrop={true}
    size="md"
    modalClass="composer-context-picker-modal"
    showHeader={false}
  >
    {#if WebFolderPickerComponent}
      <WebFolderPickerComponent
        title={i18n.t('input.add.contextPickerTitle')}
        includeFiles={true}
        onSelect={handleContextReferenceSelected}
        onCancel={() => (contextPickerOpen = false)}
      />
    {:else}
      <div class="ia-context-picker-loading" role="status"><Icon name="loader" size={18} /><span>{i18n.t('common.loading')}</span></div>
    {/if}
  </Modal>
{/if}

{#if editingBrowserAnnotation}
  <Modal
    title={i18n.t('browser.annotation.editTitle')}
    form
    closeOnEscape={!browserAnnotationSaving}
    onClose={closeBrowserAnnotationEditing}
    size="sm"
    modalClass="browser-annotation-edit-modal"
  >
    <p class="settings-hint" role="status">{i18n.t('overlay.draftSession')}</p>
    <div class="browser-annotation-edit-form">
      <textarea
        bind:value={editingBrowserAnnotationComment}
        maxlength="4000"
        rows="4"
        aria-label={i18n.t('browser.annotation.placeholder')}
        onkeydown={(event) => {
          if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            void saveBrowserAnnotationEditing();
          }
        }}
      ></textarea>
      <div class="browser-annotation-edit-actions">
        <button type="button" class="btn btn--secondary btn--sm" onclick={closeBrowserAnnotationEditing} disabled={browserAnnotationSaving}>
          {i18n.t('browser.annotation.cancel')}
        </button>
        <button
          type="button"
          class="btn btn--primary btn--sm"
          onclick={() => void saveBrowserAnnotationEditing()}
          disabled={browserAnnotationSaving || !editingBrowserAnnotationComment.trim()}
        >
          {i18n.t('browser.annotation.save')}
        </button>
      </div>
    </div>
  </Modal>
{/if}

<style>
  .ia-context-picker-loading {
    display: flex;
    min-height: 220px;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .ia-context-picker-loading :global(svg) {
    animation: ia-spin 1s linear infinite;
  }

  /* ============================================
     InputArea - 输入区域
     设计参考: ChatGPT / Claude Desktop 简约输入框
     前缀: ia-
     ============================================ */
  .ia-container {
    display: flex;
    flex-direction: column;
    gap: 0;
    flex-shrink: 0;
    /* 输入列随中间面板自适应铺满，仅保留基础安全留白。 */
    padding-block: var(--space-3) var(--space-4);
    padding-inline: var(--space-4);
    /* 输入区是主对话面板的一部分，外层不再叠加独立玻璃底色；
       只让 .ia-wrapper 作为真正的输入控件承担表面层级。 */
    background: transparent;
    backdrop-filter: none;
    -webkit-backdrop-filter: none;
    position: relative;
    z-index: 1;
  }

  /* 有待回答的问题时，卡片与输入框零间距相接。 */
  /* 叠层卡片与输入框零间隙相接；输入框的上边线保持清晰，才分得清层次。 */
  .ia-container:has(.dock-card) .ia-wrapper {
    border-top-color: color-mix(in srgb, var(--foreground-muted) 55%, var(--border));
  }

  .ia-wrapper {
    display: flex;
    flex-direction: column;
    container-name: magi-composer;
    container-type: inline-size;
    min-height: 120px;
    max-height: 50vh;
    background: var(--magi-surface-composer);
    border: 0;
    border-radius: var(--radius-xl);
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border),
      0 2px 12px rgba(var(--foreground-rgb), 0.035);
    transition: box-shadow 180ms var(--magi-ease-ink), background 180ms ease;
    position: relative;
    /* 不使用 overflow:hidden — 允许模型下拉菜单溢出显示 */
  }

  .ia-wrapper:focus-within {
    box-shadow: inset 0 0 0 1px var(--magi-focus-ring),
      0 0 0 2px color-mix(in srgb, var(--foreground) 7%, transparent),
      0 0 0 4px color-mix(in srgb, var(--foreground) 3%, transparent);
  }

  .ia-editing-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-height: 30px;
    padding: 4px var(--space-3);
    border-bottom: var(--magi-hairline) solid var(--border-subtle);
    color: var(--foreground-secondary);
    font-size: var(--text-xs);
  }

  .ia-editing-bar button {
    flex: 0 0 auto;
    padding: 3px 6px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--primary);
    font-size: inherit;
    cursor: pointer;
  }

  .ia-editing-bar button:hover,
  .ia-editing-bar button:focus-visible {
    background: var(--primary-muted);
  }

  .ia-editing-conflict {
    flex-basis: 100%;
    order: 1;
    color: var(--error);
  }

  /* 文本框 */
  .ia-textarea {
    flex: 1;
    min-height: 36px;
    width: 100%;
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-sm);
    line-height: var(--leading-relaxed);
    border: none;
    background: transparent;
    color: var(--foreground);
    outline: none;
    font-family: inherit;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    overflow-y: auto;
    scrollbar-width: none;
    cursor: text;
  }

  .ia-textarea::-webkit-scrollbar { display: none; }

  .ia-textarea.is-empty::before {
    content: attr(data-placeholder);
    color: var(--foreground-muted);
    pointer-events: none;
    display: block;
  }
  .ia-textarea[aria-disabled="true"] { opacity: 0.5; cursor: not-allowed; }
  .ia-textarea.has-images { min-height: 36px; }

  .ia-textarea :global(.md-bold) { font-weight: var(--font-semibold); }
  .ia-textarea :global(.md-italic) { font-style: italic; }
  .ia-textarea :global(.md-code) {
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: max(var(--text-xs), 0.92em);
    background: color-mix(in srgb, var(--foreground) 8%, transparent);
    border-radius: 3px;
    padding: 0 3px;
  }
  .ia-textarea :global(.md-heading) {
    font-weight: var(--font-semibold);
    color: var(--primary, currentColor);
  }
  .ia-textarea :global(.md-quote) {
    color: var(--foreground-muted);
  }
  .ia-textarea :global(.md-list-marker) {
    color: var(--primary, currentColor);
    font-weight: var(--font-medium);
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

  .ia-left, .ia-right,
  .ia-runtime-controls, .ia-submit-controls {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }

  .ia-left {
    flex: 0 1 auto;
    max-width: 48%;
  }

  .ia-right {
    flex: 1 1 auto;
    flex-wrap: nowrap;
    justify-content: flex-end;
  }

  .ia-runtime-controls {
    flex: 0 0 auto;
  }

  .ia-submit-controls {
    flex: 0 1 auto;
  }

  .ia-toolbar-divider {
    width: 1px;
    height: 16px;
    background: var(--border);
    flex-shrink: 0;
  }

  /* 主操作以高对比实心圆区别于工具图标，配色复用共享主按钮令牌。 */
  .ia-send {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    flex-shrink: 0;
    padding: 0;
    background: var(--surface-2);
    border: none;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast), opacity var(--transition-fast);
  }

  .ia-send.ready:not(:disabled) {
    background: var(--btn-primary-bg);
    color: var(--btn-primary-fg);
  }
  .ia-send.ready:hover:not(:disabled) { background: var(--btn-primary-hover-bg); }
  .ia-send.ready:active:not(:disabled) {
    background: color-mix(in srgb, var(--btn-primary-bg) 76%, var(--btn-primary-fg));
  }
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
    border: 0;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
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
  .ia-add-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-full);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast), border-color var(--transition-fast);
  }
  .ia-add-btn:hover:not(:disabled),
  .ia-add-btn.active {
    background: var(--surface-2);
    border-color: var(--border-subtle);
    color: var(--foreground);
  }
  .ia-add-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .ia-add-wrap {
    flex: 0 0 auto;
  }

  .ia-access-btn-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: 112px;
  }
  .ia-access-btn :global(svg) {
    flex: 0 0 auto;
  }
  .ia-access-btn--read_only {
    color: var(--foreground-muted);
  }
  .ia-access-btn--restricted {
    border-color: color-mix(in srgb, var(--primary) 34%, var(--border-subtle));
    color: var(--primary);
  }
  .ia-access-btn--full_access {
    border-color: color-mix(in srgb, var(--warning) 38%, var(--border-subtle));
    color: color-mix(in srgb, var(--warning) 82%, var(--foreground));
  }
  .ia-workspace-btn, .ia-model-btn { box-shadow: inset 0 0 0 var(--magi-hairline) var(--border); }
  .ia-workspace-wrap {
    max-width: 190px;
  }
  .ia-picker-popover:is(.ia-workspace-popover, .ia-source-folder-popover) {
    right: auto;
    left: 0;
    /* 项目与源文件夹都是名称 + 单行路径，收敛宽度后以省略提示保持信息密度。 */
    width: min(280px, calc(100vw - 24px));
    box-sizing: border-box;
    max-height: min(264px, 56dvh);
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
    border: 0;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
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
    gap: 6px;
  }
  .ia-model-btn:hover:not(:disabled),
  .ia-model-btn.active {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
  }
  .ia-model-btn.configured {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 36%, transparent);
    color: var(--primary);
  }
  .ia-model-btn .ia-picker-btn-label {
    max-width: 132px;
    color: inherit;
  }
  .ia-model-effort {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    height: 16px;
    padding: 0 6px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: inherit;
    font-size: var(--text-xs);
    line-height: 16px;
    font-weight: calc(var(--font-semibold) + 50);
    white-space: nowrap;
  }
  .ia-workspace-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    max-width: 180px;
    min-width: 0;
    padding: 0 8px;
    background: transparent;
    border: 0;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .ia-workspace-btn:hover:not(:disabled) {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: color-mix(in srgb, var(--primary) 38%, transparent);
    color: var(--primary);
  }
  .ia-workspace-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .ia-workspace-btn.active {
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
  .ia-workspace-btn-label {
    flex: 0 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: 120px;
  }
  .ia-picker-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    right: 0;
    z-index: 31;
    width: 280px;
    max-height: 264px;
    overflow-y: auto;
    padding: 8px;
    background: color-mix(in srgb, var(--background) 100%, white 8%);
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    border: 0;
    border-radius: calc(var(--radius-sm) + 8px);
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border), var(--shadow-lg);
  }
  .ia-add-popover {
    border-radius: calc(var(--radius-sm) + 6px);
    left: 0;
    right: auto;
    width: min(380px, calc(100vw - 24px));
    max-height: min(264px, 62vh);
    padding: 6px;
  }
  .ia-add-group-label {
    padding: 7px 9px 4px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
  }
  .ia-add-item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    min-height: 40px;
    padding: 6px 8px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    text-align: left;
    cursor: pointer;
  }
  .ia-add-item:hover,
  .ia-add-item.selected {
    background: var(--surface-2);
  }
  .ia-add-item.selected {
    color: var(--primary);
  }
  .ia-add-item.disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }
  .ia-add-item-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    flex: 0 0 24px;
    color: var(--foreground-muted);
  }
  .ia-add-item-icon.goal,
  .ia-add-item.selected .ia-add-item-icon {
    color: var(--primary);
  }
  .ia-add-item-content {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
    flex: 1;
  }
  .ia-add-item-label {
    flex: 0 0 auto;
    font-size: 13px;
    font-weight: calc(var(--font-medium) + 20);
    white-space: nowrap;
  }
  .ia-add-item-description {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: 12px;
  }
  .ia-session-model-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    right: 0;
    z-index: 31;
    display: flex;
    flex-direction: column;
    width: min(260px, calc(100vw - 24px));
    max-height: 264px;
    padding: var(--space-3);
    background: color-mix(in srgb, var(--background) 100%, white 8%);
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    border: 0;
    border-radius: calc(var(--radius-sm) + var(--space-3));
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--magi-border-faint), var(--shadow-lg);
  }
  .ia-model-summary { padding: 2px 6px 4px; }
  .ia-reasoning-actions { display: flex; justify-content: center; gap: 8px; margin-top: 8px; }
  .ia-effort-summary-header { display: flex; justify-content: center; align-items: center; min-height: 24px; }
  .ia-effort-current { text-align: center; color: var(--primary); font-size: var(--text-base); font-weight: var(--font-medium); }
  .ia-summary-model { display: flex; align-items: center; justify-content: center; gap: 5px; width: 100%; padding: 2px 8px 6px; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); font: inherit; cursor: pointer; }
  .ia-summary-model span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ia-summary-model:hover { color: var(--foreground); background: var(--surface-hover); }
  .ia-reasoning-slider { padding: 4px 6px; }
  .ia-reasoning-control { position: relative; height: 26px; }
  .ia-reasoning-slider input { position: relative; z-index: 1; width: 100%; height: 26px; margin: 0; opacity: 0; cursor: pointer; }
  .ia-reasoning-slider input:disabled { cursor: wait; }
  .ia-reasoning-track { position: absolute; inset: 10px 7px; border-radius: var(--radius-full); background: var(--surface-3); box-shadow: inset 0 0 0 var(--magi-hairline) var(--border); pointer-events: none; }
  .ia-reasoning-fill { position: absolute; inset: 0 auto 0 0; width: var(--effort-progress); border-radius: inherit; background: var(--primary); transition: width 180ms cubic-bezier(.2,.8,.2,1); }
  .ia-reasoning-thumb { position: absolute; top: 50%; left: var(--effort-progress); width: 14px; height: 14px; border-radius: var(--radius-full); background: var(--primary); transform: translate(-50%, -50%); transition: left 180ms cubic-bezier(.2,.8,.2,1), transform 160ms ease, box-shadow 160ms ease; }
  .ia-reasoning-control:focus-within .ia-reasoning-thumb { box-shadow: 0 0 0 3px var(--background), 0 0 0 5px var(--primary-muted); }
  .ia-reasoning-control:has(input:active:not(:disabled)) .ia-reasoning-thumb { transform: translate(-50%, -50%) scale(1.15); }
  .ia-reasoning-ticks { display: flex; justify-content: space-between; padding: 0 7px; pointer-events: none; }
  .ia-reasoning-ticks span { width: 4px; height: 4px; border-radius: var(--radius-full); background: var(--foreground-muted); opacity: 0.4; transition: transform 180ms ease, opacity 180ms ease, background 180ms ease; }
  .ia-reasoning-ticks span.active { background: var(--primary); opacity: 1; transform: scale(1.4); }
  @media (prefers-reduced-motion: no-preference) {
    .ia-effort-value { display: inline-block; animation: ia-effort-change 160ms ease-out; }
    .ia-reasoning-ticks span.active { animation: ia-effort-tick-pop 240ms var(--magi-spring-bouncy, cubic-bezier(.2,.8,.2,1)); }
  }
  @keyframes ia-effort-change { from { opacity: .5; transform: translateY(2px); } to { opacity: 1; transform: translateY(0); } }
  @keyframes ia-effort-tick-pop { 0% { transform: scale(.7); opacity: .5; } 60% { transform: scale(1.7); opacity: 1; } 100% { transform: scale(1.4); opacity: 1; } }
  .ia-reasoning-hint { margin: 4px 0; font-size: var(--text-xs); color: var(--foreground-muted); line-height: 1.5; }
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
  .ia-picker-refresh {
    display: inline-grid;
    place-items: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    background: transparent;
    cursor: pointer;
    transition: color var(--transition-fast), background var(--transition-fast);
  }
  .ia-picker-refresh:hover:not(:disabled) {
    color: var(--foreground);
    background: var(--surface-2);
  }
  .ia-picker-refresh:disabled {
    cursor: wait;
    opacity: 0.55;
  }

  .ia-picker-popover.ia-access-popover {
    box-sizing: border-box;
    width: min(288px, calc(100vw - 20px));
    min-width: 0;
    padding: 5px;
  }
  .ia-access-popover .ia-picker-list {
    gap: 2px;
    margin-top: 0;
    padding-top: 1px;
    border-top: 0;
  }
  .ia-access-popover .ia-picker-header {
    padding: 1px 6px 4px;
  }
  .ia-picker-item.ia-access-option {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    align-items: center;
    gap: 7px;
    min-height: 42px;
    padding: 5px 7px;
    border: 0;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }
  .ia-access-option-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--foreground-muted) 8%, transparent);
    color: var(--foreground-muted);
  }
  .ia-access-option--restricted .ia-access-option-icon {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: var(--primary);
  }
  .ia-access-option--full_access .ia-access-option-icon {
    background: color-mix(in srgb, var(--warning) 13%, transparent);
    color: color-mix(in srgb, var(--warning) 84%, var(--foreground));
  }
  .ia-access-option-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .ia-access-option-heading {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }
  .ia-access-option .ia-picker-item-label {
    white-space: nowrap;
    word-break: normal;
    font-size: var(--text-xs);
    line-height: 15px;
  }
  .ia-access-option .ia-picker-item-desc {
    font-size: var(--text-xs);
    line-height: 1.25;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ia-access-recommended {
    flex: 0 0 auto;
    padding: 0 4px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: var(--primary);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    line-height: 13px;
  }
  .ia-picker-item.ia-access-option.selected {
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--primary) 5%, transparent);
  }
  .ia-picker-item.ia-access-option--full_access.selected {
    background: color-mix(in srgb, var(--warning) 10%, transparent);
    color: var(--foreground);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warning) 4%, transparent);
  }
  .ia-picker-header {
    padding: 2px 6px 6px;
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
    /* 全局 button 是 nowrap；名称与路径要在弹层内换行，不能冲出弹层。 */
    white-space: normal;
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
  .ia-workspace-action {
    color: var(--foreground);
  }
  .ia-workspace-action-label {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  .ia-picker-item-label {
    font-size: 12px;
    font-weight: var(--font-medium, 500);
    word-break: break-all;
  }
  .ia-picker-group-heading {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 7px 10px 4px;
    color: var(--foreground-muted);
    background: var(--background);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold, 600);
    overflow-wrap: anywhere;
  }
  .ia-picker-group-origin {
    margin-inline-start: 6px;
    padding: 0 5px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm, 4px);
    font-weight: var(--font-normal, 400);
  }
  .ia-picker-row .ia-picker-item-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    word-break: normal;
    min-width: 0;
  }
  .ia-picker-check {
    display: inline-flex;
    align-items: center;
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
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: 1.4;
    min-width: 0;
    max-width: 100%;
    overflow-wrap: anywhere;
    unicode-bidi: isolate;
  }
  .ia-picker-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-top: 4px;
    margin-top: 4px;
    border-top: 1px dashed var(--border-subtle);
  }
  :is(.ia-workspace-popover, .ia-source-folder-popover) .ia-picker-item { min-inline-size: 0; text-align: start; }
  :is(.ia-workspace-popover, .ia-source-folder-popover) .ia-picker-item-label,
  :is(.ia-workspace-popover, .ia-source-folder-popover) .ia-picker-item-desc { inline-size: 100%; }
  :is(.ia-workspace-popover, .ia-source-folder-popover) .ia-picker-item-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    word-break: normal;
  }
  :is(.ia-workspace-popover, .ia-source-folder-popover) .ia-picker-item-desc[dir="ltr"] {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    word-break: normal;
  }
  .ia-picker-status {
    font-size: var(--text-xs);
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

  /* 斜杠快捷引用：Goal 与 Skill 共用一套稳定的结构化展示。 */
  .ia-reference-chip-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px var(--space-3) 0;
  }
  .ia-reference-chip,
  .ia-skill-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    padding: 3px 6px 3px 10px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--primary) 16%, transparent);
    border: 0;
    color: var(--primary);
    font-size: 12px;
    line-height: 1.2;
  }
  .ia-reference-chip-goal {
    background: color-mix(in srgb, var(--success) 12%, transparent);
    border-color: color-mix(in srgb, var(--success) 34%, transparent);
    color: color-mix(in srgb, var(--success) 82%, var(--foreground));
  }
  .ia-context-reference-chip {
    background: var(--surface-2);
    border-color: var(--border-subtle);
    color: var(--foreground-secondary);
  }
  .ia-browser-annotation-number {
    display: grid;
    place-items: center;
    flex: 0 0 17px;
    width: 17px;
    height: 17px;
    border-radius: var(--radius-full);
    background: var(--info);
    color: white;
    font-size: var(--text-xs);
    font-weight: var(--font-bold);
    line-height: 1;
  }
  .ia-reference-chip-label {
    display: inline-flex;
    min-width: 0;
    overflow: hidden;
    font-weight: var(--font-medium, 500);
    white-space: nowrap;
  }
  .ia-reference-chip-desc {
    color: color-mix(in srgb, var(--primary) 72%, var(--foreground-muted));
    font-size: var(--text-xs);
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ia-reference-chip-goal .ia-reference-chip-desc {
    color: color-mix(in srgb, var(--success) 64%, var(--foreground-muted));
  }
  .ia-reference-chip-remove {
    flex: 0 0 16px;
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
  .ia-reference-chip-remove:hover {
    background: color-mix(in srgb, var(--primary) 24%, transparent);
  }

  :global(.browser-annotation-edit-modal .modal-dialog) {
    max-width: calc(100vw - 24px);
  }
  .browser-annotation-edit-form textarea {
    box-sizing: border-box;
    width: 100%;
    min-height: 92px;
    border: var(--magi-hairline) solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--background);
    color: var(--foreground);
    padding: 8px;
    font: inherit;
  }
  .browser-annotation-edit-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 10px;
  }

  .ia-slash-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 8px;
    z-index: 31;
    width: min(420px, calc(100% - 16px));
    max-height: 264px;
    padding: 6px;
    background: color-mix(in srgb, var(--background) 100%, white 6%);
    backdrop-filter: blur(18px);
    -webkit-backdrop-filter: blur(18px);
    border: 0;
    border-radius: calc(var(--radius-sm) + 6px);
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--border), var(--shadow-lg);
  }
  .ia-slash-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: 264px;
    overflow-y: auto;
  }
  .ia-slash-group-label {
    padding: 6px 9px 4px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    line-height: 1;
  }
  .ia-slash-item.disabled {
    cursor: not-allowed;
  }
  .ia-slash-item.disabled .ia-slash-item-icon,
  .ia-slash-item.disabled .ia-slash-item-label {
    opacity: 0.5;
  }
  .ia-slash-item-reason {
    color: var(--warning);
  }
  .ia-slash-item {
    display: flex;
    align-items: center;
    width: 100%;
    gap: 8px;
    min-height: 44px;
    padding: 7px 9px;
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
  .ia-slash-item-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    flex: 0 0 24px;
    border-radius: var(--radius-sm);
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 12%, transparent);
  }
  .ia-slash-item-icon.goal {
    color: color-mix(in srgb, var(--success) 82%, var(--foreground));
    background: color-mix(in srgb, var(--success) 12%, transparent);
  }
  .ia-slash-item-content {
    display: flex;
    min-width: 0;
    flex: 1;
    flex-direction: column;
    gap: 2px;
  }
  .ia-slash-item-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* 描述最多两行：skill 的说明常常较长，单行截断后根本看不出是干什么的。 */
  .ia-slash-item-description {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.35;
    overflow: hidden;
  }

  .ia-wrapper.drag-active {
    outline: 2px dashed color-mix(in srgb, var(--primary) 70%, transparent);
    outline-offset: -2px;
  }

  .ia-drop-hint {
    position: absolute;
    inset: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--primary);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    background: color-mix(in srgb, var(--background) 82%, transparent);
    border-radius: inherit;
    pointer-events: none;
  }

  .ia-queue-badge {
    display: inline-block;
    margin-right: 6px;
    padding: 0 6px;
    color: var(--primary);
    font-size: var(--text-xs);
    line-height: 16px;
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-radius: var(--radius-full, 999px);
    vertical-align: 1px;
  }

  .ia-queue-command {
    display: inline-block;
    margin-right: 6px;
    color: var(--color-orchestrator);
    font-weight: var(--font-semibold);
  }

  .ia-queue-panel {
    overflow: hidden;
  }

  .ia-queue-header-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .ia-queue-list {
    display: flex;
    flex-direction: column;
    max-height: 140px;
    overflow-y: auto;
  }

  .ia-queue-item {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: 9px;
  }

  .ia-queue-index {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--primary) 42%, var(--foreground-muted));
  }

  .ia-queue-content {
    font-size: var(--text-sm);
    line-height: 1.35;
    color: color-mix(in srgb, var(--foreground) 85%, transparent);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .ia-queue-actions {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    opacity: 1;
    pointer-events: auto;
  }

  @media (max-width: 640px) {
    .ia-container {
      /* 移动端只压缩上下留白，横向继续保持全宽自适应。 */
      padding-block: var(--space-2) calc(var(--space-2) + env(safe-area-inset-bottom));
    }

    .ia-wrapper {
      max-height: min(46vh, 264px);
      border-radius: var(--radius-lg);
    }

    .ia-access-popover {
      position: fixed;
      right: 10px;
      bottom: calc(44px + env(safe-area-inset-bottom));
      left: auto;
      width: min(288px, calc(100vw - 20px));
      min-width: 0;
    }

    .ia-picker-popover {
      width: min(280px, calc(100vw - 24px));
      max-height: min(264px, 56vh);
    }

    .ia-session-model-popover {
      position: fixed;
      right: 10px;
      bottom: calc(44px + env(safe-area-inset-bottom));
      left: auto;
      width: min(260px, calc(100vw - 20px));
    }

  }

  @container magi-composer (max-width: 800px) {
    .ia-actions {
      flex-wrap: nowrap;
      gap: 4px;
      padding: 4px 6px;
    }

    .ia-left {
      flex: 0 0 auto;
      max-width: none;
    }

    .ia-right {
      flex: 0 1 auto;
      margin-left: auto;
    }

    .ia-runtime-controls {
      gap: 3px;
    }

    .ia-submit-controls {
      flex: 0 1 auto;
      gap: 4px;
    }

    .ia-runtime-controls .ia-toolbar-divider,
    .ia-submit-controls .ia-toolbar-divider {
      display: none;
    }

    .ia-workspace-wrap {
      flex: 0 0 28px;
      width: 28px;
      max-width: 28px;
    }

    .ia-workspace-btn {
      width: 28px;
      max-width: 28px;
      padding: 0;
      justify-content: center;
    }

    .ia-workspace-btn-label,
    .ia-access-btn-label,
    .ia-model-effort {
      display: none;
    }

    .ia-access-wrap {
      flex: 0 0 28px;
      width: 28px;
      max-width: 28px;
    }

    .ia-access-wrap .ia-picker-btn {
      width: 28px;
      max-width: 28px;
      padding: 0;
      justify-content: center;
    }

  .ia-model-wrap {
      flex: 0 1 132px;
      width: auto;
      min-width: 0;
      max-width: 132px;
    }

    .ia-model-wrap .ia-picker-btn {
      width: 100%;
      max-width: 100%;
      justify-content: flex-start;
    }

    .ia-model-wrap .ia-picker-btn :global(svg) {
      margin-left: auto;
    }

    .ia-picker-btn-label {
      min-width: 0;
      max-width: 100%;
    }

    .ia-enhance {
      flex: 0 0 24px;
    }

    .ia-send {
      flex: 0 0 28px;
    }
  }

  .ia-workspace-btn:active:not(:disabled), .ia-picker-item:active {
    background: var(--surface-active, var(--surface-hover));
  }
  @media (hover: none), (pointer: coarse) {
    .ia-send { width: 44px; height: 44px; flex: 0 0 44px; }
    .ia-actions { flex-wrap: wrap; gap: 8px; }
    .ia-left { flex: 1 1 100%; flex-wrap: wrap; gap: 8px; max-width: 100%; }
    .ia-right { flex: 1 1 100%; flex-wrap: wrap; justify-content: flex-end; gap: 8px; }
    .ia-runtime-controls, .ia-submit-controls { flex-wrap: wrap; gap: 8px; }
    .ia-workspace-wrap { flex: 0 1 auto; inline-size: auto; max-inline-size: 100%; }
    .ia-workspace-btn {
      inline-size: auto; min-inline-size: 44px; max-inline-size: 100%;
      block-size: auto; min-block-size: 44px; padding: 10px 12px; line-height: 1.5;
    }
    .ia-workspace-btn-label { display: inline; max-inline-size: 160px; }
    :is(.ia-workspace-popover, .ia-source-folder-popover) .ia-picker-item { min-block-size: 44px; padding-block: 8px; }
  }
</style>
