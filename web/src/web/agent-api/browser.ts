import { getTransport } from '../../shared/transport';
import type { VersionHandshakeDto } from '../../shared/rust-backend-types';
import { agentUrl, browserClientPlatform, parseAgentJson } from './core';
import type { BrowserAnnotationSelection, BrowserAnnotationSnapshot, BrowserCapabilitiesSnapshot, BrowserCapabilitySettings, BrowserResourcesSnapshot, BrowserSessionSnapshot, BrowserTabSnapshot, ReclaimBrowserResourcesSnapshot } from './types';

/** 后端版本握手：产品版本与构建标识（关于页展示用）。 */
export async function getAgentVersion(): Promise<VersionHandshakeDto> {
  const response = await getTransport().request(agentUrl('/version'), { cache: 'no-store' });
  return parseAgentJson<VersionHandshakeDto>(response, 'load version');
}

export async function getBrowserCapabilities(): Promise<BrowserCapabilitiesSnapshot> {
  const queryParams = new URLSearchParams({ clientPlatform: browserClientPlatform() });
  const response = await getTransport().request(
    agentUrl('/api/browser/capabilities', queryParams.toString()),
    {
    cache: 'no-store',
    },
  );
  return parseAgentJson<BrowserCapabilitiesSnapshot>(response, 'load browser capabilities');
}

export async function updateBrowserSettings(
  settings: BrowserCapabilitySettings,
): Promise<BrowserCapabilitiesSnapshot> {
  const response = await getTransport().request(agentUrl('/api/browser/settings'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...settings, clientPlatform: browserClientPlatform() }),
  });
  return parseAgentJson<BrowserCapabilitiesSnapshot>(response, 'update browser settings');
}

export async function getBrowserResources(): Promise<BrowserResourcesSnapshot> {
  const response = await getTransport().request(
    agentUrl('/api/browser/resources'),
    { cache: 'no-store' },
  );
  return parseAgentJson<BrowserResourcesSnapshot>(response, 'load browser resources');
}

export async function reclaimBrowserResources(tabIds: string[]): Promise<ReclaimBrowserResourcesSnapshot> {
  const normalizedTabIds = Array.from(new Set(
    tabIds
      .filter((tabId): tabId is string => typeof tabId === 'string')
      .map((tabId) => tabId.trim())
      .filter(Boolean),
  ));
  const response = await getTransport().request(agentUrl('/api/browser/resources/reclaim'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ tabIds: normalizedTabIds }),
  });
  return parseAgentJson<ReclaimBrowserResourcesSnapshot>(response, 'reclaim browser resources');
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

export async function activateBrowserTab(tabId: string): Promise<BrowserSessionSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}/activate`),
    { method: 'POST' },
  );
  return parseAgentJson<BrowserSessionSnapshot>(response, 'activate browser tab');
}

/** 用户把页面控制权交还给代理。 */
export async function releaseBrowserUserControl(tabId: string): Promise<BrowserSessionSnapshot> {
  const response = await getTransport().request(
    agentUrl(`/api/browser/tabs/${encodeURIComponent(tabId)}/control/release`),
    { method: 'POST' },
  );
  return parseAgentJson<BrowserSessionSnapshot>(response, 'release browser user control');
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
  action: 'url' | 'back' | 'forward' | 'stop' | 'reload',
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

function browserAnnotationArtifactUrlForSession(annotationId: string, sessionId: string): string {
  const url = new URL(agentUrl(`/api/browser/annotations/${encodeURIComponent(annotationId)}/artifact`));
  url.searchParams.set('sessionId', sessionId.trim());
  return url.toString();
}

/**
 * 为消息所属 Magi 会话构造浏览器标记截图 artifact 地址。
 *
 * 消息的 sessionId 是稳定的历史归属；不能再通过当前仍运行的 Browser
 * Session 反查，因为浏览器运行时关闭或重启后，历史标记仍然应该可读。
 * 服务端会用标记自身保存的 browser_session_id 做最终归属校验。
 */
export function resolveBrowserAnnotationArtifactUrl(
  annotationId: string,
  sessionId: string,
): string {
  const normalizedAnnotationId = annotationId.trim();
  const normalizedSessionId = sessionId.trim();
  if (!normalizedAnnotationId || !normalizedSessionId) {
    throw new Error('浏览器标记预览缺少有效的 annotationId 或 sessionId');
  }
  return browserAnnotationArtifactUrlForSession(normalizedAnnotationId, normalizedSessionId);
}
