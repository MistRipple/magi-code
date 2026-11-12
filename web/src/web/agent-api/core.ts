import { getDefaultAgentBaseUrl } from '../../shared/agent-shared-config';
import { getTransport } from '../../shared/transport';
import type { SettingsBootstrapPayload, SettingsBuiltinTool, SettingsCapabilityDependency, SettingsRuntimeSnapshot, VisionBuiltinTextModelRule } from '../../shared/settings-bootstrap';
import { i18n } from '../../stores/i18n.svelte';
import { normalizeLocale } from '../../i18n/locales';
import { resolveAgentBindingContext } from '../agent-binding-context';
import type { AgentBindingContext, AgentBindingOverride } from '../agent-binding-context';
import { normalizeToolRuntimeStatus } from '../../shared/tool-catalog';
import type { AgentNotificationScope, AgentSessionSummary, AgentSettingsBootstrapSnapshot, AgentToolCatalogDiagnosticsSnapshot, AgentUnavailableSession, AgentWorkspaceSummary, BrowserClientPlatform, RawAgentSessionSummary, RawAgentWorkspaceSummary } from './types';

export const RUNTIME_BASE_URL_STORAGE_KEY = 'magi-runtime-base-url';

export let cachedWorkspaceSummaries: AgentWorkspaceSummary[] = [];

export function normalizeUnavailableSessions(value: unknown): AgentUnavailableSession[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (!entry || typeof entry !== 'object') return [];
    const record = entry as Record<string, unknown>;
    const sessionId = typeof record.sessionId === 'string' ? record.sessionId.trim() : '';
    if (!sessionId) return [];
    return [{
      sessionId,
      sourcePath: typeof record.sourcePath === 'string' ? record.sourcePath : '',
      reason: typeof record.reason === 'string' ? record.reason : '',
    }];
  });
}

export function normalizeCommandEnvironment(value: unknown): AgentToolCatalogDiagnosticsSnapshot['commandEnvironment'] {
  const record = value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
  const commands = Array.isArray(record.commands)
    ? record.commands.flatMap((entry) => {
        if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return [];
        const item = entry as Record<string, unknown>;
        const name = typeof item.name === 'string' ? item.name.trim() : '';
        if (!name) return [];
        return [{
          name,
          available: item.available === true,
          path: typeof item.path === 'string' && item.path.trim() ? item.path : null,
        }];
      })
    : [];
  return {
    source: typeof record.source === 'string' && record.source.trim() ? record.source : 'unknown',
    pathAvailable: record.pathAvailable === true,
    commands,
  };
}

function normalizeSettingsSectionConfig(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  return value as Record<string, unknown>;
}

export function normalizeMcpServerConfig(server: Record<string, unknown>): Record<string, unknown> {
  const serverId = typeof server.id === 'string' ? server.id.trim() : '';
  return {
    ...server,
    ...(serverId ? { id: serverId } : {}),
  };
}

function normalizeNullableNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function normalizeBindingString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeRequiredBy(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
    .map((item) => item.trim());
}

function normalizeWarningMarkers(value: unknown, marker: string): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item) => typeof item === 'string' && item.trim().length > 0)
    .map(() => marker);
}

export function normalizeBuiltinTools(value: unknown): SettingsBuiltinTool[] {
  if (!Array.isArray(value)) return [];
  const tools: SettingsBuiltinTool[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    const name = typeof record.name === 'string' ? record.name.trim() : '';
    if (!name) continue;
    tools.push({
      name,
      category: typeof record.category === 'string' && record.category.trim()
        ? record.category.trim()
        : 'uncategorized',
      riskLevel: typeof record.riskLevel === 'string' ? record.riskLevel : '',
      approvalRequirement: typeof record.approvalRequirement === 'string' ? record.approvalRequirement : '',
      effectiveApprovalPolicy: typeof record.effectiveApprovalPolicy === 'string' ? record.effectiveApprovalPolicy : 'none',
      accessProfileBehavior: typeof record.accessProfileBehavior === 'string' ? record.accessProfileBehavior : 'restricted_allowed',
      accessMode: typeof record.accessMode === 'string' ? record.accessMode : 'read_only',
      policyScope: typeof record.policyScope === 'string' ? record.policyScope : 'fixed',
      inputSensitivePolicy: record.inputSensitivePolicy === true,
      policySummary: typeof record.policySummary === 'string' ? record.policySummary : '',
      runtimeInternal: record.runtimeInternal === true,
      runtimeStatus: normalizeToolRuntimeStatus(record.runtimeStatus),
      runtimeWarnings: normalizeWarningMarkers(record.runtimeWarnings, 'runtime_warning'),
      schemaStatus: typeof record.schemaStatus === 'string' ? record.schemaStatus : 'ok',
      schemaWarnings: normalizeWarningMarkers(record.schemaWarnings, 'schema_warning'),
      enabled: record.enabled !== false,
    });
  }
  return tools;
}

export function normalizeCapabilityDependencies(value: unknown): SettingsCapabilityDependency[] {
  if (!Array.isArray(value)) return [];
  const dependencies: SettingsCapabilityDependency[] = [];
  for (const entry of value) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    const name = typeof record.name === 'string' ? record.name.trim() : '';
    if (!name) continue;
    const status = typeof record.status === 'string' && record.status.trim()
      ? record.status.trim()
      : 'unknown';
    dependencies.push({
      name,
      status,
      requiredBy: normalizeRequiredBy(record.requiredBy),
      roleCount: normalizeNullableNumber(record.roleCount),
      spawnableRoleCount: normalizeNullableNumber(record.spawnableRoleCount),
      configuredCount: normalizeNullableNumber(record.configuredCount),
      enabledCount: normalizeNullableNumber(record.enabledCount),
      readyCount: normalizeNullableNumber(record.readyCount),
      enabledToolCount: normalizeNullableNumber(record.enabledToolCount),
      readyToolCount: normalizeNullableNumber(record.readyToolCount),
      toolCount: normalizeNullableNumber(record.toolCount),
    });
  }
  return dependencies;
}

function normalizeVisionBuiltinTextModelRules(value: unknown): VisionBuiltinTextModelRule[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return [];
    const record = entry as Record<string, unknown>;
    const id = typeof record.id === 'string' ? record.id.trim() : '';
    const displayName = typeof record.displayName === 'string' ? record.displayName.trim() : '';
    if (!id || !displayName) return [];
    return [{
      id,
      displayName,
      examples: Array.isArray(record.examples)
        ? record.examples.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
        : [],
    }];
  });
}

export function normalizeSettingsBootstrapPayload(
  payload: Record<string, unknown>,
): AgentSettingsBootstrapSnapshot {
  if (payload.scope !== 'personal' && payload.scope !== 'workspace') {
    throw new Error('settings bootstrap 缺少有效的会话作用域');
  }
  const runtimeSettingsRecord = (
    payload.runtimeSettings
    && typeof payload.runtimeSettings === 'object'
    && !Array.isArray(payload.runtimeSettings)
      ? payload.runtimeSettings
      : { locale: 'zh-CN', conversationDisplayMode: 'original' }
  ) as Record<string, unknown>;
  const runtimeSettings: SettingsRuntimeSnapshot = {
    locale: normalizeLocale(runtimeSettingsRecord.locale),
    conversationDisplayMode: runtimeSettingsRecord.conversationDisplayMode === 'summary'
      ? 'summary'
      : 'original',
  };

  return {
    scope: payload.scope,
    workspaceId: normalizeBindingString(payload.workspaceId),
    workspacePath: normalizeBindingString(payload.workspacePath),
    sessionId: normalizeBindingString(payload.sessionId),
    workerConfigs: (
      payload.workerConfigs
      && typeof payload.workerConfigs === 'object'
      && !Array.isArray(payload.workerConfigs)
        ? payload.workerConfigs
        : {}
    ) as Record<string, unknown>,
    orchestratorConfig: normalizeSettingsSectionConfig(payload.orchestratorConfig),
    orchestratorSessionDefaults: normalizeSettingsSectionConfig(payload.orchestratorSessionDefaults),
    orchestratorSessionConfig: normalizeSettingsSectionConfig(payload.orchestratorSessionConfig),
    effectiveOrchestratorConfig: normalizeSettingsSectionConfig(payload.effectiveOrchestratorConfig),
    orchestratorModelError: typeof payload.orchestratorModelError === 'string' ? payload.orchestratorModelError : undefined,
    auxiliaryConfig: normalizeSettingsSectionConfig(payload.auxiliaryConfig),
    visionConfig: normalizeSettingsSectionConfig(payload.visionConfig),
    visionBuiltinTextModelRules: normalizeVisionBuiltinTextModelRules(payload.visionBuiltinTextModelRules),
    imageGenerationConfig: normalizeSettingsSectionConfig(payload.imageGenerationConfig),
    modelContextWindows: Object.fromEntries(
      Object.entries(normalizeSettingsSectionConfig(payload.modelContextWindows))
        .filter(([, value]) => typeof value === 'number' && Number.isFinite(value) && value > 0)
        .map(([model, value]) => [model, Math.floor(value as number)]),
    ),
    userRulesConfig: normalizeSettingsSectionConfig(payload.userRulesConfig),
    skillsConfig: normalizeSettingsSectionConfig(payload.skillsConfig),
    safeguardConfig: normalizeSettingsSectionConfig(payload.safeguardConfig),
    safeguardAudit: (
      payload.safeguardAudit
      && typeof payload.safeguardAudit === 'object'
      && !Array.isArray(payload.safeguardAudit)
        ? payload.safeguardAudit
        : {}
    ) as SettingsBootstrapPayload['safeguardAudit'],
    repositories: Array.isArray(payload.repositories) ? payload.repositories : [],
    mcpServers: Array.isArray(payload.mcpServers) ? payload.mcpServers : [],
    builtinTools: normalizeBuiltinTools(payload.builtinTools),
    capabilityDependencies: normalizeCapabilityDependencies(payload.capabilityDependencies),
    workerStatuses: (
      payload.workerStatuses
      && typeof payload.workerStatuses === 'object'
      && !Array.isArray(payload.workerStatuses)
        ? payload.workerStatuses
        : {}
    ) as SettingsBootstrapPayload['workerStatuses'],
    runtimeSettings,
    roleTemplates: Array.isArray(payload.roleTemplates) ? payload.roleTemplates : undefined,
    registryEngines: Array.isArray(payload.registryEngines) ? payload.registryEngines : undefined,
    registryAgents: Array.isArray(payload.registryAgents) ? payload.registryAgents : undefined,
    bootstrapScope: payload.bootstrapScope === 'core' ? 'core' : 'full',
    mcpServersHydrated: payload.mcpServersHydrated !== false,
  };
}

export class AgentApiError extends Error {
  readonly status: number;
  readonly action: string;
  readonly errorCode?: string;
  readonly detail?: string;
  readonly conflictKind?: string;
  readonly activeTurnId?: string;

  constructor(
    status: number,
    message: string,
    action: string,
    errorCode?: string,
    detail?: string,
    conflictKind?: string,
    activeTurnId?: string,
  ) {
    super(message);
    this.name = 'AgentApiError';
    this.status = status;
    this.action = action;
    this.errorCode = errorCode;
    this.detail = detail;
    this.conflictKind = conflictKind;
    this.activeTurnId = activeTurnId;
  }
}

function safeReadLocalStorage(key: string): string {
  if (typeof window === 'undefined') {
    return '';
  }
  try {
    return localStorage.getItem(key)?.trim() || '';
  } catch (error) {
    console.warn(`[agent-api] localStorage 读取失败(${key})`, error);
    return '';
  }
}

export function safeWriteLocalStorage(key: string, value: string): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    localStorage.setItem(key, value);
  } catch (error) {
    console.warn(`[agent-api] localStorage 写入失败(${key})`, error);
  }
}

export function persistAgentBaseUrl(baseUrl: string): void {
  if (typeof window === 'undefined' || !baseUrl.trim()) {
    return;
  }
  safeWriteLocalStorage(RUNTIME_BASE_URL_STORAGE_KEY, baseUrl.trim());
}

export function getStoredAgentBaseUrl(): string {
  return safeReadLocalStorage(RUNTIME_BASE_URL_STORAGE_KEY);
}

function deriveWorkspaceName(rootPath: string, workspaceId: string): string {
  const fallbackName = rootPath
    .split(/[\\/]/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .pop();
  return fallbackName || workspaceId || 'workspace';
}

export function normalizeWorkspaceSummary(raw: RawAgentWorkspaceSummary): AgentWorkspaceSummary {
  const workspaceId = raw.workspaceId?.trim() || '';
  const rootPath = raw.rootPath?.trim() || '';
  const projectedFolders = Array.isArray(raw.sourceFolders)
      ? raw.sourceFolders.flatMap((folder) => {
          const sourceFolderId = folder.sourceFolderId?.trim() || '';
          const path = folder.path?.trim() || '';
          return sourceFolderId && path
            ? [{
                sourceFolderId,
                path,
                pathRef: folder.pathRef?.trim() || undefined,
                name: folder.name?.trim() || undefined,
                isPrimary: folder.isPrimary === true,
              }]
            : [];
        })
      : [];
  const sourceFolders = projectedFolders;
  return {
    workspaceId,
    rootPath,
    rootPathRef: raw.rootPathRef?.trim() || undefined,
    name: raw.name?.trim() || deriveWorkspaceName(rootPath, workspaceId),
    isActive: raw.isActive === true,
    revision: typeof raw.revision === 'number' && Number.isFinite(raw.revision) ? Math.max(1, Math.floor(raw.revision)) : 1,
    primarySourceFolderId: raw.primarySourceFolderId?.trim() || sourceFolders.find((folder) => folder.isPrimary)?.sourceFolderId || '',
    sourceFolders,
  };
}

export function cacheWorkspaceSummaries(workspaces: AgentWorkspaceSummary[]): AgentWorkspaceSummary[] {
  cachedWorkspaceSummaries = workspaces.filter((workspace) => workspace.workspaceId.length > 0);
  return workspaces;
}

export function findCachedWorkspaceSummary(workspaceId: string): AgentWorkspaceSummary {
  return cachedWorkspaceSummaries.find((workspace) => workspace.workspaceId === workspaceId) ?? {
    workspaceId,
    rootPath: '',
    name: workspaceId || 'workspace',
    isActive: false,
    revision: 1,
    primarySourceFolderId: '',
    sourceFolders: [],
  };
}

export function normalizeSessionSummary(raw: RawAgentSessionSummary): AgentSessionSummary {
  const id = raw.id?.trim() || raw.sessionId?.trim() || '';
  const workspaceId = raw.workspaceId?.trim() || '';
  const createdAt = raw.createdAt ?? Date.now();
  const updatedAt = raw.updatedAt ?? createdAt;
  const name = raw.name?.trim() || raw.title?.trim() || undefined;
  const preview = raw.preview?.trim() || undefined;
  const messageCount = raw.messageCount;
  const runningTaskCount = raw.runningTaskCount;
  const isRunning = raw.isRunning;
  const hasUnreadCompletion = raw.hasUnreadCompletion;
  return {
    id,
    ...(workspaceId ? { workspaceId } : {}),
    name,
    createdAt,
    updatedAt,
    ...(typeof messageCount === 'number' ? { messageCount } : {}),
    ...(typeof isRunning === 'boolean' ? { isRunning } : {}),
    ...(typeof runningTaskCount === 'number' ? { runningTaskCount: Math.max(0, Math.floor(runningTaskCount)) } : {}),
    ...(typeof hasUnreadCompletion === 'boolean' ? { hasUnreadCompletion } : {}),
    ...(preview ? { preview } : {}),
  };
}

export function getConfiguredAgentBaseUrl(): string {
  const viteEnv = (import.meta as ImportMeta & { env?: { VITE_AGENT_BASE_URL?: string } }).env;
  return viteEnv?.VITE_AGENT_BASE_URL?.trim() || '';
}

export function getConfiguredAgentProxyTarget(): string {
  const viteEnv = (import.meta as ImportMeta & { env?: { VITE_AGENT_PROXY_TARGET?: string } }).env;
  return viteEnv?.VITE_AGENT_PROXY_TARGET?.trim() || '';
}

export async function parseAgentJson<T>(response: Response, action: string): Promise<T> {
  if (!response.ok) {
    let backendError: string | null = null;
    let backendErrorCode: string | undefined;
    let backendDetail: string | undefined;
    let conflictKind: string | undefined;
    let activeTurnId: string | undefined;
    const contentType = response.headers.get('content-type') || '';
    if (contentType.includes('application/json')) {
      try {
        // daemon 所有非 2xx 响应都是 ErrorResponseDto：error_code / message / detail。
        const payload = await response.json() as {
          message?: string;
          error_code?: string;
          detail?: string;
          conflict_kind?: string;
          active_turn_id?: string;
        };
        if (typeof payload?.message === 'string' && payload.message.trim()) {
          backendError = payload.message.trim();
        }
        if (typeof payload?.error_code === 'string' && payload.error_code.trim()) {
          backendErrorCode = payload.error_code.trim();
        }
        backendDetail = typeof payload?.detail === 'string' && payload.detail.trim()
          ? payload.detail.trim()
          : undefined;
        conflictKind = typeof payload?.conflict_kind === 'string' && payload.conflict_kind.trim()
          ? payload.conflict_kind.trim()
          : undefined;
        activeTurnId = typeof payload?.active_turn_id === 'string' && payload.active_turn_id.trim()
          ? payload.active_turn_id.trim()
          : undefined;
      } catch {
        // ignore malformed error payload and fallback to generic message
      }
    }
    throw new AgentApiError(
      response.status,
      backendError || `${action} failed: ${response.status}`,
      action,
      backendErrorCode,
      backendDetail,
      conflictKind,
      activeTurnId,
    );
  }

  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    throw new Error(i18n.t('bridge.notConnected'));
  }

  return await response.json() as T;
}

export function resolveAgentBaseUrl(): string {
  if (typeof window === 'undefined') {
    return getDefaultAgentBaseUrl();
  }
  const currentUrl = new URL(window.location.href);
  const servedByAgent = currentUrl.protocol.startsWith('http')
    && (currentUrl.pathname === '/' || currentUrl.pathname === '/web.html' || currentUrl.pathname.startsWith('/assets/'));
  if (servedByAgent) {
    persistAgentBaseUrl(currentUrl.origin);
    return currentUrl.origin;
  }
  const injectedBaseUrl = (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim();
  if (injectedBaseUrl) {
    return injectedBaseUrl;
  }
  const configuredProxyTarget = getConfiguredAgentProxyTarget();
  if (configuredProxyTarget) {
    persistAgentBaseUrl(configuredProxyTarget);
    return configuredProxyTarget;
  }
  const configuredBaseUrl = getConfiguredAgentBaseUrl();
  if (configuredBaseUrl) {
    persistAgentBaseUrl(configuredBaseUrl);
    return configuredBaseUrl;
  }
  const fromQuery = currentUrl.searchParams.get('agentBaseUrl')?.trim();
  if (fromQuery) {
    persistAgentBaseUrl(fromQuery);
    return fromQuery;
  }
  return getStoredAgentBaseUrl() || getDefaultAgentBaseUrl();
}

/** 构造完整的 Agent API URL；公网凭据由统一传输层附加。 */
export function agentUrl(pathname: string, query?: string): string {
  const base = resolveAgentBaseUrl();
  const q = query || '';
  return q ? `${base}${pathname}?${q}` : `${base}${pathname}`;
}

export function browserClientPlatform(): BrowserClientPlatform {
  if (typeof window !== 'undefined' && window.magiDesktop?.runtime === 'electron') {
    return 'desktop';
  }
  if (
    typeof navigator !== 'undefined'
    && /Android|iPhone|iPad|iPod|Mobile/i.test(navigator.userAgent)
  ) {
    return 'mobile-web';
  }
  return 'web';
}

export function buildBoundQuery(
  extra: Record<string, string>,
  options: { includeScope?: boolean; includeSession?: boolean } = {},
): string {
  return buildBoundQueryWithOverride(extra, undefined, options);
}

export function buildFilePreviewQuery(
  filePath: string,
  options: { includeSession?: boolean; sessionId?: string; workspaceId?: string; workspacePath?: string; sourceFolderId?: string } = {},
): string {
  const explicitSessionId = typeof options.sessionId === 'string' && options.sessionId.trim().length > 0;
  const workspaceId = options.workspaceId?.trim() || '';
  const workspacePath = options.workspacePath?.trim() || '';
  const bindingOverride: AgentBindingOverride = workspaceId || workspacePath
    ? { scope: 'workspace', workspaceId, workspacePath, sessionId: options.sessionId }
    : options.workspaceId === ''
      ? { scope: 'personal', sessionId: options.sessionId }
      : { sessionId: options.sessionId };
  return buildBoundQueryWithOverride(
    { filePath, ...(options.sourceFolderId ? { sourceFolderId: options.sourceFolderId } : {}) },
    bindingOverride,
    {
      includeScope: false,
      includeSession: options.includeSession !== false && (options.includeSession === true || explicitSessionId),
    },
  );
}

export function resolveBindingWithOverride(
  bindingOverride?: AgentBindingOverride,
): AgentBindingContext {
  const resolvedBinding = resolveAgentBindingContext();
  const sessionId = bindingOverride?.sessionId === undefined
    ? resolvedBinding.sessionId
    : bindingOverride.sessionId.trim();
  if (!bindingOverride?.scope) {
    return resolvedBinding.scope === 'workspace'
      ? { ...resolvedBinding, sessionId }
      : { scope: 'personal', sessionId };
  }
  if (bindingOverride.scope === 'personal') {
    return { scope: 'personal', sessionId };
  }
  return {
    scope: 'workspace',
    workspaceId: bindingOverride.workspaceId.trim(),
    workspacePath: bindingOverride.workspacePath.trim(),
    sourceFolderId: bindingOverride.sourceFolderId?.trim() || undefined,
    sessionId,
  };
}

export function buildBoundQueryWithOverride(
  extra: Record<string, string>,
  bindingOverride?: AgentBindingOverride,
  options: { includeScope?: boolean; includeSession?: boolean } = {},
): string {
  const binding = resolveBindingWithOverride(bindingOverride);
  const query = new URLSearchParams();
  if (options.includeScope !== false) {
    query.set('scope', binding.scope);
  }
  if (binding.scope === 'workspace') {
    if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
    if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
    if (binding.sourceFolderId) query.set('sourceFolderId', binding.sourceFolderId);
  }
  if (options.includeSession !== false && binding.sessionId) query.set('sessionId', binding.sessionId);
  for (const [key, value] of Object.entries(extra)) {
    if (value) {
      query.set(key, value);
    }
  }
  return query.toString();
}

export function createNotificationBindingOverride(scope: AgentNotificationScope): AgentBindingOverride {
  const sessionId = scope.sessionId?.trim() || '';
  const workspaceId = scope.workspaceId?.trim() || '';
  const workspacePath = scope.workspacePath?.trim() || '';
  return workspaceId || workspacePath
    ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
    : { scope: 'personal', sessionId };
}

export async function postJsonWithBinding<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
  includeSession = true,
  signal?: AbortSignal,
  options: { includeWorkspaceBinding?: boolean } = {},
): Promise<T> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    const bindingPayload = {
      ...(binding.scope === 'workspace' && binding.sourceFolderId ? { sourceFolderId: binding.sourceFolderId } : {}),
      ...(options.includeWorkspaceBinding !== false && binding.scope === 'workspace' && binding.workspaceId
        ? { workspaceId: binding.workspaceId }
        : {}),
      ...(options.includeWorkspaceBinding !== false && binding.scope === 'workspace' && binding.workspacePath
        ? { workspacePath: binding.workspacePath }
        : {}),
      ...(includeSession && binding.sessionId ? { sessionId: binding.sessionId } : {}),
    };
    const response = await getTransport().request(agentUrl(pathname), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      signal,
      body: JSON.stringify({
        ...bindingPayload,
        ...payload,
      }),
    });
    return parseAgentJson<T>(response, action);
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function postBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
  signal?: AbortSignal,
): Promise<T> {
  return await postJsonWithBinding<T>(pathname, payload, action, bindingOverride, true, signal);
}

export async function postWorkspacePathBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
): Promise<T> {
  const binding = resolveBindingWithOverride(bindingOverride);
  if (binding.scope !== 'workspace') {
    throw new AgentApiError(400, '工作区作用域不完整', action);
  }
  const project = findCachedWorkspaceSummary(binding.workspaceId);
  const folder = binding.sourceFolderId
    ? project.sourceFolders.find((folder) => folder.sourceFolderId === binding.sourceFolderId)
    : null;
  if (binding.sourceFolderId && !folder) throw new AgentApiError(400, '源文件夹已不可用，请重新选择', action);
  const workspacePath = folder?.pathRef || folder?.path || binding.workspacePath.trim() || project.rootPath.trim();
  if (!workspacePath) {
    throw new AgentApiError(400, 'workspacePath 不能为空', action);
  }
  return await postJsonWithBinding<T>(
    pathname,
    { ...payload, workspacePath },
    action,
    { ...binding, sourceFolderId: undefined },
    true,
    undefined,
    { includeWorkspaceBinding: false },
  );
}

export async function postWorkspaceBoundJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
  bindingOverride?: AgentBindingOverride,
): Promise<T> {
  return await postJsonWithBinding<T>(pathname, payload, action, bindingOverride, false);
}

export async function postGlobalJson<T>(
  pathname: string,
  payload: Record<string, unknown>,
  action: string,
): Promise<T> {
  return await postJsonWithBinding<T>(
    pathname,
    payload,
    action,
    undefined,
    false,
    undefined,
    { includeWorkspaceBinding: false },
  );
}
