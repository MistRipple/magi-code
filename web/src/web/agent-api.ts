import { getDefaultAgentBaseUrl } from '../shared/agent-shared-config';
import { getTransport } from '../shared/transport';
import { readStoredBrowserWorkspaceBinding } from '../shared/bridges/browser-workspace-binding';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
import type { RoleTemplate } from '../shared/types/role-templates';
import type { SettingsBootstrapPayload, SettingsRuntimeSnapshot } from '../shared/settings-bootstrap';
import type {
  SessionNotificationItemDto,
  SessionNotificationSnapshotDto,
  SessionNotificationsResponseDto,
  MessagesResponseDto,
} from '../shared/rust-backend-types';
import { i18n } from '../stores/i18n.svelte';


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
}

export interface AgentSessionSummary {
  id: string;
  name?: string;
  createdAt: number;
  updatedAt: number;
  messageCount?: number;
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
}

interface RawAgentSessionSummary {
  id?: string;
  sessionId?: string;
  session_id?: string;
  name?: string | null;
  title?: string | null;
  createdAt?: number;
  created_at?: number;
  updatedAt?: number;
  updated_at?: number;
  messageCount?: number;
  message_count?: number;
  preview?: string | null;
}

export type AgentRuntimeSettings = SettingsRuntimeSnapshot;
export type AgentSettingsBootstrapSnapshot = SettingsBootstrapPayload;

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
          deepTask: payload.deepTask === true,
        }
  ) as SettingsRuntimeSnapshot;

  return {
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
  cacheReadTokens: number;
  cacheWriteTokens: number;
}

export interface AgentExecutionStatsPayload {
  scope: 'session' | 'workspace';
  workspaceId: string;
  sessionId?: string;
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
    cacheReadTokens: number;
    cacheWriteTokens: number;
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
  error?: string;
  payload?: Record<string, unknown>;
}

export interface AgentFilePreviewPayload {
  filePath: string;
  absolutePath: string;
  exists: boolean;
  content: string;
  language: string;
}

export interface AgentChangeDiffPayload {
  filePath: string;
  diff: string;
  additions: number;
  deletions: number;
  originalContent?: string;
  currentContent?: string;
  currentAbsolutePath?: string;
  currentExists?: boolean;
}

export interface AgentSessionTurnImagePayload {
  name: string;
  dataUrl: string;
}

export type AgentSessionTurnRoute = 'chat' | 'execute' | 'task' | 'continue';

export interface AgentSessionTurnResult {
  sessionId: string;
  entryId: string;
  eventId: string;
  acceptedAt: number;
  createdSession: boolean;
  route: AgentSessionTurnRoute;
  /** Root task ID when the backend created a task graph for this action. */
  rootTaskId?: string | null;
  /** 当前轮次实际执行的 action task ID。 */
  actionTaskId?: string | null;
  executionChainRef?: string | null;
}

export class AgentApiError extends Error {
  readonly status: number;
  readonly action: string;

  constructor(status: number, message: string, action: string) {
    super(message);
    this.name = 'AgentApiError';
    this.status = status;
    this.action = action;
  }
}

interface AgentBindingContext {
  workspaceId: string;
  workspacePath: string;
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
  };
}

function normalizeSessionSummary(raw: RawAgentSessionSummary): AgentSessionSummary {
  const id = raw.id?.trim() || raw.sessionId?.trim() || raw.session_id?.trim() || '';
  const createdAt = raw.createdAt ?? raw.created_at ?? Date.now();
  const updatedAt = raw.updatedAt ?? raw.updated_at ?? createdAt;
  const name = raw.name?.trim() || raw.title?.trim() || undefined;
  const preview = raw.preview?.trim() || undefined;
  const messageCount = raw.messageCount ?? raw.message_count;
  return {
    id,
    name,
    createdAt,
    updatedAt,
    ...(typeof messageCount === 'number' ? { messageCount } : {}),
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
    injectedBaseUrl,
    configuredProxyTarget && servedByAgentOrigin ? servedByAgentOrigin : '',
    configuredBaseUrl,
    queryBaseUrl,
    servedByAgentOrigin,
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
    const contentType = response.headers.get('content-type') || '';
    if (contentType.includes('application/json')) {
      try {
        const payload = await response.json() as {
          error?: unknown;
          message?: unknown;
          detail?: unknown;
          details?: unknown;
        };
        const nestedError = payload?.error;
        const candidates = [
          nestedError,
          typeof nestedError === 'object' && nestedError !== null
            ? (nestedError as { message?: unknown }).message
            : undefined,
          payload?.message,
          payload?.detail,
          payload?.details,
        ];
        for (const candidate of candidates) {
          if (typeof candidate === 'string' && candidate.trim()) {
            backendError = candidate.trim();
            break;
          }
        }
      } catch {
        // ignore malformed error payload and fallback to generic message
      }
    }
    throw new AgentApiError(
      response.status,
      backendError || `${action} failed: ${response.status}`,
      action,
    );
  }

  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    throw new Error('` + i18n.t("bridge.notConnected") + `');
  }

  return await response.json() as T;
}

export function resolveAgentBaseUrl(): string {
  if (typeof window === 'undefined') {
    return getDefaultAgentBaseUrl();
  }
  // VS Code webview 场景：由宿主注入 __AGENT_BASE_URL__
  const injectedBaseUrl = (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim();
  if (injectedBaseUrl) {
    return injectedBaseUrl;
  }
  const currentUrl = new URL(window.location.href);
  const servedByAgent = currentUrl.protocol.startsWith('http')
    && (currentUrl.pathname === '/' || currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/'));
  const configuredProxyTarget = getConfiguredAgentProxyTarget();
  if (configuredProxyTarget && servedByAgent) {
    persistAgentBaseUrl(currentUrl.origin);
    return currentUrl.origin;
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

function resolveAgentBindingContext(): AgentBindingContext {
  if (typeof window === 'undefined') {
    return { workspaceId: '', workspacePath: '', sessionId: '' };
  }
  const currentUrl = new URL(window.location.href);
  const bootstrapWindow = window as unknown as {
    __INITIAL_WORKSPACE_ID__?: string;
    __INITIAL_WORKSPACE_PATH__?: string;
  };
  const storedBinding = readStoredBrowserWorkspaceBinding();
  const urlWorkspaceId = currentUrl.searchParams.get('workspaceId')?.trim() || '';
  const urlWorkspacePath = currentUrl.searchParams.get('workspacePath')?.trim() || '';
  const urlSessionId = currentUrl.searchParams.get('sessionId')?.trim() || '';
  const hasExplicitUrlWorkspace = Boolean(urlWorkspaceId || urlWorkspacePath);
  return {
    workspaceId: urlWorkspaceId
      || bootstrapWindow.__INITIAL_WORKSPACE_ID__?.trim()
      || storedBinding.workspaceId
      || '',
    workspacePath: urlWorkspacePath
      || bootstrapWindow.__INITIAL_WORKSPACE_PATH__?.trim()
      || storedBinding.workspacePath
      || '',
    sessionId: urlSessionId
      || (hasExplicitUrlWorkspace ? '' : storedBinding.sessionId)
      || '',
  };
}

function buildBoundQuery(extra: Record<string, string>): string {
  const binding = resolveAgentBindingContext();
  const query = new URLSearchParams();
  if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
  if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
  if (binding.sessionId) query.set('sessionId', binding.sessionId);
  for (const [key, value] of Object.entries(extra)) {
    if (value) {
      query.set(key, value);
    }
  }
  return query.toString();
}

async function postBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<T> {
  try {
    const resolvedBinding = resolveAgentBindingContext();
    const binding: AgentBindingContext = {
      workspaceId: bindingOverride?.workspaceId?.trim() || resolvedBinding.workspaceId,
      workspacePath: bindingOverride?.workspacePath?.trim() || resolvedBinding.workspacePath,
      sessionId: bindingOverride?.sessionId?.trim() || resolvedBinding.sessionId,
    };
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
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
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
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
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
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
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
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
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
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
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

export async function listAgentDirectory(dirPath?: string, showHidden?: boolean): Promise<DirectoryListResult> {
  try {
    const query = new URLSearchParams();
    if (dirPath) {
      query.set('path', dirPath);
    }
    if (showHidden) {
      query.set('showHidden', '1');
    }
    const qs = query.toString();
    const response = await getTransport().request(agentUrl(`/api/filesystem/list${qs ? `?${qs}` : ''}`));
    const result = await parseAgentJson<DirectoryListResult>(response, 'list directory');
    
    // Fallback: Infer path if backend doesn't provide it
    if (!result.path) {
      if (dirPath) {
        result.path = dirPath;
      } else if (result.entries && result.entries.length > 0) {
        const first = result.entries[0];
        if (first.path.endsWith(first.name)) {
          let p = first.path.slice(0, -first.name.length);
          if (p.endsWith('/') && p.length > 1) p = p.slice(0, -1);
          else if (p.endsWith('\\') && p.length > 3) p = p.slice(0, -1);
          result.path = p;
        }
      } else {
        result.path = '/';
      }
    }

    // Fallback: Infer parent path
    if (!result.parent && result.path) {
      const parts = result.path.replace(/\\/g, '/').split('/').filter(Boolean);
      if (parts.length > 0) {
        parts.pop();
        const parentPath = parts.join('/');
        const isWindows = result.path.includes('\\') || /^[A-Za-z]:/.test(result.path);
        if (isWindows) {
          result.parent = parentPath ? parentPath.replace(/\//g, '\\') : result.path;
          if (result.parent === '') result.parent = result.path;
        } else {
          result.parent = parentPath ? '/' + parentPath : '/';
        }
      } else {
        result.parent = result.path;
      }
    }
    
    // Fallback: Filter hidden files in frontend since Rust backend currently ignores the showHidden parameter
    if (!showHidden && result && result.entries) {
      result.entries = result.entries.filter(e => !e.name.startsWith('.'));
    }
    
    return result;
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    if (error instanceof AgentApiError) {
      const message = error.message.trim();
      if (message.includes('ENOENT')) {
        throw new Error('` + i18n.t("bridge.dirNotFound") + `');
      }
      if (message.includes('EACCES') || message.includes('EPERM')) {
        throw new Error('` + i18n.t("bridge.dirNoAccess") + `');
      }
    }
    throw error;
  }
}

export async function getWorkspaceSessions(
  workspaceId: string,
  preferredSessionId?: string,
): Promise<AgentWorkspaceSessionsSnapshot> {
  try {
    const query = new URLSearchParams({ workspaceId });
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
      sessionId: payload.sessionId?.trim() || preferredSessionId?.trim() || sessions[0]?.id || '',
      sessions,
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    throw error;
  }
}

export async function loadAgentSessionTimelinePage(
  sessionId: string,
  options: { limit?: number; beforeCursor?: string } = {},
): Promise<MessagesResponseDto> {
  try {
    const query = new URLSearchParams({
      sessionId: sessionId.trim(),
      limit: String(options.limit ?? 50),
    });
    const binding = readStoredBrowserWorkspaceBinding();
    if (binding.workspaceId?.trim()) {
      query.set('workspaceId', binding.workspaceId.trim());
    }
    if (options.beforeCursor?.trim()) {
      query.set('beforeCursor', options.beforeCursor.trim());
    }
    const response = await getTransport().request(agentUrl('/api/messages', query.toString()));
    return await parseAgentJson<MessagesResponseDto>(response, 'load session timeline');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    throw error;
  }
}

export async function deleteAgentSession(sessionId: string): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>('/api/session/delete', { sessionId }, 'delete session');
}

export async function renameAgentSession(sessionId: string, name: string): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>('/api/session/rename', { sessionId, name }, 'rename session');
}

export async function closeAgentSession(sessionId: string): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>('/api/session/close', { sessionId }, 'close session');
}

export async function saveAgentCurrentSession(): Promise<AgentBootstrapSnapshot> {
  return await postBoundJson<AgentBootstrapSnapshot>('/api/session/save', {}, 'save current session');
}

export async function getAgentSessionNotifications(): Promise<AgentSessionNotificationsPayload> {
  const query = buildBoundQuery({});
  const response = await getTransport().request(agentUrl('/api/session/notifications', query));
  return await parseAgentJson<AgentSessionNotificationsPayload>(response, 'load session notifications');
}

export async function markAllAgentNotificationsRead(): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/mark-all-read',
    {},
    'mark all notifications read',
  );
}

export async function clearAgentNotifications(): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/clear',
    {},
    'clear notifications',
  );
}

export async function removeAgentNotification(notificationId: string): Promise<AgentSessionNotificationsPayload> {
  return await postBoundJson<AgentSessionNotificationsPayload>(
    '/api/session/notifications/remove',
    { notificationId },
    'remove notification',
  );
}

export async function submitAgentSessionTurn(
  payload: {
    text?: string | null;
    deepTask: boolean;
    skillName?: string | null;
    images: AgentSessionTurnImagePayload[];
  },
  bindingOverride?: Partial<AgentBindingContext>,
): Promise<AgentSessionTurnResult> {
  return await postBoundJson<AgentSessionTurnResult>(
    '/api/session/turn',
    {
      text: payload.text ?? null,
      deepTask: payload.deepTask,
      skillName: payload.skillName ?? null,
      images: payload.images,
    },
    'submit session turn',
    bindingOverride,
  );
}

export async function interruptAgentTask(
  payload: { taskId: string },
): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/interrupt', payload, 'interrupt task');
}

export async function continueAgentSession(
  sessionId: string,
  options: { promptText?: string | null } = {},
): Promise<{
  sessionId: string;
  missionId: string;
  rootTaskId: string;
  executionChainRef: string;
  resumedBranchCount: number;
  status: string;
  runnerStarted: boolean;
  eventId: string;
  continuedAt: number;
}> {
  return await postBoundJson<{
    sessionId: string;
    missionId: string;
    rootTaskId: string;
    executionChainRef: string;
    resumedBranchCount: number;
    status: string;
    runnerStarted: boolean;
    eventId: string;
    continuedAt: number;
  }>(
    '/api/session/continue',
    {
      sessionId,
      ...(typeof options.promptText === 'string' ? { promptText: options.promptText } : {}),
    },
    'continue session',
  );
}

export async function getAgentSettingsBootstrap(
  options: { scope?: 'core' | 'full' } = {},
): Promise<AgentSettingsBootstrapSnapshot> {
  try {
    const query = buildBoundQuery(options.scope === 'core' ? { scope: 'core' } : {});
    const response = await getTransport().request(agentUrl('/api/settings/bootstrap', query));
    const payload = await parseAgentJson<Record<string, unknown>>(response, 'load settings bootstrap');
    return normalizeSettingsBootstrapPayload(payload);
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    throw error;
  }
}

export async function getAgentStatus(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/status'));
  return parseAgentJson<Record<string, unknown>>(response, 'get status');
}

export async function resetAgentExecutionStats(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/stats/reset', {}, 'reset execution stats');
}

export async function getAgentExecutionStats(): Promise<AgentExecutionStatsPayload> {
  try {
    const query = buildBoundQuery({});
    const response = await getTransport().request(agentUrl('/api/settings/stats/session', query));
    return await parseAgentJson<AgentExecutionStatsPayload>(response, 'load execution stats');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    throw error;
  }
}

export async function enhanceAgentPrompt(prompt: string): Promise<{ enhancedPrompt: string; error?: string }> {
  return await postBoundJson<{ enhancedPrompt: string; error?: string }>('/api/prompt/enhance', { prompt }, 'enhance prompt');
}

export async function updateAgentRuntimeSetting(key: string, value: unknown): Promise<AgentRuntimeSettings> {
  const payload = await postBoundJson<AgentRuntimeSettings>('/api/settings/update', { key, value }, 'update runtime setting');
  if (key === 'locale' && (payload?.locale === 'zh-CN' || payload?.locale === 'en-US')) {
    safeWriteLocalStorage('magi-locale', payload.locale);
    i18n.setLocale(payload.locale);
  }
  return payload;
}

export async function saveAgentWorkerConfig(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/worker/save', { worker, config }, 'save worker config');
}

export async function saveAgentUserRules(data: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/user-rules/save', data, 'save user rules');
}

export async function saveAgentOrchestratorConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/orchestrator/save', config, 'save orchestrator config');
}

export async function saveAgentAuxiliaryConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/auxiliary/save', config, 'save auxiliary config');
}

export async function removeAgentWorkerConfig(worker: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/worker/remove', { worker }, 'remove worker config');
}

export async function testAgentWorkerConnection(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/worker/test', { worker, config }, 'test worker connection');
}

export async function testAgentOrchestratorConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/orchestrator/test', config, 'test orchestrator connection');
}

export async function testAgentAuxiliaryConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/auxiliary/test', config, 'test auxiliary connection');
}


export async function listAgentRoleTemplates(): Promise<RoleTemplate[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/role-templates', buildBoundQuery({})));
  const payload = await parseAgentJson<{ templates?: RoleTemplate[] }>(response, 'load role templates');
  return Array.isArray(payload.templates) ? payload.templates : [];
}

export async function listAgentRegistryEngines(): Promise<ModelEngine[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/engines', buildBoundQuery({})));
  const payload = await parseAgentJson<{ engines?: ModelEngine[] }>(response, 'load registry engines');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function listAgentRegistryAgents(): Promise<AgentBinding[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/agents', buildBoundQuery({})));
  const payload = await parseAgentJson<{ agents?: AgentBinding[] }>(response, 'load registry agents');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function upsertAgentRegistryEngine(engine: ModelEngine): Promise<ModelEngine[]> {
  const payload = await postBoundJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/upsert', engine as unknown as Record<string, unknown>, 'upsert registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function removeAgentRegistryEngine(engineId: string): Promise<ModelEngine[]> {
  const payload = await postBoundJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/remove', { engineId }, 'remove registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function upsertAgentRegistryBinding(agent: AgentBinding): Promise<AgentBinding[]> {
  const payload = await postBoundJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/upsert', agent as unknown as Record<string, unknown>, 'upsert registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function removeAgentRegistryBinding(templateId: string): Promise<AgentBinding[]> {
  const payload = await postBoundJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/remove', { templateId }, 'remove registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function fetchAgentModelList(config: Record<string, unknown>, target: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/models/fetch', { config, target }, 'fetch model list');
}

export async function clearAgentProjectKnowledge(): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/clear', {}, 'clear project knowledge');
}

export async function getAgentProjectKnowledge(): Promise<Record<string, unknown>> {
  const query = buildBoundQuery({});
  const response = await getTransport().request(agentUrl('/api/knowledge', query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load project knowledge');
}

export async function getAgentAdrs(filter?: { status?: string }): Promise<Record<string, unknown>> {
  const query = buildBoundQuery(filter?.status ? { status: filter.status } : {});
  const response = await getTransport().request(agentUrl(`/api/knowledge/adrs`, query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load adrs');
}

export async function getAgentFaqs(filter?: { category?: string }): Promise<Record<string, unknown>> {
  const query = buildBoundQuery(filter?.category ? { category: filter.category } : {});
  const response = await getTransport().request(agentUrl(`/api/knowledge/faqs`, query));
  return await parseAgentJson<Record<string, unknown>>(response, 'load faqs');
}

export async function searchAgentFaqs(keyword: string): Promise<Record<string, unknown>> {
  const query = buildBoundQuery({ q: keyword });
  const response = await getTransport().request(agentUrl(`/api/knowledge/faqs/search`, query));
  return await parseAgentJson<Record<string, unknown>>(response, 'search faqs');
}

export async function addAgentAdr(adr: Record<string, unknown>): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/adr/add', { adr }, 'add adr');
}

export async function updateAgentAdr(id: string, updates: Record<string, unknown>): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/adr/update', { id, updates }, 'update adr');
}

export async function addAgentFaq(faq: Record<string, unknown>): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/faq/add', { faq }, 'add faq');
}

export async function updateAgentFaq(id: string, updates: Record<string, unknown>): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/faq/update', { id, updates }, 'update faq');
}

export async function deleteAgentAdr(id: string): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/adr/delete', { id }, 'delete adr');
}

export async function deleteAgentFaq(id: string): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/faq/delete', { id }, 'delete faq');
}

export async function deleteAgentLearning(id: string): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/learning/delete', { id }, 'delete learning');
}

export async function loadAgentMcpServers(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/mcp'));
  return await parseAgentJson<Record<string, unknown>>(response, 'load mcp servers');
}

export async function addAgentMcpServer(server: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/add', normalizeMcpServerConfig(server), 'add mcp server');
}

export async function updateAgentMcpServer(serverId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>(
    '/api/settings/mcp/update',
    normalizeMcpServerConfig({ ...updates, id: serverId, serverId }),
    'update mcp server',
  );
}

export async function deleteAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/delete', { serverId }, 'delete mcp server');
}

export async function getAgentMcpServerTools(serverId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/tools', { serverId }, 'get mcp server tools');
}

export async function refreshAgentMcpTools(serverId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/tools/refresh', { serverId }, 'refresh mcp tools');
}

export async function connectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/connect', { serverId }, 'connect mcp server');
}

export async function disconnectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/disconnect', { serverId }, 'disconnect mcp server');
}

export async function loadAgentRepositories(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/repositories'));
  return await parseAgentJson<Record<string, unknown>>(response, 'load repositories');
}

export async function addAgentRepository(url: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/repositories/add', { url }, 'add repository');
}

export async function updateAgentRepository(repositoryId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/repositories/update', { repositoryId, updates }, 'update repository');
}

export async function deleteAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/repositories/delete', { repositoryId }, 'delete repository');
}

export async function refreshAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/repositories/refresh', { repositoryId }, 'refresh repository');
}

export async function loadAgentSkillLibrary(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/settings/skills/library'));
  return await parseAgentJson<Record<string, unknown>>(response, 'load skill library');
}

export async function installAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/install', { skillId }, 'install skill');
}

export async function installAgentLocalSkill(directoryPath?: string): Promise<Record<string, unknown>> {
  const payload: Record<string, unknown> = {};
  if (directoryPath) {
    payload.directoryPath = directoryPath;
  }
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/install-local', payload, 'install local skill');
}

export async function scanAgentLocalSkillDirectory(directoryPath: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/scan-local', { directoryPath }, 'scan local skill directory');
}

export async function saveAgentSkillsConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/config/save', config, 'save skills config');
}

export async function saveAgentSafeguardConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/safeguard/save', config, 'save safeguard config');
}

export async function addAgentCustomTool(tool: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/custom-tool/add', tool, 'add custom tool');
}

export async function removeAgentCustomTool(toolName: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/custom-tool/remove', { toolName }, 'remove custom tool');
}

export async function removeAgentInstructionSkill(skillName: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/instruction/remove', { skillName }, 'remove instruction skill');
}

export async function updateAgentSkill(skillName: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/update', { skillName }, 'update skill');
}

export async function updateAllAgentSkills(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/update-all', {}, 'update all skills');
}

export async function getAgentChangeDiff(filePath: string): Promise<AgentChangeDiffPayload> {
  try {
    const query = buildBoundQuery({ filePath });
    const response = await getTransport().request(agentUrl(`/api/changes/diff`, query));
    return await parseAgentJson<AgentChangeDiffPayload>(response, 'load change diff');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    throw error;
  }
}

export async function getAgentFilePreview(filePath: string): Promise<AgentFilePreviewPayload> {
  try {
    const query = buildBoundQuery({ filePath });
    const response = await getTransport().request(agentUrl(`/api/files/content`, query));
    return await parseAgentJson<AgentFilePreviewPayload>(response, 'load file preview');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('` + i18n.t("bridge.agentUnreachable") + `');
    }
    throw error;
  }
}

export async function approveAgentChange(filePath: string, sessionId?: string): Promise<void> {
  await postBoundJson('/api/changes/approve', { filePath }, 'approve change', { sessionId });
}

export async function revertAgentChange(filePath: string, sessionId?: string): Promise<void> {
  await postBoundJson('/api/changes/revert', { filePath }, 'revert change', { sessionId });
}

export async function approveAllAgentChanges(sessionId?: string): Promise<void> {
  await postBoundJson('/api/changes/approve-all', {}, 'approve all changes', { sessionId });
}

export async function revertAllAgentChanges(sessionId?: string): Promise<void> {
  await postBoundJson('/api/changes/revert-all', {}, 'revert all changes', { sessionId });
}

export async function revertAgentExecutionGroupChanges(
  executionGroupId: string,
  sessionId?: string,
): Promise<void> {
  await postBoundJson(
    '/api/changes/revert-execution-group',
    { executionGroupId },
    'revert execution group changes',
    { sessionId },
  );
}
