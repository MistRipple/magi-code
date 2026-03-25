import * as os from 'os';
import {
  buildAgentWebClientUrl,
  DEFAULT_AGENT_PORT,
  type AgentWebClientBinding,
} from '../shared/agent-shared-config';

export interface LanAccessInfo {
  url: string;
  ip: string;
  port: number;
  workspacePath: string | null;
  workspaceId: string | null;
  sessionId: string | null;
}

function scoreLanCandidate(interfaceName: string, address: string): number {
  let score = 0;
  if (/^(en|eth|wlan|wi-?fi)/i.test(interfaceName)) score += 50;
  if (/^(bridge|docker|veth|utun|tun|tap|vmnet|lo)/i.test(interfaceName)) score -= 100;
  if (address.startsWith('192.168.')) score += 30;
  else if (address.startsWith('10.')) score += 20;
  else if (/^172\.(1[6-9]|2\d|3[0-1])\./.test(address)) score += 10;
  else score -= 20;
  return score;
}

export function resolvePreferredLanIPv4(): string {
  const interfaces = os.networkInterfaces();
  const candidates: Array<{ interfaceName: string; address: string; score: number }> = [];

  for (const [interfaceName, entries] of Object.entries(interfaces)) {
    const normalizedEntries = Array.isArray(entries) ? entries : [];
    for (const entry of normalizedEntries) {
      const family = String((entry as { family?: string | number }).family);
      if ((family !== 'IPv4' && family !== '4') || entry.internal || !entry.address) {
        continue;
      }
      candidates.push({
        interfaceName,
        address: entry.address,
        score: scoreLanCandidate(interfaceName, entry.address),
      });
    }
  }

  if (candidates.length === 0) {
    return '127.0.0.1';
  }

  candidates.sort((left, right) => right.score - left.score || left.interfaceName.localeCompare(right.interfaceName));
  return candidates[0].address;
}

export function buildLanAccessInfo(binding?: AgentWebClientBinding, port = DEFAULT_AGENT_PORT): LanAccessInfo {
  const ip = resolvePreferredLanIPv4();
  return {
    url: buildAgentWebClientUrl(`http://${ip}:${port}`, binding),
    ip,
    port,
    workspacePath: binding?.workspacePath ?? null,
    workspaceId: binding?.workspaceId ?? null,
    sessionId: binding?.sessionId ?? null,
  };
}

