import type { SettingsBootstrapPayload, SettingsBuiltinTool, SettingsCapabilityDependency, SettingsRuntimeSnapshot } from '../../shared/settings-bootstrap';
import type { NotificationsResponseDto } from '../../shared/rust-backend-types';
import type { CanonicalTurn, CanonicalTurnItem } from '../../shared/protocol/canonical-turn';

export interface AgentConnectionEventDetail {
  status: 'connected' | 'recovering';
  reason?: string;
  error?: string;
  baseUrl?: string;
  recovered?: boolean;
}

export interface AgentWorkspaceSummary {
  workspaceId: string;
  name: string;
  rootPath: string;
  rootPathRef?: string;
  isActive: boolean;
  revision: number;
  primarySourceFolderId: string;
  sourceFolders: AgentWorkspaceSourceFolder[];
}

export interface AgentWorkspaceSourceFolder {
  sourceFolderId: string;
  path: string;
  pathRef?: string;
  name?: string;
  isPrimary: boolean;
}

export interface AgentSessionSummary {
  id: string;
  workspaceId?: string;
  name?: string;
  createdAt: number;
  updatedAt: number;
  messageCount?: number;
  isRunning?: boolean;
  runningTaskCount?: number;
  hasUnreadCompletion?: boolean;
  preview?: string;
}

/** 启动时因持久化文件损坏而无法加载的会话；原文件保留在 sourcePath。 */
export interface AgentUnavailableSession {
  sessionId: string;
  sourcePath: string;
  reason: string;
}

export interface AgentWorkspaceSessionsSnapshot {
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  workspace: AgentWorkspaceSummary;
  sessions: AgentSessionSummary[];
  unavailableSessions: AgentUnavailableSession[];
}

export interface AgentWorkspaceRegistrationResult {
  workspaceId: string;
  workspaces: AgentWorkspaceSummary[];
}

export interface RawAgentWorkspaceSummary {
  workspaceId?: string;
  rootPath?: string;
  rootPathRef?: string | null;
  name?: string | null;
  isActive?: boolean;
  revision?: number;
  primarySourceFolderId?: string;
  sourceFolders?: Array<{
    sourceFolderId?: string;
    path?: string;
    pathRef?: string | null;
    name?: string | null;
    isPrimary?: boolean;
  }>;
}

export interface RawAgentSessionSummary {
  id?: string;
  sessionId?: string;
  workspaceId?: string;
  name?: string | null;
  title?: string | null;
  createdAt?: number;
  updatedAt?: number;
  messageCount?: number;
  isRunning?: boolean;
  runningTaskCount?: number;
  hasUnreadCompletion?: boolean;
  preview?: string | null;
}

export type AgentRuntimeSettings = SettingsRuntimeSnapshot;

export type AgentSettingsBootstrapSnapshot = SettingsBootstrapPayload;

export interface AgentToolCatalogDiagnosticsSnapshot {
  builtinTools: SettingsBuiltinTool[];
  capabilityDependencies: SettingsCapabilityDependency[];
  commandEnvironment: {
    source: string;
    pathAvailable: boolean;
    commands: Array<{ name: string; available: boolean; path: string | null }>;
  };
}

export interface ModelPerformanceStats {
  ttftSampleCount: number;
  ttftTotalMs: number;
  tpsSampleCount: number;
  textOutputTokens: number;
  contentDurationMs: number;
  lastObservedAt: number;
  lastTtftMs: number | null;
  lastTps: number | null;
  responseSampleCount?: number;
  responseOutputTokens?: number;
  responseDurationMs?: number;
  lastResponseTps?: number | null;
}

export interface PromptCacheStats {
  writeObservedCallCount?: number;
  observedCallCount: number;
  hitCallCount: number;
  inputTokens: number;
  readTokens: number;
  writeTokens: number;
  tokenHitRate: number | null;
  requestHitRate: number | null;
}

export interface AgentExecutionStatsItem {
  pluginId?: string | null;
  tokenObservedCallCount?: number;
  performance?: ModelPerformanceStats;
  inputTokens?: number;
  cache?: PromptCacheStats;
  templateId: string;
  engineId: string;
  bindingRevision: number;
  role: 'worker' | 'orchestrator' | 'auxiliary' | 'image_generation' | 'vision';
  displayName: string;
  provider?: string;
  declaredModelSpec?: string;
  resolvedModel?: string;
  modelIdentityKey?: string;
  llmCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
}

export interface AgentExecutionModelStatsItem {
  modelIdentityKey: string;
  provider: string;
  declaredModelSpec: string;
  resolvedModel: string;
  baseUrlFingerprint: string;
  reasoningEffort?: import('../../shared/orchestrator-session-config').OrchestratorReasoningEffort | null;
  totals: AgentExecutionStatsPayload['totals'];
}

export interface AgentExecutionStatsPayload {
  version: number;
  lastAppliedLedgerSeq?: number;
  updatedAt: number;
  totals: {
    tokenObservedCallCount?: number;
    performance?: ModelPerformanceStats;
    inputTokens?: number;
    cache?: PromptCacheStats;
    llmCallCount: number;
    assignmentCount: number;
    turnCount: number;
    totalTokens: number;
    netInputTokens: number;
    netOutputTokens: number;
    successCount: number;
    failureCount: number;
  };
  items: AgentExecutionStatsItem[];
  models: AgentExecutionModelStatsItem[];
}

export type AgentNotificationsPayload = NotificationsResponseDto;

export interface AgentKnowledgeMutationPayload {
  success: boolean;
  workspaceId: string;
  workspacePath: string;
  knowledgeCount: number;
  error?: string;
  payload?: Record<string, unknown>;
}

export interface AgentFilePreviewPayload {
  filePath: string | null;
  content: string;
  sessionId?: string | null;
  workspaceId: string;
  workspacePath: string;
  executionGroupId?: string | null;
  absolutePath?: string;
  exists?: boolean;
  language?: string;
}

export interface AgentChangeDiffPayload {
  filePath: string | null;
  diff: string;
  sessionId?: string | null;
  workspaceId: string;
  workspacePath: string;
  executionGroupId?: string | null;
  additions?: number;
  deletions?: number;
  originalContent?: string | null;
  currentContent?: string | null;
  currentAbsolutePath?: string;
  currentExists?: boolean;
  pendingChangesState?: unknown;
}

export interface AgentSessionTurnImagePayload {
  name: string;
  dataUrl: string;
}

export interface AgentSessionTurnResult {
  sessionId: string;
  entryId: string;
  eventId: string;
  acceptedAt: number;
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  createdSession: boolean;
  route: 'chat' | 'execute' | 'task' | 'continue' | 'steer';
  turnId?: string | null;
  requestId?: string | null;
  executionProfile?: 'conversation' | 'task' | null;
  status?: string | null;
  eventSequence?: number | null;
  sessionSummary?: AgentSessionSummary | null;
  /** Root task ID when the backend created an agent run for this action. */
  rootTaskId?: string | null;
  /** 当前轮次实际执行的 action task ID。 */
  actionTaskId?: string | null;
  executionChainRef?: string | null;
  /** 后端生成的 canonical 用户消息 item ID。 */
  userMessageItemId?: string | null;
  queued?: boolean;
  queueId?: string | null;
  queuePosition?: number | null;
  canonicalSchemaVersion?: string | null;
  canonicalEventKind?: string | null;
  canonicalEventId?: string | null;
  canonicalEventSeq?: number | null;
  canonicalOccurredAt?: number | null;
  canonicalTurn?: CanonicalTurn | null;
  canonicalItem?: CanonicalTurnItem | null;
  /** 仅在 steer 路由下返回：实际接收引导的 Turn ID。 */
  steeredTurnId?: string | null;
}

export interface AgentNotificationScope {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

export interface TerminalSessionRequest {
  sourceFolderId?: string;
  terminalTabId: string;
  workspaceId?: string | null;
  workspacePath?: string;
  sessionId: string;
  cols?: number;
  rows?: number;
}

export type BrowserSessionLifecycle = 'creating' | 'ready' | 'recovering' | 'interrupted' | 'failed' | 'closed';

export type BrowserTabLifecycle = 'creating' | 'ready' | 'suspended' | 'crashed' | 'closed';

export type BrowserControlMode = 'agent' | 'user';

export type BrowserViewportMode = 'auto' | 'fixed';

export type BrowserDeviceType = 'desktop' | 'mobile';

export interface BrowserViewport {
  width: number;
  height: number;
  deviceScaleFactorMillis: number;
  deviceType: BrowserDeviceType;
}

export type BrowserAnnotationStatus = 'active' | 'resolved' | 'stale' | 'deleted';

export interface BrowserNormalizedRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface BrowserAnnotationAnchorBase {
  url: string;
  origin: string | null;
  viewport: BrowserViewport;
  scrollX: number;
  scrollY: number;
  snapshotRevision: number;
}

export type BrowserAnnotationAnchor =
  | (BrowserAnnotationAnchorBase & {
      kind: 'element';
      boundingBox: BrowserNormalizedRect;
      framePath: string[];
      testId: string | null;
      stableId: string | null;
      ariaRole: string | null;
      ariaName: string | null;
      tagName: string;
      textExcerpt: string | null;
      cssPath: string;
      ancestorFingerprint: string;
      domFingerprint: string;
    })
  | (BrowserAnnotationAnchorBase & {
      kind: 'region';
      rect: BrowserNormalizedRect;
    });

export interface BrowserAnnotationSnapshot {
  annotationId: string;
  browserSessionId: string;
  tabId: string;
  sequence: number;
  author: 'user' | 'agent';
  kind: 'element' | 'region';
  anchor: BrowserAnnotationAnchor;
  comment: string;
  status: BrowserAnnotationStatus;
  screenshotArtifactId: string | null;
  createdAt: number;
  updatedAt: number;
}

export interface BrowserTabSnapshot {
  tabId: string;
  browserSessionId: string;
  lifecycle: BrowserTabLifecycle;
  url: string;
  origin: string | null;
  title: string;
  navigationRevision: number;
  snapshotRevision: number;
  createdAt: number;
  updatedAt: number;
  annotations: BrowserAnnotationSnapshot[];
  /** 用户直接操作页面后保持控制；代理需等用户交还才能继续操作该页面。 */
  userControlHeld?: boolean;
}

export interface BrowserSessionSnapshot {
  browserSessionId: string;
  workspaceId: string | null;
  sessionId: string;
  profileId: string;
  lifecycle: BrowserSessionLifecycle;
  activeTabId: string | null;
  tabs: BrowserTabSnapshot[];
  runtimeEpoch: number;
  revision: number;
  controlMode: BrowserControlMode;
  controlFence: number;
  agentOccupied: boolean;
  createdAt: number;
  updatedAt: number;
}

export interface BrowserCapabilitySettings {
  inAppBrowserEnabled: boolean;
  browserUseEnabled: boolean;
  /** 开发者工具集（性能、Lighthouse、堆快照等低频诊断）对模型可见。 */
  devtoolsEnabled: boolean;
  /** 浏览器允许访问局域网私有网段；本机回环地址始终允许。 */
  lanAccessEnabled: boolean;
}

export interface BrowserCapabilitiesSnapshot extends BrowserCapabilitySettings {
  revision: number;
  hostStatus: 'stopped' | 'starting' | 'ready' | 'reconnecting' | 'failed';
  hostProtocolCompatible: boolean;
  hostState: string;
  lastErrorCode: string | null;
  platformCapabilities: {
    desktopBrowserSurface: boolean;
    browserRecords: boolean;
    browserAnnotations: boolean;
    browserRemoteSurface: boolean;
  };
}

export type BrowserResourceTabLifecycle = 'creating' | 'ready' | 'suspended' | 'crashed';

export interface BrowserResourceTabSnapshot {
  tabId: string;
  browserSessionId: string;
  sessionId: string;
  workspaceId: string | null;
  sessionTitle: string | null;
  lifecycle: BrowserResourceTabLifecycle;
  url: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  isCurrentSession: boolean;
  isCurrentTab: boolean;
  sessionRunning: boolean;
  canReclaim: boolean;
  reclaimReason: string | null;
}

export interface BrowserResourcesSnapshot {
  maxTabs: number;
  liveTabs: number;
  availableTabs: number;
  reclaimableTabs: number;
  tabs: BrowserResourceTabSnapshot[];
}

export interface ReclaimBrowserResourcesSnapshot {
  reclaimedCount: number;
  reclaimedTabIds: string[];
  skipped: Array<{ tabId: string; reason: string }>;
  resources: BrowserResourcesSnapshot;
}

export type BrowserClientPlatform = 'desktop' | 'web' | 'mobile-web';

export type BrowserAnnotationSelection =
  | {
      kind: 'element';
      navigationRevision: number;
      normalizedX: number;
      normalizedY: number;
    }
  | {
      kind: 'region';
      navigationRevision: number;
      rect: BrowserNormalizedRect;
    };

export interface WorkspaceDirectoryEntry {
  name: string;
  path: string;
  pathRef: string;
  displayPath: string;
  isDirectory: boolean;
  hasChildren?: boolean;
}

export interface DirectoryPathNode {
  name: string;
  pathRef: string;
  displayPath: string;
}

export interface DirectoryPickerEntry extends DirectoryPathNode {
  isDirectory: boolean;
  isHidden: boolean;
}

export interface DirectoryListResult {
  pathRef: string;
  displayPath: string;
  parentPathRef?: string | null;
  breadcrumbs: DirectoryPathNode[];
  roots: DirectoryPathNode[];
  entries: DirectoryPickerEntry[];
  error?: string;
}

export interface WorkspaceDirectoryListResult {
  workspaceId: string | null;
  workspacePath: string | null;
  sessionId?: string | null;
  path: string;
  pathRef: string;
  parent: string;
  parentPathRef: string;
  entries: WorkspaceDirectoryEntry[];
}

export interface ResolvedAgentPath {
  pathRef: string;
  displayPath: string;
  name: string;
  kind: 'file' | 'directory';
}

export interface AgentFileRevealTarget {
  targetPathRef: string;
  workspaceRootPathRef: string;
  displayPath: string;
  ancestorPathRefs: string[];
}

export interface TurnItemsPageDto {
  turnId: string;
  items: unknown[];
  hasMoreBefore: boolean;
  beforeItemSeq: number | null;
  omittedItemCount: number;
}

export interface SafeguardAuditEntry {
  eventId: string;
  sequence: number;
  occurredAt: number;
  toolName: string | null;
  decision: string | null;
  accessProfile: string | null;
  matchedRules: Array<{ category: string; pattern: string; action: string }>;
  sessionId: string | null;
  sessionTitle: string | null;
}

export interface SafeguardAuditPage {
  total: number;
  entries: SafeguardAuditEntry[];
  nextBefore: number | null;
}

export interface WorkspaceBranchesResult {
  isRepo: boolean;
  currentBranch: string | null;
  branches: string[];
  remoteBranches: string[];
  structuredBranches: GitBranch[];
  status: WorkspaceVcsStatus | null;
  observation?: GitObservation | null;
  sessionContext?: SessionCodeContext | null;
  contextDrift?: boolean;
}

export interface GitBranch {
  name: string;
  fullRef: string;
  isRemote: boolean;
  isCurrent: boolean;
  head: string | null;
  upstream: string | null;
  worktreePath: string | null;
}

export interface GitWorktree {
  path: string;
  head: string | null;
  branch: string | null;
  bare: boolean;
  detached: boolean;
  locked: boolean;
  prunable: boolean;
  managed: boolean;
}

export interface GitMergePreview {
  target: string;
  targetHead: string;
  mergeBase: string | null;
  fastForward: boolean;
  alreadyUpToDate: boolean;
  incomingCommitCount: number;
  changedPaths: string[];
}

export interface GitObservation {
  repositoryRoot: string;
  gitCommonDir: string;
  worktreePath: string;
  worktreeGitDir: string;
  branch: string | null;
  head: string | null;
  upstream?: string | null;
}

export interface SessionCodeContext {
  sessionId: string;
  workspaceId: string;
  executionRoot: string;
  runtimeWorkspaceRoots: string[];
  contextRevision: number;
  git: {
    desiredRef: string | null;
    baseHead: string | null;
    observedBranch: string | null;
    observedHead: string | null;
    worktreePath: string;
  };
}

export interface WorkspaceVcsStatus {
  upstream?: string | null;
  ahead: number;
  behind: number;
  hasUncommitted: boolean;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  renamed: number;
  deleted: number;
  additions: number;
  deletions: number;
}

export interface GitExpectedContext {
  contextRevision?: number;
  branch?: string | null;
  head?: string | null;
  worktreePath?: string | null;
}

export interface GitOperationResult {
  ok: boolean;
  observation?: GitObservation | null;
  sessionContext?: SessionCodeContext | null;
  data?: unknown;
  error?: {
    kind: string;
    message: string;
    actualBranch?: string | null;
    actualHead?: string | null;
    actualWorktreePath?: string | null;
    conflictedPaths?: string[];
  };
}

export interface AgentKnowledgeGraphQuery {
  focus?: string;
  depth?: number;
  direction?: 'forward' | 'reverse' | 'both';
  nodeKinds?: string[];
  edgeKinds?: string[];
  maxNodes?: number;
  maxEdges?: number;
}

export type AgentKnowledgeKind = 'adr' | 'faq' | 'learning';

export interface AgentKnowledgeItemPayload {
  kind: AgentKnowledgeKind;
  title?: string;
  content: string;
  tags?: string[];
  context?: string;
}

export interface AgentKnowledgeItemPatch {
  title?: string;
  content?: string;
  tags?: string[];
  context?: string;
}

export type AgentGraphNodeRef =
  | { kind: 'knowledge'; knowledgeId: string }
  | { kind: 'file'; path: string }
  | { kind: 'symbol'; path: string; qualifiedName: string; symbolKind: string };

export type AgentKnowledgeRelationKind =
  | 'applies_to'
  | 'explains'
  | 'references'
  | 'related_to'
  | 'supersedes'
  | 'contradicts';

export type AgentKnowledgeRelationStatus = 'active' | 'candidate' | 'dangling' | 'rejected';

export interface AgentKnowledgeRelation {
  relationId: string;
  workspaceId: string;
  source: AgentGraphNodeRef;
  kind: AgentKnowledgeRelationKind;
  target: AgentGraphNodeRef;
  origin: 'deterministic_code' | 'explicit_user' | 'explicit_agent' | 'inferred';
  confidence?: number;
  status: AgentKnowledgeRelationStatus;
  evidence: string[];
  discoveryKey?: string;
  discoveryEvidence?: string[];
  reviewedAt?: number;
  createdAt?: number;
  updatedAt?: number;
}

export interface AgentKnowledgeRelationDraft {
  relationId?: string;
  source: AgentGraphNodeRef;
  kind: AgentKnowledgeRelationKind;
  target: AgentGraphNodeRef;
  origin?: AgentKnowledgeRelation['origin'];
  confidence?: number;
  status?: AgentKnowledgeRelationStatus;
  evidence?: string[];
}

export interface AgentKnowledgeRelationsResponse {
  relations: AgentKnowledgeRelation[];
  totalRelations: number;
  truncated: boolean;
}

export interface AgentLocalSkillInstallRequest {
  directoryPath?: string;
  skillId?: string;
}

export type AgentSkillSource = 'custom' | 'instruction';

export interface AgentPendingChangesPayload {
  generatedAt: number;
  sessionId: string;
  workspaceId: string;
  workspacePath: string;
  pendingChanges: unknown[];
  pendingChangesState: unknown;
}

// ── Magi MCP 服务（对外暴露 Magi 工具的标准 MCP 入口）─────────────────────────

export type McpServerApprovalMode = 'ask' | 'read_write' | 'always';

export interface McpServerToken {
  tokenId: string;
  prefix: string;
  clientName: string;
  workspaceId: string;
  sourceFolderIds: string[];
  defaultSourceFolderId: string | null;
  workspaceAvailable: boolean;
  approvalMode: McpServerApprovalMode;
  attribution: 'external' | 'conversation';
  /** 是否允许经公网隧道使用。 */
  network: boolean;
  createdAtMs: number;
  expiresAtMs: number | null;
  revokedAtMs: number | null;
  lastUsedAtMs: number | null;
  active: boolean;
  state: 'active' | 'expired' | 'revoked' | 'workspace_unavailable';
  /** 是否可以重新查看原文（没有保存原文的令牌只能重新生成）。 */
  hasSecret: boolean;
}

export interface McpServerNetworkStatus {
  enabled: boolean;
  /** stopped / installing / starting / running / error */
  status: string;
  publicUrl: string | null;
  /** 客户端配置用的完整地址。Quick Tunnel 的地址每次开启都会变。 */
  mcpUrl: string | null;
  error: string | null;
  /** quick：随机地址，重启即失效；named：自己的域名，地址固定。 */
  mode: 'quick' | 'named';
  /** 已配置的命名隧道域名（令牌不外露）。 */
  namedHostname: string | null;
}

export interface McpServerDirectStatus {
  enabled: boolean;
  /** 是否正在监听公网地址。 */
  listening: boolean;
  bindHost: string;
  publicHosts: string[];
  /** 客户端配置用的完整 MCP 地址（第一个公网地址）。 */
  mcpUrl: string | null;
  /** 恢复监听失败等原因；已退回仅本机。 */
  error: string | null;
}

export interface McpServerStatus {
  /** 外部会话的隐私与留存设置。 */
  external: McpExternalRetention;
  enabled: boolean;
  running: boolean;
  port: number | null;
  url: string | null;
  stdioEndpoint: string | null;
  stateRoot: string | null;
  activeTokens: number;
  httpSessions: number;
  localConnections: number;
  network: McpServerNetworkStatus;
  direct: McpServerDirectStatus;
  tokens: McpServerToken[];
}

export interface McpDirectoryRequest {
  sessionId: string;
  clientName: string;
  conversationId: string;
  title: string;
  requestedAtMs: number;
}

export interface McpServerCreateTokenRequest {
  clientName: string;
  attribution?: 'external' | 'conversation';
  workspaceId: string;
  sourceFolderIds?: string[];
  sourceFolderId?: string;
  approvalMode?: McpServerApprovalMode;
  /** 有效期天数；缺省 90，0 表示不过期。 */
  ttlDays?: number;
  /** 允许经公网隧道使用。 */
  allowNetwork?: boolean;
}

export interface McpServerCreateTokenResponse {
  /** 令牌原文，只在创建这一次出现。 */
  secret: string;
  token: McpServerToken;
}

export interface McpServerUpdateTokenRequest {
  clientName?: string;
  approvalMode?: McpServerApprovalMode;
  /** 从现在起重新计算的有效期天数；0 表示不过期。缺省不改。 */
  ttlDays?: number;
  allowNetwork?: boolean;
}

export interface McpServerConfigSnippets {
  url: string | null;
  /** 配置里的服务名（带上客户端名，多套配置互不覆盖）。 */
  serverName: string;
  /** 配置里是否已填入令牌原文；否则是 <MAGI_MCP_TOKEN> 占位符。 */
  secretFilled: boolean;
  httpJson: Record<string, unknown> | null;
  stdioJson: Record<string, unknown> | null;
  /** Claude Code 的一行命令。 */
  claudeCli: string | null;
  remoteJson: Record<string, unknown> | null;
  /** 公网地址的 Claude Code 一行命令。 */
  remoteClaudeCli: string | null;
}

export interface McpServerApproval {
  approvalId: string;
  sessionId: string;
  tokenId: string;
  clientName: string;
  tokenPrefix: string;
  workspaceId: string;
  toolName: string;
  /** 对应调用的 id；SafetyGate 的补充确认带 `-safety` 后缀。 */
  toolCallId: string;
  summary: string;
  requestedAtMs: number;
  expiresAtMs: number;
}

export interface McpServerAuditEntry {
  sessionId: string | null;
  tokenId: string;
  clientName: string;
  workspaceId: string;
  tool: string;
  requiresApproval: boolean;
  paths: string[];
  outcome: 'succeeded' | 'failed' | 'denied';
  detail: string | null;
  atMs: number;
}

export interface McpOAuthRequest {
  requestId: string;
  clientName: string;
  redirectUri: string;
  resource: string;
  expiresAtMs: number;
}

export interface McpServerDirectRequest {
  enabled: boolean;
  /** 监听地址，缺省 0.0.0.0（所有网卡）。 */
  bindHost?: string;
  /** 客户端实际使用的公网域名 / IP（不含端口）。 */
  publicHosts?: string[];
  /** 固定监听端口（防火墙需放行）；缺省沿用当前端口。 */
  port?: number;
  /** 开启必须确认：直接对公网开放用的是明文 HTTP。 */
  confirmRisk?: boolean;
}

export interface McpServerNamedTunnelCheck {
  ok: boolean;
  /** ok / not_configured / network_off / dns_unresolved / tunnel_not_connected / origin_unreachable / unexpected */
  code: string;
  httpStatus: number | null;
}

export interface McpServerAuditPage {
  entries: McpServerAuditEntry[];
  /** 符合过滤条件的总条数，用于分页。 */
  total: number;
}

/** 外部 MCP 客户端（一个令牌）的外部会话概要；会话本身是普通会话，调用过程在它的时间线里。 */
export interface McpExternalSession {
  title?: string | null;
  tokenId: string;
  sessionId: string;
  clientName: string;
  workspaceId: string | null;
  workspaceAvailable: boolean;
  lastActivityAtMs: number;
  /** 在途调用数（含等待人工确认的）。 */
  running: number;
}

/** 外部会话的隐私与留存：是否保存调用内容，以及保留多少天（0 表示不自动清理）。 */
export interface McpExternalRetention {
  saveContent: boolean;
  retentionDays: number;
}
