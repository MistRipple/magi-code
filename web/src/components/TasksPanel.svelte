<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    addToast,
    getEnabledAgents,
    getState,
    messagesState,
  } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type {
    DeliveryPackageDto,
    SessionTaskHistoryItemDto,
    TaskDto,
    TaskProjectionDto,
    TaskStatus,
  } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    describeTaskReference,
    executeTaskReferenceAction,
    getTaskReferenceActionLabel,
    getTaskReferenceIconName,
    type TaskReferenceDescriptor,
  } from '../lib/task-reference';
  import {
    getRunnerUserStateLabel,
    getRunnerUserStateTone,
    getRunnerUserStateTooltip,
    getTaskDisplayGoal,
    getTaskDisplayText,
    getTaskDisplayTitle,
    getTaskKindLabel,
    getTaskStatusLabel,
    isUserVisibleTaskKind,
  } from '../lib/task-labels';
  import { resolveAgentDisplayName } from '../lib/agent-role-utils';
  import {
    ensureTaskProjectionState,
    clearTaskProjection,
    fetchTaskProjection,
    getTaskProjectionState,
    getTaskStatusModifier,
    refreshTaskProjection,
    selectTaskProjectionTask,
  } from '../stores/task-projection-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';
  import { vscode } from '../lib/vscode-bridge';

  const TASK_HISTORY_PREVIEW_LIMIT = 5;
  const TASK_HISTORY_REQUEST_LIMIT = 20;

  const appState = getState();
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);

  interface TaskTreeRow {
    task: TaskDto;
    depth: number;
    hasChildren: boolean;
    childCount: number;
    activeChildCount: number;
  }

  interface SelectedTaskReference {
    sourceLabel: string;
    reference: TaskReferenceDescriptor;
  }

  interface TaskReferenceGroup {
    label: string;
    sourceLabel: string;
    references: TaskReferenceDescriptor[];
  }

  interface TaskAttentionSummary {
    title: string;
    hint: string;
  }

  // ─── 任务投影视图 ─────────────────
  const currentSessionId = $derived(appState.currentSessionId);
  const taskProjection = $derived(getTaskProjectionState(currentSessionId));
  const hasTaskProjection = $derived(taskProjection.projection !== null);

  $effect(() => {
    ensureTaskProjectionState(currentSessionId);
  });

  $effect(() => {
    const sid = currentSessionId?.trim() || '';
    taskHistoryFetchGeneration += 1;
    const generation = taskHistoryFetchGeneration;
    taskHistoryRequestScope = sid;
    taskHistoryItems = [];
    taskHistoryError = null;
    taskHistoryExpanded = false;
    if (!sid) {
      taskHistoryLoading = false;
      return;
    }
    void loadSessionTaskHistory(sid, generation);
  });
  const projectionTasks = $derived(taskProjection.projection?.tasks ?? []);
  const taskById = $derived.by(() => new Map(projectionTasks.map((task) => [task.task_id, task])));
  const activeProjectionTasks = $derived.by(() => projectionTasks.filter((task) => task.status !== 'killed'));
  let taskHistoryItems = $state<SessionTaskHistoryItemDto[]>([]);
  let taskHistoryLoading = $state(false);
  let taskHistoryError = $state<string | null>(null);
  let taskHistoryRequestScope = $state('');
  let taskHistoryFetchGeneration = 0;
  let taskHistoryExpanded = $state(false);
  let restartingHistoryRootTaskId = $state<string | null>(null);
  // 记录任务树节点展开状态。
  let expandedProjectionNodes = $state<Set<string>>(new Set());

  // ─── 交付包视图（Long Mission 完成后展示） ─────────────────
  let deliveryPackage = $state<DeliveryPackageDto | null>(null);
  let deliveryPackageLoading = $state(false);
  let deliveryPackageScope = $state('');
  let deliveryPackageRequestScope = $state('');
  let deliverySummaryCopied = $state(false);
  let deliverySummaryCopyTimer: ReturnType<typeof setTimeout> | null = null;
  let taskActionLoading = $state<'stop' | 'resume' | 'restart' | 'archive' | null>(null);
  let selectedTaskReference = $state<SelectedTaskReference | null>(null);
  let referenceDetailEl = $state<HTMLElement | null>(null);
  let referenceSelectionScope = $state('');

  function clearDeliverySummaryCopyTimer() {
    if (deliverySummaryCopyTimer !== null) {
      clearTimeout(deliverySummaryCopyTimer);
      deliverySummaryCopyTimer = null;
    }
  }

  function clearDeliveryPackageViewState() {
    deliveryPackage = null;
    deliverySummaryCopied = false;
    clearDeliverySummaryCopyTimer();
    selectedTaskReference = null;
  }

  onDestroy(() => {
    clearDeliverySummaryCopyTimer();
  });

  const shouldFetchDelivery = $derived.by(() => {
    const proj = taskProjection.projection;
    if (!proj) return false;
    return proj.execution_mode === 'long_mission' && proj.runner_status === 'completed';
  });

  $effect(() => {
    const rootTaskId = taskProjection.projection?.root_task.task_id;
    const sid = currentSessionId?.trim() || '';
    const nextScope = rootTaskId && sid ? `${sid}:${rootTaskId}` : '';

    if (deliveryPackageScope !== nextScope) {
      deliveryPackageScope = nextScope;
      deliveryPackageRequestScope = '';
      deliveryPackageLoading = false;
      clearDeliveryPackageViewState();
    }

    if (!shouldFetchDelivery || !rootTaskId || !sid) {
      if (deliveryPackage || deliverySummaryCopied || selectedTaskReference) {
        clearDeliveryPackageViewState();
      }
      return;
    }
    if (deliveryPackageLoading || deliveryPackage) return;
    deliveryPackageLoading = true;
    deliveryPackageRequestScope = nextScope;
    const client = createClient();
    client.getDeliveryPackage(rootTaskId, sid, currentWorkspaceIdValue())
      .then((pkg) => {
        if (deliveryPackageScope === nextScope && deliveryPackageRequestScope === nextScope) {
          deliveryPackage = pkg;
        }
      })
      .catch((err) => {
        if (deliveryPackageScope === nextScope && deliveryPackageRequestScope === nextScope) {
          console.error('Failed to fetch delivery package:', err);
        }
      })
      .finally(() => {
        if (deliveryPackageRequestScope === nextScope) {
          deliveryPackageRequestScope = '';
          deliveryPackageLoading = false;
        }
      });
  });

  const childrenByParentId = $derived.by(() => {
    const grouped = new Map<string, TaskDto[]>();
    const projection = taskProjection.projection;
    const rootTaskId = projection?.root_task.task_id ?? null;
    const activeTaskIds = new Set(activeProjectionTasks.map((task) => task.task_id));
    for (const task of activeProjectionTasks) {
      if (!task.parent_task_id) continue;
      const displayParentId = resolveVisibleParentTaskId(task, activeTaskIds, rootTaskId);
      if (!displayParentId) continue;
      const siblings = grouped.get(displayParentId) ?? [];
      siblings.push(task);
      grouped.set(displayParentId, siblings);
    }
    for (const [parentId, children] of grouped) {
      const parent = taskById.get(parentId);
      const parentOrder = new Map((parent?.required_children ?? []).map((id, index) => [id, index]));
      children.sort((left, right) => compareTaskSiblings(left, right, parentOrder));
    }
    return grouped;
  });
  const taskTreeRows = $derived.by(() => (
    buildTaskTreeRows(taskProjection.projection?.root_task, childrenByParentId, expandedProjectionNodes)
  ));
  const canResumeTaskProjection = $derived.by(() => {
    const proj = taskProjection.projection;
    return proj?.runner_status === 'error'
      && proj.has_recoverable_chain === true
      && (proj.recoverable_branch_count ?? 0) > 0;
  });
  const canRestartTaskProjection = $derived.by(() => {
    const status = taskProjection.projection?.runner_status;
    return status === 'completed' || status === 'error' || status === 'killed' || status === 'idle';
  });
  const canArchiveTaskProjection = $derived.by(() => canRestartTaskProjection);
  const deliveryFileReferences = $derived.by(() => (
    (deliveryPackage?.file_changes ?? [])
      .map((ref) => describeTaskReference(ref, 'diff'))
      .filter((ref): ref is TaskReferenceDescriptor => Boolean(ref))
  ));
  const deliveryEvidenceReferences = $derived.by(() => (
    (deliveryPackage?.evidence_list ?? [])
      .map((ref) => describeTaskReference(ref, 'auto'))
      .filter((ref): ref is TaskReferenceDescriptor => Boolean(ref))
  ));
  const selectedProjectionTask = $derived.by(() => {
    if (taskProjection.selectedTaskId) {
      const matched = activeProjectionTasks.find((task) => task.task_id === taskProjection.selectedTaskId);
      if (matched) return matched;
    }
    return null;
  });
  const selectedProjectionExecutorDisplayName = $derived.by(() => (
    selectedProjectionTask ? getTaskExecutorDisplayName(selectedProjectionTask) : ''
  ));
  const selectedProjectionReferenceGroups = $derived.by(() => buildTaskReferenceGroups(selectedProjectionTask));
  const visibleTaskHistoryItems = $derived.by(() => {
    const activeRootTaskId = taskProjection.projection?.root_task.task_id ?? null;
    if (!activeRootTaskId) return taskHistoryItems;
    return taskHistoryItems.filter((item) => item.rootTask.task_id !== activeRootTaskId);
  });
  const displayedTaskHistoryItems = $derived.by(() => (
    taskHistoryExpanded
      ? visibleTaskHistoryItems
      : visibleTaskHistoryItems.slice(0, TASK_HISTORY_PREVIEW_LIMIT)
  ));
  const hiddenTaskHistoryCount = $derived(Math.max(
    0,
    visibleTaskHistoryItems.length - displayedTaskHistoryItems.length,
  ));
  const hasVisibleTaskHistory = $derived(visibleTaskHistoryItems.length > 0);
  const attentionTasks = $derived.by(() => {
    const projection = taskProjection.projection;
    if (!projection) return [];
    const ids = projection.failed_tasks ?? [];
    const seen = new Set<string>();
    return ids
      .filter((id) => {
        if (seen.has(id)) return false;
        seen.add(id);
        return true;
      })
      .map((id) => taskById.get(id))
      .filter((task): task is TaskDto => Boolean(task));
  });
  const attentionSummary = $derived.by(() => buildTaskAttentionSummary(
    taskProjection.projection,
    attentionTasks,
    canResumeTaskProjection,
  ));
  const runnerBlockedReason = $derived(attentionSummary?.title ?? null);
  // 用户面展示实际工作单元；root 只承担编排时不作为同级任务重复罗列。
  const userVisibleTasks = $derived.by(() => (
    activeProjectionTasks
      .filter((task) => isUserVisibleTaskKind(task.kind))
      .filter((task) => !isCoordinationEnvelopeRoot(task, taskProjection.projection))
      .slice()
      .sort((left, right) => {
        if (left.created_at !== right.created_at) return left.created_at - right.created_at;
        return left.task_id.localeCompare(right.task_id);
      })
  ));
  const taskSummary = $derived.by(() => buildTaskSummary(taskProjection.projection, userVisibleTasks));

  function getTaskExecutorDisplayName(task: TaskDto): string {
    const roleId = task.executor_binding?.target_role?.trim() ?? '';
    if (!roleId) return '';
    return resolveAgentDisplayName(roleId, enabledAgents, registrySnapshot, (key) => i18n.t(key)) || roleId;
  }

  function getTaskPerformerLabel(task: TaskDto): string {
    const executorName = getTaskExecutorDisplayName(task);
    if (executorName) return executorName;
    switch (task.kind) {
      case 'local_workflow': return '流程';
      case 'remote_agent': return '远程';
      case 'monitor_mcp': return 'MCP';
      case 'in_process_teammate': return '队友';
      case 'dream': return '后台';
      default: return '代理';
    }
  }

  function buildTaskReferences(
    refs: string[],
    preferredAction: 'auto' | 'file' | 'diff',
  ): TaskReferenceDescriptor[] {
    return refs
      .map((ref) => describeTaskReference(ref, preferredAction))
      .filter((ref): ref is TaskReferenceDescriptor => Boolean(ref));
  }

  function buildTaskReferenceGroups(task: TaskDto | null): TaskReferenceGroup[] {
    if (!task) return [];
    return [
      { label: '知识', sourceLabel: '任务详情 · 知识', refs: task.knowledge_refs, preferredAction: 'auto' as const },
      { label: '输入', sourceLabel: '任务详情 · 输入', refs: task.input_refs, preferredAction: 'auto' as const },
      { label: '产出', sourceLabel: '任务详情 · 产出', refs: task.output_refs, preferredAction: 'auto' as const },
      { label: '证据', sourceLabel: '任务详情 · 证据', refs: task.evidence_refs, preferredAction: 'auto' as const },
    ]
      .map((group) => ({
        label: group.label,
        sourceLabel: group.sourceLabel,
        references: buildTaskReferences(group.refs, group.preferredAction),
      }))
      .filter((group) => group.references.length > 0);
  }

  function getTaskLineageLabel(task: TaskDto): string {
    const lineage: string[] = [];
    const seen = new Set<string>();
    let current = task;
    while (current.parent_task_id && !seen.has(current.task_id)) {
      seen.add(current.task_id);
      const parent = taskById.get(current.parent_task_id);
      if (!parent) break;
      lineage.unshift(getTaskDisplayTitle(parent));
      current = parent;
    }
    return lineage.length > 0 ? lineage.join(' / ') : '根任务';
  }

  function resolveVisibleParentTaskId(
    task: TaskDto,
    activeTaskIds: Set<string>,
    rootTaskId: string | null,
  ): string | null {
    let parentId = task.parent_task_id ?? null;
    const visited = new Set<string>([task.task_id]);
    while (parentId) {
      if (activeTaskIds.has(parentId)) return parentId;
      if (visited.has(parentId)) return rootTaskId;
      visited.add(parentId);
      parentId = taskById.get(parentId)?.parent_task_id ?? null;
    }
    return rootTaskId;
  }

  function toggleProjectionNode(taskId: string) {
    const next = new Set(expandedProjectionNodes);
    if (next.has(taskId)) next.delete(taskId);
    else next.add(taskId);
    expandedProjectionNodes = next;
  }

  function compareTaskSiblings(left: TaskDto, right: TaskDto, parentOrder: Map<string, number>): number {
    const leftOrder = parentOrder.get(left.task_id) ?? Number.MAX_SAFE_INTEGER;
    const rightOrder = parentOrder.get(right.task_id) ?? Number.MAX_SAFE_INTEGER;
    if (leftOrder !== rightOrder) return leftOrder - rightOrder;
    if (left.created_at !== right.created_at) return left.created_at - right.created_at;
    return left.task_id.localeCompare(right.task_id);
  }

  function buildTaskTreeRows(
    rootTask: TaskDto | null | undefined,
    childrenByParentId: Map<string, TaskDto[]>,
    expandedNodeIds: Set<string>,
  ): TaskTreeRow[] {
    if (!rootTask) return [];
    const rows: TaskTreeRow[] = [];
    const visit = (task: TaskDto, depth: number) => {
      const children = childrenByParentId.get(task.task_id) ?? [];
      const activeChildren = children.filter((child) => child.status !== 'killed');
      rows.push({
        task,
        depth,
        hasChildren: children.length > 0,
        childCount: children.length,
        activeChildCount: activeChildren.length,
      });
      if (children.length > 0 && expandedNodeIds.has(task.task_id)) {
        for (const child of children) visit(child, depth + 1);
      }
    };
    visit(rootTask, 0);
    return rows;
  }

  function isCoordinationEnvelopeRoot(
    task: TaskDto,
    projection: TaskProjectionDto | null,
  ): boolean {
    if (!projection || task.task_id !== projection.root_task.task_id) {
      return false;
    }
    return (childrenByParentId.get(task.task_id) ?? [])
      .filter((child) => child.status !== 'killed')
      .some((child) => isUserVisibleTaskKind(child.kind));
  }

  function settledChildCount(taskId: string): number {
    return (childrenByParentId.get(taskId) ?? [])
      .filter((child) => child.status !== 'killed')
      .filter((child) => child.status === 'completed' || child.status === 'killed')
      .length;
  }

  function buildTaskSummary(
    projection: TaskProjectionDto | null,
    visibleTasks: TaskDto[],
  ) {
    if (visibleTasks.length > 0) {
      return {
        total: visibleTasks.length,
        completed: visibleTasks.filter((task) => task.status === 'completed').length,
      };
    }
    const progress = projection?.progress_summary;
    return {
      total: progress?.total_tasks ?? 0,
      completed: progress?.completed_tasks ?? 0,
    };
  }

  function buildTaskAttentionSummary(
    projection: TaskProjectionDto | null,
    failedTasks: TaskDto[],
    canResume: boolean,
  ): TaskAttentionSummary | null {
    if (!projection) return null;
    const failedCount = failedTasks.length;
    if (projection.runner_status !== 'error' && failedCount === 0) return null;

    const rootTaskId = projection.root_task.task_id;
    const rootFailed = failedTasks.some((task) => task.task_id === rootTaskId);
    const agentFailedCount = failedTasks
      .filter((task) => task.kind === 'local_agent' && task.task_id !== rootTaskId)
      .length;

    let title = '执行链未完成';
    if (rootFailed && agentFailedCount > 0) {
      title = `主线和 ${agentFailedCount} 个代理未完成`;
    } else if (rootFailed) {
      title = '主线任务未完成';
    } else if (agentFailedCount > 0 && agentFailedCount === failedCount) {
      title = `${agentFailedCount} 个代理未完成`;
    } else if (failedCount > 0) {
      title = `${failedCount} 个任务未完成`;
    }

    return {
      title,
      hint: canResume
        ? '可以继续原执行链；如果上下文已失效，再重新执行。'
        : '可以重新执行当前目标；需要定位链路时再展开排障明细。',
    };
  }

  function appendDeliverySummaryList(
    lines: string[],
    label: string,
    items: string[],
    limit = 10,
  ) {
    if (items.length === 0) return;
    lines.push('', `${label}（${items.length}）`);
    for (const item of items.slice(0, limit)) {
      lines.push(`- ${item}`);
    }
    const remaining = items.length - limit;
    if (remaining > 0) {
      lines.push(`- 另有 ${remaining} 项`);
    }
  }

  function buildDeliveryPackageSummary(pkg: DeliveryPackageDto): string {
    const total = pkg.progress.total || 0;
    const completed = pkg.progress.completed || 0;
    const percent = total > 0 ? Math.round((completed / total) * 100) : 0;
    const lines = [
      '交付概览',
      `目标：${getTaskDisplayText(pkg.goal) || '--'}`,
      `状态：${getTaskStatusLabel(pkg.aggregate_status as TaskStatus)}`,
      `进度：${completed}/${total}（${percent}%）`,
      `执行态：待执行 ${pkg.progress.pending || 0} · 执行中 ${pkg.progress.running || 0} · 失败 ${pkg.progress.failed || 0} · 终止 ${pkg.progress.killed || 0}`,
      `完成任务：${pkg.completed_task_count}`,
      `资产：文件 ${pkg.file_changes.length} · 证据 ${pkg.evidence_list.length} · 核验 ${pkg.verification_results.length} · 执行记录 ${pkg.execution_records.length} · 风险 ${pkg.remaining_risks.length}`,
    ].filter((line): line is string => Boolean(line));

    appendDeliverySummaryList(lines, '文件变更', pkg.file_changes);
    appendDeliverySummaryList(lines, '证据', pkg.evidence_list);

    if (pkg.verification_results.length > 0) {
      lines.push('', `核验结果（${pkg.verification_results.length}）`);
      for (const result of pkg.verification_results.slice(0, 10)) {
        lines.push(`- ${getTaskDisplayText(result.title)}: ${getTaskDisplayText(result.result)}`);
      }
    }

    appendDeliverySummaryList(lines, '剩余风险', pkg.remaining_risks.map(getTaskDisplayText), 8);
    return lines.join('\n');
  }

  // 自动展开根节点和活跃分支，确保任务树能直接反映执行状态。
  $effect(() => {
    const projection = taskProjection.projection;
    if (!projection) return;
    const next = new Set(expandedProjectionNodes);
    let changed = false;
    const expand = (taskId: string) => {
      if (!next.has(taskId)) {
        next.add(taskId);
        changed = true;
      }
    };

    expand(projection.root_task.task_id);
    const visibleTaskIds = [
      ...projection.pending_tasks,
      ...projection.running_tasks,
      ...projection.failed_tasks,
    ];
    for (const taskId of visibleTaskIds) {
      if ((childrenByParentId.get(taskId)?.length ?? 0) > 0) {
        expand(taskId);
      }
      let current = taskById.get(taskId);
      while (current?.parent_task_id) {
        expand(current.parent_task_id);
        current = taskById.get(current.parent_task_id);
      }
    }
    if (changed) expandedProjectionNodes = next;
  });

  $effect(() => {
    const nextScope = taskProjection.projection
      ? `${currentSessionId ?? ''}:${taskProjection.projection.root_task.task_id}`
      : '';
    if (referenceSelectionScope !== nextScope) {
      referenceSelectionScope = nextScope;
      selectedTaskReference = null;
      selectTaskProjectionTask(currentSessionId, null);
    }
  });

  function getProjectionStatusIcon(status: TaskStatus): { name: IconName; spinning: boolean } {
    switch (status) {
      case 'running': return { name: 'loader', spinning: true };
      case 'completed': return { name: 'check-circle', spinning: false };
      case 'failed': return { name: 'x-circle', spinning: false };
      case 'killed': return { name: 'skip-forward', spinning: false };
      case 'pending': return { name: 'circleOutline', spinning: false };
      default: return { name: 'circleOutline', spinning: false };
    }
  }

  function canRestartHistoryItem(item: SessionTaskHistoryItemDto): boolean {
    return item.restartable && item.rootTask.status !== 'running' && item.rootTask.status !== 'pending';
  }

  function createClient(): RustDaemonClient {
    return new RustDaemonClient(resolveAgentBaseUrl());
  }

  function currentWorkspaceIdValue(): string {
    return typeof messagesState.currentWorkspaceId === 'string'
      ? messagesState.currentWorkspaceId.trim()
      : '';
  }

  async function loadSessionTaskHistory(
    sessionId: string,
    generation = taskHistoryFetchGeneration,
  ) {
    const sid = sessionId.trim();
    if (!sid) return;
    taskHistoryLoading = true;
    taskHistoryError = null;
    const client = createClient();
    try {
      const response = await client.getSessionTaskHistory(sid, TASK_HISTORY_REQUEST_LIMIT, currentWorkspaceIdValue());
      if (taskHistoryRequestScope !== sid || taskHistoryFetchGeneration !== generation) {
        return;
      }
      taskHistoryItems = response.items;
    } catch (err) {
      if (taskHistoryRequestScope !== sid || taskHistoryFetchGeneration !== generation) {
        return;
      }
      taskHistoryError = err instanceof Error ? err.message : String(err);
    } finally {
      if (taskHistoryRequestScope === sid && taskHistoryFetchGeneration === generation) {
        taskHistoryLoading = false;
      }
    }
  }

  async function refreshSessionTaskHistory() {
    const sessionId = currentSessionIdValue();
    if (!sessionId) return;
    taskHistoryFetchGeneration += 1;
    await loadSessionTaskHistory(sessionId, taskHistoryFetchGeneration);
  }

  function currentRootTaskId(): string | null {
    return taskProjection.projection?.root_task.task_id ?? null;
  }

  function currentSessionIdValue(): string | null {
    const sessionId = currentSessionId?.trim();
    return sessionId || null;
  }

  async function runTaskAction(
    action: 'stop' | 'resume' | 'restart' | 'archive',
    task: () => Promise<void>,
  ) {
    if (taskActionLoading) return;
    taskActionLoading = action;
    try {
      await task();
    } finally {
      if (taskActionLoading === action) {
        taskActionLoading = null;
      }
    }
  }

  async function stopCurrentTaskProjection() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('stop', async () => {
      const client = createClient();
      await client.interruptTask({ taskId: rootTaskId, sessionId, workspaceId: currentWorkspaceIdValue() });
      clearDeliveryPackageViewState();
      await refreshTaskProjection(sessionId);
      addToast('info', '任务已中断，进度已保留');
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `停止失败: ${message}`);
    });
  }

  async function resumeCurrentTaskProjection() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('resume', async () => {
      const client = createClient();
      await client.continueSession({ sessionId, workspaceId: currentWorkspaceIdValue() });
      clearDeliveryPackageViewState();
      await refreshTaskProjection(sessionId);
      addToast('success', '任务已继续');
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `恢复失败: ${message}`);
    });
  }

  async function restartCurrentTaskProjection() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('restart', async () => {
      const client = createClient();
      const result = await client.restartTask({ taskId: rootTaskId, sessionId, workspaceId: currentWorkspaceIdValue() });
      clearDeliveryPackageViewState();
      if (result.rootTaskId) {
        await fetchTaskProjection(sessionId, result.rootTaskId);
      } else {
        await refreshTaskProjection(sessionId);
      }
      await refreshSessionTaskHistory();
      addToast('success', '任务已重新执行');
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `重新执行失败: ${message}`);
    });
  }

  async function archiveCurrentTaskProjection() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('archive', async () => {
      const client = createClient();
      await client.archiveTask({ taskId: rootTaskId, sessionId, workspaceId: currentWorkspaceIdValue() });
      clearDeliveryPackageViewState();
      clearTaskProjection(sessionId, rootTaskId);
      await refreshSessionTaskHistory();
      addToast('info', '任务已从面板移除');
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `移除失败: ${message}`);
    });
  }

  async function restartHistoryTask(rootTaskId: string) {
    const sessionId = currentSessionIdValue();
    if (!sessionId || restartingHistoryRootTaskId) return;
    restartingHistoryRootTaskId = rootTaskId;
    try {
      const client = createClient();
      const result = await client.restartTask({ taskId: rootTaskId, sessionId, workspaceId: currentWorkspaceIdValue() });
      clearDeliveryPackageViewState();
      if (result.rootTaskId) {
        await fetchTaskProjection(sessionId, result.rootTaskId);
      } else {
        await refreshTaskProjection(sessionId);
      }
      await refreshSessionTaskHistory();
      addToast('success', '任务已重新执行');
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `重新执行失败: ${message}`);
    } finally {
      if (restartingHistoryRootTaskId === rootTaskId) {
        restartingHistoryRootTaskId = null;
      }
    }
  }

  async function copyDeliveryPackageSummary() {
    if (!deliveryPackage) return;
    try {
      await navigator.clipboard.writeText(buildDeliveryPackageSummary(deliveryPackage));
      deliverySummaryCopied = true;
      clearDeliverySummaryCopyTimer();
      deliverySummaryCopyTimer = setTimeout(() => {
        deliverySummaryCopied = false;
        deliverySummaryCopyTimer = null;
      }, 1800);
      addToast('info', '交付摘要已复制');
    } catch {
      addToast('error', '复制交付摘要失败');
    }
  }

  function taskReferenceKey(sourceLabel: string, reference: TaskReferenceDescriptor): string {
    return [
      sourceLabel,
      reference.raw,
      reference.actionKind,
      reference.actionTarget,
    ].join('\u0000');
  }

  function isTaskReferenceSelected(sourceLabel: string, reference: TaskReferenceDescriptor): boolean {
    if (!selectedTaskReference) return false;
    return taskReferenceKey(selectedTaskReference.sourceLabel, selectedTaskReference.reference)
      === taskReferenceKey(sourceLabel, reference);
  }

  function selectTaskReference(sourceLabel: string, reference: TaskReferenceDescriptor) {
    selectedTaskReference = { sourceLabel, reference };
    requestAnimationFrame(() => {
      referenceDetailEl?.scrollIntoView({ block: 'nearest' });
    });
  }

  function selectProjectionTask(taskId: string) {
    selectTaskProjectionTask(currentSessionId, taskId);
    if (selectedTaskReference?.sourceLabel.startsWith('任务详情 ·')) {
      selectedTaskReference = null;
    }
  }

  async function executeSelectedTaskReference() {
    if (!selectedTaskReference) return;
    await activateTaskReference(selectedTaskReference.reference);
  }

  async function activateTaskReference(reference: TaskReferenceDescriptor) {
    await executeTaskReferenceAction(reference, {
      sessionId: currentSessionIdValue(),
      postMessage: (message) => vscode.postMessage(message),
      writeClipboard: (text) => navigator.clipboard.writeText(text),
      onCopySuccess: () => addToast('info', '引用已复制'),
      onCopyFailure: () => addToast('error', '复制引用失败'),
    });
  }

  function formatTimestamp(timestamp?: number): string {
    if (typeof timestamp !== 'number' || !Number.isFinite(timestamp) || timestamp <= 0) {
      return '--';
    }
    const date = new Date(timestamp);
    const hours = String(date.getHours()).padStart(2, '0');
    const minutes = String(date.getMinutes()).padStart(2, '0');
    return `${hours}:${minutes}`;
  }

</script>

<div class="panel-content-scrollable tasks-panel">
  {#if hasTaskProjection}
    {@const proj = taskProjection.projection}
    {#if proj}
      <section class="task-progress-panel" aria-label="任务进度">
        <div class="task-progress-head">
          <div class="task-progress-title-block">
            <span class="task-progress-label">进度</span>
            {#if taskSummary.total > 0}
              <span class="task-progress-meta">{taskSummary.completed}/{taskSummary.total} 已完成</span>
            {/if}
          </div>
          <div class="task-progress-actions">
            <span
              class="tg-status-badge tg-status--{getRunnerUserStateTone(proj.runner_status)}"
              title={getRunnerUserStateTooltip(proj.runner_status, runnerBlockedReason) ?? ''}
            >
              {getRunnerUserStateLabel(proj.runner_status)}
            </span>
            {#if proj.runner_status === 'running'}
              <button
                type="button"
                class="task-action-btn"
                disabled={taskActionLoading !== null}
                onclick={stopCurrentTaskProjection}
                title="停止当前任务，保留进度"
              >
                <Icon name={taskActionLoading === 'stop' ? 'loader' : 'stop'} size={12} class={taskActionLoading === 'stop' ? 'spinning' : ''} />
                <span>停止</span>
              </button>
            {:else if canResumeTaskProjection}
              <button
                type="button"
                class="task-action-btn"
                disabled={taskActionLoading !== null}
                onclick={resumeCurrentTaskProjection}
                title="继续任务"
              >
                <Icon name={taskActionLoading === 'resume' ? 'loader' : 'play'} size={12} class={taskActionLoading === 'resume' ? 'spinning' : ''} />
                <span>继续</span>
              </button>
            {/if}
            {#if canRestartTaskProjection}
              <button
                type="button"
                class="task-action-btn"
                disabled={taskActionLoading !== null}
                onclick={restartCurrentTaskProjection}
                title="基于当前目标创建新的执行链"
              >
                <Icon name={taskActionLoading === 'restart' ? 'loader' : 'refresh'} size={12} class={taskActionLoading === 'restart' ? 'spinning' : ''} />
                <span>重新执行</span>
              </button>
            {/if}
            {#if canArchiveTaskProjection}
              <button
                type="button"
                class="task-action-btn task-action-btn--quiet"
                disabled={taskActionLoading !== null}
                onclick={archiveCurrentTaskProjection}
                title="从当前面板移除，保留任务历史"
              >
                <Icon name={taskActionLoading === 'archive' ? 'loader' : 'eye-slash'} size={12} class={taskActionLoading === 'archive' ? 'spinning' : ''} />
                <span>移除</span>
              </button>
            {/if}
          </div>
        </div>

        {#if attentionSummary}
          <div class="task-attention-strip">
            <Icon name="alert-triangle" size={13} />
            <span class="task-attention-strip-copy">
              <strong>{attentionSummary.title}</strong>
              <span>{attentionSummary.hint}</span>
            </span>
          </div>
        {/if}

        {#if userVisibleTasks.length > 0}
          <div class="task-progress-rows" role="list">
            {#each userVisibleTasks as task (task.task_id)}
              {@const statusIcon = getProjectionStatusIcon(task.status)}
              {@const performerLabel = getTaskPerformerLabel(task)}
              <div
                role="listitem"
                class="task-progress-row task-progress-row--{getTaskStatusModifier(task.status)}"
                title={`${getTaskDisplayTitle(task)} · ${performerLabel}`}
              >
                <span class="task-progress-status tg-status-icon--{getTaskStatusModifier(task.status)}" aria-label={getTaskStatusLabel(task.status)}>
                  {#if statusIcon.spinning}
                    <Icon name={statusIcon.name} size={16} class="spinning" />
                  {:else}
                    <Icon name={statusIcon.name} size={16} />
                  {/if}
                </span>
                <span class="task-progress-task">{getTaskDisplayTitle(task)}</span>
                <span class="task-progress-performer">{performerLabel}</span>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      {#if deliveryPackage}
        <section class="delivery-package-card" aria-label="交付概览">
          <div class="dp-header">
            <Icon name="taskComplete" size={14} />
            <span class="dp-title">交付概览</span>
            <button
              type="button"
              class="dp-summary-copy"
              title={deliverySummaryCopied ? '交付摘要已复制' : '复制交付摘要'}
              onclick={copyDeliveryPackageSummary}
            >
              <Icon name={deliverySummaryCopied ? 'check' : 'copy'} size={11} />
              <span>{deliverySummaryCopied ? '已复制' : '复制摘要'}</span>
            </button>
          </div>

          {#if deliveryPackage.file_changes.length > 0}
            <div class="dp-section">
              <span class="dp-section-label">文件变更 ({deliveryPackage.file_changes.length})</span>
              <div class="dp-chip-list">
                {#each deliveryFileReferences as reference, index (`${reference.raw}:${index}`)}
                  <button
                    type="button"
                    class="dp-chip dp-chip--interactive"
                    class:dp-chip--selected={isTaskReferenceSelected('交付 · 文件变更', reference)}
                    title={reference.title}
                    onclick={() => selectTaskReference('交付 · 文件变更', reference)}
                  >
                    <Icon name={getTaskReferenceIconName(reference)} size={11} />
                    <span>{reference.displayLabel}</span>
                  </button>
                {/each}
              </div>
            </div>
          {/if}

          {#if deliveryPackage.evidence_list.length > 0}
            <div class="dp-section">
              <span class="dp-section-label">证据 ({deliveryPackage.evidence_list.length})</span>
              <div class="dp-chip-list">
                {#each deliveryEvidenceReferences as reference, index (`${reference.raw}:${index}`)}
                  <button
                    type="button"
                    class="dp-chip dp-chip--interactive"
                    class:dp-chip--selected={isTaskReferenceSelected('交付 · 证据', reference)}
                    title={reference.title}
                    onclick={() => selectTaskReference('交付 · 证据', reference)}
                  >
                    <Icon name={getTaskReferenceIconName(reference)} size={11} />
                    <span>{reference.displayLabel}</span>
                  </button>
                {/each}
              </div>
            </div>
          {/if}

          {#if deliveryPackage.verification_results.length > 0}
            <div class="dp-section">
              <span class="dp-section-label">核验结果 ({deliveryPackage.verification_results.length})</span>
              {#each deliveryPackage.verification_results as vr}
                <div class="dp-verification-row">
                  <Icon name="check-circle" size={12} />
                  <span class="dp-verification-title">{getTaskDisplayText(vr.title)}</span>
                  <span class="dp-verification-result">{getTaskDisplayText(vr.result)}</span>
                </div>
              {/each}
            </div>
          {/if}

          {#if deliveryPackage.remaining_risks.length > 0}
            <div class="dp-section">
              <span class="dp-section-label">剩余风险 ({deliveryPackage.remaining_risks.length})</span>
              {#each deliveryPackage.remaining_risks as risk}
                <div class="dp-risk-row">
                  <Icon name="alert-triangle" size={12} />
                  <span>{getTaskDisplayText(risk)}</span>
                </div>
              {/each}
            </div>
          {/if}

          <div class="dp-progress">
            <span class="dp-progress-label">完成度</span>
            <div class="dp-progress-bar">
              <div class="dp-progress-fill" style="width: {Math.round(((deliveryPackage.progress.completed || 0) / (deliveryPackage.progress.total || 1)) * 100)}%"></div>
            </div>
            <span class="dp-progress-value">{deliveryPackage.progress.completed}/{deliveryPackage.progress.total || 1}</span>
          </div>
        </section>
      {/if}

      {#if selectedTaskReference}
        <section bind:this={referenceDetailEl} class="task-reference-detail-card" aria-label="引用详情">
          <div class="task-reference-detail-top">
            <span class="task-reference-detail-title">
              <Icon name={getTaskReferenceIconName(selectedTaskReference.reference)} size={12} />
              <span>{selectedTaskReference.reference.displayLabel}</span>
            </span>
            <button
              type="button"
              class="task-reference-detail-close"
              title="关闭引用详情"
              onclick={() => selectedTaskReference = null}
            >
              <Icon name="close" size={12} />
            </button>
          </div>
          <div class="task-reference-detail-meta">
            <span>来源：{selectedTaskReference.sourceLabel}</span>
            <span>动作：{getTaskReferenceActionLabel(selectedTaskReference.reference)}</span>
          </div>
          <div class="task-reference-detail-field">
            <span class="task-reference-detail-label">目标</span>
            <span class="task-reference-detail-value task-reference-detail-value--mono" title={selectedTaskReference.reference.actionTarget}>
              {selectedTaskReference.reference.actionTarget}
            </span>
          </div>
          {#if selectedTaskReference.reference.raw !== selectedTaskReference.reference.actionTarget}
            <div class="task-reference-detail-field">
              <span class="task-reference-detail-label">原始引用</span>
              <span class="task-reference-detail-value task-reference-detail-value--mono" title={selectedTaskReference.reference.raw}>
                {selectedTaskReference.reference.raw}
              </span>
            </div>
          {/if}
          <div class="task-reference-detail-actions">
            <button type="button" class="task-reference-detail-action" onclick={executeSelectedTaskReference}>
              <Icon name={getTaskReferenceIconName(selectedTaskReference.reference)} size={12} />
              <span>{getTaskReferenceActionLabel(selectedTaskReference.reference)}</span>
            </button>
          </div>
        </section>
      {/if}

      <details class="task-details-disclosure">
        <summary>
          <span>排障明细</span>
          <span>展开查看任务链路</span>
        </summary>

      <div class="tg-tree" role="tree" aria-label="任务排障明细">
        {#each taskTreeRows as row (row.task.task_id)}
          {@const isExpanded = expandedProjectionNodes.has(row.task.task_id)}
          {@const statusIcon = getProjectionStatusIcon(row.task.status)}
          <div
            class="tg-tree-row tg-tree-row--{getTaskStatusModifier(row.task.status)}"
            class:tg-tree-row--selected={selectedProjectionTask?.task_id === row.task.task_id}
            role="treeitem"
            aria-level={row.depth + 1}
            aria-expanded={row.hasChildren ? isExpanded : undefined}
            aria-selected={selectedProjectionTask?.task_id === row.task.task_id}
            style={`--task-indent: ${row.depth * 18}px;`}
          >
            {#if row.hasChildren}
              <button
                type="button"
                class="tg-tree-toggle"
                class:expanded={isExpanded}
                aria-label={isExpanded ? '折叠任务' : '展开任务'}
                onclick={() => toggleProjectionNode(row.task.task_id)}
              >
                <Icon name="chevron-right" size={12} />
              </button>
            {:else}
              <span class="tg-tree-toggle tg-tree-toggle--empty" aria-hidden="true"></span>
            {/if}
            <span class="tg-kind-badge">{getTaskKindLabel(row.task.kind)}</span>
            <span class="tg-tree-status-icon tg-status-icon--{getTaskStatusModifier(row.task.status)}">
              {#if statusIcon.spinning}
                <Icon name={statusIcon.name} size={14} class="spinning" />
              {:else}
                <Icon name={statusIcon.name} size={14} />
              {/if}
            </span>
            <span class="tg-tree-content">
              <span class="tg-tree-title">{getTaskDisplayTitle(row.task)}</span>
              {#if getTaskDisplayGoal(row.task) && getTaskDisplayGoal(row.task) !== getTaskDisplayTitle(row.task)}
                <span class="tg-tree-goal">{getTaskDisplayGoal(row.task)}</span>
              {/if}
            </span>
            <span class="tg-tree-side">
              <span class="tg-tree-state">{getTaskStatusLabel(row.task.status)}</span>
              {#if row.task.kind === 'local_agent'}
                {#if row.activeChildCount > 0}
                  <span class="tg-tree-count">{settledChildCount(row.task.task_id)}/{row.activeChildCount}</span>
                {:else if row.childCount > 0}
                  <span class="tg-tree-count">0</span>
                {/if}
              {:else if row.childCount > 0}
                <span class="tg-tree-count">{row.childCount}</span>
              {/if}
              <button
                type="button"
                class="tg-tree-detail-btn"
                title="查看任务详情"
                aria-label="查看任务详情"
                onclick={() => selectProjectionTask(row.task.task_id)}
              >
                <Icon name="info" size={11} />
              </button>
            </span>
          </div>
        {/each}
      </div>

      {#if selectedProjectionTask}
        <section class="task-detail-card" aria-label="任务详情">
          <div class="task-detail-top">
            <span class="task-detail-title">{getTaskDisplayTitle(selectedProjectionTask)}</span>
            <div class="task-detail-actions">
              <span class="task-detail-status tg-status--{getTaskStatusModifier(selectedProjectionTask.status)}">
                {getTaskStatusLabel(selectedProjectionTask.status)}
              </span>
              <button
                type="button"
                class="task-detail-close"
                title="关闭任务详情"
                aria-label="关闭任务详情"
                onclick={() => selectTaskProjectionTask(currentSessionId, null)}
              >
                <Icon name="close" size={11} />
              </button>
            </div>
          </div>
          {#if getTaskDisplayGoal(selectedProjectionTask) && getTaskDisplayGoal(selectedProjectionTask) !== getTaskDisplayTitle(selectedProjectionTask)}
            <p class="task-detail-goal">{getTaskDisplayGoal(selectedProjectionTask)}</p>
          {/if}
          <div class="task-detail-meta">
            <span>{getTaskKindLabel(selectedProjectionTask.kind)}</span>
            <span>路径：{getTaskLineageLabel(selectedProjectionTask)}</span>
            {#if selectedProjectionExecutorDisplayName}
              <span>执行者：{selectedProjectionExecutorDisplayName}</span>
            {/if}
            {#if selectedProjectionTask.workspace_scope}
              <span>工作区：{selectedProjectionTask.workspace_scope}</span>
            {/if}
            {#if selectedProjectionTask.write_scope}
              <span>写入范围：{selectedProjectionTask.write_scope}</span>
            {/if}
            {#if selectedProjectionTask.retry_count > 0}
              <span>重试 {selectedProjectionTask.retry_count}</span>
            {/if}
          </div>
          {#if selectedProjectionTask.status === 'failed'}
            <div class="task-detail-blocker">
              <Icon name="alert-circle" size={12} />
              <span>这个任务没有完成。优先回到进度区继续或重新执行；需要定位链路时再查看这里的父子任务关系。</span>
            </div>
          {/if}
          {#if selectedProjectionReferenceGroups.length > 0}
            <div class="task-detail-reference-groups">
              {#each selectedProjectionReferenceGroups as group (group.label)}
                <div class="task-detail-reference-group">
                  <span class="task-detail-reference-label">{group.label}</span>
                  <div class="task-detail-reference-list">
                    {#each group.references as reference, index (`${group.label}-${reference.raw}:${index}`)}
                      <button
                        type="button"
                        class="task-detail-reference-chip"
                        class:task-detail-reference-chip--selected={isTaskReferenceSelected(group.sourceLabel, reference)}
                        title={reference.title}
                        onclick={() => selectTaskReference(group.sourceLabel, reference)}
                      >
                        <Icon name={getTaskReferenceIconName(reference)} size={10} />
                        <span>{reference.displayLabel}</span>
                      </button>
                    {/each}
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        </section>
      {/if}
      </details>

    {/if}
  {/if}

  {#if hasVisibleTaskHistory}
    <section class="task-history-card" aria-label="最近任务">
      <div class="task-history-header">
        <span class="task-history-title">
          <Icon name="clock" size={12} />
          <span>最近任务</span>
        </span>
        <span class="task-history-count">
          {#if hiddenTaskHistoryCount > 0}
            最近 {displayedTaskHistoryItems.length} / {visibleTaskHistoryItems.length}
          {:else}
            {visibleTaskHistoryItems.length} 个任务
          {/if}
        </span>
      </div>

      <div class="task-history-list" role="list">
        {#each displayedTaskHistoryItems as item (item.rootTask.task_id)}
          {@const task = item.rootTask}
          {@const statusIcon = getProjectionStatusIcon(task.status)}
          {@const performerLabel = getTaskPerformerLabel(task)}
          <div
            role="listitem"
            class="task-history-row"
            title={`${getTaskDisplayTitle(task)} · ${performerLabel}`}
          >
            <span class="task-progress-status tg-status-icon--{getTaskStatusModifier(task.status)}" aria-label={getTaskStatusLabel(task.status)}>
              {#if statusIcon.spinning}
                <Icon name={statusIcon.name} size={15} class="spinning" />
              {:else}
                <Icon name={statusIcon.name} size={15} />
              {/if}
            </span>
            <span class="task-history-main">
              <span class="task-history-row-title">{getTaskDisplayTitle(task)}</span>
              <span class="task-history-row-meta">
                {performerLabel} · {getTaskStatusLabel(task.status)} · {formatTimestamp(item.updatedAt)}
              </span>
            </span>
            <span class="task-history-side">
              <span
                class="tg-status-badge tg-status--{getRunnerUserStateTone(item.runnerStatus)}"
                title={item.displayStatus}
              >
                {getRunnerUserStateLabel(item.runnerStatus)}
              </span>
              {#if canRestartHistoryItem(item)}
                <button
                  type="button"
                  class="task-action-btn"
                  disabled={restartingHistoryRootTaskId !== null}
                  onclick={() => restartHistoryTask(task.task_id)}
                  title="基于这个任务目标重新执行"
                >
                  <Icon
                    name={restartingHistoryRootTaskId === task.task_id ? 'loader' : 'refresh'}
                    size={12}
                    class={restartingHistoryRootTaskId === task.task_id ? 'spinning' : ''}
                  />
                  <span>重新执行</span>
                </button>
              {/if}
            </span>
          </div>
        {/each}
      </div>

      {#if hiddenTaskHistoryCount > 0 || taskHistoryExpanded}
        <button
          type="button"
          class="task-history-toggle"
          onclick={() => {
            taskHistoryExpanded = !taskHistoryExpanded;
          }}
          aria-expanded={taskHistoryExpanded}
        >
          <Icon name={taskHistoryExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
          <span>{taskHistoryExpanded ? '收起历史任务' : `展开 ${hiddenTaskHistoryCount} 个历史任务`}</span>
        </button>
      {/if}
    </section>
  {/if}

  {#if taskProjection.error}
    <div class="tg-error">{i18n.t('tasks.projectionLoadFailed')}</div>
  {/if}

  {#if taskHistoryError}
    <div class="tg-error">{taskHistoryError}</div>
  {/if}

  {#if taskHistoryLoading && !hasTaskProjection && !hasVisibleTaskHistory}
    <div class="task-empty-state" role="status" aria-live="polite">
      <div class="task-empty-glyph" aria-hidden="true">
        <Icon name="loader" size={18} class="spinning" />
      </div>
      <div class="task-empty-copy">
        <div class="task-empty-title">正在加载任务</div>
      </div>
    </div>
  {:else if !hasTaskProjection && !hasVisibleTaskHistory}
    <div class="task-empty-state" role="status" aria-live="polite">
      <div class="task-empty-glyph" aria-hidden="true">
        <Icon name="list" size={18} />
      </div>
      <div class="task-empty-copy">
        <div class="task-empty-title">{i18n.t('tasks.empty.title')}</div>
        <div class="task-empty-hint">{i18n.t('tasks.empty.hintNoPlan')}</div>
      </div>
    </div>
  {/if}
</div>

<style>
  /* ========== 面板容器 ========== */
  .tasks-panel {
    /* panel-content-scrollable 已经包含了 padding, flex, overflow */
    gap: var(--space-4);
  }

  /* ========== 空状态 ========== */
  .task-empty-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    width: 100%;
    min-height: clamp(280px, 52vh, 560px);
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
    box-sizing: border-box;
  }

  .task-empty-glyph {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface-2) 62%, transparent);
    color: var(--foreground-muted);
    opacity: 0.56;
  }

  .task-empty-copy {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2);
    max-width: 360px;
  }

  .task-empty-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    opacity: 0.88;
  }

  .task-empty-hint {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: var(--leading-normal);
    opacity: 0.72;
  }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ========== 任务进度 ========== */
  .task-progress-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid color-mix(in srgb, var(--border) 88%, transparent);
    border-radius: var(--radius-lg);
    background: var(--background);
    box-shadow: none;
  }

  .task-progress-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .task-progress-title-block {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    min-width: 0;
  }

  .task-progress-label {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .task-progress-meta {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .task-progress-actions {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .task-action-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .task-action-btn:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
    background: transparent;
    color: var(--primary);
  }

  .task-action-btn--quiet {
    color: var(--foreground-muted);
  }

  .task-action-btn:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .task-attention-strip {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    min-height: 30px;
    padding: var(--space-1) var(--space-2);
    border: 1px solid color-mix(in srgb, var(--error) 24%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--background);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    line-height: 1.45;
  }

  .task-attention-strip :global(svg) {
    flex: 0 0 auto;
    margin-top: 2px;
    color: var(--error);
  }

  .task-attention-strip-copy {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .task-attention-strip-copy strong {
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
  }

  .task-attention-strip-copy span {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    line-height: var(--leading-normal);
  }

  .task-attention-strip-copy strong,
  .task-attention-strip-copy span {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .task-progress-rows {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding-top: var(--space-1);
  }

  .task-progress-row {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-height: 34px;
    padding: var(--space-1) var(--space-1);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    cursor: default;
    text-align: left;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .task-progress-row--running {
    background: transparent;
  }

  .task-progress-row--failed {
    background: transparent;
    border-color: color-mix(in srgb, var(--error) 24%, transparent);
  }

  .task-progress-row--completed {
    opacity: 0.76;
  }

  .task-progress-status {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    flex-shrink: 0;
  }

  .task-progress-task {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    line-height: var(--leading-tight);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-progress-performer {
    max-width: 96px;
    overflow: hidden;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-tight);
    text-align: right;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-details-disclosure {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--background);
  }

  .task-details-disclosure > summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3);
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    list-style: none;
  }

  .task-details-disclosure > summary::-webkit-details-marker {
    display: none;
  }

  .task-details-disclosure > summary::after {
    content: '';
    width: 7px;
    height: 7px;
    border-right: 1.5px solid currentColor;
    border-bottom: 1.5px solid currentColor;
    transform: rotate(45deg);
    opacity: 0.6;
    transition: transform var(--transition-fast);
  }

  .task-details-disclosure[open] > summary::after {
    transform: rotate(225deg);
  }

  .task-details-disclosure > summary span:last-child {
    margin-left: auto;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-regular);
  }

  .task-details-disclosure[open] {
    padding-bottom: var(--space-3);
  }

  .task-details-disclosure[open] > .tg-tree,
  .task-details-disclosure[open] > .task-detail-card {
    margin: 0 var(--space-3);
  }

  .tg-status-badge {
    font-size: var(--text-2xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .tg-status--running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-status--completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .tg-status--failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .tg-status--pending,
  .tg-status--killed,
  .tg-status--unknown {
    color: var(--foreground-muted);
    background: transparent;
    border-color: var(--border);
  }

  /* ========== 任务树 ========== */
  .tg-tree {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .tg-tree-row {
    display: grid;
    grid-template-columns: 18px auto 18px minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--space-2);
    min-height: 36px;
    padding: var(--space-2) var(--space-3);
    padding-left: calc(var(--space-3) + var(--task-indent, 0px));
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .tg-tree-row:hover {
    background: transparent;
  }

  .tg-tree-row--running {
    background: transparent;
    border-color: color-mix(in srgb, var(--primary) 22%, transparent);
  }

  .tg-tree-row--completed {
    opacity: 0.72;
  }

  .tg-tree-row--failed {
    border-color: color-mix(in srgb, var(--error) 30%, transparent);
  }

  .tg-tree-row--selected {
    background: transparent;
    border-color: var(--border);
  }

  .tg-tree-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    background: transparent;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      transform var(--transition-fast);
  }

  .tg-tree-toggle:hover {
    background: var(--surface-hover);
  }

  .tg-tree-toggle.expanded {
    transform: rotate(90deg);
  }

  .tg-tree-toggle--empty {
    cursor: default;
    pointer-events: none;
  }

  .tg-kind-badge {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
    flex-shrink: 0;
    letter-spacing: 0.03em;
  }

  .tg-tree-status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 18px;
    height: 18px;
  }

  .tg-status-icon--running { color: var(--primary); }
  .tg-status-icon--completed { color: var(--success); }
  .tg-status-icon--failed { color: var(--error); }
  .tg-status-icon--pending,
  .tg-status-icon--killed,
  .tg-status-icon--unknown { color: var(--foreground-muted); }

  .tg-tree-content {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .tg-tree-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tg-tree-goal {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tg-tree-side {
    display: inline-flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-1);
    min-width: 0;
  }

  .tg-tree-state {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .tg-tree-count {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    padding: 1px 5px;
    background: transparent;
    border-radius: var(--radius-full);
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .tg-tree-detail-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    flex-shrink: 0;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .tg-tree-detail-btn:hover {
    color: var(--primary);
    background: var(--surface-hover);
    border-color: color-mix(in srgb, var(--primary) 24%, var(--border));
  }

  .task-detail-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--background);
  }

  .task-detail-top {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .task-detail-title {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-detail-actions {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    flex-shrink: 0;
  }

  .task-detail-status {
    flex-shrink: 0;
    border: 1px solid transparent;
    border-radius: 999px;
    padding: 2px 8px;
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  .task-detail-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition:
      background var(--transition-fast),
      color var(--transition-fast);
  }

  .task-detail-close:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .task-detail-goal {
    margin: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .task-detail-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .task-detail-blocker {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--warning);
    background: var(--warning-muted);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, var(--border));
    border-radius: var(--radius-sm);
    padding: var(--space-1) var(--space-2);
    font-size: var(--text-xs);
  }

  .task-detail-reference-groups {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .task-detail-reference-group {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    min-width: 0;
  }

  .task-detail-reference-label {
    flex-shrink: 0;
    width: 36px;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    padding-top: 3px;
  }

  .task-detail-reference-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    min-width: 0;
  }

  .task-detail-reference-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 220px;
    padding: 1px 5px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
    color: var(--foreground-muted);
    font: inherit;
    font-size: var(--text-2xs);
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .task-detail-reference-chip:hover,
  .task-detail-reference-chip--selected {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-2));
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .task-detail-reference-chip > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* ========== 最近任务 ========== */
  .task-history-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--background);
  }

  .task-history-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .task-history-title {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .task-history-count {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 2px 8px;
    white-space: nowrap;
  }

  .task-history-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .task-history-row {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-height: 36px;
    padding: var(--space-1);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: inherit;
    text-align: left;
    cursor: default;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .task-history-row:hover {
    background: transparent;
    border-color: var(--border);
  }

  .task-history-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-1);
    min-height: 28px;
    width: 100%;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .task-history-toggle:hover {
    border-color: var(--border);
    color: var(--foreground);
  }

  .task-history-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .task-history-row-title {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-history-row-meta {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-history-side {
    display: inline-flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
    min-width: 0;
  }

  .task-reference-detail-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid color-mix(in srgb, var(--primary) 20%, var(--border));
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--primary) 4%, var(--surface-1));
  }

  .task-reference-detail-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .task-reference-detail-title {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .task-reference-detail-title > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-reference-detail-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    flex-shrink: 0;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .task-reference-detail-close:hover {
    color: var(--foreground);
    background: var(--surface-hover);
    border-color: var(--border);
  }

  .task-reference-detail-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .task-reference-detail-field {
    display: grid;
    grid-template-columns: 56px minmax(0, 1fr);
    gap: var(--space-2);
    align-items: start;
    min-width: 0;
  }

  .task-reference-detail-label {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .task-reference-detail-value {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-reference-detail-value--mono {
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  }

  .task-reference-detail-actions {
    display: flex;
    justify-content: flex-end;
  }

  .task-reference-detail-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    padding: 0 var(--space-2);
    border: 1px solid color-mix(in srgb, var(--primary) 30%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--primary);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    cursor: pointer;
  }

  .task-reference-detail-action:hover {
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-1));
  }

  .tg-error {
    font-size: var(--text-xs);
    color: var(--error);
    padding: var(--space-2) var(--space-3);
    background: var(--error-muted);
    border: 1px solid color-mix(in srgb, var(--error) 32%, var(--border));
    border-radius: var(--radius-md);
  }

  /* ========== Delivery Package ========== */
  .delivery-package-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
    border: 1px solid color-mix(in srgb, var(--success) 22%, var(--border));
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--success) 4%, var(--surface-1));
  }

  .dp-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .dp-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .dp-summary-copy {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 var(--space-2);
    border: 1px solid color-mix(in srgb, var(--success) 28%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--success) 7%, var(--surface-2));
    color: var(--success);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    cursor: pointer;
    white-space: nowrap;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .dp-summary-copy:hover {
    background: color-mix(in srgb, var(--success) 12%, var(--surface-2));
    border-color: color-mix(in srgb, var(--success) 42%, var(--border));
  }

  .dp-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .dp-section-label {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    letter-spacing: 0.04em;
  }

  .dp-chip-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .dp-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dp-chip--interactive {
    cursor: pointer;
    font: inherit;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .dp-chip--interactive:hover,
  .dp-chip--selected {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-2));
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .dp-chip > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .dp-verification-row,
  .dp-risk-row {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
  }

  .dp-verification-row {
    color: var(--success);
    background: var(--success-muted);
  }

  .dp-risk-row {
    color: var(--warning);
    background: var(--warning-muted);
  }

  .dp-verification-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dp-verification-result {
    font-size: var(--text-2xs);
    text-transform: uppercase;
    opacity: 0.8;
  }

  .dp-progress {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: var(--space-1);
  }

  .dp-progress-label {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .dp-progress-bar {
    flex: 1;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .dp-progress-fill {
    height: 100%;
    border-radius: inherit;
    background: var(--success);
    transition: width 200ms ease;
  }

  .dp-progress-value {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
  }

</style>
