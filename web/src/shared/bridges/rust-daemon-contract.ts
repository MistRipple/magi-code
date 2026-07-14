import type { SessionBootstrapSnapshot } from '../session-bootstrap';
import type { CanonicalTurn } from '../protocol/canonical-turn';
import { normalizeCanonicalTurn } from '../protocol/canonical-turn';
import { deriveProcessingStateFromCanonicalTurns } from '../protocol/canonical-processing';
import { deriveHasUnreadCompletion } from '../../lib/session-activity-indicator';
import type {
  AppState,
  OrchestrationRuntimeAssignmentSummary,
  OrchestrationRuntimeFailureRootCause,
  OrchestrationRuntimeOpsView,
  OrchestrationRuntimeRecoverySummary,
  OrchestrationRuntimeTimelineEntry,
  OrchestratorRuntimeState,
  Session,
  IncidentNotificationRecord,
  SubTaskItem,
} from '../../types/message';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';

export type BootstrapPayload = SessionBootstrapSnapshot & {
  agent?: {
    runtimeEpoch?: string;
  };
  canonicalTurns?: CanonicalTurn[];
  eventStreamNextSequence?: number;
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

interface RustAssignmentRuntimeSummary {
  assignment_id?: string;
  mission_id?: string | null;
  completed_task_count?: number;
  failed_task_count?: number;
  task_ids?: string[];
  latest_event_type?: string | null;
  current_status?: string | null;
}

interface RustTaskRuntimeSummary {
  task_id?: string;
  title?: string | null;
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
  root_task_status?: string | null;
  execution_chain_ref?: string | null;
  recovery_ref?: string | null;
  has_recoverable_chain?: boolean;
  recoverable_branch_count?: number;
  active_branches?: RustSessionRuntimeBranchSummary[];
  current_turn?: RustSessionRuntimeTurnSummary | null;
  turn_items?: RustSessionRuntimeTurnItemSummary[];
  budget?: RustSessionRuntimeBudget | null;
  context_compaction?: RustSessionRuntimeContextCompaction | null;
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
  token_limit?: number | null;
  threshold_tokens?: number | null;
  resolved_model?: string | null;
  compacted_at?: number | null;
}

interface RustSessionRuntimeTurnSummary {
  turn_id?: string;
  turn_seq?: number;
  accepted_at?: number | null;
  completed_at?: number | null;
  response_duration_ms?: number | null;
  status?: string | null;
  user_message?: string | null;
}

interface RustSessionRuntimeTurnItemSummary {
  item_id?: string;
  item_seq?: number;
  kind?: string;
  status?: string;
  source?: string;
  title?: string | null;
  content?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  role_id?: string | null;
  tool_call_id?: string | null;
  tool_name?: string | null;
  tool_status?: string | null;
  tool_arguments?: unknown;
  tool_result?: string | null;
  tool_error?: string | null;
  request_id?: string | null;
  user_message_id?: string | null;
  placeholder_message_id?: string | null;
  timeline_entry_id?: string | null;
}

interface RustSessionRuntimeBranchSummary {
  task_id?: string;
  worker_id?: string;
  status?: string;
  stage?: string;
  lease_id?: string | null;
  execution_intent_ref?: string | null;
  binding_lifecycle?: string | null;
  last_checkpoint_at?: number | null;
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
    assignments?: RustAssignmentRuntimeSummary[];
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
  generatedAt?: number;
  currentSession?: RustBootstrapSessionRecord | null;
  sessions?: RustBootstrapSessionRecord[];
  timeline?: RustTimelineEntry[];
  canonicalTurns?: unknown[];
  pendingChanges?: unknown[];
  pendingChangesState?: unknown;
  workspaces?: RustBootstrapWorkspaceRecord[];
  runtimeReadModel?: RustRuntimeReadModelDto;
  notifications?: RustNotificationRecord[];
  eventStreamNextSequence?: number;
  recentEvents?: RustEventEnvelope[];
  agent?: {
    runtimeEpoch?: string;
  };
}

interface RustTimelinePageDto {
  sessionId?: string;
  hasMoreBefore?: boolean;
  beforeCursor?: string | null;
}

function normalizeString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
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

function normalizeAssignmentRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustAssignmentRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'assignments')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      assignment_id: normalizeString(entry.assignment_id) || undefined,
      mission_id: normalizeString(entry.mission_id) || undefined,
      completed_task_count: typeof entry.completed_task_count === 'number'
        ? Math.floor(entry.completed_task_count)
        : undefined,
      failed_task_count: typeof entry.failed_task_count === 'number'
        ? Math.floor(entry.failed_task_count)
        : undefined,
      task_ids: normalizeStringArray(entry.task_ids),
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
    }));
}

function normalizeTaskRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustTaskRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'tasks')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      task_id: normalizeString(entry.task_id) || undefined,
      title: normalizeString(entry.title) || undefined,
      mission_id: normalizeString(entry.mission_id) || undefined,
      assignment_id: normalizeString(entry.assignment_id) || undefined,
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      failed_dispatch_count: typeof entry.failed_dispatch_count === 'number'
        ? Math.floor(entry.failed_dispatch_count)
        : undefined,
    }));
}

function normalizeRuntimeTurnSummary(raw: unknown): RustSessionRuntimeTurnSummary | null {
  const turn = normalizeObjectRecord(raw);
  if (!turn) {
    return null;
  }
  return {
    turn_id: normalizeString(turn.turn_id) || undefined,
    turn_seq: typeof turn.turn_seq === 'number' ? Math.floor(turn.turn_seq) : undefined,
    accepted_at: typeof turn.accepted_at === 'number' ? Math.floor(turn.accepted_at) : undefined,
    completed_at: typeof turn.completed_at === 'number' ? Math.floor(turn.completed_at) : undefined,
    response_duration_ms: typeof turn.response_duration_ms === 'number'
      ? Math.floor(turn.response_duration_ms)
      : undefined,
    status: normalizeString(turn.status) || undefined,
    user_message: normalizeString(turn.user_message) || undefined,
  };
}

function normalizeRuntimeTurnItems(raw: unknown): RustSessionRuntimeTurnItemSummary[] {
  return Array.isArray(raw)
    ? raw
      .map((item) => normalizeObjectRecord(item))
      .filter((item): item is Record<string, unknown> => item !== null)
      .map((item) => item as RustSessionRuntimeTurnItemSummary)
    : [];
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
      root_task_status: normalizeString(entry.root_task_status) || undefined,
      execution_chain_ref: normalizeString(entry.execution_chain_ref) || undefined,
      recovery_ref: normalizeString(entry.recovery_ref) || undefined,
      has_recoverable_chain: entry.has_recoverable_chain === true,
      recoverable_branch_count: typeof entry.recoverable_branch_count === 'number'
        ? Math.floor(entry.recoverable_branch_count)
        : undefined,
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
            execution_intent_ref: normalizeString(branch.execution_intent_ref) || undefined,
            binding_lifecycle: normalizeString(branch.binding_lifecycle) || undefined,
            last_checkpoint_at: typeof branch.last_checkpoint_at === 'number'
              ? Math.floor(branch.last_checkpoint_at)
              : undefined,
            is_primary: branch.is_primary === true,
          }))
        : [],
      current_turn: normalizeRuntimeTurnSummary(entry.current_turn),
      turn_items: normalizeRuntimeTurnItems(entry.turn_items),
      budget: normalizeRuntimeBudget(entry.budget),
      context_compaction: normalizeRuntimeContextCompaction(entry.context_compaction),
    }));
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
    token_limit: typeof record.token_limit === 'number' ? Math.floor(record.token_limit) : undefined,
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

function resolveEventPayload(event: RustEventEnvelope): Record<string, unknown> {
  return event.payload || {};
}

function resolveEventMissionId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.mission_id) || normalizeString(payload.mission_id);
}

function resolveEventAssignmentId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.assignment_id) || normalizeString(payload.assignment_id);
}

function resolveEventTaskId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.task_id) || normalizeString(payload.task_id);
}

function resolveEventWorkerId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(payload.worker_id);
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
  const requestedWorkspaceId = normalizeString(options.workspaceId);
  const requestedWorkspacePath = normalizeString(options.workspacePath);
  const currentSessionWorkspaceId = normalizeString(payload.currentSession?.workspaceId);
  const workspaces = Array.isArray(payload.workspaces) ? payload.workspaces : [];
  const selectedWorkspace = workspaces.find((workspace) => (
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
    || workspaces[0]
    || null;
  const workspaceId = normalizeString(selectedWorkspace?.workspaceId)
    || requestedWorkspaceId
    || currentSessionWorkspaceId;
  const rootPath = normalizeString(selectedWorkspace?.rootPath)
    || requestedWorkspacePath;
  return {
    workspaceId,
    rootPath,
    name: deriveBootstrapWorkspaceName(rootPath, workspaceId),
  };
}

function buildLookupMaps(events: RustEventEnvelope[]): {
  missionTitles: Map<string, string>;
  assignmentTitles: Map<string, string>;
  taskTitles: Map<string, string>;
  assignmentWorkers: Map<string, string>;
} {
  const missionTitles = new Map<string, string>();
  const assignmentTitles = new Map<string, string>();
  const taskTitles = new Map<string, string>();
  const assignmentWorkers = new Map<string, string>();

  for (const event of events) {
    const payload = event.payload;
    if (!payload) {
      continue;
    }
    const missionId = resolveEventMissionId(event);
    const assignmentId = resolveEventAssignmentId(event);
    const taskId = resolveEventTaskId(event);
    const missionTitle = normalizeString(payload.mission_title);
    const assignmentTitle = normalizeString(payload.assignment_title);
    const taskTitle = normalizeString(payload.task_title);
    const workerId = resolveEventWorkerId(event);

    if (missionId && missionTitle && !missionTitles.has(missionId)) {
      missionTitles.set(missionId, missionTitle);
    }
    if (assignmentId && assignmentTitle && !assignmentTitles.has(assignmentId)) {
      assignmentTitles.set(assignmentId, assignmentTitle);
    }
    if (taskId && taskTitle && !taskTitles.has(taskId)) {
      taskTitles.set(taskId, taskTitle);
    }
    if (assignmentId && workerId && !assignmentWorkers.has(assignmentId)) {
      assignmentWorkers.set(assignmentId, workerId);
    }
  }

  return {
    missionTitles,
    assignmentTitles,
    taskTitles,
    assignmentWorkers,
  };
}

function normalizeSubTaskStatus(status: string, failedDispatchCount = 0): SubTaskItem['status'] {
  const normalized = status.toLowerCase();
  if (normalized.includes('approval')) {
    return 'awaiting_approval';
  }
  if (normalized.includes('review')) {
    return 'review_required';
  }
  if (normalized.includes('cancel') || normalized.includes('abort') || normalized.includes('kill')) {
    return 'cancelled';
  }
  if (normalized.includes('reject')) {
    return 'failed';
  }
  if (normalized.includes('block') || failedDispatchCount > 0) {
    return 'blocked';
  }
  if (normalized.includes('fail')) {
    return 'failed';
  }
  if (normalized.includes('success') || normalized.includes('complete')) {
    return 'completed';
  }
  if (normalized.includes('run') || normalized.includes('resume') || normalized.includes('execute')) {
    return 'running';
  }
  if (normalized.includes('pause')) {
    return 'paused';
  }
  if (normalized.includes('wait')) {
    return 'waiting_deps';
  }
  if (normalized.includes('skip')) {
    return 'skipped';
  }
  return 'pending';
}

function buildAssignmentsFromRuntime(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  events: RustEventEnvelope[],
  sessionId: string,
): OrchestrationRuntimeAssignmentSummary[] {
  const assignmentEntries = normalizeAssignmentRuntimeEntries(runtimeReadModel);
  const { activeSession, taskEntries, activeMissionIds } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
  const tasksByAssignment = new Map<string, RustTaskRuntimeSummary[]>();
  for (const task of taskEntries) {
    const assignmentId = normalizeString(task.assignment_id);
    if (!assignmentId) {
      continue;
    }
    const bucket = tasksByAssignment.get(assignmentId) || [];
    bucket.push(task);
    tasksByAssignment.set(assignmentId, bucket);
  }
  const lookups = buildLookupMaps(events);

  const assignments: OrchestrationRuntimeAssignmentSummary[] = [];
  for (const assignment of assignmentEntries) {
    const assignmentId = normalizeString(assignment.assignment_id);
    const missionId = normalizeString(assignment.mission_id);
    if (!assignmentId) {
      continue;
    }
    if (missionId && activeMissionIds.size > 0 && !activeMissionIds.has(missionId)) {
      continue;
    }
    const assignmentTasks = tasksByAssignment.get(assignmentId) || [];
    const assignmentTaskStatuses = assignmentTasks.map((task) => normalizeSubTaskStatus(
      normalizeString(task.current_status),
      typeof task.failed_dispatch_count === 'number' ? task.failed_dispatch_count : 0,
    ));
    const taskTotal = assignmentTasks.length || normalizeStringArray(assignment.task_ids).length;
    const completedTaskCount = assignmentTasks.length > 0
      ? assignmentTaskStatuses.filter((status) => status === 'completed' || status === 'skipped').length
      : normalizeNumber(assignment.completed_task_count, 0);
    const failedTaskCount = assignmentTasks.length > 0
      ? assignmentTaskStatuses.filter((status) => status === 'failed' || status === 'blocked').length
      : normalizeNumber(assignment.failed_task_count, 0);
    const runningTaskCount = assignmentTaskStatuses.filter((status) => status === 'running').length;
    const awaitingApprovalTaskCount = assignmentTaskStatuses.filter((status) => status === 'awaiting_approval').length;
    const reviewRequiredTaskCount = assignmentTaskStatuses.filter((status) => status === 'review_required').length;
    const blockedTaskCount = assignmentTaskStatuses.filter((status) => status === 'blocked').length;
    const progress = taskTotal > 0 ? Math.round((completedTaskCount / taskTotal) * 100) : 0;

    assignments.push({
      assignmentId,
      workerId: lookups.assignmentWorkers.get(assignmentId) || undefined,
      title: lookups.assignmentTitles.get(assignmentId) || '任务分配',
      status: normalizeString(assignment.current_status) || 'pending',
      progress,
      taskTotal,
      awaitingApprovalTaskCount,
      reviewRequiredTaskCount,
      blockedTaskCount,
      completedTaskCount,
      failedTaskCount,
      runningTaskCount,
    });
  }
  if (assignments.length > 0) {
    return assignments;
  }
  return buildBranchTaskTrackingSummaries(activeSession, taskEntries, lookups);
}

function taskTrackingProgressForStatus(status: SubTaskItem['status']): number {
  switch (status) {
    case 'completed':
    case 'skipped':
    case 'failed':
    case 'blocked':
    case 'cancelled':
      return 100;
    case 'running':
    case 'in_progress':
      return 50;
    default:
      return 0;
  }
}

function fallbackBranchTaskTitle(branch: RustSessionRuntimeBranchSummary, index: number): string {
  if (branch.is_primary) {
    return '主线任务';
  }
  return `代理任务 ${Math.max(1, index)}`;
}

function buildBranchTaskTrackingSummaries(
  activeSession: RustSessionRuntimeSummary | undefined,
  taskEntries: RustTaskRuntimeSummary[],
  lookups: ReturnType<typeof buildLookupMaps>,
): OrchestrationRuntimeAssignmentSummary[] {
  const branches = Array.isArray(activeSession?.active_branches) ? activeSession.active_branches : [];
  if (branches.length === 0) {
    return [];
  }
  const taskById = new Map<string, RustTaskRuntimeSummary>();
  for (const task of taskEntries) {
    const taskId = normalizeString(task.task_id);
    if (taskId && !taskById.has(taskId)) {
      taskById.set(taskId, task);
    }
  }

  const summaries: OrchestrationRuntimeAssignmentSummary[] = [];
  for (const [index, branch] of branches.entries()) {
    const taskId = normalizeString(branch.task_id);
    if (!taskId) {
      continue;
    }
    const task = taskById.get(taskId);
    const status = normalizeSubTaskStatus(normalizeString(task?.current_status || branch.status));
    const title = normalizeString(task?.title)
      || lookups.taskTitles.get(taskId)
      || fallbackBranchTaskTitle(branch, index);
    const completedTaskCount = status === 'completed' || status === 'skipped' ? 1 : 0;
    const failedTaskCount = status === 'failed' || status === 'blocked' ? 1 : 0;
    const runningTaskCount = status === 'running' || status === 'in_progress' ? 1 : 0;
    summaries.push({
      assignmentId: taskId,
      workerId: normalizeString(branch.worker_id) || undefined,
      title,
      status,
      progress: taskTrackingProgressForStatus(status),
      taskTotal: 1,
      awaitingApprovalTaskCount: status === 'awaiting_approval' ? 1 : 0,
      reviewRequiredTaskCount: status === 'review_required' ? 1 : 0,
      blockedTaskCount: status === 'blocked' ? 1 : 0,
      completedTaskCount,
      failedTaskCount,
      runningTaskCount,
    });
  }
  return summaries;
}

function resolveSessionTaskEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): {
  activeSession: RustSessionRuntimeSummary | undefined;
  taskEntries: RustTaskRuntimeSummary[];
  activeMissionIds: Set<string>;
} {
  const allTaskEntries = normalizeTaskRuntimeEntries(runtimeReadModel);
  const activeSession = normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
  const activeTaskIds = new Set(normalizeStringArray(activeSession?.active_task_ids));
  const sessionTaskIds = new Set(activeTaskIds);
  const rootTaskId = normalizeString(activeSession?.root_task_id);
  if (rootTaskId) {
    sessionTaskIds.add(rootTaskId);
  }
  for (const branch of activeSession?.active_branches || []) {
    const branchTaskId = normalizeString(branch.task_id);
    if (branchTaskId) {
      sessionTaskIds.add(branchTaskId);
    }
  }
  const activeMissionIds = new Set(normalizeStringArray(activeSession?.active_execution_group_ids));
  if (activeMissionIds.size === 0 && sessionTaskIds.size > 0) {
    for (const task of allTaskEntries) {
      const taskId = normalizeString(task.task_id);
      const missionId = normalizeString(task.mission_id);
      if (taskId && missionId && sessionTaskIds.has(taskId)) {
        activeMissionIds.add(missionId);
      }
    }
  }
  const taskEntries = allTaskEntries
    .filter((entry) => {
      const taskId = normalizeString(entry.task_id);
      const missionId = normalizeString(entry.mission_id);
      return sessionTaskIds.has(taskId) || (missionId && activeMissionIds.has(missionId));
    });
  return { activeSession, taskEntries, activeMissionIds };
}

function buildSessionTaskStatusSummary(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): {
  activeSession: RustSessionRuntimeSummary | undefined;
  taskEntries: RustTaskRuntimeSummary[];
  runningTaskIds: string[];
  failedTaskIds: string[];
} {
  const { activeSession, taskEntries } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
  const runningTaskIds: string[] = [];
  const failedTaskIds: string[] = [];

  for (const task of taskEntries) {
    const taskId = normalizeString(task.task_id);
    if (!taskId) {
      continue;
    }
    const status = normalizeSubTaskStatus(
      normalizeString(task.current_status),
      typeof task.failed_dispatch_count === 'number' ? task.failed_dispatch_count : 0,
    );
    if (status === 'running') {
      runningTaskIds.push(taskId);
      continue;
    }
    if (status === 'failed' || status === 'blocked') {
      failedTaskIds.push(taskId);
    }
  }

  return {
    activeSession,
    taskEntries,
    runningTaskIds,
    failedTaskIds,
  };
}

function deriveProcessingState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
  canonicalTurns: CanonicalTurn[],
): AppState['processingState'] {
  const canonicalProcessingState = deriveProcessingStateFromCanonicalTurns(canonicalTurns, sessionId);
  if (canonicalProcessingState) {
    return canonicalProcessingState;
  }

  const { runningTaskIds } = buildSessionTaskStatusSummary(runtimeReadModel, sessionId);
  const isProcessing = runningTaskIds.length > 0;

  if (!isProcessing) {
    return null;
  }

  return {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: 0,
    pendingRequestIds: [],
  };
}

/**
 * 从后端 recovery.summaries 构建前端 OrchestrationRuntimeRecoverySummary。
 * 聚合所有恢复摘要条目，提取最新快照和任务统计。
 */
function deriveRecoverySummary(
  runtimeReadModel: RustRuntimeReadModelDto,
  activeSession: RustSessionRuntimeSummary | undefined,
  sessionId: string,
): OrchestrationRuntimeRecoverySummary | undefined {
  const summaries = runtimeReadModel.recovery?.summaries;
  const activeIds = normalizeStringArray(runtimeReadModel.recovery?.active_recovery_ids);
  if (!Array.isArray(summaries) || summaries.length === 0) {
    return undefined;
  }
  const sessionRecoveryIds = new Set(normalizeStringArray(activeSession?.recovery_ids));
  const recoveryRef = normalizeString(activeSession?.recovery_ref);
  if (recoveryRef) {
    sessionRecoveryIds.add(recoveryRef);
  }
  const missionId = normalizeString(activeSession?.mission_id);
  const chainRef = normalizeString(activeSession?.execution_chain_ref);
  const scopedSummaries = summaries.filter((summary) => {
    const summarySessionId = normalizeString(summary.session_id);
    const summaryRecoveryId = normalizeString(summary.recovery_id);
    return (summarySessionId && summarySessionId === sessionId)
      || (summaryRecoveryId && sessionRecoveryIds.has(summaryRecoveryId))
      || (missionId && normalizeString(summary.mission_id) === missionId)
      || (chainRef && normalizeString(summary.execution_chain_ref) === chainRef);
  });
  const scopedRecoveryIds = new Set(scopedSummaries.map((summary) => normalizeString(summary.recovery_id)).filter(Boolean));
  const scopedActiveIds = activeIds.filter((id) => scopedRecoveryIds.has(id) || sessionRecoveryIds.has(id));
  if (scopedSummaries.length === 0 || (scopedActiveIds.length === 0 && activeSession?.has_recoverable_chain !== true)) {
    return undefined;
  }
  // 取最新的 recovery 摘要
  const sorted = [...scopedSummaries].sort(
    (a, b) => normalizeNumber(b.latest_occurred_at, 0) - normalizeNumber(a.latest_occurred_at, 0),
  );
  const latest = sorted[0];
  const pendingCount = scopedActiveIds.length;
  const completedCount = scopedSummaries.filter(
    (s) => normalizeString(s.current_status) === 'consumed'
      || normalizeString(s.current_status) === 'worker_resumed',
  ).length;

  return {
    continuationPolicy: pendingCount > 0 ? 'resumable' : 'none',
    continuationReason: latest.diagnostic_summary || undefined,
    latestSnapshotId: normalizeString(latest.recovery_id) || undefined,
    latestSnapshotCreatedAt: normalizeNumber(latest.latest_occurred_at, undefined as unknown as number) || undefined,
    pendingTaskCount: pendingCount,
    completedTaskCount: completedCount,
    runningTaskCount: scopedSummaries.filter(
      (s) => normalizeString(s.current_status) === 'ready',
    ).length,
  };
}

/**
 * 从后端 recentEvents 构建前端 OrchestrationRuntimeTimelineEntry[]。
 * 仅筛选与当前 session 相关的事件。
 */
function deriveRecentTimeline(
  recentEvents: RustEventEnvelope[],
  sessionId: string,
): OrchestrationRuntimeTimelineEntry[] {
  if (!Array.isArray(recentEvents) || recentEvents.length === 0) {
    return [];
  }
  return recentEvents
    .filter((event) => {
      const eventSessionId = normalizeString(event.session_id);
      return Boolean(eventSessionId) && eventSessionId === sessionId;
    })
    .map((event) => ({
      eventId: normalizeString(event.event_id) || `evt-${normalizeNumber(event.sequence, 0)}`,
      seq: normalizeNumber(event.sequence, 0),
      timestamp: normalizeNumber(event.occurred_at, 0),
      type: normalizeString(event.event_type) || 'unknown',
      summary: buildEventSummary(event),
      diffCount: 0,
    }))
    .sort((a, b) => a.seq - b.seq);
}

/** 从 EventEnvelope 构建可读摘要 */
function buildEventSummary(event: RustEventEnvelope): string {
  const eventType = normalizeString(event.event_type);
  const payload = resolveEventPayload(event);
  const eventLabel = formatRuntimeEventLabel(eventType);
  const parts: string[] = [];
  const title = resolveEventReadableTitle(payload);
  if (title) {
    parts.push(title);
  }
  const status = formatRuntimeStatusLabel(
    normalizeString(payload.current_status)
      || normalizeString(payload.status)
      || normalizeString(payload.next_status)
      || normalizeString(payload.stage)
      || normalizeString(payload.current_stage),
  );
  if (status) {
    parts.push(status);
  }
  const hasIssueDetail = Boolean(normalizeString(payload.error)
    || normalizeString(payload.error_message)
    || normalizeString(payload.failure_reason)
    || normalizeString(payload.diagnostic_summary));
  if (hasIssueDetail && !status) {
    parts.push('需要关注');
  }
  return parts.length > 0 ? `${eventLabel}：${parts.join(' · ')}` : eventLabel;
}

function resolveEventReadableTitle(payload: Record<string, unknown>): string {
  return normalizeString(payload.task_title)
    || normalizeString(payload.assignment_title)
    || normalizeString(payload.mission_title)
    || normalizeString(payload.display_name)
    || normalizeString(payload.title)
    || normalizeString(payload.tool_name)
    || normalizeString(payload.name);
}

function formatRuntimeEventLabel(eventType: string): string {
  switch (eventType) {
    case 'task.dispatched':
      return '任务已派发';
    case 'task.status.changed':
      return '任务状态更新';
    case 'mission.execution.overview':
      return '执行概览';
    case 'mission.resume.dispatch.created':
      return '恢复调度已创建';
    case 'worker.reported':
      return '执行者上报';
    case 'worker.tool.observed':
    case 'tool.invoked':
      return '工具调用';
    case 'worker.skill_dispatch.observed':
    case 'worker.skill_dispatch.applied':
      return '技能调度';
    case 'worker.executor.observed':
      return '执行器状态';
    case 'governance.decision.applied':
      return '决策已应用';
    case 'system.runtime.maintenance.status':
      return '运行态维护';
    default:
      return eventType
        ? eventType.split('.').map(formatRuntimeEventTokenLabel).filter(Boolean).join(' · ')
        : '运行事件';
  }
}

function formatRuntimeEventTokenLabel(token: string): string {
  switch (token) {
    case 'task': return '任务';
    case 'mission': return '执行组';
    case 'worker': return '执行者';
    case 'tool': return '工具';
    case 'governance': return '决策';
    case 'decision': return '决策';
    case 'system': return '系统';
    case 'runtime': return '运行态';
    case 'execution': return '执行';
    case 'overview': return '概览';
    case 'status': return '状态';
    case 'changed': return '更新';
    case 'dispatched': return '已派发';
    case 'reported': return '上报';
    case 'observed': return '已观测';
    case 'applied': return '已应用';
    case 'resume': return '恢复';
    case 'dispatch': return '调度';
    case 'created': return '已创建';
    default:
      return token.replace(/[_-]/g, ' ');
  }
}

function formatRuntimeStatusLabel(status: string): string {
  const raw = normalizeString(status);
  if (!raw) {
    return '';
  }
  switch (normalizeSubTaskStatus(raw)) {
    case 'completed':
      return '已完成';
    case 'failed':
      return '已失败';
    case 'blocked':
      return '已阻塞';
    case 'cancelled':
      return '已取消';
    case 'running':
    case 'in_progress':
      return '运行中';
    case 'awaiting_approval':
      return '等待审批';
    case 'review_required':
      return '需要返工';
    case 'pending':
      return '待执行';
    default:
      return '';
  }
}

/**
 * 从后端 operations.attention 构建 failureRootCause（仅在有失败时）。
 */
function deriveFailureRootCause(
  generatedAt: number,
  failedTaskLabels: string[],
): OrchestrationRuntimeFailureRootCause | undefined {
  if (failedTaskLabels.length === 0) {
    return undefined;
  }
  const taskList = failedTaskLabels.slice(0, 4).join('、');
  const suffix = failedTaskLabels.length > 4 ? `等 ${failedTaskLabels.length} 个任务` : `${failedTaskLabels.length} 个任务`;
  return {
    summary: `${suffix}执行失败：${taskList}。请查看任务追踪中的失败项。`,
    occurredAt: generatedAt,
  };
}

/**
 * 构建 opsView：聚合 recovery、timeline、diagnostics 等后端已有数据。
 */
function deriveOpsView(
  runtimeReadModel: RustRuntimeReadModelDto,
  recentEvents: RustEventEnvelope[],
  sessionId: string,
  generatedAt: number,
  activeSession: RustSessionRuntimeSummary | undefined,
  failedTaskLabels: string[],
  status: OrchestratorRuntimeState['status'],
): OrchestrationRuntimeOpsView | null {
  const recovery = deriveRecoverySummary(runtimeReadModel, activeSession, sessionId);
  const recentTimeline = deriveRecentTimeline(recentEvents, sessionId);
  const failureRootCause = status === 'failed' || status === 'blocked'
    ? deriveFailureRootCause(generatedAt, failedTaskLabels)
    : undefined;
  const eventCount = recentTimeline.length;

  // 没有任何有意义数据时返回 null，避免空壳
  const hasContent = recentTimeline.length > 0
    || recovery !== undefined
    || failureRootCause !== undefined;
  if (!hasContent) {
    return null;
  }

  return {
    scope: {
      sessionId,
    },
    timelinePath: 'bootstrap',
    eventCount,
    diffCount: 0,
    recentTimeline,
    recentStateDiffs: [],
    recovery,
    failureRootCause,
  };
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
    default:
      return undefined;
  }
}

/**
 * 从后端 session.budget 装配上下文预算快照。
 *
 * 后端 usage-authority 已在 DTO 层算好 token 占用、剩余与告警分级，这里只做
 * 字段映射；没有 budget 的会话返回 null，面板据此隐藏预算卡片。
 */
function buildRuntimeSnapshot(
  activeSession: RustSessionRuntimeSummary | undefined,
): OrchestratorRuntimeSnapshotState | null {
  const budget = activeSession?.budget;
  if (!budget || typeof budget.token_limit !== 'number') {
    return null;
  }
  const compaction = activeSession?.context_compaction;
  return {
    budgetState: {
      tokenUsed: budget.token_used,
      remainingTokens: budget.remaining_tokens,
      tokenLimit: budget.token_limit,
      usageRatio: budget.usage_ratio,
      warningLevel: normalizeBudgetWarningLevel(budget.warning_level),
      lastCompactionAt: compaction?.compacted_at ?? undefined,
      lastCompactionReason: compaction?.reason ?? undefined,
      originalTokenEstimate: compaction?.original_token_estimate ?? undefined,
      compactedTokenEstimate: compaction?.compacted_token_estimate ?? undefined,
      originalMessageCount: compaction?.original_message_count ?? undefined,
      compactedMessageCount: compaction?.compacted_message_count ?? undefined,
    },
  };
}

function deriveRuntimeState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  assignments: OrchestrationRuntimeAssignmentSummary[],
  recentEvents: RustEventEnvelope[],
  sessionId: string,
  generatedAt: number,
): OrchestratorRuntimeState | null {
  if (!runtimeReadModel) {
    return null;
  }

  const { activeSession, runningTaskIds, failedTaskIds } = buildSessionTaskStatusSummary(
    runtimeReadModel,
    sessionId,
  );
  const rawRootTaskStatus = normalizeString(activeSession?.root_task_status || activeSession?.current_status);
  const rootTaskStatus = normalizeSubTaskStatus(rawRootTaskStatus);
  const rootAllowsResume = rootTaskStatus !== 'completed' && rootTaskStatus !== 'cancelled';
  const hasRecoverableChain = activeSession?.has_recoverable_chain === true && rootAllowsResume;
  const recoverableBranchCount = typeof activeSession?.recoverable_branch_count === 'number'
    ? activeSession.recoverable_branch_count
    : 0;
  const status = runningTaskIds.length > 0
    ? 'running'
    : rootTaskStatus === 'running'
      ? 'running'
      : rootTaskStatus === 'blocked'
        ? 'blocked'
        : rootTaskStatus === 'completed'
          ? 'completed'
          : rootTaskStatus === 'cancelled'
            ? 'cancelled'
            : rootTaskStatus === 'failed' || failedTaskIds.length > 0
              ? 'failed'
              : rootTaskStatus === 'paused'
                ? 'paused'
                : activeSession
                  ? 'idle'
                  : 'idle';
  const failedTaskLabels = assignments
    .filter((assignment) => {
      const normalizedStatus = normalizeSubTaskStatus(normalizeString(assignment.status));
      return normalizedStatus === 'failed' || normalizedStatus === 'blocked';
    })
    .map((assignment) => normalizeString(assignment.title))
    .filter((title, index, arr) => title && arr.indexOf(title) === index);

  const opsView = deriveOpsView(
    runtimeReadModel,
    recentEvents,
    sessionId,
    generatedAt,
    activeSession,
    failedTaskLabels,
    status,
  );

  return {
    sessionId: sessionId || undefined,
    status,
    phase: runningTaskIds.length > 0
      ? 'execute'
      : status === 'blocked'
        ? 'blocked'
        : 'idle',
    errors: failedTaskLabels.map((title) => `${title} 执行失败`),
    statusChangedAt: normalizeNumber(activeSession?.last_update, generatedAt),
    lastEventAt: normalizeNumber(activeSession?.last_update, generatedAt),
    canResume: hasRecoverableChain,
    runtimeReason: activeSession?.latest_event_type || undefined,
    assignments,
    chain: activeSession?.execution_chain_ref
      ? {
          chainId: activeSession.execution_chain_ref,
          status: activeSession.root_task_status || activeSession.current_status || 'unknown',
          recoverable: hasRecoverableChain && recoverableBranchCount > 0,
          attempt: 1,
          createdAt: normalizeNumber(activeSession.last_update, generatedAt),
          updatedAt: normalizeNumber(activeSession.last_update, generatedAt),
        }
      : undefined,
    opsView,
    runtimeSnapshot: buildRuntimeSnapshot(activeSession),
    runtimeDecisionTrace: [],
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

export function isRustEventEnvelope(value: unknown): value is RustEventEnvelope {
  return normalizeEventEnvelope(value) !== null;
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
  const sessions = normalizeRustSessions(payload, generatedAt, workspace.workspaceId);
  const selectedSessionId = normalizeString(payload.currentSession?.sessionId);
  const currentSession = selectedSessionId
    ? sessions.find((session) => session.id === selectedSessionId)
    : undefined;
  const normalizedEvents = Array.isArray(payload.recentEvents)
    ? payload.recentEvents.map((event) => normalizeEventEnvelope(event)).filter((event): event is RustEventEnvelope => event !== null)
    : [];
  const rawCanonicalTurns = Array.isArray(payload.canonicalTurns)
    ? payload.canonicalTurns
    : [];
  const canonicalTurns = rawCanonicalTurns
    .map(normalizeCanonicalTurn)
    .filter((turn): turn is CanonicalTurn => Boolean(turn));
  const assignments = buildAssignmentsFromRuntime(
    payload.runtimeReadModel,
    normalizedEvents,
    currentSession?.id || '',
  );
  const processingState = deriveProcessingState(
    payload.runtimeReadModel,
    currentSession?.id || '',
    canonicalTurns,
  );
  const pendingChanges = Array.isArray(payload.pendingChanges)
    ? payload.pendingChanges
    : [];
  const pendingChangesState = payload.pendingChangesState ?? null;
  const state: AppState = {
    ...buildEmptyWorkspaceAppState(generatedAt),
    sessions,
    currentSession,
    currentSessionId: currentSession?.id || '',
    currentWorkspaceId: workspace.workspaceId,
    currentWorkspacePath: workspace.rootPath,
    isProcessing: Boolean(processingState?.isProcessing),
    processingState,
    pendingChanges,
    pendingChangesState,
    pendingChangesStateVersion: generatedAt,
    stateUpdatedAt: generatedAt,
  };
  const normalizedNotifications = normalizeNotifications(payload.notifications);

  return {
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
    notifications: workspace.workspaceId
        ? {
          workspaceId: workspace.workspaceId,
          sessionId: currentSession?.id || undefined,
          notifications: {
            lastUpdatedAt: generatedAt,
            records: normalizedNotifications,
          },
        }
      : undefined,
    orchestratorRuntimeState: deriveRuntimeState(
      payload.runtimeReadModel,
      assignments,
      normalizedEvents,
      currentSession?.id || '',
      generatedAt,
    ),
  };
}

export function readRustTimelinePageMeta(rawPayload: unknown): {
  sessionId: string;
  hasMoreBefore: boolean;
  beforeCursor: string | null;
} {
  const payload = (rawPayload ?? {}) as RustTimelinePageDto;
  return {
    sessionId: normalizeString(payload.sessionId),
    hasMoreBefore: payload.hasMoreBefore === true,
    beforeCursor: normalizeString(payload.beforeCursor) || null,
  };
}
