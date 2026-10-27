import { i18n } from '../stores/i18n.svelte';
import type { TaskDto, TaskKind, TaskStatus } from '../shared/rust-backend-types';

type TaskDisplayInput = Pick<TaskDto, 'kind' | 'title' | 'goal' | 'task_id'>;

function compactChineseSpacing(text: string): string {
  return text.replace(/([\u4e00-\u9fff])\s+([\u4e00-\u9fff])/g, '$1$2');
}

function stripInternalTaskIds(text: string): string {
  return text.replace(
    /\b(?:task|mission|worker|agent)-[a-z0-9]+(?:-[a-z0-9]+)*\b/gi,
    i18n.t('taskLabels.related'),
  );
}

function normalizeTaskText(text: string | null | undefined): string {
  return compactChineseSpacing(stripInternalTaskIds((text ?? '').trim()));
}

export function getTaskDisplayTitle(task: TaskDisplayInput): string {
  return normalizeTaskText(task.title) || i18n.t('taskLabels.unnamed');
}

export function getTaskDisplayGoal(task: TaskDisplayInput): string {
  return normalizeTaskText(task.goal);
}

export function getDecisionDisplayReason(reason: string | null | undefined): string {
  return normalizeTaskText(reason) || i18n.t('taskLabels.failure');
}

export function getTaskDisplayText(text: string | null | undefined): string {
  return normalizeTaskText(text);
}

export function getTaskKindLabel(kind: TaskKind): string {
  switch (kind) {
    case 'local_agent': return i18n.t('taskLabels.kind.local_agent');
    case 'local_workflow': return i18n.t('taskLabels.kind.local_workflow');
    case 'remote_agent': return i18n.t('taskLabels.kind.remote_agent');
    case 'monitor_mcp': return i18n.t('taskLabels.kind.monitor_mcp');
    case 'in_process_teammate': return i18n.t('taskLabels.kind.in_process_teammate');
    case 'dream': return i18n.t('taskLabels.kind.dream');
    default: return i18n.t('taskLabels.kind.default');
  }
}

export const USER_VISIBLE_TASK_KINDS: readonly TaskKind[] = [
  'local_agent',
  'local_workflow',
  'remote_agent',
  'monitor_mcp',
  'in_process_teammate',
  'dream',
];

export function isUserVisibleTaskKind(kind: TaskKind): boolean {
  return USER_VISIBLE_TASK_KINDS.includes(kind);
}

export function getTaskStatusLabel(status: TaskStatus): string {
  switch (status) {
    case 'pending': return i18n.t('taskLabels.status.pending');
    case 'running': return i18n.t('taskLabels.status.running');
    case 'completed': return i18n.t('taskLabels.status.completed');
    case 'failed': return i18n.t('taskLabels.status.failed');
    case 'killed': return i18n.t('taskLabels.status.killed');
    default: return i18n.t('taskLabels.status.unknown');
  }
}

export function getTaskStatusTone(status: TaskStatus): string {
  switch (status) {
    case 'running': return i18n.t('taskLabels.tone.running');
    case 'completed': return i18n.t('taskLabels.tone.completed');
    case 'failed': return i18n.t('taskLabels.tone.failed');
    case 'killed': return i18n.t('taskLabels.status.killed');
    default: return i18n.t('taskLabels.tone.pending');
  }
}

export function getRunnerStatusLabel(status: string): string {
  switch (status) {
    case 'running': return i18n.t('taskLabels.runner.running');
    case 'pending': return i18n.t('taskLabels.status.pending');
    case 'completed': return i18n.t('taskLabels.status.completed');
    case 'error': return i18n.t('taskLabels.runner.error');
    case 'killed': return i18n.t('taskLabels.status.killed');
    default: return i18n.t('taskLabels.runner.idle');
  }
}

export type RunnerUserState = 'in-progress' | 'stopped' | 'finished';

export function getRunnerUserState(status: string): RunnerUserState {
  switch (status) {
    case 'running':
    case 'pending':
      return 'in-progress';
    case 'completed':
    case 'error':
    case 'killed':
      return 'finished';
    default:
      return 'stopped';
  }
}

export function getRunnerUserStateLabel(status: string): string {
  switch (getRunnerUserState(status)) {
    case 'in-progress': return i18n.t('taskLabels.status.running');
    case 'finished': return i18n.t('taskLabels.status.completed');
    default: return i18n.t('taskLabels.runner.stopped');
  }
}

export function getRunnerUserStateTone(status: string): 'running' | 'failed' | 'completed' | 'stopped' {
  if (status === 'error') return 'failed';
  if (status === 'killed') return 'stopped';
  switch (getRunnerUserState(status)) {
    case 'in-progress': return 'running';
    case 'finished': return 'completed';
    default: return 'stopped';
  }
}

export function getRunnerUserStateTooltip(status: string, failedReason?: string | null): string | null {
  if (status === 'error') {
    const reason = (failedReason ?? '').trim();
    return reason ? i18n.t('taskLabels.runner.failureReason', { reason }) : i18n.t('taskLabels.runner.failed');
  }
  if (status === 'killed') {
    return i18n.t('taskLabels.runner.killed');
  }
  return null;
}
