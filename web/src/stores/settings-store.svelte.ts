import { getClientKind, vscode } from "../lib/vscode-bridge";
import { onMount, untrack } from "svelte";
import type { StandardMessage } from "../shared/protocol/message-protocol";
import { MessageCategory } from "../shared/protocol/message-protocol";
import { ensureArray } from "../lib/utils";
import { aggregateUsageStatsForDisplay } from "../lib/usage-stats-aggregation";
import { i18n } from "./i18n.svelte";
import { reportIncident, showFeedback } from "../lib/notifications";
import {
  type AgentExecutionModelStatsItem,
  type AgentExecutionStatsItem,
  type AgentExecutionStatsPayload,
  type AgentSettingsBootstrapSnapshot,
  addAgentMcpServer,
  addAgentRepository,
  connectAgentMcpServer,
  deleteAgentMcpServer,
  deleteAgentRepository,
  fetchAgentModelList,
  getAgentExecutionStats,
  getAgentMcpServerTools,
  getAgentSettingsBootstrap,
  installAgentLocalSkill,
  installAgentSkill,
  isWebAgentMode,
  listAgentRegistryAgents,
  listAgentRegistryEngines,
  listAgentRoleTemplates,
  loadAgentToolCatalogDiagnostics,
  loadAgentSkillLibrary,
  refreshAgentMcpTools,
  removeAgentInstalledSkill,
  removeAgentRegistryEngine,
  scanAgentLocalSkillDirectory,
  resetAgentExecutionStats,
  settingsBootstrapMatchesCurrentWorkspace,
  saveAgentAuxiliaryConfig,
  saveAgentImageGenerationConfig,
  saveAgentUserRules,
  saveAgentSafeguardConfig,
  saveAgentWorkerConfig,
  saveAgentOrchestratorConfig,
  removeAgentWorkerConfig,
  testAgentAuxiliaryConnection,
  testAgentImageGenerationConnection,
  testAgentOrchestratorConnection,
  testAgentWorkerConnection,
  updateAgentMcpServer,
  upsertAgentRegistryBinding,
  upsertAgentRegistryEngine,
} from "../web/agent-api";
import type { RoleTemplate } from "../shared/types/role-templates";
import type {
  ModelEngine,
  AgentBinding,
} from "../shared/types/registry-types";
import type { LLMConfig } from "../shared/types/agent-types";
import type { ModelStatus, ModelStatusMap, ModelStatusType } from "../types/message";
import {
  getModelStatus,
  getState,
  messagesState,
  setEnabledAgents,
  setModelStatus,
} from "../stores/messages.svelte";
import type { EnabledAgent } from "../stores/messages.svelte";
import {
  resolveModelListFetchBlockReason,
} from "../shared/model-governance";
import { normalizeToolRuntimeStatus } from "../shared/tool-catalog";
import { normalizeMcpServerDraft } from "../shared/mcp-config";

export type UrlMode = "standard" | "full";
export type ReasoningEffort = "low" | "medium" | "high" | "xhigh";

export interface BaseModelFormConfig {
  baseUrl: string;
  urlMode: UrlMode;
  apiKey: string;
  model: string;
}

export interface InteractiveModelFormConfig extends BaseModelFormConfig {
  reasoningEffort: ReasoningEffort;
}

export interface WorkerModelFormConfig extends InteractiveModelFormConfig {}

type ModelConfigTarget = "orch" | "comp" | "image" | "worker";

type BaseModelConfigPayload = Record<string, unknown> & {
  baseUrl: string;
  urlMode: UrlMode;
  apiKey: string;
  model: string;
};

type InteractiveModelConfigPayload = BaseModelConfigPayload & {
  reasoningEffort: ReasoningEffort;
};

type WorkerModelConfigPayload = InteractiveModelConfigPayload & LLMConfig;

export type SafeguardCategory =
  | "git_history"
  | "git_discard"
  | "package_publish"
  | "bulk_delete"
  | "custom";

export type SafeguardAction =
  | "hard_block"
  | "require_approval_in_restricted"
  | "audit_only";

export interface SafeguardRule {
  pattern: string;
  enabled: boolean;
  category: SafeguardCategory;
  action: SafeguardAction;
}

export interface MCPServer {
  id: string;
  name: string;
  type: "stdio" | "streamable-http";
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  requestTimeoutMs?: number;
  enabled: boolean;
  connected?: boolean;
  health?: "connected" | "degraded" | "disconnected" | "disabled";
  error?: string;
  toolCount?: number;
  reconnectAttempts?: number;
  lastCheckedAt?: number;
  lastReconnectAt?: number;
  lastReconnectSuccessfulAt?: number;
}

export interface SkillItem {
  skillId: string;
  name: string;
  description: string;
  source: "custom" | "instruction";
}

export interface BuiltinToolItem {
  name: string;
  riskLevel: string;
  approvalRequirement: string;
  effectiveApprovalPolicy: string;
  accessProfileBehavior: string;
  accessMode: string;
  policyScope: string;
  inputSensitivePolicy: boolean;
  policySummary: string;
  runtimeInternal: boolean;
  runtimeStatus: string;
  runtimeWarnings: string[];
  schemaStatus: string;
  schemaWarnings: string[];
  enabled: boolean;
}

export interface CapabilityDependencyItem {
  name: string;
  status: string;
  requiredBy: string[];
  roleCount?: number | null;
  spawnableRoleCount?: number | null;
  configuredCount?: number | null;
  enabledCount?: number | null;
  readyCount?: number | null;
  enabledToolCount?: number | null;
  readyToolCount?: number | null;
  toolCount?: number | null;
}

export interface CommandEnvironmentItem {
  name: string;
  available: boolean;
  path: string | null;
}

export interface CommandEnvironmentSnapshot {
  source: string;
  pathAvailable: boolean;
  commands: CommandEnvironmentItem[];
}

function notifySettingsSuccess(message: string): void {
  showFeedback("success", message, { source: "settings-panel" });
}

function notifySettingsInfo(message: string): void {
  showFeedback("info", message, { source: "settings-panel" });
}

function notifySettingsError(actionLabel: string, error: unknown): void {
  console.warn(`[SettingsPanel] ${actionLabel} failed:`, error);
  const message = i18n.t("settings.toast.actionFailed", { action: actionLabel });
  reportIncident(message, {
    scope: "app",
    source: "settings-panel",
    fingerprint: `settings:${actionLabel}`,
  });
}

export interface Repository {
  id: string;
  url: string;
  name?: string;
  skillCount?: number;
  lastUpdated?: string;
}

export interface LibrarySkill {
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
  localSkillId?: string;
}

function createSettingsStore(props: { onClose?: () => void }) {
  const isWebMode = isWebAgentMode();
  type SettingsBootstrapScope = "core" | "full";

  interface Props {
    onClose?: () => void;
  }

  let { onClose }: Props = props;
  const clientKind = getClientKind();

  // 当前激活的 Tab
  let activeTab = $state<"stats" | "model" | "agents" | "tools" | "rules">(
    "model",
  );

  // ============================================
  // Registry 状态（引擎 + 角色）
  // ============================================
  let roleTemplates = $state<RoleTemplate[]>([]);
  let registryEngines = $state<ModelEngine[]>([]);
  let registryAgents = $state<AgentBinding[]>([]);

  // 跟踪前端暂存但未持久化的引擎 ID
  // 用户点"+"添加后暂存于此；保存配置后移除；关闭面板时自动清理残留
  const unsavedEngines = new Set<string>();
  // 记录引擎的当前显示名称（新建时录入，inline 改名时更新）
  // 使用 $state 让改名能驱动 tab/概览区 UI 即时刷新
  let engineDisplayNames = $state<Record<string, string>>({});

  // 使用全局 store 的模型状态（与执行状态共用）
  const appState = getState();
  const modelStatuses = $derived(getModelStatus());

  let isRefreshing = $state(false);
  let totalInputTokens = $state(0);
  let totalOutputTokens = $state(0);
  let totalTokens = $derived(totalInputTokens + totalOutputTokens);
  let userInfo = $state("");
  let showResetConfirm = $state(false);

  // 全局用户规则
  let userRules = $state("");

  onMount(() => {
    const handler = (e: Event) => {
      // 关闭模型下拉（如果点击区域不在 combobox 内也不在 dropdown 内）
      const target = e.target as HTMLElement;
      if (
        !target?.closest?.(".model-combobox") &&
        !target?.closest?.(".model-dropdown")
      ) {
        closeAllModelDropdowns();
      }
    };
    window.addEventListener("click", handler);
    window.addEventListener("resize", handler);
    return () => {
      window.removeEventListener("click", handler);
      window.removeEventListener("resize", handler);
    };
  });

  // Model Tab 状态
  // 统一 tab 选择：'orch' | 'comp' | 'image' | 任意 engineId（worker）
  let modelConfigTab = $state<string>("orch");
  // worker 模式下的当前引擎 ID；系统模型 tab 时为空串。
  // 给保留依赖 worker key 的 API（saveModelConfig/probeModelStatus 等）一个稳定入口。
  const activeWorkerEngineId = $derived(
    modelConfigTab === "orch" || modelConfigTab === "comp" || modelConfigTab === "image"
      ? ""
      : modelConfigTab,
  );

  function createInteractiveConfig(
    overrides: Partial<InteractiveModelFormConfig> = {},
  ): InteractiveModelFormConfig {
    return {
      baseUrl: "",
      urlMode: "standard",
      apiKey: "",
      model: "",
      reasoningEffort: "medium",
      ...overrides,
    };
  }

  function createAuxiliaryConfig(
    overrides: Partial<BaseModelFormConfig> = {},
  ): BaseModelFormConfig {
    return {
      baseUrl: "",
      urlMode: "standard",
      apiKey: "",
      model: "",
      ...overrides,
    };
  }

  function createWorkerConfig(
    overrides: Partial<WorkerModelFormConfig> = {},
  ): WorkerModelFormConfig {
    return {
      ...createInteractiveConfig(),
      ...overrides,
    };
  }

  function normalizeUrlMode(value: unknown): UrlMode {
    return value === "full" ? "full" : "standard";
  }

  function buildBaseModelConfigPayload(
    config: BaseModelFormConfig,
  ): BaseModelConfigPayload {
    return {
      baseUrl: config.baseUrl,
      urlMode: config.urlMode,
      apiKey: config.apiKey,
      model: config.model,
    };
  }

  function buildInteractiveModelConfigPayload(
    config: InteractiveModelFormConfig,
  ): InteractiveModelConfigPayload {
    return {
      ...buildBaseModelConfigPayload(config),
      reasoningEffort: config.reasoningEffort,
    };
  }

  function buildOrchestratorConnectionConfigPayload(
    config: InteractiveModelFormConfig,
  ): BaseModelConfigPayload {
    return {
      baseUrl: config.baseUrl,
      urlMode: config.urlMode,
      apiKey: config.apiKey,
      model: "",
    };
  }

  function buildWorkerModelConfigPayload(
    config: WorkerModelFormConfig,
  ): WorkerModelConfigPayload {
    return {
      ...buildInteractiveModelConfigPayload(config),
    };
  }

  function getBaseUrlPlaceholder(): string {
    return "https://api.openai.com/v1";
  }

  function normalizeBaseUrlForHint(baseUrl: string): string {
    return typeof baseUrl === "string"
      ? baseUrl.trim().replace(/\/+$/, "").toLowerCase()
      : "";
  }

  function shouldRecommendStandardUrlMode(baseUrl: string): boolean {
    const normalized = normalizeBaseUrlForHint(baseUrl);
    if (!normalized) {
      return false;
    }
    // 这些 baseUrl 是“标准兼容形”端点：应作为根地址交给后端按模型名派生协议，
    // 因此推荐使用 standard 模式而非 full（避免把整条 URL 原样透传）。
    return (
      normalized === "https://api.openai.com/v1" ||
      normalized === "https://api.lkeap.cloud.tencent.com/coding/v3"
    );
  }

  // 测试连接状态: 'idle' | 'testing' | 'success' | 'error'
  let testStatus = $state<
    Record<string, "idle" | "testing" | "success" | "error">
  >({
    orch: "idle",
    comp: "idle",
    image: "idle",
  });

  // 模型列表（从 API 获取）
  let modelLists = $state<Record<string, string[]>>({
    orch: [],
    comp: [],
    image: [],
  });
  let modelListSignatures = $state<Record<string, string>>({
    orch: "",
    comp: "",
    image: "",
  });
  // 模型列表获取状态
  let fetchingModels = $state<Record<string, boolean>>({
    orch: false,
    comp: false,
    image: false,
  });
  // 模型下拉是否展开
  let modelDropdownOpen = $state<Record<string, boolean>>({
    orch: false,
    comp: false,
    image: false,
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

  function closeModelDropdown(key: string) {
    if (!modelDropdownOpen[key]) return;
    modelDropdownOpen[key] = false;
    modelDropdownOpen = { ...modelDropdownOpen };
  }

  function buildModelListSignature(config: Partial<BaseModelFormConfig> | undefined): string {
    if (!config) {
      return "";
    }
    return JSON.stringify({
      baseUrl: typeof config.baseUrl === "string" ? config.baseUrl.trim() : "",
      apiKey: typeof config.apiKey === "string" ? config.apiKey.trim() : "",
      urlMode: config.urlMode || "standard",
    });
  }

  function clearModelListState(key: string) {
    const hadModels = Array.isArray(modelLists[key]) && modelLists[key].length > 0;
    const dropdownOpen = modelDropdownOpen[key] === true;
    if (!hadModels && !dropdownOpen) {
      return;
    }
    modelLists[key] = [];
    modelLists = { ...modelLists };
    modelDropdownOpen[key] = false;
    modelDropdownOpen = { ...modelDropdownOpen };
  }

  function syncModelListSignature(key: string, signature: string) {
    const previous = modelListSignatures[key];
    if (previous && previous !== signature) {
      clearModelListState(key);
    }
    if (modelListSignatures[key] !== signature) {
      modelListSignatures[key] = signature;
      modelListSignatures = { ...modelListSignatures };
    }
  }

  $effect(() => {
    syncModelListSignature("orch", buildModelListSignature(orchConfig));
    syncModelListSignature("comp", buildModelListSignature(compConfig));
    syncModelListSignature("image", buildModelListSignature(imageConfig));

    const liveWorkerKeys = new Set<string>(["orch", "comp", "image"]);
    for (const [workerId, config] of Object.entries(workerConfigs)) {
      liveWorkerKeys.add(workerId);
      syncModelListSignature(workerId, buildModelListSignature(config));
    }

    let changed = false;
    for (const key of Object.keys(modelListSignatures)) {
      if (liveWorkerKeys.has(key)) {
        continue;
      }
      delete modelListSignatures[key];
      delete modelLists[key];
      delete modelDropdownOpen[key];
      delete fetchingModels[key];
      changed = true;
    }
    if (changed) {
      modelListSignatures = { ...modelListSignatures };
      modelLists = { ...modelLists };
      modelDropdownOpen = { ...modelDropdownOpen };
      fetchingModels = { ...fetchingModels };
    }
  });

  // 保存配置状态: 'idle' | 'saving' | 'saved' | 'error'
  let saveStatus = $state<
    Record<string, "idle" | "saving" | "saved" | "error">
  >({
    orch: "idle",
    comp: "idle",
    image: "idle",
    mcp: "idle",
  });

  // 用户规则自动保存状态
  let userRulesSaveStatus = $state<"idle" | "saving" | "saved" | "error">("idle");
  let userRulesSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let userRulesStatusTimer: ReturnType<typeof setTimeout> | null = null;
  let persistedUserRules = "";
  let userRulesSaveVersion = 0;

  // Skill 安装/更新状态
  let installingSkills = $state<Set<string>>(new Set());

  // 安全防护
  const SAFEGUARD_CATEGORIES: SafeguardCategory[] = [
    "git_history",
    "git_discard",
    "package_publish",
    "bulk_delete",
    "custom",
  ];
  let safeguardRules = $state<SafeguardRule[]>([]);
  let newCustomRule = $state("");

  // 模型配置表单
  let orchConfig = $state<InteractiveModelFormConfig>(
    createInteractiveConfig(),
  );
  let compConfig = $state<BaseModelFormConfig>(createAuxiliaryConfig());
  let imageConfig = $state<BaseModelFormConfig>(createAuxiliaryConfig());
  let workerConfigs = $state<Record<string, WorkerModelFormConfig>>({});

  function deriveWorkerModelTabs(): string[] {
    const seen = new Set<string>();
    const tabs: string[] = [];
    const append = (engineId: string) => {
      if (
        !engineId ||
        engineId === "orchestrator" ||
        engineId === "auxiliary" ||
        seen.has(engineId)
      ) {
        return;
      }
      seen.add(engineId);
      tabs.push(engineId);
    };

    for (const engine of registryEngines) {
      append(engine.id);
    }
    for (const workerId of Object.keys(workerConfigs)) {
      append(workerId);
    }
    for (const workerId of unsavedEngines) {
      append(workerId);
    }

    return tabs;
  }

  // 动态 worker 列表：只认稳定配置真相源，不再跟随瞬时状态列表抖动
  const workerModelTabs = $derived.by(() => {
    return deriveWorkerModelTabs();
  });

  // 确保 workerConfigs 对每个 worker 都有初始值（副作用必须在 $effect 中执行）
  $effect(() => {
    for (const w of workerModelTabs) {
      if (!workerConfigs[w]) {
        workerConfigs[w] = createWorkerConfig();
      }
    }
  });

  // 当前选中的 worker 被删除时，回落到主模型 tab。
  // 系统 tab（orch/comp）永远存在，不需要 fallback。
  $effect(() => {
    if (
      activeWorkerEngineId &&
      !workerModelTabs.includes(activeWorkerEngineId)
    ) {
      modelConfigTab = "orch";
    }
  });

  // API Key 明文可见状态
  let keyVisible = $state<Record<string, boolean>>({
    orch: false,
    comp: false,
    image: false,
    worker: false,
  });

  // Tools Tab 状态 - MCP 服务器完整结构（与后端 MCPServerConfig 对齐）
  let mcpServers = $state<MCPServer[]>([]);
  let mcpServersHydrated = $state(true);
  let mcpServersLoading = $state(false);
  let mcpExpandedServer = $state<string | null>(null);
  let mcpServerTools = $state<
    Record<
      string,
      Array<{ name: string; description: string; inputSchema?: any }>
    >
  >({});
  let currentEditingMCPServer = $state<MCPServer | null>(null);
  let mcpRefreshingServers = $state<Set<string>>(new Set()); // 正在刷新工具的服务器 ID
  let mcpConnectingServers = $state<Set<string>>(new Set());

  // Skills 完整结构（内置工具已迁移到 ToolManager，不再通过 Skills 配置）
  let skills = $state<SkillItem[]>([]);
  let builtinTools = $state<BuiltinToolItem[]>([]);
  let builtinToolsLoading = $state(false);
  let capabilityDependencies = $state<CapabilityDependencyItem[]>([]);
  let commandEnvironment = $state<CommandEnvironmentSnapshot | null>(null);
  let commandEnvironmentLoading = $state(false);

  // 仓库管理
  let repositories = $state<Repository[]>([]);

  // Skill 库
  let librarySkills = $state<LibrarySkill[]>([]);
  let skillSearchQuery = $state("");

  // 对话框状态
  let showInputDialog = $state(false);
  let inputDialogTitle = $state("");
  let inputDialogValue = $state("");
  let inputDialogCallback = $state<((value: string) => void) | null>(null);

  // MCP 对话框
  let showMCPDialogState = $state(false);
  let mcpDialogIsEdit = $state(false);
  let mcpDialogJson = $state("");
  let mcpDialogError = $state("");

  // 仓库管理对话框
  let showRepoDialogState = $state(false);
  let repoAddUrl = $state("");
  let repoAddLoading = $state(false);
  let repositoriesLoading = $state(false); // 仓库列表加载状态

  // Skill 库对话框
  let showSkillLibraryDialogState = $state(false);
  let skillLibraryLoading = $state(false); // Skill 库加载状态
  let localSkillInstalling = $state(false);
  let skillLibraryFailedRepositoryCount = $state(0);
  let localSkillInstallError = $state("");
  let localSkillScanRootPath = $state("");
  let showLocalSkillFolderPicker = $state(false);

  // 通用确认对话框状态
  let showConfirmDialog = $state(false);
  let confirmDialogTitle = $state("");
  let confirmDialogMessage = $state("");
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
    available: () => i18n.t("settings.status.connected"),
    connected: () => i18n.t("settings.status.connected"),
    configured: () => i18n.t("settings.status.configured"),
    disabled: () => i18n.t("settings.status.disabled"),
    not_configured: () => i18n.t("settings.status.notConfigured"),
    checking: () => i18n.t("settings.status.checking"),
    error: () => i18n.t("settings.status.error"),
    unavailable: () => i18n.t("settings.status.unavailable"),
    invalid_model: () => i18n.t("settings.status.invalidModel"),
    auth_failed: () => i18n.t("settings.status.authFailed"),
    network_error: () => i18n.t("settings.status.networkError"),
    timeout: () => i18n.t("settings.status.timeout"),
    orchestrator: () => i18n.t("settings.status.orchestrator"),
    recorded: () => i18n.t("settings.stats.recordedUsage"),
  };

  function getStatusClass(status: string): string {
    if (
      status === "available" ||
      status === "connected"
    )
      return "success";
    if (status === "configured" || status === "orchestrator") return "warning";
    if (status === "checking") return "checking";
    if (status === "recorded") return "recorded";
    if (status === "disabled" || status === "not_configured") return "disabled";
    if (
      status === "error" ||
      status === "unavailable" ||
      status === "invalid_model" ||
      status === "auth_failed" ||
      status === "network_error" ||
      status === "timeout"
    ) {
      return "error";
    }
    return "error";
  }

  function getStatusText(status: string): string {
    return statusTexts[status]?.() || status;
  }

  function normalizeModelStatusType(status: unknown): ModelStatusType {
    if (typeof status === "string" && statusTexts[status]) {
      return status as ModelStatusType;
    }
    return "error";
  }

  function isModelErrorStatus(status: ModelStatusType): boolean {
    return (
      status === "error" ||
      status === "unavailable" ||
      status === "invalid_model" ||
      status === "auth_failed" ||
      status === "network_error" ||
      status === "timeout"
    );
  }

  function userSafeModelStatusError(status: ModelStatusType): string | undefined {
    return isModelErrorStatus(status) ? getStatusText(status) : undefined;
  }

  function toSafeTokenCount(value: unknown): number {
    if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
      return 0;
    }
    return Math.floor(value);
  }

  function hasUsableModelConfig(
    config: Partial<BaseModelFormConfig> | undefined,
  ): boolean {
    if (!config) {
      return false;
    }
    return Boolean(
      config.baseUrl?.trim() && config.apiKey?.trim() && config.model?.trim(),
    );
  }

  function hasUsableConnectionConfig(
    config: Partial<BaseModelFormConfig> | undefined,
  ): boolean {
    if (!config) {
      return false;
    }
    return Boolean(config.baseUrl?.trim() && config.apiKey?.trim());
  }

  function inferModelErrorStatus(error: unknown): {
    status:
      | "error"
      | "unavailable"
      | "invalid_model"
      | "auth_failed"
      | "network_error"
      | "timeout";
    error: string;
  } {
    const message =
      error instanceof Error ? error.message.trim() : String(error || "连接失败");
    const normalized = message.toLowerCase();
    if (
      normalized.includes("401")
      || normalized.includes("403")
      || normalized.includes("auth")
      || normalized.includes("鉴权")
      || normalized.includes("api key")
      || normalized.includes("unauthorized")
      || normalized.includes("forbidden")
    ) {
      return { status: "auth_failed", error: i18n.t("settings.status.authFailed") };
    }
    if (
      normalized.includes("timeout")
      || normalized.includes("timed out")
      || normalized.includes("超时")
    ) {
      return { status: "timeout", error: i18n.t("settings.status.timeout") };
    }
    if (
      normalized.includes("model")
      || normalized.includes("模型")
      || normalized.includes("not found")
    ) {
      return { status: "invalid_model", error: i18n.t("settings.status.invalidModel") };
    }
    if (
      normalized.includes("econnrefused")
      || normalized.includes("enotfound")
      || normalized.includes("network")
      || normalized.includes("fetch failed")
      || normalized.includes("连接失败")
      || normalized.includes("网络")
    ) {
      return { status: "network_error", error: i18n.t("settings.status.networkError") };
    }
    if (
      normalized.includes("unavailable")
      || normalized.includes("不可用")
      || normalized.includes("5")
    ) {
      return { status: "unavailable", error: i18n.t("settings.status.unavailable") };
    }
    return { status: "error", error: i18n.t("settings.status.error") };
  }

  function sanitizeIncomingModelStatus(
    incoming: any,
    fallbackModel?: string,
  ): ModelStatus | null {
    if (!incoming || typeof incoming !== "object" || incoming.status === "checking") {
      return null;
    }
    const status = normalizeModelStatusType(incoming.status);
    const model =
      typeof incoming.model === "string" && incoming.model.trim()
        ? incoming.model.trim()
        : fallbackModel;
    const next: ModelStatus = {
      status,
      ...(model ? { model } : {}),
    };
    if (typeof incoming.version === "string" && incoming.version.trim()) {
      next.version = incoming.version.trim();
    }
    if (typeof incoming.tokens === "number" && Number.isFinite(incoming.tokens)) {
      next.tokens = incoming.tokens;
    }
    const safeError = userSafeModelStatusError(status);
    if (safeError) {
      next.error = safeError;
    }
    return next;
  }

  function buildConfiguredModelStatuses(
    incoming: Record<string, any> = {},
  ): ModelStatusMap {
    const next: Record<string, any> = {};

    const resolveIncoming = (key: string, fallbackModel?: string) =>
      sanitizeIncomingModelStatus(incoming[key], fallbackModel);

    const orchestratorModel = orchConfig.model?.trim() || undefined;
    const incomingOrchestrator = resolveIncoming("orchestrator", orchestratorModel);
    next.orchestrator = incomingOrchestrator || {
      status: hasUsableConnectionConfig(orchConfig) ? "configured" : "not_configured",
      model: orchestratorModel,
    };

    const auxiliaryModel = compConfig.model?.trim() || undefined;
    const incomingAuxiliary = resolveIncoming("auxiliary", auxiliaryModel);
    next.auxiliary = incomingAuxiliary || {
      status: hasUsableModelConfig(compConfig) ? "configured" : "not_configured",
      model: auxiliaryModel,
    };

    const imageModel = imageConfig.model?.trim() || undefined;
    const incomingImage = resolveIncoming("imageGeneration", imageModel);
    next.imageGeneration = incomingImage || {
      status: hasUsableModelConfig(imageConfig) ? "configured" : "not_configured",
      model: imageModel,
    };

    for (const workerId of deriveWorkerModelTabs()) {
      const config = workerConfigs[workerId];
      const incomingWorker = resolveIncoming(workerId, config?.model?.trim() || undefined);
      if (incomingWorker) {
        next[workerId] = incomingWorker;
        continue;
      }
      next[workerId] = {
        status: hasUsableModelConfig(config) ? "configured" : "not_configured",
        model: config?.model?.trim() || undefined,
      };
    }

    for (const engineId of unsavedEngines) {
      if (!next[engineId]) {
        const config = workerConfigs[engineId];
        next[engineId] = {
          status: hasUsableModelConfig(config) ? "configured" : "not_configured",
          model: config?.model?.trim() || undefined,
        };
      }
    }

    return next as ModelStatusMap;
  }

  function normalizeModelStatuses(
    incoming: Record<string, any> | undefined,
  ): void {
    setModelStatus(buildConfiguredModelStatuses(incoming));
  }

  async function probeModelStatus(
    target: ModelConfigTarget,
    explicitWorkerKey?: string,
  ): Promise<{ key: string; value: ModelStatus }> {
    const workerKey = explicitWorkerKey || activeWorkerEngineId;
    const statusKey =
      target === "worker"
        ? workerKey
        : target === "orch"
          ? "orchestrator"
          : target === "comp"
            ? "auxiliary"
            : "imageGeneration";
    const config:
      | InteractiveModelFormConfig
      | BaseModelFormConfig
      | WorkerModelFormConfig
      | undefined =
      target === "worker"
        ? workerConfigs[workerKey]
        : target === "orch"
          ? orchConfig
          : target === "comp"
            ? compConfig
            : imageConfig;
    const model = config?.model?.trim() || undefined;

    const hasUsableConfig =
      target === "orch"
        ? hasUsableConnectionConfig(config)
        : hasUsableModelConfig(config);
    if (!hasUsableConfig) {
      return {
        key: statusKey,
        value: { status: "not_configured", model },
      };
    }

    try {
      if (target === "worker") {
        await testAgentWorkerConnection(
          workerKey,
          buildWorkerModelConfigPayload(config as WorkerModelFormConfig),
        );
      } else if (target === "orch") {
        await testAgentOrchestratorConnection(
          buildOrchestratorConnectionConfigPayload(config as InteractiveModelFormConfig),
        );
      } else if (target === "comp") {
        await testAgentAuxiliaryConnection(
          buildBaseModelConfigPayload(config as BaseModelFormConfig),
        );
      } else {
        await testAgentImageGenerationConnection(
          buildBaseModelConfigPayload(config as BaseModelFormConfig),
        );
      }
      return {
        key: statusKey,
        value: { status: "connected", model },
      };
    } catch (error) {
      return {
        key: statusKey,
        value: {
          ...inferModelErrorStatus(error),
          model,
        },
      };
    }
  }

  function getWorkerStats(worker: string) {
    const stats = aggregateUsageStatsForDisplay(executionStats, worker);
    if (!stats) {
      return null;
    }
    return {
      worker,
      totalExecutions: stats.totalExecutions,
      assignmentCount: stats.assignmentCount,
      successCount: stats.successCount,
      failureCount: stats.failureCount,
      successRate: stats.successRate,
      totalInputTokens: stats.totalInputTokens,
      totalOutputTokens: stats.totalOutputTokens,
      totalTokens: stats.totalTokens,
      resolvedModel: stats.resolvedModel,
      resolvedModels: stats.resolvedModels,
    };
  }

  function getStatsRoleModelStatus(roleKey: string): ModelStatus | undefined {
    if (roleKey === "orchestrator" || roleKey === "auxiliary" || roleKey === "imageGeneration") {
      return modelStatuses[roleKey];
    }
    const binding = registryAgents.find((item) => item.templateId === roleKey);
    return binding ? modelStatuses[binding.engineId] : undefined;
  }

  function getStatsDisplayKeys(): string[] {
    // 角色视角保持稳定的产品顺序；没有历史用量的内置角色也要展示，避免列表随调用历史漂移。
    const orderedBuiltIns = [
      "orchestrator",
      "auxiliary",
      "imageGeneration",
      "executor",
      "explorer",
      "reviewer",
      "tester",
      "architect",
    ];
    const keys = new Set<string>();
    for (const key of orderedBuiltIns) {
      keys.add(key);
    }

    // 自定义角色只在产生过真实用量后追加，且不改变内置角色的固定顺序。
    for (const item of executionStats) {
      if (item.role === "worker" && item.templateId.trim()) {
        keys.add(item.templateId);
      }
    }
    return Array.from(keys);
  }

  function applyExecutionStatsPayload(payload: AgentExecutionStatsPayload): void {
    executionStats = payload.items.map((item) => ({
      ...item,
      totalTokens: toSafeTokenCount(item.totalTokens),
      netInputTokens: toSafeTokenCount(item.netInputTokens),
      netOutputTokens: toSafeTokenCount(item.netOutputTokens),
    }));
    executionModelStats = payload.models.map((item) => ({
      ...item,
      totals: {
        ...item.totals,
        llmCallCount: toSafeTokenCount(item.totals.llmCallCount),
        assignmentCount: toSafeTokenCount(item.totals.assignmentCount),
        turnCount: toSafeTokenCount(item.totals.turnCount),
        totalTokens: toSafeTokenCount(item.totals.totalTokens),
        netInputTokens: toSafeTokenCount(item.totals.netInputTokens),
        netOutputTokens: toSafeTokenCount(item.totals.netOutputTokens),
        successCount: toSafeTokenCount(item.totals.successCount),
        failureCount: toSafeTokenCount(item.totals.failureCount),
      },
    }));
    totalInputTokens = toSafeTokenCount(payload.totals.netInputTokens);
    totalOutputTokens = toSafeTokenCount(payload.totals.netOutputTokens);
  }

  let executionStatsRequestSeq = 0;

  async function loadExecutionStats(): Promise<void> {
    const requestSeq = ++executionStatsRequestSeq;
    const payload = await getAgentExecutionStats();
    if (requestSeq !== executionStatsRequestSeq) {
      return;
    }
    applyExecutionStatsPayload(payload);
  }

  function applySettingsBootstrapPayload(
    payload: AgentSettingsBootstrapSnapshot,
    options?: {
      allowLocaleHydration?: boolean;
    },
  ) {
    const runtimeLocale = payload.runtimeSettings?.locale;
    if (options?.allowLocaleHydration !== false && (runtimeLocale === 'zh-CN' || runtimeLocale === 'en-US')) {
      i18n.setLocale(runtimeLocale);
    }
    applyUserRulesConfig(payload.userRulesConfig);
    // 主模型 / 辅助模型 / 引擎草稿不在此处派生——统一由监听
    // appState.settingsBootstrapSnapshot 的 $effect 派生（见 createSettingsStore
    // 顶部）。这样无论 snapshot 经由设置面板保存还是主线 picker 切换更新，
    // 草稿都从同一事实源重新派生，不会出现两处各持副本的不同步。
    applyMcpServersPayload(payload.mcpServers);
    mcpServersHydrated = payload.mcpServersHydrated !== false;
    mcpServersLoading = false;
    if (activeTab === "tools" && !mcpServersHydrated) {
      ensureToolsBootstrapHydrated();
    }
    applyBuiltinToolsPayload(payload.builtinTools);
    applyCapabilityDependenciesPayload(payload.capabilityDependencies);
    applySkillsConfig(payload.skillsConfig);
    applyRepositoriesPayload(payload.repositories);
    applySafeguardConfig(payload.safeguardConfig);
    if (
      Array.isArray(payload.roleTemplates)
      && Array.isArray(payload.registryEngines)
      && Array.isArray(payload.registryAgents)
    ) {
      roleTemplates = payload.roleTemplates;
      registryEngines = payload.registryEngines;
      registryAgents = payload.registryAgents;
      const state = getState();
      state.settingsRegistrySnapshot = {
        roleTemplates: payload.roleTemplates,
        registryEngines: payload.registryEngines,
        registryAgents: payload.registryAgents,
      };
      syncEnabledAgentsToStore();
    }
    normalizeModelStatuses(
      payload.workerStatuses as Record<string, { status: string }> | undefined,
    );
  }

  async function fetchCurrentSettingsBootstrap(
    options: { scope?: SettingsBootstrapScope } = {},
  ): Promise<AgentSettingsBootstrapSnapshot | null> {
    const payload = await getAgentSettingsBootstrap(options);
    return settingsBootstrapMatchesCurrentWorkspace(payload) ? payload : null;
  }

  function applyFreshSettingsBootstrapPayload(
    payload: AgentSettingsBootstrapSnapshot,
  ): void {
    appState.settingsBootstrapSnapshot = payload;
    applySettingsBootstrapPayload(payload, { allowLocaleHydration: false });
  }

  async function refreshSettingsBootstrapFromApi(scope: SettingsBootstrapScope = "core"): Promise<void> {
    const hydratesMcpState = scope === "full";
    if (hydratesMcpState) {
      mcpServersLoading = true;
    }
    try {
      const payload = await fetchCurrentSettingsBootstrap({ scope });
      if (!payload) return;
      appState.settingsBootstrapSnapshot = payload;
      applySettingsBootstrapPayload(payload);
    } catch (e) {
      if (hydratesMcpState) {
        mcpServersLoading = false;
      }
      console.error("[SettingsPanel] 加载设置数据失败:", e);
      notifySettingsError(
        i18n.t("settings.toast.action.loadSettingsData"),
        e,
      );
    }
  }

  async function requestSettingsBootstrap(
    force = false,
    scope: SettingsBootstrapScope = "core",
  ) {
    const cachedSnapshot = scope === "core" && !force
      ? (appState.settingsBootstrapSnapshot as AgentSettingsBootstrapSnapshot | null)
      : null;

    if (cachedSnapshot && settingsBootstrapMatchesCurrentWorkspace(cachedSnapshot)) {
      applySettingsBootstrapPayload(cachedSnapshot, { allowLocaleHydration: false });
      void refreshSettingsBootstrapFromApi();
      return;
    }
    if (cachedSnapshot) {
      appState.settingsBootstrapSnapshot = null;
    }

    await refreshSettingsBootstrapFromApi(scope);
  }

  function ensureToolsBootstrapHydrated() {
    if (mcpServersHydrated || mcpServersLoading) {
      return;
    }
    void requestSettingsBootstrap(true, "full");
  }

  // ============================================
  // Registry 数据加载
  // ============================================
  async function loadRegistryData() {
    try {
      const [templates, engines, agents] = await Promise.all([
        listAgentRoleTemplates(),
        listAgentRegistryEngines(),
        listAgentRegistryAgents(),
      ]);
      roleTemplates = templates;
      registryEngines = engines;
      registryAgents = agents;
      const state = getState();
      state.settingsRegistrySnapshot = {
        roleTemplates: templates,
        registryEngines: engines,
        registryAgents: agents,
      };
      // 同步 enabledAgents 到全局 store（供执行状态展示使用）
      syncEnabledAgentsToStore();
    } catch (e) {
      console.error("[SettingsPanel] 加载 Registry 数据失败:", e);
      notifySettingsError(
        i18n.t("settings.toast.action.loadRegistryData"),
        e,
      );
    }
  }

  /**
   * 将当前 registryAgents + roleTemplates 合成 EnabledAgent 列表并写入全局 store
   * 在任何 agent binding 变更后调用。
   * 这里同步的是「可派发代理角色目录」——主线 coordinator 由主模型承接，不进入该列表。
   */
  function syncEnabledAgentsToStore() {
    const templateMap = new Map(roleTemplates.map((t) => [t.templateId, t]));
    const agents: EnabledAgent[] = registryAgents
      .map((a) => {
        const tmpl = templateMap.get(a.templateId);
        return {
          templateId: a.templateId,
          displayName: tmpl?.displayName || a.templateId,
          displayNameKey: tmpl?.i18n?.displayNameKey,
          engineId: a.engineId,
          order: a.order || 0,
          colorToken: tmpl?.defaultUI?.colorToken || "",
          icon: tmpl?.defaultUI?.icon || undefined,
        };
      })
      .sort((x, y) => x.order - y.order);
    setEnabledAgents(agents);
  }

  // ============================================
  // 引擎管理操作
  // ============================================
  // 新增 worker 引擎：直接生成「模型 N」默认名 + engineId，无对话框，
  // 切到新 tab 让用户立即进入编辑/双击改名
  function openAddEngineDialog() {
    let n = workerModelTabs.length + 1;
    let engineId = `model-${n}`;
    // 防止 engineId 冲突（已存在配置或与已用 id 重复）
    while (workerConfigs[engineId] || workerModelTabs.includes(engineId)) {
      n += 1;
      engineId = `model-${n}`;
    }
    const displayName = i18n.t("settings.model.defaultEngineName", { n });
    workerConfigs[engineId] = createWorkerConfig();
    // 注入 modelStatuses 让 workerModelTabs 立即包含新 id
    setModelStatus({
      ...getModelStatus(),
      [engineId]: { status: "not_configured" },
    });
    unsavedEngines.add(engineId);
    engineDisplayNames = { ...engineDisplayNames, [engineId]: displayName };
    modelConfigTab = engineId;
  }

  // Inline 改名：把新名暂存到 engineDisplayNames，下次 saveModelConfig 一并持久化。
  // 主/辅助 tab 名固定，不处理。
  function renameEngineDisplay(engineId: string, newName: string) {
    if (engineId === "orch" || engineId === "comp") return;
    const trimmed = newName.trim();
    if (!trimmed) return;
    const current =
      engineDisplayNames[engineId] || getWorkerDisplayName(engineId);
    if (current === trimmed) return;
    engineDisplayNames = { ...engineDisplayNames, [engineId]: trimmed };
  }

  // 从前端状态中移除指定引擎（workerConfigs + modelStatus + unsavedEngines）
  function purgeEngineFromFrontend(engineId: string) {
    delete workerConfigs[engineId];
    workerConfigs = { ...workerConfigs };
    const { [engineId]: _, ...restStatus } = getModelStatus();
    setModelStatus(restStatus as ModelStatusMap);
    unsavedEngines.delete(engineId);
    const { [engineId]: _name, ...restNames } = engineDisplayNames;
    engineDisplayNames = restNames;
    // 如果删除的是当前选中 tab，回落到主模型
    if (modelConfigTab === engineId) {
      modelConfigTab = "orch";
    }
  }

  async function deleteEngine(engineId: string) {
    // 未保存的引擎：直接清理前端状态，无需调后端
    if (unsavedEngines.has(engineId)) {
      purgeEngineFromFrontend(engineId);
      return;
    }
    // 已持久化的引擎：确认后调后端删除
    const refs = registryAgents.filter((a) => a.engineId === engineId);
    const msg =
      refs.length > 0
        ? i18n.t("settings.model.confirmDeleteEngineWithRefs", {
            count: refs.length,
          })
        : i18n.t("settings.model.confirmDeleteEngine");
    showConfirm(i18n.t("settings.model.deleteEngine"), msg, async () => {
      try {
        await removeAgentRegistryEngine(engineId);
        await removeAgentWorkerConfig(engineId);
        purgeEngineFromFrontend(engineId);
        appState.settingsBootstrapSnapshot = null;
        await requestSettingsBootstrap(true);
        notifySettingsSuccess(i18n.t("settings.toast.engineDeleted"));
      } catch (e) {
        console.error("[SettingsPanel] 删除引擎失败:", e);
        notifySettingsError(i18n.t("settings.toast.action.deleteEngine"), e);
        await requestSettingsBootstrap(true);
      }
    });
  }

  // ============================================
  // 角色管理操作
  // ============================================
  async function updateRoleEngine(templateId: string, engineId: string) {
    const existing = registryAgents.find((a) => a.templateId === templateId);
    if (!existing) return;
    const updated: AgentBinding = {
      ...existing,
      engineId,
      bindingRevision: existing.bindingRevision + 1,
    };
    try {
      const result = await upsertAgentRegistryBinding(updated);
      registryAgents = result;
      syncEnabledAgentsToStore();
      notifySettingsSuccess(i18n.t("settings.toast.roleBindingUpdated"));
    } catch (e) {
      console.error("[SettingsPanel] 更新角色引擎失败:", e);
      notifySettingsError(i18n.t("settings.toast.action.updateRoleBinding"), e);
    }
  }

  // 获取 worker/引擎的展示名称
  // 优先使用 engineDisplayNames（用户输入）→ registryEngines.displayName → 首字母大写
  function getWorkerDisplayName(workerId: string): string {
    const displayName = engineDisplayNames[workerId];
    if (displayName) return displayName;
    const roleTemplate = roleTemplates.find((template) => template.templateId === workerId);
    if (roleTemplate) {
      const key = roleTemplate.i18n?.displayNameKey || `roleTemplate.${roleTemplate.templateId}.displayName`;
      const translated = i18n.t(key);
      if (translated !== key) return translated;
      if (roleTemplate.displayName) return roleTemplate.displayName;
    }
    const engine = registryEngines.find((e) => e.id === workerId);
    if (engine?.displayName) return engine.displayName;
    return workerId.charAt(0).toUpperCase() + workerId.slice(1);
  }

  async function refreshConnections() {
    if (isRefreshing) return;
    isRefreshing = true;
    try {
      const baseline = buildConfiguredModelStatuses();
      const checking: Record<string, any> = {};
      for (const [key, value] of Object.entries(baseline)) {
        checking[key] =
          value.status === "configured"
            ? { ...value, status: "checking" }
            : value;
      }
      setModelStatus(checking as ModelStatusMap);

      const probes = [
        probeModelStatus("orch"),
        probeModelStatus("comp"),
        ...deriveWorkerModelTabs().map((workerId) =>
          probeModelStatus("worker", workerId),
        ),
      ];
      const statsRefresh = loadExecutionStats().catch((error) => {
        console.error("[SettingsPanel] 刷新执行统计失败:", error);
        notifySettingsError(
          i18n.t("settings.toast.action.fetchExecutionStats"),
          error,
        );
      });
      const results = await Promise.all(probes);
      const next = { ...buildConfiguredModelStatuses() } as Record<string, any>;
      for (const result of results) {
        next[result.key] = result.value;
      }
      setModelStatus(next as ModelStatusMap);
      await statsRefresh;
    } finally {
      isRefreshing = false;
    }
  }

  function showResetConfirmDialog() {
    showResetConfirm = true;
  }

  async function confirmResetStats() {
    showResetConfirm = false;
    try {
      await resetAgentExecutionStats();
      await loadExecutionStats();
      notifySettingsSuccess(i18n.t("settings.toast.executionStatsReset"));
    } catch (e) {
      console.error("[SettingsPanel] 重置统计失败:", e);
      notifySettingsError(i18n.t("settings.toast.action.resetExecutionStats"), e);
    }
  }

  function cancelResetStats() {
    showResetConfirm = false;
  }

  function logout() {
    vscode.postMessage({ type: "logout" });
  }

  async function closeSettings() {
    await flushUserRulesSave();
    // 关闭面板前清理所有未保存的引擎（只存在于前端的幽灵引擎）
    for (const engineId of unsavedEngines) {
      purgeEngineFromFrontend(engineId);
    }
    onClose?.();
  }

  async function reloadRoleTemplates(): Promise<void> {
    await loadRegistryData();
  }

  async function saveUserRulesNow(value = userRules, saveVersion = ++userRulesSaveVersion, force = false) {
    const normalized = value;
    if (!force && normalized === persistedUserRules) {
      userRulesSaveStatus = "idle";
      return;
    }
    userRulesSaveStatus = "saving";
    try {
      const result = await saveAgentUserRules({ userRules: normalized });
      if (saveVersion !== userRulesSaveVersion) {
        return;
      }
      userRulesSaveStatus =
        (result as any)?.success !== false ? "saved" : "error";
      if ((result as any)?.success !== false) {
        persistedUserRules = normalized;
      }
    } catch (e) {
      if (saveVersion !== userRulesSaveVersion) {
        return;
      }
      console.error("[SettingsPanel] 保存规则失败:", e);
      userRulesSaveStatus = "error";
      notifySettingsError(i18n.t("settings.toast.action.saveUserRules"), e);
    }
    clearUserRulesSaveStatusLater();
  }

  function scheduleUserRulesSave(value = userRules) {
    const hadUnsettledSave =
      userRulesSaveTimer !== null || userRulesSaveStatus === "saving";
    if (userRulesSaveTimer) {
      clearTimeout(userRulesSaveTimer);
      userRulesSaveTimer = null;
    }
    const saveVersion = ++userRulesSaveVersion;
    const forceSave = hadUnsettledSave && value === persistedUserRules;
    if (!forceSave && value === persistedUserRules) {
      userRulesSaveStatus = "idle";
      return;
    }
    userRulesSaveStatus = "saving";
    userRulesSaveTimer = setTimeout(() => {
      userRulesSaveTimer = null;
      void saveUserRulesNow(value, saveVersion, forceSave);
    }, 700);
  }

  async function flushUserRulesSave(): Promise<void> {
    if (userRulesSaveTimer) {
      clearTimeout(userRulesSaveTimer);
      userRulesSaveTimer = null;
    }
    if (userRules !== persistedUserRules || userRulesSaveStatus === "saving") {
      await saveUserRulesNow(userRules, ++userRulesSaveVersion, userRulesSaveStatus === "saving");
    }
  }

  async function testModelConnection(
    target: ModelConfigTarget,
    explicitWorkerKey?: string,
  ) {
    if (target === "worker" && !explicitWorkerKey && !activeWorkerEngineId) {
      return;
    }

    // 设置测试中状态
    const workerKey = explicitWorkerKey || activeWorkerEngineId;
    const statusKey = target === "worker" ? workerKey : target;
    const modelStatusKey =
      target === "worker"
        ? workerKey
        : target === "orch"
          ? "orchestrator"
          : target === "comp"
            ? "auxiliary"
            : "imageGeneration";
    testStatus[statusKey] = "testing";
    testStatus = { ...testStatus };
    const currentModelStatus = getModelStatus();
    setModelStatus({
      ...buildConfiguredModelStatuses(currentModelStatus),
      [modelStatusKey]: {
        ...(currentModelStatus[modelStatusKey] || {}),
        model:
          (target === "worker"
            ? workerConfigs[workerKey]?.model
            : target === "orch"
              ? orchConfig.model
              : target === "comp"
                ? compConfig.model
                : imageConfig.model)?.trim() || undefined,
        status: "checking",
      },
    });

    try {
      const result = await probeModelStatus(target, explicitWorkerKey);
      setModelStatus({
        ...buildConfiguredModelStatuses(getModelStatus()),
        [result.key]: result.value,
      });
      testStatus[statusKey] =
        result.value.status === "connected" ? "success" : "error";
      notifySettingsSuccess(i18n.t("settings.toast.connectionTestCompleted"));
    } catch (e) {
      console.error("[SettingsPanel] 连接测试失败:", e);
      testStatus[statusKey] = "error";
      const failed = inferModelErrorStatus(e);
      setModelStatus({
        ...buildConfiguredModelStatuses(getModelStatus()),
        [modelStatusKey]: {
          ...failed,
          model:
            (target === "worker"
              ? workerConfigs[workerKey]?.model
              : target === "orch"
                ? orchConfig.model
                : target === "comp"
                  ? compConfig.model
                  : imageConfig.model)?.trim() || undefined,
        },
      });
      notifySettingsError(i18n.t("settings.toast.action.testConnection"), e);
    }
    testStatus = { ...testStatus };
    resetTestStatus(statusKey);
  }

  async function fetchModelList(target: ModelConfigTarget) {
    const key = target === "worker" ? activeWorkerEngineId : target;
    let config: any;
    if (target === "worker") {
      config = workerConfigs[activeWorkerEngineId];
    } else if (target === "orch") {
      config = orchConfig;
    } else if (target === "comp") {
      config = compConfig;
    } else {
      config = imageConfig;
    }

    if (!config) {
      return;
    }

    const blockReason = resolveModelListFetchBlockReason(config);
    if (blockReason) {
      notifySettingsInfo(
        blockReason === "full_url_mode"
          ? i18n.t("config.toast.modelListUnsupportedInFullMode")
          : i18n.t("config.toast.fillBaseUrlFirst"),
      );
      return;
    }

    fetchingModels[key] = true;
    fetchingModels = { ...fetchingModels };

    try {
      const payload =
        target === "worker"
          ? buildWorkerModelConfigPayload(config as WorkerModelFormConfig)
          : target === "orch"
            ? buildOrchestratorConnectionConfigPayload(config as InteractiveModelFormConfig)
            : buildBaseModelConfigPayload(config as BaseModelFormConfig);
      const result = await fetchAgentModelList(payload, key);
      fetchingModels[key] = false;
      fetchingModels = { ...fetchingModels };
      if (result.success && Array.isArray(result.models)) {
        modelLists[key] = result.models;
        modelLists = { ...modelLists };
        if (result.models.length > 0) {
          modelDropdownOpen[key] = true;
          modelDropdownOpen = { ...modelDropdownOpen };
        }
        notifySettingsSuccess(i18n.t("settings.toast.modelListRefreshed"));
      }
    } catch (e) {
      console.error("[SettingsPanel] 获取模型列表失败:", e);
      fetchingModels[key] = false;
      fetchingModels = { ...fetchingModels };
      notifySettingsError(i18n.t("settings.toast.action.fetchModelList"), e);
    }
  }

  function selectModel(target: string, model: string) {
    if (target === "orch") {
      return;
    } else if (target === "comp") {
      compConfig.model = model;
    } else if (target === "image") {
      imageConfig.model = model;
    } else if (workerConfigs[target]) {
      workerConfigs[target].model = model;
    }
    modelDropdownOpen[target] = false;
    modelDropdownOpen = { ...modelDropdownOpen };
  }

  // 重置测试状态（5秒后自动重置为 idle）
  function resetTestStatus(key: string) {
    setTimeout(() => {
      testStatus[key] = "idle";
      testStatus = { ...testStatus };
    }, 5000);
  }

  // 重置保存状态（2秒后自动重置为 idle）
  function resetSaveStatus(key: string) {
    setTimeout(() => {
      saveStatus[key] = "idle";
      saveStatus = { ...saveStatus };
    }, 2000);
  }

  function clearUserRulesSaveStatusLater() {
    if (userRulesStatusTimer) {
      clearTimeout(userRulesStatusTimer);
    }
    userRulesStatusTimer = setTimeout(() => {
      userRulesSaveStatus = "idle";
      userRulesStatusTimer = null;
    }, 2000);
  }

  async function saveModelConfig(target: ModelConfigTarget) {
    const key = target === "worker" ? activeWorkerEngineId : target;

    // 设置保存中状态
    saveStatus[key] = "saving";
    saveStatus = { ...saveStatus };

    try {
      if (target === "worker") {
        const workerKey = key;
        const wc = workerConfigs[workerKey];
        // 新建或待持久化改名：调用 Registry upsert 写入 displayName
        const pendingName = engineDisplayNames[workerKey];
        if (unsavedEngines.has(workerKey) || pendingName) {
          const displayName = pendingName || workerKey;
          const workerPayload = buildWorkerModelConfigPayload(wc);
          await upsertAgentRegistryEngine({
            id: workerKey,
            displayName,
            llm: workerPayload,
          });
        }
        await saveAgentWorkerConfig(workerKey, buildWorkerModelConfigPayload(wc));
        // 保存成功后清理暂存
        unsavedEngines.delete(workerKey);
        const { [workerKey]: _renamed, ...restNames } = engineDisplayNames;
        engineDisplayNames = restNames;
        await loadRegistryData();
      } else if (target === "orch") {
        await saveAgentOrchestratorConfig(
          buildOrchestratorConnectionConfigPayload(orchConfig),
        );
      } else if (target === "comp") {
        await saveAgentAuxiliaryConfig(buildBaseModelConfigPayload(compConfig));
      } else if (target === "image") {
        await saveAgentImageGenerationConfig(buildBaseModelConfigPayload(imageConfig));
      }

      saveStatus[key] = "saved";
      saveStatus = { ...saveStatus };
      notifySettingsSuccess(i18n.t("settings.toast.modelConfigSaved"));
      resetSaveStatus(key);
    } catch (e) {
      console.error("[SettingsPanel] 保存模型配置失败:", e);
      saveStatus[key] = "error";
      notifySettingsError(i18n.t("settings.toast.action.saveModelConfig"), e);
      saveStatus = { ...saveStatus };
      resetSaveStatus(key);
    }

    // 保存后强制拉取最新 bootstrap，避免先套用旧快照导致 tab 闪回。
    await requestSettingsBootstrap(true);
  }

  function confirmInputDialog() {
    if (inputDialogCallback && inputDialogValue.trim()) {
      inputDialogCallback(inputDialogValue.trim());
    }
    showInputDialog = false;
    inputDialogValue = "";
    inputDialogCallback = null;
  }

  function cancelInputDialog() {
    showInputDialog = false;
    inputDialogValue = "";
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
      if (server.type === "streamable-http") {
        cfg.type = "streamable-http";
        if (server.url) cfg.url = server.url;
        if (server.headers && Object.keys(server.headers).length > 0) {
          cfg.headers = server.headers;
        }
      } else {
        cfg.command = server.command || "";
        if (server.args && server.args.length > 0) cfg.args = server.args;
        if (server.env && Object.keys(server.env).length > 0) {
          cfg.env = server.env;
        }
      }
      if (server.requestTimeoutMs) cfg.requestTimeoutMs = server.requestTimeoutMs;
      defaultJSON = JSON.stringify(
        { mcpServers: { [server.name]: cfg } },
        null,
        2,
      );
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
    mcpDialogJson = "";
    mcpDialogError = "";
  }

  async function saveMCPServer() {
    mcpDialogError = "";
    const jsonText = mcpDialogJson.trim();
    if (!jsonText) {
      mcpDialogError = i18n.t("settings.mcp.emptyJson");
      return;
    }

    let parsed: any;
    try {
      parsed = JSON.parse(jsonText);
    } catch {
      mcpDialogError = i18n.t("settings.mcp.jsonError");
      return;
    }

    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      mcpDialogError = i18n.t("settings.mcp.jsonMustBeObject");
      return;
    }

    const servers = parsed.mcpServers;
    if (!servers || typeof servers !== "object" || Array.isArray(servers)) {
      mcpDialogError = i18n.t("settings.mcp.missingMcpServers");
      return;
    }

    const serverNames = Object.keys(servers);
    if (serverNames.length === 0) {
      mcpDialogError = i18n.t("settings.mcp.mcpServersEmpty");
      return;
    }

    if (serverNames.length > 1 && mcpDialogIsEdit) {
      mcpDialogError = i18n.t("settings.mcp.editOnlyOneServer");
      return;
    }

    // 设置保存中状态
    saveStatus.mcp = "saving";
    saveStatus = { ...saveStatus };

    const saveServer = async (
      name: string,
      cfg: any,
      isUpdate: boolean,
    ): Promise<boolean> => {
      if (!cfg || typeof cfg !== "object") {
        mcpDialogError = i18n.t("settings.mcp.invalidServerConfig", { name });
        return false;
      }

      const normalized = normalizeMcpServerDraft(name, cfg);
      if (!normalized.ok) {
        const errorKeys = {
          unsupportedType: "settings.mcp.unsupportedType",
          missingCommand: "settings.mcp.missingCommand",
          argsMustBeArray: "settings.mcp.argsMustBeArray",
          envMustBeObject: "settings.mcp.envMustBeObject",
          missingUrl: "settings.mcp.missingUrl",
          invalidUrl: "settings.mcp.invalidUrl",
          headersMustBeObject: "settings.mcp.headersMustBeObject",
        } as const;
        mcpDialogError = i18n.t(errorKeys[normalized.error], { name });
        return false;
      }

      const serverData = normalized.server;

      if (isUpdate && currentEditingMCPServer) {
        await updateAgentMcpServer(currentEditingMCPServer.id, {
          ...serverData,
          id: currentEditingMCPServer.id,
        });
      } else {
        await addAgentMcpServer(serverData);
      }

      return true;
    };

    let savedCount = 0;
    if (mcpDialogIsEdit && currentEditingMCPServer) {
      const name = serverNames[0];
      if (await saveServer(name, servers[name], true)) savedCount += 1;
    } else {
      for (const name of serverNames) {
        if (await saveServer(name, servers[name], false)) savedCount += 1;
      }
    }

    if (savedCount > 0) {
      // 保存成功后刷新 MCP 列表
      try {
        const payload = await fetchCurrentSettingsBootstrap();
        if (payload) {
          applyMcpServersPayload(payload.mcpServers);
        }
      } catch (_) {
        /* 忽略刷新失败 */
      }
      saveStatus.mcp = "saved";
      saveStatus = { ...saveStatus };
      notifySettingsSuccess(i18n.t("settings.toast.mcpConfigSaved"));
      resetSaveStatus("mcp");
      closeMCPDialog();
    } else {
      // 保存失败
      saveStatus.mcp = "idle";
      saveStatus = { ...saveStatus };
    }
  }

  async function deleteMCPServer(serverId: string) {
    showConfirm(
      i18n.t("settings.tools.deleteMcpServer"),
      i18n.t("settings.tools.deleteMcpServerConfirm"),
      async () => {
        try {
          await deleteAgentMcpServer(serverId);
          const payload = await fetchCurrentSettingsBootstrap();
          if (payload) {
            applyMcpServersPayload(payload.mcpServers);
          }
          notifySettingsSuccess(i18n.t("settings.toast.mcpServerDeleted"));
        } catch (e) {
          console.error("[SettingsPanel] 删除 MCP 服务器失败:", e);
          notifySettingsError(i18n.t("settings.toast.action.deleteMcpServer"), e);
        }
      },
    );
  }

  async function toggleMCPServer(serverId: string, enabled: boolean) {
    const server = mcpServers.find((s) => s.id === serverId);
    if (server) {
      try {
        await updateAgentMcpServer(serverId, { ...server, enabled: !enabled });
        const payload = await fetchCurrentSettingsBootstrap();
        if (payload) {
          applyMcpServersPayload(payload.mcpServers);
        }
        notifySettingsSuccess(i18n.t("settings.toast.mcpServerStatusUpdated"));
      } catch (e) {
        console.error("[SettingsPanel] 切换 MCP 服务器状态失败:", e);
        notifySettingsError(i18n.t("settings.toast.action.toggleMcpServer"), e);
      }
    }
  }

  async function toggleMCPExpand(serverId: string) {
    if (mcpExpandedServer === serverId) {
      mcpExpandedServer = null;
    } else {
      mcpExpandedServer = serverId;
      // 加载工具列表（如果尚未加载）
      if (!mcpServerTools[serverId]) {
        mcpRefreshingServers = new Set([...mcpRefreshingServers, serverId]);
        try {
          const result = await getAgentMcpServerTools(serverId);
          const tools = ensureArray<any>((result as any)?.tools);
          const loaded = applyMcpToolsResult(serverId, result, tools);
          if ((result as any)?.servers) {
            applyMcpServersPayload((result as any).servers);
          }
          if (loaded) {
            notifySettingsSuccess(i18n.t("settings.toast.mcpToolListLoaded"));
          }
        } catch (e) {
          console.error("[SettingsPanel] 获取 MCP 工具列表失败:", e);
          notifySettingsError(
            i18n.t("settings.toast.action.loadMcpToolList"),
            e,
          );
        }
        const newSet = new Set(mcpRefreshingServers);
        newSet.delete(serverId);
        mcpRefreshingServers = newSet;
      }
    }
  }

  async function refreshMCPTools(serverId: string) {
    mcpRefreshingServers = new Set([...mcpRefreshingServers, serverId]);
    try {
      const result = await refreshAgentMcpTools(serverId);
      const tools = ensureArray<any>((result as any)?.tools);
      const refreshed = applyMcpToolsResult(serverId, result, tools);
      if ((result as any)?.servers) {
        applyMcpServersPayload((result as any).servers);
      }
      if (refreshed) {
        notifySettingsSuccess(i18n.t("settings.toast.mcpToolsRefreshed"));
      }
    } catch (e) {
      console.error("[SettingsPanel] 刷新 MCP 工具失败:", e);
      notifySettingsError(i18n.t("settings.toast.action.refreshMcpTools"), e);
    }
    const newSet = new Set(mcpRefreshingServers);
    newSet.delete(serverId);
    mcpRefreshingServers = newSet;
  }

  function applyMcpToolsResult(
    serverId: string,
    result: Record<string, unknown>,
    tools: any[],
  ): boolean {
    const unavailable = result?.connected === false;
    const resultHealth = typeof result?.health === "string" ? result.health : "";
    const connectionFailed =
      unavailable
      && resultHealth !== "disabled"
      && typeof result?.error === "string"
      && result.error.trim().length > 0;
    mcpServerTools = { ...mcpServerTools, [serverId]: unavailable ? [] : tools };
    mcpServers = mcpServers.map((server) => {
      if (server.id !== serverId) return server;
      if (unavailable) {
        const disabled = resultHealth === "disabled" || server.enabled === false;
        return {
          ...server,
          connected: false,
          health: disabled ? "disabled" : "disconnected",
          error: !disabled && connectionFailed ? "connection_issue" : undefined,
          toolCount: 0,
        };
      }
      return {
        ...server,
        connected: true,
        health: "connected",
        error: undefined,
        toolCount: tools.length,
      };
    });
    return !unavailable;
  }

  function getMCPHealthLabel(server: MCPServer): string {
    if (server.enabled === false || server.health === "disabled")
      return i18n.t("settings.tools.disabledLabel");
    if (server.health === "connected")
      return i18n.t("settings.tools.mcpHealthConnected");
    if (server.health === "degraded")
      return i18n.t("settings.tools.mcpHealthDegraded");
    if (!server.error && (server.reconnectAttempts ?? 0) === 0)
      return i18n.t("settings.tools.mcpHealthChecking");
    return i18n.t("settings.tools.mcpHealthDisconnected");
  }

  // ============================================
  // 仓库管理操作函数
  // ============================================

  function openRepoDialog() {
    showRepoDialogState = true;
    repoAddUrl = "";
    repositoriesLoading = false;
  }

  function closeRepoDialog() {
    showRepoDialogState = false;
    repoAddUrl = "";
    repoAddLoading = false;
    repositoriesLoading = false;
  }

  async function addRepository() {
    const url = repoAddUrl.trim();
    if (!url) {
      return;
    }
    repoAddLoading = true;
    try {
      const result = await addAgentRepository(url);
      repoAddLoading = false;
      if ((result as any)?.success !== false) {
        repoAddUrl = "";
        // 刷新仓库列表
        const payload = await fetchCurrentSettingsBootstrap();
        if (payload) {
          applyRepositoriesPayload(payload.repositories);
        }
        notifySettingsSuccess(i18n.t("settings.toast.repositoryAdded"));
      }
    } catch (e) {
      console.error("[SettingsPanel] 添加仓库失败:", e);
      repoAddLoading = false;
      notifySettingsError(i18n.t("settings.toast.action.addRepository"), e);
    }
  }

  async function deleteRepository(repositoryId: string) {
    showConfirm(
      i18n.t("settings.repo.deleteRepo"),
      i18n.t("settings.repo.deleteRepoConfirm"),
      async () => {
        try {
          await deleteAgentRepository(repositoryId);
          const payload = await fetchCurrentSettingsBootstrap();
          if (payload) {
            applyRepositoriesPayload(payload.repositories);
          }
          notifySettingsSuccess(i18n.t("settings.toast.repositoryDeleted"));
        } catch (e) {
          console.error("[SettingsPanel] 删除仓库失败:", e);
          notifySettingsError(i18n.t("settings.toast.action.deleteRepository"), e);
        }
      },
    );
  }

  // ============================================
  // Skill 库操作函数
  // ============================================

  async function openSkillLibraryDialog() {
    showSkillLibraryDialogState = true;
    skillSearchQuery = "";
    skillLibraryLoading = true;
    skillLibraryFailedRepositoryCount = 0;
    localSkillInstallError = "";
    try {
      const payload = await loadAgentSkillLibrary();
      const skillsList = ensureArray<any>(payload.skills);
      librarySkills = skillsList.map((s: any) => ({
        name: s.name,
        fullName: s.fullName || s.name,
        description: s.description || "",
        author: s.author,
        version: s.version,
        category: s.category,
        skillType: s.skillType,
        repositoryId: s.repositoryId,
        repositoryName: s.repositoryName,
        installed: s.installed || false,
        icon: s.icon,
      }));
      skillLibraryFailedRepositoryCount =
        typeof payload.failedRepositoryCount === "number" && Number.isFinite(payload.failedRepositoryCount)
          ? payload.failedRepositoryCount
          : 0;
    } catch (e) {
      console.error("[SettingsPanel] 加载 Skill 库失败:", e);
      notifySettingsError(i18n.t("settings.toast.action.loadSkillLibrary"), e);
    }
    skillLibraryLoading = false;
  }

  function closeSkillLibraryDialog() {
    showSkillLibraryDialogState = false;
    skillSearchQuery = "";
    skillLibraryLoading = false;
    localSkillInstalling = false;
    skillLibraryFailedRepositoryCount = 0;
    localSkillInstallError = "";
    localSkillScanRootPath = "";
  }

  async function installSkill(skillFullName: string) {
    installingSkills.add(skillFullName);
    installingSkills = new Set(installingSkills);

    try {
      const localSkill = librarySkills.find(
        (s) => (s.fullName === skillFullName || s.name === skillFullName) && s.repositoryId === "__local__"
      );

      if (localSkill) {
        const localSkillId = localSkill.localSkillId;
        if (!localSkillScanRootPath || !localSkillId) {
          localSkillInstallError = i18n.t(
            "settings.skillLibrary.localImportFailed",
          );
          installingSkills.delete(skillFullName);
          installingSkills = new Set(installingSkills);
          return;
        }
        const result = await installAgentLocalSkill({
          directoryPath: localSkillScanRootPath,
          skillId: localSkillId,
        });
        installingSkills.delete(skillFullName);
        installingSkills = new Set(installingSkills);
        if ((result as any)?.success !== false) {
          librarySkills = librarySkills.map((s) =>
            s.fullName === skillFullName ? { ...s, installed: true } : s
          );
          const bootstrapPayload = await fetchCurrentSettingsBootstrap();
          if (bootstrapPayload) {
            applyFreshSettingsBootstrapPayload(bootstrapPayload);
          }
          notifySettingsSuccess(i18n.t("settings.toast.localSkillInstalled"));
        }
      } else {
        // 远程 Skill 通过标准安装入口写入 skillId。
        const result = await installAgentSkill(skillFullName);
        installingSkills.delete(skillFullName);
        installingSkills = new Set(installingSkills);
        if ((result as any)?.success !== false) {
          const libPayload = await loadAgentSkillLibrary();
          const skillsList = ensureArray<any>(libPayload.skills);
          skillLibraryFailedRepositoryCount =
            typeof libPayload.failedRepositoryCount === "number" && Number.isFinite(libPayload.failedRepositoryCount)
              ? libPayload.failedRepositoryCount
              : 0;
          // 刷新远程列表时保留当前本地扫描结果。
          const localEntries = librarySkills.filter((s) => s.repositoryId === "__local__");
          librarySkills = [
            ...skillsList.map((s: any) => ({
              name: s.name,
              fullName: s.fullName || s.name,
              description: s.description || "",
              author: s.author,
              version: s.version,
              category: s.category,
              skillType: s.skillType,
              repositoryId: s.repositoryId,
              repositoryName: s.repositoryName,
              installed: s.installed || false,
              icon: s.icon,
            })),
            ...localEntries,
          ];
          const bootstrapPayload = await fetchCurrentSettingsBootstrap();
          if (bootstrapPayload) {
            applyFreshSettingsBootstrapPayload(bootstrapPayload);
          }
          notifySettingsSuccess(i18n.t("settings.toast.skillInstalled"));
        }
      }
    } catch (e) {
      console.error("[SettingsPanel] 安装 Skill 失败:", e);
      installingSkills.delete(skillFullName);
      installingSkills = new Set(installingSkills);
      notifySettingsError(i18n.t("settings.toast.action.installSkill"), e);
    }
    localSkillInstalling = false;
    localSkillInstallError = "";
  }

  async function installLocalSkill() {
    if (localSkillInstalling) {
      return;
    }
    localSkillInstallError = "";
    if (isWebMode) {
      showLocalSkillFolderPicker = true;
    } else {
      localSkillInstalling = true;
      try {
        const result = await installAgentLocalSkill();
        localSkillInstalling = false;
        if ((result as any)?.canceled === true) {
          localSkillInstallError = "";
        } else if ((result as any)?.success !== false) {
          localSkillInstallError = "";
          const bootstrapPayload = await fetchCurrentSettingsBootstrap();
          if (bootstrapPayload) {
            applyFreshSettingsBootstrapPayload(bootstrapPayload);
          }
          showSkillLibraryDialogState = false;
          notifySettingsSuccess(i18n.t("settings.toast.localSkillInstalled"));
        } else {
          localSkillInstallError = i18n.t(
            "settings.skillLibrary.localImportFailed",
          );
        }
      } catch (e) {
        console.error("[SettingsPanel] 本地安装 Skill 失败:", e);
        localSkillInstalling = false;
        localSkillInstallError = i18n.t(
          "settings.skillLibrary.localImportFailed",
        );
        notifySettingsError(
          i18n.t("settings.toast.action.installLocalSkill"),
          e,
        );
      }
    }
  }

  async function handleLocalSkillFolderSelected(
    selection: { pathRef: string; displayPath: string; name: string },
  ): Promise<void> {
    showLocalSkillFolderPicker = false;
    const pathRef = selection.pathRef.trim();
    if (!pathRef) {
      return;
    }
    localSkillInstalling = true;
    localSkillInstallError = "";
    localSkillScanRootPath = "";
    try {
      const result = await scanAgentLocalSkillDirectory(pathRef);
      localSkillInstalling = false;
      const scannedSkills = ensureArray<any>((result as any)?.skills);
      if (scannedSkills.length === 0) {
        localSkillInstallError = i18n.t("settings.skillLibrary.noSkillsFound");
        return;
      }
      localSkillScanRootPath = pathRef;
      const localEntries: LibrarySkill[] = scannedSkills.map((s: any) => ({
        name: s.name || s.skillId || "",
        fullName: s.fullName || s.name || s.skillId || "",
        description: s.description || "",
        author: "",
        version: "",
        category: "",
        skillType: "instruction",
        repositoryId: "__local__",
        repositoryName: i18n.t("settings.skillLibrary.localDirectory"),
        installed: false,
        icon: "",
        localSkillId: s.localSkillId || s.skillId || "",
      }));
      librarySkills = [
        ...librarySkills.filter((s) => s.repositoryId !== "__local__"),
        ...localEntries,
      ];
    } catch (e) {
      console.error("[SettingsPanel] 扫描本地 Skill 目录失败:", e);
      localSkillInstalling = false;
      localSkillInstallError = i18n.t(
        "settings.skillLibrary.localImportFailed",
      );
      notifySettingsError(
        i18n.t("settings.toast.action.scanLocalSkillDirectory"),
        e,
      );
    }
  }

  function cancelLocalSkillFolderPicker(): void {
    showLocalSkillFolderPicker = false;
  }

  // 删除 Skill
  function deleteSkill(skill: SkillItem) {
    const isCustom = skill.source === "custom";
    const titleKey = isCustom
      ? "settings.tools.deleteCustomTool"
      : "settings.tools.deleteInstructionSkill";
    const confirmKey = isCustom
      ? "settings.tools.deleteCustomToolConfirm"
      : "settings.tools.deleteInstructionSkillConfirm";
    const successText = isCustom
      ? i18n.t("settings.toast.customToolDeleted")
      : i18n.t("settings.toast.instructionSkillDeleted");
    const errorText = isCustom
      ? i18n.t("settings.toast.action.deleteCustomTool")
      : i18n.t("settings.toast.action.deleteInstructionSkill");
    showConfirm(
      i18n.t(titleKey),
      i18n.t(confirmKey, { name: skill.name }),
      async () => {
        try {
          await removeAgentInstalledSkill(skill.skillId, skill.source);
          const payload = await fetchCurrentSettingsBootstrap();
          if (payload) {
            applyFreshSettingsBootstrapPayload(payload);
          }
          notifySettingsSuccess(successText);
        } catch (e) {
          console.error(`[SettingsPanel] ${errorText}失败:`, e);
          notifySettingsError(errorText, e);
        }
      },
    );
  }

  // Skill 搜索过滤
  let filteredLibrarySkills = $derived(
    librarySkills.filter((skill) => {
      if (!skillSearchQuery) return true;
      const query = skillSearchQuery.toLowerCase();
      const name = (skill.name || "").toLowerCase();
      const fullName = (skill.fullName || "").toLowerCase();
      const desc = (skill.description || "").toLowerCase();
      return (
        name.includes(query) || fullName.includes(query) || desc.includes(query)
      );
    }),
  );

  // 按仓库分组
  let skillsByRepo = $derived.by(() => {
    const groups: Record<string, { name: string; skills: LibrarySkill[] }> = {};
    for (const skill of filteredLibrarySkills) {
      const repoId = skill.repositoryId || "unknown";
      if (!groups[repoId]) {
        groups[repoId] = {
          name:
            skill.repositoryName || i18n.t("settings.skillLibrary.unknownRepo"),
          skills: [],
        };
      }
      groups[repoId].skills.push(skill);
    }
    return groups;
  });

  // 执行统计数据
  let executionStats = $state<AgentExecutionStatsItem[]>([]);
  let executionModelStats = $state<AgentExecutionModelStatsItem[]>([]);

  function applyUserRulesConfig(config: any): void {
    if (
      userRulesSaveTimer
      || userRulesSaveStatus === "saving"
      || userRules !== persistedUserRules
    ) {
      return;
    }
    userRules = typeof config?.userRules === "string" ? config.userRules : "";
    persistedUserRules = userRules;
    userRulesSaveStatus = "idle";
    if (userRulesSaveTimer) {
      clearTimeout(userRulesSaveTimer);
      userRulesSaveTimer = null;
    }
    if (userRulesStatusTimer) {
      clearTimeout(userRulesStatusTimer);
      userRulesStatusTimer = null;
    }
  }

  function applyWorkerConfigs(configs: Record<string, any> | undefined): void {
    if (!configs) {
      return;
    }
    // 以后端返回的当前协议 workerConfigs 为准重建，保留未保存引擎的前端暂存。
    const next: Record<string, any> = {};
    for (const [worker, config] of Object.entries(configs)) {
      if (config) {
        next[worker] = createWorkerConfig({
          baseUrl: config.baseUrl || "",
          urlMode: normalizeUrlMode(config.urlMode),
          apiKey: config.apiKey || "",
          model: config.model || "",
          reasoningEffort: config.reasoningEffort || "medium",
        });
      }
    }
    // 保留未保存引擎的前端暂存配置
    for (const engineId of unsavedEngines) {
      if (!next[engineId] && workerConfigs[engineId]) {
        next[engineId] = workerConfigs[engineId];
      }
    }
    workerConfigs = next;
  }

  function applyOrchestratorConfig(config: any): void {
    if (!config) {
      return;
    }
    orchConfig = createInteractiveConfig({
      baseUrl: config.baseUrl || "",
      urlMode: normalizeUrlMode(config.urlMode),
      apiKey: config.apiKey || "",
      model: "",
      reasoningEffort: "medium",
    });
  }

  function applyAuxiliaryConfig(config: any): void {
    if (!config) {
      return;
    }
    compConfig = createAuxiliaryConfig({
      baseUrl: config.baseUrl || "",
      urlMode: normalizeUrlMode(config.urlMode),
      apiKey: config.apiKey || "",
      model: config.model || "",
    });
  }

  function applyImageGenerationConfig(config: any): void {
    if (!config) {
      return;
    }
    imageConfig = createAuxiliaryConfig({
      baseUrl: config.baseUrl || "",
      urlMode: normalizeUrlMode(config.urlMode),
      apiKey: config.apiKey || "",
      model: config.model || "",
    });
  }

  function applyMcpServersPayload(serversPayload: unknown): void {
    const servers = ensureArray<any>(serversPayload);
    mcpServers = servers.map((s: any) => {
      const id = typeof s?.id === "string" && s.id.trim() ? s.id.trim() : "";
      if (!id) {
        throw new Error("[SettingsPanel] MCP server 缺少 id");
      }
      const name =
        typeof s?.name === "string" && s.name.trim() ? s.name.trim() : "";
      if (!name) {
        throw new Error(`[SettingsPanel] MCP server ${id} 缺少 name`);
      }
      const enabled = s.enabled !== false;
      const rawHealth = typeof s?.health === "string" ? s.health : "";
      const health: MCPServer["health"] =
        !enabled
          ? "disabled"
          : rawHealth === "connected" ||
              rawHealth === "degraded" ||
              rawHealth === "disconnected"
            ? rawHealth
            : s.connected === true
              ? "connected"
              : "disconnected";
      return {
        id,
        name,
        type: s.type === "streamable-http" || s.type === "http"
          ? "streamable-http"
          : "stdio",
        command: s.command || "",
        args: s.args || [],
        env: s.env || {},
        url: typeof s.url === "string" ? s.url : "",
        headers: s.headers && typeof s.headers === "object" && !Array.isArray(s.headers)
          ? s.headers
          : {},
        requestTimeoutMs: Number.isFinite(s.requestTimeoutMs)
          ? Number(s.requestTimeoutMs)
          : undefined,
        enabled,
        connected: enabled && s.connected === true,
        health,
        error:
          enabled && typeof s.error === "string" && s.error.trim()
            ? "connection_issue"
            : undefined,
        toolCount: Number.isFinite(s.toolCount) ? Number(s.toolCount) : 0,
        reconnectAttempts: Number.isFinite(s.reconnectAttempts)
          ? Number(s.reconnectAttempts)
          : 0,
        lastCheckedAt: Number.isFinite(s.lastCheckedAt)
          ? Number(s.lastCheckedAt)
          : undefined,
        lastReconnectAt: Number.isFinite(s.lastReconnectAt)
          ? Number(s.lastReconnectAt)
          : undefined,
        lastReconnectSuccessfulAt: Number.isFinite(s.lastReconnectSuccessfulAt)
          ? Number(s.lastReconnectSuccessfulAt)
          : undefined,
      };
    });
    void connectEnabledMcpServers();
  }

  async function connectEnabledMcpServers(): Promise<void> {
    const targets = mcpServers.filter((server) =>
      server.enabled !== false
      && server.connected !== true
      && !mcpConnectingServers.has(server.id)
    );
    if (targets.length === 0) return;

    mcpConnectingServers = new Set([
      ...mcpConnectingServers,
      ...targets.map((server) => server.id),
    ]);
    await Promise.all(targets.map(async (server) => {
      try {
        const result = await connectAgentMcpServer(server.id);
        const connected = result.connected === true;
        mcpServers = mcpServers.map((current) => current.id === server.id
          ? {
              ...current,
              connected,
              health: connected ? "connected" : "disconnected",
              error: connected ? undefined : "connection_issue",
              toolCount: Number.isFinite(result.toolCount)
                ? Number(result.toolCount)
                : current.toolCount,
            }
          : current);
      } catch (error) {
        console.error(`[SettingsPanel] 自动连接 MCP 服务器 ${server.id} 失败:`, error);
        mcpServers = mcpServers.map((current) => current.id === server.id
          ? {
              ...current,
              connected: false,
              health: "disconnected",
              error: "connection_issue",
            }
          : current);
      } finally {
        const next = new Set(mcpConnectingServers);
        next.delete(server.id);
        mcpConnectingServers = next;
      }
    }));
  }

  function applyBuiltinToolsPayload(toolsPayload: unknown): void {
    builtinTools = ensureArray<any>(toolsPayload)
      .map((tool) => {
        const name = typeof tool?.name === "string" ? tool.name.trim() : "";
        if (!name) {
          return null;
        }
        const runtimeWarnings: string[] = ensureArray<string>(tool.runtimeWarnings)
          .filter((warning) => warning === "runtime_warning");
        const schemaWarnings: string[] = ensureArray<string>(tool.schemaWarnings)
          .filter((warning) => warning === "schema_warning");
        return {
          name,
          riskLevel: typeof tool.riskLevel === "string" ? tool.riskLevel : "",
          approvalRequirement: typeof tool.approvalRequirement === "string" ? tool.approvalRequirement : "",
          effectiveApprovalPolicy: typeof tool.effectiveApprovalPolicy === "string" ? tool.effectiveApprovalPolicy : "none",
          accessProfileBehavior: typeof tool.accessProfileBehavior === "string" ? tool.accessProfileBehavior : "restricted_allowed",
          accessMode: typeof tool.accessMode === "string" ? tool.accessMode : "read_only",
          policyScope: typeof tool.policyScope === "string" ? tool.policyScope : "fixed",
          inputSensitivePolicy: tool?.inputSensitivePolicy === true,
          policySummary: typeof tool.policySummary === "string" ? tool.policySummary : "",
          runtimeInternal: tool?.runtimeInternal === true,
          runtimeStatus: normalizeToolRuntimeStatus(tool.runtimeStatus),
          runtimeWarnings,
          schemaStatus: typeof tool.schemaStatus === "string" ? tool.schemaStatus : "ok",
          schemaWarnings,
          enabled: tool?.enabled !== false,
        } satisfies BuiltinToolItem;
      })
      .filter((tool): tool is BuiltinToolItem => tool !== null);
  }

  function applyCapabilityDependenciesPayload(dependenciesPayload: unknown): void {
    const dependencies: CapabilityDependencyItem[] = [];
    for (const dependency of ensureArray<any>(dependenciesPayload)) {
      const name = typeof dependency?.name === "string" ? dependency.name.trim() : "";
      if (!name) {
        continue;
      }
      dependencies.push({
        name,
        status: typeof dependency?.status === "string" ? dependency.status : "unknown",
        requiredBy: ensureArray<string>(dependency?.requiredBy)
          .filter((tool): tool is string => typeof tool === "string" && tool.trim().length > 0)
          .map((tool) => tool.trim()),
        roleCount: typeof dependency?.roleCount === "number" && Number.isFinite(dependency.roleCount)
          ? dependency.roleCount
          : null,
        spawnableRoleCount: typeof dependency?.spawnableRoleCount === "number" && Number.isFinite(dependency.spawnableRoleCount)
          ? dependency.spawnableRoleCount
          : null,
        configuredCount: typeof dependency?.configuredCount === "number" && Number.isFinite(dependency.configuredCount)
          ? dependency.configuredCount
          : null,
        enabledCount: typeof dependency?.enabledCount === "number" && Number.isFinite(dependency.enabledCount)
          ? dependency.enabledCount
          : null,
        readyCount: typeof dependency?.readyCount === "number" && Number.isFinite(dependency.readyCount)
          ? dependency.readyCount
          : null,
        enabledToolCount: typeof dependency?.enabledToolCount === "number" && Number.isFinite(dependency.enabledToolCount)
          ? dependency.enabledToolCount
          : null,
        readyToolCount: typeof dependency?.readyToolCount === "number" && Number.isFinite(dependency.readyToolCount)
          ? dependency.readyToolCount
          : null,
        toolCount: typeof dependency?.toolCount === "number" && Number.isFinite(dependency.toolCount)
          ? dependency.toolCount
          : null,
      });
    }
    capabilityDependencies = dependencies;
  }

  async function refreshBuiltinToolCatalog(): Promise<void> {
    if (builtinToolsLoading) {
      return;
    }
    builtinToolsLoading = true;
    try {
      const snapshot = await loadAgentToolCatalogDiagnostics();
      applyBuiltinToolsPayload(snapshot.builtinTools);
      applyCapabilityDependenciesPayload(snapshot.capabilityDependencies);
      notifySettingsSuccess(i18n.t("settings.toast.toolCatalogRefreshed"));
    } catch (e) {
      console.error("[SettingsPanel] 刷新内置工具状态失败:", e);
      notifySettingsError(i18n.t("settings.toast.action.refreshToolCatalog"), e);
    } finally {
      builtinToolsLoading = false;
    }
  }

  async function refreshCommandEnvironment(): Promise<void> {
    if (commandEnvironmentLoading) {
      return;
    }
    commandEnvironmentLoading = true;
    try {
      const snapshot = await loadAgentToolCatalogDiagnostics({ refreshEnvironment: true });
      commandEnvironment = snapshot.commandEnvironment;
      notifySettingsSuccess(i18n.t("settings.toast.commandEnvironmentRefreshed"));
    } catch (error) {
      console.error("[SettingsPanel] 刷新命令环境失败:", error);
      notifySettingsError(i18n.t("settings.toast.action.refreshCommandEnvironment"), error);
    } finally {
      commandEnvironmentLoading = false;
    }
  }

  async function hydrateCommandEnvironment(): Promise<void> {
    if (commandEnvironment || commandEnvironmentLoading) {
      return;
    }
    commandEnvironmentLoading = true;
    try {
      const snapshot = await loadAgentToolCatalogDiagnostics();
      commandEnvironment = snapshot.commandEnvironment;
    } catch (error) {
      console.warn("[SettingsPanel] 加载命令环境诊断失败:", error);
    } finally {
      commandEnvironmentLoading = false;
    }
  }

  function applySkillsConfig(config: any): void {
    const skillList: SkillItem[] = [];
    if (Array.isArray(config?.customTools)) {
      for (const tool of config.customTools) {
        const name = typeof tool?.name === "string" && tool.name.trim()
          ? tool.name.trim()
          : "";
        if (!name) continue;
        skillList.push({
          name,
          skillId: name,
          description: typeof tool.description === "string" ? tool.description : "",
          source: "custom",
        });
      }
    }
    if (Array.isArray(config?.instructionSkills)) {
      for (const skill of config.instructionSkills) {
        const skillId = typeof skill?.skillId === "string" && skill.skillId.trim()
          ? skill.skillId.trim()
          : "";
        if (!skillId) continue;
        const name = typeof skill.name === "string" && skill.name.trim()
          ? skill.name.trim()
          : skillId;
        skillList.push({
          skillId,
          name,
          description: typeof skill.description === "string" ? skill.description : "",
          source: "instruction",
        });
      }
    }
    skills = skillList;
  }

  function applyRepositoriesPayload(repositoriesPayload: unknown): void {
    const repoList = ensureArray<any>(repositoriesPayload);
    repositories = repoList.map((repository: any) => ({
      id: repository.repositoryId || repository.id,
      url: repository.url,
      name: repository.name || repository.url,
      skillCount: repository.skillCount || 0,
      lastUpdated: repository.lastUpdated,
    }));
    repoAddLoading = false;
    repositoriesLoading = false;
  }

  function applySafeguardConfig(config: any): void {
    safeguardRules = Array.isArray(config?.rules)
      ? config.rules.map((r: any) => ({
          pattern: String(r.pattern || ""),
          enabled: r.enabled !== false,
          category: r.category || "custom",
          action: normalizeSafeguardAction(r.action),
        }))
      : [];
  }

  function normalizeSafeguardAction(action: unknown): SafeguardAction {
    if (
      action === "hard_block" ||
      action === "require_approval_in_restricted" ||
      action === "audit_only"
    ) {
      return action;
    }
    return "require_approval_in_restricted";
  }

  async function saveSafeguardRules(): Promise<void> {
    try {
      await saveAgentSafeguardConfig({
        rules: safeguardRules.map((r) => ({ ...r })),
      });
      notifySettingsSuccess(i18n.t("settings.toast.safeguardRulesSaved"));
    } catch (e) {
      console.error("[SettingsPanel] 保存安全规则失败:", e);
      notifySettingsError(
        i18n.t("settings.toast.action.saveSafeguardRules"),
        e,
      );
    }
  }

  function toggleSafeguardRule(index: number): void {
    safeguardRules[index] = {
      ...safeguardRules[index],
      enabled: !safeguardRules[index].enabled,
    };
    safeguardRules = [...safeguardRules];
    saveSafeguardRules();
  }

  function updateSafeguardRuleAction(index: number, action: string): void {
    const rule = safeguardRules[index];
    if (!rule) return;
    const normalizedAction = normalizeSafeguardAction(action);
    if (rule.action === normalizedAction) return;
    safeguardRules[index] = {
      ...rule,
      action: normalizedAction,
    };
    safeguardRules = [...safeguardRules];
    saveSafeguardRules();
  }

  function removeCustomRule(index: number): void {
    safeguardRules = safeguardRules.filter((_, i) => i !== index);
    saveSafeguardRules();
  }

  function addCustomRule(): void {
    const pattern = newCustomRule.trim();
    if (!pattern) return;
    if (safeguardRules.some((r) => r.pattern === pattern)) return;
    safeguardRules = [
      ...safeguardRules,
      {
        pattern,
        enabled: true,
        category: "custom" as SafeguardCategory,
        action: "require_approval_in_restricted",
      },
    ];
    newCustomRule = "";
    saveSafeguardRules();
  }

  function getRulesForCategory(
    category: SafeguardCategory,
  ): { rule: SafeguardRule; index: number }[] {
    return safeguardRules
      .map((rule, index) => ({ rule, index }))
      .filter(({ rule }) => rule.category === category);
  }

  // 主模型 / 辅助模型 / 引擎草稿统一派生自单一事实源
  // messagesState.settingsBootstrapSnapshot —— 主线右下角 picker 与设置面板
  // 共享同一份 snapshot。picker 切换模型会替换 snapshot 引用，此 effect 据此
  // 重新派生草稿，确保「设置面板已打开时 picker 切换」也实时同步，消除两处
  // 各持一份本地副本导致的不同步。apply 内部读写 workerConfigs/unsavedEngines，
  // 用 untrack 包裹避免把它们登记为依赖造成自循环——本 effect 只应由 snapshot
  // 引用变化驱动。
  $effect(() => {
    const snapshot = messagesState.settingsBootstrapSnapshot as
      | AgentSettingsBootstrapSnapshot
      | null;
    if (!snapshot) {
      return;
    }
    untrack(() => {
      applyWorkerConfigs(
        snapshot.workerConfigs as Record<string, any> | undefined,
      );
      applyOrchestratorConfig(snapshot.orchestratorConfig);
      applyAuxiliaryConfig(snapshot.auxiliaryConfig);
      applyImageGenerationConfig(snapshot.imageGenerationConfig);
    });
  });

  // 监听来自扩展的实时状态推送（仅保留 SSE 实时推送类型）
  onMount(() => {
    const unsubscribe = vscode.onMessage((msg) => {
      if (msg.type !== "unifiedMessage") return;
      const standard = msg.message as StandardMessage;
      if (
        !standard ||
        standard.category !== MessageCategory.DATA ||
        !standard.data
      )
        return;
      const { dataType, payload } = standard.data as {
        dataType: string;
        payload: any;
      };

      if (dataType === "settingsBootstrapLoaded") {
        const snapshot = payload as AgentSettingsBootstrapSnapshot;
        if (settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
          appState.settingsBootstrapSnapshot = snapshot;
          applySettingsBootstrapPayload(snapshot, { allowLocaleHydration: false });
        }
        return;
      }

      // 执行统计更新（SSE 推送）
      if (dataType === "executionStatsUpdate") {
        void loadExecutionStats().catch((error) => {
          console.error("[SettingsPanel] 刷新执行统计失败:", error);
        });
      }
    });

    // 初始化请求数据
    requestSettingsBootstrap();
    loadExecutionStats()
      .catch((e) => {
        console.error("[SettingsPanel] 获取执行统计失败:", e);
        notifySettingsError(
          i18n.t("settings.toast.action.fetchExecutionStats"),
          e,
        );
      });

    return () => unsubscribe();
  });

  return {
    get clientKind() {
      return clientKind;
    },
    get activeTab() {
      return activeTab;
    },
    set activeTab(v) {
      activeTab = v;
      if (v === "stats") {
        void loadExecutionStats().catch((e) => {
          console.error("[SettingsPanel] 刷新执行统计失败:", e);
          notifySettingsError(
            i18n.t("settings.toast.action.fetchExecutionStats"),
            e,
          );
        });
      }
      if (v === "tools") {
        ensureToolsBootstrapHydrated();
        void hydrateCommandEnvironment();
      }
    },
    get roleTemplates() {
      return roleTemplates;
    },
    get registryEngines() {
      return registryEngines;
    },
    get registryAgents() {
      return registryAgents;
    },
    get modelStatuses() {
      return modelStatuses;
    },
    get isRefreshing() {
      return isRefreshing;
    },
    get totalInputTokens() {
      return totalInputTokens;
    },
    get totalOutputTokens() {
      return totalOutputTokens;
    },
    get totalTokens() {
      return totalTokens;
    },
    get statsDisplayKeys() {
      return getStatsDisplayKeys();
    },
    get executionModelStats() {
      return executionModelStats;
    },
    get executionStats() {
      return executionStats;
    },
    get userInfo() {
      return userInfo;
    },
    get showResetConfirm() {
      return showResetConfirm;
    },
    get userRules() {
      return userRules;
    },
    set userRules(v) {
      userRules = v;
      scheduleUserRulesSave(v);
    },
    get modelConfigTab() {
      return modelConfigTab;
    },
    set modelConfigTab(v) {
      modelConfigTab = v;
    },
    get testStatus() {
      return testStatus;
    },
    get modelLists() {
      return modelLists;
    },
    get fetchingModels() {
      return fetchingModels;
    },
    get modelDropdownOpen() {
      return modelDropdownOpen;
    },
    get dropdownPosition() {
      return dropdownPosition;
    },
    get saveStatus() {
      return saveStatus;
    },
    get userRulesSaveStatus() {
      return userRulesSaveStatus;
    },
    get installingSkills() {
      return installingSkills;
    },
    get SAFEGUARD_CATEGORIES() {
      return SAFEGUARD_CATEGORIES;
    },
    get newCustomRule() {
      return newCustomRule;
    },
    set newCustomRule(v) {
      newCustomRule = v;
    },
    get orchConfig() {
      return orchConfig;
    },
    set orchConfig(v) {
      orchConfig = v;
    },
    get compConfig() {
      return compConfig;
    },
    set compConfig(v) {
      compConfig = v;
    },
    get imageConfig() {
      return imageConfig;
    },
    set imageConfig(v) {
      imageConfig = v;
    },
    get workerConfigs() {
      return workerConfigs;
    },
    set workerConfigs(v) {
      workerConfigs = v;
    },
    get workerModelTabs() {
      return workerModelTabs;
    },
    get keyVisible() {
      return keyVisible;
    },
    set keyVisible(v) {
      keyVisible = v;
    },
    get mcpServers() {
      return mcpServers;
    },
    get mcpServersHydrated() {
      return mcpServersHydrated;
    },
    get mcpServersLoading() {
      return mcpServersLoading;
    },
    get mcpExpandedServer() {
      return mcpExpandedServer;
    },
    get mcpServerTools() {
      return mcpServerTools;
    },
    get mcpRefreshingServers() {
      return mcpRefreshingServers;
    },
    get skills() {
      return skills;
    },
    get builtinTools() {
      return builtinTools;
    },
    get builtinToolsLoading() {
      return builtinToolsLoading;
    },
    get capabilityDependencies() {
      return capabilityDependencies;
    },
    get commandEnvironment() {
      return commandEnvironment;
    },
    get commandEnvironmentLoading() {
      return commandEnvironmentLoading;
    },
    get repositories() {
      return repositories;
    },
    get skillSearchQuery() {
      return skillSearchQuery;
    },
    set skillSearchQuery(v) {
      skillSearchQuery = v;
    },
    get showInputDialog() {
      return showInputDialog;
    },
    get inputDialogTitle() {
      return inputDialogTitle;
    },
    get inputDialogValue() {
      return inputDialogValue;
    },
    set inputDialogValue(v) {
      inputDialogValue = v;
    },
    get showMCPDialogState() {
      return showMCPDialogState;
    },
    get mcpDialogIsEdit() {
      return mcpDialogIsEdit;
    },
    get mcpDialogJson() {
      return mcpDialogJson;
    },
    set mcpDialogJson(v) {
      mcpDialogJson = v;
    },
    get mcpDialogError() {
      return mcpDialogError;
    },
    set mcpDialogError(v) {
      mcpDialogError = v;
    },
    get showRepoDialogState() {
      return showRepoDialogState;
    },
    get repoAddUrl() {
      return repoAddUrl;
    },
    set repoAddUrl(v) {
      repoAddUrl = v;
    },
    get repoAddLoading() {
      return repoAddLoading;
    },
    get repositoriesLoading() {
      return repositoriesLoading;
    },
    get showSkillLibraryDialogState() {
      return showSkillLibraryDialogState;
    },
    get skillLibraryLoading() {
      return skillLibraryLoading;
    },
    get localSkillInstalling() {
      return localSkillInstalling;
    },
    get skillLibraryFailedRepositoryCount() {
      return skillLibraryFailedRepositoryCount;
    },
    get localSkillInstallError() {
      return localSkillInstallError;
    },
    get showLocalSkillFolderPicker() {
      return showLocalSkillFolderPicker;
    },
    get showConfirmDialog() {
      return showConfirmDialog;
    },
    get confirmDialogTitle() {
      return confirmDialogTitle;
    },
    get confirmDialogMessage() {
      return confirmDialogMessage;
    },
    get statusTexts() {
      return statusTexts;
    },
    get filteredLibrarySkills() {
      return filteredLibrarySkills;
    },
    get skillsByRepo() {
      return skillsByRepo;
    },
    getBaseUrlPlaceholder,
    shouldRecommendStandardUrlMode,
    openModelDropdown,
    closeAllModelDropdowns,
    closeModelDropdown,
    handleConfirmYes,
    handleConfirmNo,
    getStatusClass,
    getStatusText,
    getWorkerStats,
    getStatsRoleModelStatus,
    openAddEngineDialog,
    deleteEngine,
    renameEngineDisplay,
    updateRoleEngine,
    getWorkerDisplayName,
    refreshConnections,
    showResetConfirmDialog,
    confirmResetStats,
    cancelResetStats,
    logout,
    closeSettings,
    reloadRoleTemplates,
    testModelConnection,
    fetchModelList,
    selectModel,
    saveModelConfig,
    confirmInputDialog,
    cancelInputDialog,
    openMCPDialog,
    closeMCPDialog,
    saveMCPServer,
    deleteMCPServer,
    toggleMCPServer,
    toggleMCPExpand,
    refreshMCPTools,
    refreshBuiltinToolCatalog,
    refreshCommandEnvironment,
    getMCPHealthLabel,
    openRepoDialog,
    closeRepoDialog,
    addRepository,
    deleteRepository,
    openSkillLibraryDialog,
    closeSkillLibraryDialog,
    installSkill,
    installLocalSkill,
    handleLocalSkillFolderSelected,
    cancelLocalSkillFolderPicker,
    deleteSkill,
    toggleSafeguardRule,
    updateSafeguardRuleAction,
    removeCustomRule,
    addCustomRule,
    getRulesForCategory,
  };
}

export type SettingsStore = ReturnType<typeof createSettingsStore>;

export function useSettingsStore(
  props: { onClose?: () => void },
): SettingsStore {
  return createSettingsStore(props);
}
