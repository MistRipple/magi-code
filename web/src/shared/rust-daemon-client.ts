// Direct HTTP client for the Rust daemon API.
// Uses the unified transport layer so it works in both browser-direct and VS Code proxy modes.

import { getTransport } from './transport';
import type {
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
  NotificationContextRequestDto,
  ConnectionTestResponseDto,
  CurrentGoalResponseDto,
  GoalActionRequestDto,
  GoalMutationResponseDto,
  GoalUpdateRequestDto,
  DeletedResponseDto,
  DiffResponseDto,
  EngineIdRequestDto,
  EnginesResponseDto,
  FetchModelsRequestDto,
  FetchModelsResponseDto,
  FileContentResponseDto,
  FilesystemBrowseResponseDto,
  FilesystemListResponseDto,
  HealthDto,
  KnowledgeMutationResponseDto,
  McpConnectResponseDto,
  McpDisconnectResponseDto,
  McpServerIdRequestDto,
  McpServersResponseDto,
  McpToolsResponseDto,
  RegisterWorkspaceRequestDto,
  RegisterWorkspaceResponseDto,
  RemoveNotificationRequestDto,
  RemoveWorkspaceRequestDto,
  RemoveWorkspaceResponseDto,
  RemovedResponseDto,
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
  SessionCloseRequestDto,
  SessionDeleteRequestDto,
  SessionInterruptRequestDto,
  SessionInterruptResponseDto,
  NotificationsResponseDto,
  SessionRenameRequestDto,
  ExecutionStatsResponseDto,
  SessionTurnRequestDto,
  SessionTurnResponseDto,
  SettingsUpdateRequestDto,
  SkillInstallResponseDto,
  SkillsConfigSaveResponseDto,
  SkillsLibraryResponseDto,
  SkillUpdateResponseDto,
  TaskInterruptResponseDto,
  AgentRunProjectionDto,
  UpdatedResponseDto,
  VersionHandshakeDto,
  WorkspaceListResponseDto,
  WorkspacePickResponseDto,
  WorkspaceSessionsResponseDto,
} from './rust-backend-types';

function buildApiUrl(baseUrl: string, path: string): string {
  return new URL(path, baseUrl).toString();
}

async function fetchJsonUrl<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await getTransport().request(url, signal ? { signal } : undefined);
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

  public async interruptSession(
    request: SessionInterruptRequestDto,
  ): Promise<SessionInterruptResponseDto> {
    return this.postJson<SessionInterruptResponseDto>('/api/session/interrupt', request);
  }

  public async interruptAgentRun(
    request: unknown,
  ): Promise<TaskInterruptResponseDto> {
    return this.postJson<TaskInterruptResponseDto>('/api/agent-runs/interrupt', request);
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

  public async fetchNotifications(
    workspaceId: string,
    workspacePath?: string,
    sessionId?: string,
  ): Promise<NotificationsResponseDto> {
    const params = new URLSearchParams();
    params.set('workspaceId', workspaceId);
    if (workspacePath) params.set('workspacePath', workspacePath);
    if (sessionId) params.set('sessionId', sessionId);
    const query = params.toString();
    return this.getJson<NotificationsResponseDto>(
      `/api/notifications${query ? `?${query}` : ''}`,
    );
  }

  public async markAllNotificationsRead(
    request: NotificationContextRequestDto,
  ): Promise<NotificationsResponseDto> {
    return this.postJson<NotificationsResponseDto>(
      '/api/notifications/mark-all-read',
      request,
    );
  }

  public async clearNotifications(
    request: NotificationContextRequestDto,
  ): Promise<NotificationsResponseDto> {
    return this.postJson<NotificationsResponseDto>(
      '/api/notifications/clear',
      request,
    );
  }

  public async removeNotification(
    request: RemoveNotificationRequestDto,
  ): Promise<NotificationsResponseDto> {
    return this.postJson<NotificationsResponseDto>(
      '/api/notifications/remove',
      request,
    );
  }

  public async resolveNotification(
    request: RemoveNotificationRequestDto,
  ): Promise<NotificationsResponseDto> {
    return this.postJson<NotificationsResponseDto>(
      '/api/notifications/resolve',
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
    workspacePath?: string,
  ): Promise<WorkspaceSessionsResponseDto> {
    const query = new URLSearchParams();
    if (workspaceId) query.set('workspaceId', workspaceId);
    if (workspacePath) query.set('workspacePath', workspacePath);
    const params = query.toString();
    return this.getJson<WorkspaceSessionsResponseDto>(
      `/api/workspaces/sessions${params ? `?${params}` : ''}`,
    );
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

  public async fetchModels(request: FetchModelsRequestDto): Promise<FetchModelsResponseDto> {
    return this.postJson<FetchModelsResponseDto>('/api/settings/models/fetch', request);
  }

  public async fetchExecutionStats(): Promise<ExecutionStatsResponseDto> {
    return this.getJson<ExecutionStatsResponseDto>('/api/settings/stats');
  }

  public async resetStats(): Promise<ResetStatsResponseDto> {
    return this.postJson<ResetStatsResponseDto>('/api/settings/stats/reset', {});
  }

  // ─── Knowledge ────────────────────────────────────────────────────

  public async clearKnowledge(): Promise<KnowledgeMutationResponseDto> {
    return this.postJson<KnowledgeMutationResponseDto>('/api/knowledge/clear', {});
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
    path: string | undefined,
    workspaceId: string,
    showHidden = false,
  ): Promise<FilesystemListResponseDto> {
    const params = new URLSearchParams();
    if (path) params.set('path', path);
    if (workspaceId) params.set('workspaceId', workspaceId);
    if (showHidden) params.set('showHidden', '1');
    const qs = params.toString();
    return this.getJson<FilesystemListResponseDto>(
      `/api/filesystem/list${qs ? `?${qs}` : ''}`,
    );
  }

  public async fetchFilesystemBrowse(
    pathRef?: string,
    showHidden = false,
  ): Promise<FilesystemBrowseResponseDto> {
    const params = new URLSearchParams();
    if (pathRef) params.set('pathRef', pathRef);
    if (showHidden) params.set('showHidden', '1');
    const qs = params.toString();
    return this.getJson<FilesystemBrowseResponseDto>(
      `/api/filesystem/browse${qs ? `?${qs}` : ''}`,
    );
  }

  // ─── Goal / Agent Run Projection ───────────────────────────────────

  public async getCurrentGoal(
    sessionId: string,
    workspaceId?: string,
    workspacePath?: string,
    signal?: AbortSignal,
  ): Promise<CurrentGoalResponseDto> {
    const query = new URLSearchParams();
    query.set('sessionId', sessionId);
    if (workspaceId) query.set('workspaceId', workspaceId);
    if (workspacePath) query.set('workspacePath', workspacePath);
    return this.getJson<CurrentGoalResponseDto>(
      `/api/goals/current?${query.toString()}`,
      signal,
    );
  }

  public async updateCurrentGoal(
    request: GoalUpdateRequestDto,
  ): Promise<GoalMutationResponseDto> {
    return this.postJson<GoalMutationResponseDto>('/api/goals/current/update', request);
  }

  public async pauseCurrentGoal(
    request: GoalActionRequestDto,
  ): Promise<GoalMutationResponseDto> {
    return this.postJson<GoalMutationResponseDto>('/api/goals/current/pause', request);
  }

  public async resumeCurrentGoal(
    request: GoalActionRequestDto,
  ): Promise<GoalMutationResponseDto> {
    return this.postJson<GoalMutationResponseDto>('/api/goals/current/resume', request);
  }

  public async clearCurrentGoal(
    request: GoalActionRequestDto,
  ): Promise<GoalMutationResponseDto> {
    return this.postJson<GoalMutationResponseDto>('/api/goals/current/clear', request);
  }

  public async clearCurrentGoalTodos(
    request: GoalActionRequestDto,
  ): Promise<CurrentGoalResponseDto> {
    return this.postJson<CurrentGoalResponseDto>('/api/goals/current/todos/clear', request);
  }

  public async getAgentRunProjection(
    rootTaskId: string,
    sessionId: string,
    workspaceId?: string,
    workspacePath?: string,
    signal?: AbortSignal,
  ): Promise<AgentRunProjectionDto> {
    const query = new URLSearchParams();
    query.set('sessionId', sessionId);
    if (workspaceId) query.set('workspaceId', workspaceId);
    if (workspacePath) query.set('workspacePath', workspacePath);
    return this.getJson<AgentRunProjectionDto>(
      `/api/agent-runs/projection/${encodeURIComponent(rootTaskId)}?${query.toString()}`,
      signal,
    );
  }

  private async getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
    return await fetchJsonUrl<T>(buildApiUrl(this.baseUrl, path), signal);
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
