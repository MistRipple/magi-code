<script lang="ts">
  import { onMount, type Component } from 'svelte';
  import { vscode } from '../lib/vscode-bridge';
  import {
    addToast,
    cancelTurnEditing,
    getActiveInteractionType,
    getQueuedMessages,
    isPersistedSessionId,
    messagesState,
  } from '../stores/messages.svelte';
  import type { MessageBrowserNodeSelection, QueuedMessage } from '../types/message';
  import { getAgentRunState } from '../stores/agent-run-store.svelte';
  import {
    enhanceAgentPrompt,
    fetchAgentModelList,
    getAgentSettingsBootstrap,
    isReferenceableBrowserAnnotation,
    saveAgentModelContextWindow,
    saveAgentOrchestratorSessionConfig,
    resolveAgentPath,
    updateBrowserAnnotationComment,
    type BrowserAnnotationSnapshot,
    type ResolvedAgentPath,
    type AgentSettingsBootstrapSnapshot,
    settingsBootstrapMatchesCurrentWorkspace,
  } from '../web/agent-api';
  import type { PickerWebEngineDto } from '../shared/rust-backend-types';
  import type { AgentBindingOverride } from '../web/agent-binding-context';
  import Icon from './Icon.svelte';
  import UserQuestionPanel from './UserQuestionPanel.svelte';
  import GoalRunDrawers from './GoalRunDrawers.svelte';
  import { userQuestionState } from '../stores/user-question-store.svelte';
  import Modal from './Modal.svelte';
  import ContextUsageRing from './ContextUsageRing.svelte';
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
    resolveComposerWorkspace,
    setComposerHasUnsavedInput,
    type ComposerWorkspaceOption,
  } from '../stores/composer-workspace.svelte';
  import { openWorkspaceFolderPicker } from '../stores/workspace-onboarding.svelte';
  import {
    navigateSession,
    sessionNavigationState,
    waitForSessionNavigation,
  } from '../shared/session-navigation.svelte';
  import WebModelModeChooser from './WebModelModeChooser.svelte';
  import WebModelSessionBanner from './WebModelSessionBanner.svelte';
  import ImageAttachmentTray from './ImageAttachmentTray.svelte';
  import { bindWebSavedConversation, materializeSession } from '../web/agent-api';
  import { canFetchModelList } from '../shared/model-governance';
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
    type ComposerSkillOption,
  } from '../lib/composer-actions';
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

  interface ComposerSubmissionDraft {
    text: string;
    images: SelectedImage[];
    contextReferences: ComposerContextReference[];
    browserAnnotations: SelectedBrowserAnnotation[];
    browserNodeSelections: MessageBrowserNodeSelection[];
    goalMode: boolean;
    sessionCommand: ComposerSessionCommand | null;
    skill: SkillOption | null;
  }

  // 输入框可识别的 instruction skill。来源：bootstrap 中的 skillsConfig.instructionSkills，
  // 这一组才是 `/` 唤起的指令型技能，与 customTools（已注册到工具表）的语义不同。
  type SkillOption = ComposerSkillOption;

  type ReasoningEffort = OrchestratorReasoningEffort;

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
    onSelect: (selection: { pathRef: string; displayPath: string; name: string }) => void;
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

  // 拖动调整大小相关
  let inputHeight = $state(120); // 默认高度增加到 120px
  const minHeight = 80;
  const maxHeight = 400;

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
  const IMAGE_FILE_NAME_PATTERN = /\.(png|jpe?g|gif|webp|bmp|heic|heif)$/i;
  const GENERATED_CLIPBOARD_IMAGE_NAME_PATTERN = /^(clipboard|image|pasted[-_ ]?image)\.[^.]+$/i;
  const CLIPBOARD_IMAGE_TYPE_PRIORITY = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'];

  let stopLoading = $state(false);
  let enhanceLoading = $state(false);
  let enhanceOriginalPrompt = $state<string | null>(null);
  let enhanceResultPrompt = $state<string | null>(null);
  let enhanceAbortController: AbortController | null = null;
  let enhanceRequestSeq = 0;

  // 主线模型 picker：弹窗状态 + 模型列表惰性拉取。
  // 选中后只写当前会话 orchestrator 覆盖段；全局配置仍是新会话默认值和连接凭据来源。
  let pickerOpen = $state(false);
  let pickerLoading = $state(false);
  let pickerSavingModel = $state<string | null>(null);
  let pickerSavingReasoning = $state<ReasoningEffort | null>(null);
  let pickerModels = $state<string[]>([]);
  /**
   * 从 Web 加载的引擎（`apiProtocol = chatgpt_web`，A22）。
   *
   * 与 provider 模型名并列展示在会话内主模型选择器里：选择它们会把会话绑定到
   * 引擎（`engineId`），而不是改写 provider 连接。清单来自 daemon 的引擎注册表
   * 投影，前端不保留第二份 Web 模型列表。
   */
  let pickerWebEngines = $state<PickerWebEngineDto[]>([]);
  let pickerError = $state<string | null>(null);
  let pickerLoadedOnce = false;
  let pickerModelsConfigKey = '';
  let pickerLoadPromise: Promise<void> | null = null;

  let settingsBootstrapRefreshKey = '';
  let settingsBootstrapRefreshSeq = 0;
  let workspacePickerOpen = $state(false);
  let accessProfilePickerOpen = $state(false);
  let selectedAccessProfile = $state<AccessProfile>('restricted');
  const currentPickerModel = $derived.by(() => readOrchestratorModel());
  const mainModelReady = $derived.by(() => currentPickerModel.trim().length > 0);
  /** 会话级引擎绑定：非空表示当前会话由某个引擎承载（当前只有 Web 引擎）。 */
  const currentPickerEngineId = $derived.by(() => readOrchestratorEngineId());
  const currentPickerWebEngine = $derived.by(() => (
    pickerWebEngines.find((engine) => engine.id === currentPickerEngineId) ?? null
  ));
  const currentSessionUsesWebEngine = $derived.by(() => (
    currentPickerEngineId.trim().startsWith('chatgpt-web/')
  ));
  const canSelectPickerWebEngine = $derived.by(() => (
    isDraftSession || !currentSessionHasCanonicalHistory || currentSessionUsesWebEngine
  ));
  /** 按钮上的显示名：Web 引擎用引擎显示名，其余仍用 provider 模型名。 */
  const currentPickerLabel = $derived.by(() => (
    currentPickerWebEngine?.displayName?.trim()
    // GPT Web 的“模型名”只是占位（default），界面上始终显示固定入口名。
    || (currentSessionUsesWebEngine ? i18n.t('webModel.entryName') : currentPickerModel)
  ));
  const currentPickerReasoningEffort = $derived.by(() => readOrchestratorReasoningEffort());
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
    const value = snapshot?.modelContextWindows?.[model];
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
  /**
   * 会话方向边界。已有本地 canonical 历史时不提供“导入 Web”
   * 的 UI 路径；daemon 保存入口还会再次校验，避免直接调用 API 绕过限制。
   */
  const currentSessionHasCanonicalHistory = $derived.by(() => {
    const sessionId = currentSessionId?.trim() || '';
    if (!sessionId) return false;
    // 与 daemon 的 A26 门禁保持同一口径：只有 canonical 用户消息计数才
    // 决定“已有本地历史”。不要用 timeline projection 的 artifacts 数量推断，
    // 因为其中可能包含通知、状态或非用户条目，导致 UI 比后端过早禁用 Web。
    const sessionProjections = [
      messagesState.workspaceSessionProjection.sessions,
      messagesState.personalSessionProjection.sessions,
    ];
    return sessionProjections.some((sessions) => sessions.some((session) => (
      session.id === sessionId && Number(session.messageCount ?? 0) > 0
    )));
  });
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
      : resolveComposerWorkspace(currentWorkspaceId, currentWorkspacePath, isDraftSession)
  ));
  const workspaceOptions = $derived.by(() => composerWorkspaceState.workspaces);
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
    if (composerHasDraft()) {
      scopedComposerDrafts.set(previousScopeKey, captureComposerSubmissionDraft(resolveComposerRawContent()));
    } else {
      scopedComposerDrafts.delete(previousScopeKey);
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
  const sessionInputLocked = $derived.by(() => (
    sessionNavigationState.pending !== null || messagesState.sessionHydrating
  ));

  const isSending = $derived.by(() => messagesState.isProcessing || shouldInterruptAgentRunFromComposer);
  const activeInteraction = $derived.by(() => getActiveInteractionType());
  const isInteractionBlocking = $derived.by(() => Boolean(activeInteraction));
  const queuedMessages = $derived.by(() => getQueuedMessages());
  // 叠层层级：紧挨输入框的是 1，离得越远越大（越窄）。自下而上：目标、计划、排队、提问。
  let drawersCount = $state(0);
  const queueDockLevel = $derived(drawersCount + 1);
  const questionDockLevel = $derived(drawersCount + (queuedMessages.length > 0 ? 1 : 0) + 1);
  const MAX_INPUT_CHARS = 10000;
  let inputTextareaEl = $state<HTMLDivElement | null>(null);
  let isComposing = $state(false);
  let pendingCaretOffset = $state<number | null>(null);
  const sendButtonTitle = $derived.by(() => {
    if (sendPreparing) {
      return i18n.t('input.sending');
    }
    if (isSending) {
      return i18n.t('input.followUp.queueTitle');
    }
    if (!mainModelReady) {
      return i18n.t('input.mainModelRequired');
    }
    return i18n.t('input.send');
  });
  /** GPT Web 状态条判定发送必然被 daemon 拒绝（对话已失效 / 冲突 / 被占用）。 */
  let webSendBlocked = $state(false);
  const sendDisabled = $derived.by(() => (
    sessionInputLocked || isInteractionBlocking || sendPreparing || pendingImageReadCount > 0 || !mainModelReady
    || webSendBlocked
  ));

  // 按钮双态状态 - 使用 $derived 计算
  const hasContent = $derived.by(() => {
    if (inputValue.trim().length > 0) return true;
    return selectedImages.length > 0
      || pendingImageReadCount > 0
      || selectedContextReferences.length > 0
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
    { sessionCommandDisabledReason: compactDisabledReason },
  ));

  const filteredSlashCommands = $derived.by<Array<Exclude<ComposerAction, { kind: 'resource' }>>>(() => {
    if (slashTriggerStart === null) return [];
    return filterSlashCommands(composerActions, slashFilter).filter((command) => (
      command.kind === 'goal'
        ? !selectedGoalMode
        : command.kind === 'command'
          ? command.id !== selectedSessionCommand
          : command.id !== selectedSkill?.skillId
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

  function clearComposerState(options: { preserveScopedDraft?: boolean } = {}) {
    if (!options.preserveScopedDraft && composerReferenceScopeKey) {
      scopedComposerDrafts.delete(composerReferenceScopeKey);
    }
    inputValue = '';
    selectedImages = [];
    selectedContextReferences = [];
    selectedBrowserAnnotations = [];
    selectedBrowserNodeSelections = [];
    selectedGoalMode = false;
    selectedSessionCommand = null;
    selectedSkill = null;
    addMenuOpen = false;
    contextPickerOpen = false;
    invalidateEnhanceState();
    closeSlashMenu();
  }

  function composerHasDraft(): boolean {
    return Boolean(
      inputValue.trim()
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
      text,
      images: selectedImages.map((image) => ({ ...image })),
      contextReferences: selectedContextReferences.map((reference) => ({ ...reference })),
      browserAnnotations: selectedBrowserAnnotations.map((annotation) => ({ ...annotation })),
      browserNodeSelections: selectedBrowserNodeSelections.map(cloneBrowserNodeSelection),
      goalMode: selectedGoalMode,
      sessionCommand: selectedSessionCommand,
      skill: selectedSkill ? { ...selectedSkill } : null,
    };
  }

  function composerIsPristineForSubmissionRecovery(): boolean {
    return !editingTurn
      && !resolveComposerRawContent().trim()
      && selectedImages.length === 0
      && selectedContextReferences.length === 0
      && selectedBrowserAnnotations.length === 0
      && selectedBrowserNodeSelections.length === 0
      && !selectedGoalMode
      && selectedSessionCommand === null
      && selectedSkill === null;
  }

  function restoreComposerSubmissionDraft(draft: ComposerSubmissionDraft): void {
    invalidateEnhanceState();
    inputValue = draft.text;
    pendingCaretOffset = draft.text.length;
    selectedImages = draft.images.map((image) => ({ ...image }));
    selectedContextReferences = draft.contextReferences.map((reference) => ({ ...reference }));
    selectedBrowserAnnotations = draft.browserAnnotations.map((annotation) => ({ ...annotation }));
    selectedBrowserNodeSelections = draft.browserNodeSelections.map(cloneBrowserNodeSelection);
    selectedGoalMode = draft.goalMode;
    selectedSessionCommand = draft.sessionCommand;
    selectedSkill = draft.skill ? { ...draft.skill } : null;
    queueMicrotask(focusEditor);
  }

  let loadedEditingTurnId = '';
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
      || editingTurn !== null,
    );
  });

  $effect(() => {
    const draft = editingTurn;
    if (!draft) {
      if (loadedEditingTurnId) {
        loadedEditingTurnId = '';
        clearComposerState();
      }
      return;
    }
    if (draft.turnId === loadedEditingTurnId) return;
    loadedEditingTurnId = draft.turnId;
    invalidateEnhanceState();
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
    cancelTurnEditing();
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
    if (selectedSessionCommand === null) return;
    selectedSessionCommand = null;
    addToast('info', i18n.t('input.compact.removedForAttachment'), undefined, { forceVisible: true });
  }

  function activeModeLabel(): string | null {
    if (selectedGoalMode) return i18n.t('input.goalMode.name');
    if (selectedSessionCommand) return i18n.t('input.compact.name');
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
    addToast('success', i18n.t('input.imageAdded'));
    return true;
  }

  function startBrowserAnnotationEditing(annotation: SelectedBrowserAnnotation): void {
    editingBrowserAnnotation = annotation;
    editingBrowserAnnotationComment = annotation.comment;
  }

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
    if (command.kind === 'goal') {
      selectedGoalMode = true;
      selectedSessionCommand = null;
      selectedSkill = null;
    } else if (command.kind === 'command') {
      selectedGoalMode = false;
      selectedSessionCommand = command.id;
      selectedSkill = null;
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
    if (action.kind === 'goal') {
      selectedGoalMode = !selectedGoalMode;
      selectedSessionCommand = null;
      selectedSkill = null;
    } else if (action.kind === 'command') {
      selectedGoalMode = false;
      selectedSessionCommand = selectedSessionCommand === action.id ? null : action.id;
      selectedSkill = null;
    } else {
      selectedGoalMode = false;
      selectedSessionCommand = null;
      selectedSkill = selectedSkill?.skillId === action.skill.skillId ? null : action.skill;
    }
    closeAddMenu();
    queueMicrotask(focusEditor);
  }

  function handleContextReferenceSelected(
    selection: { pathRef: string; displayPath: string; name: string },
  ) {
    addContextReference({
      kind: 'directory',
      path: selection.displayPath,
      pathRef: selection.pathRef,
      name: selection.name,
    });
    contextPickerOpen = false;
    queueMicrotask(focusEditor);
  }

  function addContextReference(input: {
    kind: ComposerContextReferenceKind;
    path: string;
    pathRef?: string;
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

  function removeContextReference(referenceId: string) {
    selectedContextReferences = selectedContextReferences.filter((reference) => (
      reference.id !== referenceId
    ));
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

  function workspaceNavigationPath(workspace: ComposerWorkspaceOption): string {
    // 会话和消息归属使用真实工作区路径，路径引用仅用于文件系统 API。
    return workspace.rootPath.trim();
  }

  function composerWorkspaceLabel(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return i18n.locale.startsWith('zh') ? 'Magi 空间' : 'Magi Space';
    return workspace.name || workspace.rootPath;
  }

  function composerWorkspaceTitle(workspace: ComposerWorkspaceOption | null): string {
    if (!workspace) return i18n.locale.startsWith('zh') ? 'Magi 内置空间' : 'Built-in Magi Space';
    return `${i18n.t('input.workspace.title')}: ${workspace.rootPath}`;
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

  function resolveSubmissionWorkspace(): ComposerWorkspaceOption | null {
    if (!currentWorkspaceId?.trim() && !currentWorkspacePath?.trim()) {
      return null;
    }
    return resolveComposerWorkspace(currentWorkspaceId, currentWorkspacePath, isDraftSession);
  }

  onMount(() => {
    selectedAccessProfile = readStoredAccessProfile();

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
      addToast('success', i18n.t('input.access.switched', {
        mode: i18n.t(accessProfileOptions.find((option) => option.value === profile)?.labelKey ?? 'input.access.restricted'),
      }));
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
    function handleOpenWebModelPicker(): void {
      if (sessionInputLocked || isInteractionBlocking) return;
      pickerOpen = true;
      if (!pickerLoadedOnce && !pickerLoading) {
        void loadPickerModels();
      }
    }
    function handleFocusWebModelComposer(): void {
      queueMicrotask(focusEditor);
    }
    function handlePickerOutsidePointerDown(event: PointerEvent) {
      const target = event.target;
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
    window.addEventListener('magi:webModelOpenModelPicker', handleOpenWebModelPicker);
    window.addEventListener('magi:webModelFocusComposer', handleFocusWebModelComposer);
    window.addEventListener(DESKTOP_CONTEXT_DROP_EVENT, handleDesktopContextDrop as EventListener);
    window.addEventListener('storage', handleStoredAccessProfileChange);
    document.addEventListener('pointerdown', handlePickerOutsidePointerDown, true);
    return () => {
      setComposerHasUnsavedInput(false);
      window.removeEventListener('magi:fillComposer', handleFillComposer as EventListener);
      window.removeEventListener('magi:setAccessProfile', handleSetAccessProfile as EventListener);
      window.removeEventListener('magi:browserAnnotationCreated', handleBrowserAnnotationCreated as EventListener);
      window.removeEventListener('magi:browserAnnotationUpdated', handleBrowserAnnotationUpdated as EventListener);
      window.removeEventListener('magi:browserNodeSelected', handleBrowserNodeSelected as EventListener);
      window.removeEventListener('magi:browserNodeSelectionInvalidated', handleBrowserNodeSelectionInvalidated as EventListener);
      window.removeEventListener('magi:browserScreenshotCaptured', handleBrowserScreenshotCaptured as EventListener);
      window.removeEventListener('magi:sessionTurnSubmissionSettled', handleSessionTurnSubmissionSettled as EventListener);
      window.removeEventListener('magi:webModelOpenModelPicker', handleOpenWebModelPicker);
      window.removeEventListener('magi:webModelFocusComposer', handleFocusWebModelComposer);
      window.removeEventListener(DESKTOP_CONTEXT_DROP_EVENT, handleDesktopContextDrop as EventListener);
      window.removeEventListener('storage', handleStoredAccessProfileChange);
      document.removeEventListener('pointerdown', handlePickerOutsidePointerDown, true);
    };
  });

  $effect(() => {
    const orchestratorConfig = getOrchestratorConfigSnapshot();
    const configKey = orchestratorModelListConfigKey(orchestratorConfig);
    if (!configKey) {
      pickerModels = [];
      pickerModelsConfigKey = '';
      pickerLoadedOnce = false;
      pickerError = null;
      return;
    }
    if (pickerModelsConfigKey && pickerModelsConfigKey !== configKey) {
      pickerModels = [];
      pickerModelsConfigKey = '';
      pickerLoadedOnce = false;
      pickerError = null;
    }
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
  // 空闲时直接执行；正在响应时自动进入排队，由 bridge 在当前轮结束后逐条提交。
  async function sendMessage() {
    if (sendPreparing) return;
    sendPreparing = true;
    try {
      await waitForPendingImageReads();
      const rawContent = resolveComposerRawContent();
      const normalizedContent = rawContent.trim();
      if (
        (!normalizedContent && selectedSessionCommand === null && selectedImages.length === 0 && selectedContextReferences.length === 0 && selectedBrowserAnnotations.length === 0 && selectedBrowserNodeSelections.length === 0)
        || sessionInputLocked
        || isInteractionBlocking
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

      const orchestratorSessionConfig = resolveTurnOrchestratorSessionConfigPayload();
      if (!orchestratorSessionConfig) {
        return;
      }

      const requestId = generateId();
      const replaceTurnId = editingTurn?.sessionId === messagesState.currentSessionId
        ? editingTurn.turnId
        : null;
      if (!replaceTurnId) {
        submittedComposerDrafts.set(requestId, captureComposerSubmissionDraft(rawContent));
      }
      vscode.postMessage({
        type: 'executeTask',
        text: submissionText,
        requestId,
        workspaceId: targetWorkspace?.workspaceId || '',
        workspacePath: targetWorkspace ? workspaceNavigationPath(targetWorkspace) : '',
        sessionId: isDraftSession ? '' : (messagesState.currentSessionId || ''),
        skillName: selectedSkill?.skillId ?? null,
        goalMode: selectedGoalMode,
        command: selectedSessionCommand,
        accessProfile: selectedAccessProfile,
        ...(isDraftSession ? { orchestratorSessionConfig } : {}),
        followUpMode: !replaceTurnId && !isDraftSession && isSending ? 'queue' : undefined,
        replaceTurnId,
        images: selectedImages.map((img) => ({
          name: img.name,
          dataUrl: img.dataUrl,
        })),
        contextReferences: toSessionContextReferencePayload(selectedContextReferences),
        browserAnnotationRefs: selectedBrowserAnnotations.map((annotation) => annotation.annotationId),
        browserNodeSelections: selectedBrowserNodeSelections.map(cloneBrowserNodeSelection),
      });
      if (!replaceTurnId) {
        clearComposerState();
      }
    } finally {
      sendPreparing = false;
    }
  }

  function insertNewlineAtCursor() {
    invalidateEnhanceState();
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

  function isNewlineShortcut(event: KeyboardEvent): boolean {
    if (!isEnterKey(event) || event.metaKey || event.ctrlKey) {
      return false;
    }
    // Shift+Enter 是统一主快捷键；保留既有的 Option/Alt+Enter，避免 macOS 用户升级后失去原有操作。
    return (event.shiftKey && !event.altKey) || (event.altKey && !event.shiftKey);
  }

  // 处理键盘事件
  function handleKeydown(event: KeyboardEvent) {
    if (isNewlineShortcut(event)) {
      // 输入法组合态下回车只用于上屏，不能插入换行或触发发送。
      if (event.isComposing || event.keyCode === 229) {
        return;
      }
      event.preventDefault();
      closeSlashMenu();
      insertNewlineAtCursor();
      return;
    }
    if (slashMenuOpen) {
      // 斜杠菜单展开时优先处理导航；输入法组合态下不拦截，交给 IME 完成上屏。
      if (!event.isComposing && event.keyCode !== 229) {
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
    }
    if (isEnterKey(event)) {
      // 输入法组合态下回车只用于上屏，不能误触发发送
      if (event.isComposing || event.keyCode === 229) {
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

  // 代理运行运行时，输入框停止入口与目标面板共用同一条可恢复中断链路。
  async function stopTask() {
    if (stopLoading) return;
    stopLoading = true;
    try {
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
    vscode.postMessage({
      type: 'removeQueuedMessage',
      queuedMessageId: normalizedId,
    });
  }

  function canGuideQueuedMessage(queued: QueuedMessage): boolean {
    return queued.canGuide === true;
  }

  function guideQueuedMessageTitle(queued: QueuedMessage): string {
    if (queued.canGuide !== true) {
      return i18n.t('input.queue.guideUnavailable');
    }
    return i18n.t('input.queue.guideTitle');
  }

  function guideQueuedMessage(queuedMessageId: string) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId) return;
    vscode.postMessage({
      type: 'guideQueuedMessage',
      queuedMessageId: normalizedId,
    });
  }

  // 编辑 = 把排队消息整条取回输入框（文字、图片、引用、命令、目标 / skill），队列移除仍由服务端权威请求完成，
  // 失败时原消息会保留并重新同步。输入框里已有草稿时不覆盖，更不能悄悄丢掉排队消息里的附件。
  function editQueuedMessage(queuedMessageId: string) {
    const normalizedId = typeof queuedMessageId === 'string' ? queuedMessageId.trim() : '';
    if (!normalizedId) return;
    const target = messagesState.queuedMessages.find((message) => message.id === normalizedId);
    if (!target) return;
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
    vscode.postMessage({
      type: 'removeQueuedMessage',
      queuedMessageId: normalizedId,
    });
    restoreComposerSubmissionDraft({
      text: (target.text ?? target.content ?? '').toString(),
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
    });
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
      addImageFiles(imageFiles);
      return;
    }

    // 纯文本路径：阻止浏览器把 HTML 粘进 contenteditable，统一按 \n 文本插入
    const text = event.clipboardData?.getData('text/plain');
    if (typeof text !== 'string' || text.length === 0) return;
    event.preventDefault();
    invalidateEnhanceState();
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
      typeof config.model === 'string' && config.model.trim()
        ? {}
        : { model },
    );
  }

  function resolveTurnOrchestratorSessionConfigPayload(): Record<string, unknown> | null {
    const model = readOrchestratorModel();
    if (!model) {
      pickerOpen = true;
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

  /**
   * 会话级引擎绑定。空串代表「继承 provider 连接」，不是错误值：
   * settings 侧的空串等价于显式解绑（`canonicalize_session_orchestrator_section`）。
   */
  function readOrchestratorEngineId(): string {
    const config = getCurrentOrchestratorSessionConfigSnapshot();
    const value = config.engineId;
    return typeof value === 'string' ? value.trim() : '';
  }

  function readOrchestratorReasoningEffort(): ReasoningEffort {
    return resolveOrchestratorReasoningEffort(
      getCurrentOrchestratorSessionConfigSnapshot(),
      getEffectiveOrchestratorConfigSnapshot(),
    );
  }

  function reasoningEffortLabel(value: ReasoningEffort): string {
    const match = reasoningOptions.find((option) => option.value === value);
    return match ? i18n.t(match.labelKey) : '';
  }

  /**
   * Web 引擎的强度档位按引擎取值域渲染，其余置灰。
   * 非 Web 引擎（provider 模型）保持既有四档行为不变。
   */
  /** 只有 daemon 判为可用的 GPT Web 入口可选；未登录时入口根本不会出现。 */
  function pickerWebEngineUsable(engine: PickerWebEngineDto): boolean {
    return (engine.status ?? 'available').trim() === 'available';
  }

  function pickerWebEngineStatusText(engine: PickerWebEngineDto): string {
    if (!canSelectPickerWebEngine) {
      return i18n.t('input.mainModelPicker.webLocalSessionBlocked');
    }
    if (isPersonalSession) {
      return i18n.t('webModel.tools.workspaceRequired');
    }
    if (engine.tools && !engine.tools.available) {
      return i18n.t('webModel.tools.unavailable');
    }
    return i18n.t('webModel.tools.available');
  }

  function objectRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {};
  }

  function orchestratorModelListConfigKey(config: Record<string, unknown> | null): string {
    if (!config || !canFetchModelList(config)) return '';
    const baseUrl = typeof config.baseUrl === 'string' ? config.baseUrl.trim() : '';
    const apiKey = typeof config.apiKey === 'string' ? config.apiKey.trim() : '';
    const urlMode = typeof config.urlMode === 'string' ? config.urlMode.trim() : '';
    const apiProtocol = typeof config.apiProtocol === 'string' ? config.apiProtocol.trim() : '';
    return JSON.stringify({ baseUrl, apiKey, urlMode, apiProtocol });
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
  ) {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!snapshot || !settingsBootstrapMatchesCurrentWorkspace(snapshot)) return;
    messagesState.settingsBootstrapSnapshot = {
      ...snapshot,
      orchestratorSessionDefaults: { ...sessionConfig },
      orchestratorSessionConfig: { ...sessionConfig },
      effectiveOrchestratorConfig: { ...effectiveConfig },
    } as AgentSettingsBootstrapSnapshot;
  }

  function applyOrchestratorSessionDefaults(defaults: Record<string, unknown>) {
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (snapshot && settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      const currentSessionConfig = objectRecord(snapshot.orchestratorSessionConfig);
      const currentModel = typeof currentSessionConfig.model === 'string'
        ? currentSessionConfig.model.trim()
        : '';
      const resolvedSessionConfig = currentModel
        ? currentSessionConfig
        : { ...defaults, ...currentSessionConfig };
      messagesState.settingsBootstrapSnapshot = {
        ...snapshot,
        orchestratorSessionDefaults: { ...defaults },
        ...(!currentModel ? {
          orchestratorSessionConfig: { ...resolvedSessionConfig },
          effectiveOrchestratorConfig: {
            ...snapshot.orchestratorConfig,
            ...resolvedSessionConfig,
          },
        } : {}),
      } as AgentSettingsBootstrapSnapshot;
    }
    if (isDraftSession) {
      messagesState.draftOrchestratorSessionConfig = { ...defaults };
    }
  }

  async function refreshPickerSettingsSnapshot() {
    const latest = await getAgentSettingsBootstrap({ bootstrapScope: 'core', accessProfile: selectedAccessProfile });
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
    } else {
      // provider 模型列表按配置缓存，但 GPT Web 入口是 daemon 的实时投影（登录 / 探测结论会变）：
      // 每次打开都重新读，否则探测完成前打开过一次，入口就一直缺失。
      await refreshPickerWebEngines();
    }
  }

  async function refreshPickerWebEngines() {
    try {
      const payload = await fetchAgentModelList(
        (getOrchestratorConfigSnapshot() ?? {}) as Record<string, unknown>,
        'orch',
      );
      pickerWebEngines = Array.isArray(payload.webEngines) ? payload.webEngines : [];
    } catch (error) {
      console.warn('[InputArea] 刷新 GPT Web 入口失败:', error);
    }
  }
  async function loadPickerModels() {
    if (pickerLoadPromise) {
      await pickerLoadPromise;
      return;
    }
    const loadPromise = (async () => {
      // GPT Web 是独立的模型来源，不要求用户先配置 provider 的
      // baseUrl/apiKey。空配置仍通过同一个 models/fetch 响应取回 daemon
      // 投影的 Web 引擎；有 provider 配置时则同时返回两类模型。
      const orchestratorConfig = getOrchestratorConfigSnapshot() ?? {};
      const configKey = orchestratorModelListConfigKey(orchestratorConfig);
      if (pickerLoadedOnce && pickerModelsConfigKey === configKey && pickerModels.length > 0) {
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
        // Web 引擎只来自 daemon 的可用性投影：未登录、未确认说明或
        // 引擎不在本次探测候选里时这里就是空数组，前端不保留上一次的旧列表。
        pickerWebEngines = Array.isArray(payload.webEngines) ? payload.webEngines : [];
        applyOrchestratorSessionDefaults(
          objectRecord(payload.orchestratorSessionDefaults),
        );
        pickerModelsConfigKey = configKey;
        pickerLoadedOnce = true;
      } catch (error) {
        pickerModelsConfigKey = configKey;
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
    if (pickerLoading) return;
    pickerLoadedOnce = false;
    await loadPickerModels();
  }
  async function selectPickerModel(model: string) {
    const normalizedModel = model.trim();
    if (!normalizedModel) return;
    // Web 引擎的 `model` 是其族名，可能与 provider 列表中的模型同名；
    // 只要仍有 Web `engineId`，点击 provider 条目就必须继续执行单向 Web → 本地。
    if (normalizedModel === currentPickerModel && !currentPickerEngineId) {
      pickerOpen = false;
      return;
    }
    const sessionId = currentSessionId?.trim() || '';
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const binding: AgentBindingOverride = workspaceId || workspacePath
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
    const reasoningEffort = readOrchestratorReasoningEffort();
    if (!sessionId) {
      messagesState.draftOrchestratorSessionConfig = withOrchestratorReasoningEffort(
        messagesState.draftOrchestratorSessionConfig,
        reasoningEffort,
        // 切回本地模型：GPT Web 的对话方式随之丢弃，避免首条消息被 daemon 以“非 GPT Web 会话”拒绝。
        { model: normalizedModel, engineId: '', webMode: undefined },
      );
      pickerError = null;
      pickerOpen = false;
      return;
    }
    pickerSavingModel = normalizedModel;
    pickerError = null;
    // 选择 provider 模型即显式解绑引擎：否则用户以为切走了，实际仍由 Web 引擎承载。
    const nextSessionConfig = withOrchestratorReasoningEffort(
      getOrchestratorSessionConfigSnapshot(),
      reasoningEffort,
      { model: normalizedModel, engineId: '' },
    );
    try {
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, binding);
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

  /**
   * 选中一个从 Web 加载的引擎。
   *
   * 与选择 provider 模型的区别只有两点：
   * - 写入会话级 `engineId` 绑定；`model` 写引擎自身的族名（与 daemon 从
   *   `chatgpt-web/<family>` 推导的值一致），避免会话里残留 provider 模型名；
   * - 强度收敛到该引擎 `efforts` 的取值域，不支持时落到引擎的第一个可用档位。
   */
  /**
   * GPT Web 对话方式（临时 / 已保存）。方式只在首条消息前可以设置，daemon 把它落到会话级
   * `webConversation` 绑定；这里只保留“选择器当前显示哪一项”的 UI 状态。
   */
  const webConversationMode = $derived.by((): 'temporary' | 'saved' => {
    if (isDraftSession) {
      return messagesState.draftOrchestratorSessionConfig?.webMode === 'saved' ? 'saved' : 'temporary';
    }
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) return 'temporary';
    return snapshot?.webConversation?.mode === 'saved' ? 'saved' : 'temporary';
  });

  function currentWebBinding(sessionId: string): AgentBindingOverride {
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    return workspaceId || workspacePath
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
  }

  async function applyWebConversationMode(mode: 'temporary' | 'saved'): Promise<void> {
    if (mode === webConversationMode) return;
    const sessionId = currentSessionId?.trim() || '';
    if (!sessionId) {
      // 草稿会话：随首条消息创建会话时由 daemon 落库。
      messagesState.draftOrchestratorSessionConfig = {
        ...messagesState.draftOrchestratorSessionConfig,
        webMode: mode,
      };
      return;
    }
    try {
      await saveAgentOrchestratorSessionConfig(
        { ...getOrchestratorSessionConfigSnapshot(), webMode: mode },
        currentWebBinding(sessionId),
      );
      await refreshPickerSettingsSnapshot();
    } catch (error) {
      console.warn('[InputArea] 设置 GPT Web 对话方式失败:', error);
      addToast('error', i18n.t('webModel.mode.saveFailed'));
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
    if (!navigation) throw new Error(i18n.t('webModel.saved.bindFailed'));
    await waitForSessionNavigation(navigation);
    return sessionId;
  }

  /**
   * 把已保存的 ChatGPT 对话绑定到当前（空白）会话。草稿会话先物化为真实会话并写入
   * GPT Web 引擎，再绑定；历史由 daemon 单向导入。
   */
  async function bindSavedWebConversation(conversationId: string): Promise<void> {
    const wasDraft = !(currentSessionId?.trim());
    const sessionId = await materializeDraftSession();
    if (wasDraft) {
      const engineId = currentPickerEngineId || 'chatgpt-web/default';
      await saveAgentOrchestratorSessionConfig(
        withOrchestratorReasoningEffort(
          messagesState.draftOrchestratorSessionConfig,
          readOrchestratorReasoningEffort(),
          { engineId, model: engineId.slice('chatgpt-web/'.length) },
        ),
        currentWebBinding(sessionId),
      );
    }
    await bindWebSavedConversation(sessionId, conversationId);
    await refreshPickerSettingsSnapshot();
    addToast('success', i18n.t('webModel.saved.bound'));
  }

  async function selectPickerWebEngine(engine: PickerWebEngineDto) {
    if (!canSelectPickerWebEngine) {
      const message = i18n.t('input.mainModelPicker.webLocalSessionBlocked');
      pickerError = message;
      addToast('warning', message);
      return;
    }
    const engineId = engine.id.trim();
    if (!engineId) return;
    if (engineId === currentPickerEngineId) {
      pickerOpen = false;
      return;
    }
    const family = engineId.startsWith('chatgpt-web/')
      ? engineId.slice('chatgpt-web/'.length)
      : (engine.displayName || engineId);
    // 强度由网页自己选择，Magi 不改写会话的 reasoningEffort。
    const reasoningEffort = readOrchestratorReasoningEffort();
    const patch = { engineId, model: family };
    const sessionId = currentSessionId?.trim() || '';
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const binding: AgentBindingOverride = workspaceId || workspacePath
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
    if (!sessionId) {
      messagesState.draftOrchestratorSessionConfig = withOrchestratorReasoningEffort(
        messagesState.draftOrchestratorSessionConfig,
        reasoningEffort,
        patch,
      );
      pickerError = null;
      pickerOpen = false;
      return;
    }
    pickerSavingModel = engineId;
    pickerError = null;
    const nextSessionConfig = withOrchestratorReasoningEffort(
      getOrchestratorSessionConfigSnapshot(),
      reasoningEffort,
      patch,
    );
    try {
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, binding);
      applyLocalOrchestratorSessionConfig(
        objectRecord(saved.orchestratorSessionConfig),
        objectRecord(saved.effectiveOrchestratorConfig),
      );
      try {
        await refreshPickerSettingsSnapshot();
      } catch (error) {
        console.warn('[InputArea] 切换 Web 引擎后刷新设置快照失败:', error);
        addToast('warning', i18n.t('input.modelSavedSyncPending'));
      }
      addToast('success', i18n.t('input.modelSwitched', { model: engine.displayName || engineId }));
      pickerOpen = false;
    } catch (error) {
      console.warn('[InputArea] 保存 Web 引擎绑定失败:', error);
      pickerError = i18n.t('input.modelSaveFailed');
      addToast('error', pickerError);
    } finally {
      pickerSavingModel = null;
    }
  }

  async function selectPickerReasoningEffort(value: ReasoningEffort) {
    const sessionId = currentSessionId?.trim() || '';
    const workspaceId = currentWorkspaceId?.trim() || '';
    const workspacePath = currentWorkspacePath?.trim() || '';
    const binding: AgentBindingOverride = workspaceId || workspacePath
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
    if (!sessionId) {
      applyDraftOrchestratorSessionPatch({ reasoningEffort: value });
      pickerError = null;
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
      const saved = await saveAgentOrchestratorSessionConfig(nextSessionConfig, binding);
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

  async function saveCurrentModelContextWindow(contextWindowTokens: number): Promise<void> {
    const model = currentPickerModel.trim();
    if (!model) {
      throw new Error(i18n.t('input.contextRing.modelRequired'));
    }
    const saved = await saveAgentModelContextWindow(model, contextWindowTokens);
    const snapshot = messagesState.settingsBootstrapSnapshot;
    if (snapshot && settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
      messagesState.settingsBootstrapSnapshot = {
        ...snapshot,
        modelContextWindows: objectRecord(saved.modelContextWindows) as Record<string, number>,
      } as AgentSettingsBootstrapSnapshot;
    }
    addToast('success', i18n.t('input.contextRing.saved', { model }));
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
      addToast('success', i18n.t('input.enhance.success'));
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
    addToast('info', i18n.t('input.enhance.restored'));
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
          {@const guideAvailable = canGuideQueuedMessage(queued)}
          {@const attachments = summarizeQueuedMessage(queued)}
          {@const commandLabel = resolveUserMessageCommandLabel({
            sessionCommand: queued.command,
            goalMode: queued.goalMode,
            skillName: queued.skillName,
          })}
          <div class="ia-queue-item dock-row">
            <span class="ia-queue-index" aria-hidden="true"></span>
            <div class="ia-queue-content" title={queued.content}>
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
            </div>
            <div class="ia-queue-actions">
              <button
                type="button"
                class="ia-queue-action ia-queue-guide dock-btn"
                disabled={!guideAvailable}
                onclick={() => guideQueuedMessage(queued.id)}
                title={guideQueuedMessageTitle(queued)}
                aria-label={guideQueuedMessageTitle(queued)}
              >
                <Icon name="corner-down-right" size={12} />
                <span>{i18n.t('input.queue.guide')}</span>
              </button>
              <button
                type="button"
                class="ia-queue-action dock-icon-btn"
                onclick={() => editQueuedMessage(queued.id)}
                title={i18n.t('input.queue.edit')}
                aria-label={i18n.t('input.queue.edit')}
              >
                <Icon name="edit" size={12} />
              </button>
              <button
                type="button"
                class="ia-queue-action dock-icon-btn dock-icon-btn--danger"
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
  <GoalRunDrawers bind:count={drawersCount} />
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="ia-wrapper"
    class:drag-active={imageDragActive}
    style="min-height: {inputHeight}px"
    ondragover={handleImageDragOver}
    ondragleave={handleImageDragLeave}
    ondrop={handleImageDrop}
  >
    {#if imageDragActive}
      <div class="ia-drop-hint" aria-hidden="true">{i18n.t('input.drop.hint')}</div>
    {/if}
    <!-- 拖动调整大小 -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="ia-resize" onmousedown={startResize}></div>

    {#if editingTurn}
      <div class="ia-editing-bar" role="status">
        <span>{i18n.t('input.editingPreviousMessage')}</span>
        <button type="button" onclick={cancelMessageEditing}>{i18n.t('input.cancelEditing')}</button>
      </div>
    {/if}

    <!-- 快捷引用保持结构化状态，不把 /goal 或 Skill 名称注入用户正文。 -->
    {#if selectedContextReferences.length > 0 || selectedBrowserAnnotations.length > 0 || selectedBrowserNodeSelections.length > 0 || selectedGoalMode || selectedSessionCommand || selectedSkill}
      <div class="ia-reference-chip-row">
        {#each selectedContextReferences as reference (reference.id)}
          <span class="ia-reference-chip ia-context-reference-chip" title={reference.path}>
            <Icon name={reference.kind === 'directory' ? 'folder' : 'document'} size={11} />
            <span class="ia-reference-chip-label">{reference.name}</span>
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
              onclick={() => { selectedBrowserAnnotations = selectedBrowserAnnotations.filter((item) => item.annotationId !== annotation.annotationId); }}
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

    <WebModelSessionBanner
      usesWeb={currentSessionUsesWebEngine}
      sessionId={currentSessionId?.trim() || ''}
      projection={messagesState.settingsBootstrapSnapshot?.webConversation}
      turnActive={messagesState.isProcessing}
      toolsAvailable={currentPickerWebEngine?.tools ? currentPickerWebEngine.tools.available : null}
      toolsDetail={currentPickerWebEngine?.tools?.detail}
      workspaceRequired={isPersonalSession}
      bind:blocksSend={webSendBlocked}
      onOwnershipLost={() => void refreshPickerSettingsSnapshot()}
    />

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
      contenteditable={!(sessionInputLocked || isInteractionBlocking)}
      role="textbox"
      tabindex={sessionInputLocked || isInteractionBlocking ? -1 : 0}
      aria-multiline="true"
      aria-disabled={sessionInputLocked || isInteractionBlocking}
      data-placeholder={selectedGoalMode
        ? i18n.t('input.placeholderWithGoal')
        : selectedSessionCommand
          ? i18n.t('input.placeholderWithCompact')
        : selectedSkill
          ? i18n.t('input.placeholderWithSkill', { skillName: selectedSkill.name })
        : selectedImages.length > 0
          ? i18n.t('input.placeholderWithImages')
        : selectedContextReferences.length > 0
          ? i18n.t('input.placeholderWithReferences')
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
                <Icon name={command.kind === 'goal' ? 'infinity' : command.kind === 'command' ? 'refresh' : 'skill'} size={12} />
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
        <div class="ia-slash-hint" aria-hidden="true">{i18n.t('input.slash.hint')}</div>
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
        <div class="ia-picker-wrap ia-workspace-wrap">
          <button
            type="button"
            class="ia-workspace-btn"
            class:active={workspacePickerOpen}
            class:configured={composerWorkspace !== null}
            class:locked={!isDraftSession}
            onclick={() => {
              if (isDraftSession) {
                workspacePickerOpen = !workspacePickerOpen;
              }
            }}
            disabled={sessionInputLocked || isInteractionBlocking}
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
            <div class="ia-picker-popover ia-workspace-popover" data-magi-surface="popover" role="menu">
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
              <div class="ia-picker-divider"></div>
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
            </div>
          {/if}
        </div>
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
              <div class="ia-picker-popover ia-access-popover" data-magi-surface="popover" role="menu">
                <div class="ia-picker-header">{i18n.t('input.access.title')}</div>
                <div class="ia-picker-list">
                  {#each accessProfileOptions as option (option.value)}
                    <button
                      type="button"
                      class="ia-picker-item ia-access-option ia-access-option--{option.value}"
                      class:selected={selectedAccessProfile === option.value}
                      onclick={() => selectAccessProfile(option.value)}
                      role="menuitemradio"
                      aria-checked={selectedAccessProfile === option.value}
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
            {#if currentPickerEngineId}
              <!-- 按钮空间有限（右栏打开时会截断名称）：来源徽标只放在下拉列表里，这里只留标记属性。 -->
              <span class="ia-model-web-marker" data-magi-web-engine={currentPickerEngineId} hidden></span>
            {/if}
            {#if currentPickerReasoningLabel && !currentSessionUsesWebEngine}
              <span class="ia-model-effort">{currentPickerReasoningLabel}</span>
            {/if}
            <Icon name="chevron-down" size={10} />
          </button>
          {#if pickerOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="ia-popover-backdrop" onclick={() => (pickerOpen = false)}></div>
            <div class="ia-session-model-popover" data-magi-surface="popover" role="menu">
              {#if !currentSessionUsesWebEngine}
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
              {/if}
              <div class="ia-model-list-section">
                <div class="ia-section-header-row">
                  <div class="ia-picker-header">{i18n.t('input.mainModelPicker.header')}</div>
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
                {:else if pickerModels.length === 0 && pickerWebEngines.length === 0}
                  <div class="ia-picker-status">{i18n.t('input.mainModelPicker.empty')}</div>
                {:else}
                  <div class="ia-picker-list">
                    {#each pickerModels as model (model)}
                      <button
                        type="button"
                        class="ia-picker-item ia-picker-row"
                        class:selected={!currentPickerEngineId && currentPickerModel === model}
                        onclick={() => void selectPickerModel(model)}
                        disabled={pickerSavingModel !== null || pickerSavingReasoning !== null}
                      >
                        <span class="ia-picker-item-label">{model}</span>
                        {#if pickerSavingModel === model}
                          <Icon name="loader" size={12} class="spinning" />
                        {:else if !currentPickerEngineId && currentPickerModel === model}
                          <span class="ia-picker-check">✓</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                  {#if pickerWebEngines.length > 0}
                    <div class="ia-picker-divider"></div>
                    <div class="ia-picker-header">{i18n.t('input.mainModelPicker.webSection')}</div>
                    <div class="ia-picker-list">
                      {#each pickerWebEngines as engine (engine.id)}
                        <button
                          type="button"
                          class="ia-picker-item ia-picker-row"
                          class:selected={currentPickerEngineId === engine.id}
                          data-magi-web-engine-row={engine.id}
                          onclick={() => void selectPickerWebEngine(engine)}
                          disabled={pickerSavingModel !== null
                            || pickerSavingReasoning !== null
                            || !pickerWebEngineUsable(engine)
                            || !canSelectPickerWebEngine}
                          title={pickerWebEngineStatusText(engine)}
                        >
                          <span class="ia-picker-item-label">{engine.displayName || engine.id}</span>
                          <span class="ia-model-web-badge">{i18n.t('webModel.badge.fromWeb')}</span>
                          {#if isPersonalSession || (engine.tools && !engine.tools.available)}
                            <span class="ia-model-tier" data-web-model-tools="off">
                              {i18n.t('webModel.tools.unavailableShort')}
                            </span>
                          {/if}
                          {#if pickerSavingModel === engine.id}
                            <Icon name="loader" size={12} class="spinning" />
                          {:else if currentPickerEngineId === engine.id}
                            <span class="ia-picker-check">✓</span>
                          {/if}
                        </button>
                      {/each}
                    </div>
                  {/if}
                {/if}
                {#if currentSessionUsesWebEngine && !currentSessionHasCanonicalHistory}
                  <WebModelModeChooser
                    mode={webConversationMode}
                    disabled={pickerSavingModel !== null || pickerSavingReasoning !== null}
                    onModeChange={applyWebConversationMode}
                    onBindSaved={bindSavedWebConversation}
                  />
                {/if}
                {#if currentSessionUsesWebEngine && currentSessionHasCanonicalHistory}
                  <!-- 首条消息之后对话方式固定，只读显示，不能互相转换。 -->
                  <p class="ia-picker-notice" data-web-model-mode-locked={webConversationMode}>
                    {i18n.t('webModel.mode.locked', {
                      mode: webConversationMode === 'saved'
                        ? i18n.t('webModel.mode.saved')
                        : i18n.t('webModel.mode.temporary'),
                    })}
                  </p>
                {/if}
                {#if currentPickerEngineId}
                  <p class="ia-picker-notice">{i18n.t('input.mainModelPicker.webNotice')}</p>
                {/if}
                {#if pickerWebEngines.length > 0 && !canSelectPickerWebEngine}
                  <p class="ia-picker-notice ia-picker-notice--warning">
                    {i18n.t('input.mainModelPicker.webLocalSessionBlocked')}
                  </p>
                {/if}
              </div>
            </div>
          {/if}
          </div>
          {#if currentSessionUsesWebEngine}
            <!-- GPT Web 对话方式：临时 / 已保存。首条消息前可切换，之后固定（只读显示）。
                 常驻在工具栏而不是藏在模型选择器里：用户新开 Web 会话时第一眼就能看到并选择。 -->
            {#if currentSessionHasCanonicalHistory}
              <span
                class="ia-web-mode ia-web-mode--locked"
                data-web-model-mode-locked={webConversationMode}
                title={i18n.t('webModel.mode.lockedTitle')}
              >{webConversationMode === 'saved' ? i18n.t('webModel.mode.chip.saved') : i18n.t('webModel.mode.chip.temporary')}</span>
            {:else}
              <div class="ia-web-mode" role="group" aria-label={i18n.t('webModel.mode.header')} data-web-model-mode-chip={webConversationMode}>
                <button
                  type="button"
                  class="ia-web-mode-btn"
                  class:selected={webConversationMode === 'temporary'}
                  data-web-model-mode-chip-option="temporary"
                  disabled={sessionInputLocked || isInteractionBlocking || pickerSavingModel !== null || pickerSavingReasoning !== null}
                  title={i18n.t('webModel.mode.temporaryHint')}
                  onclick={() => void applyWebConversationMode('temporary')}
                >{i18n.t('webModel.mode.chip.temporary')}</button>
                <button
                  type="button"
                  class="ia-web-mode-btn"
                  class:selected={webConversationMode === 'saved'}
                  data-web-model-mode-chip-option="saved"
                  disabled={sessionInputLocked || isInteractionBlocking || pickerSavingModel !== null || pickerSavingReasoning !== null}
                  title={i18n.t('webModel.mode.savedHint')}
                  onclick={() => void applyWebConversationMode('saved')}
                >{i18n.t('webModel.mode.chip.saved')}</button>
              </div>
            {/if}
          {/if}
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
              <Icon name={sendPreparing ? 'loader' : 'send'} size={14} class={sendPreparing ? 'spinning' : ''} />
            </button>
          {/if}
          <button
            class="ia-send stop"
            data-testid="input-stop-button"
            onclick={stopTask}
            disabled={stopLoading}
            title={shouldInterruptAgentRunFromComposer ? i18n.t('input.stopTaskTitle') : i18n.t('input.stop')}
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
    onClose={closeBrowserAnnotationEditing}
    closeOnBackdrop={true}
    size="sm"
    modalClass="browser-annotation-edit-modal"
  >
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
        <button type="button" onclick={closeBrowserAnnotationEditing} disabled={browserAnnotationSaving}>
          {i18n.t('browser.annotation.cancel')}
        </button>
        <button
          type="button"
          class="primary"
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
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
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

  .ia-editing-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-height: 30px;
    padding: 4px var(--space-3);
    border-bottom: 1px solid var(--border-subtle);
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
  .ia-container :global(.spinning) { animation: ia-spin 0.8s linear infinite; }
  @keyframes ia-spin { to { transform: rotate(360deg); } }
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
  .ia-add-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid transparent;
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
  .ia-web-mode {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    overflow: hidden;
    font-size: 11px;
    line-height: 1;
  }
  .ia-web-mode-btn {
    height: 100%;
    padding: 0 8px;
    border: 0;
    background: transparent;
    color: var(--foreground-muted, inherit);
    font: inherit;
    cursor: pointer;
    white-space: nowrap;
  }
  .ia-web-mode-btn:hover:not(:disabled):not(.selected) {
    background: color-mix(in srgb, var(--primary) 8%, transparent);
  }
  .ia-web-mode-btn.selected {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    color: var(--primary);
    font-weight: 650;
  }
  .ia-web-mode-btn:disabled {
    cursor: default;
    opacity: 0.55;
  }
  .ia-web-mode--locked {
    padding: 0 8px;
    color: var(--foreground-muted, inherit);
    cursor: default;
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
    font-size: 10px;
    line-height: 16px;
    font-weight: 650;
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
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    font-size: 11px;
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
  .ia-add-popover {
    left: 0;
    right: auto;
    width: min(380px, calc(100vw - 24px));
    max-height: min(520px, 62vh);
    padding: 6px;
  }
  .ia-add-group-label {
    padding: 7px 9px 4px;
    color: var(--foreground-muted);
    font-size: 11px;
    font-weight: 600;
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
    font-weight: 520;
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
    width: min(248px, calc(100vw - 20px));
    min-width: min(224px, calc(100vw - 20px));
    max-width: 248px;
    padding: 5px;
  }
  .ia-access-popover .ia-picker-list {
    gap: 2px;
    margin-top: 0;
    padding-top: 1px;
    border-top: 0;
  }
  .ia-access-popover .ia-picker-header {
    margin: 0;
    padding: 1px 6px 4px;
    border-bottom: 0;
    font-size: 10px;
    line-height: 16px;
  }
  .ia-picker-item.ia-access-option {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    align-items: center;
    gap: 7px;
    min-height: 42px;
    padding: 5px 7px;
    border: 1px solid transparent;
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
    font-size: 11px;
    line-height: 15px;
  }
  .ia-access-option .ia-picker-item-desc {
    font-size: 10px;
    line-height: 1.25;
    overflow-wrap: break-word;
  }
  .ia-access-recommended {
    flex: 0 0 auto;
    padding: 0 4px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: var(--primary);
    font-size: 8px;
    font-weight: var(--font-semibold);
    line-height: 13px;
  }
  .ia-picker-item.ia-access-option.selected {
    border-color: color-mix(in srgb, var(--primary) 28%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--primary) 5%, transparent);
  }
  .ia-picker-item.ia-access-option--full_access.selected {
    background: color-mix(in srgb, var(--warning) 10%, transparent);
    border-color: color-mix(in srgb, var(--warning) 28%, transparent);
    color: var(--foreground);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warning) 4%, transparent);
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
  /* Web 引擎的当前工具档位：与来源标注并列，只做提示，不参与判断。 */
  .ia-model-tier {
    flex: 0 0 auto;
    margin-left: 4px;
    font-size: 10px;
    letter-spacing: 0.04em;
    color: var(--foreground-muted);
  }
  /* Web 引擎的来源标注（A6 / S4）：与设置分区的 badge 同义，只是尺寸更小。 */
  .ia-model-web-badge {
    flex: 0 0 auto;
    margin-left: 6px;
    padding: 0 6px;
    border-radius: 999px;
    font-size: 10px;
    line-height: 16px;
    white-space: nowrap;
    background: var(--primary-soft, rgba(80, 140, 255, 0.16));
    color: var(--primary);
  }
  .ia-picker-notice {
    margin: 6px 6px 0;
    font-size: 11px;
    line-height: 1.5;
    color: var(--foreground-muted);
  }
  .ia-picker-notice--warning {
    color: var(--warning, var(--foreground-muted));
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
    border: 1px solid color-mix(in srgb, var(--primary) 38%, transparent);
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
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
  }
  .ia-reference-chip-label {
    font-weight: var(--font-medium, 500);
    white-space: nowrap;
  }
  .ia-reference-chip-desc {
    color: color-mix(in srgb, var(--primary) 72%, var(--foreground-muted));
    font-size: 11px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ia-reference-chip-goal .ia-reference-chip-desc {
    color: color-mix(in srgb, var(--success) 64%, var(--foreground-muted));
  }
  .ia-reference-chip-remove {
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
    resize: vertical;
    border: 1px solid var(--border);
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
  .browser-annotation-edit-actions button {
    min-height: 30px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    font: inherit;
    cursor: pointer;
  }
  .browser-annotation-edit-actions button.primary {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--accent-foreground, white);
  }
  .browser-annotation-edit-actions button:disabled {
    opacity: .5;
    cursor: default;
  }

  .ia-slash-popover {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 8px;
    z-index: 31;
    width: min(420px, calc(100% - 16px));
    max-height: 360px;
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
  .ia-slash-group-label {
    padding: 6px 9px 4px;
    color: var(--foreground-muted);
    font-size: 11px;
    font-weight: 600;
    line-height: 1;
  }
  .ia-slash-hint {
    padding: 6px 9px 2px;
    margin-top: 4px;
    color: var(--foreground-muted);
    font-size: 11px;
    border-top: 1px solid var(--border-subtle);
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
    font-size: 11px;
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
    font-weight: 600;
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
    font-weight: 600;
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
      max-height: min(46vh, 340px);
      border-radius: var(--radius-lg);
    }

    .ia-access-popover {
      position: fixed;
      right: 10px;
      bottom: calc(44px + env(safe-area-inset-bottom));
      left: auto;
      width: min(248px, calc(100vw - 20px));
      min-width: 0;
    }

    .ia-picker-popover {
      width: min(280px, calc(100vw - 24px));
      max-height: min(360px, 56vh);
    }

    .ia-session-model-popover {
      position: fixed;
      right: 10px;
      bottom: calc(44px + env(safe-area-inset-bottom));
      left: auto;
      width: min(280px, calc(100vw - 20px));
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

</style>
