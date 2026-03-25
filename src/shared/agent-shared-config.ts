export const DEFAULT_AGENT_PORT = 46231;
export const DEFAULT_AGENT_HOST = '127.0.0.1';

export interface AgentWebClientBinding {
  workspacePath?: string | null;
  workspaceId?: string | null;
  sessionId?: string | null;
}

export function getDefaultAgentBaseUrl(): string {
  return `http://${DEFAULT_AGENT_HOST}:${DEFAULT_AGENT_PORT}`;
}

export function buildAgentWebClientUrl(baseUrl: string, binding?: AgentWebClientBinding): string {
  const url = new URL('/web.html', baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`);
  if (binding?.workspacePath) {
    url.searchParams.set('workspacePath', binding.workspacePath);
  }
  if (binding?.workspaceId) {
    url.searchParams.set('workspaceId', binding.workspaceId);
  }
  if (binding?.sessionId) {
    url.searchParams.set('sessionId', binding.sessionId);
  }
  return url.toString();
}

