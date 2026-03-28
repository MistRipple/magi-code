import { getDefaultAgentBaseUrl } from '../../../../shared/agent-shared-config';
import { getTransport } from '../../../shared/transport';

const AGENT_BASE_URL_STORAGE_KEY = 'magi-agent-base-url';
const AGENT_PROBE_TIMEOUT_MS = 1500;

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

export interface AgentRuntimeSettings {
  locale: 'zh-CN' | 'en-US';
  deepTask: boolean;
}

export interface AgentExecutionStatsCatalogEntry {
  id: string;
  label: string;
  model?: string;
  provider?: string;
  enabled?: boolean;
  role?: 'worker' | 'orchestrator' | 'auxiliary';
}

export interface AgentExecutionStatsItem {
  worker: string;
  provider: 'openai' | 'anthropic' | 'unknown';
  totalExecutions: number;
  successCount: number;
  failureCount: number;
  successRate: number;
  avgDuration: number;
  isHealthy: boolean;
  healthScore: number;
  lastError?: string;
  lastExecutionTime?: number;
  totalInputTokens: number;
  totalOutputTokens: number;
}

export interface AgentExecutionStatsPayload {
  stats: AgentExecutionStatsItem[];
  orchestratorStats: {
    totalTasks: number;
    totalSuccess: number;
    totalFailed: number;
    totalInputTokens: number;
    totalOutputTokens: number;
    totalTokens: number;
  };
  modelCatalog: AgentExecutionStatsCatalogEntry[];
}

export interface AgentSessionNotificationsPayload {
  sessionId: string;
  notifications: {
    lastUpdatedAt: number;
    records: Array<Record<string, unknown>>;
  };
}


export interface AgentLanAccessInfo {
  url: string;
  ip: string;
  port: number;
  workspacePath: string | null;
  workspaceId: string | null;
  sessionId: string | null;
}

export interface AgentTunnelState {
  status: 'stopped' | 'starting' | 'running' | 'stopping' | 'installing' | 'error';
  publicUrl: string | null;
  accessUrl: string | null;
  token: string | null;
  error: string | null;
}

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

export interface AgentTaskSubmissionResult {
  success: boolean;
  accepted: boolean;
  requestId?: string;
  sessionId?: string;
  taskId?: string;
  error?: string;
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

function collectAgentBaseUrlCandidates(): string[] {
  if (typeof window === 'undefined') {
    return [getDefaultAgentBaseUrl()];
  }
 const injectedBaseUrl = (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim() || '';
  const currentUrl = new URL(window.location.href);
  const queryBaseUrl = currentUrl.searchParams.get('agentBaseUrl')?.trim() || '';
  const servedByAgentOrigin = currentUrl.protocol.startsWith('http')
    && (currentUrl.pathname === '/' || currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/'))
    ? currentUrl.origin
    : '';
  const candidates = [
   injectedBaseUrl,
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
        const payload = await response.json() as { error?: string };
        if (typeof payload?.error === 'string' && payload.error.trim()) {
          backendError = payload.error.trim();
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
    throw new Error('当前页面尚未连接 Local Agent。请先启动 Agent，或通过 Agent 托管地址访问 Web。');
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
  const fromQuery = currentUrl.searchParams.get('agentBaseUrl')?.trim();
  if (fromQuery) {
    persistAgentBaseUrl(fromQuery);
    return fromQuery;
  }
  const servedByAgent = currentUrl.protocol.startsWith('http')
    && (currentUrl.pathname === '/' || currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/'));
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
  return {
    workspaceId: currentUrl.searchParams.get('workspaceId')?.trim()
      || bootstrapWindow.__INITIAL_WORKSPACE_ID__?.trim()
      || safeReadLocalStorage('magi-workspace-id')
      || '',
    workspacePath: currentUrl.searchParams.get('workspacePath')?.trim()
      || bootstrapWindow.__INITIAL_WORKSPACE_PATH__?.trim()
      || safeReadLocalStorage('magi-workspace-path')
      || '',
    sessionId: currentUrl.searchParams.get('sessionId')?.trim()
      || safeReadLocalStorage('magi-session-id')
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

async function postBoundJson<T>(pathname: string, payload: Record<string, unknown>, action: string): Promise<T> {
  try {
    const binding = resolveAgentBindingContext();
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
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function listAgentWorkspaces(): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces'));
    const payload = await parseAgentJson<{ workspaces?: AgentWorkspaceSummary[] }>(response, 'list workspaces');
    return Array.isArray(payload.workspaces) ? payload.workspaces : [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function registerAgentWorkspace(rootPath: string, name?: string): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/register'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaces: [{ rootPath, name }],
      }),
    });
    const payload = await parseAgentJson<{ workspaces?: AgentWorkspaceSummary[] }>(response, 'register workspace');
    return Array.isArray(payload.workspaces) ? payload.workspaces : [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
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
    const payload = await parseAgentJson<{ workspaces?: AgentWorkspaceSummary[] }>(response, 'remove workspace');
    return Array.isArray(payload.workspaces) ? payload.workspaces : [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function renameAgentWorkspace(workspaceId: string, workspacePath: string, name: string): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/rename'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaceId,
        workspacePath,
        name,
      }),
    });
    const payload = await parseAgentJson<{ workspaces?: AgentWorkspaceSummary[] }>(response, 'rename workspace');
    return Array.isArray(payload.workspaces) ? payload.workspaces : [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
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
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export interface DirectoryEntry {
  name: string;
  path: string;
  hasChildren: boolean;
}

export interface DirectoryListResult {
  path: string;
  parent: string;
  entries: DirectoryEntry[];
  error?: string;
}

export async function listAgentDirectory(dirPath?: string): Promise<DirectoryListResult> {
  const params = dirPath ? `?path=${encodeURIComponent(dirPath)}` : '';
  const response = await getTransport().request(agentUrl(`/api/filesystem/list${params}`));
  return await parseAgentJson<DirectoryListResult>(response, 'list directory');
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
    return await parseAgentJson<AgentWorkspaceSessionsSnapshot>(response, 'workspace sessions');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
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

export async function executeAgentTask(prompt: string, requestId?: string): Promise<AgentTaskSubmissionResult> {
  return await postBoundJson<AgentTaskSubmissionResult>('/api/task/execute', { prompt, requestId }, 'execute task');
}

export async function appendAgentTaskMessage(taskId: string, content: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/append', { taskId, content }, 'append task message');
}

export async function updateAgentQueuedTaskMessage(queueId: string, content: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/queued/update', { queueId, content }, 'update queued task message');
}

export async function deleteAgentQueuedTaskMessage(queueId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/queued/delete', { queueId }, 'delete queued task message');
}

export async function interruptAgentTask(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/interrupt', {}, 'interrupt task');
}

export async function clearAgentAllTasks(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/clear-all', {}, 'clear all tasks');
}

export async function startAgentTask(taskId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/start', { taskId }, 'start task');
}

export async function resumeAgentTask(taskId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/resume', { taskId }, 'resume task');
}

export async function deleteAgentTask(taskId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/task/delete', { taskId }, 'delete task');
}

export async function abandonAgentChain(chainId: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/chain/abandon', { chainId }, 'abandon chain');
}

export async function confirmAgentRecovery(decision: 'retry' | 'rollback' | 'continue'): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>(
    '/api/interaction/confirm-recovery',
    { decision },
    'confirm recovery',
  );
}

export async function respondAgentInteraction(requestId: string, response: unknown): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>(
    '/api/interaction/response',
    { requestId, response },
    'interaction response',
  );
}

export async function answerAgentClarification(
  answers: Record<string, string> | null,
  additionalInfo?: string | null,
): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>(
    '/api/interaction/clarification',
    { answers, additionalInfo: additionalInfo ?? null },
    'clarification answer',
  );
}

export async function answerAgentWorkerQuestion(answer: string | null): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>(
    '/api/interaction/worker-question',
    { answer },
    'worker question answer',
  );
}

export async function getAgentRuntimeSettings(): Promise<AgentRuntimeSettings> {
  try {
    const response = await getTransport().request(agentUrl('/api/settings/runtime'));
    return await parseAgentJson<AgentRuntimeSettings>(response, 'load runtime settings');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function getAgentStatus(): Promise<Record<string, unknown>> {
  const response = await getTransport().request(agentUrl('/api/status'));
  return parseAgentJson<Record<string, unknown>>(response, 'get status');
}

export async function getAgentLanAccessInfo(): Promise<AgentLanAccessInfo> {
  try {
    const query = buildBoundQuery({});
    const response = await getTransport().request(agentUrl(`/api/lan-access`, query));
    return await parseAgentJson<AgentLanAccessInfo>(response, 'lan access');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function startAgentTunnel(): Promise<AgentTunnelState> {
  return await postBoundJson<AgentTunnelState>('/api/tunnel/start', {}, 'start tunnel');
}

export async function stopAgentTunnel(): Promise<AgentTunnelState> {
  return await postBoundJson<AgentTunnelState>('/api/tunnel/stop', {}, 'stop tunnel');
}

export async function getAgentTunnelStatus(): Promise<AgentTunnelState> {
  const response = await getTransport().request(agentUrl('/api/tunnel/status'));
  return await parseAgentJson<AgentTunnelState>(response, 'tunnel status');
}

export async function resetAgentExecutionStats(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/stats/reset', {}, 'reset execution stats');
}

export async function getAgentExecutionStats(): Promise<AgentExecutionStatsPayload> {
  try {
    const query = buildBoundQuery({});
    const response = await getTransport().request(agentUrl('/api/settings/stats', query));
    return await parseAgentJson<AgentExecutionStatsPayload>(response, 'load execution stats');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function enhanceAgentPrompt(prompt: string): Promise<{ enhancedPrompt: string; error?: string }> {
  return await postBoundJson<{ enhancedPrompt: string; error?: string }>('/api/prompt/enhance', { prompt }, 'enhance prompt');
}

export async function updateAgentRuntimeSetting(key: string, value: unknown): Promise<AgentRuntimeSettings> {
  return await postBoundJson<AgentRuntimeSettings>('/api/settings/update', { key, value }, 'update runtime setting');
}

export async function saveAgentWorkerConfig(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/worker/save', { worker, config }, 'save worker config');
}

export async function saveAgentProfileConfig(data: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/profile/save', { data }, 'save profile config');
}

export async function resetAgentProfileConfig(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/profile/reset', {}, 'reset profile config');
}

export async function saveAgentOrchestratorConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/orchestrator/save', { config }, 'save orchestrator config');
}

export async function saveAgentAuxiliaryConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/auxiliary/save', { config }, 'save auxiliary config');
}

export async function testAgentWorkerConnection(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/worker/test', { worker, config }, 'test worker connection');
}

export async function testAgentOrchestratorConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/orchestrator/test', { config }, 'test orchestrator connection');
}

export async function testAgentAuxiliaryConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/auxiliary/test', { config }, 'test auxiliary connection');
}

export async function fetchAgentModelList(config: Record<string, unknown>, target: string): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/models/fetch', { config, target }, 'fetch model list');
}

export async function clearAgentProjectKnowledge(): Promise<AgentKnowledgeMutationPayload> {
  return await postBoundJson<AgentKnowledgeMutationPayload>('/api/knowledge/clear', {}, 'clear project knowledge');
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
  const query = buildBoundQuery({ keyword });
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
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/add', { server }, 'add mcp server');
}

export async function updateAgentMcpServer(serverId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/mcp/update', { serverId, updates }, 'update mcp server');
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

export async function installAgentLocalSkill(): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/install-local', {}, 'install local skill');
}

export async function saveAgentSkillsConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/config/save', { config }, 'save skills config');
}

export async function saveAgentSafeguardConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/safeguard/save', { config }, 'save safeguard config');
}

export async function addAgentCustomTool(tool: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postBoundJson<Record<string, unknown>>('/api/settings/skills/custom-tool/add', { tool }, 'add custom tool');
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
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
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
      throw new Error('无法连接 Local Agent。请确认 Agent 已启动，或检查当前访问地址是否正确。');
    }
    throw error;
  }
}

export async function approveAgentChange(filePath: string): Promise<void> {
  await postBoundJson('/api/changes/approve', { filePath }, 'approve change');
}

export async function revertAgentChange(filePath: string): Promise<void> {
  await postBoundJson('/api/changes/revert', { filePath }, 'revert change');
}

export async function approveAllAgentChanges(): Promise<void> {
  await postBoundJson('/api/changes/approve-all', {}, 'approve all changes');
}

export async function revertAllAgentChanges(): Promise<void> {
  await postBoundJson('/api/changes/revert-all', {}, 'revert all changes');
}

export async function revertAgentMissionChanges(missionId: string): Promise<void> {
  await postBoundJson('/api/changes/revert-mission', { missionId }, 'revert mission changes');
}
