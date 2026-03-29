import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import * as http from 'http';
import * as zlib from 'zlib';
import { URL } from 'url';
import { execFile } from 'child_process';
import { AGENT_LOG_FILE, AGENT_STATE_DIR, AGENT_UI_SETTINGS_FILE, AGENT_VERSION, DEFAULT_AGENT_HOST, DEFAULT_AGENT_PORT, AGENT_WORKSPACES_FILE } from '../config';
import { removeAgentPid, removeAgentRuntimeState, writeAgentPid, writeAgentRuntimeState } from '../runtime-state';
import { buildLanAccessInfo } from '../lan-access';
import { TunnelManager } from '../tunnel-manager';
import { UnifiedSessionManager, type UnifiedSession } from '../../session';
import type { SessionMeta } from '../../session';
import type { UIState, WorkerStatus } from '../../types';
import type { WorkerSlot } from '../../types';
import type { TaskView } from '../../task/task-view-adapter';
import type { PendingChange } from '../../types';
import { ConfigManager } from '../../config';
import { t } from '../../i18n';
import { AgentRuntimeManager, type AgentWorkspaceRuntime } from './agent-runtime-service';
import type { ClientBridgeMessage } from '../../ui/shared/bridges/client-bridge';
import { OrchestrationRuntimeQueryService } from '../../orchestrator/runtime/orchestration-runtime-query-service';
import {
  buildSessionBootstrapTimelineProjection,
  type SessionBootstrapSnapshot,
} from '../../shared/session-bootstrap';
import { materializeProjectionSourceMessagesFromTimelineRecords } from '../../session/timeline-record-adapter';
import type { SettingsBootstrapPayload } from '../../shared/settings-bootstrap';
import { buildSettingsBootstrapPayload as buildSharedSettingsBootstrapPayload } from '../../ui/settings-bootstrap-builder';
import {
  GovernedKnowledgeContextService,
  type GovernedKnowledgeAuditMetadata,
} from '../../knowledge/governed-knowledge-context-service';
import type { ProjectKnowledgeBase, ADRStatus, ADRRecord, FAQRecord } from '../../knowledge/project-knowledge-base';
import type { ExecutionStatsPayload } from '../../shared/execution-stats-payload';

interface WorkspaceDescriptor {
  workspaceId: string;
  name: string;
  rootPath: string;
  sessionManager: UnifiedSessionManager;
}

interface PersistedWorkspaceRecord {
  workspaceId: string;
  name: string;
  rootPath: string;
  createdAt: number;
  updatedAt: number;
}

interface BootstrapPayload extends SessionBootstrapSnapshot {
  agent: {
    version: string;
    baseUrl: string;
    port: number;
    platform: NodeJS.Platform;
   runtimeEpoch: string;
  };
  workspace: {
    workspaceId: string;
    name: string;
    rootPath: string;
  };
}

interface WorkspaceSessionsPayload {
  workspace: {
    workspaceId: string;
    name: string;
    rootPath: string;
  };
  sessionId: string;
  sessions: SessionMeta[];
}

interface BoundSettingsRequestQuery {
  workspaceId?: string | null;
  workspacePath?: string | null;
}

interface BoundSessionRequestQuery extends BoundSettingsRequestQuery {
  sessionId?: string | null;
}

interface AgentConfigTestResult {
  success: boolean;
  error?: string;
  worker?: WorkerSlot;
  orchestratorModel?: string;
}

interface AgentModelListResult {
  target: string;
  success: boolean;
  models: string[];
  error?: string;
}

interface AgentKnowledgeMutationResult {
  success: boolean;
  error?: string;
  payload?: Record<string, unknown>;
}

interface AgentUiSettings {
  locale: 'zh-CN' | 'en-US';
  deepTask: boolean;
}

interface ResolvedWorkspaceSession {
  workspace: WorkspaceDescriptor;
  session: UnifiedSession;
}

function ensureAgentStateDir(): void {
  if (!fs.existsSync(AGENT_STATE_DIR)) {
    fs.mkdirSync(AGENT_STATE_DIR, { recursive: true });
  }
}

function stableWorkspaceId(rootPath: string): string {
  return Buffer.from(path.resolve(rootPath)).toString('base64url');
}

function safeWorkspaceName(rootPath: string, explicitName?: string): string {
  return explicitName?.trim() || path.basename(rootPath) || rootPath;
}

function buildWorkerStatuses(): WorkerStatus[] {
  return [
    { worker: 'claude', available: false, enabled: false },
    { worker: 'codex', available: false, enabled: false },
    { worker: 'gemini', available: false, enabled: false },
  ];
}

function execFileAsync(command: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(command, args, { encoding: 'utf8' }, (error, stdout) => {
      if (error) {
        reject(error);
        return;
      }
      resolve(stdout);
    });
  });
}

function defaultAgentUiSettings(): AgentUiSettings {
  return {
    locale: ConfigManager.getInstance().get('locale'),
    deepTask: false,
  };
}

class AgentWorkspaceRegistry {
  private readonly workspaces = new Map<string, WorkspaceDescriptor>();
  private readonly metadata = new Map<string, PersistedWorkspaceRecord>();

  constructor() {
    this.loadPersistedWorkspaces();
  }

  registerWorkspace(rootPath: string, explicitName?: string): WorkspaceDescriptor {
    const normalizedRoot = path.resolve(rootPath);
    const existing = Array.from(this.workspaces.values()).find((workspace) => workspace.rootPath === normalizedRoot);
    if (existing) {
      const existingMeta = this.metadata.get(existing.workspaceId);
      const nextName = safeWorkspaceName(normalizedRoot, explicitName || existingMeta?.name);
      this.metadata.set(existing.workspaceId, {
        workspaceId: existing.workspaceId,
        name: nextName,
        rootPath: normalizedRoot,
        createdAt: existingMeta?.createdAt ?? Date.now(),
        updatedAt: Date.now(),
      });
      this.persistWorkspaces();
      return existing;
    }
    const workspaceId = stableWorkspaceId(normalizedRoot);
    const metadata = this.metadata.get(workspaceId);
    const descriptor: WorkspaceDescriptor = {
      workspaceId,
      name: safeWorkspaceName(normalizedRoot, explicitName || metadata?.name),
      rootPath: normalizedRoot,
      sessionManager: new UnifiedSessionManager(normalizedRoot),
    };
    this.workspaces.set(workspaceId, descriptor);
    this.metadata.set(workspaceId, {
      workspaceId,
      name: descriptor.name,
      rootPath: normalizedRoot,
      createdAt: metadata?.createdAt ?? Date.now(),
      updatedAt: Date.now(),
    });
    this.persistWorkspaces();
    return descriptor;
  }

  renameWorkspace(workspaceId?: string | null, workspacePath?: string | null, nextName?: string | null): WorkspaceDescriptor | null {
    const workspace = this.resolveWorkspace(workspaceId, workspacePath);
    const normalizedName = (nextName || '').trim();
    if (!workspace || !normalizedName) {
      return null;
    }
    workspace.name = normalizedName;
    const existingMeta = this.metadata.get(workspace.workspaceId);
    this.metadata.set(workspace.workspaceId, {
      workspaceId: workspace.workspaceId,
      name: normalizedName,
      rootPath: workspace.rootPath,
      createdAt: existingMeta?.createdAt ?? Date.now(),
      updatedAt: Date.now(),
    });
    this.persistWorkspaces();
    return workspace;
  }

  touchWorkspace(workspaceId?: string | null, workspacePath?: string | null): void {
    const workspace = this.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return;
    }
    const existingMeta = this.metadata.get(workspace.workspaceId);
    this.metadata.set(workspace.workspaceId, {
      workspaceId: workspace.workspaceId,
      name: existingMeta?.name || workspace.name,
      rootPath: workspace.rootPath,
      createdAt: existingMeta?.createdAt ?? Date.now(),
      updatedAt: Date.now(),
    });
    this.persistWorkspaces();
  }

  removeWorkspace(workspaceId?: string | null, workspacePath?: string | null): boolean {
    const workspace = this.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return false;
    }
    this.workspaces.delete(workspace.workspaceId);
    this.metadata.delete(workspace.workspaceId);
    this.persistWorkspaces();
    return true;
  }

  listWorkspaces(): Array<Pick<WorkspaceDescriptor, 'workspaceId' | 'name' | 'rootPath'>> {
    return Array.from(this.metadata.values())
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .map(({ workspaceId, name, rootPath }) => ({
        workspaceId,
        name,
        rootPath,
      }));
  }

  resolveWorkspace(workspaceId?: string | null, workspacePath?: string | null): WorkspaceDescriptor | null {
    if (workspaceId && this.workspaces.has(workspaceId)) {
      return this.workspaces.get(workspaceId)!;
    }
    if (workspacePath) {
      const normalized = path.resolve(workspacePath);
      return Array.from(this.workspaces.values()).find((workspace) => workspace.rootPath === normalized) ?? null;
    }
    return this.workspaces.values().next().value ?? null;
  }

  private loadPersistedWorkspaces(): void {
    ensureAgentStateDir();
    if (!fs.existsSync(AGENT_WORKSPACES_FILE)) {
      return;
    }
    try {
      const raw = fs.readFileSync(AGENT_WORKSPACES_FILE, 'utf8');
      const payload = JSON.parse(raw) as { workspaces?: PersistedWorkspaceRecord[] };
      const workspaces = Array.isArray(payload?.workspaces) ? payload.workspaces : [];
      for (const item of workspaces) {
        if (!item?.rootPath) {
          continue;
        }
        const normalizedRoot = path.resolve(item.rootPath);
        const workspaceId = item.workspaceId || stableWorkspaceId(normalizedRoot);
        this.metadata.set(workspaceId, {
          workspaceId,
          name: safeWorkspaceName(normalizedRoot, item.name),
          rootPath: normalizedRoot,
          createdAt: typeof item.createdAt === 'number' ? item.createdAt : Date.now(),
          updatedAt: typeof item.updatedAt === 'number' ? item.updatedAt : Date.now(),
        });
        if (fs.existsSync(normalizedRoot)) {
          const descriptor: WorkspaceDescriptor = {
            workspaceId,
            name: safeWorkspaceName(normalizedRoot, item.name),
            rootPath: normalizedRoot,
            sessionManager: new UnifiedSessionManager(normalizedRoot),
          };
          this.workspaces.set(workspaceId, descriptor);
        }
      }
    } catch {
      // ignore broken persisted registry and rebuild on next write
    }
  }

  private persistWorkspaces(): void {
    ensureAgentStateDir();
    const workspaces = Array.from(this.metadata.values())
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .map((item) => ({
        workspaceId: item.workspaceId,
        name: item.name,
        rootPath: item.rootPath,
        createdAt: item.createdAt,
        updatedAt: item.updatedAt,
      }));
    fs.writeFileSync(AGENT_WORKSPACES_FILE, JSON.stringify({ workspaces }, null, 2), 'utf8');
  }
}

export class LocalAgentService {
  private readonly port: number;
  private readonly listenHost: string;
  private readonly baseUrl: string;
  private readonly runtimeEpoch: string;
  private readonly registry = new AgentWorkspaceRegistry();
  private readonly runtimeManager: AgentRuntimeManager;
  private readonly runtimeQueryService = new OrchestrationRuntimeQueryService();
  private uiSettings: AgentUiSettings;
  private server: http.Server | null = null;
  private readonly tunnelManager: TunnelManager;
  private stopPromise: Promise<void> | null = null;

  constructor(port = DEFAULT_AGENT_PORT, host = DEFAULT_AGENT_HOST) {
    this.port = port;
    this.listenHost = host;
    // baseUrl 用于本机内部访问与默认回填，不能使用 0.0.0.0 这类不可直连地址。
    this.baseUrl = `http://${DEFAULT_AGENT_HOST}:${port}`;
    this.runtimeEpoch = `${process.pid}:${Date.now()}`;
    this.uiSettings = this.loadUiSettings();
    this.runtimeManager = new AgentRuntimeManager(
      () => this.uiSettings.locale,
      () => this.uiSettings.deepTask,
    );
    this.tunnelManager = new TunnelManager(port);
  }

  registerWorkspace(rootPath: string, name?: string): void {
    this.registry.registerWorkspace(rootPath, name);
  }

  async start(): Promise<void> {
    if (this.server) {
      return;
    }
    ensureAgentStateDir();
    this.server = http.createServer((request, response) => {
      void this.handleRequest(request, response).catch((error) => {
        this.handleRequestFailure(request, response, error);
      });
    });
    try {
      await new Promise<void>((resolve, reject) => {
        this.server!.once('error', reject);
        this.server!.listen(this.port, this.listenHost, () => {
          this.server!.off('error', reject);
          resolve();
        });
      });
      writeAgentPid(process.pid);
      writeAgentRuntimeState({
        pid: process.pid,
        host: DEFAULT_AGENT_HOST,
        port: this.port,
      });
      this.appendLog(`agent started on listen=${this.listenHost}:${this.port}, baseUrl=${this.baseUrl}`);
    } catch (error) {
      this.clearRuntimeRegistration();
      if (this.server) {
        try {
          this.server.close();
        } catch {
          // ignore cleanup failure
        }
      }
      this.server = null;
      throw error;
    }
  }

  async stop(): Promise<void> {
    if (this.stopPromise) {
      return this.stopPromise;
    }
    this.stopPromise = (async () => {
      // shutdown 一旦开始，这个进程就不应再继续作为可发现的运行态被暴露。
      this.clearRuntimeRegistration();
      await this.tunnelManager.dispose();
      const server = this.server;
      this.server = null;
      if (!server) {
        return;
      }
      await new Promise<void>((resolve, reject) => {
        server.close((error) => {
          if (error) {
            reject(error);
            return;
          }
          resolve();
        });
      });
      this.appendLog('agent stopped');
    })().finally(() => {
      this.stopPromise = null;
    });
    return this.stopPromise;
  }

  private appendLog(message: string): void {
    ensureAgentStateDir();
    fs.appendFileSync(AGENT_LOG_FILE, `[${new Date().toISOString()}] ${message}\n`, 'utf8');
  }

  private clearRuntimeRegistration(): void {
    removeAgentPid();
    removeAgentRuntimeState();
  }

  private async handleRequest(request: http.IncomingMessage, response: http.ServerResponse): Promise<void> {
    this.setCorsHeaders(response);
    if (request.method === 'OPTIONS') {
      response.writeHead(204);
      response.end();
      return;
    }

    const url = new URL(request.url || '/', this.baseUrl);
    if (request.method === 'GET' && url.pathname === '/health') {
      this.sendJson(response, 200, {
        version: AGENT_VERSION,
        baseUrl: this.baseUrl,
        port: this.port,
        platform: process.platform,
        runtimeEpoch: this.runtimeEpoch,
        workspaces: this.registry.listWorkspaces(),
      });
      return;
    }

    // ---- 隧道 token 校验（仅对经由 Cloudflare Tunnel 的 API 请求生效） ----
    // 静态资源（/assets/*, /web.html, /favicon.ico 等）不做校验，安全边界在 API 层
    if (url.pathname.startsWith('/api/')) {
      const tunnelState = this.tunnelManager.getState();
      if (tunnelState.status === 'running' && tunnelState.token) {
        const host = String(request.headers.host || '');
        const forwardedHost = String(request.headers['x-forwarded-host'] || '');
        const cfRay = String(request.headers['cf-ray'] || '');
        const isTunnelOrigin = host.includes('.trycloudflare.com')
          || forwardedHost.includes('.trycloudflare.com')
          || cfRay.length > 0;
        if (isTunnelOrigin) {
          const requestToken = url.searchParams.get('tunnel_token');
          if (!this.tunnelManager.validateToken(requestToken)) {
            this.sendJson(response, 403, { error: 'invalid_tunnel_token' });
            return;
          }
        }
      }
    }

    if (request.method === 'GET' && url.pathname === '/api/workspaces') {
      this.sendJson(response, 200, { workspaces: this.registry.listWorkspaces() });
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/workspaces/sessions') {
      const payload = this.buildWorkspaceSessionsPayload(
        url.searchParams.get('workspaceId'),
        url.searchParams.get('workspacePath'),
        url.searchParams.get('sessionId'),
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/workspaces/register') {
      const body = await this.readJsonBody(request);
      const workspaces = Array.isArray(body?.workspaces) ? body.workspaces : [];
      for (const item of workspaces) {
        if (!item || typeof item !== 'object') {
          continue;
        }
        const rootPath = typeof item.rootPath === 'string' ? item.rootPath : '';
        const name = typeof item.name === 'string' ? item.name : undefined;
        if (rootPath.trim()) {
          this.registry.registerWorkspace(rootPath, name);
        }
      }
      this.sendJson(response, 200, { workspaces: this.registry.listWorkspaces() });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/workspaces/remove') {
      const body = await this.readJsonBody(request);
      const existingWorkspace = this.registry.resolveWorkspace(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      const removed = this.registry.removeWorkspace(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!removed) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      if (existingWorkspace?.workspaceId) {
        this.runtimeManager.removeRuntime(existingWorkspace.workspaceId);
      }
      this.sendJson(response, 200, { workspaces: this.registry.listWorkspaces() });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/workspaces/rename') {
      const body = await this.readJsonBody(request);
      const renamed = this.registry.renameWorkspace(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.name === 'string' ? body.name : undefined,
      );
      if (!renamed) {
        this.sendJson(response, 400, { error: 'workspace_rename_failed' });
        return;
      }
      this.sendJson(response, 200, { workspaces: this.registry.listWorkspaces() });
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/workspaces/pick') {
      try {
        const pickedPath = await this.pickWorkspacePath();
        this.sendJson(response, 200, {
          cancelled: !pickedPath,
          rootPath: pickedPath || null,
          name: pickedPath ? safeWorkspaceName(pickedPath) : null,
        });
      } catch (error) {
        this.sendJson(response, 500, {
          error: error instanceof Error ? error.message : String(error),
        });
      }
      return;
    }

    // Web 文件选择器：列出指定目录下的子目录（仅目录，不暴露文件内容）
    if (request.method === 'GET' && url.pathname === '/api/filesystem/list') {
      try {
        const dirPath = url.searchParams.get('path') || os.homedir();
        const entries = await this.listDirectoryEntries(dirPath);
        this.sendJson(response, 200, {
          path: path.resolve(dirPath),
          parent: path.dirname(path.resolve(dirPath)),
          entries,
        });
      } catch (error) {
        this.sendJson(response, 400, {
          error: error instanceof Error ? error.message : String(error),
        });
      }
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/bootstrap') {
      const payload = await this.buildBootstrapPayload(
        url.searchParams.get('workspaceId'),
        url.searchParams.get('workspacePath'),
        url.searchParams.get('sessionId'),
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/session/notifications') {
      const payload = await this.buildSessionNotificationsPayload({
        workspaceId: url.searchParams.get('workspaceId'),
        workspacePath: url.searchParams.get('workspacePath'),
        sessionId: url.searchParams.get('sessionId'),
      });
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/events') {
      const runtime = await this.resolveRuntime(
        url.searchParams.get('workspaceId'),
        url.searchParams.get('workspacePath'),
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const requestedSessionId = url.searchParams.get('sessionId');
      // SSE 是纯订阅行为，不应修改 runtime 全局状态（activeSessionId）。
      // 消息过滤由 attachEventStream 内部的 targetSessionId 处理。
      // session 绑定由 executeTask 等业务 API 各自负责。
      this.attachEventStream(response, runtime, requestedSessionId);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/settings/bootstrap') {
      const payload = await this.buildSettingsBootstrapPayload();
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/settings/stats') {
      const payload = await this.buildExecutionStatsPayload({
        workspaceId: url.searchParams.get('workspaceId'),
        workspacePath: url.searchParams.get('workspacePath'),
      });
      if (!payload) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/settings/runtime') {
      this.sendJson(response, 200, {
        locale: this.uiSettings.locale,
        deepTask: this.uiSettings.deepTask,
      });
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/lan-access') {
      const lanAccess = buildLanAccessInfo({
        workspaceId: url.searchParams.get('workspaceId'),
        workspacePath: url.searchParams.get('workspacePath'),
        sessionId: url.searchParams.get('sessionId'),
      }, this.port);
      this.sendJson(response, 200, lanAccess);
      return;
    }

    // ---- 公网隧道 API ----

    if (request.method === 'POST' && url.pathname === '/api/tunnel/start') {
      const body = await this.readJsonBody(request);
      const state = await this.tunnelManager.start({
        workspacePath: typeof body?.workspacePath === 'string' ? body.workspacePath : null,
        workspaceId: typeof body?.workspaceId === 'string' ? body.workspaceId : null,
        sessionId: typeof body?.sessionId === 'string' ? body.sessionId : null,
      });
      this.sendJson(response, 200, state);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/tunnel/stop') {
      const state = await this.tunnelManager.stop();
      this.sendJson(response, 200, state);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/tunnel/status') {
      this.tunnelManager.updateBinding({
        workspacePath: url.searchParams.get('workspacePath'),
        workspaceId: url.searchParams.get('workspaceId'),
        sessionId: url.searchParams.get('sessionId'),
      });
      this.sendJson(response, 200, this.tunnelManager.getState());
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/status') {
      this.sendJson(response, 200, {
        connected: true,
        agentVersion: AGENT_VERSION,
        locale: this.uiSettings.locale,
        deepTask: this.uiSettings.deepTask,
      });
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/knowledge') {
      const payload = await this.buildKnowledgePayload(
        url.searchParams.get('workspaceId'),
        url.searchParams.get('workspacePath'),
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/clear') {
      const body = await this.readJsonBody(request);
      const result = await this.clearProjectKnowledge(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/knowledge/adrs') {
      const payload = await this.getKnowledgeAdrs(
        url.searchParams.get('workspaceId') || undefined,
        url.searchParams.get('workspacePath') || undefined,
        url.searchParams.get('status') || undefined,
      );
      this.sendJson(response, payload ? 200 : 404, payload ?? { error: 'workspace_not_found' });
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/knowledge/faqs') {
      const payload = await this.getKnowledgeFaqs(
        url.searchParams.get('workspaceId') || undefined,
        url.searchParams.get('workspacePath') || undefined,
        url.searchParams.get('category') || undefined,
      );
      this.sendJson(response, payload ? 200 : 404, payload ?? { error: 'workspace_not_found' });
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/knowledge/faqs/search') {
      const payload = await this.searchKnowledgeFaqs(
        url.searchParams.get('workspaceId') || undefined,
        url.searchParams.get('workspacePath') || undefined,
        url.searchParams.get('keyword') || undefined,
      );
      this.sendJson(response, payload ? 200 : 404, payload ?? { error: 'workspace_not_found' });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/adr/add') {
      const body = await this.readJsonBody(request);
      const result = await this.addKnowledgeAdr(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        body?.adr,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/adr/update') {
      const body = await this.readJsonBody(request);
      const result = await this.updateKnowledgeAdr(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.id === 'string' ? body.id : undefined,
        body?.updates,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/faq/add') {
      const body = await this.readJsonBody(request);
      const result = await this.addKnowledgeFaq(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        body?.faq,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/faq/update') {
      const body = await this.readJsonBody(request);
      const result = await this.updateKnowledgeFaq(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.id === 'string' ? body.id : undefined,
        body?.updates,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/adr/delete') {
      const body = await this.readJsonBody(request);
      const result = await this.deleteKnowledgeAdr(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.id === 'string' ? body.id : undefined,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/faq/delete') {
      const body = await this.readJsonBody(request);
      const result = await this.deleteKnowledgeFaq(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.id === 'string' ? body.id : undefined,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/knowledge/learning/delete') {
      const body = await this.readJsonBody(request);
      const result = await this.deleteKnowledgeLearning(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.id === 'string' ? body.id : undefined,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/files/content') {
      const payload = await this.buildFilePreviewPayload(
        url.searchParams.get('workspaceId'),
        url.searchParams.get('workspacePath'),
        url.searchParams.get('sessionId'),
        url.searchParams.get('filePath'),
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'file_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/changes/diff') {
      const payload = await this.buildChangeDiffPayload(
        url.searchParams.get('workspaceId'),
        url.searchParams.get('workspacePath'),
        url.searchParams.get('sessionId'),
        url.searchParams.get('filePath'),
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'change_diff_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/changes/approve') {
      const body = await this.readJsonBody(request);
      const result = await this.applyChangeAction(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        typeof body?.filePath === 'string' ? body.filePath : undefined,
        'approve',
      );
      this.sendJson(response, result.ok ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/changes/revert') {
      const body = await this.readJsonBody(request);
      const result = await this.applyChangeAction(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        typeof body?.filePath === 'string' ? body.filePath : undefined,
        'revert',
      );
      this.sendJson(response, result.ok ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/changes/approve-all') {
      const body = await this.readJsonBody(request);
      const result = await this.applyBulkChangeAction(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        'approve-all',
      );
      this.sendJson(response, result.ok ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/changes/revert-all') {
      const body = await this.readJsonBody(request);
      const result = await this.applyBulkChangeAction(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        'revert-all',
      );
      this.sendJson(response, result.ok ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/changes/revert-mission') {
      const body = await this.readJsonBody(request);
      const result = await this.revertMissionChanges(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        typeof body?.missionId === 'string' ? body.missionId : undefined,
      );
      this.sendJson(response, result.ok ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/new') {
      const body = await this.readJsonBody(request);
      const payload = await this.createSession(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.name === 'string' ? body.name : undefined,
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/switch') {
      const body = await this.readJsonBody(request);
      const payload = await this.switchSession(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/delete') {
      const body = await this.readJsonBody(request);
      const payload = await this.deleteSession(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/rename') {
      const body = await this.readJsonBody(request);
      const payload = await this.renameSession(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        typeof body?.name === 'string' ? body.name : undefined,
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/close') {
      const body = await this.readJsonBody(request);
      const payload = await this.deleteSession(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/save') {
      const body = await this.readJsonBody(request);
      const payload = await this.saveCurrentSession(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
      );
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/notifications/mark-all-read') {
      const body = await this.readJsonBody(request);
      const payload = await this.markAllSessionNotificationsRead({
        workspaceId: typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        workspacePath: typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        sessionId: typeof body?.sessionId === 'string' ? body.sessionId : undefined,
      });
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/notifications/clear') {
      const body = await this.readJsonBody(request);
      const payload = await this.clearSessionNotifications({
        workspaceId: typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        workspacePath: typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        sessionId: typeof body?.sessionId === 'string' ? body.sessionId : undefined,
      });
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/session/notifications/remove') {
      const body = await this.readJsonBody(request);
      const payload = await this.removeSessionNotification({
        workspaceId: typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        workspacePath: typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
        sessionId: typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        notificationId: typeof body?.notificationId === 'string' ? body.notificationId : undefined,
      });
      if (!payload) {
        this.sendJson(response, 404, { error: 'session_notification_not_found' });
        return;
      }
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/execute') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const result = await runtime.submitTask(
        typeof body?.prompt === 'string' ? body.prompt : '',
        typeof body?.sessionId === 'string' ? body.sessionId : undefined,
        typeof body?.requestId === 'string' ? body.requestId : undefined,
      );
      this.sendJson(response, result.success ? 202 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/append') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const result = await runtime.appendMessage(
        typeof body?.taskId === 'string' ? body.taskId : '',
        typeof body?.content === 'string' ? body.content : '',
      );
      this.sendJson(response, 200, { success: true, ...result });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/queued/update') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const success = await runtime.updateQueuedMessage(
        typeof body?.queueId === 'string' ? body.queueId : '',
        typeof body?.content === 'string' ? body.content : '',
      );
      this.sendJson(response, success ? 200 : 400, { success });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/queued/delete') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const success = await runtime.deleteQueuedMessage(
        typeof body?.queueId === 'string' ? body.queueId : '',
      );
      this.sendJson(response, success ? 200 : 400, { success });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/interrupt') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      await runtime.interruptCurrentTask();
      this.sendJson(response, 200, { success: true });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/start') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime || typeof body?.taskId !== 'string' || !body.taskId.trim()) {
        this.sendJson(response, 400, { error: 'task_start_failed' });
        return;
      }
      const result = await runtime.startTask(body.taskId);
      this.sendJson(response, result.success ? 202 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/resume') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime || typeof body?.taskId !== 'string' || !body.taskId.trim()) {
        this.sendJson(response, 400, { error: 'task_resume_failed' });
        return;
      }
      const result = await runtime.resumeTask(body.taskId);
      this.sendJson(response, result.success ? 202 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/delete') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime || typeof body?.taskId !== 'string' || !body.taskId.trim()) {
        this.sendJson(response, 400, { error: 'task_delete_failed' });
        return;
      }
      await runtime.deleteTask(body.taskId);
      this.sendJson(response, 200, { success: true });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/chain/resume') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const sessionId = typeof body?.sessionId === 'string' ? body.sessionId.trim() : '';
      const result = await runtime.resumeChain(sessionId);
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/chain/abandon') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime || typeof body?.chainId !== 'string' || !body.chainId.trim()) {
        this.sendJson(response, 400, { error: 'chain_abandon_failed' });
        return;
      }
      const result = await runtime.abandonChain(body.chainId);
      this.sendJson(response, 200, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/task/clear-all') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      await runtime.clearAllTasks();
      this.sendJson(response, 200, { success: true });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/interaction/confirm-recovery') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime || typeof body?.decision !== 'string' || !body.decision.trim()) {
        this.sendJson(response, 400, { error: 'recovery_confirmation_failed' });
        return;
      }
      const decision = body.decision === 'rollback'
        ? 'rollback'
        : body.decision === 'continue'
          ? 'continue'
          : 'retry';
      const success = await runtime.confirmRecovery(decision);
      this.sendJson(response, success ? 200 : 400, { success, decision });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/interaction/response') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime || typeof body?.requestId !== 'string' || !body.requestId.trim()) {
        this.sendJson(response, 400, { error: 'interaction_response_failed' });
        return;
      }
      const success = await runtime.handleInteractionResponse(body.requestId, body?.response);
      this.sendJson(response, success ? 200 : 400, {
        success,
        requestId: body.requestId,
      });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/interaction/clarification') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const answers = body?.answers && typeof body.answers === 'object'
        ? body.answers as Record<string, string>
        : null;
      const additionalInfo = typeof body?.additionalInfo === 'string' ? body.additionalInfo : null;
      const success = await runtime.answerClarification(answers, additionalInfo);
      this.sendJson(response, success ? 200 : 400, { success });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/interaction/worker-question') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const answer = typeof body?.answer === 'string' ? body.answer : null;
      const success = await runtime.answerWorkerQuestion(answer);
      this.sendJson(response, success ? 200 : 400, { success });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/update') {
      const body = await this.readJsonBody(request);
      const ok = this.updateUiSetting(
        typeof body?.key === 'string' ? body.key : '',
        body?.value,
      );
      if (!ok) {
        this.sendJson(response, 400, { error: 'unsupported_setting' });
        return;
      }
      this.sendJson(response, 200, {
        locale: this.uiSettings.locale,
        deepTask: this.uiSettings.deepTask,
      });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/profile/save') {
      const body = await this.readJsonBody(request);
      const ok = await this.saveProfileConfig(body?.data);
      if (!ok) {
        this.sendJson(response, 400, { error: 'save_profile_config_failed' });
        return;
      }
      this.sendJson(response, 200, { success: true });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/profile/reset') {
      const ok = await this.resetProfileConfig();
      if (!ok) {
        this.sendJson(response, 400, { error: 'reset_profile_config_failed' });
        return;
      }
      this.sendJson(response, 200, { success: true });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/config/save') {
      const body = await this.readJsonBody(request);
      const result = await this.saveSkillsConfig(body?.config);
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/worker/save') {
      const body = await this.readJsonBody(request);
      const ok = await this.saveWorkerConfig(
        typeof body?.worker === 'string' ? body.worker : '',
        body?.config,
      );
      if (!ok) {
        this.sendJson(response, 400, { error: 'save_worker_config_failed' });
        return;
      }
      this.sendJson(response, 200, await this.buildSettingsBootstrapPayload());
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/orchestrator/save') {
      const body = await this.readJsonBody(request);
      const ok = await this.saveOrchestratorConfig(body?.config);
      if (!ok) {
        this.sendJson(response, 400, { error: 'save_orchestrator_config_failed' });
        return;
      }
      this.sendJson(response, 200, await this.buildSettingsBootstrapPayload());
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/auxiliary/save') {
      const body = await this.readJsonBody(request);
      const ok = await this.saveAuxiliaryConfig(body?.config);
      if (!ok) {
        this.sendJson(response, 400, { error: 'save_auxiliary_config_failed' });
        return;
      }
      this.sendJson(response, 200, await this.buildSettingsBootstrapPayload());
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/safeguard/save') {
      const body = await this.readJsonBody(request);
      if (body?.config && typeof body.config === 'object') {
        const { LLMConfigLoader } = await import('../../llm/config');
        LLMConfigLoader.saveSafeguardConfig(body.config as any);
      }
      this.sendJson(response, 200, await this.buildSettingsBootstrapPayload());
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/worker/test') {
      const body = await this.readJsonBody(request);
      const result = await this.testWorkerConnection(
        typeof body?.worker === 'string' ? body.worker : '',
        body?.config,
      );
      this.sendJson(response, 200, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/orchestrator/test') {
      const body = await this.readJsonBody(request);
      const result = await this.testOrchestratorConnection(body?.config);
      this.sendJson(response, 200, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/auxiliary/test') {
      const body = await this.readJsonBody(request);
      const result = await this.testAuxiliaryConnection(body?.config);
      this.sendJson(response, 200, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/models/fetch') {
      const body = await this.readJsonBody(request);
      const result = await this.fetchModelList(body?.config, typeof body?.target === 'string' ? body.target : '');
      this.sendJson(response, 200, result);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/settings/mcp') {
      this.sendJson(response, 200, { servers: await this.loadMcpServers() });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/add') {
      const body = await this.readJsonBody(request);
      const result = await this.addMcpServer(body?.server);
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/update') {
      const body = await this.readJsonBody(request);
      const result = await this.updateMcpServer(
        typeof body?.serverId === 'string' ? body.serverId : '',
        body?.updates,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/delete') {
      const body = await this.readJsonBody(request);
      const result = await this.deleteMcpServer(typeof body?.serverId === 'string' ? body.serverId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/tools') {
      const body = await this.readJsonBody(request);
      const result = await this.getMcpServerTools(typeof body?.serverId === 'string' ? body.serverId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/tools/refresh') {
      const body = await this.readJsonBody(request);
      const result = await this.refreshMcpServerTools(typeof body?.serverId === 'string' ? body.serverId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/connect') {
      const body = await this.readJsonBody(request);
      const result = await this.connectMcpServer(typeof body?.serverId === 'string' ? body.serverId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/mcp/disconnect') {
      const body = await this.readJsonBody(request);
      const result = await this.disconnectMcpServer(typeof body?.serverId === 'string' ? body.serverId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/settings/repositories') {
      this.sendJson(response, 200, { repositories: await this.loadRepositories() });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/repositories/add') {
      const body = await this.readJsonBody(request);
      const result = await this.addRepository(typeof body?.url === 'string' ? body.url : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/repositories/update') {
      const body = await this.readJsonBody(request);
      const result = await this.updateRepository(
        typeof body?.repositoryId === 'string' ? body.repositoryId : '',
        body?.updates,
      );
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/repositories/delete') {
      const body = await this.readJsonBody(request);
      const result = await this.deleteRepository(typeof body?.repositoryId === 'string' ? body.repositoryId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/repositories/refresh') {
      const body = await this.readJsonBody(request);
      const result = await this.refreshRepository(typeof body?.repositoryId === 'string' ? body.repositoryId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'GET' && url.pathname === '/api/settings/skills/library') {
      const payload = await this.loadSkillLibrary();
      this.sendJson(response, 200, payload);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/install') {
      const body = await this.readJsonBody(request);
      const result = await this.installSkill(typeof body?.skillId === 'string' ? body.skillId : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/install-local') {
      const result = await this.installLocalSkill();
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/custom-tool/add') {
      const body = await this.readJsonBody(request);
      const result = await this.addCustomTool(body?.tool);
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/custom-tool/remove') {
      const body = await this.readJsonBody(request);
      const result = await this.removeCustomTool(typeof body?.toolName === 'string' ? body.toolName : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/instruction/remove') {
      const body = await this.readJsonBody(request);
      const result = await this.removeInstructionSkill(typeof body?.skillName === 'string' ? body.skillName : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/update') {
      const body = await this.readJsonBody(request);
      const result = await this.updateSkill(typeof body?.skillName === 'string' ? body.skillName : '');
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/skills/update-all') {
      const result = await this.updateAllSkills();
      this.sendJson(response, result.success ? 200 : 400, result);
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/settings/stats/reset') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      await runtime.resetExecutionStats();
      this.sendJson(response, 200, { success: true });
      return;
    }

    if (request.method === 'POST' && url.pathname === '/api/prompt/enhance') {
      const body = await this.readJsonBody(request);
      const runtime = await this.resolveRuntime(
        typeof body?.workspaceId === 'string' ? body.workspaceId : undefined,
        typeof body?.workspacePath === 'string' ? body.workspacePath : undefined,
      );
      if (!runtime) {
        this.sendJson(response, 404, { error: 'workspace_not_found' });
        return;
      }
      const result = await runtime.enhancePrompt(typeof body?.prompt === 'string' ? body.prompt : '');
      this.sendJson(response, 200, result);
      return;
    }

    if (request.method === 'GET' && (url.pathname === '/' || url.pathname === '/web.html' || url.pathname.startsWith('/assets/'))) {
      const served = this.serveStaticAsset(url.pathname, request, response);
      if (!served) {
        this.sendJson(response, 404, { error: 'not_found' });
      }
      return;
    }

    this.sendJson(response, 404, { error: 'not_found' });
  }

  private async buildBootstrapPayload(
    workspaceId?: string | null,
    workspacePath?: string | null,
    sessionId?: string | null,
  ): Promise<BootstrapPayload | null> {
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    this.registry.touchWorkspace(workspace.workspaceId, workspace.rootPath);
    const manager = workspace.sessionManager;
    let session: UnifiedSession | null = null;
    if (sessionId) {
      session = manager.switchSession(sessionId) ?? manager.getCurrentSession();
    } else {
      session = manager.getCurrentSession() ?? manager.createSession();
    }
    if (!session) {
      session = manager.createSession();
    }
    const runtime = await this.resolveRuntime(workspace.workspaceId, workspace.rootPath);
    if (runtime) {
      await runtime.bindSession(session.id);
    }

    const state = runtime
      ? await runtime.buildUIState()
      : await this.buildWorkspaceUIState(workspace, session);
    const liveMessages = runtime ? runtime.getActiveMessageSnapshots(session.id) : [];
    const queuedMessages = runtime ? runtime.getQueuedMessagesSnapshot(session.id) : [];
    const orchestratorRuntimeState = runtime
      ? await runtime.getRuntimeState(session.id)
      : null;
    const timelineProjection = buildSessionBootstrapTimelineProjection({
      session: {
        id: session.id,
        updatedAt: session.updatedAt,
        projectionMessages: materializeProjectionSourceMessagesFromTimelineRecords(session.timeline.records),
      },
      liveMessages,
    });

    return {
      agent: {
        version: AGENT_VERSION,
        baseUrl: this.baseUrl,
        port: this.port,
        platform: process.platform,
       runtimeEpoch: this.runtimeEpoch,
      },
      workspace: {
        workspaceId: workspace.workspaceId,
        name: workspace.name,
        rootPath: workspace.rootPath,
      },
      sessionId: session.id,
      sessions: manager.getSessionMetas(),
      notifications: {
        sessionId: session.id,
        notifications: manager.getSessionNotifications(session.id) || session.notifications,
      },
      state,
      timelineProjection,
      queuedMessages,
      orchestratorRuntimeState,
      ...(runtime ? { executionChainSummary: runtime.buildExecutionChainSummary(session.id) } : {}),
    };
  }

  private buildWorkspaceSessionsPayload(
    workspaceId?: string | null,
    workspacePath?: string | null,
    preferredSessionId?: string | null,
  ): WorkspaceSessionsPayload | null {
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    const manager = workspace.sessionManager;
    const sessions = manager.getSessionMetas();
    const requestedSessionId = typeof preferredSessionId === 'string' ? preferredSessionId.trim() : '';
    const resolvedSessionId = requestedSessionId && sessions.some((session) => session.id === requestedSessionId)
      ? requestedSessionId
      : (manager.getCurrentSessionId() ?? sessions[0]?.id ?? '');

    return {
      workspace: {
        workspaceId: workspace.workspaceId,
        name: workspace.name,
        rootPath: workspace.rootPath,
      },
      sessionId: resolvedSessionId,
      sessions,
    };
  }

  private async createSession(workspaceId?: string, workspacePath?: string, name?: string): Promise<BootstrapPayload | null> {
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    this.registry.touchWorkspace(workspace.workspaceId, workspace.rootPath);
    const session = workspace.sessionManager.createSession(name);
    const runtime = await this.resolveRuntime(workspace.workspaceId, workspace.rootPath);
    if (runtime) {
      await runtime.bindSession(session.id);
    }
    return this.buildBootstrapPayload(workspace.workspaceId, workspace.rootPath, session.id);
  }

  private async switchSession(workspaceId?: string, workspacePath?: string, sessionId?: string): Promise<BootstrapPayload | null> {
    if (!sessionId) {
      return null;
    }
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    this.registry.touchWorkspace(workspace.workspaceId, workspace.rootPath);
    const session = workspace.sessionManager.switchSession(sessionId);
    if (!session) {
      return null;
    }
    const runtime = await this.resolveRuntime(workspace.workspaceId, workspace.rootPath);
    if (runtime) {
      await runtime.bindSession(session.id);
    }
    return this.buildBootstrapPayload(workspace.workspaceId, workspace.rootPath, session.id);
  }

  private async deleteSession(workspaceId?: string, workspacePath?: string, sessionId?: string): Promise<BootstrapPayload | null> {
    if (!sessionId) {
      return null;
    }
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    const manager = workspace.sessionManager;
    const currentSessionId = manager.getCurrentSession()?.id ?? null;
    const deleted = manager.deleteSession(sessionId);
    if (!deleted) {
      return null;
    }

    const remainingSessions = manager.getSessionMetas();
    if (remainingSessions.length === 0) {
      const nextSession = manager.createSession();
      return this.buildBootstrapPayload(workspace.workspaceId, workspace.rootPath, nextSession.id);
    }

    const nextSessionId = currentSessionId === sessionId
      ? (manager.getCurrentSession()?.id ?? remainingSessions[0]?.id)
      : (manager.getCurrentSession()?.id ?? currentSessionId ?? remainingSessions[0]?.id);
    if (!nextSessionId) {
      return null;
    }
    const runtime = await this.resolveRuntime(workspace.workspaceId, workspace.rootPath);
    if (runtime) {
      await runtime.bindSession(nextSessionId);
    }
    return this.buildBootstrapPayload(workspace.workspaceId, workspace.rootPath, nextSessionId);
  }

  private async renameSession(
    workspaceId?: string,
    workspacePath?: string,
    sessionId?: string,
    name?: string,
  ): Promise<BootstrapPayload | null> {
    if (!sessionId || !name?.trim()) {
      return null;
    }
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    const renamed = workspace.sessionManager.renameSession(sessionId, name.trim());
    if (!renamed) {
      return null;
    }
    return this.buildBootstrapPayload(workspace.workspaceId, workspace.rootPath, sessionId);
  }

  private async saveCurrentSession(
    workspaceId?: string,
    workspacePath?: string,
    sessionId?: string,
  ): Promise<BootstrapPayload | null> {
    return this.buildBootstrapPayload(workspaceId, workspacePath, sessionId);
  }

  private async resolveRuntime(
    workspaceId?: string | null,
    workspacePath?: string | null,
  ) {
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    const runtime = this.runtimeManager.getRuntime(workspace);
    await runtime.ensureInitialized();
    return runtime;
  }

  private attachEventStream(
    response: http.ServerResponse,
    runtime: AgentWorkspaceRuntime,
    sessionId?: string | null,
  ): void {
    response.writeHead(200, {
      'Content-Type': 'text/event-stream; charset=utf-8',
      'Cache-Control': 'no-cache, no-transform',
      Connection: 'keep-alive',
      'X-Accel-Buffering': 'no',
    });
   // SSE 首帧：推送 runtimeEpoch，让所有客户端感知当前后台代际
   response.write(`data: ${JSON.stringify({ type: 'runtimeEpoch', epoch: this.runtimeEpoch })}\n\n`);
    const targetSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    const writeMessage = (message: ClientBridgeMessage): void => {
      const messageSessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
      if (targetSessionId && messageSessionId && messageSessionId !== targetSessionId) {
        return;
      }
      response.write(`data: ${JSON.stringify(message)}\n\n`);
    };
    const unsubscribe = runtime.subscribe(writeMessage);
    const heartbeat = setInterval(() => {
      response.write(': keep-alive\n\n');
    }, 15000);
    response.on('close', () => {
      clearInterval(heartbeat);
      unsubscribe();
      response.end();
    });
  }

  private async buildWorkspaceUIState(
    workspace: WorkspaceDescriptor,
    session: UnifiedSession,
  ): Promise<UIState> {
    const [taskViews, pendingChanges] = await Promise.all([
      this.getTaskViews(workspace, session.id),
      this.getPendingChanges(workspace, session.id),
    ]);
    const stateUpdatedAt = Date.now();
    return this.runtimeQueryService.queryState({
      sessionId: session.id,
      sessions: workspace.sessionManager.getSessionMetas(),
      taskViews,
      locale: this.uiSettings.locale,
      workerStatuses: buildWorkerStatuses(),
      pendingChanges,
      isRunning: taskViews.some((task) => task.status === 'running'),
      logs: [],
      orchestratorPhase: 'idle',
      stateUpdatedAt,
      recovered: false,
      activePlan: undefined,
      planHistory: [],
    });
  }

  private async getTaskViews(workspace: WorkspaceDescriptor, sessionId: string): Promise<TaskView[]> {
    const [{ createFileBasedMissionStorage }, { PlanLedgerService }, { OrchestrationReadModelService }, { TaskViewService }] = await Promise.all([
      import('../../orchestrator/mission'),
      import('../../orchestrator/plan-ledger'),
      import('../../orchestrator/runtime'),
      import('../../services/task-view-service'),
    ]);
    const sessionsDir = path.join(workspace.rootPath, '.magi', 'sessions');
    const missionStorage = createFileBasedMissionStorage(sessionsDir);
    const planLedger = new PlanLedgerService(workspace.sessionManager);
    const readModelService = new OrchestrationReadModelService(missionStorage, planLedger);
    const taskViewService = new TaskViewService(missionStorage, workspace.rootPath, readModelService);
    return taskViewService.listTaskViews(sessionId);
  }

  private async getPendingChanges(workspace: WorkspaceDescriptor, sessionId: string): Promise<PendingChange[]> {
    const manager = workspace.sessionManager;
    manager.switchSession(sessionId);
    const snapshotManager = new (await import('../../snapshot-manager')).SnapshotManager(manager, workspace.rootPath);
    return snapshotManager.getPendingChanges();
  }

  private async buildKnowledgePayload(
    workspaceId?: string | null,
    workspacePath?: string | null,
  ): Promise<Record<string, unknown> | null> {
    const payload = await this.withKnowledgeBase(
      workspaceId || undefined,
      workspacePath || undefined,
      async (_knowledgeBase, governedKnowledge) =>
        governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.read'),
        ),
    );
    return payload ? payload as unknown as Record<string, unknown> : null;
  }

  private async withKnowledgeBase<T>(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    operation: (
      knowledgeBase: ProjectKnowledgeBase,
      governedKnowledge: GovernedKnowledgeContextService,
    ) => Promise<T>,
  ): Promise<T | null> {
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    const { ProjectKnowledgeBase } = await import('../../knowledge/project-knowledge-base');
    const knowledgeBase = new ProjectKnowledgeBase({ projectRoot: workspace.rootPath });
    await knowledgeBase.initialize();
    const governedKnowledge = new GovernedKnowledgeContextService(knowledgeBase);
    return await operation(knowledgeBase, governedKnowledge);
  }

  private createKnowledgeAuditMetadata(
    purpose: GovernedKnowledgeAuditMetadata['purpose'],
    consumer: string,
  ): GovernedKnowledgeAuditMetadata {
    return {
      purpose,
      consumer,
      requestId: `knowledge_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
    };
  }

  private async clearProjectKnowledge(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
  ): Promise<AgentKnowledgeMutationResult> {
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      const counts = knowledgeBase.clearAll();
      return {
        counts,
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.clear'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    return { success: true, payload };
  }

  private async getKnowledgeAdrs(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    status?: string,
  ): Promise<Record<string, unknown> | null> {
    return this.withKnowledgeBase(workspaceId, workspacePath, async (_knowledgeBase, governedKnowledge) =>
      governedKnowledge.buildAdrPayload(
        status ? { status: status as ADRStatus } : undefined,
        this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.adrs'),
      ),
    );
  }

  private async getKnowledgeFaqs(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    category?: string,
  ): Promise<Record<string, unknown> | null> {
    return this.withKnowledgeBase(workspaceId, workspacePath, async (_knowledgeBase, governedKnowledge) =>
      governedKnowledge.buildFaqPayload(
        category ? { category } : undefined,
        this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.faqs'),
      ),
    );
  }

  private async searchKnowledgeFaqs(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    keyword?: string,
  ): Promise<Record<string, unknown> | null> {
    return this.withKnowledgeBase(workspaceId, workspacePath, async (_knowledgeBase, governedKnowledge) =>
      governedKnowledge.buildFaqSearchPayload(
        (keyword || '').trim(),
        this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.search_faqs'),
      ),
    );
  }

  private async addKnowledgeAdr(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    adr: unknown,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!adr || typeof adr !== 'object') {
      return { success: false, error: 'invalid_adr_payload' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      knowledgeBase.addADR(adr as ADRRecord);
      return {
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.add_adr'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    return { success: true, payload };
  }

  private async updateKnowledgeAdr(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    id?: string,
    updates?: unknown,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!id?.trim() || !updates || typeof updates !== 'object') {
      return { success: false, error: 'invalid_adr_update' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      const updated = knowledgeBase.updateADR(id, updates as Record<string, unknown>);
      return {
        updated,
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.update_adr'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    if (!payload.updated) {
      return { success: false, error: 'adr_not_found' };
    }
    return { success: true, payload };
  }

  private async addKnowledgeFaq(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    faq: unknown,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!faq || typeof faq !== 'object') {
      return { success: false, error: 'invalid_faq_payload' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      knowledgeBase.addFAQ(faq as FAQRecord);
      return {
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.add_faq'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    return { success: true, payload };
  }

  private async updateKnowledgeFaq(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    id?: string,
    updates?: unknown,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!id?.trim() || !updates || typeof updates !== 'object') {
      return { success: false, error: 'invalid_faq_update' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      const updated = knowledgeBase.updateFAQ(id, updates as Record<string, unknown>);
      return {
        updated,
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.update_faq'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    if (!payload.updated) {
      return { success: false, error: 'faq_not_found' };
    }
    return { success: true, payload };
  }

  private async deleteKnowledgeAdr(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    id?: string,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!id?.trim()) {
      return { success: false, error: 'missing_adr_id' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      const deleted = knowledgeBase.deleteADR(id);
      return {
        deleted,
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.delete_adr'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    if (!payload.deleted) {
      return { success: false, error: 'adr_not_found' };
    }
    return { success: true, payload };
  }

  private async deleteKnowledgeFaq(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    id?: string,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!id?.trim()) {
      return { success: false, error: 'missing_faq_id' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      const deleted = knowledgeBase.deleteFAQ(id);
      return {
        deleted,
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.delete_faq'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    if (!payload.deleted) {
      return { success: false, error: 'faq_not_found' };
    }
    return { success: true, payload };
  }

  private async deleteKnowledgeLearning(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    id?: string,
  ): Promise<AgentKnowledgeMutationResult> {
    if (!id?.trim()) {
      return { success: false, error: 'missing_learning_id' };
    }
    const payload = await this.withKnowledgeBase(workspaceId, workspacePath, async (knowledgeBase, governedKnowledge) => {
      const deleted = knowledgeBase.deleteLearning(id);
      return {
        deleted,
        knowledge: governedKnowledge.buildKnowledgeSnapshot(
          this.createKnowledgeAuditMetadata('knowledge_api', 'local_agent_service.delete_learning'),
        ),
      };
    });
    if (!payload) {
      return { success: false, error: 'workspace_not_found' };
    }
    if (!payload.deleted) {
      return { success: false, error: 'learning_not_found' };
    }
    return { success: true, payload };
  }

  private resolveWorkspaceSession(
    workspaceId?: string | null,
    workspacePath?: string | null,
    sessionId?: string | null,
  ): ResolvedWorkspaceSession | null {
    const workspace = this.registry.resolveWorkspace(workspaceId, workspacePath);
    if (!workspace) {
      return null;
    }
    const manager = workspace.sessionManager;
    const session = sessionId
      ? (manager.switchSession(sessionId) ?? manager.getCurrentSession())
      : (manager.getCurrentSession() ?? manager.createSession());
    if (!session) {
      return null;
    }
    this.registry.touchWorkspace(workspace.workspaceId, workspace.rootPath);
    return { workspace, session };
  }

  private resolveWorkspaceFilePath(
    workspaceRoot: string,
    filePath?: string | null,
  ): { relativePath: string; absolutePath: string } | null {
    const rawPath = typeof filePath === 'string' ? filePath.trim() : '';
    if (!rawPath) {
      return null;
    }
    const relativePath = path.isAbsolute(rawPath)
      ? path.relative(workspaceRoot, rawPath)
      : rawPath;
    const absolutePath = path.resolve(workspaceRoot, relativePath);
    const normalizedRoot = path.resolve(workspaceRoot) + path.sep;
    const normalizedAbsolute = path.resolve(absolutePath);
    if (normalizedAbsolute !== path.resolve(workspaceRoot) && !normalizedAbsolute.startsWith(normalizedRoot)) {
      return null;
    }
    return {
      relativePath,
      absolutePath,
    };
  }

  private inferLanguage(filePath: string): string {
    const ext = path.extname(filePath).slice(1).toLowerCase();
    const map: Record<string, string> = {
      ts: 'typescript',
      tsx: 'typescript',
      js: 'javascript',
      jsx: 'javascript',
      json: 'json',
      md: 'markdown',
      css: 'css',
      scss: 'scss',
      html: 'html',
      vue: 'vue',
      svelte: 'svelte',
      py: 'python',
      go: 'go',
      java: 'java',
      sh: 'bash',
      yml: 'yaml',
      yaml: 'yaml',
    };
    return map[ext] || 'text';
  }

  private async buildFilePreviewPayload(
    workspaceId?: string | null,
    workspacePath?: string | null,
    sessionId?: string | null,
    filePath?: string | null,
  ): Promise<Record<string, unknown> | null> {
    const resolved = this.resolveWorkspaceSession(workspaceId, workspacePath, sessionId);
    if (!resolved) {
      return null;
    }
    const target = this.resolveWorkspaceFilePath(resolved.workspace.rootPath, filePath);
    if (!target) {
      return null;
    }
    const exists = fs.existsSync(target.absolutePath);
    let content = exists ? fs.readFileSync(target.absolutePath, 'utf8') : '';
    if (!exists) {
      const snapshot = resolved.workspace.sessionManager.getSnapshot(resolved.session.id, target.relativePath);
      if (snapshot) {
        const snapshotFile = resolved.workspace.sessionManager.getSnapshotFilePath(resolved.session.id, snapshot.id);
        if (fs.existsSync(snapshotFile)) {
          content = fs.readFileSync(snapshotFile, 'utf8');
        }
      }
    }
    return {
      filePath: target.relativePath,
      absolutePath: target.absolutePath,
      exists,
      content,
      language: this.inferLanguage(target.relativePath),
    };
  }

  private async buildChangeDiffPayload(
    workspaceId?: string | null,
    workspacePath?: string | null,
    sessionId?: string | null,
    filePath?: string | null,
  ): Promise<Record<string, unknown> | null> {
    const resolved = this.resolveWorkspaceSession(workspaceId, workspacePath, sessionId);
    if (!resolved) {
      return null;
    }
    const target = this.resolveWorkspaceFilePath(resolved.workspace.rootPath, filePath);
    if (!target) {
      return null;
    }
    const { DiffGenerator } = await import('../../diff-generator');
    const diffGenerator = new DiffGenerator(resolved.workspace.sessionManager, resolved.workspace.rootPath);
    const diffResult = diffGenerator.generateDiff(target.relativePath);
    if (!diffResult) {
      return null;
    }
    const snapshot = resolved.workspace.sessionManager.getSnapshot(resolved.session.id, target.relativePath);
    const snapshotFile = snapshot
      ? resolved.workspace.sessionManager.getSnapshotFilePath(resolved.session.id, snapshot.id)
      : '';
    const originalContent = snapshotFile && fs.existsSync(snapshotFile)
      ? fs.readFileSync(snapshotFile, 'utf8')
      : '';
    const currentExists = fs.existsSync(target.absolutePath);
    const currentContent = currentExists
      ? fs.readFileSync(target.absolutePath, 'utf8')
      : '';
    return {
      filePath: diffResult.filePath,
      diff: diffGenerator.formatDiff(diffResult),
      additions: diffResult.additions,
      deletions: diffResult.deletions,
      originalContent,
      currentContent,
      currentAbsolutePath: target.absolutePath,
      currentExists,
    };
  }

  private async applyChangeAction(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    sessionId: string | undefined,
    filePath: string | undefined,
    action: 'approve' | 'revert',
  ): Promise<Record<string, unknown>> {
    const resolved = this.resolveWorkspaceSession(workspaceId, workspacePath, sessionId);
    if (!resolved) {
      return { ok: false, error: 'workspace_not_found' };
    }
    const target = this.resolveWorkspaceFilePath(resolved.workspace.rootPath, filePath);
    if (!target) {
      return { ok: false, error: 'invalid_file_path' };
    }
    const { SnapshotManager } = await import('../../snapshot-manager');
    const snapshotManager = new SnapshotManager(resolved.workspace.sessionManager, resolved.workspace.rootPath);
    const ok = action === 'approve'
      ? snapshotManager.acceptChange(target.relativePath)
      : snapshotManager.revertToSnapshot(target.relativePath);
    return {
      ok,
      action,
      filePath: target.relativePath,
    };
  }

  private async applyBulkChangeAction(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    sessionId: string | undefined,
    action: 'approve-all' | 'revert-all',
  ): Promise<Record<string, unknown>> {
    const resolved = this.resolveWorkspaceSession(workspaceId, workspacePath, sessionId);
    if (!resolved) {
      return { ok: false, error: 'workspace_not_found' };
    }
    const { SnapshotManager } = await import('../../snapshot-manager');
    const snapshotManager = new SnapshotManager(resolved.workspace.sessionManager, resolved.workspace.rootPath);
    const count = action === 'approve-all'
      ? snapshotManager.acceptAllChanges()
      : snapshotManager.revertAllChanges();
    return {
      ok: true,
      action,
      count,
    };
  }

  private async revertMissionChanges(
    workspaceId: string | undefined,
    workspacePath: string | undefined,
    sessionId: string | undefined,
    missionId: string | undefined,
  ): Promise<Record<string, unknown>> {
    const resolved = this.resolveWorkspaceSession(workspaceId, workspacePath, sessionId);
    if (!resolved) {
      return { ok: false, error: 'workspace_not_found' };
    }
    if (!missionId?.trim()) {
      return { ok: false, error: 'missing_mission_id' };
    }
    const { SnapshotManager } = await import('../../snapshot-manager');
    const snapshotManager = new SnapshotManager(resolved.workspace.sessionManager, resolved.workspace.rootPath);
    const result = snapshotManager.revertMission(missionId);
    return {
      ok: true,
      missionId,
      reverted: result.reverted,
      files: result.files,
    };
  }

  private async buildSettingsBootstrapPayload(): Promise<SettingsBootstrapPayload> {
    const { LLMConfigLoader } = await import('../../llm/config');
    const fullConfig = LLMConfigLoader.loadFullConfig();
    const mcpServers = await this.loadMcpServers();

    return buildSharedSettingsBootstrapPayload({
      mcpServers,
      workerStatuses: this.buildSettingsWorkerStatuses(fullConfig),
    });
  }

  private async buildExecutionStatsPayload(
    query: BoundSettingsRequestQuery,
  ): Promise<ExecutionStatsPayload | null> {
    const runtime = await this.resolveRuntime(query.workspaceId, query.workspacePath);
    if (!runtime) {
      return null;
    }
    return runtime.getExecutionStatsPayload();
  }

  private async buildSessionNotificationsPayload(
    query: BoundSessionRequestQuery,
  ): Promise<Record<string, unknown> | null> {
    const runtime = await this.resolveRuntime(query.workspaceId, query.workspacePath);
    if (!runtime) {
      return null;
    }
    const payload = await runtime.getSessionNotificationsPayload(query.sessionId);
    if (!payload) {
      return null;
    }
    return payload as unknown as Record<string, unknown>;
  }

  private async markAllSessionNotificationsRead(
    query: BoundSessionRequestQuery,
  ): Promise<Record<string, unknown> | null> {
    const runtime = await this.resolveRuntime(query.workspaceId, query.workspacePath);
    if (!runtime) {
      return null;
    }
    const payload = await runtime.markAllSessionNotificationsRead(query.sessionId);
    if (!payload) {
      return null;
    }
    return payload as unknown as Record<string, unknown>;
  }

  private async clearSessionNotifications(
    query: BoundSessionRequestQuery,
  ): Promise<Record<string, unknown> | null> {
    const runtime = await this.resolveRuntime(query.workspaceId, query.workspacePath);
    if (!runtime) {
      return null;
    }
    const payload = await runtime.clearSessionNotifications(query.sessionId);
    if (!payload) {
      return null;
    }
    return payload as unknown as Record<string, unknown>;
  }

  private async removeSessionNotification(
    query: BoundSessionRequestQuery & { notificationId?: string | null },
  ): Promise<Record<string, unknown> | null> {
    const runtime = await this.resolveRuntime(query.workspaceId, query.workspacePath);
    if (!runtime) {
      return null;
    }
    const payload = await runtime.removeSessionNotification(
      typeof query.notificationId === 'string' ? query.notificationId : '',
      query.sessionId,
    );
    if (!payload) {
      return null;
    }
    return payload as unknown as Record<string, unknown>;
  }

  private loadUiSettings(): AgentUiSettings {
    ensureAgentStateDir();
    const fallback = defaultAgentUiSettings();
    if (!fs.existsSync(AGENT_UI_SETTINGS_FILE)) {
      return fallback;
    }
    try {
      const raw = fs.readFileSync(AGENT_UI_SETTINGS_FILE, 'utf8');
      const parsed = JSON.parse(raw) as Partial<AgentUiSettings>;
      return {
        locale: parsed.locale === 'en-US' ? 'en-US' : fallback.locale,
        deepTask: typeof parsed.deepTask === 'boolean' ? parsed.deepTask : fallback.deepTask,
      };
    } catch {
      return fallback;
    }
  }

  private persistUiSettings(): void {
    ensureAgentStateDir();
    fs.writeFileSync(AGENT_UI_SETTINGS_FILE, JSON.stringify(this.uiSettings, null, 2), 'utf8');
  }

  private updateUiSetting(key: string, value: unknown): boolean {
    if (key === 'locale') {
      const locale = value === 'en-US' ? 'en-US' : value === 'zh-CN' ? 'zh-CN' : null;
      if (!locale) {
        return false;
      }
      this.uiSettings = {
        ...this.uiSettings,
        locale,
      };
      ConfigManager.getInstance().set('locale', locale);
      ConfigManager.getInstance().save();
      this.persistUiSettings();
      return true;
    }

    if (key === 'deepTask') {
      this.uiSettings = {
        ...this.uiSettings,
        deepTask: Boolean(value),
      };
      this.persistUiSettings();
      return true;
    }

    return false;
  }

  private async pickWorkspacePath(): Promise<string | null> {
    if (process.platform === 'darwin') {
      const output = await execFileAsync('osascript', [
        '-e',
        'try',
        '-e',
        'set selectedFolder to POSIX path of (choose folder with prompt "选择要添加的工作区")',
        '-e',
        'return selectedFolder',
        '-e',
        'on error number -128',
        '-e',
        'return ""',
        '-e',
        'end try',
      ]);
      const rootPath = output.trim();
      return rootPath || null;
    }

    if (process.platform === 'win32') {
      const output = await execFileAsync('powershell.exe', [
        '-NoProfile',
        '-STA',
        '-Command',
        [
          'Add-Type -AssemblyName System.Windows.Forms;',
          '$dialog = New-Object System.Windows.Forms.FolderBrowserDialog;',
          '$dialog.Description = "选择要添加的工作区";',
          '$dialog.ShowNewFolderButton = $true;',
          'if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {',
          '  [Console]::OutputEncoding = [System.Text.Encoding]::UTF8;',
          '  Write-Output $dialog.SelectedPath',
          '}',
        ].join(' '),
      ]);
      const rootPath = output.trim();
      return rootPath || null;
    }

    throw new Error(`当前平台暂不支持目录选择器: ${process.platform}`);
  }

  /**
   * 列出指定目录下的子目录条目（仅目录，不暴露文件内容）。
   * 供 Web 文件选择器使用，让远程用户也能浏览 Agent 所在机器的目录结构。
   */
  private async listDirectoryEntries(dirPath: string): Promise<{ name: string; path: string; hasChildren: boolean }[]> {
    const resolved = path.resolve(dirPath);
    const dirents = await fs.promises.readdir(resolved, { withFileTypes: true });
    const results: { name: string; path: string; hasChildren: boolean }[] = [];
    for (const dirent of dirents) {
      // 跳过隐藏目录（以 . 开头）和常见不可访问目录
      if (dirent.name.startsWith('.') || dirent.name === 'node_modules') {
        continue;
      }
      if (!dirent.isDirectory()) {
        continue;
      }
      const fullPath = path.join(resolved, dirent.name);
      let hasChildren = false;
      try {
        const children = await fs.promises.readdir(fullPath, { withFileTypes: true });
        hasChildren = children.some(child => child.isDirectory() && !child.name.startsWith('.'));
      } catch {
        // 无权限读取子目录，标记为无子节点
      }
      results.push({ name: dirent.name, path: fullPath, hasChildren });
    }
    results.sort((a, b) => a.name.localeCompare(b.name));
    return results;
  }

  private async pickLocalSkillPath(): Promise<string | null> {
    if (process.platform === 'darwin') {
      const output = await execFileAsync('osascript', [
        '-e',
        'try',
        '-e',
        'set selectedFile to POSIX path of (choose file with prompt "选择要导入的本地 Skill" of type {"md"})',
        '-e',
        'return selectedFile',
        '-e',
        'on error number -128',
        '-e',
        'return ""',
        '-e',
        'end try',
      ]);
      const filePath = output.trim();
      return filePath || null;
    }

    if (process.platform === 'win32') {
      const output = await execFileAsync('powershell.exe', [
        '-NoProfile',
        '-STA',
        '-Command',
        [
          'Add-Type -AssemblyName System.Windows.Forms;',
          '$dialog = New-Object System.Windows.Forms.OpenFileDialog;',
          '$dialog.Title = "选择要导入的本地 Skill";',
          '$dialog.Filter = "Markdown Files (*.md)|*.md";',
          'if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {',
          '  [Console]::OutputEncoding = [System.Text.Encoding]::UTF8;',
          '  Write-Output $dialog.FileName',
          '}',
        ].join(' '),
      ]);
      const filePath = output.trim();
      return filePath || null;
    }

    throw new Error(`当前平台暂不支持文件选择器: ${process.platform}`);
  }

  private async loadMcpServers(): Promise<unknown[]> {
    const { LLMConfigLoader } = await import('../../llm/config');
    const { MCPToolExecutor } = await import('../../tools/mcp-executor');
    const executor = new MCPToolExecutor();
    await executor.initialize();
    const manager = executor.getMCPManager();
    const statusMap = new Map<string, any>();
    for (const status of manager.getAllServerStatuses()) {
      if (status?.id) {
        statusMap.set(status.id, status);
      }
    }
    const servers = LLMConfigLoader.loadMCPConfig();
    return servers.map((server: Record<string, unknown>) => {
      const status = statusMap.get(String(server.id || ''));
      return {
        ...server,
        connected: status?.connected === true,
        health: status?.health || (status?.connected ? 'connected' : 'disconnected'),
        error: typeof status?.error === 'string' ? status.error : undefined,
        toolCount: Number.isFinite(status?.toolCount) ? status.toolCount : 0,
        reconnectAttempts: Number.isFinite(status?.reconnectAttempts) ? status.reconnectAttempts : 0,
        lastCheckedAt: Number.isFinite(status?.lastCheckedAt) ? status.lastCheckedAt : undefined,
        lastReconnectAt: Number.isFinite(status?.lastReconnectAt) ? status.lastReconnectAt : undefined,
        lastReconnectSuccessfulAt: Number.isFinite(status?.lastReconnectSuccessfulAt) ? status.lastReconnectSuccessfulAt : undefined,
      };
    });
  }

  private async addMcpServer(server: any): Promise<Record<string, unknown>> {
    if (!server || typeof server !== 'object' || !server.id || !server.name) {
      return { success: false, error: 'invalid_mcp_server' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.addMCPServer(server);
    return { success: true, server, servers: await this.loadMcpServers() };
  }

  private async updateMcpServer(serverId: string, updates: any): Promise<Record<string, unknown>> {
    if (!serverId.trim() || !updates || typeof updates !== 'object') {
      return { success: false, error: 'invalid_mcp_update' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.updateMCPServer(serverId, updates);
    return { success: true, serverId, servers: await this.loadMcpServers() };
  }

  private async deleteMcpServer(serverId: string): Promise<Record<string, unknown>> {
    if (!serverId.trim()) {
      return { success: false, error: 'missing_mcp_server_id' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.deleteMCPServer(serverId);
    return { success: true, serverId, servers: await this.loadMcpServers() };
  }

  private async getMcpServerTools(serverId: string): Promise<Record<string, unknown>> {
    if (!serverId.trim()) {
      return { success: false, error: 'missing_mcp_server_id' };
    }
    const { MCPToolExecutor } = await import('../../tools/mcp-executor');
    const executor = new MCPToolExecutor();
    await executor.initialize();
    const manager = executor.getMCPManager();
    const tools = manager.getServerTools(serverId);
    return { success: true, serverId, tools, servers: await this.loadMcpServers() };
  }

  private async refreshMcpServerTools(serverId: string): Promise<Record<string, unknown>> {
    if (!serverId.trim()) {
      return { success: false, error: 'missing_mcp_server_id' };
    }
    const { MCPToolExecutor } = await import('../../tools/mcp-executor');
    const executor = new MCPToolExecutor();
    await executor.initialize();
    const manager = executor.getMCPManager();
    const tools = await manager.refreshServerTools(serverId);
    return { success: true, serverId, tools, servers: await this.loadMcpServers() };
  }

  private async connectMcpServer(serverId: string): Promise<Record<string, unknown>> {
    if (!serverId.trim()) {
      return { success: false, error: 'missing_mcp_server_id' };
    }
    const [{ LLMConfigLoader }, { MCPToolExecutor }] = await Promise.all([
      import('../../llm/config'),
      import('../../tools/mcp-executor'),
    ]);
    const executor = new MCPToolExecutor();
    await executor.initialize();
    const servers = LLMConfigLoader.loadMCPConfig();
    const server = servers.find((item: any) => item.id === serverId);
    if (!server) {
      return { success: false, error: 'mcp_server_not_found' };
    }
    await executor.connectServer(server);
    return { success: true, serverId, servers: await this.loadMcpServers() };
  }

  private async disconnectMcpServer(serverId: string): Promise<Record<string, unknown>> {
    if (!serverId.trim()) {
      return { success: false, error: 'missing_mcp_server_id' };
    }
    const { MCPToolExecutor } = await import('../../tools/mcp-executor');
    const executor = new MCPToolExecutor();
    await executor.initialize();
    await executor.disconnectServer(serverId);
    return { success: true, serverId, servers: await this.loadMcpServers() };
  }

  private async loadRepositories(): Promise<unknown[]> {
    const { LLMConfigLoader } = await import('../../llm/config');
    return LLMConfigLoader.loadRepositories();
  }

  private async addRepository(url: string): Promise<Record<string, unknown>> {
    if (!url.trim()) {
      return { success: false, error: 'missing_repository_url' };
    }
    try {
      const [{ LLMConfigLoader }, { SkillRepositoryManager }] = await Promise.all([
        import('../../llm/config'),
        import('../../tools/skill-repository-manager'),
      ]);
      const manager = new SkillRepositoryManager();
      const repoInfo = await manager.validateRepository(url);
      const result = await LLMConfigLoader.addRepository(url);
      LLMConfigLoader.updateRepositoryName(result.id, repoInfo.name);
      LLMConfigLoader.updateRepository(result.id, { type: repoInfo.type });
      return {
        success: true,
        repository: { id: result.id, url, name: repoInfo.name, type: repoInfo.type, enabled: true },
        repositories: await this.loadRepositories(),
      };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async updateRepository(repositoryId: string, updates: any): Promise<Record<string, unknown>> {
    if (!repositoryId.trim() || !updates || typeof updates !== 'object') {
      return { success: false, error: 'invalid_repository_update' };
    }
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateRepository(repositoryId, updates);
      return { success: true, repositoryId, repositories: await this.loadRepositories() };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async deleteRepository(repositoryId: string): Promise<Record<string, unknown>> {
    if (!repositoryId.trim()) {
      return { success: false, error: 'missing_repository_id' };
    }
    try {
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.deleteRepository(repositoryId);
      return { success: true, repositoryId, repositories: await this.loadRepositories() };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async refreshRepository(repositoryId: string): Promise<Record<string, unknown>> {
    if (!repositoryId.trim()) {
      return { success: false, error: 'missing_repository_id' };
    }
    try {
      const { SkillRepositoryManager } = await import('../../tools/skill-repository-manager');
      const manager = new SkillRepositoryManager();
      manager.clearCache(repositoryId);
      return { success: true, repositoryId };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async loadSkillLibrary(): Promise<Record<string, unknown>> {
    const [{ LLMConfigLoader }, { SkillRepositoryManager }] = await Promise.all([
      import('../../llm/config'),
      import('../../tools/skill-repository-manager'),
    ]);
    const repositories = LLMConfigLoader.loadRepositories();
    const manager = new SkillRepositoryManager();
    const { skills, failedRepositories } = await manager.getAllSkillsWithReport(repositories);
    const skillsConfig = LLMConfigLoader.loadSkillsConfig();
    const installedSkillIds = new Set<string>();
    const toSkillId = (item: any): string => {
      if (!item) return '';
      if (typeof item.fullName === 'string' && item.fullName.trim()) return item.fullName.trim();
      if (typeof item.name === 'string' && item.name.trim()) return item.name.trim();
      return '';
    };
    for (const tool of Array.isArray(skillsConfig?.customTools) ? skillsConfig.customTools : []) {
      const skillId = toSkillId(tool);
      if (skillId) installedSkillIds.add(skillId);
    }
    for (const skill of Array.isArray(skillsConfig?.instructionSkills) ? skillsConfig.instructionSkills : []) {
      const skillId = toSkillId(skill);
      if (skillId) installedSkillIds.add(skillId);
    }
    return {
      skills: skills.map((skill) => {
        const skillId = toSkillId(skill);
        return { ...skill, installed: skillId ? installedSkillIds.has(skillId) : false };
      }),
      failedRepositories,
      totalRepositories: repositories.length,
    };
  }

  private async installSkill(skillId: string): Promise<Record<string, unknown>> {
    if (!skillId.trim()) {
      return { success: false, error: 'missing_skill_id' };
    }
    try {
      const [{ LLMConfigLoader }, { SkillRepositoryManager }, { applySkillInstall }] = await Promise.all([
        import('../../llm/config'),
        import('../../tools/skill-repository-manager'),
        import('../../tools/skill-installation'),
      ]);
      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      const skills = await manager.getAllSkills(repositories);
      const skill = skills.find((item: any) => item.fullName === skillId || item.id === skillId);
      if (!skill) {
        return { success: false, error: `skill_not_found:${skillId}` };
      }
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories };
      const updatedConfig = applySkillInstall(config, skill);
      Object.assign(config, updatedConfig);
      LLMConfigLoader.saveSkillsConfig(config);
      return { success: true, skillId, skill };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private parseLocalSkillMarkdown(content: string): { meta: Record<string, any>; body: string } {
    const trimmed = content.trim();
    if (!trimmed.startsWith('---')) {
      return { meta: {}, body: content.trim() };
    }
    const lines = trimmed.split('\n');
    const metaLines: string[] = [];
    let i = 1;
    for (; i < lines.length; i++) {
      const line = lines[i].trim();
      if (line === '---') {
        i++;
        break;
      }
      metaLines.push(lines[i]);
    }
    const meta: Record<string, any> = {};
    for (const line of metaLines) {
      const match = line.trim().match(/^([A-Za-z0-9_-]+)\s*:\s*(.*)$/);
      if (!match) {
        continue;
      }
      meta[match[1]] = match[2].trim().replace(/^['"]|['"]$/g, '');
    }
    return { meta, body: lines.slice(i).join('\n').trim() };
  }

  private normalizeLocalSkillSlug(rawName: string): string {
    const trimmed = rawName.trim().toLowerCase();
    const replaced = trimmed.replace(/\s+/g, '-').replace(/[^a-z0-9._-]/g, '-');
    return replaced.replace(/-+/g, '-').replace(/^[-_.]+|[-_.]+$/g, '');
  }

  private extractLocalSkillDescription(body: string): string {
    const lines = body.split('\n').map((line) => line.trim()).filter(Boolean);
    for (const line of lines) {
      if (line.startsWith('#')) {
        const title = line.replace(/^#+\s*/, '').trim();
        if (title) return title;
        continue;
      }
      return line.length > 120 ? `${line.slice(0, 120)}...` : line;
    }
    return 'Local instruction skill';
  }

  private async installLocalSkill(): Promise<Record<string, unknown>> {
    const filePath = await this.pickLocalSkillPath();
    if (!filePath) {
      return { success: false, canceled: true };
    }
    try {
      const content = fs.readFileSync(filePath, 'utf8');
      const { meta, body } = this.parseLocalSkillMarkdown(content);
      const instruction = body.trim();
      if (!instruction) {
        return { success: false, error: 'SKILL.md 内容为空' };
      }
      const fileBaseName = path.basename(filePath, path.extname(filePath));
      const rawName = String(meta.name || fileBaseName || 'local-skill');
      const slug = this.normalizeLocalSkillSlug(rawName);
      const normalizedName = `local/${slug || 'skill'}`;
      const description = String(meta.description || this.extractLocalSkillDescription(instruction));
      const localSkill = {
        id: normalizedName,
        name: normalizedName,
        fullName: normalizedName,
        description,
        version: meta.version ? String(meta.version) : undefined,
        repositoryId: 'local',
        repositoryName: 'Local Skills',
        skillType: 'instruction' as const,
        instruction,
        userInvocable: true,
      };
      const [{ LLMConfigLoader }, { applySkillInstall }] = await Promise.all([
        import('../../llm/config'),
        import('../../tools/skill-installation'),
      ]);
      const repositories = LLMConfigLoader.loadRepositories();
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories };
      const updatedConfig = applySkillInstall(config, localSkill as any);
      Object.assign(config, updatedConfig);
      LLMConfigLoader.saveSkillsConfig(config);
      return { success: true, source: 'local', skillId: normalizedName, skill: localSkill };
    } catch (error) {
      return { success: false, source: 'local', filePath, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async saveSkillsConfig(config: any): Promise<Record<string, unknown>> {
    if (!config || typeof config !== 'object') {
      return { success: false, error: 'invalid_skills_config' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.saveSkillsConfig(config);
    return { success: true, config: LLMConfigLoader.loadSkillsConfig() };
  }

  private async addCustomTool(tool: any): Promise<Record<string, unknown>> {
    if (!tool || typeof tool !== 'object' || !tool.name) {
      return { success: false, error: 'invalid_custom_tool' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
    const existingIndex = config.customTools.findIndex((item: any) => item.name === tool.name);
    if (existingIndex >= 0) {
      config.customTools[existingIndex] = tool;
    } else {
      config.customTools.push(tool);
    }
    LLMConfigLoader.saveSkillsConfig(config);
    return { success: true, tool, config: LLMConfigLoader.loadSkillsConfig() };
  }

  private async removeCustomTool(toolName: string): Promise<Record<string, unknown>> {
    if (!toolName.trim()) {
      return { success: false, error: 'missing_tool_name' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
    config.customTools = (config.customTools || []).filter((tool: any) => tool.name !== toolName);
    LLMConfigLoader.saveSkillsConfig(config);
    return { success: true, toolName };
  }

  private async removeInstructionSkill(skillName: string): Promise<Record<string, unknown>> {
    if (!skillName.trim()) {
      return { success: false, error: 'missing_skill_name' };
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
    config.instructionSkills = (config.instructionSkills || []).filter((skill: any) => skill.name !== skillName);
    LLMConfigLoader.saveSkillsConfig(config);
    return { success: true, skillName };
  }

  private async updateSkill(skillName: string): Promise<Record<string, unknown>> {
    if (!skillName.trim()) {
      return { success: false, error: 'missing_skill_name' };
    }
    try {
      const [{ LLMConfigLoader }, { SkillRepositoryManager }, { applySkillInstall }] = await Promise.all([
        import('../../llm/config'),
        import('../../tools/skill-repository-manager'),
        import('../../tools/skill-installation'),
      ]);
      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      for (const repo of repositories) {
        manager.clearCache(repo.id);
      }
      const skills = await manager.getAllSkills(repositories);
      const latestSkill = skills.find((item: any) => item.fullName === skillName || item.name === skillName);
      if (!latestSkill) {
        return { success: false, error: `skill_not_found:${skillName}` };
      }
      const config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories };
      const updatedConfig = applySkillInstall(config, latestSkill);
      Object.assign(config, updatedConfig);
      LLMConfigLoader.saveSkillsConfig(config);
      return { success: true, skillName, version: latestSkill.version };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error), skillName };
    }
  }

  private async updateAllSkills(): Promise<Record<string, unknown>> {
    try {
      const [{ LLMConfigLoader }, { SkillRepositoryManager }, { applySkillInstall }] = await Promise.all([
        import('../../llm/config'),
        import('../../tools/skill-repository-manager'),
        import('../../tools/skill-installation'),
      ]);
      let config = LLMConfigLoader.loadSkillsConfig() || { customTools: [], instructionSkills: [], repositories: [] };
      const installedNames = new Set<string>();
      for (const skill of Array.isArray(config.instructionSkills) ? config.instructionSkills : []) {
        if (skill?.name) installedNames.add(skill.name);
        if (skill?.fullName) installedNames.add(skill.fullName);
      }
      for (const tool of Array.isArray(config.customTools) ? config.customTools : []) {
        if (tool?.name) installedNames.add(tool.name);
        if (tool?.fullName) installedNames.add(tool.fullName);
      }
      if (installedNames.size === 0) {
        return { success: true, updatedCount: 0 };
      }
      const repositories = LLMConfigLoader.loadRepositories();
      const manager = new SkillRepositoryManager();
      for (const repo of repositories) {
        manager.clearCache(repo.id);
      }
      const remoteSkills = await manager.getAllSkills(repositories);
      let updatedCount = 0;
      for (const remoteSkill of remoteSkills) {
        if (installedNames.has(remoteSkill.fullName) || installedNames.has(remoteSkill.name)) {
          config = applySkillInstall(config, remoteSkill);
          updatedCount++;
        }
      }
      LLMConfigLoader.saveSkillsConfig(config);
      return { success: true, updatedCount };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private buildSettingsWorkerStatuses(fullConfig: any): Record<string, { status: string; model?: string; error?: string }> {
    const statuses: Record<string, { status: string; model?: string; error?: string }> = {};
    const formatModelLabel = (config: any): string | undefined => {
      if (!config?.provider || !config?.model) {
        return undefined;
      }
      return `${config.provider} - ${config.model}`;
    };

    const applyStatus = (name: 'orchestrator' | 'auxiliary' | WorkerSlot, config: any, required = false) => {
      if (!config?.enabled) {
        statuses[name] = { status: 'disabled', model: 'Disabled' };
        return;
      }
      const modelLabel = formatModelLabel(config);
      if (!config?.apiKey || !config?.model) {
        statuses[name] = {
          status: 'not_configured',
          model: required ? 'Not Configured' : (modelLabel || 'Not Configured'),
        };
        return;
      }
      statuses[name] = {
        status: 'configured',
        model: modelLabel,
      };
    };

    applyStatus('orchestrator', fullConfig.orchestrator, true);
    applyStatus('auxiliary', fullConfig.auxiliary);
    applyStatus('claude', fullConfig.workers?.claude);
    applyStatus('codex', fullConfig.workers?.codex);
    applyStatus('gemini', fullConfig.workers?.gemini);
    return statuses;
  }

  private async saveProfileConfig(data: any): Promise<boolean> {
    try {
      const { WorkerAssignmentStorage, WORKER_ASSIGNMENTS_VERSION } = await import('../../orchestrator/profile');
      const workerAssignments: Record<WorkerSlot, string[]> = {
        claude: [],
        codex: [],
        gemini: [],
      };
      const assignmentMap = data?.assignments && typeof data.assignments === 'object' ? data.assignments : {};
      for (const [category, worker] of Object.entries(assignmentMap)) {
        const normalizedWorker = String(worker).toLowerCase() as WorkerSlot;
        if (!['claude', 'codex', 'gemini'].includes(normalizedWorker)) {
          return false;
        }
        workerAssignments[normalizedWorker].push(category);
      }
      WorkerAssignmentStorage.save({
        version: WORKER_ASSIGNMENTS_VERSION,
        assignments: workerAssignments,
      });
      const { LLMConfigLoader } = await import('../../llm/config');
      const userRulesContent = typeof data?.userRules === 'string' ? data.userRules : '';
      const trimmed = userRulesContent.trim();
      LLMConfigLoader.updateUserRules({
        enabled: trimmed.length > 0,
        content: userRulesContent,
      });
      return true;
    } catch {
      return false;
    }
  }

  private async resetProfileConfig(): Promise<boolean> {
    try {
      const { WorkerAssignmentStorage } = await import('../../orchestrator/profile');
      const defaults = WorkerAssignmentStorage.buildDefault();
      WorkerAssignmentStorage.save(defaults);
      const { LLMConfigLoader } = await import('../../llm/config');
      LLMConfigLoader.updateUserRules({ enabled: false, content: '' });
      return true;
    } catch {
      return false;
    }
  }

  private async saveWorkerConfig(worker: string, config: any): Promise<boolean> {
    if (worker !== 'claude' && worker !== 'codex' && worker !== 'gemini') {
      return false;
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.updateWorkerConfig(worker, config);
    // 清除运行时 adapter 缓存，确保新配置立即生效
    await this.runtimeManager.reloadAllLLMConfigs('worker', worker);
    return true;
  }

  private async saveOrchestratorConfig(config: any): Promise<boolean> {
    if (!config || typeof config !== 'object') {
      return false;
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.updateOrchestratorConfig(config);
    // 清除运行时 adapter 缓存，确保新配置立即生效
    await this.runtimeManager.reloadAllLLMConfigs('orchestrator');
    return true;
  }

  private async saveAuxiliaryConfig(config: any): Promise<boolean> {
    if (!config || typeof config !== 'object') {
      return false;
    }
    const { LLMConfigLoader } = await import('../../llm/config');
    LLMConfigLoader.updateAuxiliaryConfig(config);
    // 清除运行时 adapter 缓存，确保新配置立即生效
    await this.runtimeManager.reloadAllLLMConfigs('auxiliary');
    return true;
  }

  private async testWorkerConnection(worker: string, config: any): Promise<AgentConfigTestResult> {
    if (worker !== 'claude' && worker !== 'codex' && worker !== 'gemini') {
      return { success: false, error: 'unknown_worker' };
    }
    try {
      const normalizedConfig = { ...config, enabled: config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        return { success: false, worker, error: t('config.toast.workerNotEnabled', { worker }) };
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'ping' }],
        maxTokens: 1,
        temperature: 0,
      });
      if (!response) {
        return { success: false, worker, error: 'No response from LLM' };
      }
      return { success: true, worker };
    } catch (error) {
      return { success: false, worker, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async testOrchestratorConnection(config: any): Promise<AgentConfigTestResult> {
    try {
      const normalizedConfig = { ...config, enabled: config?.enabled !== false };
      if (!normalizedConfig.enabled) {
        return { success: false, error: t('config.toast.orchestratorNotEnabled') };
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'ping' }],
        maxTokens: 1,
        temperature: 0,
      });
      if (!response) {
        return { success: false, error: 'No response from LLM' };
      }
      return { success: true };
    } catch (error) {
      return { success: false, error: error instanceof Error ? error.message : String(error) };
    }
  }

  private async testAuxiliaryConnection(config: any): Promise<AgentConfigTestResult> {
    let orchestratorModel: string | undefined;
    try {
      const normalizedConfig = { ...config, enabled: Boolean(config?.apiKey) || config?.enabled === true };
      const { LLMConfigLoader } = await import('../../llm/config');
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();
      orchestratorModel = orchestratorConfig?.provider && orchestratorConfig?.model
        ? `${orchestratorConfig.provider} - ${orchestratorConfig.model}`
        : undefined;
      if (!normalizedConfig.enabled || !normalizedConfig.apiKey || !normalizedConfig.model) {
        return { success: false, error: t('config.toast.auxiliaryNotAvailable'), orchestratorModel };
      }
      const { createLLMClient } = await import('../../llm/clients/client-factory');
      const client = createLLMClient(normalizedConfig);
      const response = await client.sendMessage({
        messages: [{ role: 'user', content: 'ping' }],
        maxTokens: 1,
        temperature: 0,
      });
      if (!response) {
        return { success: false, error: 'No response from LLM', orchestratorModel };
      }
      return { success: true, orchestratorModel };
    } catch (error) {
      return {
        success: false,
        error: error instanceof Error ? error.message : String(error),
        orchestratorModel,
      };
    }
  }

  private async fetchModelList(config: any, target: string): Promise<AgentModelListResult> {
    try {
      if (!config?.baseUrl || !config?.apiKey) {
        return { target, success: false, models: [], error: t('config.toast.fillBaseUrlFirst') };
      }
      const provider = config.provider === 'anthropic' ? 'anthropic' : 'openai';
      const [{ resolveModelsBaseUrl }, { fetchWithRetry, isRetryableNetworkError, toErrorMessage }] = await Promise.all([
        import('../../llm/url-mode'),
        import('../../tools/network-utils'),
      ]);
      let modelsUrl = resolveModelsBaseUrl(provider, config.baseUrl, config.urlMode);
      if (!modelsUrl) {
        return { target, success: false, models: [], error: t('config.toast.modelListUnsupportedInFullMode') };
      }
      modelsUrl += '/models';
      const headers: Record<string, string> = { 'Content-Type': 'application/json' };
      if (provider === 'anthropic') {
        headers['x-api-key'] = config.apiKey;
        headers['anthropic-version'] = '2023-06-01';
      } else {
        headers.Authorization = `Bearer ${config.apiKey}`;
      }
      const response = await fetchWithRetry(modelsUrl, {
        method: 'GET',
        headers,
      }, {
        timeoutMs: 10000,
        attempts: 2,
        retryOnStatuses: [429, 500, 502, 503, 504],
      });
      if (!response.ok) {
        const status = response.status;
        let error = `HTTP ${status}`;
        if (status === 401 || status === 403) error = t('config.toast.invalidApiKey');
        else if (status === 404) error = t('config.toast.apiNotSupportModelList');
        return { target, success: false, models: [], error };
      }
      const data = await response.json();
      const models = (data?.data || [])
        .map((m: any) => m.id)
        .filter((id: any) => typeof id === 'string' && id.length > 0)
        .sort();
      return { target, success: true, models };
    } catch (error) {
      const [{ isRetryableNetworkError, toErrorMessage }] = await Promise.all([
        import('../../tools/network-utils'),
      ]);
      const errorMessage = toErrorMessage(error);
      let displayError = errorMessage;
      const lower = errorMessage.toLowerCase();
      if (lower.includes('timeout') || lower.includes('timed out')) displayError = t('config.toast.connectionTimeout');
      else if (isRetryableNetworkError(errorMessage)) displayError = t('config.toast.networkFailed');
      return { target, success: false, models: [], error: displayError };
    }
  }

  /** 带 hash 的不变文件 gzip 缓存 */
  private readonly gzipCache = new Map<string, Buffer>();

  private serveStaticAsset(
    requestPath: string,
    request: http.IncomingMessage,
    response: http.ServerResponse,
  ): boolean {
    const webRoot = path.resolve(__dirname, './web');
    const relativePath = requestPath === '/' ? 'web.html' : requestPath.replace(/^\//, '');
    const filePath = path.resolve(webRoot, relativePath);
    if (!filePath.startsWith(webRoot) || !fs.existsSync(filePath) || fs.statSync(filePath).isDirectory()) {
      return false;
    }

    const extension = path.extname(filePath);
    const contentType = extension === '.html'
      ? 'text/html; charset=utf-8'
      : extension === '.js'
        ? 'application/javascript; charset=utf-8'
        : extension === '.css'
          ? 'text/css; charset=utf-8'
          : extension === '.svg'
            ? 'image/svg+xml'
            : 'application/octet-stream';

    // 带 hash 后缀的文件（如 treemap-KMMF4GRG.js）可长期缓存
    const fileName = path.basename(filePath);
    const hasHash = /[-\.][A-Z0-9]{6,}\.\w+$/i.test(fileName);
    const cacheControl = hasHash
      ? 'public, max-age=31536000, immutable'
      : 'no-cache';

    // 检查客户端是否支持 gzip（仅对文本资源压缩）
    const acceptEncoding = String(request.headers['accept-encoding'] || '');
    const isCompressible = ['.html', '.js', '.css', '.svg', '.json'].includes(extension);
    const useGzip = isCompressible && acceptEncoding.includes('gzip');

    const headers: Record<string, string> = {
      'Content-Type': contentType,
      'Cache-Control': cacheControl,
      Vary: 'Accept-Encoding',
    };

    if (useGzip) {
      headers['Content-Encoding'] = 'gzip';
      // 带 hash 的文件用内存缓存避免重复压缩
      const cached = hasHash ? this.gzipCache.get(filePath) : undefined;
      if (cached) {
        headers['Content-Length'] = String(cached.length);
        response.writeHead(200, headers);
        response.end(cached);
      } else {
        response.writeHead(200, headers);
        const gzip = zlib.createGzip({ level: 6 });
        if (hasHash) {
          // 边压缩边收集到缓存
          const chunks: Buffer[] = [];
          gzip.on('data', (chunk: Buffer) => chunks.push(chunk));
          gzip.on('end', () => { this.gzipCache.set(filePath, Buffer.concat(chunks)); });
        }
        fs.createReadStream(filePath).pipe(gzip).pipe(response);
      }
    } else {
      response.writeHead(200, headers);
      fs.createReadStream(filePath).pipe(response);
    }
    return true;
  }

  private async readJsonBody(request: http.IncomingMessage): Promise<Record<string, unknown> | null> {
    const chunks: Buffer[] = [];
    for await (const chunk of request) {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
    }
    if (chunks.length === 0) {
      return null;
    }
    const raw = Buffer.concat(chunks).toString('utf8').trim();
    if (!raw) {
      return null;
    }
    return JSON.parse(raw) as Record<string, unknown>;
  }

  private handleRequestFailure(
    request: http.IncomingMessage,
    response: http.ServerResponse,
    error: unknown,
  ): void {
    const errorMessage = error instanceof Error ? error.message : String(error);
    const method = request.method || 'UNKNOWN';
    const requestUrl = request.url || '/';
    this.appendLog(`[agent.http] ${method} ${requestUrl} failed: ${errorMessage}`);

    const isInvalidJson = error instanceof SyntaxError;
    const statusCode = isInvalidJson ? 400 : 500;
    const payload = {
      error: isInvalidJson ? 'invalid_json_body' : (errorMessage || 'internal_error'),
    };

    if (!response.headersSent && !response.writableEnded) {
      this.sendJson(response, statusCode, payload);
      return;
    }

    if (!response.writableEnded) {
      try {
        response.end();
      } catch {
        // ignore end failure
      }
    }
  }

  private setCorsHeaders(response: http.ServerResponse): void {
    response.setHeader('Access-Control-Allow-Origin', '*');
    response.setHeader('Access-Control-Allow-Methods', 'GET,POST,OPTIONS');
    response.setHeader('Access-Control-Allow-Headers', 'Content-Type');
  }

  private sendJson(response: http.ServerResponse, statusCode: number, payload: unknown): void {
    response.writeHead(statusCode, { 'Content-Type': 'application/json; charset=utf-8' });
    response.end(JSON.stringify(payload));
  }
}
