export type McpDisplaySummary =
  | { kind: 'checking' }
  | { kind: 'not_configured' }
  | { kind: 'disabled' }
  | { kind: 'connected'; connected: number; enabled: number }
  | { kind: 'partial'; connected: number; enabled: number }
  | { kind: 'disconnected' };

export function humanizeIdentifier(value: string): string {
  return value
    .trim()
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[._-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .toLowerCase();
}

export function getBuiltinToolFallbackLabel(name: string): string {
  return humanizeIdentifier(name) || 'unnamed built-in tool';
}

export function getCapabilityDependencyFallbackLabel(name: string): string {
  return humanizeIdentifier(name) || 'unnamed capability';
}

export function summarizeMcpServers(
  servers: Array<{
    enabled?: boolean;
    connected?: boolean;
    health?: string;
    error?: string;
  }>,
  hydrated: boolean,
  loading: boolean,
): McpDisplaySummary {
  if (loading || !hydrated) {
    return { kind: 'checking' };
  }
  if (servers.length === 0) {
    return { kind: 'not_configured' };
  }

  const enabledServers = servers.filter((server) => server.enabled !== false);
  if (enabledServers.length === 0) {
    return { kind: 'disabled' };
  }

  const connectedServers = enabledServers.filter(
    (server) => server.connected === true || server.health === 'connected',
  );
  if (connectedServers.length === enabledServers.length) {
    return { kind: 'connected', connected: connectedServers.length, enabled: enabledServers.length };
  }
  if (connectedServers.length > 0) {
    return { kind: 'partial', connected: connectedServers.length, enabled: enabledServers.length };
  }

  const hasConnectionErrors = enabledServers.some((server) => Boolean(server.error));
  return hasConnectionErrors ? { kind: 'disconnected' } : { kind: 'checking' };
}
