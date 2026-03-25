import * as fs from 'fs';
import * as path from 'path';
import { AGENT_CLIENTS_DIR, AGENT_PID_FILE, AGENT_RUNTIME_FILE } from './config';
import { DEFAULT_AGENT_HOST, DEFAULT_AGENT_PORT, getDefaultAgentBaseUrl } from '../shared/agent-shared-config';
import { atomicWriteFileSync } from '../utils/atomic-write';

export interface AgentRuntimeState {
  pid: number;
  host: string;
  port: number;
  baseUrl: string;
  startedAt: number;
  updatedAt: number;
}

export interface AgentClientLease {
  clientId: string;
  pid: number;
  workspaceRoots: string[];
  createdAt: number;
  updatedAt: number;
}

const AGENT_CLIENT_LEASE_STALE_MS = 60_000;

function ensureParentDir(filePath: string): void {
  const dir = path.dirname(filePath);
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
}

function normalizePositiveInteger(value: unknown): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return null;
  }
  const normalized = Math.floor(value);
  return normalized > 0 ? normalized : null;
}

function normalizeHost(value: unknown): string {
  return typeof value === 'string' && value.trim()
    ? value.trim()
    : DEFAULT_AGENT_HOST;
}

function normalizeBaseUrl(value: unknown, host: string, port: number): string {
  if (typeof value === 'string' && value.trim()) {
    return value.trim();
  }
  return `http://${host}:${port}`;
}

function normalizeClientId(value: unknown): string | null {
  if (typeof value !== 'string') {
    return null;
  }
  const normalized = value.trim();
  if (!normalized) {
    return null;
  }
  return normalized.replace(/[^a-zA-Z0-9._-]/g, '_');
}

function normalizeWorkspaceRoots(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value
    .filter((item): item is string => typeof item === 'string')
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

function ensureDir(dirPath: string): void {
  if (!fs.existsSync(dirPath)) {
    fs.mkdirSync(dirPath, { recursive: true });
  }
}

function getClientLeaseFile(clientId: string): string {
  const normalizedClientId = normalizeClientId(clientId);
  if (!normalizedClientId) {
    throw new Error('无效的 agent clientId');
  }
  return path.join(AGENT_CLIENTS_DIR, `${normalizedClientId}.json`);
}

function normalizeClientLease(raw: Record<string, unknown>): AgentClientLease | null {
  const clientId = normalizeClientId(raw.clientId);
  const pid = normalizePositiveInteger(raw.pid);
  const createdAt = normalizePositiveInteger(raw.createdAt);
  const updatedAt = normalizePositiveInteger(raw.updatedAt) || createdAt;
  if (!clientId || !pid || !createdAt || !updatedAt) {
    return null;
  }
  return {
    clientId,
    pid,
    workspaceRoots: normalizeWorkspaceRoots(raw.workspaceRoots),
    createdAt,
    updatedAt,
  };
}

export function readAgentRuntimeState(): AgentRuntimeState | null {
  try {
    if (!fs.existsSync(AGENT_RUNTIME_FILE)) {
      return null;
    }
    const parsed = JSON.parse(fs.readFileSync(AGENT_RUNTIME_FILE, 'utf8')) as Record<string, unknown>;
    const pid = normalizePositiveInteger(parsed.pid);
    const port = normalizePositiveInteger(parsed.port);
    const startedAt = normalizePositiveInteger(parsed.startedAt);
    const updatedAt = normalizePositiveInteger(parsed.updatedAt) || startedAt;
    if (!pid || !port || !startedAt || !updatedAt) {
      return null;
    }
    if (!isProcessAlive(pid)) {
      removeAgentRuntimeState();
      return null;
    }
    const host = normalizeHost(parsed.host);
    return {
      pid,
      host,
      port,
      baseUrl: normalizeBaseUrl(parsed.baseUrl, host, port),
      startedAt,
      updatedAt,
    };
  } catch {
    return null;
  }
}

export function writeAgentRuntimeState(input: {
  pid: number;
  host?: string;
  port: number;
  startedAt?: number;
}): AgentRuntimeState {
  const host = normalizeHost(input.host);
  const port = normalizePositiveInteger(input.port) || DEFAULT_AGENT_PORT;
  const startedAt = normalizePositiveInteger(input.startedAt) || Date.now();
  const runtimeState: AgentRuntimeState = {
    pid: normalizePositiveInteger(input.pid) || process.pid,
    host,
    port,
    baseUrl: `http://${host}:${port}`,
    startedAt,
    updatedAt: Date.now(),
  };
  ensureParentDir(AGENT_RUNTIME_FILE);
  atomicWriteFileSync(AGENT_RUNTIME_FILE, JSON.stringify(runtimeState, null, 2));
  return runtimeState;
}

export function removeAgentRuntimeState(): void {
  fs.rmSync(AGENT_RUNTIME_FILE, { force: true });
}

export function readAgentPid(): number | null {
  try {
    if (!fs.existsSync(AGENT_PID_FILE)) {
      return null;
    }
    const raw = fs.readFileSync(AGENT_PID_FILE, 'utf8').trim();
    const parsed = Number(raw);
    const normalizedPid = normalizePositiveInteger(parsed);
    if (!normalizedPid) {
      return null;
    }
    if (!isProcessAlive(normalizedPid)) {
      removeAgentPid();
      return null;
    }
    return normalizedPid;
  } catch {
    return null;
  }
}

export function writeAgentPid(pid: number): void {
  const normalizedPid = normalizePositiveInteger(pid) || process.pid;
  ensureParentDir(AGENT_PID_FILE);
  atomicWriteFileSync(AGENT_PID_FILE, String(normalizedPid));
}

export function removeAgentPid(): void {
  fs.rmSync(AGENT_PID_FILE, { force: true });
}

export function writeAgentClientLease(input: {
  clientId: string;
  pid?: number;
  workspaceRoots?: string[];
  createdAt?: number;
}): AgentClientLease {
  const clientId = normalizeClientId(input.clientId);
  if (!clientId) {
    throw new Error('无效的 agent clientId');
  }
  const existingLease = readAgentClientLease(clientId);
  const createdAt = normalizePositiveInteger(input.createdAt)
    || existingLease?.createdAt
    || Date.now();
  const lease: AgentClientLease = {
    clientId,
    pid: normalizePositiveInteger(input.pid) || process.pid,
    workspaceRoots: normalizeWorkspaceRoots(input.workspaceRoots),
    createdAt,
    updatedAt: Date.now(),
  };
  ensureDir(AGENT_CLIENTS_DIR);
  atomicWriteFileSync(getClientLeaseFile(clientId), JSON.stringify(lease, null, 2));
  return lease;
}

export function readAgentClientLease(clientId: string): AgentClientLease | null {
  try {
    const filePath = getClientLeaseFile(clientId);
    if (!fs.existsSync(filePath)) {
      return null;
    }
    const parsed = JSON.parse(fs.readFileSync(filePath, 'utf8')) as Record<string, unknown>;
    return normalizeClientLease(parsed);
  } catch {
    return null;
  }
}

export function removeAgentClientLease(clientId: string): void {
  try {
    fs.rmSync(getClientLeaseFile(clientId), { force: true });
  } catch {
    // ignore invalid client id / missing file
  }
}

export function listAgentClientLeases(): AgentClientLease[] {
  if (!fs.existsSync(AGENT_CLIENTS_DIR)) {
    return [];
  }
  const activeLeases: AgentClientLease[] = [];
  const now = Date.now();
  for (const entry of fs.readdirSync(AGENT_CLIENTS_DIR)) {
    if (!entry.endsWith('.json')) {
      continue;
    }
    const filePath = path.join(AGENT_CLIENTS_DIR, entry);
    try {
      const parsed = JSON.parse(fs.readFileSync(filePath, 'utf8')) as Record<string, unknown>;
      const lease = normalizeClientLease(parsed);
      const isStale = !lease
        || now - lease.updatedAt > AGENT_CLIENT_LEASE_STALE_MS
        || !isProcessAlive(lease.pid);
      if (isStale) {
        fs.rmSync(filePath, { force: true });
        continue;
      }
      activeLeases.push(lease);
    } catch {
      fs.rmSync(filePath, { force: true });
    }
  }
  return activeLeases.sort((a, b) => a.createdAt - b.createdAt);
}

export function resolveConfiguredAgentBaseUrl(): string {
  return readAgentRuntimeState()?.baseUrl || getDefaultAgentBaseUrl();
}

export function resolveConfiguredAgentPort(): number {
  return readAgentRuntimeState()?.port || DEFAULT_AGENT_PORT;
}

export function isProcessAlive(pid: number | null | undefined): boolean {
  if (!pid || !Number.isFinite(pid) || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}
