import * as path from 'path';
import * as fs from 'fs';
import * as net from 'net';
import * as vscode from 'vscode';
import { spawn } from 'child_process';
import { AGENT_LAUNCH_LOCK_FILE, AGENT_STATE_DIR, DEFAULT_AGENT_PORT, DEFAULT_AGENT_HOST, getDefaultAgentBaseUrl } from '../config';
import {
  isProcessAlive,
  listAgentClientLeases,
  readAgentPid,
  readAgentRuntimeState,
  removeAgentClientLease,
  removeAgentPid,
  removeAgentRuntimeState,
  resolveConfiguredAgentBaseUrl,
  writeAgentClientLease,
} from '../runtime-state';
import { buildAgentWebClientUrl } from '../../shared/agent-shared-config';

interface WorkspaceRegistration {
  rootPath: string;
  name: string;
}

interface AgentLaunchLockPayload {
  pid: number;
  createdAt: number;
}

const AGENT_STARTUP_TIMEOUT_MS = 20_000;
const AGENT_STARTUP_POLL_MS = 400;
const AGENT_LAUNCH_LOCK_WAIT_MS = 15_000;
const AGENT_LAUNCH_LOCK_STALE_MS = 30_000;
const AGENT_CLIENT_HEARTBEAT_MS = 15_000;

function getWorkspaceRegistrations(): WorkspaceRegistration[] {
  return (vscode.workspace.workspaceFolders ?? []).map((folder) => ({
    rootPath: folder.uri.fsPath,
    name: folder.name,
  }));
}

export class LocalAgentManager {
  private readonly extensionRoot: string;
  private readonly agentPort: number;
  private readonly clientId: string;
  private startPromise: Promise<void> | null = null;
  private started = false;
  private clientHeartbeat: ReturnType<typeof setInterval> | null = null;
  private disposed = false;

  constructor(extensionRoot: string, agentPort = DEFAULT_AGENT_PORT) {
    this.extensionRoot = extensionRoot;
    this.agentPort = agentPort;
    this.clientId = `magi-client-${process.pid}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  }

  getBaseUrl(): string {
    return resolveConfiguredAgentBaseUrl();
  }

  async ensureStarted(): Promise<void> {
    this.ensureClientLease();
    // 已成功启动过，不重复走完整流程（健康检查 + registerWorkspaces）
    if (this.started) {
      return;
    }
    if (!this.startPromise) {
      this.startPromise = this.ensureStartedInternal().then(() => {
        this.started = true;
      }).finally(() => {
        this.startPromise = null;
      });
    }
    await this.startPromise;
  }

  async restart(): Promise<void> {
   this.started = false;
    this.ensureClientLease();
    const releaseLock = await this.acquireLaunchLock();
    try {
      await this.stopIfRunning();
      const port = await this.resolveLaunchPort();
      await this.startDetached(port);
      const baseUrl = await this.waitForHealthy(port);
      await this.registerWorkspaces(baseUrl);
    } finally {
      releaseLock();
    }
  }

  async isHealthy(): Promise<boolean> {
    return Boolean(await this.resolveHealthyBaseUrl());
  }

  async dispose(): Promise<void> {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    this.stopClientHeartbeat();
    const releaseLock = await this.acquireLaunchLock();
    try {
      removeAgentClientLease(this.clientId);
      if (listAgentClientLeases().length === 0) {
        await this.stopIfRunning();
      }
    } finally {
      releaseLock();
    }
  }

  async openWebClient(): Promise<void> {
    this.ensureClientLease();
    const firstWorkspace = getWorkspaceRegistrations()[0];
    const baseUrl = await this.resolveHealthyBaseUrl() || this.getBaseUrl();
   // 显式传递所有绑定参数，确保 Web 前端直接落到正确工作区
    const url = buildAgentWebClientUrl(baseUrl, {
      workspacePath: firstWorkspace?.rootPath ?? null,
     // workspaceId 由前端从 bootstrap 自动解析（通过 workspacePath 匹配）
    });
    await vscode.env.openExternal(vscode.Uri.parse(url));
  }

  private ensureClientLease(): void {
    if (this.disposed) {
      return;
    }
    if (!this.clientHeartbeat) {
     // 首次创建租约时立即写入
     writeAgentClientLease({
       clientId: this.clientId,
       pid: process.pid,
       workspaceRoots: getWorkspaceRegistrations().map((workspace) => workspace.rootPath),
     });
      this.clientHeartbeat = setInterval(() => {
        if (this.disposed) {
          return;
        }
        try {
          writeAgentClientLease({
            clientId: this.clientId,
            pid: process.pid,
            workspaceRoots: getWorkspaceRegistrations().map((workspace) => workspace.rootPath),
          });
        } catch {
          // ignore lease heartbeat failure and retry on next tick
        }
      }, AGENT_CLIENT_HEARTBEAT_MS);
      this.clientHeartbeat.unref?.();
    }
  }

  private stopClientHeartbeat(): void {
    if (!this.clientHeartbeat) {
      return;
    }
    clearInterval(this.clientHeartbeat);
    this.clientHeartbeat = null;
  }

  private async ensureStartedInternal(): Promise<void> {
    const existingBaseUrl = await this.resolveHealthyBaseUrl();
    if (existingBaseUrl) {
      await this.registerWorkspaces(existingBaseUrl);
      return;
    }
    const releaseLock = await this.acquireLaunchLock();
    try {
      const recheckedBaseUrl = await this.resolveHealthyBaseUrl();
      if (recheckedBaseUrl) {
        await this.registerWorkspaces(recheckedBaseUrl);
        return;
      }
      const port = await this.resolveLaunchPort();
      await this.startDetached(port);
      const baseUrl = await this.waitForHealthy(port);
      await this.registerWorkspaces(baseUrl);
    } finally {
      releaseLock();
    }
  }

  private async isHealthyAt(baseUrl: string): Promise<boolean> {
    if (!baseUrl) {
      return false;
    }
    try {
      const response = await fetch(`${baseUrl}/health`);
      return response.ok;
    } catch {
      return false;
    }
  }

  private getCandidateBaseUrls(): string[] {
    const runtimeState = readAgentRuntimeState();
    const candidates = new Set<string>();
    if (runtimeState?.baseUrl) {
      candidates.add(runtimeState.baseUrl);
    }
    candidates.add(getDefaultAgentBaseUrl());
    candidates.add(resolveConfiguredAgentBaseUrl());
    return Array.from(candidates).filter((url) => typeof url === 'string' && url.trim().length > 0);
  }

  private async resolveHealthyBaseUrl(): Promise<string | null> {
    for (const baseUrl of this.getCandidateBaseUrls()) {
      if (await this.isHealthyAt(baseUrl)) {
        return baseUrl;
      }
    }
    return null;
  }

  private async registerWorkspaces(baseUrl: string): Promise<void> {
    const workspaces = getWorkspaceRegistrations();
    if (workspaces.length === 0) {
      return;
    }
    await fetch(`${baseUrl}/api/workspaces/register`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ workspaces }),
    });
  }

  private async startDetached(port: number): Promise<void> {
    const agentEntry = path.join(this.extensionRoot, 'dist', 'agent.js');
    if (!fs.existsSync(agentEntry)) {
      throw new Error(`Agent 入口不存在: ${agentEntry}`);
    }
    const child = spawn(process.execPath, [agentEntry, `--port=${port}`], {
      detached: true,
      stdio: 'ignore',
      env: {
        ...process.env,
        MAGI_AGENT_WORKSPACES: JSON.stringify(getWorkspaceRegistrations()),
      },
    });
    child.unref();
  }

  private async waitForHealthy(port: number): Promise<string> {
    const expectedBaseUrl = `http://${DEFAULT_AGENT_HOST}:${port}`;
    const maxAttempts = Math.ceil(AGENT_STARTUP_TIMEOUT_MS / AGENT_STARTUP_POLL_MS);
    for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
      if (await this.isHealthyAt(expectedBaseUrl)) {
        return expectedBaseUrl;
      }
      const discoveredBaseUrl = await this.resolveHealthyBaseUrl();
      if (discoveredBaseUrl) {
        return discoveredBaseUrl;
      }
      await this.delay(AGENT_STARTUP_POLL_MS);
    }
    throw new Error('Local Agent 启动超时');
  }

  private async resolveLaunchPort(): Promise<number> {
    if (await this.isPortAvailable(this.agentPort)) {
      return this.agentPort;
    }
    return this.allocateFreePort();
  }

  private async isPortAvailable(port: number): Promise<boolean> {
    return new Promise((resolve) => {
      const server = net.createServer();
      server.once('error', () => resolve(false));
      server.once('listening', () => {
        server.close(() => resolve(true));
      });
      server.listen(port, DEFAULT_AGENT_HOST);
    });
  }

  private async allocateFreePort(): Promise<number> {
    return new Promise((resolve, reject) => {
      const server = net.createServer();
      server.once('error', reject);
      server.listen(0, DEFAULT_AGENT_HOST, () => {
        const address = server.address();
        if (!address || typeof address === 'string') {
          server.close(() => reject(new Error('无法分配可用端口')));
          return;
        }
        const port = address.port;
        server.close((error) => {
          if (error) {
            reject(error);
            return;
          }
          resolve(port);
        });
      });
    });
  }

  private async acquireLaunchLock(): Promise<() => void> {
    const startedAt = Date.now();
    if (!fs.existsSync(AGENT_STATE_DIR)) {
      fs.mkdirSync(AGENT_STATE_DIR, { recursive: true });
    }
    while (Date.now() - startedAt < AGENT_LAUNCH_LOCK_WAIT_MS) {
      if (this.tryClearStaleLaunchLock()) {
        continue;
      }
      try {
        const fd = fs.openSync(AGENT_LAUNCH_LOCK_FILE, 'wx');
        const payload: AgentLaunchLockPayload = {
          pid: process.pid,
          createdAt: Date.now(),
        };
        fs.writeFileSync(fd, JSON.stringify(payload), 'utf8');
        fs.closeSync(fd);
        return () => {
          try {
            fs.rmSync(AGENT_LAUNCH_LOCK_FILE, { force: true });
          } catch {
            // ignore release failure
          }
        };
      } catch (error) {
        const code = (error as NodeJS.ErrnoException).code;
        if (code !== 'EEXIST') {
          throw error;
        }
      }
      await this.delay(200);
    }
    throw new Error('等待 Agent 启动锁超时');
  }

  private tryClearStaleLaunchLock(): boolean {
    if (!fs.existsSync(AGENT_LAUNCH_LOCK_FILE)) {
      return false;
    }
    try {
      const raw = fs.readFileSync(AGENT_LAUNCH_LOCK_FILE, 'utf8');
      const parsed = JSON.parse(raw) as Partial<AgentLaunchLockPayload>;
      const createdAt = typeof parsed.createdAt === 'number' && Number.isFinite(parsed.createdAt)
        ? Math.floor(parsed.createdAt)
        : 0;
      const pid = typeof parsed.pid === 'number' && Number.isFinite(parsed.pid)
        ? Math.floor(parsed.pid)
        : 0;
      const staleByAge = createdAt <= 0 || Date.now() - createdAt > AGENT_LAUNCH_LOCK_STALE_MS;
      const ownerDead = pid <= 0 || !isProcessAlive(pid);
      if (staleByAge || ownerDead) {
        fs.rmSync(AGENT_LAUNCH_LOCK_FILE, { force: true });
        return true;
      }
      return false;
    } catch {
      fs.rmSync(AGENT_LAUNCH_LOCK_FILE, { force: true });
      return true;
    }
  }

  private delay(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  private resolveKnownAgentPid(): number | null {
    const runtimePid = readAgentRuntimeState()?.pid;
    if (runtimePid && isProcessAlive(runtimePid)) {
      return runtimePid;
    }
    const pidFileValue = readAgentPid();
    return pidFileValue && isProcessAlive(pidFileValue) ? pidFileValue : null;
  }

  private async stopIfRunning(): Promise<void> {
    const pid = this.resolveKnownAgentPid() || readAgentRuntimeState()?.pid || readAgentPid();
    if (!pid || !Number.isFinite(pid) || pid <= 0) {
      removeAgentPid();
      removeAgentRuntimeState();
      return;
    }
    try {
      process.kill(pid);
    } catch {
      // ignore stale pid
    }
    const waitStartedAt = Date.now();
    while (Date.now() - waitStartedAt < 5_000) {
      if (!isProcessAlive(pid)) {
        break;
      }
      await this.delay(150);
    }
    removeAgentPid();
    removeAgentRuntimeState();
  }
}
