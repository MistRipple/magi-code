<script lang="ts">
  import { getClientKind, vscode } from '../lib/vscode-bridge';
  import { onMount } from 'svelte';
  import type { StandardMessage } from '../../../../protocol/message-protocol';
  import { MessageCategory } from '../../../../protocol/message-protocol';
  import { ensureArray } from '../lib/utils';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import { getState } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';


  interface Props {
    onClose?: () => void;
  }

  let { onClose }: Props = $props();
  const clientKind = getClientKind();

  // 当前激活的 Tab
  let activeTab = $state<'stats' | 'model' | 'profile' | 'tools'>('stats');

  // 使用全局 store 的模型状态（与 BottomTabs 共用）
  const appState = getState();
  const modelStatuses = $derived(appState.modelStatus);

  let isRefreshing = $state(false);
  let totalInputTokens = $state(0);
  let totalOutputTokens = $state(0);
  let totalTokens = $derived(totalInputTokens + totalOutputTokens);
  let userInfo = $state('');
  let showResetConfirm = $state(false);

  // Profile Tab 状态（Worker 分工）
  let taskCategories = $state<Record<string, string>>({
    architecture: 'claude',
    backend: 'claude',
    frontend: 'gemini',
    data_analysis: 'codex',
    implement: 'codex',
    refactor: 'claude',
    bugfix: 'codex',
    debug: 'claude',
    test: 'codex',
    review: 'claude',
    document: 'gemini',
    integration: 'claude',
    simple: 'codex',
    general: 'claude'
  });
  let categoryGuidance = $state<Record<string, {
    displayName: string;
    description: string;
    guidance: { focus: string[]; constraints: string[] };
    priority: string;
    riskLevel: string;
  }>>({});
  let categoryPriority = $state<string[]>([]);
  let openGuidanceCategory = $state<string | null>(null);
  let guidancePopover = $state<{ category: string; top: number; left: number; width: number } | null>(null);

  // 全局用户规则
  let userRules = $state('');

  const categoryLabels: Record<string, () => string> = {
    architecture: () => i18n.t('settings.profile.category.architecture'),
    implement: () => i18n.t('settings.profile.category.implement'),
    refactor: () => i18n.t('settings.profile.category.refactor'),
    bugfix: () => i18n.t('settings.profile.category.bugfix'),
    debug: () => i18n.t('settings.profile.category.debug'),
    data_analysis: () => i18n.t('settings.profile.category.dataAnalysis'),
    frontend: () => i18n.t('settings.profile.category.frontend'),
    backend: () => i18n.t('settings.profile.category.backend'),
    test: () => i18n.t('settings.profile.category.test'),
    document: () => i18n.t('settings.profile.category.document'),
    review: () => i18n.t('settings.profile.category.review'),
    integration: () => i18n.t('settings.profile.category.integration'),
    simple: () => i18n.t('settings.profile.category.simple'),
    general: () => i18n.t('settings.profile.category.general')
  };

  // 分类显示顺序
  let categoryOrder = $derived(
    categoryPriority.length > 0 ? categoryPriority : Object.keys(categoryLabels)
  );

  function closeGuidancePopover() {
    openGuidanceCategory = null;
    guidancePopover = null;
  }

  onMount(() => {
    const handler = (e: Event) => {
      closeGuidancePopover();
      // 关闭模型下拉（如果点击区域不在 combobox 内也不在 dropdown 内）
      const target = e.target as HTMLElement;
      if (!target?.closest?.('.model-combobox') && !target?.closest?.('.model-dropdown')) {
        closeAllModelDropdowns();
      }
    };
    window.addEventListener('click', handler);
    window.addEventListener('resize', handler);
    return () => {
      window.removeEventListener('click', handler);
      window.removeEventListener('resize', handler);
    };
  });

  // Model Tab 状态
  let modelConfigTab = $state<'orch' | 'comp'>('orch');
  let workerModelTab = $state<'claude' | 'codex' | 'gemini'>('claude');
  type UrlMode = 'standard' | 'full';
  type ProviderName = 'openai' | 'anthropic';

  interface BaseModelFormConfig {
    baseUrl: string;
    urlMode: UrlMode;
    apiKey: string;
    model: string;
    provider: ProviderName;
    openaiProtocol: 'responses' | 'chat';
  }

  interface InteractiveModelFormConfig extends BaseModelFormConfig {
    thinking: boolean;
    reasoningEffort: 'low' | 'medium' | 'high' | 'xhigh';
  }

  interface WorkerModelFormConfig extends InteractiveModelFormConfig {
    enabled: boolean;
  }

  function createInteractiveConfig(
    provider: ProviderName,
    overrides: Partial<InteractiveModelFormConfig> = {},
  ): InteractiveModelFormConfig {
    return {
      baseUrl: '',
      urlMode: 'standard',
      apiKey: '',
      model: '',
      provider,
      openaiProtocol: 'responses',
      thinking: false,
      reasoningEffort: 'medium',
      ...overrides,
    };
  }

  function createAuxiliaryConfig(overrides: Partial<BaseModelFormConfig> = {}): BaseModelFormConfig {
    return {
      baseUrl: '',
      urlMode: 'standard',
      apiKey: '',
      model: '',
      provider: 'anthropic',
      openaiProtocol: 'responses',
      ...overrides,
    };
  }

  function createWorkerConfig(
    provider: ProviderName,
    overrides: Partial<WorkerModelFormConfig> = {},
  ): WorkerModelFormConfig {
    return {
      ...createInteractiveConfig(provider),
      enabled: true,
      ...overrides,
    };
  }

  function normalizeUrlMode(value: unknown): UrlMode {
    return value === 'full' ? 'full' : 'standard';
  }

  function getBaseUrlPlaceholder(provider: string): string {
    return provider === 'openai' ? 'https://api.openai.com' : 'https://api.anthropic.com';
  }

  function normalizeBaseUrlForHint(baseUrl: string): string {
    return typeof baseUrl === 'string'
      ? baseUrl.trim().replace(/\/+$/, '').toLowerCase()
      : '';
  }

  function shouldRecommendStandardUrlMode(provider: ProviderName, baseUrl: string): boolean {
    const normalized = normalizeBaseUrlForHint(baseUrl);
    if (!normalized) {
      return false;
    }

    if (provider === 'anthropic') {
      return normalized === 'https://api.anthropic.com'
        || normalized === 'https://api.lkeap.cloud.tencent.com/coding/anthropic';
    }

    return normalized === 'https://api.openai.com'
      || normalized === 'https://api.lkeap.cloud.tencent.com/coding/v3';
  }

  // 测试连接状态: 'idle' | 'testing' | 'success' | 'error'
  let testStatus = $state<Record<string, 'idle' | 'testing' | 'success' | 'error'>>({
    orch: 'idle',
    comp: 'idle',
    claude: 'idle',
    codex: 'idle',
    gemini: 'idle'
  });

  // 模型列表（从 API 获取）
  let modelLists = $state<Record<string, string[]>>({
    orch: [],
    comp: [],
    claude: [],
    codex: [],
    gemini: [],
  });
  // 模型列表获取状态
  let fetchingModels = $state<Record<string, boolean>>({
    orch: false,
    comp: false,
    claude: false,
    codex: false,
    gemini: false,
  });
  // 模型下拉是否展开
  let modelDropdownOpen = $state<Record<string, boolean>>({
    orch: false,
    comp: false,
    claude: false,
    codex: false,
    gemini: false,
  });

  // 模型下拉的 fixed 定位坐标（用于突破 overflow 容器限制）
  let dropdownPosition = $state({ top: 0, left: 0, width: 0 });

  function openModelDropdown(key: string, inputEl: EventTarget | null) {
    const el = inputEl as HTMLElement;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    dropdownPosition = { top: rect.bottom, left: rect.left, width: rect.width };
    modelDropdownOpen[key] = true;
    modelDropdownOpen = { ...modelDropdownOpen };
  }

  function closeAllModelDropdowns() {
    let changed = false;
    for (const key of Object.keys(modelDropdownOpen)) {
      if (modelDropdownOpen[key]) {
        modelDropdownOpen[key] = false;
        changed = true;
      }
    }
    if (changed) modelDropdownOpen = { ...modelDropdownOpen };
  }

  // 保存配置状态: 'idle' | 'saving' | 'saved' | 'error'
  let saveStatus = $state<Record<string, 'idle' | 'saving' | 'saved' | 'error'>>({
    orch: 'idle',
    comp: 'idle',
    claude: 'idle',
    codex: 'idle',
    gemini: 'idle',
    mcp: 'idle'
  });

  // 画像保存/重置状态
  let profileSaveStatus = $state<'idle' | 'saving' | 'saved' | 'error'>('idle');
  let profileResetStatus = $state<'idle' | 'saving' | 'saved' | 'error'>('idle');

  // Skill 安装/更新状态
  let installingSkills = $state<Set<string>>(new Set());
  let updatingSkills = $state<Set<string>>(new Set());
  let updatingAllSkills = $state(false);

  // 模型配置表单
  let orchConfig = $state<InteractiveModelFormConfig>(createInteractiveConfig('anthropic'));
  let compConfig = $state<BaseModelFormConfig>(createAuxiliaryConfig());
  let workerConfigs = $state<Record<string, WorkerModelFormConfig>>({
    claude: createWorkerConfig('anthropic'),
    codex: createWorkerConfig('openai'),
    gemini: createWorkerConfig('openai'),
  });

  // API Key 明文可见状态
  let keyVisible = $state<Record<string, boolean>>({ orch: false, comp: false, worker: false });

  // Tools Tab 状态 - MCP 服务器完整结构（与后端 MCPServerConfig 对齐）
  interface MCPServer {
    id: string;
    name: string;
    type: 'stdio' | 'sse' | 'streamable-http';
    command?: string;
    args?: string[];
    env?: Record<string, string>;
    url?: string;
    headers?: Record<string, string>;
    enabled: boolean;
    connected?: boolean;
    health?: 'connected' | 'degraded' | 'disconnected';
    error?: string;
    toolCount?: number;
    reconnectAttempts?: number;
    lastCheckedAt?: number;
    lastReconnectAt?: number;
    lastReconnectSuccessfulAt?: number;
  }
  let mcpServers = $state<MCPServer[]>([]);
  let mcpExpandedServer = $state<string | null>(null);
  let mcpServerTools = $state<Record<string, Array<{ name: string; description: string; inputSchema?: any }>>>({});
  let mcpExpandedTool = $state<string | null>(null); // 用于跟踪展开描述的工具
  let currentEditingMCPServer = $state<MCPServer | null>(null);
  let mcpRefreshingServers = $state<Set<string>>(new Set()); // 正在刷新工具的服务器 ID

  // Skills 完整结构（内置工具已迁移到 ToolManager，不再通过 Skills 配置）
  interface SkillItem {
    name: string;
    description: string;
    source: 'custom' | 'instruction';
  }
  let skills = $state<SkillItem[]>([]);

  // 仓库管理
  interface Repository {
    id: string;
    url: string;
    name?: string;
    skillCount?: number;
    lastUpdated?: string;
  }
  let repositories = $state<Repository[]>([]);

  // Skill 库
  interface LibrarySkill {
    name: string;
    fullName: string;
    description?: string;
    author?: string;
    version?: string;
    category?: string;
    skillType?: string;
    repositoryId?: string;
    repositoryName?: string;
    installed?: boolean;
    icon?: string;
  }
  let librarySkills = $state<LibrarySkill[]>([]);
  let skillSearchQuery = $state('');

  // 对话框状态
  let showInputDialog = $state(false);
  let inputDialogTitle = $state('');
  let inputDialogValue = $state('');
  let inputDialogCallback = $state<((value: string) => void) | null>(null);

  // MCP 对话框
  let showMCPDialogState = $state(false);
  let mcpDialogIsEdit = $state(false);
  let mcpDialogJson = $state('');
  let mcpDialogError = $state('');

  // 仓库管理对话框
  let showRepoDialogState = $state(false);
  let repoAddUrl = $state('');
  let repoAddLoading = $state(false);
  let repositoriesLoading = $state(false); // 仓库列表加载状态

  // Skill 库对话框
  let showSkillLibraryDialogState = $state(false);
  let skillLibraryLoading = $state(false); // Skill 库加载状态
  let localSkillInstalling = $state(false);
  let skillLibraryFailedRepositories = $state<Array<{ repositoryId: string; url?: string; error?: string }>>([]);

  // 通用确认对话框状态
  let showConfirmDialog = $state(false);
  let confirmDialogTitle = $state('');
  let confirmDialogMessage = $state('');
  let confirmDialogAction: (() => void) | null = $state(null);

  // 显示确认对话框
  function showConfirm(title: string, message: string, action: () => void) {
    confirmDialogTitle = title;
    confirmDialogMessage = message;
    confirmDialogAction = action;
    showConfirmDialog = true;
  }

  // 确认操作
  function handleConfirmYes() {
    if (confirmDialogAction) {
      confirmDialogAction();
    }
    showConfirmDialog = false;
    confirmDialogAction = null;
  }

  // 取消操作
  function handleConfirmNo() {
    showConfirmDialog = false;
    confirmDialogAction = null;
  }

  // 状态文本映射
  const statusTexts: Record<string, () => string> = {
    available: () => i18n.t('settings.status.connected'),
    connected: () => i18n.t('settings.status.connected'),
    configured: () => i18n.t('settings.status.connected'),
    disabled: () => i18n.t('settings.status.disabled'),
    not_configured: () => i18n.t('settings.status.notConfigured'),
    checking: () => i18n.t('settings.status.checking'),
    error: () => i18n.t('settings.status.error'),
    unavailable: () => i18n.t('settings.status.unavailable'),
    invalid_model: () => i18n.t('settings.status.invalidModel'),
    auth_failed: () => i18n.t('settings.status.authFailed'),
    network_error: () => i18n.t('settings.status.networkError'),
    timeout: () => i18n.t('settings.status.timeout'),
    orchestrator: () => i18n.t('settings.status.orchestrator')
  };

  function getStatusClass(status: string): string {
    if (status === 'available' || status === 'connected' || status === 'configured') return 'success';
    if (status === 'checking') return 'checking';
    if (status === 'orchestrator') return 'warning';
    if (status === 'disabled' || status === 'not_configured') return 'disabled';
    if (status === 'error' || status === 'unavailable' || status === 'invalid_model' || status === 'auth_failed' || status === 'network_error' || status === 'timeout') {
      return 'error';
    }
    return 'error';
  }

  // 获取 worker 的执行统计数据
  interface RuntimeTokenCounter {
    rawInputTokens: number;
    rawOutputTokens: number;
    carryInputTokens: number;
    carryOutputTokens: number;
    effectiveInputTokens: number;
    effectiveOutputTokens: number;
    updatedAt: number;
  }
  let runtimeTokenCounters = $state<Record<string, RuntimeTokenCounter>>({});

  function toSafeTokenCount(value: unknown): number {
    if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
      return 0;
    }
    return Math.floor(value);
  }

  function getSnapshotWorkerTokens(worker: string): { inputTokens: number; outputTokens: number } {
    const stats = executionStats.find(s => s.worker === worker);
    return {
      inputTokens: toSafeTokenCount(stats?.totalInputTokens),
      outputTokens: toSafeTokenCount(stats?.totalOutputTokens),
    };
  }

  function upsertRuntimeTokenCounter(worker: string, usage: unknown): void {
    const usageObj = usage && typeof usage === 'object'
      ? usage as Record<string, unknown>
      : {};
    const rawInputTokens = toSafeTokenCount(usageObj.inputTokens);
    const rawOutputTokens = toSafeTokenCount(usageObj.outputTokens);

    const snapshot = getSnapshotWorkerTokens(worker);
    const previous = runtimeTokenCounters[worker];
    let carryInputTokens = previous?.carryInputTokens ?? Math.max(0, snapshot.inputTokens - rawInputTokens);
    let carryOutputTokens = previous?.carryOutputTokens ?? Math.max(0, snapshot.outputTokens - rawOutputTokens);

    if (previous) {
      // 适配器重建/重连后 raw 计数可能回退，需要把旧 raw 折算进累计值。
      if (rawInputTokens < previous.rawInputTokens) {
        carryInputTokens += previous.rawInputTokens;
      }
      if (rawOutputTokens < previous.rawOutputTokens) {
        carryOutputTokens += previous.rawOutputTokens;
      }
    }

    const requiredInputFloor = Math.max(snapshot.inputTokens, previous?.effectiveInputTokens ?? 0);
    const requiredOutputFloor = Math.max(snapshot.outputTokens, previous?.effectiveOutputTokens ?? 0);
    const currentInputTotal = carryInputTokens + rawInputTokens;
    const currentOutputTotal = carryOutputTokens + rawOutputTokens;

    if (currentInputTotal < requiredInputFloor) {
      carryInputTokens += (requiredInputFloor - currentInputTotal);
    }
    if (currentOutputTotal < requiredOutputFloor) {
      carryOutputTokens += (requiredOutputFloor - currentOutputTotal);
    }

    const effectiveInputTokens = carryInputTokens + rawInputTokens;
    const effectiveOutputTokens = carryOutputTokens + rawOutputTokens;

    runtimeTokenCounters = {
      ...runtimeTokenCounters,
      [worker]: {
        rawInputTokens,
        rawOutputTokens,
        carryInputTokens,
        carryOutputTokens,
        effectiveInputTokens,
        effectiveOutputTokens,
        updatedAt: Date.now(),
      },
    };
  }

  function recalibrateRuntimeTokenCounters(): void {
    if (Object.keys(runtimeTokenCounters).length === 0) {
      return;
    }
    const nextCounters: Record<string, RuntimeTokenCounter> = {};
    for (const [worker, counter] of Object.entries(runtimeTokenCounters)) {
      const snapshot = getSnapshotWorkerTokens(worker);
      let carryInputTokens = counter.carryInputTokens;
      let carryOutputTokens = counter.carryOutputTokens;

      const requiredInputFloor = Math.max(snapshot.inputTokens, counter.effectiveInputTokens);
      const requiredOutputFloor = Math.max(snapshot.outputTokens, counter.effectiveOutputTokens);
      const currentInputTotal = carryInputTokens + counter.rawInputTokens;
      const currentOutputTotal = carryOutputTokens + counter.rawOutputTokens;

      if (currentInputTotal < requiredInputFloor) {
        carryInputTokens += (requiredInputFloor - currentInputTotal);
      }
      if (currentOutputTotal < requiredOutputFloor) {
        carryOutputTokens += (requiredOutputFloor - currentOutputTotal);
      }

      nextCounters[worker] = {
        ...counter,
        carryInputTokens,
        carryOutputTokens,
        effectiveInputTokens: carryInputTokens + counter.rawInputTokens,
        effectiveOutputTokens: carryOutputTokens + counter.rawOutputTokens,
      };
    }
    runtimeTokenCounters = nextCounters;
  }

  function getWorkerStats(worker: string) {
    const stats = executionStats.find(s => s.worker === worker);
    const runtime = runtimeTokenCounters[worker];
    if (!stats && !runtime) {
      return null;
    }
    const snapshotInput = toSafeTokenCount(stats?.totalInputTokens);
    const snapshotOutput = toSafeTokenCount(stats?.totalOutputTokens);
    return {
      worker,
      totalExecutions: stats?.totalExecutions ?? 0,
      successCount: stats?.successCount ?? 0,
      failureCount: stats?.failureCount ?? 0,
      successRate: stats?.successRate ?? 1,
      avgDuration: stats?.avgDuration ?? 0,
      totalInputTokens: runtime ? Math.max(snapshotInput, runtime.effectiveInputTokens) : snapshotInput,
      totalOutputTokens: runtime ? Math.max(snapshotOutput, runtime.effectiveOutputTokens) : snapshotOutput,
    };
  }

  function recomputeTokenStatsSummary() {
    let inputSum = 0;
    let outputSum = 0;
    const seenWorkers = new Set<string>();

    for (const stats of executionStats) {
      const runtime = runtimeTokenCounters[stats.worker];
      const snapshotInput = toSafeTokenCount(stats.totalInputTokens);
      const snapshotOutput = toSafeTokenCount(stats.totalOutputTokens);
      inputSum += runtime ? Math.max(snapshotInput, runtime.effectiveInputTokens) : snapshotInput;
      outputSum += runtime ? Math.max(snapshotOutput, runtime.effectiveOutputTokens) : snapshotOutput;
      seenWorkers.add(stats.worker);
    }

    for (const [worker, runtime] of Object.entries(runtimeTokenCounters)) {
      if (seenWorkers.has(worker)) {
        continue;
      }
      inputSum += runtime.effectiveInputTokens;
      outputSum += runtime.effectiveOutputTokens;
    }

    totalInputTokens = inputSum;
    totalOutputTokens = outputSum;
  }

  // 格式化 Token 数量
  function formatTokens(tokens: number | undefined): string {
    if (tokens === undefined || tokens === null) return '--';
    if (tokens >= 1000000) return `${(tokens / 1000000).toFixed(1)}M`;
    if (tokens >= 1000) return `${(tokens / 1000).toFixed(1)}K`;
    return String(tokens);
  }

  function requestSettingsBootstrap(force = false) {
    vscode.postMessage({ type: 'loadSettingsBootstrap', force });
  }

  function refreshConnections() {
    if (isRefreshing) return;
    isRefreshing = true;
    // 将所有模型状态设为 checking（更新全局 store）
    appState.modelStatus = {
      claude: { status: 'checking' },
      codex: { status: 'checking' },
      gemini: { status: 'checking' },
      orchestrator: { status: 'checking' },
      auxiliary: { status: 'checking' }
    };
    requestSettingsBootstrap(true);
    // 30秒超时保护，防止状态卡住
    setTimeout(() => { isRefreshing = false; }, 30000);
  }

  function showResetConfirmDialog() {
    showResetConfirm = true;
  }

  function confirmResetStats() {
    vscode.postMessage({ type: 'resetExecutionStats' });
    showResetConfirm = false;
    runtimeTokenCounters = {};
    totalInputTokens = 0;
    totalOutputTokens = 0;
  }

  function cancelResetStats() {
    showResetConfirm = false;
  }

  function logout() {
    vscode.postMessage({ type: 'logout' });
  }


  function closeSettings() {
    onClose?.();
  }

  function saveProfile() {
    profileSaveStatus = 'saving';

    vscode.postMessage({
      type: 'saveProfileConfig',
      data: {
        assignments: taskCategories,
        userRules
      }
    });
  }

  function updateGuidancePosition(category: string, event: MouseEvent) {
    const width = Math.min(380, window.innerWidth - 24);
    const left = Math.min(window.innerWidth - width - 12, Math.max(12, event.clientX + 12));
    const top = Math.min(window.innerHeight - 12, Math.max(12, event.clientY + 12));
    openGuidanceCategory = category;
    guidancePopover = { category, top, left, width };
  }

  function showGuidance(category: string, event: MouseEvent) {
    updateGuidancePosition(category, event);
  }

  function moveGuidance(category: string, event: MouseEvent) {
    if (openGuidanceCategory !== category) return;
    updateGuidancePosition(category, event);
  }

  function hideGuidance(category: string) {
    if (openGuidanceCategory !== category) return;
    closeGuidancePopover();
  }

  function resetProfile() {
    profileResetStatus = 'saving';

    vscode.postMessage({ type: 'resetProfileConfig' });
  }

  function testModelConnection(target: 'orch' | 'comp' | 'worker') {
    // 设置测试中状态
    const statusKey = target === 'worker' ? workerModelTab : target;
    testStatus[statusKey] = 'testing';
    testStatus = { ...testStatus };

    // 后端使用 testWorkerConnection / testOrchestratorConnection / testAuxiliaryConnection
    if (target === 'worker') {
      vscode.postMessage({ type: 'testWorkerConnection', worker: workerModelTab, config: workerConfigs[workerModelTab] });
    } else if (target === 'orch') {
      vscode.postMessage({ type: 'testOrchestratorConnection', config: orchConfig });
    } else if (target === 'comp') {
      vscode.postMessage({ type: 'testAuxiliaryConnection', config: compConfig });
    }
  }

  function fetchModelList(target: 'orch' | 'comp' | 'worker') {
    const key = target === 'worker' ? workerModelTab : target;
    fetchingModels[key] = true;
    fetchingModels = { ...fetchingModels };

    let config: any;
    if (target === 'worker') {
      config = workerConfigs[workerModelTab];
    } else if (target === 'orch') {
      config = orchConfig;
    } else {
      config = compConfig;
    }

    vscode.postMessage({ type: 'fetchModelList', config, target: key });
  }

  function selectModel(target: string, model: string) {
    if (target === 'orch') {
      orchConfig.model = model;
    } else if (target === 'comp') {
      compConfig.model = model;
    } else if (workerConfigs[target]) {
      workerConfigs[target].model = model;
    }
    modelDropdownOpen[target] = false;
    modelDropdownOpen = { ...modelDropdownOpen };
  }

  // 重置测试状态（3秒后自动重置为 idle）
  function resetTestStatus(key: string) {
    setTimeout(() => {
      testStatus[key] = 'idle';
      testStatus = { ...testStatus };
    }, 3000);
  }

  // 重置保存状态（2秒后自动重置为 idle）
  function resetSaveStatus(key: string) {
    setTimeout(() => {
      saveStatus[key] = 'idle';
      saveStatus = { ...saveStatus };
    }, 2000);
  }

  function resetProfileStatus(kind: 'save' | 'reset') {
    setTimeout(() => {
      if (kind === 'save') {
        profileSaveStatus = 'idle';
      } else {
        profileResetStatus = 'idle';
      }
    }, 2000);
  }


  function saveModelConfig(target: 'orch' | 'comp' | 'worker') {
    const key = target === 'worker' ? workerModelTab : target;

    // 设置保存中状态
    saveStatus[key] = 'saving';
    saveStatus = { ...saveStatus };

    if (target === 'worker') {
      const wc = workerConfigs[workerModelTab];
      vscode.postMessage({ type: 'saveWorkerConfig', worker: workerModelTab, config: {
        baseUrl: wc.baseUrl, urlMode: wc.urlMode, apiKey: wc.apiKey, model: wc.model, provider: wc.provider, openaiProtocol: wc.openaiProtocol,
        enabled: wc.enabled, enableThinking: wc.thinking, reasoningEffort: wc.reasoningEffort
      }});
    } else if (target === 'orch') {
      vscode.postMessage({ type: 'saveOrchestratorConfig', config: {
        baseUrl: orchConfig.baseUrl, urlMode: orchConfig.urlMode, apiKey: orchConfig.apiKey, model: orchConfig.model, provider: orchConfig.provider, openaiProtocol: orchConfig.openaiProtocol,
        enableThinking: orchConfig.thinking, reasoningEffort: orchConfig.reasoningEffort
      }});
    } else if (target === 'comp') {
      vscode.postMessage({ type: 'saveAuxiliaryConfig', config: compConfig });
    }

    // 保存后自动进行一次连接测试
    testModelConnection(target);

    // 模拟保存成功（实际应该通过消息回调）
    setTimeout(() => {
      saveStatus[key] = 'saved';
      saveStatus = { ...saveStatus };
      resetSaveStatus(key);
    }, 300);
  }

  function confirmInputDialog() {
    if (inputDialogCallback && inputDialogValue.trim()) {
      inputDialogCallback(inputDialogValue.trim());
    }
    showInputDialog = false;
    inputDialogValue = '';
    inputDialogCallback = null;
  }

  function cancelInputDialog() {
    showInputDialog = false;
    inputDialogValue = '';
    inputDialogCallback = null;
  }

  // ============================================
  // MCP 服务器操作函数
  // ============================================

  function openMCPDialog(server: MCPServer | null = null) {
    currentEditingMCPServer = server;
    mcpDialogIsEdit = server !== null;

    let defaultJSON: string;
    if (server) {
      // 编辑模式：从实际数据序列化，去掉内部状态字段
      const cfg: Record<string, any> = {};
      if (server.command) cfg.command = server.command;
      if (server.args && server.args.length > 0) cfg.args = server.args;
      if (server.env && Object.keys(server.env).length > 0) cfg.env = server.env;
      if (server.url) cfg.url = server.url;
      if (server.headers && Object.keys(server.headers).length > 0) cfg.headers = server.headers;
      defaultJSON = JSON.stringify({ mcpServers: { [server.name]: cfg } }, null, 2);
    } else {
      // 新增模式：默认 stdio 示例模板
      defaultJSON = `{
  "mcpServers": {
    "mcp-server": {
      "command": "npx",
      "args": [
        "@modelcontextprotocol/server-filesystem",
        "/path/to/allowed/files"
      ],
      "env": {}
    }
  }
}`;
    }
    mcpDialogJson = defaultJSON;
    showMCPDialogState = true;
  }

  function closeMCPDialog() {
    showMCPDialogState = false;
    currentEditingMCPServer = null;
    mcpDialogJson = '';
    mcpDialogError = '';
  }

  function saveMCPServer() {
    mcpDialogError = '';
    const jsonText = mcpDialogJson.trim();
    if (!jsonText) {
      mcpDialogError = i18n.t('settings.mcp.emptyJson');
      return;
    }

    let parsed: any;
    try {
      parsed = JSON.parse(jsonText);
    } catch (error: any) {
      mcpDialogError = i18n.t('settings.mcp.jsonError', { error: error.message });
      return;
    }

    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      mcpDialogError = i18n.t('settings.mcp.jsonMustBeObject');
      return;
    }

    const servers = parsed.mcpServers;
    if (!servers || typeof servers !== 'object' || Array.isArray(servers)) {
      mcpDialogError = i18n.t('settings.mcp.missingMcpServers');
      return;
    }

    const serverNames = Object.keys(servers);
    if (serverNames.length === 0) {
      mcpDialogError = i18n.t('settings.mcp.mcpServersEmpty');
      return;
    }

    if (serverNames.length > 1 && mcpDialogIsEdit) {
      mcpDialogError = i18n.t('settings.mcp.editOnlyOneServer');
      return;
    }

    // 设置保存中状态
    saveStatus.mcp = 'saving';
    saveStatus = { ...saveStatus };

    const saveServer = (name: string, cfg: any, isUpdate: boolean): boolean => {
      if (!cfg || typeof cfg !== 'object') {
        mcpDialogError = i18n.t('settings.mcp.invalidServerConfig', { name });
        return false;
      }

      const command = String(cfg.command || '').trim();
      const url = String(cfg.url || '').trim();

      // command 和 url 至少要有一个
      if (!command && !url) {
        mcpDialogError = i18n.t('settings.mcp.missingCommandOrUrl', { name });
        return false;
      }

      let serverData: any;

      if (url) {
        // HTTP (SSE / Streamable HTTP) 类型
        const headers = cfg.headers ?? {};
        if (typeof headers !== 'object' || Array.isArray(headers)) {
          mcpDialogError = i18n.t('settings.mcp.headersMustBeObject', { name });
          return false;
        }

        serverData = {
          id: name,
          name,
          url,
          headers: Object.keys(headers).length > 0 ? headers : undefined,
          enabled: cfg.enabled !== false,
          type: 'streamable-http'
        };
      } else {
        // stdio 类型
        const args = cfg.args ?? [];
        if (!Array.isArray(args)) {
          mcpDialogError = i18n.t('settings.mcp.argsMustBeArray', { name });
          return false;
        }

        const env = cfg.env ?? {};
        if (typeof env !== 'object' || Array.isArray(env)) {
          mcpDialogError = i18n.t('settings.mcp.envMustBeObject', { name });
          return false;
        }

        serverData = {
          id: name,
          name,
          command,
          args,
          env,
          enabled: cfg.enabled !== false,
          type: 'stdio'
        };
      }

      if (isUpdate && currentEditingMCPServer) {
        vscode.postMessage({
          type: 'updateMCPServer',
          serverId: currentEditingMCPServer.id,
          updates: { ...serverData, id: currentEditingMCPServer.id }
        });
      } else {
        vscode.postMessage({
          type: 'addMCPServer',
          server: serverData
        });
      }

      return true;
    };

    let savedCount = 0;
    if (mcpDialogIsEdit && currentEditingMCPServer) {
      const name = serverNames[0];
      if (saveServer(name, servers[name], true)) savedCount += 1;
    } else {
      serverNames.forEach((name) => {
        if (saveServer(name, servers[name], false)) savedCount += 1;
      });
    }

    if (savedCount > 0) {
      // 保存成功
      saveStatus.mcp = 'saved';
      saveStatus = { ...saveStatus };
      resetSaveStatus('mcp');
      closeMCPDialog();
    } else {
      // 保存失败
      saveStatus.mcp = 'idle';
      saveStatus = { ...saveStatus };
    }
  }

  function deleteMCPServer(serverId: string) {
    showConfirm(i18n.t('settings.tools.deleteMcpServer'), i18n.t('settings.tools.deleteMcpServerConfirm'), () => {
      vscode.postMessage({ type: 'deleteMCPServer', serverId });
    });
  }

  function toggleMCPServer(serverId: string, enabled: boolean) {
    const server = mcpServers.find(s => s.id === serverId);
    if (server) {
      vscode.postMessage({
        type: 'updateMCPServer',
        serverId,
        updates: { ...server, enabled: !enabled }
      });
    }
  }

  function toggleMCPExpand(serverId: string) {
    if (mcpExpandedServer === serverId) {
      mcpExpandedServer = null;
    } else {
      mcpExpandedServer = serverId;
      // 加载工具列表（如果尚未加载）
      if (!mcpServerTools[serverId]) {
        // 设置加载状态
        mcpRefreshingServers = new Set([...mcpRefreshingServers, serverId]);
        vscode.postMessage({ type: 'getMCPServerTools', serverId });
      }
    }
  }

  function refreshMCPTools(serverId: string) {
    // 设置刷新状态
    mcpRefreshingServers = new Set([...mcpRefreshingServers, serverId]);
    vscode.postMessage({ type: 'refreshMCPTools', serverId });
  }

  // 切换 MCP 工具描述展开状态
  function toggleMCPToolDesc(toolKey: string, e: Event) {
    e.stopPropagation();
    if (mcpExpandedTool === toolKey) {
      mcpExpandedTool = null;
    } else {
      mcpExpandedTool = toolKey;
    }
  }

  function getMCPHealthLabel(server: MCPServer): string {
    if (server.health === 'connected') return i18n.t('settings.tools.mcpHealthConnected');
    if (server.health === 'degraded') return i18n.t('settings.tools.mcpHealthDegraded');
    return i18n.t('settings.tools.mcpHealthDisconnected');
  }

  // ============================================
  // 仓库管理操作函数
  // ============================================

  function openRepoDialog() {
    showRepoDialogState = true;
    repoAddUrl = '';
    repositoriesLoading = false;
  }

  function closeRepoDialog() {
    showRepoDialogState = false;
    repoAddUrl = '';
    repoAddLoading = false;
    repositoriesLoading = false;
  }

  function addRepository() {
    const url = repoAddUrl.trim();
    if (!url) {
      return;
    }
    repoAddLoading = true;
    vscode.postMessage({ type: 'addRepository', url });
  }

  function deleteRepository(repositoryId: string) {
    showConfirm(i18n.t('settings.repo.deleteRepo'), i18n.t('settings.repo.deleteRepoConfirm'), () => {
      vscode.postMessage({ type: 'deleteRepository', repositoryId });
    });
  }

  function refreshRepository(repositoryId: string) {
    vscode.postMessage({ type: 'refreshRepository', repositoryId });
  }

  // ============================================
  // Skill 库操作函数
  // ============================================

  function openSkillLibraryDialog() {
    showSkillLibraryDialogState = true;
    skillSearchQuery = '';
    skillLibraryLoading = true; // 设置加载状态
    skillLibraryFailedRepositories = [];
    vscode.postMessage({ type: 'loadSkillLibrary' });
  }

  function closeSkillLibraryDialog() {
    showSkillLibraryDialogState = false;
    skillSearchQuery = '';
    skillLibraryLoading = false;
    localSkillInstalling = false;
    skillLibraryFailedRepositories = [];
  }

  function installSkill(skillFullName: string) {
    // 添加到安装中集合
    installingSkills.add(skillFullName);
    installingSkills = new Set(installingSkills);

    vscode.postMessage({ type: 'installSkill', skillId: skillFullName });
    // 状态清除由 skillInstalled 消息回调处理
  }

  function installLocalSkill() {
    if (localSkillInstalling) {
      return;
    }
    localSkillInstalling = true;
    vscode.postMessage({ type: 'installLocalSkill' });
  }

  // 删除 Skill
  function deleteSkill(skill: SkillItem) {
    if (skill.source === 'custom') {
      // 删除自定义工具
      showConfirm(i18n.t('settings.tools.deleteCustomTool'), i18n.t('settings.tools.deleteCustomToolConfirm', { name: skill.name }), () => {
        vscode.postMessage({ type: 'removeCustomTool', toolName: skill.name });
      });
    } else if (skill.source === 'instruction') {
      // Instruction skill 删除
      showConfirm(i18n.t('settings.tools.deleteInstructionSkill'), i18n.t('settings.tools.deleteInstructionSkillConfirm', { name: skill.name }), () => {
        vscode.postMessage({ type: 'removeInstructionSkill', skillName: skill.name });
      });
    }
  }

  // 更新单个 Skill
  function updateSkill(skillName: string) {
    updatingSkills.add(skillName);
    updatingSkills = new Set(updatingSkills);
    vscode.postMessage({ type: 'updateSkill', skillName });
  }

  // 更新所有 Skill
  function updateAllSkills() {
    if (skills.length === 0) return;
    updatingAllSkills = true;
    vscode.postMessage({ type: 'updateAllSkills' });
  }

  // Skill 搜索过滤
  let filteredLibrarySkills = $derived(
    librarySkills.filter(skill => {
      if (!skillSearchQuery) return true;
      const query = skillSearchQuery.toLowerCase();
      const name = (skill.name || '').toLowerCase();
      const fullName = (skill.fullName || '').toLowerCase();
      const desc = (skill.description || '').toLowerCase();
      return name.includes(query) || fullName.includes(query) || desc.includes(query);
    })
  );

  // 按仓库分组
  let skillsByRepo = $derived.by(() => {
    const groups: Record<string, { name: string; skills: LibrarySkill[] }> = {};
    for (const skill of filteredLibrarySkills) {
      const repoId = skill.repositoryId || 'unknown';
      if (!groups[repoId]) {
        groups[repoId] = {
          name: skill.repositoryName || i18n.t('settings.skillLibrary.unknownRepo'),
          skills: []
        };
      }
      groups[repoId].skills.push(skill);
    }
    return groups;
  });

  // 执行统计数据
  let executionStats = $state<Array<{
    worker: string;
    totalExecutions: number;
    successCount: number;
    failureCount: number;
    successRate: number;
    avgDuration: number;
    totalInputTokens: number;
    totalOutputTokens: number;
  }>>([]);

  function applyWorkerStatuses(statuses: Record<string, { status: string }> | undefined): void {
    if (!statuses) {
      return;
    }
    const allSettled = Object.values(statuses).every((status) => status.status !== 'checking');
    if (allSettled) {
      isRefreshing = false;
    }
  }

  function applyProfileConfig(config: any): void {
    if (!config) {
      return;
    }
    if (config.assignments) {
      taskCategories = { ...taskCategories, ...config.assignments };
    }
    if (config.categoryGuidance) {
      categoryGuidance = config.categoryGuidance;
    }
    if (Array.isArray(config.categoryPriority)) {
      categoryPriority = config.categoryPriority;
    }
    if (typeof config.userRules === 'string') {
      userRules = config.userRules;
    }
  }

  function applyWorkerConfigs(configs: Record<string, any> | undefined): void {
    if (!configs) {
      return;
    }
    for (const [worker, config] of Object.entries(configs)) {
      if (config && workerConfigs[worker]) {
        workerConfigs[worker] = createWorkerConfig(config.provider || 'anthropic', {
          baseUrl: config.baseUrl || '',
          urlMode: normalizeUrlMode(config.urlMode),
          apiKey: config.apiKey || '',
          model: config.model || '',
          provider: config.provider || 'anthropic',
          openaiProtocol: config.openaiProtocol || 'responses',
          enabled: config.enabled !== false,
          thinking: config.enableThinking === true,
          reasoningEffort: config.reasoningEffort || 'medium'
        });
      }
    }
  }

  function applyOrchestratorConfig(config: any): void {
    if (!config) {
      return;
    }
    orchConfig = createInteractiveConfig(config.provider || 'anthropic', {
      baseUrl: config.baseUrl || '',
      urlMode: normalizeUrlMode(config.urlMode),
      apiKey: config.apiKey || '',
      model: config.model || '',
      provider: config.provider || 'anthropic',
      openaiProtocol: config.openaiProtocol || 'responses',
      thinking: config.enableThinking === true,
      reasoningEffort: config.reasoningEffort || 'medium'
    });
  }

  function applyAuxiliaryConfig(config: any): void {
    if (!config) {
      return;
    }
    compConfig = createAuxiliaryConfig({
      baseUrl: config.baseUrl || '',
      urlMode: normalizeUrlMode(config.urlMode),
      apiKey: config.apiKey || '',
      model: config.model || '',
      provider: config.provider || 'anthropic',
      openaiProtocol: config.openaiProtocol || 'responses'
    });
  }

  function applyMcpServersPayload(serversPayload: unknown): void {
    const servers = ensureArray<any>(serversPayload);
    mcpServers = servers.map((s: any) => {
      const id = typeof s?.id === 'string' && s.id.trim() ? s.id.trim() : '';
      if (!id) {
        throw new Error('[SettingsPanel] MCP server 缺少 id');
      }
      const name = typeof s?.name === 'string' && s.name.trim() ? s.name.trim() : '';
      if (!name) {
        throw new Error(`[SettingsPanel] MCP server ${id} 缺少 name`);
      }
      return {
        id,
        name,
        type: s.type || 'stdio',
        command: s.command || '',
        args: s.args || [],
        env: s.env || {},
        url: s.url || '',
        headers: s.headers || {},
        enabled: s.enabled !== false,
        connected: s.connected === true,
        health: s.health === 'connected' || s.health === 'degraded' || s.health === 'disconnected'
          ? s.health
          : (s.connected === true ? 'connected' : 'disconnected'),
        error: typeof s.error === 'string' ? s.error : undefined,
        toolCount: Number.isFinite(s.toolCount) ? Number(s.toolCount) : 0,
        reconnectAttempts: Number.isFinite(s.reconnectAttempts) ? Number(s.reconnectAttempts) : 0,
        lastCheckedAt: Number.isFinite(s.lastCheckedAt) ? Number(s.lastCheckedAt) : undefined,
        lastReconnectAt: Number.isFinite(s.lastReconnectAt) ? Number(s.lastReconnectAt) : undefined,
        lastReconnectSuccessfulAt: Number.isFinite(s.lastReconnectSuccessfulAt) ? Number(s.lastReconnectSuccessfulAt) : undefined
      };
    });
  }

  function applySkillsConfig(config: any): void {
    const skillList: SkillItem[] = [];
    if (Array.isArray(config?.customTools)) {
      for (const tool of config.customTools) {
        skillList.push({ name: tool.name, description: tool.description || '', source: 'custom' });
      }
    }
    if (Array.isArray(config?.instructionSkills)) {
      for (const skill of config.instructionSkills) {
        skillList.push({ name: skill.name, description: skill.description || '', source: 'instruction' });
      }
    }
    skills = skillList;
  }

  function applyRepositoriesPayload(repositoriesPayload: unknown): void {
    const repoList = ensureArray<any>(repositoriesPayload);
    repositories = repoList.map((repository: any) => ({
      id: repository.id,
      url: repository.url,
      name: repository.name || repository.url,
      skillCount: repository.skillCount || 0,
      lastUpdated: repository.lastUpdated
    }));
    repoAddLoading = false;
    repositoriesLoading = false;
  }

  // 监听来自扩展的状态更新
  $effect(() => {
    const unsubscribe = vscode.onMessage((msg) => {
      if (msg.type !== 'unifiedMessage') return;
      const standard = msg.message as StandardMessage;
      if (!standard || standard.category !== MessageCategory.DATA || !standard.data) return;
      const { dataType, payload } = standard.data as { dataType: string; payload: any };

      if (dataType === 'settingsBootstrapLoaded') {
        applyWorkerStatuses(payload?.workerStatuses as Record<string, { status: string }> | undefined);
        applyProfileConfig(payload?.profileConfig);
        applyWorkerConfigs(payload?.workerConfigs);
        applyOrchestratorConfig(payload?.orchestratorConfig);
        applyAuxiliaryConfig(payload?.auxiliaryConfig);
        applyMcpServersPayload(payload?.mcpServers);
        applySkillsConfig(payload?.skillsConfig);
        applyRepositoriesPayload(payload?.repositories);
      }
      // 执行统计更新
      else if (dataType === 'executionStatsUpdate') {
        if (Array.isArray(payload?.stats)) {
          executionStats = payload.stats.map((item: any) => ({
            ...item,
            totalInputTokens: toSafeTokenCount(item?.totalInputTokens),
            totalOutputTokens: toSafeTokenCount(item?.totalOutputTokens),
          }));
          recalibrateRuntimeTokenCounters();
          recomputeTokenStatsSummary();
        } else if (payload?.orchestratorStats) {
          totalInputTokens = toSafeTokenCount(payload.orchestratorStats.totalInputTokens);
          totalOutputTokens = toSafeTokenCount(payload.orchestratorStats.totalOutputTokens);
        }
      }
      // 实时 token 运行态：按 worker 合并到统计面板，实现运行中动态更新
      else if (dataType === 'executionTokenRuntime') {
        const worker = typeof payload?.worker === 'string' ? payload.worker.trim() : '';
        if (!worker) {
          return;
        }
        upsertRuntimeTokenCounter(worker, payload?.usage);
        recomputeTokenStatsSummary();
      }
      // 画像保存结果
      else if (dataType === 'profileConfigSaved') {
        if (payload?.success) {
          profileSaveStatus = 'saved';
        } else {
          profileSaveStatus = 'error';
        }
        resetProfileStatus('save');
      }
      // 画像重置结果
      else if (dataType === 'profileConfigReset') {
        if (payload?.success) {
          profileResetStatus = 'saved';
        } else {
          profileResetStatus = 'error';
        }
        resetProfileStatus('reset');
      }
      // Worker 连接测试结果 - modelStatus 已由全局 message-handler 更新，此处仅维护按钮状态
      else if (dataType === 'workerConnectionTestResult') {
        const worker = payload?.worker;
        if (worker) {
          testStatus[worker] = payload?.success ? 'success' : 'error';
          testStatus = { ...testStatus };
          resetTestStatus(worker);
        }
      }
      // 编排者连接测试结果 - modelStatus 已由全局 message-handler 更新，此处仅维护按钮状态
      else if (dataType === 'orchestratorConnectionTestResult') {
        testStatus.orch = payload?.success ? 'success' : 'error';
        testStatus = { ...testStatus };
        resetTestStatus('orch');
      }
      // 辅助模型连接测试结果 - modelStatus 已由全局 message-handler 更新，此处仅维护按钮状态
      else if (dataType === 'auxiliaryConnectionTestResult') {
        testStatus.comp = payload?.success ? 'success' : 'error';
        testStatus = { ...testStatus };
        resetTestStatus('comp');
      }
      // 模型列表获取结果
      else if (dataType === 'modelListFetched') {
        const target = payload?.target as string;
        if (target) {
          fetchingModels[target] = false;
          fetchingModels = { ...fetchingModels };
          if (payload?.success && Array.isArray(payload.models)) {
            modelLists[target] = payload.models as string[];
            modelLists = { ...modelLists };
            // 自动展开下拉
            if ((payload.models as string[]).length > 0) {
              modelDropdownOpen[target] = true;
              modelDropdownOpen = { ...modelDropdownOpen };
            }
          }
        }
      }
      // MCP 工具列表加载（首次获取）
      else if (dataType === 'mcpServerTools') {
        if (payload?.serverId) {
          const tools = ensureArray(payload.tools);
          mcpServerTools = { ...mcpServerTools, [payload.serverId]: tools };
          // 清除刷新状态
          const newSet = new Set(mcpRefreshingServers);
          newSet.delete(payload.serverId);
          mcpRefreshingServers = newSet;
        }
      }
      // MCP 工具列表刷新
      else if (dataType === 'mcpToolsRefreshed') {
        if (payload?.serverId) {
          const tools = ensureArray(payload.tools);
          mcpServerTools = { ...mcpServerTools, [payload.serverId]: tools };
          // 清除刷新状态
          const newSet = new Set(mcpRefreshingServers);
          newSet.delete(payload.serverId);
          mcpRefreshingServers = newSet;
        }
      }
      // 仓库添加成功
      else if (dataType === 'repositoryAdded') {
        repoAddLoading = false;
        repoAddUrl = '';
      }
      // 仓库添加失败
      else if (dataType === 'repositoryAddFailed') {
        repoAddLoading = false;
      }
      // Skill 库加载
      else if (dataType === 'skillLibraryLoaded') {
        const skillsList = ensureArray<any>(payload?.skills);
        const failedRepos = ensureArray<any>(payload?.failedRepositories);
        librarySkills = skillsList.map((s: any) => ({
          name: s.name,
          fullName: s.fullName || s.name,
          description: s.description || '',
          author: s.author,
          version: s.version,
          category: s.category,
          skillType: s.skillType,
          repositoryId: s.repositoryId,
          repositoryName: s.repositoryName,
          installed: s.installed || false,
          icon: s.icon
        }));
        skillLibraryFailedRepositories = failedRepos.map((repo: any) => ({
          repositoryId: String(repo.repositoryId || ''),
          url: repo.url ? String(repo.url) : undefined,
          error: repo.error ? String(repo.error) : undefined,
        })).filter((repo: any) => repo.repositoryId);
        skillLibraryLoading = false; // 清除加载状态
      }
      // Skill 安装成功
      else if (dataType === 'skillInstalled') {
        // 清除安装状态
        if (payload?.skillId) {
          installingSkills.delete(payload.skillId);
          installingSkills = new Set(installingSkills);
        }
        localSkillInstalling = false;
        vscode.postMessage({ type: 'loadSkillLibrary' });
        showSkillLibraryDialogState = false;
      }
      // Skill 安装失败
      else if (dataType === 'skillInstallFailed') {
        if (payload?.skillId) {
          installingSkills.delete(payload.skillId);
          installingSkills = new Set(installingSkills);
        }
        localSkillInstalling = false;
      }
      // Skill 更新成功
      else if (dataType === 'skillUpdated') {
        if (payload?.skillName) {
          updatingSkills.delete(payload.skillName);
          updatingSkills = new Set(updatingSkills);
        }
      }
      // 所有 Skill 更新完成
      else if (dataType === 'allSkillsUpdated') {
        updatingAllSkills = false;
      }
    });

    // 初始化请求数据
    requestSettingsBootstrap(true);
    vscode.postMessage({ type: 'requestExecutionStats' });

    return () => unsubscribe();
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="settings-overlay">
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="settings-panel" onclick={() => closeGuidancePopover()}>
    <div class="settings-header">
      <span class="settings-title">{i18n.t('settings.title')}</span>
      {#if userInfo && clientKind === 'vscode'}
        <div class="logout-section">
          <span class="user-info-text">{userInfo}</span>
          <button class="settings-btn secondary logout-btn" onclick={logout}>{i18n.t('settings.logout')}</button>
        </div>
      {/if}
      <!-- Language selector - compact segment button -->
      <div class="locale-selector">
        <button
          class="locale-btn"
          class:active={i18n.locale === 'zh-CN'}
          onclick={() => { i18n.setLocale('zh-CN'); vscode.postMessage({ type: 'updateSetting', key: 'locale', value: 'zh-CN' }); }}
        >
          {i18n.t('settings.locale.zhCN')}
        </button>
        <button
          class="locale-btn"
          class:active={i18n.locale === 'en-US'}
          onclick={() => { i18n.setLocale('en-US'); vscode.postMessage({ type: 'updateSetting', key: 'locale', value: 'en-US' }); }}
        >
          {i18n.t('settings.locale.enUS')}
        </button>
      </div>
      <button class="btn-icon btn-icon--sm" onclick={closeSettings} title={i18n.t('settings.closeSettings')}>
        <Icon name="close" size={14} />
      </button>
    </div>

    <!-- Tab 切换栏 -->
    <div class="settings-tabs">
      <button class="settings-tab" class:active={activeTab === 'stats'} onclick={() => activeTab = 'stats'}>
        <Icon name="stats" size={14} />
        {i18n.t('settings.tabStats')}
      </button>
      <button class="settings-tab" class:active={activeTab === 'model'} onclick={() => activeTab = 'model'}>
        <Icon name="model" size={14} />
        {i18n.t('settings.tabModel')}
      </button>
      <button class="settings-tab" class:active={activeTab === 'profile'} onclick={() => activeTab = 'profile'}>
        <Icon name="profile" size={14} />
        {i18n.t('settings.tabProfile')}
      </button>
      <button class="settings-tab" class:active={activeTab === 'tools'} onclick={() => activeTab = 'tools'}>
        <Icon name="tools" size={14} />
        {i18n.t('settings.tabTools')}
      </button>
    </div>

    <!-- Tab 内容区域 -->
  <div class="settings-tab-content" onscroll={() => { closeGuidancePopover(); closeAllModelDropdowns(); }}>
      {#if activeTab === 'stats'}
        <!-- 统计 Tab -->
        <div class="settings-section stats-section">
          <div class="settings-section-header">
            <div class="settings-section-title">{i18n.t('settings.stats.title')}</div>
            <div class="settings-section-actions">
              <div class="settings-summary-chip">{i18n.t('settings.stats.inputTokens', { count: formatTokens(totalInputTokens) })}</div>
              <div class="settings-summary-chip">{i18n.t('settings.stats.outputTokens', { count: formatTokens(totalOutputTokens) })}</div>
              <div class="settings-summary-chip">{i18n.t('settings.stats.totalTokens', { count: formatTokens(totalTokens) })}</div>
              <div class="stats-action-buttons">
                <button class="model-refresh-btn" class:loading={isRefreshing} onclick={refreshConnections} disabled={isRefreshing}>
                  <Icon name="refresh" size={14} />
                  {isRefreshing ? i18n.t('settings.stats.checking') : i18n.t('settings.stats.check')}
                </button>
                <button class="settings-btn secondary" onclick={showResetConfirmDialog}>{i18n.t('settings.stats.resetTokens')}</button>
              </div>
            </div>
          </div>

          <div class="model-connection-list">
            {#each ['orchestrator', 'auxiliary', 'claude', 'codex', 'gemini'] as worker}
              {@const status = modelStatuses[worker]}
              {@const workerStats = getWorkerStats(worker)}
              {@const statusClass = getStatusClass(status?.status || 'checking')}
              {@const modelLabel = status?.model
                || (status?.status === 'not_configured'
                  ? i18n.t('settings.stats.notConfigured')
                  : status?.status === 'disabled'
                    ? i18n.t('settings.stats.disabled')
                    : i18n.t('settings.stats.unknownModel'))}
              <div class="model-connection-item {statusClass}" data-worker={worker}>
                <div class="model-connection-icon {worker}">
                  <Icon name="circle" size={14} />
                </div>
                <div class="model-connection-info">
                  <div class="model-connection-header">
                    <span class="model-connection-name">
                      {worker === 'orchestrator' ? i18n.t('settings.stats.orchestratorModel') : worker === 'auxiliary' ? i18n.t('settings.stats.auxiliaryModel') : worker.charAt(0).toUpperCase() + worker.slice(1)}
                      {#if worker === 'orchestrator' || worker === 'auxiliary'}
                        <span class="required-badge">{i18n.t('settings.stats.required')}</span>
                      {/if}
                    </span>
                    <span class="model-connection-badge {statusClass}">{(statusTexts[status?.status] || statusTexts['checking'])()}</span>
                  </div>
                  <div class="model-connection-model">{modelLabel}</div>
                  {#if status?.error}
                    <div class="model-connection-error">{status.error}</div>
                  {/if}
                  <div class="model-connection-stats-inline">
                    <span>{i18n.t('settings.stats.tasks', { count: workerStats?.totalExecutions ?? '--' })}</span>
                    <span class="stats-divider">|</span>
                    <span>{i18n.t('settings.stats.successRate', { rate: workerStats?.successRate != null ? `${Math.round(workerStats.successRate * 100)}%` : '--' })}</span>
                    <span class="stats-divider">|</span>
                    <span>{i18n.t('settings.stats.input', { count: formatTokens(workerStats?.totalInputTokens ?? 0) })}</span>
                    <span class="stats-divider">|</span>
                    <span>{i18n.t('settings.stats.output', { count: formatTokens(workerStats?.totalOutputTokens ?? 0) })}</span>
                    <span class="stats-divider">|</span>
                    <span>{i18n.t('settings.stats.total', { count: formatTokens((workerStats?.totalInputTokens ?? 0) + (workerStats?.totalOutputTokens ?? 0)) })}</span>
                  </div>
                </div>
              </div>
            {/each}
          </div>
        </div>
      {:else if activeTab === 'model'}
        <!-- 模型配置 Tab -->
        <div class="model-config-grid">
          <div class="model-config-stack">
            <div class="model-config-tabs">
              <button class="model-config-tab" class:active={modelConfigTab === 'orch'} onclick={() => modelConfigTab = 'orch'}>{i18n.t('settings.model.orchestratorModel')}</button>
              <button class="model-config-tab" class:active={modelConfigTab === 'comp'} onclick={() => modelConfigTab = 'comp'}>{i18n.t('settings.model.auxiliaryModel')}</button>
            </div>

            {#if modelConfigTab === 'orch'}
              <div class="model-config-card">
                <div class="model-config-header">
                  <div class="model-config-title">{i18n.t('settings.model.orchestratorModel')}</div>
                  <div class="model-config-desc">{i18n.t('settings.model.orchestratorDesc')}</div>
                </div>
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <div class="llm-config-form">
                  <div class="llm-config-field-row url-mode-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
                      <input
                        type="text"
                        class="llm-config-input"
                        bind:value={orchConfig.baseUrl}
                        placeholder={getBaseUrlPlaceholder(orchConfig.provider)}
                      >
                    </div>
                    <div class="llm-config-field llm-config-field--compact">
                      <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
                      <div class="segmented-control">
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={orchConfig.urlMode === 'standard'}
                          onclick={() => orchConfig.urlMode = 'standard'}
                        >
                          {i18n.t('settings.model.urlMode.standard')}
                        </button>
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={orchConfig.urlMode === 'full'}
                          onclick={() => orchConfig.urlMode = 'full'}
                        >
                          {i18n.t('settings.model.urlMode.full')}
                        </button>
                      </div>
                      {#if shouldRecommendStandardUrlMode(orchConfig.provider, orchConfig.baseUrl)}
                        <div class="llm-config-hint">
                          {i18n.t('settings.model.urlMode.standardRecommended')}
                        </div>
                      {/if}
                    </div>
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
                    <div class="api-key-wrapper">
                      <input type={keyVisible.orch ? 'text' : 'password'} class="llm-config-input api-key-input" bind:value={orchConfig.apiKey} placeholder="sk-ant-...">
                      <button type="button" class="api-key-toggle" onclick={() => keyVisible.orch = !keyVisible.orch} title={keyVisible.orch ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
                        <Icon name={keyVisible.orch ? 'eye-slash' : 'eye'} size={14} />
                      </button>
                    </div>
                  </div>
                  <div class="llm-config-field-row has-thinking" class:has-level={orchConfig.provider === 'openai'}>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
                      <div class="model-combobox">
                        <input type="text" class="llm-config-input" bind:value={orchConfig.model} placeholder="claude-3-5-sonnet-20241022"
                          onfocus={(e) => { if (modelLists.orch.length > 0) openModelDropdown('orch', e.currentTarget); }}
                        >
                        {#if !orchConfig.model && modelLists.orch.length === 0}
                          <button class="model-fetch-btn" onclick={() => fetchModelList('orch')} disabled={fetchingModels.orch || !orchConfig.baseUrl || !orchConfig.apiKey}>
                            {#if fetchingModels.orch}
                              <Icon name="refresh" size={12} />
                            {:else}
                              <Icon name="download" size={12} />
                            {/if}
                          </button>
                        {/if}
                        {#if modelDropdownOpen.orch && modelLists.orch.length > 0}
                          <div class="model-dropdown" style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;">
                            {#each modelLists.orch as m}
                              <button class="model-dropdown-item" class:selected={orchConfig.model === m} onclick={() => selectModel('orch', m)}>{m}</button>
                            {/each}
                          </div>
                        {/if}
                      </div>
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.provider')}</label>
                      <select class="llm-config-select" bind:value={orchConfig.provider}>
                        <option value="openai">{i18n.t('settings.model.provider.openai')}</option>
                        <option value="anthropic">{i18n.t('settings.model.provider.anthropic')}</option>
                      </select>
                    </div>
                    {#if orchConfig.provider === 'openai'}
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.protocol')}</label>
                      <select class="llm-config-select" bind:value={orchConfig.openaiProtocol}>
                        <option value="responses">{i18n.t('settings.model.protocol.responses')}</option>
                        <option value="chat">{i18n.t('settings.model.protocol.chat')}</option>
                      </select>
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.level')}</label>
                      <select class="llm-config-select" bind:value={orchConfig.reasoningEffort}>
                        <option value="low">{i18n.t('settings.model.reasoning.low')}</option>
                        <option value="medium">{i18n.t('settings.model.reasoning.medium')}</option>
                        <option value="high">{i18n.t('settings.model.reasoning.high')}</option>
                        <option value="xhigh">{i18n.t('settings.model.reasoning.xhigh')}</option>
                      </select>
                    </div>
                    {/if}
                    <div class="llm-config-field inline-toggle">
                      <label class="llm-config-label">{i18n.t('settings.model.field.thinking')}</label>
                      <button type="button" class="llm-config-toggle-btn" title={orchConfig.thinking ? i18n.t('settings.model.disableThinking') : i18n.t('settings.model.enableThinking')} onclick={() => orchConfig.thinking = !orchConfig.thinking}>
                        <span class="toggle-switch" class:active={orchConfig.thinking}></span>
                      </button>
                    </div>
                  </div>
                  <div class="llm-config-actions">
                    <button
                      class="llm-config-save-btn"
                      class:saving={saveStatus.orch === 'saving'}
                      class:saved={saveStatus.orch === 'saved'}
                      onclick={() => saveModelConfig('orch')}
                      disabled={saveStatus.orch === 'saving'}
                    >
                      {#if saveStatus.orch === 'saving'}
                        <Icon name="refresh" size={14} />
                        {i18n.t('settings.model.saving')}
                      {:else if saveStatus.orch === 'saved'}
                        <Icon name="check" size={14} />
                        {i18n.t('settings.model.saved')}
                      {:else}
                        {i18n.t('settings.model.saveConfig')}
                      {/if}
                    </button>
                    <button
                      class="llm-config-test-btn"
                      class:testing={testStatus.orch === 'testing'}
                      class:success={testStatus.orch === 'success'}
                      class:error={testStatus.orch === 'error'}
                      onclick={() => testModelConnection('orch')}
                      disabled={testStatus.orch === 'testing'}
                    >
                      {#if testStatus.orch === 'testing'}
                        <Icon name="refresh" size={14} />
                        {i18n.t('settings.model.testing')}
                      {:else if testStatus.orch === 'success'}
                        <Icon name="check" size={14} />
                        {i18n.t('settings.model.testSuccess')}
                      {:else if testStatus.orch === 'error'}
                        <Icon name="close" size={14} />
                        {i18n.t('settings.model.testFailed')}
                      {:else}
                        <Icon name="check" size={14} />
                        {i18n.t('settings.model.testConnection')}
                      {/if}
                    </button>
                  </div>
                </div>
              </div>
            {:else if modelConfigTab === 'comp'}
              <div class="model-config-card">
                <div class="model-config-header">
                  <div class="model-config-title">{i18n.t('settings.model.auxiliaryModel')}</div>
                  <div class="model-config-desc">{i18n.t('settings.model.auxiliaryDesc')}</div>
                </div>
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <div class="llm-config-form">
                  <div class="llm-config-field-row url-mode-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
                      <input
                        type="text"
                        class="llm-config-input"
                        bind:value={compConfig.baseUrl}
                        placeholder={getBaseUrlPlaceholder(compConfig.provider)}
                      >
                    </div>
                    <div class="llm-config-field llm-config-field--compact">
                      <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
                      <div class="segmented-control">
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={compConfig.urlMode === 'standard'}
                          onclick={() => compConfig.urlMode = 'standard'}
                        >
                          {i18n.t('settings.model.urlMode.standard')}
                        </button>
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={compConfig.urlMode === 'full'}
                          onclick={() => compConfig.urlMode = 'full'}
                        >
                          {i18n.t('settings.model.urlMode.full')}
                        </button>
                      </div>
                      {#if shouldRecommendStandardUrlMode(compConfig.provider, compConfig.baseUrl)}
                        <div class="llm-config-hint">
                          {i18n.t('settings.model.urlMode.standardRecommended')}
                        </div>
                      {/if}
                    </div>
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
                    <div class="api-key-wrapper">
                      <input type={keyVisible.comp ? 'text' : 'password'} class="llm-config-input api-key-input" bind:value={compConfig.apiKey} placeholder="sk-ant-...">
                      <button type="button" class="api-key-toggle" onclick={() => keyVisible.comp = !keyVisible.comp} title={keyVisible.comp ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
                        <Icon name={keyVisible.comp ? 'eye-slash' : 'eye'} size={14} />
                      </button>
                    </div>
                  </div>
                  <div class="llm-config-field-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
                      <div class="model-combobox">
                        <input type="text" class="llm-config-input" bind:value={compConfig.model} placeholder="claude-3-haiku-20240307"
                          onfocus={(e) => { if (modelLists.comp.length > 0) openModelDropdown('comp', e.currentTarget); }}
                        >
                        {#if !compConfig.model && modelLists.comp.length === 0}
                          <button class="model-fetch-btn" onclick={() => fetchModelList('comp')} disabled={fetchingModels.comp || !compConfig.baseUrl || !compConfig.apiKey}>
                            {#if fetchingModels.comp}
                              <Icon name="refresh" size={12} />
                            {:else}
                              <Icon name="download" size={12} />
                            {/if}
                          </button>
                        {/if}
                        {#if modelDropdownOpen.comp && modelLists.comp.length > 0}
                          <div class="model-dropdown" style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;">
                            {#each modelLists.comp as m}
                              <button class="model-dropdown-item" class:selected={compConfig.model === m} onclick={() => selectModel('comp', m)}>{m}</button>
                            {/each}
                          </div>
                        {/if}
                      </div>
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.provider')}</label>
                      <select class="llm-config-select" bind:value={compConfig.provider}>
                        <option value="openai">{i18n.t('settings.model.provider.openai')}</option>
                        <option value="anthropic">{i18n.t('settings.model.provider.anthropic')}</option>
                      </select>
                    </div>
                    {#if compConfig.provider === 'openai'}
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.protocol')}</label>
                      <select class="llm-config-select" bind:value={compConfig.openaiProtocol}>
                        <option value="responses">{i18n.t('settings.model.protocol.responses')}</option>
                        <option value="chat">{i18n.t('settings.model.protocol.chat')}</option>
                      </select>
                    </div>
                    {/if}
                  </div>
                  <div class="llm-config-actions">
                    <button
                      class="llm-config-save-btn"
                      class:saving={saveStatus.comp === 'saving'}
                      class:saved={saveStatus.comp === 'saved'}
                      onclick={() => saveModelConfig('comp')}
                      disabled={saveStatus.comp === 'saving'}
                    >
                      {#if saveStatus.comp === 'saving'}
                        <Icon name="refresh" size={14} />
                        {i18n.t('settings.model.saving')}
                      {:else if saveStatus.comp === 'saved'}
                        <Icon name="check" size={14} />
                        {i18n.t('settings.model.saved')}
                      {:else}
                        {i18n.t('settings.model.saveConfig')}
                      {/if}
                    </button>
                    <button
                      class="llm-config-test-btn"
                      class:testing={testStatus.comp === 'testing'}
                      class:success={testStatus.comp === 'success'}
                      class:error={testStatus.comp === 'error'}
                      onclick={() => testModelConnection('comp')}
                      disabled={testStatus.comp === 'testing'}
                    >
                      {#if testStatus.comp === 'testing'}
                        <Icon name="refresh" size={14} />
                        {i18n.t('settings.model.testing')}
                      {:else if testStatus.comp === 'success'}
                        <Icon name="check" size={14} />
                        {i18n.t('settings.model.testSuccess')}
                      {:else if testStatus.comp === 'error'}
                        <Icon name="close" size={14} />
                        {i18n.t('settings.model.testFailed')}
                      {:else}
                        <Icon name="check" size={14} />
                        {i18n.t('settings.model.testConnection')}
                      {/if}
                    </button>
                  </div>
                </div>
              </div>
            {/if}
          </div>
          <div class="model-config-card">
            <div class="model-config-header-row">
              <div class="model-config-title">{i18n.t('settings.model.workerModel')}</div>
              <div class="worker-model-tabs">
                <button class="worker-model-tab" class:active={workerModelTab === 'claude'} onclick={() => workerModelTab = 'claude'}>
                  <span class="worker-dot claude"></span>
                  {i18n.t('settings.model.worker.claude')}
                </button>
                <button class="worker-model-tab" class:active={workerModelTab === 'codex'} onclick={() => workerModelTab = 'codex'}>
                  <span class="worker-dot codex"></span>
                  {i18n.t('settings.model.worker.codex')}
                </button>
                <button class="worker-model-tab" class:active={workerModelTab === 'gemini'} onclick={() => workerModelTab = 'gemini'}>
                  <span class="worker-dot gemini"></span>
                  {i18n.t('settings.model.worker.gemini')}
                </button>
              </div>
            </div>
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <div class="llm-config-form">
              <div class="llm-config-field-row worker-url-mode-row">
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
                  <input
                    type="text"
                    class="llm-config-input"
                    bind:value={workerConfigs[workerModelTab].baseUrl}
                    placeholder={getBaseUrlPlaceholder(workerConfigs[workerModelTab].provider)}
                  >
                </div>
                <div class="llm-config-field llm-config-field--compact">
                  <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
                  <div class="segmented-control">
                    <button
                      type="button"
                      class="segmented-control__option"
                      class:active={workerConfigs[workerModelTab].urlMode === 'standard'}
                      onclick={() => workerConfigs[workerModelTab].urlMode = 'standard'}
                    >
                      {i18n.t('settings.model.urlMode.standard')}
                    </button>
                    <button
                      type="button"
                      class="segmented-control__option"
                      class:active={workerConfigs[workerModelTab].urlMode === 'full'}
                      onclick={() => workerConfigs[workerModelTab].urlMode = 'full'}
                    >
                      {i18n.t('settings.model.urlMode.full')}
                    </button>
                  </div>
                  {#if shouldRecommendStandardUrlMode(workerConfigs[workerModelTab].provider, workerConfigs[workerModelTab].baseUrl)}
                    <div class="llm-config-hint">
                      {i18n.t('settings.model.urlMode.standardRecommended')}
                    </div>
                  {/if}
                </div>
                <div class="llm-config-field inline-toggle">
                  <label class="llm-config-label">{i18n.t('settings.model.status')}</label>
                  <button type="button" class="llm-config-toggle-btn" onclick={() => workerConfigs[workerModelTab].enabled = !workerConfigs[workerModelTab].enabled}>
                    <span class="toggle-switch" class:active={workerConfigs[workerModelTab].enabled}></span>
                    <span>{i18n.t('settings.model.enable')}</span>
                  </button>
                </div>
              </div>
              <div class="llm-config-field">
                <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
                <div class="api-key-wrapper">
                  <input type={keyVisible.worker ? 'text' : 'password'} class="llm-config-input api-key-input" bind:value={workerConfigs[workerModelTab].apiKey} placeholder="sk-ant-...">
                  <button type="button" class="api-key-toggle" onclick={() => keyVisible.worker = !keyVisible.worker} title={keyVisible.worker ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
                    <Icon name={keyVisible.worker ? 'eye-slash' : 'eye'} size={14} />
                  </button>
                </div>
              </div>
              <div class="llm-config-field-row has-thinking" class:has-level={workerConfigs[workerModelTab].provider === 'openai'}>
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
                  <div class="model-combobox">
                    <input type="text" class="llm-config-input" bind:value={workerConfigs[workerModelTab].model} placeholder="claude-3-5-sonnet-20241022"
                      onfocus={(e) => { if (modelLists[workerModelTab]?.length > 0) openModelDropdown(workerModelTab, e.currentTarget); }}
                    >
                    {#if !workerConfigs[workerModelTab].model && (!modelLists[workerModelTab] || modelLists[workerModelTab].length === 0)}
                      <button class="model-fetch-btn" onclick={() => fetchModelList('worker')} disabled={fetchingModels[workerModelTab] || !workerConfigs[workerModelTab].baseUrl || !workerConfigs[workerModelTab].apiKey}>
                        {#if fetchingModels[workerModelTab]}
                          <Icon name="refresh" size={12} />
                        {:else}
                          <Icon name="download" size={12} />
                        {/if}
                      </button>
                    {/if}
                    {#if modelDropdownOpen[workerModelTab] && modelLists[workerModelTab]?.length > 0}
                      <div class="model-dropdown" style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;">
                        {#each modelLists[workerModelTab] as m}
                          <button class="model-dropdown-item" class:selected={workerConfigs[workerModelTab].model === m} onclick={() => selectModel(workerModelTab, m)}>{m}</button>
                        {/each}
                      </div>
                    {/if}
                  </div>
                </div>
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.provider')}</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].provider}>
                    <option value="openai">{i18n.t('settings.model.provider.openai')}</option>
                    <option value="anthropic">{i18n.t('settings.model.provider.anthropic')}</option>
                  </select>
                </div>
                {#if workerConfigs[workerModelTab].provider === 'openai'}
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.protocol')}</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].openaiProtocol}>
                    <option value="responses">{i18n.t('settings.model.protocol.responses')}</option>
                    <option value="chat">{i18n.t('settings.model.protocol.chat')}</option>
                  </select>
                </div>
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.level')}</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].reasoningEffort}>
                    <option value="low">{i18n.t('settings.model.reasoning.low')}</option>
                    <option value="medium">{i18n.t('settings.model.reasoning.medium')}</option>
                    <option value="high">{i18n.t('settings.model.reasoning.high')}</option>
                    <option value="xhigh">{i18n.t('settings.model.reasoning.xhigh')}</option>
                  </select>
                </div>
                {/if}
                <div class="llm-config-field inline-toggle">
                  <label class="llm-config-label">{i18n.t('settings.model.field.thinking')}</label>
                  <button type="button" class="llm-config-toggle-btn" title={workerConfigs[workerModelTab].thinking ? i18n.t('settings.model.disableThinking') : i18n.t('settings.model.enableThinking')} onclick={() => workerConfigs[workerModelTab].thinking = !workerConfigs[workerModelTab].thinking}>
                    <span class="toggle-switch" class:active={workerConfigs[workerModelTab].thinking}></span>
                  </button>
                </div>
              </div>
              <div class="llm-config-actions">
                <button
                  class="llm-config-save-btn"
                  class:saving={saveStatus[workerModelTab] === 'saving'}
                  class:saved={saveStatus[workerModelTab] === 'saved'}
                  onclick={() => saveModelConfig('worker')}
                  disabled={saveStatus[workerModelTab] === 'saving'}
                >
                  {#if saveStatus[workerModelTab] === 'saving'}
                    <Icon name="refresh" size={14} />
                    {i18n.t('settings.model.saving')}
                  {:else if saveStatus[workerModelTab] === 'saved'}
                    <Icon name="check" size={14} />
                    {i18n.t('settings.model.saved')}
                  {:else}
                    {i18n.t('settings.model.saveConfig')}
                  {/if}
                </button>
                <button
                  class="llm-config-test-btn"
                  class:testing={testStatus[workerModelTab] === 'testing'}
                  class:success={testStatus[workerModelTab] === 'success'}
                  class:error={testStatus[workerModelTab] === 'error'}
                  onclick={() => testModelConnection('worker')}
                  disabled={testStatus[workerModelTab] === 'testing'}
                >
                  {#if testStatus[workerModelTab] === 'testing'}
                    <Icon name="refresh" size={14} />
                    {i18n.t('settings.model.testing')}
                  {:else if testStatus[workerModelTab] === 'success'}
                    <Icon name="check" size={14} />
                    {i18n.t('settings.model.testSuccess')}
                  {:else if testStatus[workerModelTab] === 'error'}
                    <Icon name="close" size={14} />
                    {i18n.t('settings.model.testFailed')}
                  {:else}
                    <Icon name="check" size={14} />
                    {i18n.t('settings.model.testConnection')}
                  {/if}
                </button>
              </div>
            </div>
          </div>
        </div>
      {:else if activeTab === 'profile'}
        <!-- 画像配置 Tab -->
        <!-- Worker 分工配置 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">{i18n.t('settings.profile.workerAssignment')}</div>
          </div>
          <div class="settings-section-desc">{i18n.t('settings.profile.configStorage')}<code>~/.magi/worker-assignments.json</code></div>
          <div class="profile-categories-grid">
            {#each categoryOrder as category}
              {#if categoryLabels[category]}
                <div class="profile-category-row">
                  <div class="profile-category-label-group">
                    <span class="profile-category-label">{categoryLabels[category]()}</span>
                  <button
                    class="profile-guidance-btn"
                    aria-label={i18n.t('settings.profile.viewGuidance')}
                    aria-expanded={openGuidanceCategory === category}
                    onmouseenter={(event) => showGuidance(category, event)}
                    onmousemove={(event) => moveGuidance(category, event)}
                    onmouseleave={() => hideGuidance(category)}
                  >
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                      <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1.6" />
                      <path d="M12 10.2v6" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
                      <circle cx="12" cy="7.2" r="1.2" fill="currentColor" />
                    </svg>
                  </button>
                </div>
                <select class="profile-category-select" bind:value={taskCategories[category]}>
                  <option value="claude">{i18n.t('settings.model.worker.claude')}</option>
                    <option value="codex">{i18n.t('settings.model.worker.codex')}</option>
                    <option value="gemini">{i18n.t('settings.model.worker.gemini')}</option>
                  </select>
              </div>
              {/if}
            {/each}
          </div>
        </div>

        {#if guidancePopover && categoryGuidance[guidancePopover.category]}
          <div
            class="profile-guidance-popover"
            style={`top:${guidancePopover.top}px; left:${guidancePopover.left}px; width:${guidancePopover.width}px;`}
          >
            <div class="profile-guidance-header">
              <div class="profile-guidance-title">
                {categoryGuidance[guidancePopover.category].displayName}
                <span class="profile-guidance-id">({guidancePopover.category})</span>
              </div>
              <div class="profile-guidance-badges">
                <span class="profile-badge priority-{categoryGuidance[guidancePopover.category].priority}">
                  {i18n.t('settings.profile.priority', { level: categoryGuidance[guidancePopover.category].priority })}
                </span>
                <span class="profile-badge risk-{categoryGuidance[guidancePopover.category].riskLevel}">
                  {i18n.t('settings.profile.risk', { level: categoryGuidance[guidancePopover.category].riskLevel })}
                </span>
              </div>
            </div>
            <div class="profile-guidance-desc">{categoryGuidance[guidancePopover.category].description}</div>
            <div class="profile-guidance-columns">
              <div class="profile-guidance-block">
                <div class="profile-guidance-label">{i18n.t('settings.profile.focusArea')}</div>
                <ul class="profile-guidance-listing">
                  {#each categoryGuidance[guidancePopover.category].guidance.focus as item}
                    <li>{item}</li>
                  {/each}
                </ul>
              </div>
              <div class="profile-guidance-block">
                <div class="profile-guidance-label">{i18n.t('settings.profile.constraints')}</div>
                <ul class="profile-guidance-listing">
                  {#each categoryGuidance[guidancePopover.category].guidance.constraints as item}
                    <li>{item}</li>
                  {/each}
                </ul>
              </div>
            </div>
          </div>
        {/if}

        <!-- 全局用户规则 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">{i18n.t('settings.profile.userRules')}</div>
          </div>
          <div class="settings-section-desc">{i18n.t('settings.profile.userRulesDesc')}</div>
          <div class="profile-editor">
            <div class="profile-field">
              <textarea
                class="profile-textarea user-rules-textarea"
                bind:value={userRules}
                placeholder={i18n.t('settings.profile.userRulesPlaceholder')}
              ></textarea>
            </div>
          </div>
        </div>
        <div class="profile-save-footer">
          <div class="profile-save-hint">{i18n.t('settings.profile.saveHint')}</div>
          <div class="profile-save-actions">
            <button
              class="settings-btn secondary"
              class:saving={profileResetStatus === 'saving'}
              onclick={resetProfile}
              disabled={profileResetStatus === 'saving' || profileSaveStatus === 'saving'}
            >
              {#if profileResetStatus === 'saving'}
                <Icon name="refresh" size={14} />
                {i18n.t('settings.profile.processing')}
              {:else if profileResetStatus === 'saved'}
                <Icon name="check" size={14} />
                {i18n.t('settings.profile.resetDone')}
              {:else if profileResetStatus === 'error'}
                <Icon name="close" size={14} />
                {i18n.t('settings.profile.resetFailed')}
              {:else}
                {i18n.t('settings.profile.resetAll')}
              {/if}
            </button>
            <button
              class="settings-btn primary"
              class:saving={profileSaveStatus === 'saving'}
              class:saved={profileSaveStatus === 'saved'}
              onclick={saveProfile}
              disabled={profileSaveStatus === 'saving' || profileResetStatus === 'saving'}
            >
              {#if profileSaveStatus === 'saving'}
                <Icon name="refresh" size={14} />
                {i18n.t('settings.profile.savingProfile')}
              {:else if profileSaveStatus === 'saved'}
                <Icon name="check" size={14} />
                {i18n.t('settings.profile.savedProfile')}
              {:else if profileSaveStatus === 'error'}
                <Icon name="close" size={14} />
                {i18n.t('settings.profile.saveFailed')}
              {:else}
                {i18n.t('settings.profile.saveAll')}
              {/if}
            </button>
          </div>
        </div>
      {:else if activeTab === 'tools'}
        <!-- 工具 Tab -->
        <!-- MCP 工具 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">{i18n.t('settings.tools.mcpTools')}</div>
            <button class="settings-btn primary" onclick={() => openMCPDialog(null)}>
              <Icon name="plus" size={14} />
              <span>{i18n.t('settings.tools.addServer')}</span>
            </button>
          </div>
          <div class="settings-section-desc">{i18n.t('settings.tools.mcpDesc')}</div>
          <div class="mcp-server-list">
            {#if mcpServers.length === 0}
              <div class="empty-state">
                <Icon name="tools" size={48} />
                <p>{i18n.t('settings.tools.noMcpServer')}</p>
                <p class="empty-state-hint">{i18n.t('settings.tools.noMcpServerHint')}</p>
              </div>
            {:else}
              {#each mcpServers as server (server.id)}
                <div class="mcp-server-item" class:expanded={mcpExpandedServer === server.id}>
                  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                  <div class="mcp-server-header" onclick={() => toggleMCPExpand(server.id)}>
                    <div class="mcp-server-info">
                      <div class="mcp-server-name">{server.name}</div>
                      <div class="mcp-server-command">{server.type === 'stdio' ? (server.command || '') : (server.url || '')}</div>
                    </div>
                    <div class="mcp-server-actions">
                      <span class="mcp-server-badge" class:enabled={server.enabled} class:disabled={!server.enabled}>
                        {server.enabled ? i18n.t('settings.tools.enabled') : i18n.t('settings.tools.disabledLabel')}
                      </span>
                      <span
                        class="mcp-server-badge mcp-server-health-badge"
                        class:connected={server.health === 'connected'}
                        class:degraded={server.health === 'degraded'}
                        class:disconnected={server.health === 'disconnected' || !server.health}
                        title={server.error || getMCPHealthLabel(server)}
                      >
                        {getMCPHealthLabel(server)}
                      </span>
                      <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                      <span onclick={(e) => e.stopPropagation()}>
                        <Toggle
                          checked={server.enabled}
                          title={server.enabled ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                          onchange={() => toggleMCPServer(server.id, server.enabled)}
                        />
                      </span>
                      <button class="btn-icon btn-icon--sm" title={i18n.t('settings.tools.edit')}
                        onclick={(e) => { e.stopPropagation(); openMCPDialog(server); }}>
                        <Icon name="edit" size={14} />
                      </button>
                      <button class="btn-icon btn-icon--sm btn-icon--danger" title={i18n.t('settings.tools.delete')}
                        onclick={(e) => { e.stopPropagation(); deleteMCPServer(server.id); }}>
                        <Icon name="trash" size={14} />
                      </button>
                      <span class="mcp-expand-icon" class:expanded={mcpExpandedServer === server.id}>
                        <Icon name="chevronDown" size={14} />
                      </span>
                    </div>
                  </div>
                  {#if mcpExpandedServer === server.id}
                    <div class="mcp-tools-panel">
                      <div class="mcp-server-runtime">
                        <span>{i18n.t('settings.tools.runtimeStatus', { status: getMCPHealthLabel(server) })}</span>
                        <span>{i18n.t('settings.tools.reconnectCount', { count: server.reconnectAttempts || 0 })}</span>
                        {#if server.error}
                          <span class="mcp-server-runtime-error" title={server.error}>{i18n.t('settings.tools.lastError', { error: server.error })}</span>
                        {/if}
                      </div>
                      <div class="mcp-tools-header">
                        <span>{i18n.t('settings.tools.toolList')} {mcpServerTools[server.id]?.length ? `(${mcpServerTools[server.id].length})` : ''}</span>
                        <button class="btn-icon btn-icon--sm" class:refreshing={mcpRefreshingServers.has(server.id)} title={i18n.t('settings.tools.refreshTools')}
                          onclick={() => refreshMCPTools(server.id)} disabled={mcpRefreshingServers.has(server.id)}>
                          <Icon name="refresh" size={14} />
                        </button>
                      </div>
                      <div class="mcp-tools-list">
                        {#if mcpRefreshingServers.has(server.id)}
                          <div class="mcp-tools-empty">{i18n.t('settings.tools.loading')}</div>
                        {:else if mcpServerTools[server.id] && mcpServerTools[server.id].length > 0}
                          {#each mcpServerTools[server.id] as tool, toolIndex}
                            {@const toolKey = `${server.id}-${toolIndex}`}
                            <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
                            <div class="mcp-tool-item" class:show-desc={mcpExpandedTool === toolKey}>
                              <div class="mcp-tool-row">
                                <div class="mcp-tool-name">{tool.name}</div>
                                {#if tool.description}
                                  <button class="mcp-tool-desc-btn" title={i18n.t('settings.tools.viewDesc')} onclick={(e) => toggleMCPToolDesc(toolKey, e)}>
                                    <Icon name="info" size={14} />
                                  </button>
                                {/if}
                              </div>
                              {#if tool.description}
                                <div class="mcp-tool-desc">{tool.description}</div>
                                <div class="mcp-tool-desc-pop">{tool.description}</div>
                              {/if}
                            </div>
                          {/each}
                        {:else}
                          <div class="mcp-tools-empty">{i18n.t('settings.tools.noToolsHint')}</div>
                        {/if}
                      </div>
                    </div>
                  {/if}
                </div>
              {/each}
            {/if}
          </div>
        </div>

        <!-- Claude Skills 工具 -->
        <div class="settings-section">
          <div class="settings-section-header">
            <div class="settings-section-title">{i18n.t('settings.tools.claudeSkills')}</div>
            <div class="settings-btn-group">
              <button class="settings-btn primary" onclick={() => openSkillLibraryDialog()}>
                <Icon name="plus" size={14} />
                <span>{i18n.t('settings.tools.installSkill')}</span>
              </button>
              {#if skills.length > 0}
                <button class="settings-btn secondary" onclick={() => updateAllSkills()} disabled={updatingAllSkills}>
                  <Icon name="refresh" size={14} />
                  <span>{updatingAllSkills ? i18n.t('settings.tools.updatingAll') : i18n.t('settings.tools.updateAll')}</span>
                </button>
              {/if}
              <button class="settings-btn secondary" onclick={() => openRepoDialog()}>
                <Icon name="grid" size={14} />
                <span>{i18n.t('settings.tools.manageRepos')}</span>
              </button>
            </div>
          </div>
          <div class="settings-section-desc">{i18n.t('settings.tools.skillsDesc')}</div>
          <div class="skills-tool-list">
            {#if skills.length === 0}
              <div class="empty-state">
                <Icon name="tools" size={48} />
                <p>{i18n.t('settings.tools.noSkills')}</p>
                <p class="empty-state-hint">{i18n.t('settings.tools.noSkillsHint')}</p>
              </div>
            {:else}
              {#each skills as skill}
                <div class="skill-item">
                  <div class="skill-info">
                    <div class="skill-name">{skill.name}</div>
                    <div class="skill-desc" title={skill.description}>{skill.description}</div>
                  </div>
                  <div class="skill-actions">
                    <span class="skill-source-badge" class:custom={skill.source === 'custom'} class:instruction={skill.source === 'instruction'}>
                      {skill.source === 'custom' ? i18n.t('settings.tools.custom') : 'Instruction'}
                    </span>
                    <button
                      class="btn-icon btn-icon--sm"
                      title={i18n.t('settings.tools.update')}
                      disabled={updatingSkills.has(skill.name) || updatingAllSkills}
                      onclick={() => updateSkill(skill.name)}
                    >
                      {#if updatingSkills.has(skill.name)}
                        <Icon name="refresh" size={14} />
                      {:else}
                        <Icon name="refresh" size={14} />
                      {/if}
                    </button>
                    <button class="btn-icon btn-icon--sm btn-icon--danger" title={i18n.t('settings.tools.delete')} onclick={() => deleteSkill(skill)}>
                      <Icon name="trash" size={14} />
                    </button>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<!-- 输入对话框 -->
{#if showInputDialog}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay modal-overlay--top" onkeydown={(e) => e.key === 'Escape' && cancelInputDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog modal-dialog--sm" onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>{inputDialogTitle}</h3>
      </div>
      <div class="modal-body">
        <div class="form-field">
          <input type="text" bind:value={inputDialogValue} placeholder={i18n.t('settings.dialog.inputPlaceholder')}>
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={cancelInputDialog}>{i18n.t('settings.dialog.cancel')}</button>
        <button class="settings-btn primary" onclick={confirmInputDialog}>{i18n.t('settings.dialog.confirm')}</button>
      </div>
    </div>
  </div>
{/if}

<!-- MCP 对话框 -->
{#if showMCPDialogState}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onkeydown={(e) => e.key === 'Escape' && closeMCPDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog" onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>{mcpDialogIsEdit ? i18n.t('settings.mcp.editTitle') : i18n.t('settings.mcp.addTitle')}</h3>
        <button class="modal-close" onclick={closeMCPDialog}>×</button>
      </div>
      <div class="modal-body">
        <div class="form-field">
          <label for="mcp-json">{i18n.t('settings.mcp.jsonLabel')}</label>
          <textarea id="mcp-json" rows="12" placeholder={i18n.t('settings.mcp.jsonPlaceholder')} bind:value={mcpDialogJson} oninput={() => mcpDialogError = ''}></textarea>
          {#if mcpDialogError}
            <div class="form-error">{mcpDialogError}</div>
          {:else}
            <div class="form-help">
              {i18n.t('settings.mcp.jsonHelp')} stdio: {'{'} "mcpServers": {'{'} "name": {'{'} "command": "...", "args": [...] {'}'} {'}'} {'}'}；HTTP: {'{'} "mcpServers": {'{'} "name": {'{'} "url": "https://...", "headers": {'{'} ... {'}'} {'}'} {'}'} {'}'}
            </div>
          {/if}
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={closeMCPDialog}>{i18n.t('settings.dialog.cancel')}</button>
        <button
          class="settings-btn primary"
          class:saving={saveStatus.mcp === 'saving'}
          class:saved={saveStatus.mcp === 'saved'}
          onclick={saveMCPServer}
          disabled={saveStatus.mcp === 'saving'}
        >
          {#if saveStatus.mcp === 'saving'}
            <Icon name="refresh" size={14} />
            {i18n.t('settings.mcp.saving')}
          {:else if saveStatus.mcp === 'saved'}
            <Icon name="check" size={14} />
            {i18n.t('settings.mcp.saved')}
          {:else}
            {i18n.t('settings.mcp.save')}
          {/if}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- 仓库管理对话框 -->
{#if showRepoDialogState}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onkeydown={(e) => e.key === 'Escape' && closeRepoDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog modal-dialog-lg" onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>{i18n.t('settings.repo.title')}</h3>
        <button class="modal-close" onclick={closeRepoDialog}>×</button>
      </div>
      <div class="modal-body">
        <div class="repo-add-section">
          <div class="repo-add-form">
            <div class="form-field" style="flex: 1; margin-bottom: 0;">
              <label for="repo-url">{i18n.t('settings.repo.urlLabel')}</label>
              <input type="text" id="repo-url" placeholder={i18n.t('settings.repo.urlPlaceholder')} bind:value={repoAddUrl}>
            </div>
            <button class="settings-btn primary" onclick={addRepository} disabled={repoAddLoading}>
              <Icon name="plus" size={14} />
              <span>{repoAddLoading ? i18n.t('settings.repo.adding') : i18n.t('settings.repo.add')}</span>
            </button>
          </div>
        </div>
        <div class="repo-list-title">{i18n.t('settings.repo.addedRepos')}</div>
        <div class="repo-manage-list">
          {#if repositoriesLoading}
            <div class="loading-state">
              <Icon name="refresh" size={24} />
              <span>{i18n.t('settings.repo.loading')}</span>
            </div>
          {:else if repositories.length === 0}
            <div class="empty-state-sm">{i18n.t('settings.repo.noRepos')}</div>
          {:else}
            {#each repositories as repo (repo.id)}
              <div class="repo-item">
                <div class="repo-info">
                  <div class="repo-name">{repo.name || repo.url}</div>
                  <div class="repo-url">{repo.url}</div>
                  {#if repo.skillCount}
                    <div class="repo-meta">{i18n.t('settings.repo.skillCount', { count: repo.skillCount })}</div>
                  {/if}
                </div>
                <div class="repo-actions">
                  <button class="btn-icon btn-icon--sm" title={i18n.t('settings.repo.refresh')} onclick={() => refreshRepository(repo.id)}>
                    <Icon name="refresh" size={14} />
                  </button>
                  <button class="btn-icon btn-icon--sm" title={i18n.t('settings.tools.delete')} onclick={() => deleteRepository(repo.id)}>
                    <Icon name="close" size={14} />
                  </button>
                </div>
              </div>
            {/each}
          {/if}
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={closeRepoDialog}>{i18n.t('settings.repo.close')}</button>
      </div>
    </div>
  </div>
{/if}

<!-- Skill 库对话框 -->
{#if showSkillLibraryDialogState}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onkeydown={(e) => e.key === 'Escape' && closeSkillLibraryDialog()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog modal-dialog-lg" role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>{i18n.t('settings.skillLibrary.title')}</h3>
        <button class="modal-close" onclick={closeSkillLibraryDialog}>×</button>
      </div>
      <div class="modal-body">
        <div class="skill-library-search">
          <div class="skill-library-search-row">
            <input type="text" placeholder={i18n.t('settings.skillLibrary.searchPlaceholder')} bind:value={skillSearchQuery}>
            <button class="settings-btn secondary skill-library-import-btn" onclick={installLocalSkill} disabled={localSkillInstalling}>
              {#if localSkillInstalling}
                <Icon name="refresh" size={14} />
                {i18n.t('settings.skillLibrary.importing')}
              {:else}
                <Icon name="plus" size={14} />
                {i18n.t('settings.skillLibrary.localImport')}
              {/if}
            </button>
          </div>
        </div>
        <div class="skill-library-list">
          {#if skillLibraryFailedRepositories.length > 0}
            <div class="skill-library-warning">
              <div class="skill-library-warning-title">
                {i18n.t('settings.skillLibrary.failedRepos', { count: skillLibraryFailedRepositories.length })}
              </div>
              <div class="skill-library-warning-list">
                {#each skillLibraryFailedRepositories as repo}
                  <div class="skill-library-warning-item">
                    <strong>{repo.repositoryId}</strong>
                    {#if repo.error}
                      <span>：{repo.error}</span>
                    {/if}
                  </div>
                {/each}
              </div>
            </div>
          {/if}
          {#if skillLibraryLoading}
            <div class="loading-state">
              <Icon name="refresh" size={32} />
              <span>{i18n.t('settings.skillLibrary.loadingSkills')}</span>
            </div>
          {:else if filteredLibrarySkills.length === 0}
            <div class="empty-state">
              <Icon name="tools" size={48} />
              <p>{i18n.t('settings.skillLibrary.noSkillsAvailable')}</p>
              <p class="empty-state-hint">{i18n.t('settings.skillLibrary.noSkillsHint')}</p>
            </div>
          {:else}
            {#each Object.entries(skillsByRepo) as [_, repoData]}
              <div class="skill-repo-group">
                <div class="skill-repo-title">{i18n.t('settings.skillLibrary.repoSkillCount', { name: repoData.name, count: repoData.skills.length })}</div>
                {#each repoData.skills as skill}
                  <div class="skill-library-item">
                    <div class="skill-library-icon">
                      <Icon name="tools" size={14} />
                    </div>
                    <div class="skill-library-info">
                      <div class="skill-library-name">{skill.name}</div>
                      <div class="skill-library-desc" title={skill.description || ''}>{skill.description || ''}</div>
                      {#if skill.author || skill.version || skill.category}
                        <div class="skill-library-meta">
                          {#if skill.author}<span class="skill-library-meta-item">{i18n.t('settings.skillLibrary.author', { name: skill.author })}</span>{/if}
                          {#if skill.version}<span class="skill-library-meta-item">{i18n.t('settings.skillLibrary.version', { version: skill.version })}</span>{/if}
                          {#if skill.category}<span class="skill-library-meta-item">{i18n.t('settings.skillLibrary.category', { category: skill.category })}</span>{/if}
                        </div>
                      {/if}
                    </div>
                    <div class="skill-library-actions">
                      <button
                        class="settings-btn"
                        class:primary={!skill.installed && !installingSkills.has(skill.fullName)}
                        class:saving={installingSkills.has(skill.fullName)}
                        onclick={() => installSkill(skill.fullName)}
                        disabled={skill.installed || installingSkills.has(skill.fullName)}
                      >
                        {#if installingSkills.has(skill.fullName)}
                          <Icon name="refresh" size={14} />
                          {i18n.t('settings.skillLibrary.installing')}
                        {:else if skill.installed}
                          {i18n.t('settings.skillLibrary.installed')}
                        {:else}
                          {i18n.t('settings.skillLibrary.install')}
                        {/if}
                      </button>
                    </div>
                  </div>
                {/each}
              </div>
            {/each}
          {/if}
        </div>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={closeSkillLibraryDialog}>{i18n.t('settings.skillLibrary.close')}</button>
      </div>
    </div>
  </div>
{/if}

<!-- 重置 Token 确认对话框 -->
{#if showResetConfirm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay modal-overlay--top" role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <div class="modal-title">{i18n.t('settings.resetConfirm.title')}</div>
        <button class="modal-close" onclick={cancelResetStats}>×</button>
      </div>
      <div class="modal-body">
        <p style="margin: 0; color: var(--foreground);">{i18n.t('settings.resetConfirm.message')}</p>
        <p style="margin: var(--space-2) 0 0; color: var(--foreground-muted); font-size: var(--text-sm);">{i18n.t('settings.resetConfirm.irreversible')}</p>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={cancelResetStats}>{i18n.t('settings.resetConfirm.cancel')}</button>
        <button class="settings-btn primary" onclick={confirmResetStats}>{i18n.t('settings.resetConfirm.confirm')}</button>
      </div>
    </div>
  </div>
{/if}

<!-- 通用确认对话框 -->
{#if showConfirmDialog}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal-overlay modal-overlay--top" role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-dialog modal-dialog--sm" role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <div class="modal-title">{confirmDialogTitle}</div>
        <button class="modal-close" onclick={handleConfirmNo}>×</button>
      </div>
      <div class="modal-body">
        <p style="margin: 0; color: var(--foreground);">{confirmDialogMessage}</p>
      </div>
      <div class="modal-footer">
        <button class="settings-btn secondary" onclick={handleConfirmNo}>{i18n.t('settings.confirmDialog.cancel')}</button>
        <button class="settings-btn primary" onclick={handleConfirmYes}>{i18n.t('settings.confirmDialog.confirm')}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* ============================================
     Settings Panel - 优化后的样式
     ============================================ */

  /* 基础面板布局 */
  .settings-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

  .settings-panel {
    width: 90%;
    max-width: 640px;
    max-height: 85vh;
    background: var(--vscode-editor-background, #1e1e1e);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow:
      0 25px 50px -12px rgba(0, 0, 0, 0.25),
      0 0 0 1px rgba(255, 255, 255, 0.05);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: slideUp var(--duration-normal) var(--ease-out);
  }

  .settings-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
    gap: var(--space-3);
  }

  .locale-selector {
    display: flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
    margin-left: auto;
  }

  .locale-btn {
    padding: var(--space-1) var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    cursor: pointer;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .locale-btn:hover {
    color: var(--foreground);
    background: var(--surface-3);
  }

  .locale-btn.active {
    background: var(--primary);
    color: var(--primary-foreground, #fff);
  }

  .locale-btn + .locale-btn {
    border-left: 1px solid var(--border);
  }

  .settings-title {
    font-size: var(--text-xl);
    font-weight: var(--font-bold);
    color: var(--foreground);
  }

  .settings-tabs {
    display: flex;
    gap: 0;
    padding: 0;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }

  .settings-tab {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    flex: 1;
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: all var(--transition-fast);
    position: relative;
  }

  .settings-tab:hover {
    color: var(--foreground);
    background: var(--surface-hover);
  }
  .settings-tab.active {
    color: var(--primary);
    border-bottom-color: var(--primary);
    font-weight: var(--font-semibold);
  }

  .settings-tab-content {
    flex: 1;
    overflow-y: auto;
    padding: 5px;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .settings-tab-content::-webkit-scrollbar { width: 6px; }
  .settings-tab-content::-webkit-scrollbar-track { background: transparent; }
  .settings-tab-content::-webkit-scrollbar-thumb { background: var(--scrollbar-thumb); border-radius: 3px; }
  .settings-tab-content::-webkit-scrollbar-thumb:hover { background: var(--scrollbar-thumb-hover); }

  /* 动画 */
  @keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }
  @keyframes slideUp { from { opacity: 0; transform: translateY(20px) scale(0.98); } to { opacity: 1; transform: translateY(0) scale(1); } }
  @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* Section 容器 */
  .settings-section {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-5);
    transition: border-color var(--transition-fast);
  }
  .settings-section:hover {
    border-color: var(--primary-muted);
  }

  .settings-section-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-4);
    flex-wrap: wrap;
    gap: var(--space-3);
  }

  .settings-section-title {
    font-size: var(--text-base);
    font-weight: var(--font-bold);
    color: var(--foreground);
  }

  .settings-section-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin-bottom: var(--space-4);
    line-height: 1.5;
  }

  .settings-section-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .settings-btn-group {
    display: flex;
    gap: var(--space-2);
  }

  /* 统一按钮样式 */
  .settings-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    background: var(--primary);
    border: none;
    border-radius: var(--radius-md);
    color: white;
    cursor: pointer;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .settings-btn:hover:not(:disabled) {
    background: var(--primary-hover);
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(var(--primary-rgb, 99, 102, 241), 0.3);
  }
  .settings-btn:active:not(:disabled) {
    transform: translateY(0);
  }
  .settings-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .settings-btn.secondary {
    background: var(--surface-3);
    color: var(--foreground);
    border: 1px solid var(--border);
  }
  .settings-btn.secondary:hover:not(:disabled) {
    background: var(--surface-hover);
    border-color: var(--primary-muted);
    transform: translateY(-1px);
    box-shadow: none;
  }

  /* settings-btn 保存状态样式 */
  .settings-btn.saving {
    background: var(--warning-muted);
    color: var(--warning);
    border-color: var(--warning-muted);
  }
  .settings-btn.saving :global(svg) { animation: spin 1s linear infinite; }
  .settings-btn.saved {
    background: var(--success-muted);
    color: var(--success);
    border-color: var(--success-muted);
  }

  /* 统计 Tab 专用样式 */
  .stats-section { padding: var(--space-4); }
  .stats-section .settings-section-header { margin-bottom: var(--space-3); }
  .stats-section .settings-section-actions { width: 100%; }

  .stats-action-buttons {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-left: auto;
  }

  .settings-summary-chip {
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    background: var(--primary-muted);
    color: var(--primary);
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    white-space: nowrap;
  }

  /* 模型连接状态 - 紧凑布局 */
  .model-connection-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .model-connection-item {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }

  .model-connection-item:hover {
    border-color: var(--primary-muted);
  }
  .model-connection-item.success {
    border-left: 3px solid var(--success);
  }
  .model-connection-item.error {
    border-left: 3px solid var(--error);
  }
  .model-connection-item.warning {
    border-left: 3px solid var(--warning);
  }
  .model-connection-item.disabled {
    border-left: 3px solid var(--foreground-muted);
    opacity: 0.6;
  }
  .model-connection-item.checking {
    border-left: 3px solid var(--info);
  }

  .model-connection-icon {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius-md);
    flex-shrink: 0;
  }

  .model-connection-icon.claude { background: var(--color-claude-muted); color: var(--color-claude); }
  .model-connection-icon.codex { background: var(--color-codex-muted); color: var(--color-codex); }
  .model-connection-icon.gemini { background: var(--color-gemini-muted); color: var(--color-gemini); }
  .model-connection-icon.orchestrator { background: var(--color-orchestrator-muted); color: var(--color-orchestrator); }
  .model-connection-icon.auxiliary { background: var(--color-auxiliary-muted); color: var(--color-auxiliary); }

  .model-connection-info { min-width: 0; overflow: hidden; flex: 1; }

  .model-connection-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .model-connection-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    display: flex;
    align-items: center;
  }

  .required-badge {
    font-size: 10px;
    color: var(--warning);
    background: rgba(245, 158, 11, 0.15);
    padding: 1px 4px;
    border-radius: var(--radius-sm);
    margin-left: var(--space-1);
  }

  .model-connection-model {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: 2px;
  }
  .model-connection-error {
    font-size: var(--text-xs);
    color: var(--error);
    margin-top: 2px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .model-connection-stats-inline {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: 4px;
  }
  .stats-divider {
    opacity: 0.3;
  }

  .model-connection-badge {
    height: 20px;
    padding: 0 var(--space-2);
    font-size: 10px;
    font-weight: var(--font-medium);
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .model-connection-badge.success { background: var(--success-muted); color: var(--success); }
  .model-connection-badge.checking { background: var(--info-muted); color: var(--info); }
  .model-connection-badge.warning { background: rgba(245, 158, 11, 0.15); color: var(--warning); }
  .model-connection-badge.disabled { background: var(--surface-3); color: var(--foreground-muted); }
  .model-connection-badge.error { background: var(--error-muted); color: var(--error); }

  .model-refresh-btn {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    height: var(--btn-height-sm);
    padding: 0 var(--space-2);
    font-size: var(--text-xs);
    background: var(--secondary);
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .model-refresh-btn:hover:not(:disabled) { background: var(--secondary-hover); }
  .model-refresh-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .model-refresh-btn.loading :global(svg) { animation: spin 1s linear infinite; }

  /* Worker Tab 切换 - 统一样式 */
  .worker-tabs {
    display: flex;
    gap: var(--space-2);
  }

  .worker-tab {
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

  .worker-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .worker-tab.active { background: var(--primary); border-color: var(--primary); color: white; }

  .worker-dot {
    width: 8px;
    height: 8px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .worker-dot.claude { background: var(--color-claude); }
  .worker-dot.codex { background: var(--color-codex); }
  .worker-dot.gemini { background: var(--color-gemini); }

  /* Worker Model Tabs - 放在标题右侧 */
  .worker-model-tabs { display: flex; gap: var(--space-2); }
  .worker-model-tab { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: transparent; border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); }
  .worker-model-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .worker-model-tab.active { background: var(--primary); border-color: var(--primary); color: white; }
  .profile-worker-tabs { display: flex; gap: var(--space-2); }
  .profile-worker-tab { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: transparent; border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); }
  .profile-worker-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .profile-worker-tab.active { background: var(--primary); border-color: var(--primary); color: white; }
  .profile-worker-dot { width: 8px; height: 8px; border-radius: var(--radius-full); }
  .profile-worker-dot.claude { background: var(--color-claude); }
  .profile-worker-dot.codex { background: var(--color-codex); }
  .profile-worker-dot.gemini { background: var(--color-gemini); }

  /* 模型配置表单 */
  .model-config-card {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-4);
  }

  .model-config-header-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: var(--space-4);
    flex-wrap: wrap;
    gap: var(--space-3);
  }

  .model-config-header { margin-bottom: var(--space-4); }
  .model-config-title { font-size: var(--text-base); font-weight: var(--font-semibold); color: var(--foreground); }
  .model-config-desc { font-size: var(--text-sm); color: var(--foreground-muted); margin-top: var(--space-1); }

  /* 模型配置顶部 Tab 切换 */
  .model-config-grid { display: flex; flex-direction: column; gap: var(--space-4); }
  .model-config-stack { display: flex; flex-direction: column; gap: var(--space-4); }
  .model-config-tabs { display: flex; gap: var(--space-1); border-bottom: 1px solid var(--border); margin-bottom: var(--space-4); }
  .model-config-tab {
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    font-size: var(--text-sm);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .model-config-tab:hover { color: var(--foreground); }
  .model-config-tab.active { color: var(--primary); border-bottom-color: var(--primary); }

  .llm-config-form { display: flex; flex-direction: column; gap: var(--space-3); }
  .llm-config-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .llm-config-field-row { display: grid; grid-template-columns: 1fr 96px; gap: var(--space-3); }
  .llm-config-field-row.has-thinking { grid-template-columns: 1fr 96px 72px; }
  .llm-config-field-row.has-thinking.has-level { grid-template-columns: 1fr 96px 88px 88px 72px; }
  .llm-config-field-row.url-mode-row { grid-template-columns: minmax(0, 1fr) 180px; align-items: end; }
  .llm-config-field-row.worker-url-mode-row { grid-template-columns: minmax(0, 1fr) 180px 88px; align-items: end; }
  .llm-config-label { font-size: var(--text-sm); color: var(--foreground-muted); }
  .llm-config-field--compact { min-width: 0; }
  .llm-config-hint {
    margin-top: var(--space-2);
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--foreground-muted);
  }

  .llm-config-input, .llm-config-select {
    height: var(--btn-height-md);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground);
    outline: none;
    transition: border-color var(--transition-fast);
    width: 100%;
    box-sizing: border-box;
  }

  .llm-config-input:focus, .llm-config-select:focus { border-color: var(--primary); }

  .segmented-control {
    display: grid;
    grid-template-columns: 1fr 1fr;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    overflow: hidden;
    min-width: 0;
    height: var(--btn-height-md);
    background: var(--surface-2);
  }

  .segmented-control__option {
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    cursor: pointer;
    transition: all var(--transition-fast);
    min-width: 0;
    padding: 0 var(--space-2);
    white-space: nowrap;
  }

  .segmented-control__option + .segmented-control__option {
    border-left: 1px solid var(--border);
  }

  .segmented-control__option:hover {
    color: var(--foreground);
    background: var(--surface-3);
  }

  .segmented-control__option.active {
    background: var(--primary);
    color: var(--primary-foreground, #fff);
  }

  .api-key-wrapper { position: relative; }
  .api-key-wrapper .api-key-input { padding-right: 32px; }
  .api-key-toggle {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
    opacity: 0.6;
  }
  .api-key-toggle:hover { background: var(--secondary); color: var(--foreground); opacity: 1; }

  .model-combobox { position: relative; }
  .model-combobox .llm-config-input { padding-right: 32px; }
  .model-fetch-btn {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .model-fetch-btn:hover:not(:disabled) { background: var(--secondary); color: var(--foreground); }
  .model-fetch-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .model-fetch-btn :global(svg) { animation: none; }
  .model-combobox:has(.model-fetch-btn:disabled) .model-fetch-btn :global(svg) { animation: none; }
  /* fetchingModels 状态下的旋转动画，由 Icon name="refresh" 触发 */

  .model-dropdown {
    position: fixed;
    z-index: 10000;
    max-height: 200px;
    overflow-y: auto;
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-top: none;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    box-shadow: 0 4px 12px rgba(0,0,0,0.3);
  }
  .model-dropdown-item {
    display: block;
    width: 100%;
    padding: 6px var(--space-3);
    font-size: var(--text-sm);
    text-align: left;
    border: none;
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    transition: background var(--transition-fast);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .model-dropdown-item:hover { background: var(--secondary); }
  .model-dropdown-item.selected { color: var(--primary); background: var(--primary-muted, rgba(var(--primary-rgb, 100,149,237), 0.1)); }

  .llm-config-actions { display: flex; justify-content: flex-end; gap: var(--space-2); margin-top: var(--space-3); }
  .llm-config-test-btn { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: var(--secondary); border: none; border-radius: var(--radius-sm); color: var(--foreground); cursor: pointer; transition: all var(--transition-fast); }
  .llm-config-test-btn:hover { background: var(--secondary-hover); }
  .llm-config-test-btn:disabled { opacity: 0.7; cursor: wait; }
  .llm-config-test-btn.testing :global(svg) { animation: spin 1s linear infinite; }
  .llm-config-test-btn.success { background: var(--success-muted); color: var(--success); }
  .llm-config-test-btn.error { background: var(--error-muted); color: var(--error); }
  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
  .llm-config-save-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    background: var(--primary);
    border: none;
    border-radius: var(--radius-sm);
    color: white;
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .llm-config-save-btn:hover { background: var(--primary-hover); }
  .llm-config-save-btn:disabled { opacity: 0.7; cursor: not-allowed; }
  .llm-config-save-btn.saving { background: var(--warning-muted); color: var(--warning); }
  .llm-config-save-btn.saving :global(svg) { animation: spin 1s linear infinite; }
  .llm-config-save-btn.saved { background: var(--success-muted); color: var(--success); }

  .llm-config-toggle-label { display: flex; align-items: center; gap: var(--space-2); font-size: var(--text-sm); cursor: pointer; }
  .llm-config-toggle-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-md);
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
    justify-content: flex-start;
    width: 100%;
  }
  .inline-toggle { display: flex; flex-direction: column; gap: var(--space-2); align-items: flex-start; }
  .toggle-switch { width: 32px; height: 18px; background: var(--secondary); border-radius: var(--radius-full); position: relative; transition: background var(--transition-fast); cursor: pointer; flex-shrink: 0; }
  .toggle-switch::after { content: ''; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; background: white; border-radius: var(--radius-full); transition: transform var(--transition-fast); }
  .toggle-switch.active { background: var(--primary); }
  .toggle-switch.active::after { transform: translateX(14px); }

  /* Profile 编辑器 */
  .profile-editor { display: flex; flex-direction: column; gap: var(--space-4); margin-top: var(--space-4); }
  .profile-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .profile-field-header { display: flex; justify-content: space-between; align-items: center; }
  .profile-field-label { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); }

  .profile-textarea {
    min-height: 80px;
    padding: var(--space-3);
    font-size: var(--text-sm);
    background: var(--vscode-input-background, #3c3c3c);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground);
    resize: vertical;
    outline: none;
    transition: border-color var(--transition-fast);
  }
  .user-rules-textarea { resize: none; }

  .user-rules-textarea {
    min-height: 140px;
  }

  .profile-textarea:focus { border-color: var(--primary); }
  .profile-two-columns { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-4); }
  .profile-column { min-width: 0; }

  .profile-add-btn { display: flex; align-items: center; justify-content: center; width: var(--btn-height-xs); height: var(--btn-height-xs); padding: 0; background: var(--primary); border: none; border-radius: var(--radius-sm); color: white; cursor: pointer; transition: all var(--transition-fast); }
  .profile-add-btn:hover { background: var(--primary-hover); }

  .profile-tags { display: flex; flex-wrap: wrap; gap: var(--space-2); min-height: var(--btn-height-lg); padding: var(--space-3); background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius-sm); }
  .profile-tag { display: inline-flex; align-items: center; gap: var(--space-2); height: var(--btn-height-xs); padding: 0 var(--space-3); font-size: var(--text-sm); background: var(--primary); border-radius: var(--radius-full); color: white; }
  .profile-tag-remove { display: flex; align-items: center; justify-content: center; width: 14px; height: 14px; padding: 0; background: rgba(0, 0, 0, 0.2); border: none; border-radius: var(--radius-full); color: white; cursor: pointer; transition: background var(--transition-fast); }
  .profile-tag-remove:hover { background: rgba(0, 0, 0, 0.4); }
  .profile-tags-empty { font-size: var(--text-sm); color: var(--foreground-muted); font-style: italic; }

  .profile-categories-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: var(--space-2); margin-top: var(--space-3); }
  .profile-category-row { position: relative; display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); height: var(--btn-height-md); padding: 0 var(--space-3); background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius-sm); }
  .profile-category-label-group { display: flex; align-items: center; gap: var(--space-2); min-width: 0; flex: 1; }
  .profile-category-label { font-size: var(--text-sm); color: var(--foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; }
  .profile-category-select { height: var(--btn-height-xs); padding: 0 var(--space-2); font-size: var(--text-xs); background: var(--vscode-input-background, #3c3c3c); border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground); outline: none; }
  .profile-guidance-btn { display: inline-flex; align-items: center; justify-content: center; width: 26px; height: 26px; border-radius: var(--radius-full); border: 1px solid var(--border); background: transparent; color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); }
  .profile-guidance-btn:hover { background: var(--surface-hover); color: var(--foreground); }
  .profile-guidance-btn:focus-visible { outline: 2px solid var(--primary); outline-offset: 2px; }
  .profile-guidance-btn svg { width: 14px; height: 14px; }
  .profile-guidance-popover { position: fixed; z-index: var(--z-modal); background: var(--vscode-editor-background, #1e1e1e); border: 1px solid var(--border); border-radius: var(--radius-md); padding: var(--space-4); box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35); display: flex; flex-direction: column; gap: var(--space-3); max-height: 70vh; overflow-y: auto; pointer-events: none; }
  .profile-guidance-header { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-2); }
  .profile-guidance-title { font-size: var(--text-md); font-weight: var(--font-semibold); color: var(--foreground); }
  .profile-guidance-id { font-size: var(--text-xs); color: var(--foreground-muted); margin-left: var(--space-2); }
  .profile-guidance-badges { display: flex; gap: var(--space-2); }
  .profile-badge { font-size: var(--text-xs); padding: 2px 8px; border-radius: var(--radius-full); background: var(--surface-3); color: var(--foreground); border: 1px solid var(--border); }
  .profile-badge.priority-high { background: rgba(255, 153, 51, 0.15); border-color: rgba(255, 153, 51, 0.4); }
  .profile-badge.priority-medium { background: rgba(88, 196, 255, 0.15); border-color: rgba(88, 196, 255, 0.4); }
  .profile-badge.priority-low { background: rgba(120, 120, 120, 0.15); border-color: rgba(120, 120, 120, 0.4); }
  .profile-badge.risk-high { background: rgba(255, 77, 79, 0.15); border-color: rgba(255, 77, 79, 0.4); }
  .profile-badge.risk-medium { background: rgba(255, 204, 0, 0.15); border-color: rgba(255, 204, 0, 0.4); }
  .profile-badge.risk-low { background: rgba(52, 199, 89, 0.15); border-color: rgba(52, 199, 89, 0.4); }
  .profile-guidance-desc { font-size: var(--text-sm); color: var(--foreground-muted); }
  .profile-guidance-columns { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-4); }
  .profile-guidance-block { display: flex; flex-direction: column; gap: var(--space-2); }
  .profile-guidance-label { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); }
  .profile-guidance-listing { margin: 0; padding-left: var(--space-4); color: var(--foreground); font-size: var(--text-sm); }
  @media (max-width: 720px) {
    .profile-guidance-columns { grid-template-columns: 1fr; }
    .llm-config-field-row.url-mode-row,
    .llm-config-field-row.worker-url-mode-row {
      grid-template-columns: 1fr;
    }
  }
  .profile-save-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    margin-top: 5px;
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .profile-save-hint {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
  }
  .profile-save-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  /* MCP 服务器列表 */
  .mcp-server-list { display: flex; flex-direction: column; gap: var(--space-2); }

  .mcp-server-item {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }

  .mcp-server-header {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .mcp-server-header:hover { background: var(--surface-hover); }
  .mcp-server-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .mcp-server-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .mcp-server-command { font-size: var(--text-xs); color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-server-actions { display: flex; align-items: center; gap: var(--space-2); flex-shrink: 0; }

  /* MCP 操作按钮样式 */
  .mcp-server-actions .btn-icon {
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .mcp-server-actions .btn-icon:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
    color: var(--foreground);
  }
  .mcp-server-actions .btn-icon--danger:hover {
    background: var(--error-muted);
    border-color: var(--error);
    color: var(--error);
  }
  .mcp-server-actions .btn-icon:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .mcp-server-actions .btn-icon.refreshing :global(svg) {
    animation: spin 1s linear infinite;
  }

  .mcp-server-badge { padding: 2px 8px; font-size: var(--text-xs); border-radius: var(--radius-full); white-space: nowrap; }
  .mcp-server-badge.enabled { background: var(--success-muted); color: var(--success); }
  .mcp-server-badge.disabled { background: var(--surface-3); color: var(--foreground-muted); }
  .mcp-server-health-badge.connected { background: var(--success-muted); color: var(--success); }
  .mcp-server-health-badge.degraded { background: var(--warning-muted); color: var(--warning); }
  .mcp-server-health-badge.disconnected { background: var(--error-muted); color: var(--error); }
  .mcp-expand-icon { transition: transform var(--transition-fast); display: flex; color: var(--foreground-muted); margin-left: var(--space-1); }
  .mcp-expand-icon.expanded { transform: rotate(180deg); }

  .mcp-tools-panel { border-top: 1px solid var(--border); padding: var(--space-3) var(--space-4); background: var(--surface-1); max-height: 280px; overflow-y: auto; }
  .mcp-server-runtime {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-bottom: var(--space-2);
    white-space: nowrap;
    overflow: hidden;
  }
  .mcp-server-runtime-error {
    color: var(--error);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mcp-tools-header { display: flex; justify-content: space-between; align-items: center; font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground-muted); margin-bottom: var(--space-2); }
  .mcp-tools-list { display: flex; flex-direction: column; gap: var(--space-2); }

  /* MCP 工具项样式 */
  .mcp-tool-item {
    position: relative;
    padding: var(--space-2) var(--space-3);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }
  .mcp-tool-item:hover { border-color: var(--primary-muted); background: var(--surface-hover); }
  .mcp-tool-row { display: flex; align-items: center; gap: var(--space-2); }
  .mcp-tool-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-tool-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 2px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }

  /* MCP 工具描述查看按钮 */
  .mcp-tool-desc-btn {
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--foreground-muted);
    transition: all var(--transition-fast);
    flex-shrink: 0;
  }
  .mcp-tool-desc-btn:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
    color: var(--foreground);
  }

  /* MCP 工具描述弹出框 */
  .mcp-tool-desc-pop {
    position: absolute;
    right: var(--space-3);
    top: calc(100% + var(--space-2));
    width: min(360px, 80vw);
    max-height: 160px;
    overflow-y: auto;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--space-3);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: 1.5;
    z-index: 10;
    display: none;
  }
  .mcp-tool-item.show-desc .mcp-tool-desc-pop { display: block; }

  .mcp-tools-empty { font-size: var(--text-sm); color: var(--foreground-muted); text-align: center; padding: var(--space-4); }

  /* Skills 列表 */
  .skills-tool-list { display: flex; flex-direction: column; gap: var(--space-2); }

  .skill-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }
  .skill-item:hover { border-color: var(--primary-muted); }

  .skill-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .skill-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .skill-desc { font-size: var(--text-xs); color: var(--foreground-muted); display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; cursor: help; }

  .skill-actions { display: flex; align-items: center; gap: var(--space-2); flex-shrink: 0; }

  .skill-source-badge { padding: 2px 8px; font-size: var(--text-xs); border-radius: var(--radius-full); background: var(--surface-3); color: var(--foreground-muted); white-space: nowrap; flex-shrink: 0; }
  .skill-source-badge.builtin { background: var(--info-muted); color: var(--info); }
  .skill-source-badge.custom { background: var(--success-muted); color: var(--success); }
  .skill-source-badge.instruction { background: var(--primary-muted); color: var(--primary); }

  /* Skill 删除按钮 */
  .skill-actions .btn-icon--danger {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--foreground-muted);
  }
  .skill-actions .btn-icon--danger:hover {
    background: var(--error-muted);
    border-color: var(--error);
    color: var(--error);
  }

  /* Skill 更新按钮 */
  .skill-actions .btn-icon:not(.btn-icon--danger) {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--foreground-muted);
  }
  .skill-actions .btn-icon:not(.btn-icon--danger):hover:not(:disabled) {
    background: var(--primary-muted);
    border-color: var(--primary);
    color: var(--primary);
  }
  .skill-actions .btn-icon:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  /* 空状态 */
  .empty-state { display: flex; flex-direction: column; align-items: center; justify-content: center; padding: var(--space-8); color: var(--foreground-muted); text-align: center; }
  .empty-state p { margin: var(--space-2) 0 0 0; }
  .empty-state-hint { font-size: var(--text-sm); opacity: 0.7; }
  .empty-state-sm { padding: var(--space-6); text-align: center; color: var(--foreground-muted); font-size: var(--text-sm); }

  /* 加载状态 */
  .loading-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8);
    color: var(--foreground-muted);
    gap: var(--space-3);
  }
  .loading-state :global(svg) {
    animation: spin 1s linear infinite;
  }
  .loading-state span {
    font-size: var(--text-sm);
  }

  /* Modal 对话框 */
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: calc(var(--z-modal) + 10);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

  .modal-dialog {
    width: 480px;
    max-width: 90vw;
    max-height: 80vh;
    background: var(--vscode-editor-background, #1e1e1e);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow:
      0 25px 50px -12px rgba(0, 0, 0, 0.25),
      0 0 0 1px rgba(255, 255, 255, 0.05);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: slideUp var(--duration-normal) var(--ease-out);
  }

  .modal-dialog-lg { width: 600px; max-height: 70vh; }
  .modal-dialog--sm { width: 360px; }
  .modal-overlay--top { z-index: calc(var(--z-modal) + 20); }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
  }
  .modal-header h3 {
    font-size: var(--text-lg);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin: 0;
  }
  .modal-close {
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    color: var(--foreground-muted);
    font-size: 20px;
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .modal-close:hover {
    background: var(--surface-hover);
    border-color: var(--border);
    color: var(--foreground);
  }
  .modal-body {
    flex: 1;
    min-height: 0;
    padding: var(--space-5);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .modal-body::-webkit-scrollbar { width: 6px; }
  .modal-body::-webkit-scrollbar-track { background: transparent; }
  .modal-body::-webkit-scrollbar-thumb { background: var(--scrollbar-thumb); border-radius: 3px; }
  .modal-body::-webkit-scrollbar-thumb:hover { background: var(--scrollbar-thumb-hover); }

  .modal-footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-5);
    border-top: 1px solid var(--border);
    background: var(--surface-2);
  }

  /* 表单字段 */
  .form-field { margin-bottom: var(--space-4); }
  .form-field:last-child { margin-bottom: 0; }
  .form-field label {
    display: block;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }
  .form-field input, .form-field textarea {
    width: 100%;
    padding: var(--space-3);
    font-size: var(--text-sm);
    background: var(--surface-2);
    color: var(--foreground);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    outline: none;
    box-sizing: border-box;
    transition: all var(--transition-fast);
  }
  .form-field input { height: var(--btn-height-lg); padding: 0 var(--space-3); }
  .form-field textarea { font-family: var(--font-mono); resize: vertical; min-height: 120px; }
  .form-field input:hover, .form-field textarea:hover { border-color: var(--primary-muted); }
  .form-field input:focus, .form-field textarea:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px rgba(var(--primary-rgb, 99, 102, 241), 0.15);
  }
  .form-help { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: var(--space-2); line-height: 1.5; }
  .form-error { font-size: var(--text-xs); color: var(--error); margin-top: var(--space-2); line-height: 1.5; }

  /* 仓库管理 */
  .repo-add-section {
    margin-bottom: var(--space-5);
    padding: var(--space-4);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }
  .repo-add-form { display: flex; gap: var(--space-3); align-items: flex-end; }
  .repo-add-form .form-field { flex: 1; margin-bottom: 0; }
  .repo-list-title { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); margin-bottom: var(--space-3); }
  .repo-manage-list { display: flex; flex-direction: column; gap: var(--space-2); max-height: 320px; overflow-y: auto; }
  .repo-manage-list::-webkit-scrollbar { width: 6px; }
  .repo-manage-list::-webkit-scrollbar-track { background: transparent; }
  .repo-manage-list::-webkit-scrollbar-thumb { background: var(--scrollbar-thumb); border-radius: 3px; }
  .repo-manage-list::-webkit-scrollbar-thumb:hover { background: var(--scrollbar-thumb-hover); }
  .repo-item {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }
  .repo-item:hover {
    border-color: var(--primary-muted);
    background: var(--surface-hover);
  }
  .repo-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; overflow: hidden; }
  .repo-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .repo-url { font-size: var(--text-xs); color: var(--foreground-muted); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .repo-meta { font-size: var(--text-xs); color: var(--foreground-muted); }
  .repo-actions { display: flex; gap: var(--space-2); flex-shrink: 0; }

  /* Skill 库 */
  .skill-library-search { margin-bottom: var(--space-4); flex-shrink: 0; }
  .skill-library-search-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--space-3);
    align-items: center;
  }
  .skill-library-import-btn {
    height: var(--btn-height-lg);
    min-width: 112px;
    padding: 0 var(--space-3);
    white-space: nowrap;
  }
  .skill-library-search input {
    width: 100%;
    height: var(--btn-height-lg);
    padding: 0 var(--space-4);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground);
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    outline: none;
    box-sizing: border-box;
    transition: all var(--transition-fast);
  }
  .skill-library-search input::placeholder { color: var(--foreground-muted); }
  .skill-library-search input:hover { border-color: var(--primary-muted); }
  .skill-library-search input:focus {
    border-color: var(--primary);
    box-shadow: 0 0 0 3px rgba(var(--primary-rgb, 99, 102, 241), 0.15);
  }
  .skill-library-warning {
    margin-bottom: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--warning);
    background: color-mix(in srgb, var(--warning) 10%, var(--surface-1));
    border-radius: var(--radius-md);
  }
  .skill-library-warning-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--warning);
    margin-bottom: var(--space-2);
  }
  .skill-library-warning-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .skill-library-warning-item {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    word-break: break-all;
  }
  .skill-library-list { flex: 1; min-height: 0; overflow-y: auto; }
  .skill-library-list::-webkit-scrollbar { width: 6px; }
  .skill-library-list::-webkit-scrollbar-track { background: transparent; }
  .skill-library-list::-webkit-scrollbar-thumb { background: var(--scrollbar-thumb); border-radius: 3px; }
  .skill-library-list::-webkit-scrollbar-thumb:hover { background: var(--scrollbar-thumb-hover); }
  .skill-repo-group { margin-bottom: var(--space-5); }
  .skill-repo-group:last-child { margin-bottom: 0; }
  .skill-repo-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: var(--space-3);
    padding-bottom: var(--space-2);
    border-bottom: 1px solid var(--border);
  }
  .skill-library-item {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin-bottom: var(--space-2);
    transition: all var(--transition-fast);
  }
  .skill-library-item:last-child { margin-bottom: 0; }
  .skill-library-item:hover {
    border-color: var(--primary-muted);
    background: var(--surface-hover);
  }
  .skill-library-icon {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--primary-muted);
    border-radius: var(--radius-md);
    color: var(--primary);
    flex-shrink: 0;
  }
  .skill-library-info { min-width: 0; overflow: hidden; }
  .skill-library-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); }
  .skill-library-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin-top: 4px; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; cursor: help; line-height: 1.5; }
  .skill-library-meta { display: flex; gap: var(--space-3); margin-top: var(--space-2); flex-wrap: wrap; }
  .skill-library-meta-item { font-size: var(--text-xs); color: var(--foreground-muted); }
  .skill-library-actions { flex-shrink: 0; align-self: center; }

  @media (max-width: 720px) {
    .settings-overlay {
      align-items: stretch;
      justify-content: stretch;
      background: var(--overlay-heavy);
    }

    .settings-panel {
      width: 100vw;
      max-width: 100vw;
      height: 100vh;
      max-height: 100vh;
      border-radius: 0;
      border-left: none;
      border-right: none;
      border-top: none;
      box-shadow: none;
    }

    .settings-header,
    .modal-header {
      padding: var(--space-3) var(--space-4);
      flex-wrap: wrap;
      gap: var(--space-2);
    }

    .settings-title {
      font-size: var(--text-lg);
    }

    .settings-tabs {
      overflow-x: auto;
      scrollbar-width: none;
    }

    .settings-tabs::-webkit-scrollbar {
      display: none;
    }

    .settings-tab {
      flex: 0 0 auto;
      min-width: 88px;
      padding: var(--space-3);
    }

    .settings-tab-content,
    .modal-body,
    .modal-footer {
      padding-left: var(--space-4);
      padding-right: var(--space-4);
    }

    .settings-tab-content {
      gap: var(--space-3);
    }

    .settings-section {
      padding: var(--space-4);
    }

    .settings-section-header,
    .settings-section-actions,
    .settings-btn-group,
    .worker-tabs,
    .worker-model-tabs,
    .profile-save-footer,
    .profile-save-actions,
    .stats-action-buttons,
    .modal-footer {
      flex-wrap: wrap;
    }

    .model-connection-item {
      gap: var(--space-2);
    }

    .model-connection-header,
    .mcp-server-header,
    .skill-item,
    .skill-library-item {
      grid-template-columns: 1fr;
    }

    .mcp-server-actions,
    .skill-actions,
    .skill-library-actions {
      align-self: stretch;
    }

    .llm-config-field-row,
    .llm-config-field-row.has-thinking,
    .llm-config-field-row.has-thinking.has-level,
    .llm-config-field-row.url-mode-row,
    .llm-config-field-row.worker-url-mode-row,
    .profile-categories-grid,
    .profile-two-columns,
    .profile-guidance-columns {
      grid-template-columns: 1fr;
    }

    .modal-dialog,
    .modal-dialog-lg,
    .modal-dialog--sm {
      width: 100vw;
      max-width: 100vw;
      min-height: 100vh;
      max-height: 100vh;
      border-radius: 0;
      border-left: none;
      border-right: none;
      border-top: none;
    }
  }
</style>
