import type { TaskDto, TaskKind, TaskStatus } from '../shared/rust-backend-types';

type TaskDisplayInput = Pick<TaskDto, 'kind' | 'title' | 'goal' | 'task_id'>;

function compactChineseSpacing(text: string): string {
  return text.replace(/([\u4e00-\u9fff])\s+([\u4e00-\u9fff])/g, '$1$2');
}

function stripInternalTaskIds(text: string): string {
  return text.replace(
    /\b(?:task|mission|worker|local|agent|bash)-[a-z0-9]+(?:-[a-z0-9]+)*\b/gi,
    '相关任务',
  );
}

function normalizeTaskText(text: string | null | undefined): string {
  return compactChineseSpacing(stripInternalTaskIds((text ?? '').trim()));
}

export function getTaskDisplayTitle(task: TaskDisplayInput): string {
  return normalizeTaskText(task.title) || task.task_id;
}

export function getTaskDisplayGoal(task: TaskDisplayInput): string {
  return normalizeTaskText(task.goal);
}

export function getTaskDisplayBlockedReason(task: Pick<TaskDto, 'goal'>): string {
  return normalizeTaskText(task.goal) || '任务执行失败。';
}

export function getDecisionDisplayReason(reason: string | null | undefined): string {
  return normalizeTaskText(reason) || '任务执行失败。';
}

export function getTaskDisplayText(text: string | null | undefined): string {
  return normalizeTaskText(text);
}

export function getTaskKindLabel(kind: TaskKind): string {
  switch (kind) {
    case 'local_agent': return '代理任务';
    case 'local_bash': return '本地命令';
    case 'local_workflow': return '本地流程';
    case 'remote_agent': return '远程代理';
    case 'monitor_mcp': return 'MCP 监控';
    case 'in_process_teammate': return '进程内队友';
    case 'dream': return '后台整理';
    default: return kind;
  }
}

export const USER_VISIBLE_TASK_KINDS: readonly TaskKind[] = [
  'local_agent',
  'local_bash',
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
    case 'pending': return '待执行';
    case 'running': return '执行中';
    case 'completed': return '已完成';
    case 'failed': return '失败';
    case 'killed': return '已终止';
    default: return status;
  }
}

export function getTaskStatusTone(status: TaskStatus): string {
  switch (status) {
    case 'running': return '正在推进';
    case 'completed': return '已收束';
    case 'failed': return '执行失败';
    case 'killed': return '已终止';
    default: return '等待执行';
  }
}

export function getRunnerStatusLabel(status: string): string {
  switch (status) {
    case 'running': return '运行中';
    case 'pending': return '待执行';
    case 'completed': return '已完成';
    case 'error': return '异常';
    case 'killed': return '已终止';
    default: return '空闲';
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
    case 'in-progress': return '执行中';
    case 'finished': return '已完成';
    default: return '已停止';
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
    return reason ? `执行异常：${reason}` : '执行过程中出现异常';
  }
  if (status === 'killed') {
    return '任务已终止';
  }
  return null;
}
