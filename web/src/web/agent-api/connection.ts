import { getDefaultAgentBaseUrl } from '../../shared/agent-shared-config';
import { getTransport } from '../../shared/transport';
import type { SettingsBootstrapPayload } from '../../shared/settings-bootstrap';
import { resolveAgentBindingContext } from '../agent-binding-context';
import { agentUrl, getConfiguredAgentBaseUrl, getConfiguredAgentProxyTarget, getStoredAgentBaseUrl, normalizeBindingString, parseAgentJson, persistAgentBaseUrl } from './core';
import type { AgentConnectionEventDetail, BrowserAnnotationStatus, TerminalSessionRequest } from './types';

const AGENT_PROBE_TIMEOUT_MS = 1500;

export const RUNTIME_CONNECTION_EVENT = 'magi-runtime-connection';

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
  // daemon 提供的页面绑定该实例。断线时不能探测默认端口或其它实例，
  // 否则独立状态根的 Desktop/Web 会在恢复期间切换到用户日常 daemon。
  if (servedByAgentOrigin) return [servedByAgentOrigin];
  const candidates = [
    queryBaseUrl,
    injectedBaseUrl,
    configuredProxyTarget,
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

export function isPublicTunnelAccess(): boolean {
  if (typeof window === 'undefined') return false;
  const currentUrl = new URL(window.location.href);
  return Boolean(currentUrl.searchParams.get('tunnel_token'));
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
  if (hasWorkspaceBinding && request.sourceFolderId) url.searchParams.set('sourceFolderId', request.sourceFolderId);
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

export const BROWSER_AUTHORITY_CHANGED_EVENT = 'magi:browserAuthorityChanged';

/** 历史 artifact 仍可引用；只有明确删除的标记退出消息上下文。 */
export function isReferenceableBrowserAnnotation(
  status: BrowserAnnotationStatus,
): boolean {
  return status !== 'deleted';
}
