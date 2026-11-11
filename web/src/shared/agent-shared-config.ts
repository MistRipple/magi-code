export const DEFAULT_AGENT_PORT = 38123;
export const DEFAULT_AGENT_HOST = '127.0.0.1';

export function getDefaultAgentBaseUrl(): string {
  return `http://${DEFAULT_AGENT_HOST}:${DEFAULT_AGENT_PORT}`;
}
