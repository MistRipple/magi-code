<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    addToast,
    getState,
    setCurrentBottomTab,
    setCurrentTopTab,
    setTaskComposerDraft,
    type TaskComposerIntent,
  } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { DecisionOptionDto, DeliveryPackageDto, TaskDto, TaskProjectionDto, TaskStatus } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    describeTaskReference,
    executeTaskReferenceAction,
    getTaskReferenceActionLabel,
    getTaskReferenceIconName,
    type TaskReferenceDescriptor,
  } from '../lib/task-reference';
  import {
    getRunnerStatusLabel,
    getTaskDisplayBlockedReason,
    getTaskDisplayGoal,
    getTaskDisplayText,
    getTaskDisplayTitle,
    getTaskKindLabel,
    getTaskStatusLabel,
    getTaskStatusTone,
  } from '../lib/task-labels';
  import {
    getTaskGraphState,
    getTaskStatusModifier,
    refreshTaskProjection,
    selectTaskGraphTask,
  } from '../stores/task-graph-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';
  import { vscode } from '../lib/vscode-bridge';

  const appState = getState();

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

  // ─── 任务投影视图 ─────────────────
  const currentSessionId = $derived(appState.currentSessionId);
  const taskGraph = $derived(getTaskGraphState(currentSessionId));
  const hasTaskProjection = $derived(taskGraph.projection !== null);
  const projectionProgress = $derived.by(() => {
    const p = taskGraph.projection?.progress_summary;
    if (!p || p.total_tasks === 0) return null;
    const percent = Math.round((p.settled_tasks / p.total_tasks) * 100);
    return { ...p, percent };
  });
  const projectionTasks = $derived(taskGraph.projection?.tasks ?? []);
  const taskById = $derived.by(() => new Map(projectionTasks.map((task) => [task.task_id, task])));
  const activeProjectionTasks = $derived.by(() => projectionTasks.filter((task) => task.status !== 'Cancelled'));
  const cancelledHistoryTasks = $derived.by(() => (
    projectionTasks
      .filter((task) => task.status === 'Cancelled')
      .sort((left, right) => right.updated_at - left.updated_at || right.created_at - left.created_at)
  ));
  let selectedHistoryTaskId = $state<string | null>(null);
  // 记录任务树节点展开状态。
  let expandedGraphNodes = $state<Set<string>>(new Set());

  // ─── 交付包视图（深度模式完成后展示） ─────────────────
  let deliveryPackage = $state<DeliveryPackageDto | null>(null);
  let deliveryPackageLoading = $state(false);
  let deliveryPackageScope = $state('');
  let deliveryPackageRequestScope = $state('');
  let deliverySummaryCopied = $state(false);
  let deliverySummaryCopyTimer: ReturnType<typeof setTimeout> | null = null;
  let taskActionLoading = $state<'pause' | 'resume' | 'replan' | 'delivery' | null>(null);
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
    const proj = taskGraph.projection;
    if (!proj) return false;
    return proj.execution_mode === 'deep' && proj.runner_status === 'completed';
  });

  $effect(() => {
    const rootTaskId = taskGraph.projection?.root_task.task_id;
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
    client.getDeliveryPackage(rootTaskId, sid)
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
    const projection = taskGraph.projection;
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
    buildTaskTreeRows(taskGraph.projection?.root_task, childrenByParentId, expandedGraphNodes)
  ));
  const taskSummary = $derived.by(() => buildTaskSummary(taskGraph.projection, activeProjectionTasks, cancelledHistoryTasks));
  const workpackageSummaries = $derived(taskGraph.projection?.workpackage_summaries ?? []);
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
  const currentFocusTask = $derived.by(() => resolveCurrentFocusTask(activeProjectionTasks));
  const selectedGraphTask = $derived.by(() => {
    if (taskGraph.selectedTaskId) {
      const matched = activeProjectionTasks.find((task) => task.task_id === taskGraph.selectedTaskId);
      if (matched) return matched;
    }
    return currentFocusTask;
  });
  const selectedGraphReferenceGroups = $derived.by(() => buildTaskReferenceGroups(selectedGraphTask));
  const canUseTaskIntake = $derived.by(() => {
    const projection = taskGraph.projection;
    if (!projection || projection.execution_mode !== 'deep') return false;
    return projection.runner_status === 'running' || projection.runner_status === 'blocked';
  });
  const selectedHistoryTask = $derived.by(() => {
    if (cancelledHistoryTasks.length === 0) return null;
    if (selectedHistoryTaskId) {
      const matched = taskById.get(selectedHistoryTaskId);
      if (matched && matched.status === 'Cancelled') {
        return matched;
      }
    }
    return cancelledHistoryTasks[0] ?? null;
  });
  const selectedHistoryContextReferences = $derived.by(() => (
    buildTaskReferences(selectedHistoryTask?.context_refs ?? [], 'auto')
  ));
  const selectedHistoryOutputReferences = $derived.by(() => (
    buildTaskReferences(selectedHistoryTask?.output_refs ?? [], 'auto')
  ));
  const selectedHistoryEvidenceReferences = $derived.by(() => (
    buildTaskReferences(selectedHistoryTask?.evidence_refs ?? [], 'auto')
  ));
  const attentionTasks = $derived.by(() => {
    const projection = taskGraph.projection;
    if (!projection) return [];
    const ids = [...projection.pending_decisions, ...projection.blocked_tasks];
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
  const pendingDecisionTask = $derived.by(() => (
    attentionTasks.find((task) => task.kind === 'Decision') ?? null
  ));

  function getTaskParentTitle(task: TaskDto): string {
    if (!task.parent_task_id) return '根任务';
    const parent = taskById.get(task.parent_task_id);
    return parent ? getTaskDisplayTitle(parent) : task.parent_task_id;
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
      { label: '上下文', sourceLabel: '任务详情 · 上下文', refs: task.context_refs, preferredAction: 'auto' as const },
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

  function buildTaskComposerDraftText(task: TaskDto, intent: TaskComposerIntent): string {
    const taskTitle = getTaskDisplayTitle(task);
    const taskGoal = getTaskDisplayGoal(task);
    const taskLabel = `${getTaskKindLabel(task.kind)} · ${taskTitle}`;
    const goalLine = taskGoal && taskGoal !== taskTitle ? `\n当前目标：${taskGoal}` : '';
    if (intent === 'supplement_context') {
      return `补充上下文：\n${taskLabel}${goalLine}\n\n请补充这个任务接下来执行需要的上下文、约束、证据或前置条件。`;
    }
    if (intent === 'append_task') {
      return `追加任务：\n${taskLabel}${goalLine}\n\n请追加一个可以直接纳入当前任务图的新子任务。`;
    }
    return `重新规划：\n${taskLabel}${goalLine}\n\n请基于这个任务所在的上下文，重新规划剩余任务图。`;
  }

  function openTaskComposerDraft(task: TaskDto, intent: TaskComposerIntent) {
    const sessionId = currentSessionIdValue();
    if (!sessionId) return;
    setTaskComposerDraft({
      intent,
      taskId: task.task_id,
      text: buildTaskComposerDraftText(task, intent),
    });
    selectTaskGraphTask(sessionId, task.task_id);
    setCurrentTopTab('thread');
    setCurrentBottomTab('thread');
    addToast('info', '已放入输入框，可编辑后发送');
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

  function toggleGraphNode(taskId: string) {
    const next = new Set(expandedGraphNodes);
    if (next.has(taskId)) next.delete(taskId);
    else next.add(taskId);
    expandedGraphNodes = next;
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
      const activeChildren = children.filter((child) => child.status !== 'Cancelled');
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

  function settledChildCount(taskId: string): number {
    return (childrenByParentId.get(taskId) ?? [])
      .filter((child) => child.status !== 'Cancelled')
      .filter((child) => child.status === 'Completed' || child.status === 'Skipped')
      .length;
  }

  function buildTaskSummary(projection: TaskProjectionDto | null, tasks: TaskDto[], history: TaskDto[]) {
    const progress = projection?.progress_summary;
    return {
      total: progress?.total_tasks ?? 0,
      completed: progress?.settled_tasks ?? 0,
      active: tasks.filter((task) => ['Running', 'Verifying', 'Repairing'].includes(task.status)).length,
      blocked: tasks.filter((task) => task.status === 'Blocked' || task.status === 'AwaitingApproval').length,
      failed: tasks.filter((task) => task.status === 'Failed').length,
      history: history.length,
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
      `模式：${pkg.execution_mode === 'deep' ? '深度模式' : '普通模式'}`,
      `状态：${getTaskStatusLabel(pkg.aggregate_status as TaskStatus)}`,
      pkg.current_phase ? `阶段：${pkg.current_phase}` : null,
      `进度：${completed}/${total}（${percent}%）`,
      `执行态：失败 ${pkg.progress.failed || 0} · 执行中 ${pkg.progress.running || 0} · 阻塞 ${pkg.progress.blocked || 0}`,
      `完成任务：${pkg.completed_task_count}`,
      `资产：文件 ${pkg.file_changes.length} · 证据 ${pkg.evidence_list.length} · 验证 ${pkg.validation_results.length} · 修复 ${pkg.repair_records.length} · 决策 ${pkg.key_decisions.length} · 风险 ${pkg.remaining_risks.length}`,
    ].filter((line): line is string => Boolean(line));

    appendDeliverySummaryList(lines, '文件变更', pkg.file_changes);
    appendDeliverySummaryList(lines, '证据', pkg.evidence_list);

    if (pkg.validation_results.length > 0) {
      lines.push('', `验证结果（${pkg.validation_results.length}）`);
      for (const result of pkg.validation_results.slice(0, 10)) {
        lines.push(`- ${getTaskDisplayText(result.title)}: ${getTaskDisplayText(result.result)}`);
      }
    }

    if (pkg.key_decisions.length > 0) {
      lines.push('', `关键决策（${pkg.key_decisions.length}）`);
      for (const decision of pkg.key_decisions.slice(0, 8)) {
        const chosen = decision.chosen_option ? ` · ${decision.chosen_option}` : '';
        lines.push(`- ${getTaskDisplayText(decision.context)}${chosen}`);
      }
    }

    appendDeliverySummaryList(lines, '剩余风险', pkg.remaining_risks.map(getTaskDisplayText), 8);
    return lines.join('\n');
  }

  function resolveCurrentFocusTask(tasks: TaskDto[]): TaskDto | null {
    const priority: TaskStatus[] = ['AwaitingApproval', 'Blocked', 'Repairing', 'Verifying', 'Running', 'Ready'];
    for (const status of priority) {
      const matched = tasks.find((task) => task.status === status && task.kind !== 'Objective');
      if (matched) return matched;
    }
    return tasks.find((task) => task.kind === 'Objective') ?? null;
  }

  // 自动展开根节点和活跃分支，确保任务树能直接反映执行状态。
  $effect(() => {
    const projection = taskGraph.projection;
    if (!projection) return;
    const next = new Set(expandedGraphNodes);
    let changed = false;
    const expand = (taskId: string) => {
      if (!next.has(taskId)) {
        next.add(taskId);
        changed = true;
      }
    };

    expand(projection.root_task.task_id);
    const visibleTaskIds = [
      ...projection.running_tasks,
      ...projection.blocked_tasks,
      ...projection.pending_decisions,
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
    if (changed) expandedGraphNodes = next;
  });

  $effect(() => {
    if (cancelledHistoryTasks.length === 0) {
      if (selectedHistoryTaskId !== null) selectedHistoryTaskId = null;
      return;
    }
    const available = new Set(cancelledHistoryTasks.map((task) => task.task_id));
    if (!selectedHistoryTaskId || !available.has(selectedHistoryTaskId)) {
      selectedHistoryTaskId = cancelledHistoryTasks[0]?.task_id ?? null;
    }
  });

  $effect(() => {
    const nextScope = taskGraph.projection
      ? `${currentSessionId ?? ''}:${taskGraph.projection.root_task.task_id}`
      : '';
    if (referenceSelectionScope !== nextScope) {
      referenceSelectionScope = nextScope;
      selectedTaskReference = null;
      selectTaskGraphTask(currentSessionId, null);
    }
  });

  function getProjectionStatusIcon(status: TaskStatus): { name: IconName; spinning: boolean } {
    switch (status) {
      case 'Running': return { name: 'loader', spinning: true };
      case 'Completed': return { name: 'check-circle', spinning: false };
      case 'Failed': return { name: 'x-circle', spinning: false };
      case 'Cancelled': case 'Skipped': return { name: 'skip-forward', spinning: false };
      case 'Blocked': return { name: 'alert-circle', spinning: false };
      case 'AwaitingApproval': return { name: 'shield', spinning: false };
      case 'Verifying': return { name: 'check-circle', spinning: true };
      case 'Repairing': return { name: 'wrench', spinning: true };
      default: return { name: 'circleOutline', spinning: false };
    }
  }

  function createClient(): RustDaemonClient {
    return new RustDaemonClient(resolveAgentBaseUrl());
  }

  function currentRootTaskId(): string | null {
    return taskGraph.projection?.root_task.task_id ?? null;
  }

  function currentSessionIdValue(): string | null {
    const sessionId = currentSessionId?.trim();
    return sessionId || null;
  }

  async function runTaskAction(
    action: 'pause' | 'resume' | 'replan' | 'delivery',
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

  async function pauseCurrentTaskGraph() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('pause', async () => {
      const client = createClient();
      await client.pauseTask({ taskId: rootTaskId, sessionId });
      clearDeliveryPackageViewState();
      await refreshTaskProjection(sessionId);
      addToast('info', '任务链已暂停');
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `暂停失败: ${message}`);
    });
  }

  async function resumeCurrentTaskGraph() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('resume', async () => {
      const client = createClient();
      await client.continueSession({ sessionId });
      clearDeliveryPackageViewState();
      await refreshTaskProjection(sessionId);
      addToast('success', '任务链已恢复');
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `恢复失败: ${message}`);
    });
  }

  async function replanCurrentTaskGraph() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runTaskAction('replan', async () => {
      const client = createClient();
      const result = await client.replanTaskGraph(rootTaskId, sessionId);
      clearDeliveryPackageViewState();
      await refreshTaskProjection(sessionId);
      addToast('info', `已重规划，取消 ${result.cancelledTaskIds.length} 个旧任务`);
    }).catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      addToast('error', `重规划失败: ${message}`);
    });
  }

  async function refreshDeliveryPackage() {
    const sessionId = currentSessionIdValue();
    if (!sessionId) return;
    await runTaskAction('delivery', async () => {
      await refreshTaskProjection(sessionId);
      clearDeliveryPackageViewState();
    });
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

  function selectGraphTask(taskId: string) {
    selectTaskGraphTask(currentSessionId, taskId);
    if (selectedTaskReference?.sourceLabel.startsWith('任务详情 ·')) {
      selectedTaskReference = null;
    }
  }

  function focusPendingDecision() {
    if (!pendingDecisionTask) return;
    selectGraphTask(pendingDecisionTask.task_id);
  }

  function selectHistoryTask(taskId: string) {
    selectedHistoryTaskId = taskId;
    if (selectedTaskReference?.sourceLabel.startsWith('历史 ·')) {
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

  // ─── 决策任务处理 ──────────────────────────────────────
  let decisionLoading = $state<Set<string>>(new Set());

  function getDecisionOptions(task: TaskDto): DecisionOptionDto[] {
    return task.decision_payload?.options ?? [];
  }

  function isRecommendedDecisionOption(task: TaskDto, option: DecisionOptionDto): boolean {
    return task.decision_payload?.recommended_option === option.option_id;
  }

  async function resolveDecision(taskId: string, option: DecisionOptionDto) {
    const sessionId = currentSessionId?.trim();
    if (!sessionId) return;
    const next = new Set(decisionLoading);
    next.add(taskId);
    decisionLoading = next;
    try {
      const client = createClient();
      await client.resolveTaskDecision(taskId, sessionId, {
        chosenOption: option.option_id,
      });
      await refreshTaskProjection(currentSessionId);
    } catch (err) {
      console.error('Failed to resolve decision:', err);
    } finally {
      const after = new Set(decisionLoading);
      after.delete(taskId);
      decisionLoading = after;
    }
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
    {@const proj = taskGraph.projection}
    {#if proj}
      <section class="task-overview-card" aria-label="任务概览">
        <div class="task-overview-top">
          <div class="task-overview-main">
            <span class="task-overview-kicker">当前任务</span>
            <h3 class="task-overview-title">{getTaskDisplayTitle(proj.root_task)}</h3>
            {#if getTaskDisplayGoal(proj.root_task) && getTaskDisplayGoal(proj.root_task) !== getTaskDisplayTitle(proj.root_task)}
              <p class="task-overview-goal">{getTaskDisplayGoal(proj.root_task)}</p>
            {/if}
          </div>
          <div class="task-overview-badges">
            <span class="tg-mode-badge tg-mode--{proj.execution_mode}">
              {proj.execution_mode === 'deep' ? '深度模式' : '普通模式'}
            </span>
            <span class="tg-runner-badge tg-runner--{proj.runner_status}">
              {getRunnerStatusLabel(proj.runner_status)}
            </span>
            <span class="tg-status-badge tg-status--{getTaskStatusModifier(proj.aggregate_status)}">
              {proj.display_status}
            </span>
          </div>
        </div>

        <div class="task-overview-actions">
          {#if proj.runner_status === 'running'}
            <button
              type="button"
              class="task-action-btn"
              disabled={taskActionLoading !== null}
              onclick={pauseCurrentTaskGraph}
              title="暂停任务链"
            >
              <Icon name={taskActionLoading === 'pause' ? 'loader' : 'stop'} size={12} class={taskActionLoading === 'pause' ? 'spinning' : ''} />
              <span>暂停</span>
            </button>
          {:else if proj.runner_status === 'blocked'}
            {#if pendingDecisionTask}
              <button
                type="button"
                class="task-action-btn"
                onclick={focusPendingDecision}
                title="查看待处理决策"
              >
                <Icon name="shield" size={12} />
                <span>处理决策</span>
              </button>
            {:else}
            <button
              type="button"
              class="task-action-btn"
              disabled={taskActionLoading !== null}
              onclick={resumeCurrentTaskGraph}
              title="恢复任务链"
            >
              <Icon name={taskActionLoading === 'resume' ? 'loader' : 'play'} size={12} class={taskActionLoading === 'resume' ? 'spinning' : ''} />
              <span>恢复</span>
            </button>
            {/if}
          {/if}

          {#if proj.execution_mode === 'deep' && proj.runner_status !== 'completed'}
            <button
              type="button"
              class="task-action-btn"
              disabled={taskActionLoading !== null}
              onclick={replanCurrentTaskGraph}
              title="重规划剩余任务图"
            >
              <Icon name={taskActionLoading === 'replan' ? 'loader' : 'refresh'} size={12} class={taskActionLoading === 'replan' ? 'spinning' : ''} />
              <span>重规划</span>
            </button>
          {/if}

          {#if deliveryPackage || shouldFetchDelivery}
            <button
              type="button"
              class="task-action-btn"
              disabled={taskActionLoading !== null || deliveryPackageLoading}
              onclick={refreshDeliveryPackage}
              title="刷新交付概览"
            >
              <Icon name={taskActionLoading === 'delivery' ? 'loader' : 'download'} size={12} class={taskActionLoading === 'delivery' ? 'spinning' : ''} />
              <span>刷新交付</span>
            </button>
          {/if}
        </div>

        {#if projectionProgress}
          <div class="tg-progress-wrap">
            <span class="tg-progress-label">{projectionProgress.percent}%</span>
            <div class="tg-progress-bar">
              <div class="tg-progress-fill" style="width: {projectionProgress.percent}%"></div>
            </div>
          </div>
          <div class="task-metrics">
            <span>{taskSummary.completed}/{taskSummary.total} 已完成</span>
            {#if taskSummary.active > 0}
              <span class="tg-stat tg-stat--running">{taskSummary.active} 正在推进</span>
            {/if}
            {#if taskSummary.blocked > 0}
              <span class="tg-stat tg-stat--blocked">{taskSummary.blocked} 需要处理</span>
            {/if}
            {#if taskSummary.failed > 0}
              <span class="tg-stat tg-stat--failed">{taskSummary.failed} 失败</span>
            {/if}
            {#if taskSummary.history > 0}
              <span class="tg-stat tg-stat--history">{taskSummary.history} 历史节点</span>
            {/if}
          </div>
        {/if}

        {#if currentFocusTask}
          <div class="task-focus-card">
            <span class="task-focus-label">当前焦点</span>
            <div class="task-focus-main">
              <span class="task-focus-title">{getTaskDisplayTitle(currentFocusTask)}</span>
              <span class="task-focus-status">{getTaskStatusTone(currentFocusTask.status)}</span>
            </div>
            {#if proj.current_phase}
              <span class="task-focus-meta">阶段：{proj.current_phase}</span>
            {/if}
          </div>
        {/if}

        {#if proj.validation_summary}
          <div class="tg-validation-summary">{proj.validation_summary}</div>
        {/if}

        {#if workpackageSummaries.length > 0}
          <div class="wp-summaries">
            <div class="wp-summaries-label">工作包进度</div>
            {#each workpackageSummaries as wp (wp.task_id)}
              <div class="wp-summary-row">
                <div class="wp-summary-header">
                  <span class="wp-summary-title">{getTaskDisplayText(wp.title)}</span>
                  <span class="wp-summary-badge tg-status--{getTaskStatusModifier(wp.aggregate_status)}">
                    {getTaskStatusLabel(wp.aggregate_status)}
                  </span>
                </div>
                <div class="wp-progress-wrap">
                  <div class="wp-progress-bar">
                    <div class="wp-progress-fill" style="width: {Math.round(wp.progress_ratio * 100)}%"></div>
                  </div>
                  <span class="wp-progress-label">{Math.round(wp.progress_ratio * 100)}%</span>
                </div>
                {#if wp.recent_issues.length > 0}
                  <div class="wp-summary-issues">
                    <Icon name="alert-circle" size={10} />
                    <span>{wp.recent_issues.length} 项异常: {wp.recent_issues.map(getTaskDisplayText).join('、')}</span>
                  </div>
                {/if}
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
            <span class="dp-mode">{deliveryPackage.execution_mode === 'deep' ? '深度模式' : '普通模式'}</span>
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

          {#if deliveryPackage.validation_results.length > 0}
            <div class="dp-section">
              <span class="dp-section-label">验证结果 ({deliveryPackage.validation_results.length})</span>
              {#each deliveryPackage.validation_results as vr}
                <div class="dp-validation-row">
                  <Icon name="check-circle" size={12} />
                  <span class="dp-validation-title">{getTaskDisplayText(vr.title)}</span>
                  <span class="dp-validation-result">{getTaskDisplayText(vr.result)}</span>
                </div>
              {/each}
            </div>
          {/if}

          {#if deliveryPackage.key_decisions.length > 0}
            <div class="dp-section">
              <span class="dp-section-label">关键决策 ({deliveryPackage.key_decisions.length})</span>
              {#each deliveryPackage.key_decisions as kd}
                <div class="dp-decision-row">
                  <Icon name="shield" size={12} />
                  <span class="dp-decision-context">{getTaskDisplayText(kd.context)}</span>
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

      <div class="task-section-header">
        <span>执行结构</span>
        <span class="task-section-meta">{activeProjectionTasks.length} 个活跃节点</span>
      </div>

      <div class="tg-tree" role="tree" aria-label="任务执行结构">
        {#each taskTreeRows as row (row.task.task_id)}
          {@const isExpanded = expandedGraphNodes.has(row.task.task_id)}
          {@const statusIcon = getProjectionStatusIcon(row.task.status)}
          <div
            class="tg-tree-row tg-tree-row--{getTaskStatusModifier(row.task.status)}"
            class:tg-tree-row--focus={currentFocusTask?.task_id === row.task.task_id}
            class:tg-tree-row--selected={selectedGraphTask?.task_id === row.task.task_id}
            role="treeitem"
            aria-level={row.depth + 1}
            aria-expanded={row.hasChildren ? isExpanded : undefined}
            aria-selected={selectedGraphTask?.task_id === row.task.task_id}
            style={`--task-indent: ${row.depth * 18}px;`}
          >
            {#if row.hasChildren}
              <button
                type="button"
                class="tg-tree-toggle"
                class:expanded={isExpanded}
                aria-label={isExpanded ? '折叠任务' : '展开任务'}
                onclick={() => toggleGraphNode(row.task.task_id)}
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
              {#if row.task.kind === 'WorkPackage'}
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
                onclick={() => selectGraphTask(row.task.task_id)}
              >
                <Icon name="info" size={11} />
              </button>
            </span>
          </div>
        {/each}
      </div>

      {#if selectedGraphTask}
        <section class="task-detail-card" aria-label="任务详情">
          <div class="task-detail-top">
            <span class="task-detail-title">{getTaskDisplayTitle(selectedGraphTask)}</span>
            <span class="task-detail-status tg-status--{getTaskStatusModifier(selectedGraphTask.status)}">
              {getTaskStatusLabel(selectedGraphTask.status)}
            </span>
          </div>
          {#if selectedGraphTask.kind !== 'Decision' && getTaskDisplayGoal(selectedGraphTask) && getTaskDisplayGoal(selectedGraphTask) !== getTaskDisplayTitle(selectedGraphTask)}
            <p class="task-detail-goal">{getTaskDisplayGoal(selectedGraphTask)}</p>
          {/if}
          <div class="task-detail-meta">
            <span>{getTaskKindLabel(selectedGraphTask.kind)}</span>
            <span>路径：{getTaskLineageLabel(selectedGraphTask)}</span>
            {#if selectedGraphTask.executor_binding?.target_role}
              <span>执行者：{selectedGraphTask.executor_binding.target_role}</span>
            {/if}
            {#if selectedGraphTask.workspace_scope}
              <span>工作区：{selectedGraphTask.workspace_scope}</span>
            {/if}
            {#if selectedGraphTask.write_scope}
              <span>写入范围：{selectedGraphTask.write_scope}</span>
            {/if}
            {#if selectedGraphTask.retry_count > 0 || selectedGraphTask.repair_count > 0}
              <span>重试 {selectedGraphTask.retry_count} · 修复 {selectedGraphTask.repair_count}</span>
            {/if}
          </div>
          {#if canUseTaskIntake}
            <div class="task-detail-actions">
              <button
                type="button"
                class="tg-attention-action"
                onclick={() => openTaskComposerDraft(selectedGraphTask, 'supplement_context')}
                title="补充当前任务的上下文"
              >
                <Icon name="sparkles" size={11} />
                <span>补充上下文</span>
              </button>
              <button
                type="button"
                class="tg-attention-action"
                onclick={() => openTaskComposerDraft(selectedGraphTask, 'append_task')}
                title="追加一个新子任务"
              >
                <Icon name="plus" size={11} />
                <span>追加任务</span>
              </button>
              <button
                type="button"
                class="tg-attention-action"
                onclick={() => openTaskComposerDraft(selectedGraphTask, 'replan')}
                title="基于当前任务上下文重新规划"
              >
                <Icon name="refresh" size={11} />
                <span>重新规划</span>
              </button>
            </div>
          {/if}
          {#if selectedGraphTask.kind === 'Decision' && getTaskDisplayBlockedReason(selectedGraphTask)}
            <div class="task-detail-blocker">
              <Icon name="alert-circle" size={12} />
              <span>{getTaskDisplayBlockedReason(selectedGraphTask)}</span>
            </div>
          {/if}
          {#if selectedGraphReferenceGroups.length > 0}
            <div class="task-detail-reference-groups">
              {#each selectedGraphReferenceGroups as group (group.label)}
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

      {#if attentionTasks.length > 0}
        <div class="tg-attention-section">
          <div class="task-section-header">
            <span>需要处理</span>
            <span class="task-section-meta">{attentionTasks.length} 项</span>
          </div>
          {#each attentionTasks as task (task.task_id)}
            {@const isDecLoading = decisionLoading.has(task.task_id)}
            {@const decisionOptions = task.kind === 'Decision' ? getDecisionOptions(task) : []}
            <div class="tg-attention-item tg-attention--{task.kind === 'Decision' ? 'decision' : 'blocked'}">
              <Icon name={task.kind === 'Decision' ? 'shield' : 'alert-circle'} size={12} />
              <div class="tg-attention-copy">
                <span class="tg-attention-title">{getTaskDisplayTitle(task)}</span>
                <span class="tg-attention-meta">
                  {#if task.kind === 'Decision' && getTaskDisplayBlockedReason(task)}
                    {getTaskDisplayBlockedReason(task)}
                  {:else}
                    {getTaskStatusLabel(task.status)}
                  {/if}
                </span>
              </div>
              {#if task.kind === 'Decision'}
                <div class="tg-decision-actions">
                  {#if decisionOptions.length > 0}
                    {#each decisionOptions as option (option.option_id)}
                      <button
                        class={isRecommendedDecisionOption(task, option)
                          ? 'tg-decision-btn tg-decision-btn--recommended'
                          : 'tg-decision-btn'}
                        title={option.description || option.label}
                        disabled={isDecLoading}
                        onclick={() => resolveDecision(task.task_id, option)}
                      >
                        {#if isDecLoading}
                          <Icon name="loader" size={12} class="spinning" />
                        {:else}
                          <Icon name="check" size={12} />
                        {/if}
                        {option.label}
                      </button>
                    {/each}
                  {:else}
                    <span class="tg-decision-empty">缺少决策选项</span>
                  {/if}
                </div>
              {:else if task.status === 'Blocked'}
                <div class="tg-attention-actions">
                  {#if proj.runner_status === 'blocked'}
                    <button
                      type="button"
                      class="tg-attention-action"
                      disabled={taskActionLoading !== null}
                      onclick={resumeCurrentTaskGraph}
                      title="恢复任务链"
                    >
                      <Icon name={taskActionLoading === 'resume' ? 'loader' : 'play'} size={12} class={taskActionLoading === 'resume' ? 'spinning' : ''} />
                      <span>恢复</span>
                    </button>
                  {/if}
                  {#if proj.execution_mode === 'deep' && proj.runner_status !== 'completed'}
                    <button
                      type="button"
                      class="tg-attention-action"
                      disabled={taskActionLoading !== null}
                      onclick={replanCurrentTaskGraph}
                      title="重规划剩余任务图"
                    >
                      <Icon name={taskActionLoading === 'replan' ? 'loader' : 'refresh'} size={12} class={taskActionLoading === 'replan' ? 'spinning' : ''} />
                      <span>重规划</span>
                    </button>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if cancelledHistoryTasks.length > 0}
        <section class="task-history-card" aria-label="重规划历史">
          <div class="task-history-header">
            <span class="task-history-title">
              <Icon name="clock" size={12} />
              <span>重规划历史</span>
            </span>
            <span class="task-history-count">{cancelledHistoryTasks.length} 个旧节点</span>
          </div>

          <div class="task-history-list">
            {#each cancelledHistoryTasks.slice(0, 8) as task (task.task_id)}
              <button
                type="button"
                class="task-history-row"
                class:task-history-row--selected={selectedHistoryTask?.task_id === task.task_id}
                title={`${getTaskLineageLabel(task)} / ${getTaskDisplayTitle(task)}`}
                onclick={() => selectHistoryTask(task.task_id)}
              >
                <Icon name="skip-forward" size={12} />
                <span class="task-history-kind">{getTaskKindLabel(task.kind)}</span>
                <span class="task-history-main">
                  <span class="task-history-row-title">{getTaskDisplayTitle(task)}</span>
                  <span class="task-history-row-meta">父级：{getTaskParentTitle(task)} · {formatTimestamp(task.updated_at)}</span>
                </span>
              </button>
            {/each}
          </div>

          {#if selectedHistoryTask}
            <div class="task-history-detail">
              <div class="task-history-detail-top">
                <span class="task-history-detail-title">{getTaskDisplayTitle(selectedHistoryTask)}</span>
                <span class="task-history-detail-status">{getTaskStatusLabel(selectedHistoryTask.status)}</span>
              </div>
              {#if getTaskDisplayGoal(selectedHistoryTask) && getTaskDisplayGoal(selectedHistoryTask) !== getTaskDisplayTitle(selectedHistoryTask)}
                <p class="task-history-detail-goal">{getTaskDisplayGoal(selectedHistoryTask)}</p>
              {/if}
              <div class="task-history-detail-meta">
                <span>{getTaskKindLabel(selectedHistoryTask.kind)}</span>
                <span>路径：{getTaskLineageLabel(selectedHistoryTask)}</span>
                <span>更新：{formatTimestamp(selectedHistoryTask.updated_at)}</span>
              </div>
              <div class="task-history-ref-summary">
                <span>上下文 {selectedHistoryTask.context_refs.length}</span>
                <span>产出 {selectedHistoryTask.output_refs.length}</span>
                <span>证据 {selectedHistoryTask.evidence_refs.length}</span>
              </div>
              {#if selectedHistoryContextReferences.length > 0 || selectedHistoryOutputReferences.length > 0 || selectedHistoryEvidenceReferences.length > 0}
                <div class="task-history-reference-groups">
                  {#if selectedHistoryContextReferences.length > 0}
                    <div class="task-history-reference-group">
                      <span class="task-history-reference-label">上下文</span>
                      <div class="task-history-reference-list">
                        {#each selectedHistoryContextReferences as reference, index (`context-${reference.raw}:${index}`)}
                          <button
                            type="button"
                            class="task-history-reference-chip"
                            class:task-history-reference-chip--selected={isTaskReferenceSelected('历史 · 上下文', reference)}
                            title={reference.title}
                            onclick={() => selectTaskReference('历史 · 上下文', reference)}
                          >
                            <Icon name={getTaskReferenceIconName(reference)} size={10} />
                            <span>{reference.displayLabel}</span>
                          </button>
                        {/each}
                      </div>
                    </div>
                  {/if}
                  {#if selectedHistoryOutputReferences.length > 0}
                    <div class="task-history-reference-group">
                      <span class="task-history-reference-label">产出</span>
                      <div class="task-history-reference-list">
                        {#each selectedHistoryOutputReferences as reference, index (`output-${reference.raw}:${index}`)}
                          <button
                            type="button"
                            class="task-history-reference-chip"
                            class:task-history-reference-chip--selected={isTaskReferenceSelected('历史 · 产出', reference)}
                            title={reference.title}
                            onclick={() => selectTaskReference('历史 · 产出', reference)}
                          >
                            <Icon name={getTaskReferenceIconName(reference)} size={10} />
                            <span>{reference.displayLabel}</span>
                          </button>
                        {/each}
                      </div>
                    </div>
                  {/if}
                  {#if selectedHistoryEvidenceReferences.length > 0}
                    <div class="task-history-reference-group">
                      <span class="task-history-reference-label">证据</span>
                      <div class="task-history-reference-list">
                        {#each selectedHistoryEvidenceReferences as reference, index (`evidence-${reference.raw}:${index}`)}
                          <button
                            type="button"
                            class="task-history-reference-chip"
                            class:task-history-reference-chip--selected={isTaskReferenceSelected('历史 · 证据', reference)}
                            title={reference.title}
                            onclick={() => selectTaskReference('历史 · 证据', reference)}
                          >
                            <Icon name={getTaskReferenceIconName(reference)} size={10} />
                            <span>{reference.displayLabel}</span>
                          </button>
                        {/each}
                      </div>
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/if}
        </section>
      {/if}

      {#if taskGraph.error}
        <div class="tg-error">{taskGraph.error}</div>
      {/if}
    {/if}
  {/if}

  {#if !hasTaskProjection}
    <div class="empty-state">
      <div class="task-empty-card">
        <div class="empty-icon-wrap">
          <Icon name="circleOutline" size={32} class="empty-icon" />
        </div>
        <div class="empty-text">{i18n.t('tasks.empty.title')}</div>
        <div class="empty-hint">
          {i18n.t('tasks.empty.hintNoPlan')}
        </div>
        <div class="task-empty-points" aria-label="任务面板说明">
          <span class="task-empty-point">{i18n.t('tasks.empty.pointGoal')}</span>
          <span class="task-empty-point">{i18n.t('tasks.empty.pointFlow')}</span>
          <span class="task-empty-point">{i18n.t('tasks.empty.pointDelivery')}</span>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  /* ========== WorkPackage 进度概览 ========== */
  .wp-summaries {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin-top: var(--space-3);
    padding-top: var(--space-3);
    border-top: 1px solid var(--border);
  }

  .wp-summaries-label {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    letter-spacing: 0.04em;
  }

  .wp-summary-row {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }

  .wp-summary-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .wp-summary-title {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .wp-summary-badge {
    font-size: var(--text-2xs);
    border-radius: 999px;
    padding: 1px 6px;
    border: 1px solid transparent;
    flex-shrink: 0;
  }

  .wp-progress-wrap {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .wp-progress-bar {
    flex: 1;
    height: 6px;
    background: var(--surface-hover);
    border-radius: 999px;
    overflow: hidden;
  }

  .wp-progress-fill {
    height: 100%;
    background: var(--primary);
    border-radius: inherit;
    transition: width 200ms ease;
  }

  .wp-progress-label {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    flex-shrink: 0;
    min-width: 28px;
    text-align: right;
  }

  .wp-summary-issues {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-2xs);
    color: var(--warning);
    margin-top: var(--space-1);
  }

  /* ========== 面板容器 ========== */
  .tasks-panel {
    /* panel-content-scrollable 已经包含了 padding, flex, overflow */
    gap: var(--space-4);
  }

  /* ========== 空状态 ========== */
  .empty-state {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    width: 100%;
    min-height: 320px;
    box-sizing: border-box;
  }

  .task-empty-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    width: min(100%, 420px);
    padding: var(--space-5) var(--space-4);
    border: 1px solid color-mix(in srgb, var(--border) 88%, transparent);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-sm);
    color: var(--foreground-muted);
    text-align: center;
    box-sizing: border-box;
  }

  .empty-icon-wrap {
    opacity: 0.34;
  }

  .empty-text {
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .empty-hint {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: var(--leading-normal);
  }

  .task-empty-points {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--space-2);
  }

  .task-empty-point {
    display: inline-flex;
    align-items: center;
    min-height: 24px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    background: var(--surface-2);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ========== 任务概览 ========== */
  .task-overview-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
    border: 1px solid color-mix(in srgb, var(--border) 88%, transparent);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-sm);
  }

  .task-overview-top {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .task-overview-main {
    min-width: 0;
  }

  .task-overview-badges {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .task-overview-kicker {
    display: block;
    margin-bottom: var(--space-1);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    letter-spacing: 0.08em;
  }

  .task-overview-title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: var(--font-semibold);
    line-height: var(--leading-tight);
    color: var(--foreground);
  }

  .task-overview-goal {
    margin: var(--space-2) 0 0;
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
    color: var(--foreground-muted);
  }

  .task-overview-actions {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
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
    background: var(--surface-2);
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
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-2));
    color: var(--primary);
  }

  .task-action-btn:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .task-metrics {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .task-focus-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-3);
    border: 1px solid color-mix(in srgb, var(--primary) 18%, var(--border));
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--primary) 5%, transparent);
  }

  .task-focus-label {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
  }

  .task-focus-main {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .task-focus-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .task-focus-status,
  .task-focus-meta,
  .task-section-meta {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
  }

  .task-section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    padding: 0 var(--space-1);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
  }

  .tg-status-badge,
  .tg-mode-badge,
  .tg-runner-badge {
    font-size: var(--text-2xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .tg-status--running,
  .tg-runner--running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-status--completed,
  .tg-runner--completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .tg-status--failed,
  .tg-runner--error {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .tg-status--blocked,
  .tg-runner--blocked {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-status--cancelled,
  .tg-status--skipped {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .tg-status--draft,
  .tg-status--ready,
  .tg-status--unknown {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .tg-status--awaiting-approval {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-status--verifying {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-status--repairing {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-progress-wrap {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .tg-progress-label {
    min-width: 50px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
  }

  .tg-progress-bar {
    flex: 1;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .tg-progress-fill {
    height: 100%;
    border-radius: inherit;
    background: var(--primary);
    transition: width 200ms ease;
  }

  .tg-stat {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
  }

  .tg-stat--running { color: var(--primary); }
  .tg-stat--failed { color: var(--error); }
  .tg-stat--blocked { color: var(--warning); }
  .tg-stat--history { color: var(--foreground-muted); }

  .tg-validation-summary {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    padding: var(--space-1) 0;
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
    background: var(--surface-1);
  }

  .tg-tree-row--running {
    background: color-mix(in srgb, var(--primary) 6%, transparent);
    border-color: color-mix(in srgb, var(--primary) 22%, transparent);
  }

  .tg-tree-row--completed {
    opacity: 0.72;
  }

  .tg-tree-row--failed {
    border-color: color-mix(in srgb, var(--error) 30%, transparent);
  }

  .tg-tree-row--focus {
    background: color-mix(in srgb, var(--primary) 7%, transparent);
    border-color: color-mix(in srgb, var(--primary) 28%, transparent);
  }

  .tg-tree-row--selected {
    background: color-mix(in srgb, var(--primary) 9%, transparent);
    border-color: color-mix(in srgb, var(--primary) 34%, transparent);
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
    background: var(--surface-2);
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
  .tg-status-icon--blocked { color: var(--warning); }
  .tg-status-icon--cancelled,
  .tg-status-icon--skipped { color: var(--foreground-muted); }
  .tg-status-icon--draft,
  .tg-status-icon--ready { color: var(--foreground-muted); }
  .tg-status-icon--awaiting-approval { color: var(--warning); }
  .tg-status-icon--verifying { color: var(--primary); }
  .tg-status-icon--repairing { color: var(--warning); }
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
    background: var(--surface-3);
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
    background: var(--surface-1);
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

  .task-detail-status {
    flex-shrink: 0;
    border: 1px solid transparent;
    border-radius: 999px;
    padding: 2px 8px;
    font-size: var(--text-2xs);
    white-space: nowrap;
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

  .task-detail-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1);
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

  /* ========== Attention Section (blocked / decisions) ========== */
  .tg-attention-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .tg-attention-item {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    font-size: var(--text-xs);
  }

  .tg-attention-copy {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .tg-attention-title {
    overflow: hidden;
    color: var(--foreground);
    font-weight: var(--font-medium);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tg-attention-meta {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .tg-attention--blocked {
    color: var(--warning);
    background: var(--warning-muted);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-attention--decision {
    color: var(--primary);
    background: var(--primary-muted);
    border: 1px solid color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-decision-actions {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    flex-shrink: 0;
  }

  .tg-decision-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-2xs);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    border: 1px solid transparent;
    cursor: pointer;
    white-space: nowrap;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .tg-decision-btn {
    color: var(--text-secondary);
    background: var(--surface-1);
    border-color: var(--border);
  }

  .tg-decision-btn:hover:not(:disabled) {
    color: var(--primary);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-1));
  }

  .tg-decision-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .tg-decision-btn--recommended {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .tg-decision-btn--recommended:hover:not(:disabled) {
    background: color-mix(in srgb, var(--success) 20%, var(--surface-1));
  }

  .tg-decision-empty {
    font-size: var(--text-2xs);
    color: var(--text-tertiary);
    white-space: nowrap;
  }

  .tg-attention-actions {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    flex-shrink: 0;
  }

  .tg-attention-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    font-size: var(--text-2xs);
    cursor: pointer;
    white-space: nowrap;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .tg-attention-action:hover:not(:disabled) {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-1));
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-attention-action:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  /* ========== Replan History ========== */
  .task-history-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
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
    background: var(--surface-2);
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
    grid-template-columns: 16px auto minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    padding: var(--space-2);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: inherit;
    text-align: left;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .task-history-row:hover,
  .task-history-row--selected {
    background: var(--surface-2);
    border-color: var(--border);
  }

  .task-history-kind {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
    white-space: nowrap;
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

  .task-history-detail {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid color-mix(in srgb, var(--border) 80%, transparent);
    border-radius: var(--radius-md);
    background: var(--surface-2);
  }

  .task-history-detail-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .task-history-detail-title {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-history-detail-status {
    flex-shrink: 0;
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 2px 8px;
  }

  .task-history-detail-goal {
    margin: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .task-history-detail-meta,
  .task-history-ref-summary {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .task-history-ref-summary span {
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 6px;
    background: var(--surface-1);
  }

  .task-history-reference-groups {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .task-history-reference-group {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    min-width: 0;
  }

  .task-history-reference-label {
    flex-shrink: 0;
    width: 36px;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    padding-top: 3px;
  }

  .task-history-reference-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    min-width: 0;
  }

  .task-history-reference-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 220px;
    padding: 1px 5px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground-muted);
    font: inherit;
    font-size: var(--text-2xs);
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .task-history-reference-chip:hover,
  .task-history-reference-chip--selected {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 8%, var(--surface-1));
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .task-history-reference-chip > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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

  .dp-mode {
    margin-left: auto;
    font-size: var(--text-2xs);
    color: var(--success);
    background: var(--success-muted);
    border: 1px solid color-mix(in srgb, var(--success) 32%, var(--border));
    border-radius: 999px;
    padding: 2px 8px;
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

  .dp-validation-row,
  .dp-decision-row,
  .dp-risk-row {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
  }

  .dp-validation-row {
    color: var(--success);
    background: var(--success-muted);
  }

  .dp-decision-row {
    color: var(--primary);
    background: var(--primary-muted);
  }

  .dp-risk-row {
    color: var(--warning);
    background: var(--warning-muted);
  }

  .dp-validation-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dp-validation-result {
    font-size: var(--text-2xs);
    text-transform: uppercase;
    opacity: 0.8;
  }

  .dp-decision-context {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
