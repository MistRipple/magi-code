import type { DecisionTaskPayloadDto, TaskDto, TaskKind, TaskStatus } from '../shared/rust-backend-types';

type TaskDisplayInput = Pick<TaskDto, 'kind' | 'title' | 'goal' | 'task_id' | 'decision_payload'>;

const ESCALATION_REASON_LABELS: Record<string, string> = {
  on_failure: '执行失败',
  high_risk: '高风险操作',
  on_repair_exhausted: '修复次数耗尽',
  repair_budget_exhausted: '修复预算耗尽',
  conflicting_requirements: '需求冲突',
  architecture_fork: '架构分歧',
  missing_acceptance_criteria: '验收标准缺失',
  unsafe_or_destructive_action: '安全或破坏性风险',
  permission_boundary: '权限边界',
  irreversible_action: '不可逆操作',
};

function compactChineseSpacing(text: string): string {
  return text.replace(/([\u4e00-\u9fff])\s+([\u4e00-\u9fff])/g, '$1$2');
}

function stripDecisionPrefix(text: string): string {
  return text.replace(/^Decision:\s*/i, '').trim();
}

function stripInternalTaskIds(text: string): string {
  return text.replace(
    /\b(?:task|phase|wp|workpackage|validation|repair|action|act|obj|root)-[a-z0-9]+(?:-[a-z0-9]+)*\b/gi,
    '相关任务',
  );
}

function normalizeTaskText(text: string | null | undefined): string {
  return compactChineseSpacing(stripInternalTaskIds(stripDecisionPrefix((text ?? '').trim())));
}

function normalizeDecisionContext(text: string | null | undefined): string {
  const normalized = normalizeTaskText(text)
    .replace(/需要决策后续操作/g, '需要选择后续处理方式')
    .replace(/任务\s+相关任务\s+执行失败，需要选择后续处理方式/g, '任务执行失败，需要选择后续处理方式')
    .replace(/任务\s+(.+?)\s+执行失败，需要选择后续处理方式/g, '$1执行失败，需要选择后续处理方式')
    .replace(/验证任务\s+(.+?)\s+缺少交付证据/g, '$1缺少验证证据');
  return normalized || '等待确认后续处理方式';
}

function extractEscalationReasonLabels(...texts: Array<string | null | undefined>): string[] {
  const combined = texts.filter(Boolean).join(' ');
  const keys = new Set<string>();
  const quotedKeyPattern = /"([^"]+)"/g;
  let match: RegExpExecArray | null;
  while ((match = quotedKeyPattern.exec(combined)) !== null) {
    if (match[1]) keys.add(match[1]);
  }
  for (const key of Object.keys(ESCALATION_REASON_LABELS)) {
    if (combined.includes(key)) keys.add(key);
  }
  return Array.from(keys)
    .map((key) => ESCALATION_REASON_LABELS[key])
    .filter((label): label is string => Boolean(label));
}

function joinReasonLabels(labels: string[]): string {
  if (labels.length === 0) return '';
  if (labels.length === 1) return labels[0];
  if (labels.length === 2) return `${labels[0]}和${labels[1]}`;
  return `${labels.slice(0, -1).join('、')}和${labels[labels.length - 1]}`;
}

export function getTaskDisplayTitle(task: TaskDisplayInput): string {
  if (task.kind === 'Decision') {
    const context = normalizeDecisionContext(task.decision_payload?.decision_context || task.title);
    return context.startsWith('需要决策') ? context : `需要决策：${context}`;
  }
  return normalizeTaskText(task.title) || task.task_id;
}

export function getTaskDisplayGoal(task: TaskDisplayInput): string {
  if (task.kind === 'Decision') {
    return getTaskDisplayBlockedReason(task);
  }
  return normalizeTaskText(task.goal);
}

export function getTaskDisplayBlockedReason(task: Pick<TaskDto, 'kind' | 'goal' | 'decision_payload'>): string {
  const payload = task.decision_payload;
  const reason = payload?.blocked_reason || task.goal || '';
  return getDecisionDisplayReason(reason, payload);
}

export function getDecisionDisplayReason(
  reason: string | null | undefined,
  payload?: DecisionTaskPayloadDto | null,
): string {
  const riskNotes = payload?.risk_notes ?? [];
  const labels = extractEscalationReasonLabels(reason, ...riskNotes);
  if (labels.length > 0) {
    return `失败原因：涉及${joinReasonLabels(labels)}，需要确认后续处理方式。`;
  }

  const normalized = normalizeTaskText(reason)
    .replace(/\s*\(escalation:.*\)\s*$/i, '')
    .replace(/^任务\s+相关任务\s+失败$/g, '任务执行失败，需要确认后续处理方式')
    .replace(/^任务\s+(.+?)\s+失败$/g, '$1失败，需要确认后续处理方式');
  return normalized || '需要确认后续处理方式。';
}

export function getTaskDisplayText(text: string | null | undefined): string {
  const normalized = normalizeTaskText(text);
  const labels = extractEscalationReasonLabels(text);
  if (labels.length > 0) {
    return `涉及${joinReasonLabels(labels)}`;
  }
  return normalized;
}

export function getTaskKindLabel(kind: TaskKind): string {
  switch (kind) {
    case 'Objective': return '目标';
    case 'Phase': return '阶段';
    case 'WorkPackage': return '工作包';
    case 'Action': return '步骤';
    case 'Validation': return '验证';
    case 'Repair': return '修复';
    case 'Decision': return '决策';
    default: return kind;
  }
}

// 用户面（TasksPanel 主视图）只暴露这三类节点；
// Phase / WorkPackage / Repair / Objective 属于引擎结构，仅在“技术明细”折叠区呈现。
export const USER_VISIBLE_TASK_KINDS: readonly TaskKind[] = ['Action', 'Validation', 'Decision'];

export function isUserVisibleTaskKind(kind: TaskKind): boolean {
  return USER_VISIBLE_TASK_KINDS.includes(kind);
}

export function getTaskStatusLabel(status: TaskStatus): string {
  switch (status) {
    case 'Draft': return '待规划';
    case 'Ready': return '待执行';
    case 'Running': return '执行中';
    case 'Blocked': return '需要处理';
    case 'AwaitingApproval': return '等待确认';
    case 'Verifying': return '验证中';
    case 'Repairing': return '修复中';
    case 'Completed': return '已完成';
    case 'Failed': return '失败';
    case 'Cancelled': return '已取消';
    case 'Skipped': return '已跳过';
    default: return status;
  }
}

export function getTaskStatusTone(status: TaskStatus): string {
  if (status === 'AwaitingApproval' || status === 'Blocked') return '需要处理';
  if (status === 'Running' || status === 'Verifying' || status === 'Repairing') return '正在推进';
  if (status === 'Completed' || status === 'Skipped') return '已收束';
  if (status === 'Cancelled') return '已取消';
  if (status === 'Failed') return '需要修复';
  return '等待执行';
}

export function getRunnerStatusLabel(status: string): string {
  switch (status) {
    case 'running': return '运行中';
    case 'blocked': return '等待处理';
    case 'completed': return '已完成';
    case 'error': return '异常';
    default: return '空闲';
  }
}

// 用户面三态：把 5 态 runner_status 压成用户能识别的「执行中 / 已停止 / 已完成」。
// running + blocked 都算执行中（blocked 用 tooltip 解释等待原因）；
// idle 算已停止；completed + error 都算已完成（error 用 tone 标红）。
export type RunnerUserState = 'in-progress' | 'stopped' | 'finished';

export function getRunnerUserState(status: string): RunnerUserState {
  switch (status) {
    case 'running':
    case 'blocked':
      return 'in-progress';
    case 'completed':
    case 'error':
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

// tone 用于 badge 着色：error → failed（红），其余按用户态语义。
export function getRunnerUserStateTone(status: string): 'running' | 'failed' | 'completed' | 'stopped' {
  if (status === 'error') return 'failed';
  switch (getRunnerUserState(status)) {
    case 'in-progress': return 'running';
    case 'finished': return 'completed';
    default: return 'stopped';
  }
}

// blocked 状态需要 tooltip 解释「等待什么」；其余态返回 null（继续用普通 title）。
export function getRunnerUserStateTooltip(status: string, blockedReason?: string | null): string | null {
  if (status === 'blocked') {
    const reason = (blockedReason ?? '').trim();
    return reason ? `等待处理：${reason}` : '等待处理';
  }
  if (status === 'error') {
    return '执行过程中出现异常';
  }
  return null;
}
