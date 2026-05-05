import type { SessionBootstrapSnapshot } from '../session-bootstrap';
import type { CanonicalTurn } from '../protocol/canonical-turn';
import { normalizeCanonicalTurn } from '../protocol/canonical-turn';
import { deriveProcessingStateFromCanonicalTurns } from '../protocol/canonical-processing';
import type {
  AppState,
  OrchestrationRuntimeAssignmentSummary,
  OrchestrationRuntimeFailureRootCause,
  OrchestrationRuntimeOpsView,
  OrchestrationRuntimeRecoverySummary,
  OrchestrationRuntimeTimelineEntry,
  OrchestratorRuntimeState,
  Session,
  SessionNotificationRecord,
  SubTaskItem,
} from '../../types/message';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';

export type BootstrapPayload = SessionBootstrapSnapshot & {
  agent?: {
    runtimeEpoch?: string;
  };
  canonicalTurns?: CanonicalTurn[];
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
  title?: string | null;
  createdAt?: number;
  updatedAt?: number;
  messageCount?: number;
}

interface RustBootstrapWorkspaceRecord {
  workspaceId?: string;
  rootPath?: string;
}

interface RustNotificationRecord {
  notificationId?: string;
  sessionId?: string;
  kind?: string;
  message?: string;
  createdAt?: number;
  handled?: boolean;
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
  worker_lanes?: RustSessionRuntimeTurnLaneSummary[];
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
  lane_id?: string | null;
  lane_seq?: number | null;
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
  thread_visible?: boolean;
  worker_visible?: boolean;
}

interface RustSessionRuntimeTurnLaneSummary {
  lane_id?: string;
  lane_seq?: number;
  task_id?: string;
  worker_id?: string;
  role_id?: string | null;
  title?: string;
  status?: string;
  is_primary?: boolean;
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
  canonical_turns?: unknown[];
  pendingChanges?: unknown[];
  pending_changes?: unknown[];
  workspaces?: RustBootstrapWorkspaceRecord[];
  runtimeReadModel?: RustRuntimeReadModelDto;
  notifications?: RustNotificationRecord[];
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

interface NormalizedNotificationEntry {
  sessionId: string;
  record: SessionNotificationRecord;
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

function normalizeRuntimeTurnLanes(raw: unknown): RustSessionRuntimeTurnLaneSummary[] {
  return Array.isArray(raw)
    ? raw
      .map((lane) => normalizeObjectRecord(lane))
      .filter((lane): lane is Record<string, unknown> => lane !== null)
      .map((lane) => lane as RustSessionRuntimeTurnLaneSummary)
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
      worker_lanes: normalizeRuntimeTurnLanes(entry.worker_lanes),
    }));
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
    const createdAt = normalizeNumber(session.createdAt, generatedAt);
    const updatedAt = normalizeNumber(session.updatedAt, createdAt);
    const messageCount = normalizeNumber(session.messageCount, NaN);
    sessions.push({
      id: sessionId,
      name: normalizeString(session.title) || undefined,
      createdAt,
      updatedAt,
      ...(Number.isFinite(messageCount) ? { messageCount } : {}),
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
  const workspaces = Array.isArray(payload.workspaces) ? payload.workspaces : [];
  const selectedWorkspace = workspaces.find((workspace) => normalizeString(workspace.workspaceId) === requestedWorkspaceId)
    || workspaces[0]
    || null;
  const workspaceId = requestedWorkspaceId || normalizeString(selectedWorkspace?.workspaceId);
  const rootPath = requestedWorkspacePath || normalizeString(selectedWorkspace?.rootPath);
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

function shortenId(value: string, prefix: string): string {
  if (!value) {
    return prefix;
  }
  if (value.length <= 20) {
    return value;
  }
  return `${prefix}-${value.slice(-8)}`;
}

function normalizeSubTaskStatus(status: string, failedDispatchCount = 0): SubTaskItem['status'] {
  const normalized = status.toLowerCase();
  if (normalized.includes('approval')) {
    return 'awaiting_approval';
  }
  if (normalized.includes('review')) {
    return 'review_required';
  }
  if (normalized.includes('cancel') || normalized.includes('abort')) {
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
  const { taskEntries, activeMissionIds } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
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
      title: lookups.assignmentTitles.get(assignmentId) || shortenId(assignmentId, 'assignment'),
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
  return assignments;
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
  const activeMissionIds = new Set(normalizeStringArray(activeSession?.active_execution_group_ids));
  if (activeMissionIds.size === 0 && activeTaskIds.size > 0) {
    for (const task of allTaskEntries) {
      const taskId = normalizeString(task.task_id);
      const missionId = normalizeString(task.mission_id);
      if (taskId && missionId && activeTaskIds.has(taskId)) {
        activeMissionIds.add(missionId);
      }
    }
  }
  const taskEntries = allTaskEntries
    .filter((entry) => {
      const taskId = normalizeString(entry.task_id);
      const missionId = normalizeString(entry.mission_id);
      return activeTaskIds.has(taskId) || (missionId && activeMissionIds.has(missionId));
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
): OrchestrationRuntimeRecoverySummary | undefined {
  const summaries = runtimeReadModel.recovery?.summaries;
  const activeIds = normalizeStringArray(runtimeReadModel.recovery?.active_recovery_ids);
  if (!Array.isArray(summaries) || summaries.length === 0) {
    return undefined;
  }
  // 取最新的 recovery 摘要
  const sorted = [...summaries].sort(
    (a, b) => normalizeNumber(b.latest_occurred_at, 0) - normalizeNumber(a.latest_occurred_at, 0),
  );
  const latest = sorted[0];
  const pendingCount = activeIds.length;
  const completedCount = summaries.filter(
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
    runningTaskCount: summaries.filter(
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
      // 保留匹配当前 session 的事件，或无 session 归属的系统事件
      return !eventSessionId || eventSessionId === sessionId;
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
  const parts: string[] = [];
  if (eventType) {
    parts.push(eventType);
  }
  const taskId = normalizeString(event.task_id);
  if (taskId) {
    parts.push(`task:${taskId}`);
  }
  const missionId = normalizeString(event.mission_id);
  if (missionId && !taskId) {
    parts.push(`mission:${missionId}`);
  }
  return parts.join(' ') || 'event';
}

/**
 * 从后端 operations.attention 构建 failureRootCause（仅在有失败时）。
 */
function deriveFailureRootCause(
  runtimeReadModel: RustRuntimeReadModelDto,
  generatedAt: number,
): OrchestrationRuntimeFailureRootCause | undefined {
  const attention = runtimeReadModel.operations?.attention;
  if (!attention) {
    return undefined;
  }
  const failedTaskIds = normalizeStringArray(attention.failed_task_ids);
  const failedWorkerIds = normalizeStringArray(attention.failed_worker_ids);
  const failedAssignmentIds = normalizeStringArray(attention.failed_assignment_ids);
  if (failedTaskIds.length === 0 && failedWorkerIds.length === 0 && failedAssignmentIds.length === 0) {
    return undefined;
  }
  const parts: string[] = [];
  if (failedTaskIds.length > 0) {
    parts.push(`${failedTaskIds.length} 个任务失败 (${failedTaskIds.join(', ')})`);
  }
  if (failedWorkerIds.length > 0) {
    parts.push(`${failedWorkerIds.length} 个 Worker 失败`);
  }
  if (failedAssignmentIds.length > 0) {
    parts.push(`${failedAssignmentIds.length} 个分配失败`);
  }
  return {
    summary: parts.join('; '),
    taskId: failedTaskIds[0] || undefined,
    assignmentId: failedAssignmentIds[0] || undefined,
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
): OrchestrationRuntimeOpsView | null {
  const recovery = deriveRecoverySummary(runtimeReadModel);
  const recentTimeline = deriveRecentTimeline(recentEvents, sessionId);
  const failureRootCause = deriveFailureRootCause(runtimeReadModel, generatedAt);
  const diagnostics = runtimeReadModel.overview?.diagnostics;
  const categoryCounts = runtimeReadModel.overview?.category_counts;

  // 计算总事件数
  const eventCount = (categoryCounts?.domain ?? 0)
    + (categoryCounts?.audit ?? 0)
    + (categoryCounts?.usage ?? 0)
    + (categoryCounts?.projection ?? 0)
    + (categoryCounts?.system ?? 0);

  // 没有任何有意义数据时返回 null，避免空壳
  const hasContent = recentTimeline.length > 0
    || recovery !== undefined
    || failureRootCause !== undefined
    || eventCount > 0
    || (diagnostics && (
      (diagnostics.running_task_count ?? 0) > 0
      || (diagnostics.failed_task_count ?? 0) > 0
      || (diagnostics.active_worker_count ?? 0) > 0
    ));
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
  const hasRecoverableChain = activeSession?.has_recoverable_chain === true;
  const recoverableBranchCount = typeof activeSession?.recoverable_branch_count === 'number'
    ? activeSession.recoverable_branch_count
    : 0;
  const rootTaskStatus = normalizeSubTaskStatus(
    normalizeString(activeSession?.root_task_status || activeSession?.current_status),
  );
  const status = runningTaskIds.length > 0
    ? 'running'
    : rootTaskStatus === 'blocked'
      ? 'blocked'
      : hasRecoverableChain
      ? 'paused'
      : failedTaskIds.length > 0
        ? 'failed'
        : activeSession
          ? rootTaskStatus === 'completed'
            ? 'completed'
            : 'idle'
          : 'idle';

  const opsView = deriveOpsView(runtimeReadModel, recentEvents, sessionId, generatedAt);

  return {
    sessionId: sessionId || undefined,
    status,
    phase: runningTaskIds.length > 0
      ? 'execute'
      : status === 'blocked'
        ? 'blocked'
        : (hasRecoverableChain ? 'paused' : 'idle'),
    errors: failedTaskIds.length > 0 ? failedTaskIds.map((id) => `task_failed:${id}`) : [],
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
    runtimeSnapshot: null,
    runtimeDecisionTrace: [],
  };
}

function normalizeNotificationKind(kind: string): SessionNotificationRecord['kind'] {
  if (kind === 'incident' || kind === 'audit' || kind === 'center') {
    return kind;
  }
  return 'toast';
}

function normalizeNotifications(
  notifications: RustNotificationRecord[] | undefined,
): NormalizedNotificationEntry[] {
  if (!Array.isArray(notifications)) {
    return [];
  }
  const normalized: NormalizedNotificationEntry[] = [];
  for (const notification of notifications) {
    const notificationId = normalizeString(notification.notificationId);
    const sessionId = normalizeString(notification.sessionId);
    const message = normalizeString(notification.message);
    if (!notificationId || !sessionId || !message) {
      continue;
    }
    normalized.push({
      sessionId,
      record: {
        notificationId,
        kind: normalizeNotificationKind(normalizeString(notification.kind)),
        level: 'info',
        message,
        createdAt: normalizeNumber(notification.createdAt, Date.now()),
        read: Boolean(notification.handled),
        persistToCenter: true,
        actionRequired: false,
        countUnread: !notification.handled,
      },
    });
  }
  return normalized;
}

function buildEmptyTimelineProjection(
  sessionId: string,
  generatedAt: number,
): SessionBootstrapSnapshot['timelineProjection'] {
  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: generatedAt,
    lastAppliedEventSeq: 0,
    artifacts: [],
    threadRenderEntries: [],
    workerRenderEntries: {},
  };
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
  const sessions = normalizeRustSessions(payload, generatedAt);
  const selectedSessionId = normalizeString(options.sessionId)
    || normalizeString(payload.currentSession?.sessionId)
    || '';
  const currentSession = selectedSessionId
    ? sessions.find((session) => session.id === selectedSessionId)
    : undefined;
  const workspace = resolveSelectedWorkspace(payload, options);
  const normalizedEvents = Array.isArray(payload.recentEvents)
    ? payload.recentEvents.map((event) => normalizeEventEnvelope(event)).filter((event): event is RustEventEnvelope => event !== null)
    : [];
  const rawCanonicalTurns = Array.isArray(payload.canonicalTurns)
    ? payload.canonicalTurns
    : Array.isArray(payload.canonical_turns)
      ? payload.canonical_turns
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
    : Array.isArray(payload.pending_changes)
      ? payload.pending_changes
      : [];
  const state: AppState = {
    ...buildEmptyWorkspaceAppState(generatedAt),
    sessions,
    currentSession,
    currentSessionId: currentSession?.id || '',
    isProcessing: Boolean(processingState?.isProcessing),
    processingState,
    pendingChanges,
    pendingChangesStateVersion: generatedAt,
    stateUpdatedAt: generatedAt,
  };
  const timelineProjection = buildEmptyTimelineProjection(currentSession?.id || '', generatedAt);
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
    timelineProjection,
    canonicalTurns,
    notifications: currentSession?.id
        ? {
          sessionId: currentSession.id,
          notifications: {
            lastUpdatedAt: generatedAt,
            records: normalizedNotifications
              .filter((item) => item.sessionId === currentSession.id)
              .map((item) => item.record),
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
