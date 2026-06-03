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
  SessionNotificationItemDto,
  SessionNotificationSnapshotDto,
  SessionNotificationsResponseDto,
  MessagesResponseDto,
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


const AGENT_BASE_URL_STORAGE_KEY = 'magi-agent-base-url';
const AGENT_PROBE_TIMEOUT_MS = 1500;
let cachedWorkspaceSummaries: AgentWorkspaceSummary[] = [];

export const AGENT_CONNECTION_EVENT = 'magi-agent-connection';

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
  preview?: string;
}

export interface AgentBootstrapSnapshot {
  workspace: AgentWorkspaceSummary;
  sessionId: string;
  session: AgentSessionSummary;
  notifications?: AgentSessionNotificationsPayload;
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
  workspace_id?: string;
  path?: string;
  rootPath?: string;
  root_path?: string;
  name?: string | null;
  isActive?: boolean;
  is_active?: boolean;
}

interface RawAgentSessionSummary {
  id?: string;
  sessionId?: string;
  session_id?: string;
  workspaceId?: string;
  workspace_id?: string;
  name?: string | null;
  title?: string | null;
  createdAt?: number;
  created_at?: number;
  updatedAt?: number;
  updated_at?: number;
  messageCount?: number;
  message_count?: number;
  isRunning?: boolean;
  is_running?: boolean;
  runningTaskCount?: number;
  running_task_count?: number;
  preview?: string | null;
}

export type AgentRuntimeSettings = SettingsRuntimeSnapshot;
export type AgentSettingsBootstrapSnapshot = SettingsBootstrapPayload;

export interface AgentToolCatalogDiagnosticsSnapshot {
  builtinTools: SettingsBuiltinTool[];
  capabilityDependencies: SettingsCapabilityDependency[];
}

function normalizeSettingsSectionConfig(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  const record = value as Record<string, unknown>;
  const nestedConfig = record.config;
  if (nestedConfig && typeof nestedConfig === 'object' && !Array.isArray(nestedConfig)) {
    return nestedConfig as Record<string, unknown>;
  }
  const nestedData = record.data;
  if (nestedData && typeof nestedData === 'object' && !Array.isArray(nestedData)) {
    return nestedData as Record<string, unknown>;
  }
  return record;
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

function normalizeNullableString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
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

function readRecordField(record: Record<string, unknown>, camelKey: string, snakeKey: string): unknown {
  return record[camelKey] ?? record[snakeKey];
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
    const riskLevel = readRecordField(record, 'riskLevel', 'risk_level');
    const approvalRequirement = readRecordField(record, 'approvalRequirement', 'approval_requirement');
    const effectiveApprovalPolicy = readRecordField(record, 'effectiveApprovalPolicy', 'effective_approval_policy');
    const accessProfileBehavior = readRecordField(record, 'accessProfileBehavior', 'access_profile_behavior');
    const accessMode = readRecordField(record, 'accessMode', 'access_mode');
    const policyScope = readRecordField(record, 'policyScope', 'policy_scope');
    const inputSensitivePolicy = readRecordField(record, 'inputSensitivePolicy', 'input_sensitive_policy');
    const policySummary = readRecordField(record, 'policySummary', 'policy_summary');
    const runtimeInternal = readRecordField(record, 'runtimeInternal', 'runtime_internal');
    const runtimeStatus = readRecordField(record, 'runtimeStatus', 'runtime_status');
    const runtimeWarnings = readRecordField(record, 'runtimeWarnings', 'runtime_warnings');
    const schemaStatus = readRecordField(record, 'schemaStatus', 'schema_status');
    const schemaWarnings = readRecordField(record, 'schemaWarnings', 'schema_warnings');
    tools.push({
      name,
      riskLevel: typeof riskLevel === 'string' ? riskLevel : '',
      approvalRequirement: typeof approvalRequirement === 'string' ? approvalRequirement : '',
      effectiveApprovalPolicy: typeof effectiveApprovalPolicy === 'string' ? effectiveApprovalPolicy : 'none',
      accessProfileBehavior: typeof accessProfileBehavior === 'string' ? accessProfileBehavior : 'restricted_allowed',
      accessMode: typeof accessMode === 'string' ? accessMode : 'read_only',
      policyScope: typeof policyScope === 'string' ? policyScope : 'fixed',
      inputSensitivePolicy: inputSensitivePolicy === true,
      policySummary: typeof policySummary === 'string' ? policySummary : '',
      runtimeInternal: runtimeInternal === true,
      runtimeStatus: normalizeToolRuntimeStatus(runtimeStatus),
      runtimeWarnings: normalizeWarningMarkers(runtimeWarnings, 'runtime_warning'),
      schemaStatus: typeof schemaStatus === 'string' ? schemaStatus : 'ok',
      schemaWarnings: normalizeWarningMarkers(schemaWarnings, 'schema_warning'),
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
    const snapshotActive = record.snapshot_active ?? record.snapshotActive;
    dependencies.push({
      name,
      status,
      requiredBy: normalizeRequiredBy(record.required_by ?? record.requiredBy),
      workspaceId: normalizeNullableString(record.workspace_id ?? record.workspaceId),
      sessionId: normalizeNullableString(record.session_id ?? record.sessionId),
      fileCount: normalizeNullableNumber(record.file_count ?? record.fileCount),
      lastIndexed: normalizeNullableNumber(record.last_indexed ?? record.lastIndexed),
      cacheStatus: normalizeNullableString(record.cache_status ?? record.cacheStatus),
      roleCount: normalizeNullableNumber(record.role_count ?? record.roleCount),
      spawnableRoleCount: normalizeNullableNumber(
        record.spawnable_role_count ?? record.spawnableRoleCount,
      ),
      snapshotActive: typeof snapshotActive === 'boolean' ? snapshotActive : null,
      configuredCount: normalizeNullableNumber(record.configured_count ?? record.configuredCount),
      enabledCount: normalizeNullableNumber(record.enabled_count ?? record.enabledCount),
      readyCount: normalizeNullableNumber(record.ready_count ?? record.readyCount),
      enabledToolCount: normalizeNullableNumber(
        record.enabled_tool_count ?? record.enabledToolCount,
      ),
      readyToolCount: normalizeNullableNumber(
        record.ready_tool_count ?? record.readyToolCount,
      ),
      toolCount: normalizeNullableNumber(record.tool_count ?? record.toolCount),
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
      : {
          locale: typeof payload.locale === 'string' ? payload.locale : 'zh-CN',
        }
  ) as SettingsRuntimeSnapshot;

  return {
    workspaceId: normalizeBindingString(payload.workspaceId ?? payload.workspace_id),
    workspacePath: normalizeBindingString(payload.workspacePath ?? payload.workspace_path),
    sessionId: normalizeBindingString(payload.sessionId ?? payload.session_id),
    workerConfigs: (
      payload.workerConfigs
      && typeof payload.workerConfigs === 'object'
      && !Array.isArray(payload.workerConfigs)
        ? payload.workerConfigs
        : (
            payload.workers
            && typeof payload.workers === 'object'
            && !Array.isArray(payload.workers)
              ? payload.workers
              : {}
          )
    ) as Record<string, unknown>,
    orchestratorConfig: normalizeSettingsSectionConfig(payload.orchestratorConfig ?? payload.orchestrator),
    auxiliaryConfig: normalizeSettingsSectionConfig(payload.auxiliaryConfig ?? payload.auxiliary),
    userRulesConfig: normalizeSettingsSectionConfig(payload.userRulesConfig ?? payload.userRules),
    skillsConfig: normalizeSettingsSectionConfig(payload.skillsConfig ?? payload.skills),
    safeguardConfig: normalizeSettingsSectionConfig(payload.safeguardConfig ?? payload.safeguard),
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
    registryEngines: Array.isArray(payload.registryEngines)
      ? payload.registryEngines
      : (Array.isArray(payload.engines) ? payload.engines : undefined),
    registryAgents: Array.isArray(payload.registryAgents)
      ? payload.registryAgents
      : (Array.isArray(payload.agents) ? payload.agents : undefined),
    bootstrapScope: payload.bootstrapScope === 'core' ? 'core' : 'full',
    mcpServersHydrated: payload.mcpServersHydrated !== false,
  };
}

export function settingsBootstrapMatchesCurrentWorkspace(
  snapshot: Pick<SettingsBootstrapPayload, 'workspaceId' | 'workspacePath' | 'sessionId'> | null | undefined,
): boolean {
  if (!snapshot) return false;
  const binding = resolveAgentBindingContext();
  return normalizeBindingString(snapshot.workspaceId) === binding.workspaceId
    && normalizeBindingString(snapshot.workspacePath) === binding.workspacePath
    && normalizeBindingString(snapshot.sessionId) === binding.sessionId;
}

export interface AgentExecutionStatsItem {
  templateId: string;
  engineId: string;
  bindingRevision: number;
  role: 'worker' | 'orchestrator' | 'auxiliary';
  displayName: string;
  provider?: string;
  declaredModelSpec?: string;
  resolvedModel?: string;
  llmCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
}

export interface AgentExecutionStatsPayload {
  scope: 'session' | 'workspace';
  workspaceId: string;
  sessionId?: string | null;
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
}

// Migrated to canonical types from rust-backend-types.ts:
//   AgentSessionNotificationRecord  → SessionNotificationItemDto
//   AgentSessionNotificationSnapshot → SessionNotificationSnapshotDto
//   AgentSessionNotificationsPayload → SessionNotificationsResponseDto
export type AgentSessionNotificationRecord = SessionNotificationItemDto;
export type AgentSessionNotificationSnapshot = SessionNotificationSnapshotDto;
export type AgentSessionNotificationsPayload = SessionNotificationsResponseDto;


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
  route: 'chat' | 'execute' | 'task' | 'continue' | 'supplement_context';
  /** Root task ID when the backend created a task projection for this action. */
  rootTaskId?: string | null;
  /** 当前轮次实际执行的 action task ID。 */
  actionTaskId?: string | null;
  executionChainRef?: string | null;
  /** 后端生成的 canonical 用户消息 item ID。 */
  userMessageItemId?: string | null;
  canonicalSchemaVersion?: string | null;
  canonicalEventKind?: string | null;
  canonicalTurn?: CanonicalTurn | null;
  canonicalItem?: CanonicalTurnItem | null;
  /** 仅在 supplement_context 路由下返回：本次入栈的 mailbox signal ID。 */
  signalRef?: string | null;
  /** 仅在 supplement_context 路由下返回：被投递的任务 ID。 */
  targetTaskId?: string | null;
}

export class AgentApiError extends Error {
  readonly status: number;
  readonly action: string;
  readonly errorCode?: string;

  constructor(status: number, message: string, action: string, errorCode?: string) {
    super(message);
    this.name = 'AgentApiError';
    this.status = status;
    this.action = action;
    this.errorCode = errorCode;
  }
}

export interface AgentNotificationScope {
  workspaceId: string;
  workspacePath?: string;
  sessionId: string;
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
  safeWriteLocalStorage(AGENT_BASE_URL_STORAGE_KEY, baseUrl.trim());
}

function getStoredAgentBaseUrl(): string {
  return safeReadLocalStorage(AGENT_BASE_URL_STORAGE_KEY);
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
  const workspaceId = raw.workspaceId?.trim() || raw.workspace_id?.trim() || '';
  const rootPath = raw.rootPath?.trim() || raw.root_path?.trim() || raw.path?.trim() || '';
  return {
    workspaceId,
    rootPath,
    name: deriveWorkspaceName(rootPath, workspaceId),
    isActive: raw.isActive === true || raw.is_active === true,
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
  const id = raw.id?.trim() || raw.sessionId?.trim() || raw.session_id?.trim() || '';
  const workspaceId = raw.workspaceId?.trim() || raw.workspace_id?.trim() || '';
  const createdAt = raw.createdAt ?? raw.created_at ?? Date.now();
  const updatedAt = raw.updatedAt ?? raw.updated_at ?? createdAt;
  const name = raw.name?.trim() || raw.title?.trim() || undefined;
  const preview = raw.preview?.trim() || undefined;
  const messageCount = raw.messageCount ?? raw.message_count;
  const runningTaskCount = raw.runningTaskCount ?? raw.running_task_count;
  const isRunning = raw.isRunning ?? raw.is_running;
  return {
    id,
    ...(workspaceId ? { workspaceId } : {}),
    name,
    createdAt,
    updatedAt,
    ...(typeof messageCount === 'number' ? { messageCount } : {}),
    ...(typeof isRunning === 'boolean' ? { isRunning } : {}),
    ...(typeof runningTaskCount === 'number' ? { runningTaskCount: Math.max(0, Math.floor(runningTaskCount)) } : {}),
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
  const controller = typeof AbortController !== 'undefined' ? new AbortController() : null;
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
  window.dispatchEvent(new CustomEvent<AgentConnectionEventDetail>(AGENT_CONNECTION_EVENT, { detail }));
}

async function parseAgentJson<T>(response: Response, action: string): Promise<T> {
  if (!response.ok) {
    let backendError: string | null = null;
    let backendErrorCode: string | undefined;
    const contentType = response.headers.get('content-type') || '';
    if (contentType.includes('application/json')) {
      try {
        const payload = await response.json() as { error?: string; message?: string; error_code?: string; code?: string };
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
      } catch {
        // ignore malformed error payload and fallback to generic message
      }
    }
    throw new AgentApiError(
      response.status,
      backendError || `${action} failed: ${response.status}`,
      action,
      backendErrorCode,
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

/**
 * 从当前页面 URL 提取 tunnel_token（用户通过隧道公网链接访问时自带）。
 * 本地访问时返回 null。
 */
function getTunnelToken(): string | null {
  if (typeof window === 'undefined') return null;
  return new URL(window.location.href).searchParams.get('tunnel_token');
}

export function isPublicTunnelAccess(): boolean {
  if (typeof window === 'undefined') return false;
  const currentUrl = new URL(window.location.href);
  return Boolean(currentUrl.searchParams.get('tunnel_token'));
}

/**
 * 将 tunnel_token 附加到已有的 query string 上（如果存在）。
 */
function appendTokenToQuery(query: string): string {
  const token = getTunnelToken();
  if (!token) return query;
  const sep = query ? '&' : '';
  return `${query}${sep}tunnel_token=${encodeURIComponent(token)}`;
}

/**
 * 构造完整的 Agent API URL = baseUrl + pathname + 可选 query + tunnel_token。
 */
export function agentUrl(pathname: string, query?: string): string {
  const base = resolveAgentBaseUrl();
  const q = appendTokenToQuery(query || '');
  return q ? `${base}${pathname}?${q}` : `${base}${pathname}`;
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
  return Boolean(
    bindingOverride
      && Object.prototype.hasOwnProperty.call(bindingOverride, key)
      && bindingOverride[key] !== undefined,
  );
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
  const sessionId = scope.sessionId.trim();
  const workspaceId = scope.workspaceId.trim();
  if (!sessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'resolve notification scope');
  }
  if (!workspaceId) {
    throw new AgentApiError(400, 'workspaceId 不能为空', 'resolve notification scope');
  }
  return {
    sessionId,
    workspaceId,
    workspacePath: scope.workspacePath?.trim() || '',
  };
}

async function postBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<T> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    const response = await getTransport().request(agentUrl(pathname), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaceId: binding.workspaceId,
        workspacePath: binding.workspacePath,
        sessionId: binding.sessionId,
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

export interface DirectoryEntry {
  name: string;
  path: string;
  isDirectory: boolean;
  hasChildren?: boolean;
}

export interface DirectoryListResult {
  path: string;
  parent: string;
  entries: DirectoryEntry[];
  error?: string;
}

export interface WorkspaceDirectoryListResult extends DirectoryListResult {
  workspaceId: string;
  workspacePath: string;
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
  dirPath?: string,
  showHidden = false,
): Promise<DirectoryListResult> {
  try {
    const query = new URLSearchParams();
    if (dirPath) {
      query.set('path', dirPath);
    }
    if (showHidden) {
      query.set('showHidden', '1');
    }
    const response = await getTransport().request(agentUrl('/api/filesystem/browse', query.toString()));
    return await parseAgentJson<DirectoryListResult>(response, 'browse directory');
  } catch (error) {
    throwNormalizedDirectoryError(error);
  }
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

export async function loadAgentSessionSnapshot(
  sessionId: string,
  options: { limit?: number; beforeCursor?: string; workspaceId: string; workspacePath?: string },
): Promise<MessagesResponseDto> {
  try {
    const binding = resolveAgentBindingContext();
    const workspaceId = options.workspaceId.trim() || binding.workspaceId;
    const workspacePath = options.workspacePath?.trim() || binding.workspacePath;
    if (!workspaceId) {
      throw new Error('workspaceId 不能为空');
    }
    const query = new URLSearchParams({
      sessionId: sessionId.trim(),
      workspaceId,
      limit: String(options.limit ?? 50),
    });
    if (workspacePath) {
      query.set('workspacePath', workspacePath);
    }
    if (options.beforeCursor?.trim()) {
      query.set('beforeCursor', options.beforeCursor.trim());
    }
    const response = await getTransport().request(agentUrl('/api/messages', query.toString()));
    return await parseAgentJson<MessagesResponseDto>(response, 'load session snapshot');
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
  return await postBoundJson<T>(pathname, payload, action, { ...bindingOverride, sessionId: '' });
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

export async function saveAgentCurrentSession(
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>(
    '/api/session/save',
    {},
    'save current session',
    bindingOverride,
  );
}

export async function getAgentSessionNotifications(scope: AgentNotificationScope): Promise<AgentSessionNotificationsPayload> {
  const query = buildBoundQueryWithOverride({}, createNotificationBindingOverride(scope));
  const response = await getTransport().request(agentUrl('/api/session/notifications', query));
  return await parseAgentJson<AgentSessionNotificationsPayload>(response, 'load session notifications');
}

export async function appendAgentNotification(
  notification: Record<string, unknown>,
  scope: AgentNotificationScope,
): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/append',
    { ...notification },
    'append notification',
    createNotificationBindingOverride(scope),
  );
}

export async function markAllAgentNotificationsRead(scope: AgentNotificationScope): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/mark-all-read',
    {},
    'mark all notifications read',
    createNotificationBindingOverride(scope),
  );
}

export async function clearAgentNotifications(scope: AgentNotificationScope): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/clear',
    {},
    'clear notifications',
    createNotificationBindingOverride(scope),
  );
}

export async function removeAgentNotification(
  notificationId: string,
  scope: AgentNotificationScope,
): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/remove',
    { notificationId },
    'remove notification',
    createNotificationBindingOverride(scope),
  );
}

export async function submitSessionTurn(
  payload: {
    text?: string | null;
    skillName?: string | null;
    images: AgentSessionTurnImagePayload[];
    accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
    requestId?: string | null;
    userMessageId?: string | null;
    placeholderMessageId?: string | null;
    supplementContext?: boolean;
    targetTaskId?: string | null;
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
        accessProfile: payload.accessProfile ?? null,
        requestId: payload.requestId ?? null,
        userMessageId: payload.userMessageId ?? null,
        placeholderMessageId: payload.placeholderMessageId ?? null,
        supplementContext: payload.supplementContext === true,
        targetTaskId: payload.targetTaskId ?? null,
        images: payload.images.map((image) => ({
          name: image.name,
          dataUrl: image.dataUrl,
        })),
      }),
    });
    const raw = await parseAgentJson<{
      sessionId: string;
      entryId: string;
      eventId: string;
      acceptedAt: number;
      createdSession: boolean;
      route: 'chat' | 'execute' | 'task' | 'continue' | 'supplement_context';
      rootTaskId?: string | null;
      actionTaskId?: string | null;
      executionChainRef?: string | null;
      userMessageItemId?: string | null;
      canonicalSchemaVersion?: string | null;
      canonicalEventKind?: string | null;
      canonicalTurn?: CanonicalTurn | null;
      canonicalItem?: CanonicalTurnItem | null;
      signalRef?: string | null;
      targetTaskId?: string | null;
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
      canonicalSchemaVersion: typeof raw.canonicalSchemaVersion === 'string' && raw.canonicalSchemaVersion.trim()
        ? raw.canonicalSchemaVersion.trim()
        : null,
      canonicalEventKind: typeof raw.canonicalEventKind === 'string' && raw.canonicalEventKind.trim()
        ? raw.canonicalEventKind.trim()
        : null,
      canonicalTurn: raw.canonicalTurn ?? null,
      canonicalItem: raw.canonicalItem ?? null,
      signalRef: typeof raw.signalRef === 'string' && raw.signalRef.trim() ? raw.signalRef.trim() : null,
      targetTaskId: typeof raw.targetTaskId === 'string' && raw.targetTaskId.trim()
        ? raw.targetTaskId.trim()
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
  return await postBoundJson<Record<string, unknown>>('/api/task/interrupt', payload, 'interrupt task');
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

export async function clearAgentAllTasks(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/clear-all', {}, 'clear all tasks');
}

export async function startAgentTask(taskId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/start', { taskId }, 'start task');
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

export async function deleteAgentTask(taskId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/delete', { taskId }, 'delete task');
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
  options: { accessProfile?: AccessProfile | null } = {},
): Promise<AgentToolCatalogDiagnosticsSnapshot> {
  try {
    const query = buildBoundQuery({
      includeExternal: 'true',
      includeMcpServers: 'true',
      includeAgentRoles: 'true',
      accessProfile: normalizeAccessProfile(options.accessProfile ?? readStoredAccessProfile()),
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
      capabilityDependencies: normalizeCapabilityDependencies(
        payload.runtime_dependencies ?? payload.runtimeDependencies,
      ),
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
  return await postWorkspaceBoundJson<Record<string, unknown>>(
    '/api/settings/stats/reset',
    {},
    'reset execution stats',
  );
}

export async function getAgentExecutionStats(): Promise<AgentExecutionStatsPayload> {
  try {
    const query = buildWorkspaceBoundQuery({});
    const response = await getTransport().request(agentUrl('/api/settings/stats/session', query));
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
  additions: number;
  deletions: number;
}

export async function fetchWorkspaceBranches(): Promise<WorkspaceBranchesResult> {
  return await postWorkspaceBoundJson<WorkspaceBranchesResult>('/api/workspace/vcs/branches', {}, 'fetch workspace branches');
}

export interface CheckoutBranchResult {
  ok: boolean;
  currentBranch: string | null;
  error?: string;
}

export async function checkoutWorkspaceBranch(branch: string): Promise<CheckoutBranchResult> {
  return await postWorkspaceBoundJson<CheckoutBranchResult>('/api/workspace/vcs/checkout', { branch }, 'checkout workspace branch');
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

export async function saveAgentAuxiliaryConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/auxiliary/save', config, 'save auxiliary config');
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

export async function saveAgentSafeguardConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/safeguard/save', config, 'save safeguard config');
}

export async function addAgentCustomTool(tool: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/custom-tool/add', tool, 'add custom tool');
}

export type AgentSkillSource = 'custom' | 'instruction';

export async function removeAgentInstalledSkill(skillName: string, source: AgentSkillSource): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/remove', { skillName, source }, 'remove installed skill');
}

export async function updateAgentSkill(skillName: string): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/update', { skillName }, 'update skill');
}

export async function updateAllAgentSkills(): Promise<Record<string, unknown>> {
  return await postWorkspaceBoundJson<Record<string, unknown>>('/api/settings/skills/update-all', {}, 'update all skills');
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
