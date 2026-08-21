import { getDefaultAgentBaseUrl } from '../shared/agent-shared-config';
import { getTransport } from '../shared/transport';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
import type { RoleTemplate } from '../shared/types/role-templates';
import type {
  SettingsBootstrapPayload,
  SettingsBuiltinTool,
  SettingsCapabilityDependency,
  SettingsRuntimeSnapshot,
  VisionBuiltinTextModelRule,
} from '../shared/settings-bootstrap';
import type {
  IncidentNotificationItemDto,
  NotificationCenterSnapshotDto,
  NotificationsResponseDto,
  SessionInterruptResponseDto,
  SessionTurnQueueResponseDto,
  FetchModelsResponseDto,
  EnhancePromptRequestDto,
  GenerateSessionSuggestionsRequestDto,
  SessionSuggestionsResponseDto,
  SkillsLibraryResponseDto,
  MessagesResponseDto,
} from '../shared/rust-backend-types';
import type { CanonicalTurn, CanonicalTurnItem } from '../shared/protocol/canonical-turn';
import { i18n } from '../stores/i18n.svelte';
import {
  resolveAgentBindingContext,
  type AgentBindingContext,
  type AgentBindingOverride,
  type WorkspaceAgentBindingOverride,
} from './agent-binding-context';
import { normalizeToolRuntimeStatus } from '../shared/tool-catalog';
import {
  type AccessProfile,
  normalizeAccessProfile,
  readStoredAccessProfile,
} from '../shared/access-profile';


export const RUNTIME_BASE_URL_STORAGE_KEY = 'magi-runtime-base-url';
const LEGACY_AGENT_BASE_URL_STORAGE_KEY = 'magi-agent-base-url';
const AGENT_PROBE_TIMEOUT_MS = 1500;
let cachedWorkspaceSummaries: AgentWorkspaceSummary[] = [];

export const RUNTIME_CONNECTION_EVENT = 'magi-runtime-connection';

function clearLegacyAgentRuntimeStorage(): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.localStorage.removeItem(LEGACY_AGENT_BASE_URL_STORAGE_KEY);
  } catch (error) {
    console.warn(`[agent-api] 清理旧运行态 localStorage 失败(${LEGACY_AGENT_BASE_URL_STORAGE_KEY})`, error);
  }
}

clearLegacyAgentRuntimeStorage();

export interface AgentConnectionEventDetail {
  status: 'connected' | 'recovering';
  reason?: string;
  error?: string;
  baseUrl?: string;
  recovered?: boolean;
}

export interface AgentWorkspaceSummary {
  workspaceId: string;
  name: string;
  rootPath: string;
  rootPathRef?: string;
  isActive: boolean;
}

export interface AgentSessionSummary {
  id: string;
  workspaceId?: string;
  name?: string;
  createdAt: number;
  updatedAt: number;
  messageCount?: number;
  isRunning?: boolean;
  runningTaskCount?: number;
  hasUnreadCompletion?: boolean;
  preview?: string;
}

export interface AgentWorkspaceSessionsSnapshot {
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  workspace: AgentWorkspaceSummary;
  sessions: AgentSessionSummary[];
}

export interface AgentWorkspacePickResult {
  cancelled: boolean;
  rootPath: string | null;
  name: string | null;
}

interface RawAgentWorkspaceSummary {
  workspaceId?: string;
  rootPath?: string;
  rootPathRef?: string | null;
  name?: string | null;
  isActive?: boolean;
}

interface RawAgentSessionSummary {
  id?: string;
  sessionId?: string;
  workspaceId?: string;
  name?: string | null;
  title?: string | null;
  createdAt?: number;
  updatedAt?: number;
  messageCount?: number;
  isRunning?: boolean;
  runningTaskCount?: number;
  hasUnreadCompletion?: boolean;
  preview?: string | null;
}

export type AgentRuntimeSettings = SettingsRuntimeSnapshot;
export type AgentSettingsBootstrapSnapshot = SettingsBootstrapPayload;

export interface AgentToolCatalogDiagnosticsSnapshot {
  builtinTools: SettingsBuiltinTool[];
  capabilityDependencies: SettingsCapabilityDependency[];
  commandEnvironment: {
    source: string;
    pathAvailable: boolean;
    commands: Array<{ name: string; available: boolean; path: string | null }>;
  };
}

function normalizeCommandEnvironment(value: unknown): AgentToolCatalogDiagnosticsSnapshot['commandEnvironment'] {
  const record = value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
  const commands = Array.isArray(record.commands)
    ? record.commands.flatMap((entry) => {
        if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return [];
        const item = entry as Record<string, unknown>;
        const name = typeof item.name === 'string' ? item.name.trim() : '';
        if (!name) return [];
        return [{
          name,
          available: item.available === true,
          path: typeof item.path === 'string' && item.path.trim() ? item.path : null,
        }];
      })
    : [];
  return {
    source: typeof record.source === 'string' && record.source.trim() ? record.source : 'unknown',
    pathAvailable: record.pathAvailable === true,
    commands,
  };
}

function normalizeSettingsSectionConfig(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  return value as Record<string, unknown>;
}

function normalizeMcpServerConfig(server: Record<string, unknown>): Record<string, unknown> {
  const serverId = typeof server.id === 'string' && server.id.trim()
    ? server.id.trim()
    : (typeof server.serverId === 'string' ? server.serverId.trim() : '');
  return {
    ...server,
    ...(serverId ? { id: serverId, serverId } : {}),
  };
}

function normalizeNullableNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function normalizeBindingString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeRequiredBy(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
    .map((item) => item.trim());
}

function normalizeWarningMarkers(value: unknown, marker: string): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item) => typeof item === 'string' && item.trim().length > 0)
    .map(() => marker);
}

function normalizeBuiltinTools(value: unknown): SettingsBuiltinTool[] {
  if (!Array.isArray(value)) return [];
  const tools: SettingsBuiltinTool[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    const name = typeof record.name === 'string' ? record.name.trim() : '';
    if (!name) continue;
    tools.push({
      name,
      category: typeof record.category === 'string' && record.category.trim()
        ? record.category.trim()
        : 'uncategorized',
      riskLevel: typeof record.riskLevel === 'string' ? record.riskLevel : '',
      approvalRequirement: typeof record.approvalRequirement === 'string' ? record.approvalRequirement : '',
      effectiveApprovalPolicy: typeof record.effectiveApprovalPolicy === 'string' ? record.effectiveApprovalPolicy : 'none',
      accessProfileBehavior: typeof record.accessProfileBehavior === 'string' ? record.accessProfileBehavior : 'restricted_allowed',
      accessMode: typeof record.accessMode === 'string' ? record.accessMode : 'read_only',
      policyScope: typeof record.policyScope === 'string' ? record.policyScope : 'fixed',
      inputSensitivePolicy: record.inputSensitivePolicy === true,
      policySummary: typeof record.policySummary === 'string' ? record.policySummary : '',
      runtimeInternal: record.runtimeInternal === true,
      runtimeStatus: normalizeToolRuntimeStatus(record.runtimeStatus),
      runtimeWarnings: normalizeWarningMarkers(record.runtimeWarnings, 'runtime_warning'),
      schemaStatus: typeof record.schemaStatus === 'string' ? record.schemaStatus : 'ok',
      schemaWarnings: normalizeWarningMarkers(record.schemaWarnings, 'schema_warning'),
      enabled: record.enabled !== false,
    });
  }
  return tools;
}

function normalizeCapabilityDependencies(value: unknown): SettingsCapabilityDependency[] {
  if (!Array.isArray(value)) return [];
  const dependencies: SettingsCapabilityDependency[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    const name = typeof record.name === 'string' ? record.name.trim() : '';
    if (!name) continue;
    const status = typeof record.status === 'string' && record.status.trim()
      ? record.status.trim()
      : 'unknown';
    dependencies.push({
      name,
      status,
      requiredBy: normalizeRequiredBy(record.requiredBy),
      roleCount: normalizeNullableNumber(record.roleCount),
      spawnableRoleCount: normalizeNullableNumber(record.spawnableRoleCount),
      configuredCount: normalizeNullableNumber(record.configuredCount),
      enabledCount: normalizeNullableNumber(record.enabledCount),
      readyCount: normalizeNullableNumber(record.readyCount),
      enabledToolCount: normalizeNullableNumber(record.enabledToolCount),
      readyToolCount: normalizeNullableNumber(record.readyToolCount),
      toolCount: normalizeNullableNumber(record.toolCount),
    });
  }
  return dependencies;
}

function normalizeVisionBuiltinTextModelRules(value: unknown): VisionBuiltinTextModelRule[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return [];
    const record = entry as Record<string, unknown>;
    const id = typeof record.id === 'string' ? record.id.trim() : '';
    const displayName = typeof record.displayName === 'string' ? record.displayName.trim() : '';
    if (!id || !displayName) return [];
    return [{
      id,
      displayName,
      examples: Array.isArray(record.examples)
        ? record.examples.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
        : [],
    }];
  });
}

function normalizeSettingsBootstrapPayload(
  payload: Record<string, unknown>,
): AgentSettingsBootstrapSnapshot {
  if (payload.scope !== 'personal' && payload.scope !== 'workspace') {
    throw new Error('settings bootstrap 缺少有效的会话作用域');
  }
  const runtimeSettings = (
    payload.runtimeSettings
    && typeof payload.runtimeSettings === 'object'
    && !Array.isArray(payload.runtimeSettings)
      ? payload.runtimeSettings
      : { locale: 'zh-CN' }
  ) as SettingsRuntimeSnapshot;

  return {
    scope: payload.scope,
    workspaceId: normalizeBindingString(payload.workspaceId),
    workspacePath: normalizeBindingString(payload.workspacePath),
    sessionId: normalizeBindingString(payload.sessionId),
    workerConfigs: (
      payload.workerConfigs
      && typeof payload.workerConfigs === 'object'
      && !Array.isArray(payload.workerConfigs)
        ? payload.workerConfigs
        : {}
    ) as Record<string, unknown>,
    orchestratorConfig: normalizeSettingsSectionConfig(payload.orchestratorConfig),
    orchestratorSessionDefaults: normalizeSettingsSectionConfig(payload.orchestratorSessionDefaults),
    orchestratorSessionConfig: normalizeSettingsSectionConfig(payload.orchestratorSessionConfig),
    effectiveOrchestratorConfig: normalizeSettingsSectionConfig(payload.effectiveOrchestratorConfig),
    auxiliaryConfig: normalizeSettingsSectionConfig(payload.auxiliaryConfig),
    visionConfig: normalizeSettingsSectionConfig(payload.visionConfig),
    visionBuiltinTextModelRules: normalizeVisionBuiltinTextModelRules(payload.visionBuiltinTextModelRules),
    imageGenerationConfig: normalizeSettingsSectionConfig(payload.imageGenerationConfig),
    modelContextWindows: Object.fromEntries(
      Object.entries(normalizeSettingsSectionConfig(payload.modelContextWindows))
        .filter(([, value]) => typeof value === 'number' && Number.isFinite(value) && value > 0)
        .map(([model, value]) => [model, Math.floor(value as number)]),
    ),
    userRulesConfig: normalizeSettingsSectionConfig(payload.userRulesConfig),
    skillsConfig: normalizeSettingsSectionConfig(payload.skillsConfig),
    safeguardConfig: normalizeSettingsSectionConfig(payload.safeguardConfig),
    safeguardAudit: (
      payload.safeguardAudit
      && typeof payload.safeguardAudit === 'object'
      && !Array.isArray(payload.safeguardAudit)
        ? payload.safeguardAudit
        : {}
    ) as SettingsBootstrapPayload['safeguardAudit'],
    repositories: Array.isArray(payload.repositories) ? payload.repositories : [],
    mcpServers: Array.isArray(payload.mcpServers) ? payload.mcpServers : [],
    builtinTools: normalizeBuiltinTools(payload.builtinTools),
    capabilityDependencies: normalizeCapabilityDependencies(payload.capabilityDependencies),
    workerStatuses: (
      payload.workerStatuses
      && typeof payload.workerStatuses === 'object'
      && !Array.isArray(payload.workerStatuses)
        ? payload.workerStatuses
        : {}
    ) as SettingsBootstrapPayload['workerStatuses'],
    runtimeSettings,
    roleTemplates: Array.isArray(payload.roleTemplates) ? payload.roleTemplates : undefined,
    registryEngines: Array.isArray(payload.registryEngines) ? payload.registryEngines : undefined,
    registryAgents: Array.isArray(payload.registryAgents) ? payload.registryAgents : undefined,
    bootstrapScope: payload.bootstrapScope === 'core' ? 'core' : 'full',
    mcpServersHydrated: payload.mcpServersHydrated !== false,
  };
}

export function settingsBootstrapMatchesCurrentWorkspace(
  snapshot: Pick<SettingsBootstrapPayload, 'scope' | 'workspaceId' | 'workspacePath' | 'sessionId'> | null | undefined,
): boolean {
  if (!snapshot) return false;
  const binding = resolveAgentBindingContext();
  const snapshotWorkspaceId = normalizeBindingString(snapshot.workspaceId);
  const snapshotWorkspacePath = normalizeBindingString(snapshot.workspacePath);
  const snapshotSessionId = normalizeBindingString(snapshot.sessionId);
  if (snapshotSessionId !== normalizeBindingString(binding.sessionId)) {
    return false;
  }
  if (snapshot.scope !== binding.scope) {
    return false;
  }
  if (binding.scope === 'personal') {
    return !snapshotWorkspaceId && !snapshotWorkspacePath;
  }
  return snapshotWorkspaceId
    ? snapshotWorkspaceId === binding.workspaceId
    : snapshotWorkspacePath === binding.workspacePath;
}

export interface AgentExecutionStatsItem {
  templateId: string;
  engineId: string;
  bindingRevision: number;
  role: 'worker' | 'orchestrator' | 'auxiliary' | 'image_generation';
  displayName: string;
  provider?: string;
  declaredModelSpec?: string;
  resolvedModel?: string;
  modelIdentityKey?: string;
  llmCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
}

export interface AgentExecutionModelStatsItem {
  modelIdentityKey: string;
  provider: string;
  declaredModelSpec: string;
  resolvedModel: string;
  baseUrlFingerprint: string;
  reasoningEffort?: 'low' | 'medium' | 'high' | 'xhigh' | null;
  totals: AgentExecutionStatsPayload['totals'];
}

export interface AgentExecutionStatsPayload {
  version: number;
  lastAppliedLedgerSeq?: number;
  updatedAt: number;
  totals: {
    llmCallCount: number;
    assignmentCount: number;
    turnCount: number;
    totalTokens: number;
    netInputTokens: number;
    netOutputTokens: number;
    successCount: number;
    failureCount: number;
  };
  items: AgentExecutionStatsItem[];
  models: AgentExecutionModelStatsItem[];
}

// 通知中心直接复用 Rust incident 契约，前端不再维护旧会话通知镜像类型。
export type AgentIncidentNotificationRecord = IncidentNotificationItemDto;
export type AgentNotificationCenterSnapshot = NotificationCenterSnapshotDto;
export type AgentNotificationsPayload = NotificationsResponseDto;


export interface AgentKnowledgeMutationPayload {
  success: boolean;
  workspaceId: string;
  workspacePath: string;
  knowledgeCount: number;
  error?: string;
  payload?: Record<string, unknown>;
}

export interface AgentFilePreviewPayload {
  filePath: string | null;
  content: string;
  sessionId?: string | null;
  workspaceId: string;
  workspacePath: string;
  executionGroupId?: string | null;
  absolutePath?: string;
  exists?: boolean;
  language?: string;
}

export interface AgentChangeDiffPayload {
  filePath: string | null;
  diff: string;
  sessionId?: string | null;
  workspaceId: string;
  workspacePath: string;
  executionGroupId?: string | null;
  additions?: number;
  deletions?: number;
  originalContent?: string | null;
  currentContent?: string | null;
  currentAbsolutePath?: string;
  currentExists?: boolean;
  pendingChangesState?: unknown;
}

export interface AgentSessionTurnImagePayload {
  name: string;
  dataUrl: string;
}

export interface AgentSessionTurnResult {
  sessionId: string;
  entryId: string;
  eventId: string;
  acceptedAt: number;
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  createdSession: boolean;
  route: 'chat' | 'execute' | 'task' | 'continue' | 'steer';
  sessionSummary?: AgentSessionSummary | null;
  /** Root task ID when the backend created an agent run for this action. */
  rootTaskId?: string | null;
  /** 当前轮次实际执行的 action task ID。 */
  actionTaskId?: string | null;
  executionChainRef?: string | null;
  /** 后端生成的 canonical 用户消息 item ID。 */
  userMessageItemId?: string | null;
  queued?: boolean;
  queueId?: string | null;
  queuePosition?: number | null;
  canonicalSchemaVersion?: string | null;
  canonicalEventKind?: string | null;
  canonicalTurn?: CanonicalTurn | null;
  canonicalItem?: CanonicalTurnItem | null;
  /** 仅在 steer 路由下返回：实际接收引导的 Turn ID。 */
  steeredTurnId?: string | null;
}

export class AgentApiError extends Error {
  readonly status: number;
  readonly action: string;
  readonly errorCode?: string;
  readonly detail?: string;
  readonly conflictKind?: string;
  readonly activeTurnId?: string;

  constructor(
    status: number,
    message: string,
    action: string,
    errorCode?: string,
    detail?: string,
    conflictKind?: string,
    activeTurnId?: string,
  ) {
    super(message);
    this.name = 'AgentApiError';
    this.status = status;
    this.action = action;
    this.errorCode = errorCode;
    this.detail = detail;
    this.conflictKind = conflictKind;
    this.activeTurnId = activeTurnId;
  }
}

export interface AgentNotificationScope {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

function safeReadLocalStorage(key: string): string {
  if (typeof window === 'undefined') {
    return '';
  }
  try {
    return localStorage.getItem(key)?.trim() || '';
  } catch (error) {
    console.warn(`[agent-api] localStorage 读取失败(${key})`, error);
    return '';
  }
}

function safeWriteLocalStorage(key: string, value: string): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    localStorage.setItem(key, value);
  } catch (error) {
    console.warn(`[agent-api] localStorage 写入失败(${key})`, error);
  }
}

function persistAgentBaseUrl(baseUrl: string): void {
  if (typeof window === 'undefined' || !baseUrl.trim()) {
    return;
  }
  safeWriteLocalStorage(RUNTIME_BASE_URL_STORAGE_KEY, baseUrl.trim());
}

function getStoredAgentBaseUrl(): string {
  return safeReadLocalStorage(RUNTIME_BASE_URL_STORAGE_KEY);
}

function deriveWorkspaceName(rootPath: string, workspaceId: string): string {
  const fallbackName = rootPath
    .split(/[\\/]/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .pop();
  return fallbackName || workspaceId || 'workspace';
}

function normalizeWorkspaceSummary(raw: RawAgentWorkspaceSummary): AgentWorkspaceSummary {
  const workspaceId = raw.workspaceId?.trim() || '';
  const rootPath = raw.rootPath?.trim() || '';
  return {
    workspaceId,
    rootPath,
    rootPathRef: raw.rootPathRef?.trim() || undefined,
    name: deriveWorkspaceName(rootPath, workspaceId),
    isActive: raw.isActive === true,
  };
}

function cacheWorkspaceSummaries(workspaces: AgentWorkspaceSummary[]): AgentWorkspaceSummary[] {
  cachedWorkspaceSummaries = workspaces.filter((workspace) => workspace.workspaceId.length > 0);
  return workspaces;
}

function findCachedWorkspaceSummary(workspaceId: string): AgentWorkspaceSummary {
  return cachedWorkspaceSummaries.find((workspace) => workspace.workspaceId === workspaceId) ?? {
    workspaceId,
    rootPath: '',
    name: workspaceId || 'workspace',
    isActive: false,
  };
}

function normalizeSessionSummary(raw: RawAgentSessionSummary): AgentSessionSummary {
  const id = raw.id?.trim() || raw.sessionId?.trim() || '';
  const workspaceId = raw.workspaceId?.trim() || '';
  const createdAt = raw.createdAt ?? Date.now();
  const updatedAt = raw.updatedAt ?? createdAt;
  const name = raw.name?.trim() || raw.title?.trim() || undefined;
  const preview = raw.preview?.trim() || undefined;
  const messageCount = raw.messageCount;
  const runningTaskCount = raw.runningTaskCount;
  const isRunning = raw.isRunning;
  const hasUnreadCompletion = raw.hasUnreadCompletion;
  return {
    id,
    ...(workspaceId ? { workspaceId } : {}),
    name,
    createdAt,
    updatedAt,
    ...(typeof messageCount === 'number' ? { messageCount } : {}),
    ...(typeof isRunning === 'boolean' ? { isRunning } : {}),
    ...(typeof runningTaskCount === 'number' ? { runningTaskCount: Math.max(0, Math.floor(runningTaskCount)) } : {}),
    ...(typeof hasUnreadCompletion === 'boolean' ? { hasUnreadCompletion } : {}),
    ...(preview ? { preview } : {}),
  };
}

function getConfiguredAgentBaseUrl(): string {
  const viteEnv = (import.meta as ImportMeta & { env?: { VITE_AGENT_BASE_URL?: string } }).env;
  return viteEnv?.VITE_AGENT_BASE_URL?.trim() || '';
}

function getConfiguredAgentProxyTarget(): string {
  const viteEnv = (import.meta as ImportMeta & { env?: { VITE_AGENT_PROXY_TARGET?: string } }).env;
  return viteEnv?.VITE_AGENT_PROXY_TARGET?.trim() || '';
}

function collectAgentBaseUrlCandidates(): string[] {
  if (typeof window === 'undefined') {
    return [getDefaultAgentBaseUrl()];
  }
  const injectedBaseUrl = (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim() || '';
  const configuredBaseUrl = getConfiguredAgentBaseUrl();
  const configuredProxyTarget = getConfiguredAgentProxyTarget();
  const currentUrl = new URL(window.location.href);
  const queryBaseUrl = currentUrl.searchParams.get('agentBaseUrl')?.trim() || '';
  const servedByAgentOrigin = currentUrl.protocol.startsWith('http')
    && (currentUrl.pathname === '/' || currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/'))
    ? currentUrl.origin
    : '';
  const candidates = [
    servedByAgentOrigin,
    queryBaseUrl,
    injectedBaseUrl,
    configuredProxyTarget && servedByAgentOrigin ? servedByAgentOrigin : '',
    configuredBaseUrl,
    getStoredAgentBaseUrl(),
    getDefaultAgentBaseUrl(),
  ].filter((value) => value && value.trim());
  return Array.from(new Set(candidates));
}

async function isReachableAgentBaseUrl(baseUrl: string): Promise<boolean> {
  const controller = typeof AbortController === 'function' ? new AbortController() : null;
  const timer = controller
    ? window.setTimeout(() => controller.abort(), AGENT_PROBE_TIMEOUT_MS)
    : null;
  try {
    const response = await getTransport().request(`${baseUrl.replace(/\/$/, '')}/health`, {
      cache: 'no-store',
      ...(controller ? { signal: controller.signal } : {}),
    });
    return response.ok;
  } catch {
    return false;
  } finally {
    if (timer !== null) {
      window.clearTimeout(timer);
    }
  }
}

export async function probeReachableAgentBaseUrl(): Promise<string | null> {
  const candidates = collectAgentBaseUrlCandidates();
  for (const candidate of candidates) {
    if (await isReachableAgentBaseUrl(candidate)) {
      persistAgentBaseUrl(candidate);
      return candidate;
    }
  }
  return null;
}

export function dispatchAgentConnectionEvent(detail: AgentConnectionEventDetail): void {
  if (typeof window === 'undefined') {
    return;
  }
  window.dispatchEvent(new CustomEvent<AgentConnectionEventDetail>(RUNTIME_CONNECTION_EVENT, { detail }));
}

async function parseAgentJson<T>(response: Response, action: string): Promise<T> {
  if (!response.ok) {
    let backendError: string | null = null;
    let backendErrorCode: string | undefined;
    let backendDetail: string | undefined;
    let conflictKind: string | undefined;
    let activeTurnId: string | undefined;
    const contentType = response.headers.get('content-type') || '';
    if (contentType.includes('application/json')) {
      try {
        const payload = await response.json() as {
          error?: string;
          message?: string;
          error_code?: string;
          code?: string;
          detail?: string;
          conflict_kind?: string;
          active_turn_id?: string;
        };
        if (typeof payload?.error === 'string' && payload.error.trim()) {
          backendError = payload.error.trim();
        } else if (typeof payload?.message === 'string' && payload.message.trim()) {
          backendError = payload.message.trim();
        }
        const rawErrorCode = typeof payload?.error_code === 'string' && payload.error_code.trim()
          ? payload.error_code.trim()
          : (typeof payload?.code === 'string' && payload.code.trim() ? payload.code.trim() : '');
        if (rawErrorCode) {
          backendErrorCode = rawErrorCode;
        }
        backendDetail = typeof payload?.detail === 'string' && payload.detail.trim()
          ? payload.detail.trim()
          : undefined;
        conflictKind = typeof payload?.conflict_kind === 'string' && payload.conflict_kind.trim()
          ? payload.conflict_kind.trim()
          : undefined;
        activeTurnId = typeof payload?.active_turn_id === 'string' && payload.active_turn_id.trim()
          ? payload.active_turn_id.trim()
          : undefined;
      } catch {
        // ignore malformed error payload and fallback to generic message
      }
    }
    throw new AgentApiError(
      response.status,
      backendError || `${action} failed: ${response.status}`,
      action,
      backendErrorCode,
      backendDetail,
      conflictKind,
      activeTurnId,
    );
  }

  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    throw new Error(i18n.t('bridge.notConnected'));
  }

  return await response.json() as T;
}

export function resolveAgentBaseUrl(): string {
  if (typeof window === 'undefined') {
    return getDefaultAgentBaseUrl();
  }
  const currentUrl = new URL(window.location.href);
  const servedByAgent = currentUrl.protocol.startsWith('http')
    && (currentUrl.pathname === '/' || currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/'));
  if (servedByAgent) {
    persistAgentBaseUrl(currentUrl.origin);
    return currentUrl.origin;
  }
  const injectedBaseUrl = (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim();
  if (injectedBaseUrl) {
    return injectedBaseUrl;
  }
  const configuredProxyTarget = getConfiguredAgentProxyTarget();
  if (configuredProxyTarget) {
    persistAgentBaseUrl(configuredProxyTarget);
    return configuredProxyTarget;
  }
  const configuredBaseUrl = getConfiguredAgentBaseUrl();
  if (configuredBaseUrl) {
    persistAgentBaseUrl(configuredBaseUrl);
    return configuredBaseUrl;
  }
  const fromQuery = currentUrl.searchParams.get('agentBaseUrl')?.trim();
  if (fromQuery) {
    persistAgentBaseUrl(fromQuery);
    return fromQuery;
  }
  if (servedByAgent) {
    persistAgentBaseUrl(currentUrl.origin);
    return currentUrl.origin;
  }
  return getStoredAgentBaseUrl() || getDefaultAgentBaseUrl();
}

export function isPublicTunnelAccess(): boolean {
  if (typeof window === 'undefined') return false;
  const currentUrl = new URL(window.location.href);
  return Boolean(currentUrl.searchParams.get('tunnel_token'));
}

/** 构造完整的 Agent API URL；公网凭据由统一传输层附加。 */
export function agentUrl(pathname: string, query?: string): string {
  const base = resolveAgentBaseUrl();
  const q = query || '';
  return q ? `${base}${pathname}?${q}` : `${base}${pathname}`;
}

export interface TerminalSessionRequest {
  terminalTabId: string;
  workspaceId?: string | null;
  workspacePath?: string;
  sessionId: string;
  cols?: number;
  rows?: number;
}

function terminalSessionUrl(request: TerminalSessionRequest, channel: boolean): URL {
  const suffix = channel ? '/channel' : '';
  const url = new URL(agentUrl(
    `/api/terminal/sessions/${encodeURIComponent(request.terminalTabId.trim())}${suffix}`,
  ));
  const workspaceId = request.workspaceId?.trim() || '';
  const workspacePath = request.workspacePath?.trim() || '';
  const hasWorkspaceBinding = Boolean(workspaceId || workspacePath);
  url.searchParams.set('scope', hasWorkspaceBinding ? 'workspace' : 'personal');
  if (workspaceId) url.searchParams.set('workspaceId', workspaceId);
  url.searchParams.set('sessionId', request.sessionId.trim());
  if (workspacePath) url.searchParams.set('workspacePath', workspacePath);
  if (request.cols) url.searchParams.set('cols', String(request.cols));
  if (request.rows) url.searchParams.set('rows', String(request.rows));
  return url;
}

export function terminalChannelUrl(request: TerminalSessionRequest): string {
  const url = terminalSessionUrl(request, true);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.toString();
}

export async function closeTerminalSession(request: TerminalSessionRequest): Promise<void> {
  const response = await getTransport().request(terminalSessionUrl(request, false).toString(), {
    method: 'DELETE',
  });
  if (!response.ok) await parseAgentJson(response, 'close terminal session');
}

export type BrowserSessionLifecycle = 'creating' | 'ready' | 'recovering' | 'interrupted' | 'failed' | 'closed';
export type BrowserTabLifecycle = 'creating' | 'ready' | 'suspended' | 'crashed' | 'closed';
export type BrowserControlMode = 'agent' | 'user';
export type BrowserViewportMode = 'auto' | 'fixed';
export type BrowserDeviceType = 'desktop' | 'mobile';
export const BROWSER_AUTHORITY_CHANGED_EVENT = 'magi:browserAuthorityChanged';

export interface BrowserViewport {
  width: number;
  height: number;
  deviceScaleFactorMillis: number;
  deviceType: BrowserDeviceType;
}

export type BrowserAnnotationStatus = 'active' | 'resolved' | 'stale' | 'deleted';

export interface BrowserNormalizedRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface BrowserAnnotationAnchorBase {
  url: string;
  origin: string | null;
  viewport: BrowserViewport;
  scrollX: number;
  scrollY: number;
  snapshotRevision: number;
}

export type BrowserAnnotationAnchor =
  | (BrowserAnnotationAnchorBase & {
      kind: 'element';
      boundingBox: BrowserNormalizedRect;
      framePath: string[];
      testId: string | null;
      stableId: string | null;
      ariaRole: string | null;
      ariaName: string | null;
      tagName: string;
      textExcerpt: string | null;
      cssPath: string;
      ancestorFingerprint: string;
      domFingerprint: string;
    })
  | (BrowserAnnotationAnchorBase & {
      kind: 'region';
      rect: BrowserNormalizedRect;
    });

export interface BrowserAnnotationSnapshot {
  annotationId: string;
  browserSessionId: string;
  tabId: string;
  sequence: number;
  author: 'user' | 'agent';
  kind: 'element' | 'region';
  anchor: BrowserAnnotationAnchor;
  comment: string;
  status: BrowserAnnotationStatus;
  screenshotArtifactId: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface BrowserTabSnapshot {
  tabId: string;
  browserSessionId: string;
  lifecycle: BrowserTabLifecycle;
  url: string;
  origin: string | null;
  title: string;
  navigationRevision: number;
  snapshotRevision: number;
  createdAt: number;
  updatedAt: number;
  annotations: BrowserAnnotationSnapshot[];
}

export interface BrowserSessionSnapshot {
  browserSessionId: string;
  workspaceId: string | null;
  sessionId: string;
  profileId: string;
  lifecycle: BrowserSessionLifecycle;
  activeTabId: string | null;
  tabs: BrowserTabSnapshot[];
  runtimeEpoch: number;
  revision: number;
  controlMode: BrowserControlMode;
  controlFence: number;
  agentOccupied: boolean;
  createdAt: number;
  updatedAt: number;
}

export interface BrowserCapabilitiesSnapshot {
  revision: number;
  inAppBrowserEnabled: boolean;
  browserUseEnabled: boolean;
  hostStatus: 'stopped' | 'starting' | 'ready' | 'reconnecting' | 'failed';
  hostProtocolCompatible: boolean;
  accessProfile: string;
  hostState: string;
  lastErrorCode: string | null;
  platformCapabilities: {
    desktopBrowserSurface: boolean;
    browserRecords: boolean;
    browserAnnotations: boolean;
    browserRemoteSurface: boolean;
  };
}

export type BrowserClientPlatform = 'desktop' | 'web' | 'mobile-web';

export function browserClientPlatform(): BrowserClientPlatform {
  if (typeof window !== 'undefined' && window.magiDesktop?.runtime === 'electron') {
    return 'desktop';
  }
  if (
    typeof navigator !== 'undefined'
    && /Android|iPhone|iPad|iPod|Mobile/i.test(navigator.userAgent)
  ) {
    return 'mobile-web';
  }
  return 'web';
}

export async function getBrowserCapabilities(sessionId?: string): Promise<BrowserCapabilitiesSnapshot> {
  const queryParams = new URLSearchParams({ clientPlatform: browserClientPlatform() });
  if (sessionId?.trim()) queryParams.set('sessionId', sessionId.trim());
  const response = await getTransport().request(
    agentUrl('/api/browser/capabilities', queryParams.toString()),
    {
    cache: 'no-store',
    },
  );
  return parseAgentJson<BrowserCapabilitiesSnapshot>(response, 'load browser capabilities');
}

export async function updateBrowserSettings(settings: {
  inAppBrowserEnabled: boolean;
  browserUseEnabled: boolean;
}): Promise<BrowserCapabilitiesSnapshot> {
  const response = await getTransport().request(agentUrl('/api/browser/settings'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...settings, clientPlatform: browserClientPlatform() }),
  });
  return parseAgentJson<BrowserCapabilitiesSnapshot>(response, 'update browser settings');
}

export async function createBrowserSession(
  workspaceId: string | null | undefined,
  sessionId: string,
  workspacePath?: string,
): Promise<BrowserSessionSnapshot> {
  const normalizedWorkspaceId = workspaceId?.trim() || '';
  const normalizedWorkspacePath = workspacePath?.trim() || '';
  const hasWorkspaceBinding = Boolean(normalizedWorkspaceId || normalizedWorkspacePath);
  const response = await getTransport().request(agentUrl('/api/browser/sessions'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
        scope: hasWorkspaceBinding ? 'workspace' : 'personal',
        workspaceId: hasWorkspaceBinding ? (normalizedWorkspaceId || null) : null,
        workspacePath: hasWorkspaceBinding ? (normalizedWorkspacePath || null) : null,
        sessionId,
        clientPlatform: browserClientPlatform(),
      }),
  });
  return parseAgentJson<BrowserSessionSnapshot>(response, 'create browser session');
}

export async function materializeSession(
  workspaceId?: string | null,
  workspacePath?: string,
): Promise<{ sessionId: string; workspaceId: string | null }> {
  const normalizedWorkspaceId = workspaceId?.trim() || '';
  const normalizedWorkspacePath = workspacePath?.trim() || '';
  const hasWorkspaceBinding = Boolean(normalizedWorkspaceId || normalizedWorkspacePath);
  const response = await getTransport().request(agentUrl('/api/session/materialize'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      scope: hasWorkspaceBinding ? 'workspace' : 'personal',
      workspaceId: hasWorkspaceBinding ? (normalizedWorkspaceId || null) : null,
      workspacePath: hasWorkspaceBinding ? (normalizedWorkspacePath || null) : null,
    }),
  });
  const payload = await parseAgentJson<{ sessionId?: unknown; workspaceId?: unknown }>(
    response,
    'materialize session',
  );
  const sessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
  if (!sessionId) throw new Error('materialize session response missing sessionId');
  return {
    sessionId,
    workspaceId: typeof payload.workspaceId === 'string' && payload.workspaceId.trim()
      ? payload.workspaceId.trim()
      : null,
  };
}

export async function getBrowserSession(browserSessionId: string): Promise<BrowserSessionSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/sessions/${encodeURIComponent(browserSessionId)}`),
    { cache: 'no-store' },
  );
  return parseAgentJson<BrowserSessionSnapshot>(response, 'load browser session');
}

export async function getCurrentBrowserSession(
  workspaceId: string | null | undefined,
  sessionId: string,
  workspacePath?: string,
): Promise<BrowserSessionSnapshot | null> {
  const normalizedWorkspaceId = workspaceId?.trim() || '';
  const normalizedWorkspacePath = workspacePath?.trim() || '';
  const hasWorkspaceBinding = Boolean(normalizedWorkspaceId || normalizedWorkspacePath);
  const query = new URLSearchParams({
    scope: hasWorkspaceBinding ? 'workspace' : 'personal',
    sessionId,
    ...(normalizedWorkspaceId ? { workspaceId: normalizedWorkspaceId } : {}),
    ...(normalizedWorkspacePath ? { workspacePath: normalizedWorkspacePath } : {}),
  }).toString();
  const response = await getTransport().request(
    agentUrl('/api/browser/sessions/current', query),
    { cache: 'no-store' },
  );
  const payload = await parseAgentJson<{ session: BrowserSessionSnapshot | null }>(
    response,
    'load current browser session',
  );
  return payload.session;
}

export async function createBrowserTab(
  browserSessionId: string,
  initialUrl: string,
): Promise<BrowserTabSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/sessions/${encodeURIComponent(browserSessionId)}/tabs`),
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ initialUrl, clientPlatform: browserClientPlatform() }),
    },
  );
  return parseAgentJson<BrowserTabSnapshot>(response, 'create browser tab');
}

const BROWSER_TAB_READY_POLL_INTERVAL_MS = 100;
const BROWSER_TAB_READY_TIMEOUT_MS = 30_000;

/**
 * 创建 Browser Tab 的 HTTP 请求只提交逻辑 Tab，Chromium Page 由 Host 异步物化。
 * 创建后的首个 authority 事件可能早于当前会话切换完成，因此这里以权威快照作为
 * 创建链路的确定性收敛点，不把 UI 是否刚好订阅到某个事件作为 ready 的前提。
 */
export async function waitForBrowserTabReady(
  browserSessionId: string,
  tabId: string,
): Promise<BrowserSessionSnapshot> {
  const deadline = Date.now() + BROWSER_TAB_READY_TIMEOUT_MS;
  let lastError: unknown = null;
  while (Date.now() <= deadline) {
    try {
      const snapshot = await getBrowserSession(browserSessionId);
      const tab = snapshot.tabs.find((candidate) => candidate.tabId === tabId);
      if (!tab) {
        throw new Error(`browser_tab_not_found:${tabId}`);
      }
      if (tab.lifecycle !== 'creating') {
        return snapshot;
      }
      lastError = null;
    } catch (error) {
      lastError = error;
    }
    await new Promise<void>((resolve) => {
      globalThis.setTimeout(resolve, BROWSER_TAB_READY_POLL_INTERVAL_MS);
    });
  }
  if (lastError instanceof Error) {
    throw lastError;
  }
  throw new Error(`browser_tab_ready_timeout:${tabId}`);
}

export async function activateBrowserTab(tabId: string): Promise<BrowserSessionSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}/activate`),
    { method: 'POST' },
  );
  return parseAgentJson<BrowserSessionSnapshot>(response, 'activate browser tab');
}

/** 同步右侧面板当前选中的 Browser Tab，供 LLM 工具默认目标选择使用。 */
export async function setActiveBrowserTab(
  browserSessionId: string,
  tabId: string,
): Promise<BrowserSessionSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/sessions/${encodeURIComponent(browserSessionId)}/active-tab`),
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ tabId }),
    },
  );
  return parseAgentJson<BrowserSessionSnapshot>(response, 'set active browser tab');
}

export async function closeBrowserTab(tabId: string): Promise<void> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}`),
    { method: 'DELETE' },
  );
  if (!response.ok) await parseAgentJson(response, 'close browser tab');
}

export async function navigateBrowserTab(
  tabId: string,
  action: 'url' | 'back' | 'forward' | 'reload',
  url?: string,
): Promise<BrowserTabSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}/navigation`),
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        action,
        ...(url ? { url } : {}),
        clientPlatform: browserClientPlatform(),
      }),
    },
  );
  return parseAgentJson<BrowserTabSnapshot>(response, 'navigate browser tab');
}

export type BrowserAnnotationSelection =
  | {
      kind: 'element';
      navigationRevision: number;
      x: number;
      y: number;
    }
  | {
      kind: 'region';
      navigationRevision: number;
      rect: BrowserNormalizedRect;
    };

export async function createBrowserAnnotation(
  tabId: string,
  selection: BrowserAnnotationSelection,
  comment: string,
): Promise<BrowserAnnotationSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}/annotations`),
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ selection, comment }),
    },
  );
  return parseAgentJson<BrowserAnnotationSnapshot>(response, 'create browser annotation');
}

export function createBrowserElementAnnotation(
  tabId: string,
  selection: Omit<Extract<BrowserAnnotationSelection, { kind: 'element' }>, 'kind'>,
  comment: string,
): Promise<BrowserAnnotationSnapshot> {
  return createBrowserAnnotation(tabId, { kind: 'element', ...selection }, comment);
}

export function createBrowserRegionAnnotation(
  tabId: string,
  selection: Omit<Extract<BrowserAnnotationSelection, { kind: 'region' }>, 'kind'>,
  comment: string,
): Promise<BrowserAnnotationSnapshot> {
  return createBrowserAnnotation(tabId, { kind: 'region', ...selection }, comment);
}

export async function updateBrowserAnnotationStatus(
  annotationId: string,
  status: BrowserAnnotationStatus,
): Promise<BrowserAnnotationSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/annotations/${encodeURIComponent(annotationId)}/status`),
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ status }),
    },
  );
  return parseAgentJson<BrowserAnnotationSnapshot>(response, 'update browser annotation');
}

export async function updateBrowserAnnotationComment(
  annotationId: string,
  comment: string,
): Promise<BrowserAnnotationSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/annotations/${encodeURIComponent(annotationId)}`),
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ comment }),
    },
  );
  return parseAgentJson<BrowserAnnotationSnapshot>(response, 'update browser annotation comment');
}

export function browserScreenshotUrl(tabId: string): string {
  return agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}/screenshot`);
}

export function browserAnnotationArtifactUrl(annotationId: string, sessionId: string): string {
  const url = new URL(agentUrl(`/api/browser/annotations/${encodeURIComponent(annotationId)}/artifact`));
  url.searchParams.set('sessionId', sessionId.trim());
  return url.toString();
}

export function isWebAgentMode(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  const currentUrl = new URL(window.location.href);
  return currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/');
}

function buildBoundQuery(
  extra: Record<string, string>,
  options: { includeScope?: boolean; includeSession?: boolean } = {},
): string {
  return buildBoundQueryWithOverride(extra, undefined, options);
}

export function buildFilePreviewQuery(
  filePath: string,
  options: { includeSession?: boolean; sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): string {
  const explicitSessionId = typeof options.sessionId === 'string' && options.sessionId.trim().length > 0;
  const workspaceId = options.workspaceId?.trim() || '';
  const workspacePath = options.workspacePath?.trim() || '';
  const bindingOverride: AgentBindingOverride = workspaceId || workspacePath
    ? { scope: 'workspace', workspaceId, workspacePath, sessionId: options.sessionId }
    : { sessionId: options.sessionId };
  return buildBoundQueryWithOverride(
    { filePath },
    bindingOverride,
    {
      includeScope: false,
      includeSession: options.includeSession !== false && (options.includeSession === true || explicitSessionId),
    },
  );
}

function resolveBindingWithOverride(
  bindingOverride?: AgentBindingOverride,
): AgentBindingContext {
  const resolvedBinding = resolveAgentBindingContext();
  const sessionId = bindingOverride?.sessionId === undefined
    ? resolvedBinding.sessionId
    : bindingOverride.sessionId.trim();
  if (!bindingOverride?.scope) {
    return resolvedBinding.scope === 'workspace'
      ? { ...resolvedBinding, sessionId }
      : { scope: 'personal', sessionId };
  }
  if (bindingOverride.scope === 'personal') {
    return { scope: 'personal', sessionId };
  }
  return {
    scope: 'workspace',
    workspaceId: bindingOverride.workspaceId.trim(),
    workspacePath: bindingOverride.workspacePath.trim(),
    sessionId,
  };
}

function buildBoundQueryWithOverride(
  extra: Record<string, string>,
  bindingOverride?: AgentBindingOverride,
  options: { includeScope?: boolean; includeSession?: boolean } = {},
): string {
  const binding = resolveBindingWithOverride(bindingOverride);
  const query = new URLSearchParams();
  if (options.includeScope !== false) {
    query.set('scope', binding.scope);
  }
  if (binding.scope === 'workspace') {
    if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
    if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
  }
  if (options.includeSession !== false && binding.sessionId) query.set('sessionId', binding.sessionId);
  for (const [key, value] of Object.entries(extra)) {
    if (value) {
      query.set(key, value);
    }
  }
  return query.toString();
}

function createNotificationBindingOverride(scope: AgentNotificationScope): AgentBindingOverride {
  const sessionId = scope.sessionId?.trim() || '';
  const workspaceId = scope.workspaceId?.trim() || '';
  const workspacePath = scope.workspacePath?.trim() || '';
  return workspaceId || workspacePath
    ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
    : { scope: 'personal', sessionId };
}

async function postJsonWithBinding<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
  includeSession = true,
  signal?: AbortSignal,
  options: { includeWorkspaceBinding?: boolean } = {},
): Promise<T> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    const bindingPayload = {
      ...(options.includeWorkspaceBinding !== false && binding.scope === 'workspace' && binding.workspaceId
        ? { workspaceId: binding.workspaceId }
        : {}),
      ...(options.includeWorkspaceBinding !== false && binding.scope === 'workspace' && binding.workspacePath
        ? { workspacePath: binding.workspacePath }
        : {}),
      ...(includeSession && binding.sessionId ? { sessionId: binding.sessionId } : {}),
    };
    const response = await getTransport().request(agentUrl(pathname), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      signal,
      body: JSON.stringify({
        ...bindingPayload,
        ...payload,
      }),
    });
    return parseAgentJson<T>(response, action);
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

async function postBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
  signal?: AbortSignal,
): Promise<T> {
  return await postJsonWithBinding<T>(pathname, payload, action, bindingOverride, true, signal);
}

async function postWorkspacePathBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
): Promise<T> {
  return await postJsonWithBinding<T>(
    pathname,
    payload,
    action,
    bindingOverride,
    true,
    undefined,
    { includeWorkspaceBinding: false },
  );
}

export async function listAgentWorkspaces(): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces'));
    const payload = await parseAgentJson<{ workspaces?: RawAgentWorkspaceSummary[] }>(response, 'list workspaces');
    return cacheWorkspaceSummaries(
      Array.isArray(payload.workspaces)
        ? payload.workspaces.map((workspace) => normalizeWorkspaceSummary(workspace))
        : [],
    );
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function registerAgentWorkspace(rootPath: string): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/register'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        path: rootPath,
      }),
    });
    const payload = await parseAgentJson<{ workspaces?: RawAgentWorkspaceSummary[]; workspaceId?: string; registered?: boolean }>(response, 'register workspace');
    if (Array.isArray(payload.workspaces)) {
      return cacheWorkspaceSummaries(payload.workspaces.map((workspace) => normalizeWorkspaceSummary(workspace)));
    }
    if (payload.registered || payload.workspaceId) {
      return await listAgentWorkspaces();
    }
    return [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function removeAgentWorkspace(workspaceId: string): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/remove'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaceId,
      }),
    });
    const payload = await parseAgentJson<{ workspaces?: RawAgentWorkspaceSummary[]; removed?: boolean }>(response, 'remove workspace');
    if (Array.isArray(payload.workspaces)) {
      return cacheWorkspaceSummaries(payload.workspaces.map((workspace) => normalizeWorkspaceSummary(workspace)));
    }
    if (payload.removed) {
      return await listAgentWorkspaces();
    }
    return [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function pickAgentWorkspace(): Promise<AgentWorkspacePickResult> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/pick'));
    return await parseAgentJson<AgentWorkspacePickResult>(response, 'pick workspace');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export interface WorkspaceDirectoryEntry {
  name: string;
  path: string;
  pathRef: string;
  displayPath: string;
  isDirectory: boolean;
  hasChildren?: boolean;
}

export interface DirectoryPathNode {
  name: string;
  pathRef: string;
  displayPath: string;
}

export interface DirectoryPickerEntry extends DirectoryPathNode {
  isDirectory: true;
  isHidden: boolean;
}

export interface DirectoryListResult {
  pathRef: string;
  displayPath: string;
  parentPathRef?: string | null;
  breadcrumbs: DirectoryPathNode[];
  roots: DirectoryPathNode[];
  entries: DirectoryPickerEntry[];
  error?: string;
}

export interface WorkspaceDirectoryListResult {
  workspaceId: string;
  workspacePath: string;
  path: string;
  pathRef: string;
  parent: string;
  parentPathRef: string;
  entries: WorkspaceDirectoryEntry[];
}

export interface ResolvedAgentPath {
  pathRef: string;
  displayPath: string;
  name: string;
  kind: 'file' | 'directory';
}

export interface AgentFileRevealTarget {
  targetPathRef: string;
  workspaceRootPathRef: string;
  displayPath: string;
  ancestorPathRefs: string[];
}

function throwNormalizedDirectoryError(error: unknown): never {
  if (error instanceof TypeError) {
    throw new Error(i18n.t('bridge.agentUnreachable'));
  }
  if (error instanceof AgentApiError) {
    const message = error.message.trim();
    if (message.includes('ENOENT')) {
      throw new Error(i18n.t('bridge.dirNotFound'));
    }
    if (message.includes('EACCES') || message.includes('EPERM')) {
      throw new Error(i18n.t('bridge.dirNoAccess'));
    }
  }
  throw error;
}

export async function listAgentDirectory(
  dirPath: string,
  showHidden: boolean,
  workspaceId: string,
): Promise<WorkspaceDirectoryListResult> {
  try {
    const query = new URLSearchParams();
    if (dirPath) {
      query.set('path', dirPath);
    }
    query.set('workspaceId', workspaceId);
    if (showHidden) {
      query.set('showHidden', '1');
    }
    const response = await getTransport().request(agentUrl('/api/filesystem/list', query.toString()));
    return await parseAgentJson<WorkspaceDirectoryListResult>(response, 'list directory');
  } catch (error) {
    throwNormalizedDirectoryError(error);
  }
}

export async function browseAgentDirectory(
  options: {
    pathRef?: string;
    input?: string;
    basePathRef?: string;
    showHidden?: boolean;
  } = {},
): Promise<DirectoryListResult> {
  try {
    const query = new URLSearchParams();
    if (options.pathRef) {
      query.set('pathRef', options.pathRef);
    }
    if (options.input) {
      query.set('path', options.input);
    }
    if (options.basePathRef) {
      query.set('basePathRef', options.basePathRef);
    }
    if (options.showHidden) {
      query.set('showHidden', '1');
    }
    const response = await getTransport().request(agentUrl('/api/filesystem/browse', query.toString()));
    return await parseAgentJson<DirectoryListResult>(response, 'browse directory');
  } catch (error) {
    throwNormalizedDirectoryError(error);
  }
}

export async function resolveAgentPath(
  input: string,
  basePathRef?: string,
): Promise<ResolvedAgentPath> {
  return await postJsonWithBinding<ResolvedAgentPath>(
    '/api/filesystem/resolve',
    { input, ...(basePathRef ? { basePathRef } : {}) },
    'resolve filesystem path',
    undefined,
    false,
    undefined,
    { includeWorkspaceBinding: false },
  );
}

export async function resolveAgentFileRevealTarget(
  filePath: string,
  scope: WorkspaceAgentBindingOverride,
  signal?: AbortSignal,
): Promise<AgentFileRevealTarget> {
  return await postJsonWithBinding<AgentFileRevealTarget>(
    '/api/files/reveal-target',
    { filePath },
    'resolve workspace file reveal target',
    scope,
    true,
    signal,
  );
}

export async function getWorkspaceSessions(
  workspaceId: string,
  workspacePath?: string,
): Promise<AgentWorkspaceSessionsSnapshot> {
  try {
    const query = new URLSearchParams({ workspaceId });
    if (workspacePath?.trim()) {
      query.set('workspacePath', workspacePath.trim());
    }
    const response = await getTransport().request(
      agentUrl('/api/workspaces/sessions', query.toString())
    );
    const payload = await parseAgentJson<{
      runtimeEpoch?: string;
      eventStreamNextSequence?: number;
      workspace?: RawAgentWorkspaceSummary;
      sessions?: RawAgentSessionSummary[];
    }>(response, 'workspace sessions');
    const sessions = Array.isArray(payload.sessions)
      ? payload.sessions.map((session) => normalizeSessionSummary(session))
      : [];
    const runtimeEpoch = payload.runtimeEpoch?.trim() || '';
    const eventStreamNextSequence = payload.eventStreamNextSequence;
    if (
      !runtimeEpoch
      || typeof eventStreamNextSequence !== 'number'
      || !Number.isFinite(eventStreamNextSequence)
      || eventStreamNextSequence < 1
    ) {
      throw new Error('workspace sessions 缺少有效的运行时代际或事件游标');
    }
    return {
      runtimeEpoch,
      eventStreamNextSequence: Math.floor(eventStreamNextSequence),
      workspace: payload.workspace
        ? normalizeWorkspaceSummary(payload.workspace)
        : findCachedWorkspaceSummary(workspaceId),
      sessions,
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getPersonalSessions(): Promise<{
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  sessions: AgentSessionSummary[];
}> {
  const response = await getTransport().request(agentUrl('/api/sessions/personal'));
  const payload = await parseAgentJson<{
    runtimeEpoch?: string;
    eventStreamNextSequence?: number;
    sessions?: RawAgentSessionSummary[];
  }>(response, 'personal sessions');
  const runtimeEpoch = payload.runtimeEpoch?.trim() || '';
  const eventStreamNextSequence = payload.eventStreamNextSequence;
  if (!runtimeEpoch || typeof eventStreamNextSequence !== 'number' || !Number.isFinite(eventStreamNextSequence)) {
    throw new Error('personal sessions 缺少有效的运行时代际或事件游标');
  }
  return {
    runtimeEpoch,
    eventStreamNextSequence: Math.floor(eventStreamNextSequence),
    sessions: Array.isArray(payload.sessions) ? payload.sessions.map(normalizeSessionSummary) : [],
  };
}

export async function getAgentSessionMessages(options: {
  sessionId: string;
  scope: 'personal' | 'workspace';
  workspaceId?: string;
  workspacePath?: string;
  beforeCursor?: string | null;
  canonicalBeforeCursor?: string | null;
  limit?: number;
}): Promise<MessagesResponseDto> {
  const sessionId = options.sessionId.trim();
  const workspaceId = options.workspaceId?.trim() || '';
  if (!sessionId || (options.scope === 'workspace' && !workspaceId && !options.workspacePath?.trim())) {
    throw new AgentApiError(400, '会话作用域不完整', 'load older session messages');
  }
  try {
    const bindingOverride: AgentBindingOverride = options.scope === 'workspace'
      ? {
          scope: 'workspace',
          sessionId,
          workspaceId,
          workspacePath: options.workspacePath?.trim() || '',
        }
      : { scope: 'personal', sessionId };
    const query = buildBoundQueryWithOverride(
      {
        ...(options.limit ? { limit: String(options.limit) } : {}),
        ...(options.beforeCursor ? { beforeCursor: options.beforeCursor } : {}),
        ...(options.canonicalBeforeCursor
          ? { canonicalBeforeCursor: options.canonicalBeforeCursor }
          : {}),
      },
      bindingOverride,
    );
    const response = await getTransport().request(agentUrl('/api/messages', query));
    return await parseAgentJson<MessagesResponseDto>(response, 'load older session messages');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

async function postWorkspaceBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
): Promise<T> {
  return await postJsonWithBinding<T>(pathname, payload, action, bindingOverride, false);
}

async function postGlobalJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
): Promise<T> {
  return await postJsonWithBinding<T>(
    pathname,
    payload,
    action,
    undefined,
    false,
    undefined,
    { includeWorkspaceBinding: false },
  );
}

export async function deleteAgentSession(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<unknown> {
  return await postBoundJson<unknown>(
    '/api/session/delete',
    { sessionId },
    'delete session',
    bindingOverride,
  );
}

export async function markAgentSessionViewed(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<{
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  sessionId: string;
  hasUnreadCompletion: boolean;
}> {
  const result = await postBoundJson<{
    runtimeEpoch: string;
    eventStreamNextSequence: number;
    sessionId: string;
    workspaceId?: string | null;
    hasUnreadCompletion: boolean;
  }>(
    '/api/session/viewed',
    { sessionId },
    'mark session viewed',
    bindingOverride,
  );
  if (
    !result.runtimeEpoch?.trim()
    || !Number.isFinite(result.eventStreamNextSequence)
    || result.eventStreamNextSequence < 1
  ) {
    throw new Error('mark session viewed 缺少有效的运行时代际或事件游标');
  }
  return {
    ...result,
    runtimeEpoch: result.runtimeEpoch.trim(),
    eventStreamNextSequence: Math.floor(result.eventStreamNextSequence),
  };
}

export async function renameAgentSession(
  sessionId: string,
  name: string,
  bindingOverride?: AgentBindingOverride,
): Promise<unknown> {
  return await postBoundJson<unknown>(
    '/api/session/rename',
    { sessionId, name },
    'rename session',
    bindingOverride,
  );
}

export async function closeAgentSession(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<unknown> {
  return await postBoundJson<unknown>(
    '/api/session/close',
    { sessionId },
    'close session',
    bindingOverride,
  );
}

export async function getAgentNotifications(scope: AgentNotificationScope): Promise<AgentNotificationsPayload> {
  const query = buildBoundQueryWithOverride({}, createNotificationBindingOverride(scope));
  const response = await getTransport().request(agentUrl('/api/notifications', query));
  return await parseAgentJson<AgentNotificationsPayload>(response, 'load notifications');
}

export async function reportAgentIncident(
  incident: Record<string, unknown>,
  scope: AgentNotificationScope,
): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/report',
    { ...incident },
    'report incident',
    createNotificationBindingOverride(scope),
  );
}

export async function markAllAgentNotificationsRead(scope: AgentNotificationScope): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/mark-all-read',
    {},
    'mark all notifications read',
    createNotificationBindingOverride(scope),
  );
}

export async function clearAgentNotifications(scope: AgentNotificationScope): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/clear',
    {},
    'clear notifications',
    createNotificationBindingOverride(scope),
  );
}

export async function removeAgentNotification(
  notificationId: string,
  scope: AgentNotificationScope,
): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/remove',
    { notificationId },
    'remove notification',
    createNotificationBindingOverride(scope),
  );
}

export async function resolveAgentNotification(
  notificationId: string,
  scope: AgentNotificationScope,
): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/resolve',
    { notificationId },
    'resolve notification',
    createNotificationBindingOverride(scope),
  );
}

export async function submitSessionTurn(
  payload: {
    text?: string | null;
    skillName?: string | null;
    locale?: string;
    goalMode?: boolean;
    images: AgentSessionTurnImagePayload[];
    contextReferences?: Array<{
      kind: 'file' | 'directory';
      path: string;
      pathRef?: string;
      name: string;
    }>;
    browserAnnotationRefs?: string[];
    accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
    orchestratorSessionConfig?: Record<string, unknown> | null;
    requestId?: string | null;
    userMessageId?: string | null;
    placeholderMessageId?: string | null;
    steerCurrentTurn?: boolean;
    expectedTurnId?: string | null;
    replaceTurnId?: string | null;
  },
  bindingOverride?: AgentBindingOverride,
): Promise<AgentSessionTurnResult> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    const response = await getTransport().request(agentUrl('/api/session/turn'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sessionId: binding.sessionId || null,
        scope: binding.scope,
        workspaceId: binding.scope === 'workspace' ? binding.workspaceId || null : null,
        workspacePath: binding.scope === 'workspace' ? binding.workspacePath || null : null,
        text: payload.text ?? null,
        skillName: payload.skillName ?? null,
        locale: payload.locale ?? i18n.locale,
        goalMode: payload.goalMode === true,
        accessProfile: payload.accessProfile ?? null,
        requestId: payload.requestId ?? null,
        userMessageId: payload.userMessageId ?? null,
        placeholderMessageId: payload.placeholderMessageId ?? null,
        steerCurrentTurn: payload.steerCurrentTurn === true,
        expectedTurnId: payload.expectedTurnId ?? null,
        replaceTurnId: payload.replaceTurnId ?? null,
        images: payload.images.map((image) => ({
          name: image.name,
          dataUrl: image.dataUrl,
        })),
        contextReferences: (payload.contextReferences ?? []).map((reference) => ({
          kind: reference.kind,
          path: reference.path,
          ...(reference.pathRef ? { pathRef: reference.pathRef } : {}),
          name: reference.name,
        })),
        browserAnnotationRefs: (payload.browserAnnotationRefs ?? [])
          .map((annotationId) => annotationId.trim())
          .filter(Boolean),
        orchestratorSessionConfig: payload.orchestratorSessionConfig ?? null,
      }),
    });
    const raw = await parseAgentJson<{
      sessionId: string;
      entryId: string;
      eventId: string;
      acceptedAt: number;
      runtimeEpoch: string;
      eventStreamNextSequence: number;
      createdSession: boolean;
      route: 'chat' | 'execute' | 'task' | 'continue' | 'steer';
      sessionSummary?: RawAgentSessionSummary | null;
      rootTaskId?: string | null;
      actionTaskId?: string | null;
      executionChainRef?: string | null;
      userMessageItemId?: string | null;
      queued?: boolean;
      queueId?: string | null;
      queuePosition?: number | null;
      canonicalSchemaVersion?: string | null;
      canonicalEventKind?: string | null;
      canonicalTurn?: CanonicalTurn | null;
      canonicalItem?: CanonicalTurnItem | null;
      steeredTurnId?: string | null;
    }>(response, 'submit session turn');
    const runtimeEpoch = typeof raw.runtimeEpoch === 'string' ? raw.runtimeEpoch.trim() : '';
    const eventStreamNextSequence = typeof raw.eventStreamNextSequence === 'number'
      && Number.isFinite(raw.eventStreamNextSequence)
      ? Math.floor(raw.eventStreamNextSequence)
      : 0;
    if (!runtimeEpoch || eventStreamNextSequence < 1) {
      throw new Error('submit session turn 缺少有效的运行时代际或事件游标');
    }
    return {
      sessionId: raw.sessionId,
      entryId: raw.entryId,
      eventId: raw.eventId,
      acceptedAt: raw.acceptedAt,
      runtimeEpoch,
      eventStreamNextSequence,
      createdSession: raw.createdSession,
      route: raw.route,
      sessionSummary: raw.sessionSummary ? normalizeSessionSummary(raw.sessionSummary) : null,
      rootTaskId: typeof raw.rootTaskId === 'string' && raw.rootTaskId.trim() ? raw.rootTaskId.trim() : null,
      actionTaskId: typeof raw.actionTaskId === 'string' && raw.actionTaskId.trim()
        ? raw.actionTaskId.trim()
        : null,
      executionChainRef: typeof raw.executionChainRef === 'string' && raw.executionChainRef.trim()
        ? raw.executionChainRef.trim()
        : null,
      userMessageItemId: typeof raw.userMessageItemId === 'string' && raw.userMessageItemId.trim()
        ? raw.userMessageItemId.trim()
        : null,
      queued: raw.queued === true,
      queueId: typeof raw.queueId === 'string' && raw.queueId.trim()
        ? raw.queueId.trim()
        : null,
      queuePosition: typeof raw.queuePosition === 'number' && Number.isFinite(raw.queuePosition)
        ? Math.max(1, Math.floor(raw.queuePosition))
        : null,
      canonicalSchemaVersion: typeof raw.canonicalSchemaVersion === 'string' && raw.canonicalSchemaVersion.trim()
        ? raw.canonicalSchemaVersion.trim()
        : null,
      canonicalEventKind: typeof raw.canonicalEventKind === 'string' && raw.canonicalEventKind.trim()
        ? raw.canonicalEventKind.trim()
        : null,
      canonicalTurn: raw.canonicalTurn ?? null,
      canonicalItem: raw.canonicalItem ?? null,
      steeredTurnId: typeof raw.steeredTurnId === 'string' && raw.steeredTurnId.trim()
        ? raw.steeredTurnId.trim()
        : null,
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentSessionTurnQueue(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'get session turn queue');
  }
  const binding = resolveBindingWithOverride(bindingOverride);
  const query = new URLSearchParams({
    sessionId: normalizedSessionId,
  });
  if (binding.scope === 'workspace') {
    if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
    if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
  }
  const response = await getTransport().request(agentUrl('/api/session/queue', query.toString()));
  return await parseAgentJson<SessionTurnQueueResponseDto>(response, 'get session turn queue');
}

export async function removeAgentSessionTurnQueueItem(
  sessionId: string,
  queueId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  const normalizedSessionId = sessionId.trim();
  const normalizedQueueId = queueId.trim();
  if (!normalizedSessionId || !normalizedQueueId) {
    throw new AgentApiError(400, 'sessionId 和 queueId 不能为空', 'remove session turn queue item');
  }
  return await postBoundJson<SessionTurnQueueResponseDto>(
    '/api/session/queue/remove',
    { sessionId: normalizedSessionId, queueId: normalizedQueueId },
    'remove session turn queue item',
    bindingOverride,
  );
}

export async function guideAgentSessionTurnQueueItem(
  sessionId: string,
  queueId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  const normalizedSessionId = sessionId.trim();
  const normalizedQueueId = queueId.trim();
  if (!normalizedSessionId || !normalizedQueueId) {
    throw new AgentApiError(400, 'sessionId 和 queueId 不能为空', 'guide session turn queue item');
  }
  return await postBoundJson<SessionTurnQueueResponseDto>(
    '/api/session/queue/guide',
    { sessionId: normalizedSessionId, queueId: normalizedQueueId },
    'guide session turn queue item',
    bindingOverride,
  );
}

export async function interruptAgentSession(
  sessionId: string,
): Promise<SessionInterruptResponseDto> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'interrupt session turn');
  }
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort('interrupt_request_timeout'), 10_000);
  try {
    return await postBoundJson<SessionInterruptResponseDto>(
      '/api/session/interrupt',
      { sessionId: normalizedSessionId },
      'interrupt session turn',
      undefined,
      controller.signal,
    );
  } finally {
    window.clearTimeout(timeout);
  }
}

export async function getAgentSettingsBootstrap(
  options: { bootstrapScope?: 'core' | 'full'; accessProfile?: AccessProfile | null } = {},
): Promise<AgentSettingsBootstrapSnapshot> {
  try {
    const query = buildBoundQuery({
      ...(options.bootstrapScope === 'core' ? { bootstrapScope: 'core' } : {}),
      accessProfile: normalizeAccessProfile(options.accessProfile ?? readStoredAccessProfile()),
    });
    const response = await getTransport().request(agentUrl('/api/settings/bootstrap', query));
    const payload = await parseAgentJson<Record<string, unknown>>(response, 'load settings bootstrap');
    return normalizeSettingsBootstrapPayload(payload);
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function loadAgentToolCatalogDiagnostics(
  options: { accessProfile?: AccessProfile | null; refreshEnvironment?: boolean } = {},
): Promise<AgentToolCatalogDiagnosticsSnapshot> {
  try {
    const query = buildBoundQuery({
      includeExternal: 'true',
      includeMcpServers: 'true',
      includeAgentRoles: 'true',
      accessProfile: normalizeAccessProfile(options.accessProfile ?? readStoredAccessProfile()),
      ...(options.refreshEnvironment ? { refreshEnvironment: 'true' } : {}),
    });
    const response = await getTransport().request(agentUrl('/api/tools/catalog', query));
    const payload = await parseAgentJson<Record<string, unknown>>(response, 'load tool catalog diagnostics');
    const rawBuiltinTools = Array.isArray(payload.tools)
      ? payload.tools.filter((tool) => {
          return Boolean(
            tool
              && typeof tool === 'object'
              && !Array.isArray(tool)
              && (tool as Record<string, unknown>).public === true,
          );
        })
      : [];
    return {
      builtinTools: normalizeBuiltinTools(rawBuiltinTools),
      capabilityDependencies: normalizeCapabilityDependencies(payload.runtimeDependencies),
      commandEnvironment: normalizeCommandEnvironment(payload.commandEnvironment),
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentStatus(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/status'));
  return parseAgentJson<Record<string, unknown>>(response, 'get status');
}

export async function resetAgentExecutionStats(): Promise<Record<string, unknown>> {
  try {
    const response = await getTransport().request(agentUrl('/api/settings/stats/reset'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    });
    return await parseAgentJson<Record<string, unknown>>(response, 'reset execution stats');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentExecutionStats(): Promise<AgentExecutionStatsPayload> {
  try {
    const response = await getTransport().request(agentUrl('/api/settings/stats'));
    return await parseAgentJson<AgentExecutionStatsPayload>(response, 'load execution stats');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function enhanceAgentPrompt(
  request: EnhancePromptRequestDto,
  signal?: AbortSignal,
): Promise<{ enhancedPrompt: string; error?: string }> {
  return await postBoundJson<{ enhancedPrompt: string; error?: string }>(
    '/api/prompt/enhance',
    request,
    'enhance prompt',
    undefined,
    signal,
  );
}

export async function generateSessionSuggestions(
  request: GenerateSessionSuggestionsRequestDto = {},
  signal?: AbortSignal,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionSuggestionsResponseDto> {
  return await postBoundJson<SessionSuggestionsResponseDto>(
    '/api/prompt/suggestions',
    request,
    'generate session suggestions',
    bindingOverride,
    signal,
  );
}

export interface WorkspaceBranchesResult {
  isRepo: boolean;
  currentBranch: string | null;
  branches: string[];
  remoteBranches: string[];
  structuredBranches: GitBranch[];
  status: WorkspaceVcsStatus | null;
  observation?: GitObservation | null;
  sessionContext?: SessionCodeContext | null;
  contextDrift?: boolean;
}

export interface GitBranch {
  name: string;
  fullRef: string;
  isRemote: boolean;
  isCurrent: boolean;
  head: string | null;
  upstream: string | null;
  worktreePath: string | null;
}

export interface GitWorktree {
  path: string;
  head: string | null;
  branch: string | null;
  bare: boolean;
  detached: boolean;
  locked: boolean;
  prunable: boolean;
  managed: boolean;
}

export interface GitMergePreview {
  target: string;
  targetHead: string;
  mergeBase: string | null;
  fastForward: boolean;
  alreadyUpToDate: boolean;
  incomingCommitCount: number;
  changedPaths: string[];
}

export interface GitObservation {
  repositoryRoot: string;
  gitCommonDir: string;
  worktreePath: string;
  worktreeGitDir: string;
  branch: string | null;
  head: string | null;
  upstream?: string | null;
}

export interface SessionCodeContext {
  sessionId: string;
  workspaceId: string;
  executionRoot: string;
  runtimeWorkspaceRoots: string[];
  contextRevision: number;
  git: {
    desiredRef: string | null;
    baseHead: string | null;
    observedBranch: string | null;
    observedHead: string | null;
    worktreePath: string;
  };
}

export interface WorkspaceVcsStatus {
  upstream?: string | null;
  ahead: number;
  behind: number;
  hasUncommitted: boolean;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  renamed: number;
  deleted: number;
  additions: number;
  deletions: number;
}

export async function fetchWorkspaceBranches(
  bindingOverride?: AgentBindingOverride,
): Promise<WorkspaceBranchesResult> {
  return await postWorkspacePathBoundJson<WorkspaceBranchesResult>('/api/workspace/vcs/branches', { includeRemote: true }, 'fetch workspace branches', bindingOverride);
}

interface GitExpectedContext {
  contextRevision?: number;
  branch?: string | null;
  head?: string | null;
  worktreePath?: string | null;
}

function gitExpectedPayload(expected?: GitExpectedContext): Record<string, unknown> {
  return {
    ...(typeof expected?.contextRevision === 'number' ? { expectedContextRevision: expected.contextRevision } : {}),
    ...(expected?.branch ? { expectedBranch: expected.branch } : {}),
    ...(expected?.head ? { expectedHead: expected.head } : {}),
    ...(expected?.worktreePath ? { expectedWorktreePath: expected.worktreePath } : {}),
  };
}

export async function checkoutWorkspaceBranch(
  branch: string,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/branch/switch', {
    branch,
    ...gitExpectedPayload(expected),
  }, 'switch workspace branch', bindingOverride);
}

export interface GitOperationResult {
  ok: boolean;
  observation?: GitObservation | null;
  sessionContext?: SessionCodeContext | null;
  data?: unknown;
  error?: {
    kind: string;
    message: string;
    actualBranch?: string | null;
    actualHead?: string | null;
    actualWorktreePath?: string | null;
    conflictedPaths?: string[];
  };
}

export async function createWorkspaceBranch(
  branch: string,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/branch/create', {
    branch,
    switch: true,
    ...gitExpectedPayload(expected),
  }, 'create workspace branch', bindingOverride);
}

export async function previewWorkspaceMerge(
  target: string,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/merge/preview', {
    target,
    ...gitExpectedPayload(expected),
  }, 'preview workspace merge', bindingOverride);
}

export async function mergeWorkspaceBranch(
  target: string,
  ffOnly: boolean,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/merge', {
    target,
    ffOnly,
    confirm: true,
    ...gitExpectedPayload(expected),
  }, 'merge workspace branch', bindingOverride);
}

export async function deleteWorkspaceBranch(
  branch: string,
  options: { remote?: string; force?: boolean; confirmForce?: boolean; confirmRemote?: boolean },
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/branch/delete', {
    branch,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'delete workspace branch', bindingOverride);
}

export async function fetchWorkspaceWorktrees(
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/list', {}, 'list workspace worktrees', bindingOverride);
}

export async function createWorkspaceWorktree(
  mode: 'readOnly' | 'writable',
  options: { base?: string; branch?: string; allocationKey?: string },
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/create', {
    mode,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'create workspace worktree', bindingOverride);
}

export async function removeWorkspaceWorktree(
  path: string,
  options: { force?: boolean; confirmForce?: boolean },
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/remove', {
    path,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'remove workspace worktree', bindingOverride);
}

export async function acceptWorkspaceGitContext(
  expectedContextRevision: number | null,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/context/accept', {
    ...(typeof expectedContextRevision === 'number'
      ? { expectedContextRevision }
      : {}),
  }, 'accept workspace git context', bindingOverride);
}

export async function updateAgentRuntimeSetting(key: string, value: unknown): Promise<AgentRuntimeSettings> {
  const payload = await postGlobalJson<AgentRuntimeSettings>('/api/settings/update', { key, value }, 'update runtime setting');
  if (key === 'locale' && (payload?.locale === 'zh-CN' || payload?.locale === 'en-US')) {
    safeWriteLocalStorage('magi-locale', payload.locale);
    i18n.setLocale(payload.locale);
  }
  return payload;
}

export async function saveAgentWorkerConfig(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/worker/save', { worker, config }, 'save worker config');
}

export async function saveAgentUserRules(data: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/user-rules/save', data, 'save user rules');
}

export async function saveAgentOrchestratorConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/orchestrator/save', config, 'save orchestrator config');
}

export async function saveAgentOrchestratorSessionConfig(
  config: Record<string, unknown>,
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const binding = resolveBindingWithOverride(bindingOverride);
  return await postBoundJson<Record<string, unknown>>(
    '/api/settings/orchestrator/session/save',
    {
      scope: binding.scope,
      config,
    },
    'save session orchestrator config',
    bindingOverride,
  );
}

export async function saveAgentModelContextWindow(
  model: string,
  contextWindowTokens: number,
): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>(
    '/api/settings/model-context-window/save',
    { model, contextWindowTokens },
    'save model context window',
  );
}

export async function saveAgentAuxiliaryConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/auxiliary/save', config, 'save auxiliary config');
}

export async function saveAgentVisionConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/vision/save', config, 'save vision config');
}

export async function saveAgentImageGenerationConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/image-generation/save', config, 'save image generation config');
}

export async function removeAgentWorkerConfig(worker: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/worker/remove', { worker }, 'remove worker config');
}

export async function testAgentWorkerConnection(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/worker/test', { worker, config }, 'test worker connection');
}

export async function testAgentOrchestratorConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/orchestrator/test', config, 'test orchestrator connection');
}

export async function testAgentAuxiliaryConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/auxiliary/test', config, 'test auxiliary connection');
}

export async function testAgentVisionConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/vision/test', config, 'test vision connection');
}

export async function testAgentImageGenerationConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/image-generation/test', config, 'test image generation connection');
}


export async function listAgentRoleTemplates(): Promise<RoleTemplate[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/role-templates'));
  const payload = await parseAgentJson<{ templates?: RoleTemplate[] }>(response, 'load role templates');
  return Array.isArray(payload.templates) ? payload.templates : [];
}

export async function listAgentRegistryEngines(): Promise<ModelEngine[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/engines'));
  const payload = await parseAgentJson<{ engines?: ModelEngine[] }>(response, 'load registry engines');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function listAgentRegistryAgents(): Promise<AgentBinding[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/agents'));
  const payload = await parseAgentJson<{ agents?: AgentBinding[] }>(response, 'load registry agents');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function upsertAgentRegistryEngine(engine: ModelEngine): Promise<ModelEngine[]> {
  const payload = await postGlobalJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/upsert', engine as unknown as Record<string, unknown>, 'upsert registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function removeAgentRegistryEngine(engineId: string): Promise<ModelEngine[]> {
  const payload = await postGlobalJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/remove', { engineId }, 'remove registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function upsertAgentRegistryBinding(agent: AgentBinding): Promise<AgentBinding[]> {
  const payload = await postGlobalJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/upsert', agent as unknown as Record<string, unknown>, 'upsert registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function removeAgentRegistryBinding(templateId: string): Promise<AgentBinding[]> {
  const payload = await postGlobalJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/remove', { templateId }, 'remove registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function fetchAgentModelList(config: Record<string, unknown>, target: string): Promise<FetchModelsResponseDto> {
  return await postGlobalJson<FetchModelsResponseDto>('/api/settings/models/fetch', { config, target }, 'fetch model list');
}

export async function clearAgentProjectKnowledge(
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/clear',
    {},
    'clear project knowledge',
    bindingOverride,
  );
}

export async function getAgentProjectKnowledge(
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const query = buildBoundQueryWithOverride({}, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load project knowledge');
}

export interface AgentKnowledgeGraphQuery {
  focus?: string;
  depth?: number;
  direction?: 'forward' | 'reverse' | 'both';
  nodeKinds?: string[];
  edgeKinds?: string[];
  maxNodes?: number;
  maxEdges?: number;
}

/** 按节点重新加载知识图谱局部视图，避免前端从首屏全量图中做假聚焦。 */
export async function getAgentKnowledgeGraph(
  graphQuery: AgentKnowledgeGraphQuery = {},
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const params: Record<string, string> = {};
  if (graphQuery.focus?.trim()) params.focus = graphQuery.focus.trim();
  if (typeof graphQuery.depth === 'number' && Number.isFinite(graphQuery.depth)) params.depth = String(Math.trunc(graphQuery.depth));
  if (graphQuery.direction) params.direction = graphQuery.direction;
  if (graphQuery.nodeKinds?.length) params.nodeKinds = graphQuery.nodeKinds.join(',');
  if (graphQuery.edgeKinds?.length) params.edgeKinds = graphQuery.edgeKinds.join(',');
  if (typeof graphQuery.maxNodes === 'number' && Number.isFinite(graphQuery.maxNodes)) params.maxNodes = String(Math.trunc(graphQuery.maxNodes));
  if (typeof graphQuery.maxEdges === 'number' && Number.isFinite(graphQuery.maxEdges)) params.maxEdges = String(Math.trunc(graphQuery.maxEdges));
  const query = buildBoundQueryWithOverride(params, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/graph', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load knowledge graph');
}

export async function reindexAgentProjectKnowledge(
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/knowledge/reindex',
    {},
    'reindex project knowledge',
    bindingOverride,
  );
}

export type AgentKnowledgeKind = 'adr' | 'faq' | 'learning';

export interface AgentKnowledgeItemPayload {
  kind: AgentKnowledgeKind;
  title?: string;
  content: string;
  tags?: string[];
  context?: string;
}

export interface AgentKnowledgeItemPatch {
  title?: string;
  content?: string;
  tags?: string[];
  context?: string;
}

export type AgentGraphNodeRef =
  | { kind: 'knowledge'; knowledgeId: string }
  | { kind: 'file'; path: string }
  | { kind: 'symbol'; path: string; qualifiedName: string; symbolKind: string };

export type AgentKnowledgeRelationKind =
  | 'applies_to'
  | 'explains'
  | 'references'
  | 'related_to'
  | 'supersedes'
  | 'contradicts';

export type AgentKnowledgeRelationStatus = 'active' | 'candidate' | 'dangling' | 'rejected';

export interface AgentKnowledgeRelation {
  relationId: string;
  workspaceId: string;
  source: AgentGraphNodeRef;
  kind: AgentKnowledgeRelationKind;
  target: AgentGraphNodeRef;
  origin: 'deterministic_code' | 'explicit_user' | 'explicit_agent' | 'inferred';
  confidence?: number;
  status: AgentKnowledgeRelationStatus;
  evidence: string[];
  discoveryKey?: string;
  discoveryEvidence?: string[];
  reviewedAt?: number;
  createdAt?: number;
  updatedAt?: number;
}

export interface AgentKnowledgeRelationDraft {
  relationId?: string;
  source: AgentGraphNodeRef;
  kind: AgentKnowledgeRelationKind;
  target: AgentGraphNodeRef;
  origin?: AgentKnowledgeRelation['origin'];
  confidence?: number;
  status?: AgentKnowledgeRelationStatus;
  evidence?: string[];
}

export interface AgentKnowledgeRelationsResponse {
  relations: AgentKnowledgeRelation[];
  totalRelations: number;
  truncated: boolean;
}

export async function listAgentKnowledgeItems(
  kind?: AgentKnowledgeKind,
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const params: Record<string, string> = {};
  if (kind) params.kind = kind;
  const query = buildBoundQueryWithOverride(params, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/items', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'list knowledge items');
}

export async function searchAgentKnowledgeItems(
  keyword: string,
  kind?: AgentKnowledgeKind,
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const params: Record<string, string> = { q: keyword };
  if (kind) params.kind = kind;
  const query = buildBoundQueryWithOverride(params, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/items/search', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'search knowledge items');
}

export async function addAgentKnowledgeItem(
  payload: AgentKnowledgeItemPayload,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items',
    { ...payload },
    'add knowledge item',
    bindingOverride,
  );
}

export async function updateAgentKnowledgeItem(
  knowledgeId: string,
  patch: AgentKnowledgeItemPatch,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items/update',
    { knowledgeId, ...patch },
    'update knowledge item',
    bindingOverride,
  );
}

export async function deleteAgentKnowledgeItem(
  knowledgeId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items/delete',
    { knowledgeId },
    'delete knowledge item',
    bindingOverride,
  );
}

export async function listAgentKnowledgeRelations(
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeRelationsResponse> {
  const query = buildBoundQueryWithOverride({}, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/relations', query));
  const payload = await parseAgentJson<Partial<AgentKnowledgeRelationsResponse>>(response, 'list knowledge relations');
  const relations = Array.isArray(payload.relations)
    ? payload.relations.map((relation) => ({
        ...relation,
        evidence: Array.isArray(relation.evidence) ? relation.evidence : [],
      }))
    : [];
  const totalRelations = typeof payload.totalRelations === 'number' && Number.isFinite(payload.totalRelations)
    ? Math.max(relations.length, Math.trunc(payload.totalRelations))
    : relations.length;
  return {
    relations,
    totalRelations,
    truncated: payload.truncated === true || totalRelations > relations.length,
  };
}

export async function addAgentKnowledgeRelation(
  relation: AgentKnowledgeRelationDraft,
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeRelation> {
  const payload = await postWorkspaceBoundJson<{ relation?: AgentKnowledgeRelation }>(
    '/api/knowledge/relations',
    relation as unknown as Record<string, unknown>,
    'add knowledge relation',
    bindingOverride,
  );
  if (!payload.relation) {
    throw new Error('知识关系创建响应缺少 relation');
  }
  return payload.relation;
}

export async function updateAgentKnowledgeRelation(
  relation: AgentKnowledgeRelationDraft & { relationId: string },
  bindingOverride?: AgentBindingOverride,
): Promise<AgentKnowledgeRelation> {
  const payload = await postWorkspaceBoundJson<{ relation?: AgentKnowledgeRelation }>(
    '/api/knowledge/relations/update',
    relation as unknown as Record<string, unknown>,
    'update knowledge relation',
    bindingOverride,
  );
  if (!payload.relation) {
    throw new Error('知识关系更新响应缺少 relation');
  }
  return payload.relation;
}

export async function deleteAgentKnowledgeRelation(
  relationId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<void> {
  await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/knowledge/relations/delete',
    { relationId },
    'delete knowledge relation',
    bindingOverride,
  );
}

export async function loadAgentMcpServers(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/mcp'));
  return await parseAgentJson<Record<string, unknown>>(response, 'load mcp servers');
}

export async function addAgentMcpServer(server: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/add', normalizeMcpServerConfig(server), 'add mcp server');
}

export async function updateAgentMcpServer(serverId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>(
    '/api/settings/mcp/update',
    normalizeMcpServerConfig({ ...updates, id: serverId, serverId }),
    'update mcp server',
  );
}

export async function deleteAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/delete', { serverId }, 'delete mcp server');
}

export async function getAgentMcpServerTools(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/tools', { serverId }, 'get mcp server tools');
}

export async function refreshAgentMcpTools(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/tools/refresh', { serverId }, 'refresh mcp tools');
}

export async function connectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/connect', { serverId }, 'connect mcp server');
}

export async function disconnectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/disconnect', { serverId }, 'disconnect mcp server');
}

export async function loadAgentRepositories(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/repositories'));
  return await parseAgentJson<Record<string, unknown>>(response, 'load repositories');
}

export async function addAgentRepository(url: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/add', { url }, 'add repository');
}

export async function updateAgentRepository(repositoryId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/update', { repositoryId, updates }, 'update repository');
}

export async function deleteAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/delete', { repositoryId }, 'delete repository');
}

export async function refreshAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/refresh', { repositoryId }, 'refresh repository');
}

export async function loadAgentSkillLibrary(): Promise<SkillsLibraryResponseDto> {
  const response = await getTransport().request(agentUrl('/api/settings/skills/library'));
  return await parseAgentJson<SkillsLibraryResponseDto>(response, 'load skill library');
}

export async function installAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/install', { skillId }, 'install skill');
}

export interface AgentLocalSkillInstallRequest {
  directoryPath?: string;
  skillId?: string;
}

export async function installAgentLocalSkill(
  request?: string | AgentLocalSkillInstallRequest,
): Promise<Record<string, unknown>> {
  const payload: Record<string, unknown> = {};
  if (typeof request === 'string') {
    payload.directoryPath = request;
  } else if (request) {
    if (request.directoryPath) {
      payload.directoryPath = request.directoryPath;
    }
    if (request.skillId) {
      payload.skillId = request.skillId;
    }
  }
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/install-local', payload, 'install local skill');
}

export async function scanAgentLocalSkillDirectory(directoryPath: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/scan-local', { directoryPath }, 'scan local skill directory');
}

export async function saveAgentSkillsConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/config/save', config, 'save skills config');
}

export async function toggleAgentSkill(skillId: string, enabled: boolean): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>(
    '/api/settings/skills/toggle',
    { skillId, enabled },
    'toggle skill',
  );
}

export async function saveAgentSafeguardConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/safeguard/save', config, 'save safeguard config');
}

export async function addAgentCustomTool(tool: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/custom-tool/add', tool, 'add custom tool');
}

export type AgentSkillSource = 'custom' | 'instruction';

export async function removeAgentInstalledSkill(skillId: string, source: AgentSkillSource): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/remove', { skillId, source }, 'remove installed skill');
}

export async function updateAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/update', { skillId }, 'update skill');
}

export async function updateAllAgentSkills(): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/update-all', {}, 'update all skills');
}

export async function checkAgentSkillUpdates(): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/check-updates', {}, 'check skill updates');
}

export async function rollbackAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/rollback', { skillId }, 'rollback skill');
}

export interface AgentPendingChangesPayload {
  generatedAt: number;
  sessionId: string;
  workspaceId: string;
  workspacePath: string;
  pendingChanges: unknown[];
  pendingChangesState: unknown;
}

export async function getAgentPendingChanges(
  options: WorkspaceAgentBindingOverride & {
    forceRefresh?: boolean;
  },
): Promise<AgentPendingChangesPayload> {
  try {
    const query = buildBoundQueryWithOverride(
      options.forceRefresh ? { forceRefresh: 'true' } : {},
      options,
      { includeScope: false },
    );
    const response = await getTransport().request(agentUrl('/api/changes', query));
    return await parseAgentJson<AgentPendingChangesPayload>(response, 'load pending changes');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentChangeDiff(
  filePath: string,
  options: WorkspaceAgentBindingOverride,
): Promise<AgentChangeDiffPayload> {
  try {
    const query = buildBoundQueryWithOverride({ filePath }, options, { includeScope: false });
    const response = await getTransport().request(agentUrl(`/api/changes/diff`, query));
    return await parseAgentJson<AgentChangeDiffPayload>(response, 'load change diff');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentFilePreview(
  filePath: string,
  options: { includeSession?: boolean; sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<AgentFilePreviewPayload> {
  try {
    const query = buildFilePreviewQuery(filePath, options);
    const response = await getTransport().request(agentUrl(`/api/files/content`, query));
    return await parseAgentJson<AgentFilePreviewPayload>(response, 'load file preview');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function approveAgentChange(
  filePath: string,
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson('/api/changes/approve', { filePath }, 'approve change', options);
}

export async function revertAgentChange(
  filePath: string,
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson('/api/changes/revert', { filePath }, 'revert change', options);
}

export async function approveAllAgentChanges(
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson('/api/changes/approve-all', {}, 'approve all changes', options);
}

export async function revertAllAgentChanges(
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson('/api/changes/revert-all', {}, 'revert all changes', options);
}

export async function revertAgentExecutionGroupChanges(
  executionGroupId: string,
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson(
    '/api/changes/revert-execution-group',
    { executionGroupId },
    'revert execution group changes',
    options,
  );
}
