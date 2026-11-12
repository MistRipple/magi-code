import { getTransport } from '../../shared/transport';
import type { McpDirectoryRequest, McpExternalRetention, McpExternalSession, McpOAuthRequest, McpServerApproval, McpServerAuditPage, McpServerConfigSnippets, McpServerCreateTokenRequest, McpServerCreateTokenResponse, McpServerDirectRequest, McpServerNamedTunnelCheck, McpServerStatus, McpServerUpdateTokenRequest } from './types';
import { agentUrl, parseAgentJson } from './core';

export function listMcpDirectoryRequests(): Promise<McpDirectoryRequest[]> {
  return mcpServerJson('/api/mcp-server/directory-requests', 'list MCP directory requests');
}

export async function resolveMcpDirectoryRequest(sessionId: string, directory: { workspaceId: string; sourceFolderId: string } | null): Promise<void> {
  await mcpServerJson(`/api/mcp-server/directory-requests/${encodeURIComponent(sessionId)}`, 'select MCP conversation directory', {
    method: 'POST', body: directory ?? { workspaceId: null, sourceFolderId: null },
  });
}

async function mcpServerJson<T>(
  path: string,
  action: string,
  init?: { method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'; body?: unknown },
): Promise<T> {
  const response = await getTransport().request(agentUrl(path), {
    method: init?.method ?? 'GET',
    headers: { 'content-type': 'application/json' },
    cache: 'no-store',
    ...(init?.body === undefined ? {} : { body: JSON.stringify(init.body) }),
  });
  return parseAgentJson<T>(response, action);
}

export function getMcpServerStatus(): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/status', 'load mcp server status');
}

export function listMcpOAuthRequests(): Promise<McpOAuthRequest[]> {
  return mcpServerJson('/api/mcp-server/oauth/requests', 'list MCP OAuth requests');
}

export function resolveMcpOAuthRequest(requestId: string, tokenId: string | null): Promise<{ ok: boolean }> {
  return mcpServerJson(`/api/mcp-server/oauth/requests/${encodeURIComponent(requestId)}`, 'resolve MCP OAuth request', {
    method: 'POST', body: { tokenId },
  });
}

export function setMcpServerEnabled(enabled: boolean): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/enabled', 'set mcp server enabled', {
    method: 'POST',
    body: { enabled },
  });
}

export function setMcpServerNetwork(
  enabled: boolean,
  confirmRisk: boolean,
): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/network', 'set mcp server network mode', {
    method: 'POST',
    body: { enabled, confirmRisk },
  });
}

/** 直接对公网开放（监听公网地址，不经隧道）。 */
export function setMcpServerDirect(request: McpServerDirectRequest): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/direct', 'set mcp server direct access', {
    method: 'PUT',
    body: request,
  });
}

/** 保存命名隧道：域名 + 隧道令牌（或 Cloudflare 复制的整条安装命令）。令牌不会再被返回。 */
export function saveMcpServerNamedTunnel(
  hostname: string,
  token: string,
): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/named-tunnel', 'save mcp server named tunnel', {
    method: 'PUT',
    body: { hostname, token },
  });
}

export function clearMcpServerNamedTunnel(): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/named-tunnel', 'clear mcp server named tunnel', {
    method: 'DELETE',
  });
}

export function verifyMcpServerNamedTunnel(): Promise<McpServerNamedTunnelCheck> {
  return mcpServerJson('/api/mcp-server/named-tunnel/verify', 'verify mcp server named tunnel', {
    method: 'POST',
    body: {},
  });
}

export function createMcpServerToken(
  request: McpServerCreateTokenRequest,
): Promise<McpServerCreateTokenResponse> {
  return mcpServerJson('/api/mcp-server/tokens', 'create mcp server token', {
    method: 'POST',
    body: request,
  });
}

export function revokeMcpServerToken(tokenId: string): Promise<McpServerStatus> {
  return mcpServerJson(
    `/api/mcp-server/tokens/${encodeURIComponent(tokenId)}`,
    'revoke mcp server token',
    { method: 'DELETE' },
  );
}

export function revokeAllMcpServerTokens(): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/tokens/revoke-all', 'revoke all mcp server tokens', {
    method: 'POST',
    body: {},
  });
}

export function updateMcpServerToken(
  tokenId: string,
  request: McpServerUpdateTokenRequest,
): Promise<McpServerStatus> {
  return mcpServerJson(
    `/api/mcp-server/tokens/${encodeURIComponent(tokenId)}`,
    'update mcp server token',
    { method: 'PATCH', body: request },
  );
}

export async function getMcpServerTokenSecret(tokenId: string): Promise<string> {
  const result = await mcpServerJson<{ secret: string }>(
    `/api/mcp-server/tokens/${encodeURIComponent(tokenId)}/secret`,
    'view mcp server token secret',
  );
  return result.secret;
}

export function rotateMcpServerToken(tokenId: string): Promise<McpServerCreateTokenResponse> {
  return mcpServerJson(
    `/api/mcp-server/tokens/${encodeURIComponent(tokenId)}/rotate`,
    'rotate mcp server token',
    { method: 'POST', body: {} },
  );
}

export function getMcpServerConfigSnippets(tokenId?: string): Promise<McpServerConfigSnippets> {
  const suffix = tokenId ? `?tokenId=${encodeURIComponent(tokenId)}` : '';
  return mcpServerJson(
    `/api/mcp-server/config-snippets${suffix}`,
    'load mcp server config snippets',
  );
}

export async function listMcpServerApprovals(): Promise<McpServerApproval[]> {
  const result = await mcpServerJson<{ approvals: McpServerApproval[] }>(
    '/api/mcp-server/approvals',
    'load mcp server approvals',
  );
  return result.approvals;
}

export async function resolveMcpServerApproval(
  approvalId: string,
  decision: 'allow_once' | 'deny',
): Promise<McpServerApproval[]> {
  const result = await mcpServerJson<{ approvals: McpServerApproval[] }>(
    '/api/mcp-server/approvals/resolve',
    'resolve mcp server approval',
    { method: 'POST', body: { approvalId, decision } },
  );
  return result.approvals;
}

export function listMcpServerAudit(
  options: { limit?: number; offset?: number; tokenId?: string } = {},
): Promise<McpServerAuditPage> {
  const query = new URLSearchParams();
  if (options.limit) query.set('limit', String(options.limit));
  if (options.offset) query.set('offset', String(options.offset));
  if (options.tokenId) query.set('tokenId', options.tokenId);
  const suffix = query.toString();
  return mcpServerJson<McpServerAuditPage>(
    `/api/mcp-server/audit${suffix ? `?${suffix}` : ''}`,
    'load mcp server audit',
  );
}

export function listMcpExternalSessions(): Promise<{ sessions: McpExternalSession[] }> {
  return mcpServerJson('/api/mcp-server/sessions', 'load mcp external sessions');
}

export function setMcpExternalRetention(retention: McpExternalRetention): Promise<McpServerStatus> {
  return mcpServerJson('/api/mcp-server/external', 'set mcp external retention', {
    method: 'PUT',
    body: retention,
  });
}

/** 清理调用记录：带 tokenId 只清理该客户端的，否则清空全部。 */
export function clearMcpServerAudit(tokenId?: string): Promise<{ removed: number }> {
  const suffix = tokenId ? `?tokenId=${encodeURIComponent(tokenId)}` : '';
  return mcpServerJson<{ removed: number }>(
    `/api/mcp-server/audit${suffix}`,
    'clear mcp server audit',
    { method: 'DELETE' },
  );
}
