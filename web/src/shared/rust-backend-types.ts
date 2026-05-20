// Canonical Rust backend API type definitions.
// Source of truth for all DTO types returned by the Rust daemon HTTP API.
// Formerly maintained in support/frontend-contract/src/contracts.ts.

export type EventCategory = 'Domain' | 'Audit' | 'Usage' | 'Projection' | 'System';

export interface HealthDto {
  status: string;
  serviceName: string;
  apiVersion: string;
}

export interface VersionHandshakeDto {
  apiVersion: string;
  minSupportedUiVersion: string;
  hostScope: string[];
}

export interface SessionTurnImageDto {
  name: string;
  dataUrl: string;
}

export type SessionTurnRouteDto =
  | 'chat'
  | 'execute'
  | 'task'
  | 'continue'
  | 'supplement_context';

export interface SessionTurnRequestDto {
  sessionId?: string | null;
  workspaceId?: string | null;
  text?: string | null;
  skillName?: string | null;
  images: SessionTurnImageDto[];
  requestId?: string | null;
  userMessageId?: string | null;
  placeholderMessageId?: string | null;
  /** 当为 true 时，本次输入直接作为运行时 followup 信号投递到目标任务 Mailbox，不进入分类器。 */
  supplementContext?: boolean;
  /** 当 supplementContext 为 true 时，可选指定投递到哪个任务。 */
  targetTaskId?: string | null;
}

export interface SessionTurnResponseDto {
  sessionId: string;
  entryId: string;
  eventId: string;
  acceptedAt: number;
  createdSession: boolean;
  route: SessionTurnRouteDto;
  /** Root task ID when the backend created a task projection for this action. */
  rootTaskId?: string | null;
  /** 当前轮次实际执行的 action task ID。 */
  actionTaskId?: string | null;
  executionChainRef?: string | null;
  /** 后端生成的用户消息 turnItemId，前端应使用此 ID 创建 canonical 节点 */
  userMessageItemId?: string | null;
  /** 仅在 supplement_context 路由下返回：本次入栈的 mailbox signal ID。 */
  signalRef?: string | null;
  /** 仅在 supplement_context 路由下返回：被投递的任务 ID。 */
  targetTaskId?: string | null;
}

export interface TaskInterruptResponseDto {
  interrupted: boolean;
  eventId: string;
  requestedAt: number;
}

export interface TaskRestartResponseDto {
  restarted: boolean;
  sessionId: string;
  entryId: string;
  eventId: string;
  acceptedAt: number;
  createdSession: boolean;
  rootTaskId: string;
  actionTaskId: string;
  executionChainRef?: string | null;
  requestedAt: number;
}

export interface TaskArchiveResponseDto {
  archived: boolean;
  sessionId: string;
  rootTaskId: string;
  eventId: string;
  requestedAt: number;
}

export interface SessionInterruptRequestDto {
  workspaceId?: string | null;
  sessionId?: string | null;
}

export interface SessionInterruptResponseDto {
  interrupted: boolean;
  sessionId: string;
  turnId?: string | null;
  eventId: string;
  requestedAt: number;
  removedTimelineEntryIds: string[];
}

export interface ServiceInfoDto {
  serviceName: string;
  apiVersion: string;
}

export interface SessionDto {
  sessionId: string;
  title: string;
  status: string;
  createdAt: number;
  updatedAt: number;
}

export interface TimelineEntryDto {
  entryId: string;
  sessionId: string;
  kind: string;
  message: string;
  occurredAt: number;
}

export interface WorkspaceDto {
  workspaceId: string;
  name?: string | null;
  rootPath: string;
  worktreeRoot?: string | null;
  status: string;
  createdAt: number;
  updatedAt: number;
}

export interface NotificationDto {
  notificationId: string;
  sessionId: string;
  kind: string;
  message: string;
  createdAt: number;
  handled: boolean;
}

export interface ExecutionOwnershipDto {
  session_id?: string | null;
  workspace_id?: string | null;
  mission_id?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  execution_chain_ref?: string | null;
}

export interface SnapshotDto {
  snapshotId: string;
  workspaceId: string;
  ownership: ExecutionOwnershipDto;
  label: string;
  createdAt: number;
}

export interface RecoveryHandleDto {
  recoveryId: string;
  workspaceId: string;
  ownership: ExecutionOwnershipDto;
  snapshotId: string;
  diagnosticSummary?: string | null;
  status: string;
  createdAt: number;
  updatedAt: number;
  consumedAt?: number | null;
}

export type BridgeServerKind = 'model' | 'host' | 'mcp';
export type BridgeErrorLayer = 'transport' | 'protocol' | 'remote_business';

export interface BridgeHandshakeDto {
  protocol_version: string;
  server_kind: BridgeServerKind;
  health_method: string;
  supported_methods: string[];
}

export interface BridgeHealthDto {
  protocol_version: string;
  server_kind: BridgeServerKind;
  status: string;
  ok: boolean;
}

export interface BridgeServerShellManifestDto {
  shell_id: string;
  minimum_version: string;
  capability_version: string;
  implementation_source: string;
  capability_profile: string;
  workspace_roots_source: string;
}

export interface BridgeServerShellProfileDto {
  profile_id: string;
  shell_id: string;
  host_kind: string;
  shell_family: string;
  minimum_version: string;
  capability_version: string;
}

export interface BridgeServerCommandCapabilityProfileDto {
  command_name: string;
  capability_id: string;
  interaction_mode: string;
  side_effect_level: string;
  requires_session_context: boolean;
  requires_workspace_context: boolean;
  path_argument_policy: string;
}

export interface BridgeServerSessionDescriptorDto {
  session_id: string;
  session_scope: string;
}

export interface BridgeServerWorkspaceContextDto {
  workspace_id: string;
  workspace_scope: string;
  workspace_roots_source: string;
}

export interface BridgeServerContextResolutionBoundaryDto {
  request_binding: string;
  session_resolution_strategy: string;
  workspace_resolution_strategy: string;
  session_resolution_source: string;
  workspace_resolution_source: string;
}

export interface BridgeServiceDescriptorDto {
  service_name: string;
  shim_kind: string;
  supported_operations: string[];
  capabilities: string[];
  service_health?: string | null;
  service_health_reason?: string | null;
  implementation_source?: string | null;
  capability_profile?: string | null;
  workspace_roots_source?: string | null;
  manager_version?: string | null;
  registry_profile?: string | null;
  registry_manifest?: string | null;
  selection_strategy?: string | null;
  default_server?: string | null;
  default_server_health?: string | null;
  default_server_selection_key?: string | null;
  default_route_status?: string | null;
  default_route_target?: string | null;
  selection_targets?: string[] | null;
  selection_key?: string | null;
  server_manifest?: string | null;
  shell_manifest?: BridgeServerShellManifestDto | null;
  shell_profile?: BridgeServerShellProfileDto | null;
  command_capability_profiles?: BridgeServerCommandCapabilityProfileDto[] | null;
  session_descriptor?: BridgeServerSessionDescriptorDto | null;
  workspace_context?: BridgeServerWorkspaceContextDto | null;
  context_resolution_boundary?: BridgeServerContextResolutionBoundaryDto | null;
}

export interface BridgeServiceCatalogDto {
  protocol_version: string;
  server_kind: BridgeServerKind;
  services: BridgeServiceDescriptorDto[];
}

export interface BridgeProbeErrorDto {
  layer?: BridgeErrorLayer | null;
  code?: number | null;
  message: string;
}

export interface BridgeServiceSnapshotDto {
  server_kind: BridgeServerKind;
  handshake?: BridgeHandshakeDto | null;
  handshake_error?: BridgeProbeErrorDto | null;
  health?: BridgeHealthDto | null;
  health_error?: BridgeProbeErrorDto | null;
  service_catalog?: BridgeServiceCatalogDto | null;
  service_catalog_error?: BridgeProbeErrorDto | null;
}

export interface BridgeServicesSnapshotDto {
  services: BridgeServiceSnapshotDto[];
}

export interface BridgePreflightCheckDto {
  check_name: string;
  target: string;
  ok: boolean;
  response_excerpt?: string | null;
  error?: BridgeProbeErrorDto | null;
}

export interface BridgePreflightServiceDto {
  server_kind: BridgeServerKind;
  checks: BridgePreflightCheckDto[];
}

export interface BridgePreflightSnapshotDto {
  services: BridgePreflightServiceDto[];
}

export interface BridgeModelContractDto {
  contract_profile: string;
  payload_kind: string;
  contract_ok: boolean;
  has_content: boolean;
  has_finish_reason: boolean;
  has_usage: boolean;
  tool_call_count: number;
  blocking_reason?: string | null;
}

export interface BridgeMcpDefaultRouteContractDto {
  route_status: string;
  route_target: string;
  resolved_server?: string | null;
  describe_ok: boolean;
  blank_selection_ok: boolean;
  contract_ok: boolean;
  blocking_reason?: string | null;
}

export interface BridgeMcpDefaultRouteGateDto {
  route_status: string;
  route_target: string;
  resolved_server?: string | null;
  contract_ok: boolean;
}

export interface EventEnvelope {
  event_id: string;
  event_type: string;
  category: EventCategory;
  occurred_at: number;
  sequence: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  payload: unknown;
}

export interface RuntimeLedgerDto {
  schema_version: string;
  audit_count: number;
  usage_count: number;
  next_sequence: number;
  last_persist_error?: string | null;
}

export interface RuntimeSectionOrderingRuleDto {
  target: string;
  ordering: string;
}

export interface RuntimeContractFreezeSummaryDto {
  canonical_entries: string[];
  canonical_signature: string;
}

export interface RuntimeContractFreezeGateSummaryDto {
  is_ready: boolean;
  blocking_issue_count: number;
  blocking_issues: string[];
  readiness_checks: string[];
  required_validation_refs: string[];
  satisfied_validation_refs: string[];
  pending_validation_refs: string[];
}

export interface RuntimeContractFreezeEvidenceSummaryDto {
  evidence_entries: string[];
  evidence_signature: string;
}

export interface RuntimeContractFreezeReportSummaryDto {
  status: string;
  ready_check_count: number;
  blocking_issue_count: number;
  summary_line: string;
  evidence_signature: string;
}

export interface RuntimeContractFreezeConsistencySummaryDto {
  is_consistent: boolean;
  issue_count: number;
  issues: string[];
}

export interface RuntimeContractFreezeClosureSummaryDto {
  is_closed: boolean;
  final_status: string;
  closure_issue_count: number;
  closure_issues: string[];
}

export interface RuntimeContractValidationSummaryDto {
  is_valid: boolean;
  issue_count: number;
  issues: string[];
}

export interface RuntimeMetaDto {
  contract_version: string;
  contract_sections: string[];
  ordering_strategy: string;
  section_ordering_rules: RuntimeSectionOrderingRuleDto[];
  ledger: RuntimeLedgerDto;
  freeze: RuntimeContractFreezeSummaryDto;
  freeze_gate: RuntimeContractFreezeGateSummaryDto;
  freeze_evidence: RuntimeContractFreezeEvidenceSummaryDto;
  freeze_report: RuntimeContractFreezeReportSummaryDto;
  freeze_consistency: RuntimeContractFreezeConsistencySummaryDto;
  freeze_closure: RuntimeContractFreezeClosureSummaryDto;
  validation: RuntimeContractValidationSummaryDto;
  latest_sequence: number;
  recent_event_count: number;
}

export interface EventCategoryCountsDto {
  domain: number;
  audit: number;
  usage: number;
  projection: number;
  system: number;
}

export interface RuntimeActivitySummaryDto {
  execution_group_event_count: number;
  worker_event_count: number;
  tool_event_count: number;
  skill_dispatch_event_count: number;
  executor_event_count: number;
  recovery_event_count: number;
  active_task_ids: string[];
}

export interface RuntimeDiagnosticSummaryDto {
  running_execution_group_count: number;
  failed_execution_group_count: number;
  running_task_count: number;
  failed_task_count: number;
  running_assignment_count: number;
  failed_assignment_count: number;
  active_worker_count: number;
  failed_worker_count: number;
  blocked_tool_count: number;
  failed_tool_count: number;
  governance_total_count: number;
  governance_allowed_count: number;
  governance_needs_approval_count: number;
  governance_blocked_count: number;
  governance_rejected_count: number;
  rejected_skill_dispatch_count: number;
  failed_skill_dispatch_count: number;
  context_execution_group_count: number;
  context_used_knowledge_count: number;
  context_used_memory_count: number;
  context_code_index_knowledge_count: number;
  context_extracted_memory_count: number;
  degraded_executor_count: number;
  unavailable_executor_count: number;
  pending_recovery_count: number;
  resumed_recovery_count: number;
}

export interface RuntimeOverviewDto {
  category_counts: EventCategoryCountsDto;
  activity: RuntimeActivitySummaryDto;
  diagnostics: RuntimeDiagnosticSummaryDto;
}

export interface MissionMetricsSummaryDto {
  turn_count: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_tokens: number;
  wall_clock_millis: number;
  first_turn_started_at?: number | null;
  last_turn_finished_at?: number | null;
  last_lifecycle_phase?: string | null;
}

export interface ExecutionGroupRuntimeSummaryDto {
  mission_id: string;
  event_count: number;
  audit_event_count: number;
  skill_dispatch_count: number;
  builtin_dispatch_count: number;
  bridge_dispatch_count: number;
  rejected_dispatch_count: number;
  failed_dispatch_count: number;
  active_task_ids: string[];
  latest_event_type?: string | null;
  current_status?: string | null;
  lifecycle_phase?: string | null;
  metrics?: MissionMetricsSummaryDto | null;
}

export interface TaskRuntimeSummaryDto {
  task_id: string;
  mission_id?: string | null;
  assignment_id?: string | null;
  event_count: number;
  audit_event_count: number;
  skill_dispatch_count: number;
  builtin_dispatch_count: number;
  bridge_dispatch_count: number;
  rejected_dispatch_count: number;
  failed_dispatch_count: number;
  latest_event_type?: string | null;
  current_status?: string | null;
}

export interface AssignmentRuntimeSummaryDto {
  assignment_id: string;
  mission_id?: string | null;
  event_count: number;
  audit_event_count: number;
  dispatch_count: number;
  task_ids: string[];
  completed_task_count: number;
  failed_task_count: number;
  latest_event_type?: string | null;
  current_status?: string | null;
}

export interface WorkerRuntimeSummaryDto {
  worker_id: string;
  event_count: number;
  audit_event_count: number;
  report_count: number;
  tool_call_count: number;
  skill_dispatch_count: number;
  builtin_dispatch_count: number;
  bridge_dispatch_count: number;
  rejected_dispatch_count: number;
  failed_dispatch_count: number;
  current_task_id?: string | null;
  latest_event_type?: string | null;
  current_status?: string | null;
  current_stage?: string | null;
}

export interface ToolRuntimeSummaryDto {
  tool_name: string;
  tool_kind?: string | null;
  event_count: number;
  success_count: number;
  blocked_count: number;
  failed_count: number;
  latest_status?: string | null;
  latest_event_type?: string | null;
  worker_ids: string[];
  task_ids: string[];
  session_ids: string[];
  workspace_ids: string[];
}

export interface SessionRuntimeSummaryDto {
  session_id: string;
  event_count: number;
  audit_event_count: number;
  worker_event_count: number;
  tool_event_count: number;
  recovery_event_count: number;
  latest_event_type?: string | null;
  active_task_ids: string[];
  recovery_ids: string[];
  current_status?: string | null;
  last_update?: number | null;
  mission_id?: string | null;
  root_task_id?: string | null;
  root_task_status?: string | null;
  execution_chain_ref?: string | null;
  recovery_ref?: string | null;
  has_recoverable_chain?: boolean;
  recoverable_branch_count?: number;
  active_branches: SessionRuntimeBranchSummaryDto[];
}

export interface SessionRuntimeBranchSummaryDto {
  task_id: string;
  worker_id: string;
  status: string;
  stage: string;
  lease_id?: string | null;
  execution_intent_ref?: string | null;
  binding_lifecycle?: string | null;
  checkpoint_stage?: string | null;
  next_step_index?: number | null;
  checkpoint_at?: number | null;
  resume_mode?: string | null;
  is_primary: boolean;
}

export interface WorkspaceRuntimeSummaryDto {
  workspace_id: string;
  event_count: number;
  audit_event_count: number;
  worker_event_count: number;
  tool_event_count: number;
  recovery_event_count: number;
  latest_event_type?: string | null;
  active_task_ids: string[];
  recovery_ids: string[];
  execution_chain_refs: string[];
  current_status?: string | null;
  last_update?: number | null;
  execution_chain_ref?: string | null;
  recovery_ref?: string | null;
}

export interface RuntimeDetailsDto {
  execution_groups: ExecutionGroupRuntimeSummaryDto[];
  tasks: TaskRuntimeSummaryDto[];
  assignments: AssignmentRuntimeSummaryDto[];
  workers: WorkerRuntimeSummaryDto[];
  tools: ToolRuntimeSummaryDto[];
  sessions: SessionRuntimeSummaryDto[];
  workspaces: WorkspaceRuntimeSummaryDto[];
}

export interface DispatchRuntimeSummaryDto {
  total_dispatches: number;
  resume_dispatches: number;
  latest_dispatch_reason?: string | null;
  active_assignment_ids: string[];
}

export interface RuntimeAttentionSummaryDto {
  failed_execution_group_ids: string[];
  failed_task_ids: string[];
  failed_assignment_ids: string[];
  failed_worker_ids: string[];
  blocked_tool_names: string[];
  governance_blocked_task_ids: string[];
  governance_approval_required_task_ids: string[];
  governance_rejected_task_ids: string[];
  governance_blocked_worker_ids: string[];
  governance_approval_required_worker_ids: string[];
  governance_rejected_worker_ids: string[];
  rejected_skill_dispatch_worker_ids: string[];
  failed_skill_dispatch_worker_ids: string[];
  degraded_executor_worker_ids: string[];
  unavailable_executor_worker_ids: string[];
  pending_recovery_ids: string[];
}

export interface RuntimeWorkQueueSummaryDto {
  running_execution_group_ids: string[];
  running_task_ids: string[];
  running_assignment_ids: string[];
  active_worker_ids: string[];
  pending_recovery_ids: string[];
}

export interface RecoveryResumeObservationSummaryDto {
  total_recoveries: number;
  resume_command_count: number;
  resume_dispatch_count: number;
  mission_resumed_count: number;
  worker_resumed_count: number;
  affected_execution_group_ids: string[];
  affected_worker_ids: string[];
}

export interface RuntimeOperationsDto {
  dispatch: DispatchRuntimeSummaryDto;
  attention: RuntimeAttentionSummaryDto;
  work_queues: RuntimeWorkQueueSummaryDto;
  resume_observation: RecoveryResumeObservationSummaryDto;
}

export interface RecoveryDiagnosticSummaryDto {
  recovery_id: string;
  event_count: number;
  latest_stage: string;
  latest_event_type: string;
  latest_sequence: number;
  latest_occurred_at: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  execution_chain_ref?: string | null;
  diagnostic_summary?: string | null;
  current_status: string;
}

export interface RecoveryActivityEntryDto {
  recovery_id: string;
  stage: string;
  event_type: string;
  category: string;
  occurred_at: number;
  sequence: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  execution_chain_ref?: string | null;
  diagnostic_summary?: string | null;
}

export interface RecoveryReadModelDto {
  active_recovery_ids: string[];
  entries: RecoveryActivityEntryDto[];
  summaries: RecoveryDiagnosticSummaryDto[];
}

export interface RuntimeReadModelDto {
  meta: RuntimeMetaDto;
  overview: RuntimeOverviewDto;
  details: RuntimeDetailsDto;
  operations: RuntimeOperationsDto;
  recovery: RecoveryReadModelDto;
}

export interface AuditUsageLedgerDto {
  schema_version: string;
  next_sequence: number;
  audit_count: number;
  usage_count: number;
  persistence_path?: string | null;
  last_persist_error?: string | null;
}

export interface BootstrapDto {
  service: ServiceInfoDto;
  generatedAt: number;
  currentSession?: SessionDto | null;
  sessions: SessionDto[];
  timeline: TimelineEntryDto[];
  canonicalTurns?: unknown[];
  workspaces: WorkspaceDto[];
  snapshots: SnapshotDto[];
  recoveryHandles: RecoveryHandleDto[];
  runtimeReadModel: RuntimeReadModelDto;
  auditUsageLedger: AuditUsageLedgerDto;
  bridgeServices: BridgeServicesSnapshotDto;
  bridgePreflight: BridgePreflightSnapshotDto;
  notifications: NotificationDto[];
  recentEvents: EventEnvelope[];
  hasMoreBefore: boolean;
  beforeCursor?: string | null;
}

// ─── Session management endpoints ───────────────────────────────────

export interface SessionDeleteRequestDto {
  sessionId: string;
  workspaceId?: string | null;
  workspace_id?: string | null;
}

export interface SessionRenameRequestDto {
  sessionId: string;
  name: string;
  workspaceId?: string | null;
  workspace_id?: string | null;
}

export interface SessionCloseRequestDto {
  sessionId: string;
  workspaceId?: string | null;
  workspace_id?: string | null;
}

export interface SessionSaveRequestDto {
  sessionId?: string | null;
  workspaceId?: string | null;
  workspace_id?: string | null;
}

export interface SessionContinueRequestDto {
  sessionId: string;
  promptText?: string | null;
  requestedWorkerIds?: string[];
  requestId?: string | null;
  userMessageId?: string | null;
  placeholderMessageId?: string | null;
}

export interface SessionContinueResponseDto {
  sessionId: string;
  missionId: string;
  rootTaskId: string;
  executionChainRef: string;
  resumedBranchCount: number;
  status: string;
  runnerStarted: boolean;
  eventId: string;
  continuedAt: number;
}

export interface SessionSelectionResponseDto {
  sessionId: string;
  currentSession?: SessionDto | null;
}

export interface SessionNotificationItemDto {
  notificationId: string;
  message: string;
  kind: string;
  level: string;
  title?: string | null;
  source?: string | null;
  handled: boolean;
  read: boolean;
  createdAt: number;
  persistToCenter: boolean;
  actionRequired: boolean;
  countUnread: boolean;
  displayMode?: 'toast' | 'notification_center' | 'silent' | null;
  duration?: number | null;
}

export interface SessionNotificationSnapshotDto {
  lastUpdatedAt: number;
  records: SessionNotificationItemDto[];
}

export interface SessionNotificationsResponseDto {
  sessionId: string;
  workspaceId?: string | null;
  notifications: SessionNotificationSnapshotDto;
}

export interface ClearNotificationsRequestDto {
  workspaceId?: string | null;
  sessionId?: string | null;
}

export interface RemoveNotificationRequestDto {
  workspaceId?: string | null;
  sessionId?: string | null;
  notificationId: string;
}

export interface AppendNotificationRequestDto {
  workspaceId?: string | null;
  sessionId?: string | null;
  notificationId?: string | null;
  kind?: 'incident' | 'audit' | 'center' | 'toast' | string | null;
  level?: string | null;
  title?: string | null;
  message: string;
  source?: string | null;
  persistToCenter?: boolean | null;
  actionRequired?: boolean | null;
  countUnread?: boolean | null;
  displayMode?: 'toast' | 'notification_center' | 'silent' | null;
  duration?: number | null;
}

export type MarkAllNotificationsReadResponseDto = SessionNotificationsResponseDto;
export type ClearNotificationsResponseDto = SessionNotificationsResponseDto;
export type RemoveNotificationResponseDto = SessionNotificationsResponseDto;
export type AppendNotificationResponseDto = SessionNotificationsResponseDto;

// ─── Workspace management endpoints ─────────────────────────────────

export interface WorkspaceListItemDto {
  workspaceId: string;
  path: string;
  name?: string | null;
  isActive: boolean;
}

export interface WorkspaceListResponseDto {
  workspaces: WorkspaceListItemDto[];
}

export interface RegisterWorkspaceRequestDto {
  path: string;
}

export interface RegisterWorkspaceResponseDto {
  workspaceId: string;
  registered: boolean;
  reused?: boolean;
}

export interface RemoveWorkspaceRequestDto {
  workspaceId: string;
}

export interface RemoveWorkspaceResponseDto {
  removed: boolean;
}

export interface WorkspacePickResponseDto {
  workspaces: WorkspaceListItemDto[];
}

export interface WorkspaceSessionItemDto {
  sessionId: string;
  title: string;
  status: string;
  createdAt: number;
}

export interface WorkspaceSessionsResponseDto {
  sessions: WorkspaceSessionItemDto[];
}

// ─── Settings endpoints ─────────────────────────────────────────────

export interface SettingsUpdateRequestDto {
  key: string;
  value: unknown;
}

export interface ConnectionTestResponseDto {
  success: boolean;
  message: string;
}

export interface RoleTemplatesResponseDto {
  templates: unknown;
}

export interface EnginesResponseDto {
  engines: unknown;
}

export interface EngineIdRequestDto {
  engineId: string;
}

export interface AgentsResponseDto {
  agents: unknown;
}

export interface AgentTemplateIdRequestDto {
  templateId: string;
}

export interface FetchModelsResponseDto {
  success: boolean;
  target: string;
  models: string[];
  requestedAt: number;
}

export interface FetchModelsRequestDto {
  config: Record<string, unknown>;
  target: string;
}

export interface SessionStatsTotalsDto {
  llmCallCount: number;
  assignmentCount: number;
  turnCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
  successCount: number;
  failureCount: number;
}

export interface SessionStatsItemDto {
  templateId: string;
  engineId: string;
  bindingRevision: number;
  role: 'worker' | 'orchestrator' | 'auxiliary';
  displayName: string;
  provider?: string | null;
  declaredModelSpec?: string | null;
  resolvedModel?: string | null;
  modelIdentityKey?: string | null;
  llmCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
}

export interface SessionStatsModelDto {
  modelIdentityKey: string;
  provider: string;
  declaredModelSpec: string;
  resolvedModel: string;
  baseUrlFingerprint: string;
  reasoningEffort?: 'low' | 'medium' | 'high' | 'xhigh' | null;
  totals: SessionStatsTotalsDto;
}

export interface SessionStatsSessionDto {
  sessionId: string;
  version: number;
  updatedAt: number;
  totals: SessionStatsTotalsDto;
}

export interface SessionStatsResponseDto {
  scope: 'session' | 'workspace';
  workspaceId: string;
  sessionId?: string | null;
  version: number;
  lastAppliedLedgerSeq?: number;
  updatedAt: number;
  totals: SessionStatsTotalsDto;
  items: SessionStatsItemDto[];
  models?: SessionStatsModelDto[];
  sessions?: SessionStatsSessionDto[];
}

export interface ResetStatsResponseDto {
  reset: boolean;
}

export interface SavedResponseDto {
  saved: boolean;
}

// ─── Knowledge endpoints ────────────────────────────────────────────

export interface KnowledgeMutationResponseDto {
  success: boolean;
  knowledgeCount: number;
}

export interface KnowledgeItemDto {
  id: string;
  kind: 'adr' | 'faq' | 'learning';
  title: string;
  content: string;
  context: string | null;
  tags: string[];
  createdAt: number;
  updatedAt: number;
}

// ─── MCP / Skills / Repos endpoints ─────────────────────────────────

export interface McpServersResponseDto {
  servers: unknown;
}

export interface McpServerIdRequestDto {
  serverId: string;
}

export interface McpToolsResponseDto {
  tools: unknown[];
}

export interface McpConnectResponseDto {
  connected: boolean;
}

export interface McpDisconnectResponseDto {
  disconnected: boolean;
}

export interface RepositoriesResponseDto {
  repositories: unknown;
}

export interface RepositoryIdRequestDto {
  repositoryId: string;
}

export interface RepositoryRefreshResponseDto {
  refreshed: boolean;
}

export interface SkillsLibraryResponseDto {
  skills: unknown;
  failedRepositories?: unknown;
}

export interface SkillInstallResponseDto {
  installed: boolean;
}

export interface SkillsConfigSaveResponseDto {
  saved: boolean;
}

export interface SkillUpdateResponseDto {
  updated: boolean;
}

export interface AddedResponseDto {
  added: boolean;
}

export interface UpdatedResponseDto {
  updated: boolean;
}

export interface DeletedResponseDto {
  deleted: boolean;
}

export interface RemovedResponseDto {
  removed: boolean;
}

// ─── Changes / Files / Tunnel endpoints ─────────────────────────────

export interface DiffResponseDto {
  diff: string;
  filePath?: string | null;
}

export interface ApproveChangeRequestDto {
  filePath: string;
}

export interface ApproveChangeResponseDto {
  approved: boolean;
  filePath: string;
}

export interface RevertChangeRequestDto {
  filePath: string;
}

export interface RevertChangeResponseDto {
  reverted: boolean;
  filePath: string;
}

export interface ApproveAllChangesResponseDto {
  approved: boolean;
}

export interface RevertAllChangesResponseDto {
  reverted: boolean;
}

export interface RevertExecutionGroupChangesRequestDto {
  executionGroupId: string;
}

export interface RevertExecutionGroupChangesResponseDto {
  reverted: boolean;
  executionGroupId: string;
}

export interface FileContentResponseDto {
  content: string;
  filePath?: string | null;
}

export interface FilesystemEntryDto {
  name: string;
  path: string;
  isDirectory: boolean;
}

export interface FilesystemListResponseDto {
  entries: FilesystemEntryDto[];
}

export interface EnhancePromptRequestDto {
  prompt: string;
}

export interface EnhancePromptResponseDto {
  enhancedPrompt: string;
}

export interface MessagesResponseDto {
  generatedAt: number;
  currentSession?: SessionDto | null;
  sessions: SessionDto[];
  timeline: TimelineEntryDto[];
  canonicalTurns?: unknown[];
  notifications: NotificationDto[];
  sessionId: string;
  hasMoreBefore: boolean;
  beforeCursor?: string | null;
}

// ─── Task Projection types (magi-core::task) ────────────────────────

export type TaskKind =
  | 'local_agent'
  | 'local_workflow'
  | 'remote_agent'
  | 'monitor_mcp'
  | 'in_process_teammate'
  | 'dream';

export type TaskStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'failed'
  | 'killed';

export interface ExecutorBindingDto {
  target_role: string;
  capability_requirements: string[];
  parallelism_group?: string | null;
  exclusive_scope?: string | null;
  worker_selector?: string | null;
}

export interface TaskPolicyDto {
  autonomy_level: string;
  approval_mode: string;
  allowed_tools: string[];
  denied_tools: string[];
  allowed_paths: string[];
  denied_paths: string[];
  network_mode: string;
  command_mode: string;
  retry_limit: number;
  validation_profile?: string | null;
  checkpoint_mode: string;
  task_tier: 'execution_chain' | 'long_mission';
  background_allowed: boolean;
  escalation_conditions: string[];
}

export interface TaskDto {
  task_id: string;
  mission_id: string;
  root_task_id: string;
  parent_task_id?: string | null;
  kind: TaskKind;
  title: string;
  goal: string;
  status: TaskStatus;
  dependency_ids: string[];
  required_children: string[];
  policy_snapshot?: TaskPolicyDto | null;
  executor_binding?: ExecutorBindingDto | null;
  knowledge_refs: string[];
  workspace_scope?: string | null;
  write_scope?: string | null;
  input_refs: string[];
  output_refs: string[];
  evidence_refs: string[];
  retry_count: number;
  created_at: number;
  updated_at: number;
}

export interface ProgressSummaryDto {
  total_tasks: number;
  pending_tasks: number;
  running_tasks: number;
  completed_tasks: number;
  failed_tasks: number;
  killed_tasks: number;
  settled_tasks: number;
}

export type TaskExecutionModeDto = 'session_turn' | 'execution_chain' | 'long_mission';

export interface TaskProjectionDto {
  root_task: TaskDto;
  tasks: TaskDto[];
  running_tasks: string[];
  pending_tasks: string[];
  completed_tasks: string[];
  failed_tasks: string[];
  killed_tasks: string[];
  progress_summary: ProgressSummaryDto;
  aggregate_status: TaskStatus;
  display_status: string;
  execution_mode: TaskExecutionModeDto;
  runner_status: 'pending' | 'idle' | 'running' | 'completed' | 'error' | 'killed';
  has_recoverable_chain: boolean;
  recoverable_branch_count: number;
}

export interface SessionTaskHistoryItemDto {
  rootTask: TaskDto;
  runnerStatus: 'pending' | 'idle' | 'running' | 'completed' | 'error' | 'killed';
  displayStatus: string;
  executionMode: TaskExecutionModeDto;
  active: boolean;
  archived: boolean;
  restartable: boolean;
  updatedAt: number;
}

export interface SessionTaskHistoryResponseDto {
  sessionId: string;
  items: SessionTaskHistoryItemDto[];
}

export interface DeliveryPackageProgressDto {
  total: number;
  completed: number;
  failed: number;
  running: number;
  pending: number;
  killed: number;
}

export interface DeliveryPackageVerificationResultDto {
  task_id: string;
  title: string;
  result: string;
  evidence: string[];
}

export interface DeliveryPackageExecutionRecordDto {
  task_id: string;
  title: string;
  goal: string;
  evidence: string[];
}

export interface DeliveryPackageDto {
  goal: string;
  scope?: string | null;
  execution_mode: TaskExecutionModeDto;
  aggregate_status: string;
  progress: DeliveryPackageProgressDto;
  file_changes: string[];
  evidence_list: string[];
  verification_results: DeliveryPackageVerificationResultDto[];
  execution_records: DeliveryPackageExecutionRecordDto[];
  remaining_risks: string[];
  completed_task_count: number;
}
