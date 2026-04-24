// Direct HTTP client for the Rust daemon API.
// Uses the unified transport layer so it works in both browser-direct and VS Code proxy modes.

import { getTransport } from './transport';
import type {
  AddAdrRequestDto,
  AddFaqRequestDto,
  AddedResponseDto,
  AgentsResponseDto,
  AgentTemplateIdRequestDto,
  ApproveAllChangesResponseDto,
  ApproveChangeRequestDto,
  ApproveChangeResponseDto,
  AuditUsageLedgerDto,
  BootstrapDto,
  BridgePreflightSnapshotDto,
  BridgeServicesSnapshotDto,
  ClearNotificationsRequestDto,
  ConnectionTestResponseDto,
  CustomToolNameRequestDto,
  DeletedResponseDto,
  DeleteKnowledgeRequestDto,
  DiffResponseDto,
  EngineIdRequestDto,
  EnginesResponseDto,
  EnhancePromptRequestDto,
  EnhancePromptResponseDto,
  EventEnvelope,
  FetchModelsResponseDto,
  FileContentResponseDto,
  FilesystemListResponseDto,
  HealthDto,
  InstructionSkillNameRequestDto,
  KnowledgeAdrsResponseDto,
  KnowledgeFaqSearchResponseDto,
  KnowledgeFaqsResponseDto,
  KnowledgeMutationResponseDto,
  McpConnectResponseDto,
  McpDisconnectResponseDto,
  McpServerIdRequestDto,
  McpServersResponseDto,
  McpToolsResponseDto,
  MessagesResponseDto,
  RegisterWorkspaceRequestDto,
  RegisterWorkspaceResponseDto,
  RemoveNotificationRequestDto,
  RemoveWorkspaceRequestDto,
  RemoveWorkspaceResponseDto,
  RemovedResponseDto,
  ResolveTaskDecisionRequestDto,
  ResolveTaskDecisionResponseDto,
  RepositoriesResponseDto,
  RepositoryIdRequestDto,
  RepositoryRefreshResponseDto,
  ResetStatsResponseDto,
  RevertAllChangesResponseDto,
  RevertChangeRequestDto,
  RevertChangeResponseDto,
  RevertExecutionGroupChangesRequestDto,
  RevertExecutionGroupChangesResponseDto,
  RoleTemplatesResponseDto,
  RuntimeReadModelDto,
  SavedResponseDto,
  SessionContinueRequestDto,
  SessionContinueResponseDto,
  SessionCloseRequestDto,
  SessionDeleteRequestDto,
  SessionNotificationsResponseDto,
  SessionRenameRequestDto,
  SessionStatsResponseDto,
  SessionTurnRequestDto,
  SessionTurnResponseDto,
  SettingsUpdateRequestDto,
  SkillInstallResponseDto,
  SkillsConfigSaveResponseDto,
  SkillsLibraryResponseDto,
  SkillUpdateResponseDto,
  TaskDto,
  TaskInterruptResponseDto,
  TaskProjectionDto,
  UpdatedResponseDto,
  UpdateKnowledgeRequestDto,
  VersionHandshakeDto,
  WorkspaceListResponseDto,
  WorkspacePickResponseDto,
  WorkspaceSessionsResponseDto,
} from './rust-backend-types';

export interface EventStreamHandlers {
  onOpen?: () => void;
  onEvent?: (event: EventEnvelope) => void;
  onError?: (error: Error) => void;
}

export interface EventStreamProbeResult {
  event: EventEnvelope;
}

function buildApiUrl(baseUrl: string, path: string): string {
  return new URL(path, baseUrl).toString();
}

async function fetchJsonUrl<T>(url: string): Promise<T> {
  const response = await getTransport().request(url);
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}: ${url}`);
  }
  return await response.json() as T;
}

export class RustDaemonClient {
  public constructor(private readonly baseUrl: string) {}

  public getBaseUrl(): string {
    return this.baseUrl;
  }

  public async fetchHealth(): Promise<HealthDto> {
    return this.getJson<HealthDto>('/health');
  }

  public async fetchVersion(): Promise<VersionHandshakeDto> {
    return this.getJson<VersionHandshakeDto>('/version');
  }

  public async fetchBootstrap(): Promise<BootstrapDto> {
    return this.getJson<BootstrapDto>('/bootstrap');
  }

  public async fetchRuntimeReadModel(): Promise<RuntimeReadModelDto> {
    return this.getJson<RuntimeReadModelDto>('/runtime/read-model');
  }

  public async fetchAuditUsageLedger(): Promise<AuditUsageLedgerDto> {
    return this.getJson<AuditUsageLedgerDto>('/ledger');
  }

  public async fetchBridgeServices(): Promise<BridgeServicesSnapshotDto> {
    return this.getJson<BridgeServicesSnapshotDto>('/bridges/services');
  }

  public async fetchBridgePreflight(): Promise<BridgePreflightSnapshotDto> {
    return this.getJson<BridgePreflightSnapshotDto>('/bridges/preflight');
  }

  public async submitSessionTurn(
    request: SessionTurnRequestDto,
  ): Promise<SessionTurnResponseDto> {
    return this.postJson<SessionTurnResponseDto>('/api/session/turn', request);
  }

  public async continueSession(
    request: SessionContinueRequestDto,
  ): Promise<SessionContinueResponseDto> {
    return this.postJson<SessionContinueResponseDto>('/api/session/continue', request);
  }

  public async interruptTask(
    request: unknown,
  ): Promise<TaskInterruptResponseDto> {
    return this.postJson<TaskInterruptResponseDto>('/api/task/interrupt', request);
  }

  // ─── Session management ───────────────────────────────────────────

  public async deleteSession(
    request: SessionDeleteRequestDto,
  ): Promise<BootstrapDto> {
    return this.postJson<BootstrapDto>('/api/session/delete', request);
  }

  public async renameSession(
    request: SessionRenameRequestDto,
  ): Promise<BootstrapDto> {
    return this.postJson<BootstrapDto>('/api/session/rename', request);
  }

  public async closeSession(
    request: SessionCloseRequestDto,
  ): Promise<BootstrapDto> {
    return this.postJson<BootstrapDto>('/api/session/close', request);
  }

  public async saveSession(): Promise<BootstrapDto> {
    return this.postJson<BootstrapDto>('/api/session/save', {});
  }

  public async fetchNotifications(
    sessionId?: string,
  ): Promise<SessionNotificationsResponseDto> {
    const params = sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : '';
    return this.getJson<SessionNotificationsResponseDto>(`/api/session/notifications${params}`);
  }

  public async markAllNotificationsRead(): Promise<SessionNotificationsResponseDto> {
    return this.postJson<SessionNotificationsResponseDto>(
      '/api/session/notifications/mark-all-read',
      {},
    );
  }

  public async clearNotifications(
    request?: ClearNotificationsRequestDto,
  ): Promise<SessionNotificationsResponseDto> {
    return this.postJson<SessionNotificationsResponseDto>(
      '/api/session/notifications/clear',
      request ?? {},
    );
  }

  public async removeNotification(
    request: RemoveNotificationRequestDto,
  ): Promise<SessionNotificationsResponseDto> {
    return this.postJson<SessionNotificationsResponseDto>(
      '/api/session/notifications/remove',
      request,
    );
  }

  // ─── Workspace management ─────────────────────────────────────────

  public async fetchWorkspaces(): Promise<WorkspaceListResponseDto> {
    return this.getJson<WorkspaceListResponseDto>('/api/workspaces');
  }

  public async registerWorkspace(
    request: RegisterWorkspaceRequestDto,
  ): Promise<RegisterWorkspaceResponseDto> {
    return this.postJson<RegisterWorkspaceResponseDto>('/api/workspaces/register', request);
  }

  public async removeWorkspace(
    request: RemoveWorkspaceRequestDto,
  ): Promise<RemoveWorkspaceResponseDto> {
    return this.postJson<RemoveWorkspaceResponseDto>('/api/workspaces/remove', request);
  }

  public async pickWorkspace(): Promise<WorkspacePickResponseDto> {
    return this.getJson<WorkspacePickResponseDto>('/api/workspaces/pick');
  }

  public async fetchWorkspaceSessions(
    workspaceId?: string,
  ): Promise<WorkspaceSessionsResponseDto> {
    const params = workspaceId ? `?workspaceId=${encodeURIComponent(workspaceId)}` : '';
    return this.getJson<WorkspaceSessionsResponseDto>(`/api/workspaces/sessions${params}`);
  }

  // ─── Settings ─────────────────────────────────────────────────────

  public async fetchSettingsBootstrap(): Promise<unknown> {
    return this.getJson<unknown>('/api/settings/bootstrap');
  }

  public async fetchRuntimeStatus(): Promise<unknown> {
    return this.getJson<unknown>('/api/status');
  }

  public async updateSetting(
    request: SettingsUpdateRequestDto,
  ): Promise<unknown> {
    return this.postJson<unknown>('/api/settings/update', request);
  }

  public async saveWorkerConfig(config: unknown): Promise<SavedResponseDto> {
    return this.postJson<SavedResponseDto>('/api/settings/worker/save', config);
  }

  public async removeWorkerConfig(request: { worker: string }): Promise<RemovedResponseDto> {
    return this.postJson<RemovedResponseDto>('/api/settings/worker/remove', request);
  }

  public async testWorkerConnection(request: unknown): Promise<ConnectionTestResponseDto> {
    return this.postJson<ConnectionTestResponseDto>('/api/settings/worker/test', request);
  }

  public async saveOrchestratorConfig(config: unknown): Promise<SavedResponseDto> {
    return this.postJson<SavedResponseDto>('/api/settings/orchestrator/save', config);
  }

  public async testOrchestratorConnection(request: unknown): Promise<ConnectionTestResponseDto> {
    return this.postJson<ConnectionTestResponseDto>('/api/settings/orchestrator/test', request);
  }

  public async saveAuxiliaryConfig(config: unknown): Promise<SavedResponseDto> {
    return this.postJson<SavedResponseDto>('/api/settings/auxiliary/save', config);
  }

  public async testAuxiliaryConnection(request: unknown): Promise<ConnectionTestResponseDto> {
    return this.postJson<ConnectionTestResponseDto>('/api/settings/auxiliary/test', request);
  }

  public async saveUserRules(rules: unknown): Promise<SavedResponseDto> {
    return this.postJson<SavedResponseDto>('/api/settings/user-rules/save', rules);
  }

  public async saveSafeguardConfig(config: unknown): Promise<SavedResponseDto> {
    return this.postJson<SavedResponseDto>('/api/settings/safeguard/save', config);
  }

  public async fetchRoleTemplates(): Promise<RoleTemplatesResponseDto> {
    return this.getJson<RoleTemplatesResponseDto>('/api/settings/registry/role-templates');
  }

  public async fetchEngines(): Promise<EnginesResponseDto> {
    return this.getJson<EnginesResponseDto>('/api/settings/registry/engines');
  }

  public async upsertEngine(engine: unknown): Promise<EnginesResponseDto> {
    return this.postJson<EnginesResponseDto>('/api/settings/registry/engines/upsert', engine);
  }

  public async removeEngine(request: EngineIdRequestDto): Promise<EnginesResponseDto> {
    return this.postJson<EnginesResponseDto>('/api/settings/registry/engines/remove', request);
  }

  public async fetchAgents(): Promise<AgentsResponseDto> {
    return this.getJson<AgentsResponseDto>('/api/settings/registry/agents');
  }

  public async upsertAgent(agent: unknown): Promise<AgentsResponseDto> {
    return this.postJson<AgentsResponseDto>('/api/settings/registry/agents/upsert', agent);
  }

  public async removeAgent(request: AgentTemplateIdRequestDto): Promise<AgentsResponseDto> {
    return this.postJson<AgentsResponseDto>('/api/settings/registry/agents/remove', request);
  }

  public async fetchModels(request?: unknown): Promise<FetchModelsResponseDto> {
    return this.postJson<FetchModelsResponseDto>('/api/settings/models/fetch', request ?? {});
  }

  public async fetchSessionStats(): Promise<SessionStatsResponseDto> {
    return this.getJson<SessionStatsResponseDto>('/api/settings/stats/session');
  }

  public async resetStats(): Promise<ResetStatsResponseDto> {
    return this.postJson<ResetStatsResponseDto>('/api/settings/stats/reset', {});
  }

  // ─── Knowledge ────────────────────────────────────────────────────

  public async clearKnowledge(): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/clear', {});
  }

  public async fetchAdrs(
    sessionId?: string,
    workspaceId?: string,
  ): Promise<KnowledgeAdrsResponseDto> {
    const params = new URLSearchParams();
    if (sessionId) params.set('sessionId', sessionId);
    if (workspaceId) params.set('workspaceId', workspaceId);
    const qs = params.toString();
    return this.getJson<KnowledgeAdrsResponseDto>(`/api/knowledge/adrs${qs ? `?${qs}` : ''}`);
  }

  public async fetchFaqs(
    sessionId?: string,
    workspaceId?: string,
  ): Promise<KnowledgeFaqsResponseDto> {
    const params = new URLSearchParams();
    if (sessionId) params.set('sessionId', sessionId);
    if (workspaceId) params.set('workspaceId', workspaceId);
    const qs = params.toString();
    return this.getJson<KnowledgeFaqsResponseDto>(`/api/knowledge/faqs${qs ? `?${qs}` : ''}`);
  }

  public async searchFaqs(
    q?: string,
    sessionId?: string,
    workspaceId?: string,
  ): Promise<KnowledgeFaqSearchResponseDto> {
    const params = new URLSearchParams();
    if (q) params.set('q', q);
    if (sessionId) params.set('sessionId', sessionId);
    if (workspaceId) params.set('workspaceId', workspaceId);
    const qs = params.toString();
    return this.getJson<KnowledgeFaqSearchResponseDto>(
      `/api/knowledge/faqs/search${qs ? `?${qs}` : ''}`,
    );
  }

  public async addAdr(
    request: AddAdrRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/adr/add', request);
  }

  public async updateAdr(
    request: UpdateKnowledgeRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/adr/update', request);
  }

  public async deleteAdr(
    request: DeleteKnowledgeRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/adr/delete', request);
  }

  public async addFaq(
    request: AddFaqRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/faq/add', request);
  }

  public async updateFaq(
    request: UpdateKnowledgeRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/faq/update', request);
  }

  public async deleteFaq(
    request: DeleteKnowledgeRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/faq/delete', request);
  }

  public async deleteLearning(
    request: DeleteKnowledgeRequestDto,
  ): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/learning/delete', request);
  }

  // ─── MCP servers ──────────────────────────────────────────────────

  public async fetchMcpServers(): Promise<McpServersResponseDto> {
    return this.getJson<McpServersResponseDto>('/api/settings/mcp');
  }

  public async addMcpServer(server: unknown): Promise<AddedResponseDto> {
    return this.postJson<AddedResponseDto>('/api/settings/mcp/add', server);
  }

  public async updateMcpServer(server: unknown): Promise<UpdatedResponseDto> {
    return this.postJson<UpdatedResponseDto>('/api/settings/mcp/update', server);
  }

  public async deleteMcpServer(request: McpServerIdRequestDto): Promise<DeletedResponseDto> {
    return this.postJson<DeletedResponseDto>('/api/settings/mcp/delete', request);
  }

  public async fetchMcpTools(request: unknown): Promise<McpToolsResponseDto> {
    return this.postJson<McpToolsResponseDto>('/api/settings/mcp/tools', request);
  }

  public async refreshMcpTools(request: unknown): Promise<McpToolsResponseDto> {
    return this.postJson<McpToolsResponseDto>('/api/settings/mcp/tools/refresh', request);
  }

  public async connectMcpServer(request: unknown): Promise<McpConnectResponseDto> {
    return this.postJson<McpConnectResponseDto>('/api/settings/mcp/connect', request);
  }

  public async disconnectMcpServer(request: unknown): Promise<McpDisconnectResponseDto> {
    return this.postJson<McpDisconnectResponseDto>('/api/settings/mcp/disconnect', request);
  }

  // ─── Repositories ─────────────────────────────────────────────────

  public async fetchRepositories(): Promise<RepositoriesResponseDto> {
    return this.getJson<RepositoriesResponseDto>('/api/settings/repositories');
  }

  public async addRepository(repo: unknown): Promise<AddedResponseDto> {
    return this.postJson<AddedResponseDto>('/api/settings/repositories/add', repo);
  }

  public async updateRepository(repo: unknown): Promise<UpdatedResponseDto> {
    return this.postJson<UpdatedResponseDto>('/api/settings/repositories/update', repo);
  }

  public async deleteRepository(
    request: RepositoryIdRequestDto,
  ): Promise<DeletedResponseDto> {
    return this.postJson<DeletedResponseDto>('/api/settings/repositories/delete', request);
  }

  public async refreshRepository(request: unknown): Promise<RepositoryRefreshResponseDto> {
    return this.postJson<RepositoryRefreshResponseDto>(
      '/api/settings/repositories/refresh',
      request,
    );
  }

  // ─── Skills ───────────────────────────────────────────────────────

  public async fetchSkillsLibrary(): Promise<SkillsLibraryResponseDto> {
    return this.getJson<SkillsLibraryResponseDto>('/api/settings/skills/library');
  }

  public async installSkill(skill: unknown): Promise<SkillInstallResponseDto> {
    return this.postJson<SkillInstallResponseDto>('/api/settings/skills/install', skill);
  }

  public async installLocalSkill(skill: unknown): Promise<SkillInstallResponseDto> {
    return this.postJson<SkillInstallResponseDto>('/api/settings/skills/install-local', skill);
  }

  public async saveSkillsConfig(config: unknown): Promise<SkillsConfigSaveResponseDto> {
    return this.postJson<SkillsConfigSaveResponseDto>('/api/settings/skills/config/save', config);
  }

  public async addCustomTool(tool: unknown): Promise<AddedResponseDto> {
    return this.postJson<AddedResponseDto>('/api/settings/skills/custom-tool/add', tool);
  }

  public async removeCustomTool(
    request: CustomToolNameRequestDto,
  ): Promise<RemovedResponseDto> {
    return this.postJson<RemovedResponseDto>('/api/settings/skills/custom-tool/remove', request);
  }

  public async removeInstructionSkill(
    request: InstructionSkillNameRequestDto,
  ): Promise<RemovedResponseDto> {
    return this.postJson<RemovedResponseDto>(
      '/api/settings/skills/instruction/remove',
      request,
    );
  }

  public async updateSkill(skill: unknown): Promise<SkillUpdateResponseDto> {
    return this.postJson<SkillUpdateResponseDto>('/api/settings/skills/update', skill);
  }

  public async updateAllSkills(): Promise<SkillUpdateResponseDto> {
    return this.postJson<SkillUpdateResponseDto>('/api/settings/skills/update-all', {});
  }

  // ─── Changes / Files / Tunnel ─────────────────────────────────────

  public async fetchDiff(
    filePath?: string,
    sessionId?: string,
    workspaceId?: string,
  ): Promise<DiffResponseDto> {
    const params = new URLSearchParams();
    if (filePath) params.set('filePath', filePath);
    if (sessionId) params.set('sessionId', sessionId);
    if (workspaceId) params.set('workspaceId', workspaceId);
    const qs = params.toString();
    return this.getJson<DiffResponseDto>(`/api/changes/diff${qs ? `?${qs}` : ''}`);
  }

  public async approveChange(
    request: ApproveChangeRequestDto,
  ): Promise<ApproveChangeResponseDto> {
    return this.postJson<ApproveChangeResponseDto>('/api/changes/approve', request);
  }

  public async revertChange(
    request: RevertChangeRequestDto,
  ): Promise<RevertChangeResponseDto> {
    return this.postJson<RevertChangeResponseDto>('/api/changes/revert', request);
  }

  public async approveAllChanges(): Promise<ApproveAllChangesResponseDto> {
    return this.postJson<ApproveAllChangesResponseDto>('/api/changes/approve-all', {});
  }

  public async revertAllChanges(): Promise<RevertAllChangesResponseDto> {
    return this.postJson<RevertAllChangesResponseDto>('/api/changes/revert-all', {});
  }

  public async revertExecutionGroupChanges(
    request: RevertExecutionGroupChangesRequestDto,
  ): Promise<RevertExecutionGroupChangesResponseDto> {
    return this.postJson<RevertExecutionGroupChangesResponseDto>(
      '/api/changes/revert-execution-group',
      request,
    );
  }

  public async fetchFileContent(
    filePath?: string,
    sessionId?: string,
    workspaceId?: string,
  ): Promise<FileContentResponseDto> {
    const params = new URLSearchParams();
    if (filePath) params.set('filePath', filePath);
    if (sessionId) params.set('sessionId', sessionId);
    if (workspaceId) params.set('workspaceId', workspaceId);
    const qs = params.toString();
    return this.getJson<FileContentResponseDto>(`/api/files/content${qs ? `?${qs}` : ''}`);
  }

  public async fetchFilesystemList(
    path?: string,
    workspaceId?: string,
  ): Promise<FilesystemListResponseDto> {
    const params = new URLSearchParams();
    if (path) params.set('path', path);
    if (workspaceId) params.set('workspaceId', workspaceId);
    const qs = params.toString();
    return this.getJson<FilesystemListResponseDto>(
      `/api/filesystem/list${qs ? `?${qs}` : ''}`,
    );
  }

  public async enhancePrompt(
    request: EnhancePromptRequestDto,
  ): Promise<EnhancePromptResponseDto> {
    return this.postJson<EnhancePromptResponseDto>('/api/prompt/enhance', request);
  }

  // ─── Messages ─────────────────────────────────────────────────────

  public async getMessages(sessionId?: string, limit?: number): Promise<MessagesResponseDto> {
    const params = new URLSearchParams();
    if (sessionId) params.set('sessionId', sessionId);
    if (limit !== undefined) params.set('limit', String(limit));
    const qs = params.toString();
    return this.getJson<MessagesResponseDto>(`/api/messages${qs ? `?${qs}` : ''}`);
  }

  // ─── Task Graph ────────────────────────────────────────────────────

  public async getTaskProjection(rootTaskId: string, sessionId: string): Promise<TaskProjectionDto> {
    const query = new URLSearchParams();
    query.set('sessionId', sessionId);
    return this.getJson<TaskProjectionDto>(
      `/api/tasks/graph/${encodeURIComponent(rootTaskId)}?${query.toString()}`,
    );
  }

  public async getTask(taskId: string, sessionId: string): Promise<TaskDto> {
    const query = new URLSearchParams();
    query.set('sessionId', sessionId);
    return this.getJson<TaskDto>(
      `/api/tasks/${encodeURIComponent(taskId)}?${query.toString()}`,
    );
  }

  public async resolveTaskDecision(
    taskId: string,
    sessionId: string,
    request: ResolveTaskDecisionRequestDto,
  ): Promise<ResolveTaskDecisionResponseDto> {
    const query = new URLSearchParams();
    query.set('sessionId', sessionId);
    return this.postJson<ResolveTaskDecisionResponseDto>(
      `/api/tasks/${encodeURIComponent(taskId)}/decision?${query.toString()}`,
      request,
    );
  }

  public connectEvents(handlers: EventStreamHandlers): () => void {
    const url = buildApiUrl(this.baseUrl, '/events');
    const connection = getTransport().connectEventStream(url, {
      onOpen() {
        handlers.onOpen?.();
      },
      onMessage(data: string) {
        try {
          const payload = JSON.parse(data) as EventEnvelope;
          handlers.onEvent?.(payload);
        } catch (error) {
          handlers.onError?.(
            error instanceof Error ? error : new Error(String(error)),
          );
        }
      },
      onError() {
        handlers.onError?.(new Error('事件流连接失败'));
      },
    });
    return () => {
      connection.close();
    };
  }

  public async probeEventStream(
    timeoutMs = 2_000,
    options: { trigger?: () => Promise<unknown> } = {},
  ): Promise<EventStreamProbeResult> {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => {
      controller.abort();
    }, timeoutMs);

    try {
      const response = await getTransport().request(buildApiUrl(this.baseUrl, '/events'), {
        headers: {
          Accept: 'text/event-stream',
        },
        signal: controller.signal,
      });
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}: /events`);
      }
      if (!response.body) {
        throw new Error('/events 响应缺少可读事件流 body');
      }

      if (options.trigger) {
        await options.trigger();
      }

      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';

      try {
        for (;;) {
          const parsedEvent = parseFirstEventEnvelope(buffer);
          if (parsedEvent) {
            return { event: parsedEvent };
          }

          const { done, value } = await reader.read();
          if (done) {
            break;
          }
          buffer += decoder.decode(value, { stream: true });
        }
      } finally {
        try {
          await reader.cancel();
        } catch {
          // Best-effort cleanup for the probe connection.
        }
      }

      const parsedEvent = parseFirstEventEnvelope(buffer);
      if (parsedEvent) {
        return { event: parsedEvent };
      }
      throw new Error('/events 已连接，但未读取到可解析的事件');
    } catch (error) {
      if (controller.signal.aborted) {
        throw new Error(`/events 在 ${timeoutMs}ms 内未返回可读事件`);
      }
      throw error;
    } finally {
      clearTimeout(timeoutId);
    }
  }

  private async getJson<T>(path: string): Promise<T> {
    return await fetchJsonUrl<T>(buildApiUrl(this.baseUrl, path));
  }

  private async postJson<T>(path: string, body: unknown): Promise<T> {
    const response = await getTransport().request(buildApiUrl(this.baseUrl, path), {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${path}`);
    }
    return await response.json() as T;
  }
}

function parseFirstEventEnvelope(buffer: string): EventEnvelope | null {
  const normalizedBuffer = buffer.replace(/\r\n/g, '\n');
  const blocks = normalizedBuffer.split('\n\n');
  for (let index = 0; index < blocks.length - 1; index += 1) {
    const block = blocks[index]?.trim();
    if (!block) {
      continue;
    }
    const eventEnvelope = parseEventEnvelopeBlock(block);
    if (eventEnvelope) {
      return eventEnvelope;
    }
  }
  return null;
}

function parseEventEnvelopeBlock(block: string): EventEnvelope | null {
  let eventId = '';
  let eventType = '';
  const dataLines: string[] = [];

  for (const line of block.split('\n')) {
    if (line.startsWith('id:')) {
      eventId = line.slice(3).trimStart();
      continue;
    }
    if (line.startsWith('event:')) {
      eventType = line.slice(6).trimStart();
      continue;
    }
    if (line.startsWith('data:')) {
      dataLines.push(line.slice(5).trimStart());
    }
  }

  if (dataLines.length === 0) {
    return null;
  }

  const payload = JSON.parse(dataLines.join('\n')) as EventEnvelope;
  if (!payload.event_id || !payload.event_type) {
    throw new Error('SSE 事件载荷缺少 event_id 或 event_type');
  }
  if (eventId && payload.event_id !== eventId) {
    throw new Error(
      `SSE event id 不匹配: expected=${eventId}, actual=${payload.event_id}`,
    );
  }
  if (eventType && payload.event_type !== eventType) {
    throw new Error(
      `SSE event type 不匹配: expected=${eventType}, actual=${payload.event_type}`,
    );
  }
  return payload;
}
