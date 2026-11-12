import { normalizeContextUsageBreakdown, type ContextUsageBreakdown } from '../../lib/context-usage-breakdown';
import type { SessionBootstrapSnapshot } from '../session-bootstrap';
import type { CanonicalTurn } from '../protocol/canonical-turn';
import { normalizeCanonicalTurnStrict } from '../protocol/canonical-turn';
import { deriveMainlineRuntimeState, deriveProcessingStateFromCanonicalTurns } from '../protocol/canonical-processing';
import { deriveHasUnreadCompletion } from '../../lib/session-activity-indicator';
import type {
  AppState,
  OrchestratorRuntimeState,
  Session,
  IncidentNotificationRecord,
} from '../../types/message';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';
import type { SessionContinuation } from '../rust-backend-types';

export type BootstrapPayload = SessionBootstrapSnapshot & {
  scope: 'personal' | 'workspace';
  agent?: {
    runtimeEpoch?: string;
  };
  canonicalTurns?: CanonicalTurn[];
  eventStreamNextSequence?: number;
  canonicalEventNextSequence?: number;
  sessions: Session[];
  state: AppState;
  workspace: {
    workspaceId: string;
    name: string;
    rootPath: string;
  };
};

export interface RustEventEnvelope {
  event_id?: string;
  event_type?: string;
  category?: string;
  occurred_at?: number;
  sequence?: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  payload?: Record<string, unknown> | null;
}

interface RustBootstrapSessionRecord {
  sessionId?: string;
  workspaceId?: string | null;
  title?: string | null;
  createdAt?: number;
  updatedAt?: number;
  messageCount?: number;
  lastCompletedAt?: number;
  lastViewedAt?: number;
}

interface RustBootstrapWorkspaceRecord {
  workspaceId?: string;
  rootPath?: string;
}

interface RustNotificationRecord {
  notificationId?: string;
  scope?: string;
  workspaceId?: string | null;
  sessionId?: string | null;
  kind?: string;
  level?: string | null;
  title?: string | null;
  message?: string;
  detail?: string | null;
  errorCode?: string | null;
  failureStage?: string | null;
  taskId?: string | null;
  requestId?: string | null;
  source?: string | null;
  createdAt?: number;
  handled?: boolean;
  actionRequired?: boolean;
  countUnread?: boolean;
  occurrenceCount?: number;
  resolved?: boolean;
}

interface RustTimelineEntry {
  entryId?: string;
  sessionId?: string;
  kind?: string;
  message?: string;
  occurredAt?: number;
}

interface RustExecutionGroupRuntimeSummary {
  mission_id?: string;
  latest_event_type?: string | null;
  current_status?: string | null;
  failed_dispatch_count?: number;
  active_task_ids?: string[];
  context_used_turn_count?: number;
  context_used_knowledge_count?: number;
  context_used_memory_count?: number;
  context_used_shared_item_count?: number;
  context_used_file_summary_count?: number;
  context_recent_turn_resolved_count?: number;
  context_recent_turn_retained_count?: number;
  context_recent_turn_session_source_count?: number;
  context_recent_turn_project_source_count?: number;
  context_recent_turn_provided_source_count?: number;
  context_truncation_count?: number;
  context_truncation_parts?: string[];
  context_knowledge_ids?: string[];
  context_knowledge_source_paths?: string[];
  context_memory_ids?: string[];
  context_memory_extraction_refs?: string[];
  context_shared_context_ids?: string[];
  context_file_summary_paths?: string[];
  context_code_index_knowledge_count?: number;
  context_audited_knowledge_count?: number;
  context_governed_knowledge_count?: number;
  context_extracted_memory_count?: number;
  context_provenance_linked_memory_count?: number;
}

interface RustTaskRuntimeSummary {
  task_id?: string;
  title?: string | null;
  failure_detail?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  latest_event_type?: string | null;
  current_status?: string | null;
  failed_dispatch_count?: number;
}

interface RustWorkerRuntimeSummary {
  worker_id?: string;
  tool_call_count?: number;
  failed_dispatch_count?: number;
  current_task_id?: string | null;
  latest_event_type?: string | null;
  current_status?: string | null;
  current_stage?: string | null;
}

interface RustSessionRuntimeSummary {
  session_id?: string;
  active_execution_group_ids?: string[];
  active_task_ids?: string[];
  recovery_ids?: string[];
  latest_event_type?: string | null;
  current_status?: string | null;
  last_update?: number | null;
  mission_id?: string | null;
  root_task_id?: string | null;
  recovery_ref?: string | null;
  continuation?: SessionContinuation;
  active_branches?: RustSessionRuntimeBranchSummary[];
  usage_observation?: RustSessionRuntimeUsageObservation | null;
  budget?: RustSessionRuntimeBudget | null;
  context_compaction?: RustSessionRuntimeContextCompaction | null;
}

interface RustSessionRuntimeUsageObservation {
  provider_context_tokens?: number | null;
  projected_request_tokens?: number;
  context_window_limit_tokens?: number | null;
  model_provider?: string | null;
  binding_revision?: number | null;
  response_reserve_tokens?: number | null;
  recovery_buffer_tokens?: number | null;
  proactive_threshold_tokens?: number | null;
  hard_request_limit_tokens?: number | null;
  pressure_level?: string | null;
  checkpoint_generation?: number | null;
  resolved_model?: string | null;
  observed_at?: number | null;
  measurement?: string | null;
  phase?: string | null;
  turn_id?: string | null;
  call_id?: string | null;
  context_breakdown?: ContextUsageBreakdown | null;
}

interface RustSessionRuntimeBudget {
  token_used?: number;
  remaining_tokens?: number;
  token_limit?: number;
  percent_remaining?: number;
  usage_ratio?: number;
  warning_level?: string;
}

interface RustSessionRuntimeContextCompaction {
  reason?: string;
  phase?: string | null;
  original_message_count?: number;
  compacted_message_count?: number;
  original_token_estimate?: number;
  compacted_token_estimate?: number;
  context_window_tokens?: number | null;
  threshold_tokens?: number | null;
  resolved_model?: string | null;
  compacted_at?: number | null;
}

interface RustSessionRuntimeBranchSummary {
  task_id?: string;
  worker_id?: string;
  status?: string;
  stage?: string;
  lease_id?: string | null;
  is_primary?: boolean;
}

/** 后端 RecoveryDiagnosticSummaryEntry 的 TS 映射 */
interface RustRecoveryDiagnosticSummary {
  recovery_id?: string;
  event_count?: number;
  latest_stage?: string;
  latest_event_type?: string;
  latest_sequence?: number;
  latest_occurred_at?: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  execution_chain_ref?: string | null;
  diagnostic_summary?: string | null;
  current_status?: string;
}

/** 后端 RuntimeDiagnosticSummary 的 TS 映射 */
interface RustRuntimeDiagnosticSummary {
  running_execution_group_count?: number;
  failed_execution_group_count?: number;
  running_task_count?: number;
  failed_task_count?: number;
  running_assignment_count?: number;
  failed_assignment_count?: number;
  active_worker_count?: number;
  failed_worker_count?: number;
  blocked_tool_count?: number;
  failed_tool_count?: number;
  governance_total_count?: number;
  governance_allowed_count?: number;
  governance_needs_approval_count?: number;
  governance_blocked_count?: number;
  governance_rejected_count?: number;
  pending_recovery_count?: number;
  resumed_recovery_count?: number;
}

/** 后端 EventCategoryCounts 的 TS 映射 */
interface RustEventCategoryCounts {
  domain?: number;
  audit?: number;
  usage?: number;
  projection?: number;
  system?: number;
}

interface RustRuntimeReadModelDto {
  meta?: {
    latest_sequence?: number;
  };
  details?: {
    execution_groups?: RustExecutionGroupRuntimeSummary[];
    tasks?: RustTaskRuntimeSummary[];
    workers?: RustWorkerRuntimeSummary[];
    sessions?: RustSessionRuntimeSummary[];
  };
  overview?: {
    category_counts?: RustEventCategoryCounts;
    activity?: {
      active_execution_group_ids?: string[];
      active_task_ids?: string[];
    };
    diagnostics?: RustRuntimeDiagnosticSummary;
  };
  operations?: {
    attention?: {
      failed_execution_group_ids?: string[];
      failed_task_ids?: string[];
      failed_assignment_ids?: string[];
      failed_worker_ids?: string[];
      blocked_tool_names?: string[];
      pending_recovery_ids?: string[];
    };
    work_queues?: {
      running_execution_group_ids?: string[];
      running_task_ids?: string[];
      running_assignment_ids?: string[];
      active_worker_ids?: string[];
      pending_recovery_ids?: string[];
    };
    dispatch?: {
      total_dispatches?: number;
      resume_dispatches?: number;
      latest_dispatch_reason?: string | null;
      active_assignment_ids?: string[];
    };
    resume_observation?: {
      total_recoveries?: number;
      resume_command_count?: number;
      resume_dispatch_count?: number;
      mission_resumed_count?: number;
      worker_resumed_count?: number;
    };
  };
  recovery?: {
    active_recovery_ids?: string[];
    summaries?: RustRecoveryDiagnosticSummary[];
  };
}

interface RustBootstrapDto {
  scope?: 'personal' | 'workspace';
  workspaceId?: string | null;
  workspacePath?: string | null;
  generatedAt?: number;
  currentSession?: RustBootstrapSessionRecord | null;
  sessions?: RustBootstrapSessionRecord[];
  timeline?: RustTimelineEntry[];
  canonicalTurns?: unknown[];
  workspaces?: RustBootstrapWorkspaceRecord[];
  runtimeReadModel?: RustRuntimeReadModelDto;
  notifications?: RustNotificationRecord[];
  eventStreamNextSequence?: number;
  canonicalEventNextSequence?: number;
  recentEvents?: RustEventEnvelope[];
  hasMoreBefore?: boolean;
  beforeCursor?: string | null;
  canonicalHasMoreBefore?: boolean;
  canonicalBeforeCursor?: string | null;
  agent?: {
    runtimeEpoch?: string;
  };
}

interface RustCanonicalHistoryPageDto {
  sessionId?: string;
  canonicalHasMoreBefore?: boolean;
  canonicalBeforeCursor?: string | null;
}

function normalizeString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

/** 只接受 daemon 约定的继续方式；其它取值一律视为不可继续，不在客户端猜测。 */
function normalizeSessionContinuation(value: unknown): SessionContinuation | undefined {
  return value === 'resume_chain' || value === 'resume_turn' ? value : undefined;
}

function normalizeNumber(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.floor(value) : fallback;
}

function normalizeStringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.map((item) => normalizeString(item)).filter((item) => item.length > 0)
    : [];
}

function normalizeObjectRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function getRuntimeDetailEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  key: string,
): unknown[] {
  const details = normalizeObjectRecord(runtimeReadModel?.details);
  const entries = details?.[key];
  return Array.isArray(entries) ? entries : [];
}


function normalizeSessionRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustSessionRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'sessions')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      session_id: normalizeString(entry.session_id) || undefined,
      active_execution_group_ids: normalizeStringArray(entry.active_execution_group_ids),
      active_task_ids: normalizeStringArray(entry.active_task_ids),
      recovery_ids: normalizeStringArray(entry.recovery_ids),
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      last_update: typeof entry.last_update === 'number' ? Math.floor(entry.last_update) : undefined,
      mission_id: normalizeString(entry.mission_id) || undefined,
      root_task_id: normalizeString(entry.root_task_id) || undefined,
      recovery_ref: normalizeString(entry.recovery_ref) || undefined,
      continuation: normalizeSessionContinuation(entry.continuation),
      active_branches: Array.isArray(entry.active_branches)
        ? entry.active_branches
          .map((branch) => normalizeObjectRecord(branch))
          .filter((branch): branch is Record<string, unknown> => branch !== null)
          .map((branch) => ({
            task_id: normalizeString(branch.task_id) || undefined,
            worker_id: normalizeString(branch.worker_id) || undefined,
            status: normalizeString(branch.status) || undefined,
            stage: normalizeString(branch.stage) || undefined,
            lease_id: normalizeString(branch.lease_id) || undefined,
            is_primary: branch.is_primary === true,
          }))
        : [],
      usage_observation: normalizeRuntimeUsageObservation(entry.usage_observation),
      budget: normalizeRuntimeBudget(entry.budget),
      context_compaction: normalizeRuntimeContextCompaction(entry.context_compaction),
    }));
}

function normalizeRuntimeUsageObservation(raw: unknown): RustSessionRuntimeUsageObservation | undefined {
  const record = normalizeObjectRecord(raw);
  if (!record) return undefined;
  return {
    provider_context_tokens: typeof record.provider_context_tokens === 'number'
      ? Math.floor(record.provider_context_tokens)
      : undefined,
    projected_request_tokens: typeof record.projected_request_tokens === 'number'
      ? Math.floor(record.projected_request_tokens)
      : undefined,
    context_window_limit_tokens: typeof record.context_window_limit_tokens === 'number'
      ? Math.floor(record.context_window_limit_tokens)
      : undefined,
    model_provider: normalizeString(record.model_provider) || undefined,
    binding_revision: typeof record.binding_revision === 'number'
      ? Math.floor(record.binding_revision)
      : undefined,
    response_reserve_tokens: typeof record.response_reserve_tokens === 'number'
      ? Math.floor(record.response_reserve_tokens)
      : undefined,
    recovery_buffer_tokens: typeof record.recovery_buffer_tokens === 'number'
      ? Math.floor(record.recovery_buffer_tokens)
      : undefined,
    proactive_threshold_tokens: typeof record.proactive_threshold_tokens === 'number'
      ? Math.floor(record.proactive_threshold_tokens)
      : undefined,
    hard_request_limit_tokens: typeof record.hard_request_limit_tokens === 'number'
      ? Math.floor(record.hard_request_limit_tokens)
      : undefined,
    pressure_level: normalizeString(record.pressure_level) || undefined,
    checkpoint_generation: typeof record.checkpoint_generation === 'number'
      ? Math.floor(record.checkpoint_generation)
      : undefined,
    resolved_model: normalizeString(record.resolved_model) || undefined,
    observed_at: typeof record.observed_at === 'number' ? Math.floor(record.observed_at) : undefined,
    measurement: normalizeString(record.measurement) || undefined,
    phase: normalizeString(record.phase) || undefined,
    turn_id: normalizeString(record.turn_id) || undefined,
    call_id: normalizeString(record.call_id) || undefined,
    context_breakdown: normalizeContextUsageBreakdown(record.context_breakdown),
  };
}

function normalizeRuntimeContextCompaction(raw: unknown): RustSessionRuntimeContextCompaction | undefined {
  const record = normalizeObjectRecord(raw);
  if (!record) {
    return undefined;
  }
  const reason = normalizeString(record.reason);
  if (!reason) {
    return undefined;
  }
  return {
    reason,
    phase: normalizeString(record.phase) || undefined,
    original_message_count: typeof record.original_message_count === 'number'
      ? Math.floor(record.original_message_count)
      : undefined,
    compacted_message_count: typeof record.compacted_message_count === 'number'
      ? Math.floor(record.compacted_message_count)
      : undefined,
    original_token_estimate: typeof record.original_token_estimate === 'number'
      ? Math.floor(record.original_token_estimate)
      : undefined,
    compacted_token_estimate: typeof record.compacted_token_estimate === 'number'
      ? Math.floor(record.compacted_token_estimate)
      : undefined,
    context_window_tokens: typeof record.context_window_tokens === 'number'
      ? Math.floor(record.context_window_tokens)
      : undefined,
    threshold_tokens: typeof record.threshold_tokens === 'number'
      ? Math.floor(record.threshold_tokens)
      : undefined,
    resolved_model: normalizeString(record.resolved_model) || undefined,
    compacted_at: typeof record.compacted_at === 'number' ? Math.floor(record.compacted_at) : undefined,
  };
}

function normalizeRuntimeBudget(raw: unknown): RustSessionRuntimeBudget | undefined {
  const record = normalizeObjectRecord(raw);
  if (!record) {
    return undefined;
  }
  const tokenLimit = typeof record.token_limit === 'number' ? Math.floor(record.token_limit) : undefined;
  if (tokenLimit === undefined) {
    return undefined;
  }
  return {
    token_used: typeof record.token_used === 'number' ? Math.floor(record.token_used) : undefined,
    remaining_tokens: typeof record.remaining_tokens === 'number'
      ? Math.floor(record.remaining_tokens)
      : undefined,
    token_limit: tokenLimit,
    percent_remaining: typeof record.percent_remaining === 'number'
      ? Math.floor(record.percent_remaining)
      : undefined,
    usage_ratio: typeof record.usage_ratio === 'number' ? record.usage_ratio : undefined,
    warning_level: normalizeString(record.warning_level) || undefined,
  };
}

function normalizeEventEnvelope(raw: unknown): RustEventEnvelope | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const eventId = normalizeString(record.event_id);
  const eventType = normalizeString(record.event_type);
  if (!eventId || !eventType) {
    return null;
  }
  return {
    event_id: eventId,
    event_type: eventType,
    category: normalizeString(record.category),
    occurred_at: typeof record.occurred_at === 'number' ? Math.floor(record.occurred_at) : undefined,
    sequence: typeof record.sequence === 'number' ? Math.floor(record.sequence) : undefined,
    workspace_id: normalizeString(record.workspace_id) || undefined,
    session_id: normalizeString(record.session_id) || undefined,
    mission_id: normalizeString(record.mission_id) || undefined,
    assignment_id: normalizeString(record.assignment_id) || undefined,
    task_id: normalizeString(record.task_id) || undefined,
    payload: record.payload && typeof record.payload === 'object' && !Array.isArray(record.payload)
      ? record.payload as Record<string, unknown>
      : null,
  };
}


function normalizeRustSessions(
  payload: RustBootstrapDto,
  generatedAt: number,
  workspaceId: string,
): Session[] {
  if (!Array.isArray(payload.sessions)) {
    return [];
  }
  const sessions: Session[] = [];
  for (const session of payload.sessions) {
    const sessionId = normalizeString(session.sessionId);
    if (!sessionId) {
      continue;
    }
    const sessionWorkspaceId = normalizeString(session.workspaceId);
    if (sessionWorkspaceId && workspaceId && sessionWorkspaceId !== workspaceId) {
      continue;
    }
    const createdAt = normalizeNumber(session.createdAt, generatedAt);
    const updatedAt = normalizeNumber(session.updatedAt, createdAt);
    const messageCount = normalizeNumber(session.messageCount, NaN);
    const lastCompletedAt = normalizeNumber(session.lastCompletedAt, NaN);
    const lastViewedAt = normalizeNumber(session.lastViewedAt, NaN);
    sessions.push({
      id: sessionId,
      ...(sessionWorkspaceId ? { workspaceId: sessionWorkspaceId } : {}),
      name: normalizeString(session.title) || undefined,
      createdAt,
      updatedAt,
      ...(Number.isFinite(messageCount) ? { messageCount } : {}),
      hasUnreadCompletion: deriveHasUnreadCompletion(
        Number.isFinite(lastCompletedAt) ? lastCompletedAt : undefined,
        Number.isFinite(lastViewedAt) ? lastViewedAt : undefined,
      ),
    });
  }
  return sessions;
}

function deriveBootstrapWorkspaceName(rootPath: string, workspaceId: string): string {
  const fallbackName = rootPath
    .split(/[\\/]/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .pop();
  return fallbackName || workspaceId || 'workspace';
}

function resolveSelectedWorkspace(
  payload: RustBootstrapDto,
  options: { workspaceId?: string; workspacePath?: string },
): { workspaceId: string; rootPath: string; name: string } {
  if (payload.scope === 'personal') {
    return {
      workspaceId: '',
      rootPath: '',
      name: '',
    };
  }
  const requestedWorkspaceId = normalizeString(options.workspaceId);
  const requestedWorkspacePath = normalizeString(options.workspacePath);
  const authoritativeWorkspaceId = normalizeString(payload.workspaceId);
  const authoritativeWorkspacePath = normalizeString(payload.workspacePath);
  const currentSessionWorkspaceId = normalizeString(payload.currentSession?.workspaceId);
  const workspaces = Array.isArray(payload.workspaces) ? payload.workspaces : [];
  const selectedWorkspace = workspaces.find((workspace) => (
    authoritativeWorkspaceId
    && normalizeString(workspace.workspaceId) === authoritativeWorkspaceId
  ))
    || workspaces.find((workspace) => (
      authoritativeWorkspacePath
      && normalizeString(workspace.rootPath) === authoritativeWorkspacePath
    ))
    || workspaces.find((workspace) => (
    requestedWorkspacePath && normalizeString(workspace.rootPath) === requestedWorkspacePath
  ))
    || workspaces.find((workspace) => (
      requestedWorkspaceId && normalizeString(workspace.workspaceId) === requestedWorkspaceId
    ))
    || workspaces.find((workspace) => (
      !requestedWorkspaceId
      && !requestedWorkspacePath
      && currentSessionWorkspaceId
      && normalizeString(workspace.workspaceId) === currentSessionWorkspaceId
    ))
    || null;
  const workspaceId = normalizeString(selectedWorkspace?.workspaceId)
    || authoritativeWorkspaceId
    || requestedWorkspaceId
    || currentSessionWorkspaceId;
  const rootPath = normalizeString(selectedWorkspace?.rootPath)
    || authoritativeWorkspacePath
    || requestedWorkspacePath;
  return {
    workspaceId,
    rootPath,
    name: deriveBootstrapWorkspaceName(rootPath, workspaceId),
  };
}

function resolveActiveSessionRuntime(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): RustSessionRuntimeSummary | undefined {
  return normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
}

function deriveProcessingState(
  sessionId: string,
  canonicalTurns: CanonicalTurn[],
): AppState['processingState'] {
  return deriveProcessingStateFromCanonicalTurns(canonicalTurns, sessionId);
}

type OrchestratorRuntimeSnapshotState = NonNullable<OrchestratorRuntimeState['runtimeSnapshot']>;
type BudgetWarningLevel = NonNullable<NonNullable<OrchestratorRuntimeSnapshotState['budgetState']>['warningLevel']>;

function normalizeBudgetWarningLevel(value: string | undefined): BudgetWarningLevel | undefined {
  switch (value) {
    case 'normal':
    case 'notice':
    case 'warning':
    case 'danger':
      return value;
    case 'compaction_due':
      return 'danger';
    default:
      return undefined;
  }
}

/**
 * 从后端 session.budget 装配上下文预算快照。
 *
 * 后端 usage-authority 已在 DTO 层算好 token 占用、剩余与告警分级，这里只做
 * 字段映射；没有 budget 的会话返回 null，展示层保留参数名并将值显示为未知。
 */
function buildRuntimeSnapshot(
  activeSession: RustSessionRuntimeSummary | undefined,
): OrchestratorRuntimeSnapshotState | null {
  const budget = activeSession?.budget;
  if (!budget || typeof budget.token_limit !== 'number') {
    return null;
  }
  const compaction = activeSession?.context_compaction;
  const usageObservation = activeSession?.usage_observation;
  const measurement = usageObservation?.measurement === 'estimated'
    || usageObservation?.measurement === 'authoritative'
    ? usageObservation.measurement
    : undefined;
  return {
    budgetState: {
      tokenUsed: budget.token_used,
      remainingTokens: budget.remaining_tokens,
      tokenLimit: budget.token_limit,
      usageRatio: budget.usage_ratio,
      warningLevel: normalizeBudgetWarningLevel(budget.warning_level),
      ...(usageObservation?.projected_request_tokens != null || budget.token_used != null
        ? { projectedRequestTokens: usageObservation?.projected_request_tokens ?? budget.token_used }
        : {}),
      ...(usageObservation?.provider_context_tokens != null
        ? { providerContextTokens: usageObservation.provider_context_tokens }
        : {}),
      ...(usageObservation?.pressure_level ? { pressureLevel: usageObservation.pressure_level } : {}),
      ...(usageObservation?.proactive_threshold_tokens != null
        ? { proactiveThresholdTokens: usageObservation.proactive_threshold_tokens }
        : {}),
      ...(usageObservation?.hard_request_limit_tokens != null
        ? { hardRequestLimitTokens: usageObservation.hard_request_limit_tokens }
        : {}),
      ...(usageObservation?.response_reserve_tokens != null
        ? { responseReserveTokens: usageObservation.response_reserve_tokens }
        : {}),
      ...(usageObservation?.recovery_buffer_tokens != null
        ? { recoveryBufferTokens: usageObservation.recovery_buffer_tokens }
        : {}),
      ...(usageObservation?.context_breakdown ? { contextBreakdown: usageObservation.context_breakdown } : {}),
      ...(compaction?.compacted_at != null ? { lastCompactionAt: compaction.compacted_at } : {}),
      ...(compaction?.reason ? { lastCompactionReason: compaction.reason } : {}),
      ...(compaction?.original_token_estimate != null
        ? { originalTokenEstimate: compaction.original_token_estimate }
        : {}),
      ...(compaction?.compacted_token_estimate != null
        ? { compactedTokenEstimate: compaction.compacted_token_estimate }
        : {}),
      ...(compaction?.original_message_count != null
        ? { originalMessageCount: compaction.original_message_count }
        : {}),
      ...(compaction?.compacted_message_count != null
        ? { compactedMessageCount: compaction.compacted_message_count }
        : {}),
      ...(measurement ? { measurement } : {}),
      ...(usageObservation?.phase ? { phase: usageObservation.phase } : {}),
      ...(usageObservation?.observed_at != null
        ? { updatedAt: usageObservation.observed_at }
        : {}),
      ...(usageObservation?.turn_id ? { turnId: usageObservation.turn_id } : {}),
      ...(usageObservation?.call_id ? { callId: usageObservation.call_id } : {}),
      ...(usageObservation?.resolved_model
        ? { resolvedModel: usageObservation.resolved_model }
        : {}),
    },
  };
}

function deriveRuntimeState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  canonicalTurns: CanonicalTurn[],
  sessionId: string,
  generatedAt: number,
): OrchestratorRuntimeState | null {
  const mainline = deriveMainlineRuntimeState(canonicalTurns, sessionId);
  if (!runtimeReadModel) {
    return mainline ? { sessionId, ...mainline } : null;
  }

  const activeSession = resolveActiveSessionRuntime(runtimeReadModel, sessionId);

  return {
    sessionId: sessionId || undefined,
    ...(mainline ?? {
      status: 'idle',
      phase: 'idle',
      statusChangedAt: generatedAt,
      lastEventAt: generatedAt,
    }),
    ...(mainline && activeSession?.continuation ? { continuation: activeSession.continuation } : {}),
    runtimeSnapshot: buildRuntimeSnapshot(activeSession),
  };
}

function normalizeNotifications(
  notifications: RustNotificationRecord[] | undefined,
): IncidentNotificationRecord[] {
  if (!Array.isArray(notifications)) {
    return [];
  }
  const normalized: IncidentNotificationRecord[] = [];
  for (const notification of notifications) {
    const notificationId = normalizeString(notification.notificationId);
    const message = normalizeString(notification.message);
    const scope = normalizeString(notification.scope);
    if (!notificationId || !message || notification.kind !== 'incident') {
      continue;
    }
    if (scope !== 'app' && scope !== 'workspace' && scope !== 'session') {
      continue;
    }
    normalized.push({
      notificationId,
      kind: 'incident',
      scope,
      level: normalizeString(notification.level) || 'error',
      title: normalizeString(notification.title) || undefined,
      message,
      detail: normalizeString(notification.detail) || undefined,
      errorCode: normalizeString(notification.errorCode) || undefined,
      failureStage: normalizeString(notification.failureStage) || undefined,
      taskId: normalizeString(notification.taskId) || undefined,
      requestId: normalizeString(notification.requestId) || undefined,
      source: normalizeString(notification.source) || undefined,
      workspaceId: normalizeString(notification.workspaceId) || undefined,
      sessionId: normalizeString(notification.sessionId) || undefined,
      createdAt: normalizeNumber(notification.createdAt, Date.now()),
      read: Boolean(notification.handled),
      handled: Boolean(notification.handled),
      resolved: Boolean(notification.resolved),
      actionRequired: notification.actionRequired !== false,
      countUnread: notification.countUnread !== false,
      occurrenceCount: Math.max(1, normalizeNumber(notification.occurrenceCount, 1)),
    });
  }
  return normalized;
}

export function parseRustEventEnvelope(rawData: string): RustEventEnvelope | null {
  try {
    return normalizeEventEnvelope(JSON.parse(rawData));
  } catch {
    return null;
  }
}

export function normalizeRustBootstrapPayload(
  rawPayload: unknown,
  options: { workspaceId?: string; workspacePath?: string; sessionId?: string } = {},
): BootstrapPayload {
  const payload = (rawPayload ?? {}) as RustBootstrapDto;
  const generatedAt = normalizeNumber(payload.generatedAt, Date.now());
  const workspace = resolveSelectedWorkspace(payload, options);
  const scope = payload.scope === 'workspace' || payload.scope === 'personal'
    ? payload.scope
    : (workspace.workspaceId || workspace.rootPath ? 'workspace' : 'personal');
  const sessions = normalizeRustSessions(payload, generatedAt, workspace.workspaceId);
  const selectedSessionId = normalizeString(payload.currentSession?.sessionId);
  const currentSession = selectedSessionId
    ? sessions.find((session) => session.id === selectedSessionId)
    : undefined;
  const rawCanonicalTurns = Array.isArray(payload.canonicalTurns)
    ? payload.canonicalTurns
    : [];
  const canonicalTurns = rawCanonicalTurns.map((turn, index) => (
    normalizeCanonicalTurnStrict(turn, `bootstrap.canonicalTurns[${index}]`)
  ));
  const processingState = deriveProcessingState(
    currentSession?.id || '',
    canonicalTurns,
  );
  const state: AppState = {
    ...buildEmptyWorkspaceAppState(generatedAt),
    sessions,
    currentSession,
    currentSessionId: currentSession?.id || '',
    currentWorkspaceId: workspace.workspaceId,
    currentWorkspacePath: workspace.rootPath,
    isProcessing: Boolean(processingState?.isProcessing),
    processingState,
    stateUpdatedAt: generatedAt,
  };
  const normalizedNotifications = normalizeNotifications(payload.notifications);
  const canonicalEventNextSequence = normalizeNumber(payload.canonicalEventNextSequence, 0);

  return {
    scope,
    agent: {
      runtimeEpoch: normalizeString(payload.agent?.runtimeEpoch)
        || (currentSession?.id && workspace.workspaceId ? `${workspace.workspaceId}:${currentSession.id}` : String(generatedAt)),
    },
    workspace,
    sessionId: currentSession?.id || '',
    sessions,
    state,
    canonicalTurns,
    eventStreamNextSequence: normalizeNumber(payload.eventStreamNextSequence, 0),
    ...(canonicalEventNextSequence >= 1 ? { canonicalEventNextSequence } : {}),
    notifications: currentSession?.id || workspace.workspaceId
        ? {
          workspaceId: workspace.workspaceId || null,
          sessionId: currentSession?.id || undefined,
          notifications: {
            lastUpdatedAt: generatedAt,
            records: normalizedNotifications,
          },
        }
      : undefined,
    orchestratorRuntimeState: deriveRuntimeState(
      payload.runtimeReadModel,
      canonicalTurns,
      currentSession?.id || '',
      generatedAt,
    ),
  };
}

export function readRustCanonicalHistoryPageMeta(rawPayload: unknown): {
  sessionId: string;
  canonicalHasMoreBefore: boolean;
  canonicalBeforeCursor: string | null;
} {
  const payload = (rawPayload ?? {}) as RustCanonicalHistoryPageDto;
  return {
    sessionId: normalizeString(payload.sessionId),
    canonicalHasMoreBefore: payload.canonicalHasMoreBefore === true,
    canonicalBeforeCursor: normalizeString(payload.canonicalBeforeCursor) || null,
  };
}
