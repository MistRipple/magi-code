import { getDefaultAgentBaseUrl } from '../shared/agent-shared-config';
import { getTransport } from '../shared/transport';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
import type { RoleTemplate } from '../shared/types/role-templates';
import type {
  SettingsBootstrapPayload,
  SettingsBuiltinTool,
  SettingsCapabilityDependency,
  SettingsRuntimeSnapshot,
} from '../shared/settings-bootstrap';
import type {
  IncidentNotificationItemDto,
  NotificationCenterSnapshotDto,
  NotificationsResponseDto,
  FetchModelsResponseDto,
  EnhancePromptRequestDto,
  SkillsLibraryResponseDto,
} from '../shared/rust-backend-types';
import type { CanonicalTurn, CanonicalTurnItem } from '../shared/protocol/canonical-turn';
import { i18n } from '../stores/i18n.svelte';
import {
  resolveAgentBindingContext,
  type AgentBindingContext,
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

export interface AgentBootstrapSnapshot {
  workspace: AgentWorkspaceSummary;
  sessionId: string;
  session: AgentSessionSummary;
  notifications?: AgentNotificationsPayload;
  sessions: AgentSessionSummary[];
}

export interface AgentWorkspaceSessionsSnapshot {
  workspace: AgentWorkspaceSummary;
  sessionId: string;
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

function normalizeSettingsBootstrapPayload(
  payload: Record<string, unknown>,
): AgentSettingsBootstrapSnapshot {
  const runtimeSettings = (
    payload.runtimeSettings
    && typeof payload.runtimeSettings === 'object'
    && !Array.isArray(payload.runtimeSettings)
      ? payload.runtimeSettings
      : { locale: 'zh-CN' }
  ) as SettingsRuntimeSnapshot;

  return {
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
    orchestratorSessionConfig: normalizeSettingsSectionConfig(payload.orchestratorSessionConfig),
    effectiveOrchestratorConfig: normalizeSettingsSectionConfig(payload.effectiveOrchestratorConfig),
    auxiliaryConfig: normalizeSettingsSectionConfig(payload.auxiliaryConfig),
    imageGenerationConfig: normalizeSettingsSectionConfig(payload.imageGenerationConfig),
    userRulesConfig: normalizeSettingsSectionConfig(payload.userRulesConfig),
    skillsConfig: normalizeSettingsSectionConfig(payload.skillsConfig),
    safeguardConfig: normalizeSettingsSectionConfig(payload.safeguardConfig),
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
  snapshot: Pick<SettingsBootstrapPayload, 'workspaceId' | 'workspacePath' | 'sessionId'> | null | undefined,
): boolean {
  if (!snapshot) return false;
  const binding = resolveAgentBindingContext();
  const snapshotWorkspaceId = normalizeBindingString(snapshot.workspaceId);
  const snapshotWorkspacePath = normalizeBindingString(snapshot.workspacePath);
  const snapshotSessionId = normalizeBindingString(snapshot.sessionId);
  if (snapshotSessionId !== binding.sessionId) {
    return false;
  }
  if (snapshotWorkspaceId || binding.workspaceId) {
    return Boolean(snapshotWorkspaceId)
      && Boolean(binding.workspaceId)
      && snapshotWorkspaceId === binding.workspaceId;
  }
  return snapshotWorkspacePath === binding.workspacePath;
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
  createdSession: boolean;
  route: 'chat' | 'execute' | 'task' | 'continue' | 'steer';
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
  readonly conflictKind?: string;
  readonly activeTurnId?: string;

  constructor(
    status: number,
    message: string,
    action: string,
    errorCode?: string,
    conflictKind?: string,
    activeTurnId?: string,
  ) {
    super(message);
    this.name = 'AgentApiError';
    this.status = status;
    this.action = action;
    this.errorCode = errorCode;
    this.conflictKind = conflictKind;
    this.activeTurnId = activeTurnId;
  }
}

export interface AgentNotificationScope {
  workspaceId: string;
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

/** 构造可直接交给 iframe / 新窗口导航的 URL，并显式携带公网传输凭据。 */
export function agentNavigationUrl(pathname: string, query?: string): string {
  const url = new URL(agentUrl(pathname, query));
  if (typeof window !== 'undefined') {
    const tunnelToken = new URL(window.location.href).searchParams.get('tunnel_token')?.trim();
    if (tunnelToken && !url.searchParams.has('tunnel_token')) {
      url.searchParams.set('tunnel_token', tunnelToken);
    }
  }
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
  options: { includeSession?: boolean } = {},
): string {
  return buildBoundQueryWithOverride(extra, undefined, options);
}

export function buildWorkspaceBoundQuery(extra: Record<string, string>): string {
  return buildBoundQuery(extra, { includeSession: false });
}

export function buildFilePreviewQuery(
  filePath: string,
  options: { includeSession?: boolean; sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): string {
  const explicitSessionId = typeof options.sessionId === 'string' && options.sessionId.trim().length > 0;
  return buildBoundQueryWithOverride(
    { filePath },
    {
      sessionId: options.sessionId,
      workspaceId: options.workspaceId,
      workspacePath: options.workspacePath,
    },
    { includeSession: options.includeSession === true || explicitSessionId },
  );
}

function resolveBindingOverrideValue(
  bindingOverride: Partial<AgentBindingContext> | undefined,
  key: keyof AgentBindingContext,
  fallback: string,
): string {
  if (bindingOverride && Object.prototype.hasOwnProperty.call(bindingOverride, key)) {
    const value = bindingOverride[key];
    if (typeof value !== 'string') {
      return fallback;
    }
    const trimmed = value.trim();
    if (key === 'sessionId') {
      return trimmed;
    }
    return trimmed || fallback;
  }
  return fallback;
}

function hasBindingOverrideKey(
  bindingOverride: Partial<AgentBindingContext> | undefined,
  key: keyof AgentBindingContext,
): boolean {
  if (!bindingOverride || !Object.prototype.hasOwnProperty.call(bindingOverride, key)) {
    return false;
  }
  const value = bindingOverride[key];
  if (value === undefined) {
    return false;
  }
  if (key !== 'sessionId' && typeof value === 'string' && value.trim() === '') {
    return false;
  }
  return true;
}

function resolveBindingWithOverride(
  bindingOverride?: Partial<AgentBindingContext>,
): AgentBindingContext {
  const resolvedBinding = resolveAgentBindingContext();
  const hasWorkspaceId = hasBindingOverrideKey(bindingOverride, 'workspaceId');
  const hasWorkspacePath = hasBindingOverrideKey(bindingOverride, 'workspacePath');
  const hasWorkspaceOverride = hasWorkspaceId || hasWorkspacePath;
  return {
    workspaceId: hasWorkspaceId
      ? resolveBindingOverrideValue(bindingOverride, 'workspaceId', '')
      : (hasWorkspaceOverride ? '' : resolvedBinding.workspaceId),
    workspacePath: hasWorkspacePath
      ? resolveBindingOverrideValue(bindingOverride, 'workspacePath', '')
      : (hasWorkspaceOverride ? '' : resolvedBinding.workspacePath),
    sessionId: resolveBindingOverrideValue(
      bindingOverride,
      'sessionId',
      hasWorkspaceOverride ? '' : resolvedBinding.sessionId,
    ),
  };
}

function buildBoundQueryWithOverride(
  extra: Record<string, string>,
  bindingOverride?: Partial<AgentBindingContext>,
  options: { includeSession?: boolean } = {},
): string {
  const binding = resolveBindingWithOverride(bindingOverride);
  const query = new URLSearchParams();
  if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
  if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
  if (options.includeSession !== false && binding.sessionId) query.set('sessionId', binding.sessionId);
  for (const [key, value] of Object.entries(extra)) {
    if (value) {
      query.set(key, value);
    }
  }
  return query.toString();
}

function createNotificationBindingOverride(scope: AgentNotificationScope): Partial<AgentBindingContext> {
  const sessionId = scope.sessionId?.trim() || '';
  const workspaceId = scope.workspaceId.trim();
  if (!workspaceId) {
    throw new AgentApiError(400, 'workspaceId 不能为空', 'resolve notification scope');
  }
  return {
    sessionId,
    workspaceId,
    workspacePath: scope.workspacePath?.trim() || '',
  };
}

async function postJsonWithBinding<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: Partial<AgentBindingContext>,
  includeSession = true,
): Promise<T> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    const bindingPayload = {
      ...(binding.workspaceId ? { workspaceId: binding.workspaceId } : {}),
      ...(binding.workspacePath ? { workspacePath: binding.workspacePath } : {}),
      ...(includeSession && binding.sessionId ? { sessionId: binding.sessionId } : {}),
    };
    const response = await getTransport().request(agentUrl(pathname), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<T> {
  return await postJsonWithBinding<T>(pathname, payload, action, bindingOverride, true);
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

export async function removeAgentWorkspace(workspaceId: string, workspacePath: string): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/remove'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaceId,
        workspacePath,
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
  );
}

export async function getWorkspaceSessions(
  workspaceId: string,
  preferredSessionId?: string,
  workspacePath?: string,
): Promise<AgentWorkspaceSessionsSnapshot> {
  try {
    const query = new URLSearchParams({ workspaceId });
    if (workspacePath?.trim()) {
      query.set('workspacePath', workspacePath.trim());
    }
    if (preferredSessionId && preferredSessionId.trim()) {
      query.set('sessionId', preferredSessionId.trim());
    }
    const response = await getTransport().request(
      agentUrl('/api/workspaces/sessions', query.toString())
    );
    const payload = await parseAgentJson<{
      workspace?: RawAgentWorkspaceSummary;
      sessionId?: string;
      sessions?: RawAgentSessionSummary[];
    }>(response, 'workspace sessions');
    const sessions = Array.isArray(payload.sessions)
      ? payload.sessions.map((session) => normalizeSessionSummary(session))
      : [];
    return {
      workspace: payload.workspace
        ? normalizeWorkspaceSummary(payload.workspace)
        : findCachedWorkspaceSummary(workspaceId),
      sessionId: payload.sessionId?.trim() || '',
      sessions,
    };
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<T> {
  return await postJsonWithBinding<T>(pathname, payload, action, bindingOverride, false);
}

export async function deleteAgentSession(
  sessionId: string,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>(
    '/api/session/delete',
    { sessionId },
    'delete session',
    bindingOverride,
  );
}

export async function markAgentSessionViewed(
  sessionId: string,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<{ sessionId: string; hasUnreadCompletion: boolean }> {
  return await postWorkspaceBoundJson<{ sessionId: string; hasUnreadCompletion: boolean }>(
    '/api/workspaces/sessions/viewed',
    { sessionId },
    'mark session viewed',
    bindingOverride,
  );
}

export async function renameAgentSession(
  sessionId: string,
  name: string,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>(
    '/api/session/rename',
    { sessionId, name },
    'rename session',
    bindingOverride,
  );
}

export async function closeAgentSession(
  sessionId: string,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>(
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
    accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
    orchestratorSessionConfig?: Record<string, unknown> | null;
    requestId?: string | null;
    userMessageId?: string | null;
    placeholderMessageId?: string | null;
    steerCurrentTurn?: boolean;
    expectedTurnId?: string | null;
    replaceTurnId?: string | null;
  },
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentSessionTurnResult> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    if (!binding.workspaceId) {
      throw new AgentApiError(400, 'workspaceId 不能为空', 'submit session turn');
    }
    const response = await getTransport().request(agentUrl('/api/session/turn'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sessionId: binding.sessionId || null,
        workspaceId: binding.workspaceId || null,
        workspacePath: binding.workspacePath || null,
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
        orchestratorSessionConfig: payload.orchestratorSessionConfig ?? null,
      }),
    });
    const raw = await parseAgentJson<{
      sessionId: string;
      entryId: string;
      eventId: string;
      acceptedAt: number;
      createdSession: boolean;
      route: 'chat' | 'execute' | 'task' | 'continue' | 'steer';
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
    return {
      sessionId: raw.sessionId,
      entryId: raw.entryId,
      eventId: raw.eventId,
      acceptedAt: raw.acceptedAt,
      createdSession: raw.createdSession,
      route: raw.route,
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

export async function interruptAgentTask(
  payload: { taskId: string },
): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/agent-runs/interrupt', payload, 'interrupt agent run');
}

export async function interruptAgentSession(
  sessionId: string,
): Promise<Record<string, unknown>> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'interrupt session turn');
  }
  return await postBoundJson<Record<string, unknown>>(
    '/api/session/interrupt',
    { sessionId: normalizedSessionId },
    'interrupt session turn',
  );
}

export async function continueAgentSession(
  sessionId: string,
): Promise<Record<string, unknown>> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'continue session');
  }
  return await postBoundJson<Record<string, unknown>>(
    '/api/session/continue',
    { sessionId: normalizedSessionId },
    'continue session',
  );
}

export async function getAgentSettingsBootstrap(
  options: { scope?: 'core' | 'full'; accessProfile?: AccessProfile | null } = {},
): Promise<AgentSettingsBootstrapSnapshot> {
  try {
    const query = buildBoundQuery({
      ...(options.scope === 'core' ? { scope: 'core' } : {}),
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

export async function enhanceAgentPrompt(request: EnhancePromptRequestDto): Promise<{ enhancedPrompt: string; error?: string }> {
  return await postBoundJson<{ enhancedPrompt: string; error?: string }>('/api/prompt/enhance', request, 'enhance prompt');
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<WorkspaceBranchesResult> {
  return await postBoundJson<WorkspaceBranchesResult>('/api/workspace/vcs/branches', { includeRemote: true }, 'fetch workspace branches', bindingOverride);
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/branch/switch', {
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/branch/create', {
    branch,
    switch: true,
    ...gitExpectedPayload(expected),
  }, 'create workspace branch', bindingOverride);
}

export async function previewWorkspaceMerge(
  target: string,
  expected?: GitExpectedContext,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/merge/preview', {
    target,
    ...gitExpectedPayload(expected),
  }, 'preview workspace merge', bindingOverride);
}

export async function mergeWorkspaceBranch(
  target: string,
  ffOnly: boolean,
  expected?: GitExpectedContext,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/merge', {
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/branch/delete', {
    branch,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'delete workspace branch', bindingOverride);
}

export async function fetchWorkspaceWorktrees(
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/list', {}, 'list workspace worktrees', bindingOverride);
}

export async function createWorkspaceWorktree(
  mode: 'readOnly' | 'writable',
  options: { base?: string; branch?: string; allocationKey?: string },
  expected?: GitExpectedContext,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/create', {
    mode,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'create workspace worktree', bindingOverride);
}

export async function removeWorkspaceWorktree(
  path: string,
  options: { force?: boolean; confirmForce?: boolean },
  expected?: GitExpectedContext,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/remove', {
    path,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'remove workspace worktree', bindingOverride);
}

export async function acceptWorkspaceGitContext(
  expectedContextRevision: number | null,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<GitOperationResult> {
  return await postBoundJson<GitOperationResult>('/api/workspace/vcs/context/accept', {
    ...(typeof expectedContextRevision === 'number'
      ? { expectedContextRevision }
      : {}),
  }, 'accept workspace git context', bindingOverride);
}

export async function updateAgentRuntimeSetting(key: string, value: unknown): Promise<AgentRuntimeSettings> {
  const payload = await postWorkspaceBoundJson<AgentRuntimeSettings>('/api/settings/update', { key, value }, 'update runtime setting');
  if (key === 'locale' && (payload?.locale === 'zh-CN' || payload?.locale === 'en-US')) {
    safeWriteLocalStorage('magi-locale', payload.locale);
    i18n.setLocale(payload.locale);
  }
  return payload;
}

export async function saveAgentWorkerConfig(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/worker/save', { worker, config }, 'save worker config');
}

export async function saveAgentUserRules(data: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/user-rules/save', data, 'save user rules');
}

export async function saveAgentOrchestratorConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/orchestrator/save', config, 'save orchestrator config');
}

export async function saveAgentOrchestratorSessionConfig(
  config: Record<string, unknown>,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>(
    '/api/settings/orchestrator/session/save',
    { config },
    'save session orchestrator config',
    bindingOverride,
  );
}

export async function saveAgentAuxiliaryConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/auxiliary/save', config, 'save auxiliary config');
}

export async function saveAgentImageGenerationConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/image-generation/save', config, 'save image generation config');
}

export async function removeAgentWorkerConfig(worker: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/worker/remove', { worker }, 'remove worker config');
}

export async function testAgentWorkerConnection(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/worker/test', { worker, config }, 'test worker connection');
}

export async function testAgentOrchestratorConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/orchestrator/test', config, 'test orchestrator connection');
}

export async function testAgentAuxiliaryConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/auxiliary/test', config, 'test auxiliary connection');
}

export async function testAgentImageGenerationConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/image-generation/test', config, 'test image generation connection');
}


export async function listAgentRoleTemplates(): Promise<RoleTemplate[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/role-templates', buildWorkspaceBoundQuery({})));
  const payload = await parseAgentJson<{ templates?: RoleTemplate[] }>(response, 'load role templates');
  return Array.isArray(payload.templates) ? payload.templates : [];
}

export async function listAgentRegistryEngines(): Promise<ModelEngine[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/engines', buildWorkspaceBoundQuery({})));
  const payload = await parseAgentJson<{ engines?: ModelEngine[] }>(response, 'load registry engines');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function listAgentRegistryAgents(): Promise<AgentBinding[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/agents', buildWorkspaceBoundQuery({})));
  const payload = await parseAgentJson<{ agents?: AgentBinding[] }>(response, 'load registry agents');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function upsertAgentRegistryEngine(engine: ModelEngine): Promise<ModelEngine[]> {
  const payload = await postWorkspaceBoundJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/upsert', engine as unknown as Record<string, unknown>, 'upsert registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function removeAgentRegistryEngine(engineId: string): Promise<ModelEngine[]> {
  const payload = await postWorkspaceBoundJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/remove', { engineId }, 'remove registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function upsertAgentRegistryBinding(agent: AgentBinding): Promise<AgentBinding[]> {
  const payload = await postWorkspaceBoundJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/upsert', agent as unknown as Record<string, unknown>, 'upsert registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function removeAgentRegistryBinding(templateId: string): Promise<AgentBinding[]> {
  const payload = await postWorkspaceBoundJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/remove', { templateId }, 'remove registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function fetchAgentModelList(config: Record<string, unknown>, target: string): Promise<FetchModelsResponseDto> {
  return await postWorkspaceBoundJson<FetchModelsResponseDto>('/api/settings/models/fetch', { config, target }, 'fetch model list');
}

export async function clearAgentProjectKnowledge(
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/clear',
    {},
    'clear project knowledge',
    bindingOverride,
  );
}

export async function getAgentProjectKnowledge(
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<Record<string, unknown>> {
  const query = buildBoundQueryWithOverride({}, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load project knowledge');
}

export async function reindexAgentProjectKnowledge(
  bindingOverride?: Partial<AgentBindingContext>,
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

export async function listAgentKnowledgeItems(
  kind?: AgentKnowledgeKind,
  bindingOverride?: Partial<AgentBindingContext>,
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<Record<string, unknown>> {
  const params: Record<string, string> = { q: keyword };
  if (kind) params.kind = kind;
  const query = buildBoundQueryWithOverride(params, bindingOverride, { includeSession: false });
  const response = await getTransport().request(agentUrl('/api/knowledge/items/search', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'search knowledge items');
}

export async function addAgentKnowledgeItem(
  payload: AgentKnowledgeItemPayload,
  bindingOverride?: Partial<AgentBindingContext>,
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
  bindingOverride?: Partial<AgentBindingContext>,
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
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentKnowledgeMutationPayload> {
  return await postWorkspaceBoundJson<AgentKnowledgeMutationPayload>(
    '/api/knowledge/items/delete',
    { knowledgeId },
    'delete knowledge item',
    bindingOverride,
  );
}

export async function loadAgentMcpServers(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/mcp', buildWorkspaceBoundQuery({})));
  return await parseAgentJson<Record<string, unknown>>(response, 'load mcp servers');
}

export async function addAgentMcpServer(server: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/mcp/add', normalizeMcpServerConfig(server), 'add mcp server');
}

export async function updateAgentMcpServer(serverId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/settings/mcp/update',
    normalizeMcpServerConfig({ ...updates, id: serverId, serverId }),
    'update mcp server',
  );
}

export async function deleteAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/mcp/delete', { serverId }, 'delete mcp server');
}

export async function getAgentMcpServerTools(serverId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/mcp/tools', { serverId }, 'get mcp server tools');
}

export async function refreshAgentMcpTools(serverId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/mcp/tools/refresh', { serverId }, 'refresh mcp tools');
}

export async function connectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/mcp/connect', { serverId }, 'connect mcp server');
}

export async function disconnectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/mcp/disconnect', { serverId }, 'disconnect mcp server');
}

export async function loadAgentRepositories(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/repositories', buildWorkspaceBoundQuery({})));
  return await parseAgentJson<Record<string, unknown>>(response, 'load repositories');
}

export async function addAgentRepository(url: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/repositories/add', { url }, 'add repository');
}

export async function updateAgentRepository(repositoryId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/repositories/update', { repositoryId, updates }, 'update repository');
}

export async function deleteAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/repositories/delete', { repositoryId }, 'delete repository');
}

export async function refreshAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/repositories/refresh', { repositoryId }, 'refresh repository');
}

export async function loadAgentSkillLibrary(): Promise<SkillsLibraryResponseDto> {
  const response = await getTransport().request(agentUrl('/api/settings/skills/library', buildWorkspaceBoundQuery({})));
  return await parseAgentJson<SkillsLibraryResponseDto>(response, 'load skill library');
}

export async function installAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/install', { skillId }, 'install skill');
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
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/install-local', payload, 'install local skill');
}

export async function scanAgentLocalSkillDirectory(directoryPath: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/scan-local', { directoryPath }, 'scan local skill directory');
}

export async function saveAgentSkillsConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/config/save', config, 'save skills config');
}

export async function toggleAgentSkill(skillId: string, enabled: boolean): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/settings/skills/toggle',
    { skillId, enabled },
    'toggle skill',
  );
}

export async function saveAgentSafeguardConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/safeguard/save', config, 'save safeguard config');
}

export async function addAgentCustomTool(tool: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/custom-tool/add', tool, 'add custom tool');
}

export type AgentSkillSource = 'custom' | 'instruction';

export async function removeAgentInstalledSkill(skillId: string, source: AgentSkillSource): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/remove', { skillId, source }, 'remove installed skill');
}

export async function updateAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/update', { skillId }, 'update skill');
}

export async function updateAllAgentSkills(): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/update-all', {}, 'update all skills');
}

export async function checkAgentSkillUpdates(): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/check-updates', {}, 'check skill updates');
}

export async function rollbackAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/rollback', { skillId }, 'rollback skill');
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
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<AgentPendingChangesPayload> {
  try {
    const query = buildBoundQueryWithOverride({}, options);
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
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<AgentChangeDiffPayload> {
  try {
    const query = buildBoundQueryWithOverride({ filePath }, options);
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
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  await postBoundJson('/api/changes/approve', { filePath }, 'approve change', options);
}

export async function revertAgentChange(
  filePath: string,
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  await postBoundJson('/api/changes/revert', { filePath }, 'revert change', options);
}

export async function approveAllAgentChanges(
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  await postBoundJson('/api/changes/approve-all', {}, 'approve all changes', options);
}

export async function revertAllAgentChanges(
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  await postBoundJson('/api/changes/revert-all', {}, 'revert all changes', options);
}

export async function revertAgentExecutionGroupChanges(
  executionGroupId: string,
  options: { sessionId?: string; workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  await postBoundJson(
    '/api/changes/revert-execution-group',
    { executionGroupId },
    'revert execution group changes',
    options,
  );
}
