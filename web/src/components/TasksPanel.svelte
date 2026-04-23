<script lang="ts">
  import { getState, requestMessageJump, setCurrentBottomTab, setCurrentTopTab } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import type { ActivePlanState, PlanLedgerRecord, Message } from '../types/message';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { DecisionOptionDto, TaskDto, TaskStatus } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    getTaskGraphState,
    getTaskStatusModifier,
    refreshTaskProjection,
  } from '../stores/task-graph-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';

  const appState = getState();

  interface TaskTreeRow {
    task: TaskDto;
    depth: number;
    hasChildren: boolean;
    childCount: number;
  }

  const appPayload = $derived((appState.appState || {}) as Record<string, unknown>);
  const activePlanState = $derived((appPayload.activePlan || null) as ActivePlanState | null);
  const planHistory = $derived(ensureArray(appPayload.planHistory) as PlanLedgerRecord[]);
  const threadMessages = $derived(ensureArray(appState.threadMessages) as Message[]);

  // ─── 任务投影视图 ─────────────────
  const currentSessionId = $derived(appState.currentSessionId);
  const taskGraph = $derived(getTaskGraphState(currentSessionId));
  const hasTaskProjection = $derived(taskGraph.projection !== null);
  const projectionProgress = $derived.by(() => {
    const p = taskGraph.projection?.progress_summary;
    if (!p || p.total_tasks === 0) return null;
    const percent = Math.round((p.completed_tasks / p.total_tasks) * 100);
    return { ...p, percent };
  });
  const projectionTasks = $derived(taskGraph.projection?.tasks ?? []);
  const taskById = $derived.by(() => new Map(projectionTasks.map((task) => [task.task_id, task])));
  // 记录任务树节点展开状态。
  let expandedGraphNodes = $state<Set<string>>(new Set());

  const childrenByParentId = $derived.by(() => {
    const grouped = new Map<string, TaskDto[]>();
    for (const task of projectionTasks) {
      if (!task.parent_task_id) continue;
      const siblings = grouped.get(task.parent_task_id) ?? [];
      siblings.push(task);
      grouped.set(task.parent_task_id, siblings);
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
  const taskSummary = $derived.by(() => buildTaskSummary(projectionTasks));
  const currentFocusTask = $derived.by(() => resolveCurrentFocusTask(projectionTasks));
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
      rows.push({
        task,
        depth,
        hasChildren: children.length > 0,
        childCount: children.length,
      });
      if (children.length > 0 && expandedNodeIds.has(task.task_id)) {
        for (const child of children) visit(child, depth + 1);
      }
    };
    visit(rootTask, 0);
    return rows;
  }

  function completedChildCount(taskId: string): number {
    return (childrenByParentId.get(taskId) ?? [])
      .filter((child) => child.status === 'Completed')
      .length;
  }

  function buildTaskSummary(tasks: TaskDto[]) {
    return {
      total: tasks.length,
      completed: tasks.filter((task) => task.status === 'Completed' || task.status === 'Skipped').length,
      active: tasks.filter((task) => ['Running', 'Verifying', 'Repairing'].includes(task.status)).length,
      blocked: tasks.filter((task) => task.status === 'Blocked' || task.status === 'AwaitingApproval').length,
      failed: tasks.filter((task) => task.status === 'Failed').length,
    };
  }

  function resolveCurrentFocusTask(tasks: TaskDto[]): TaskDto | null {
    const priority: TaskStatus[] = ['AwaitingApproval', 'Blocked', 'Repairing', 'Verifying', 'Running', 'Ready'];
    for (const status of priority) {
      const matched = tasks.find((task) => task.status === status && task.kind !== 'Objective');
      if (matched) return matched;
    }
    return tasks.find((task) => task.kind === 'Objective') ?? null;
  }

  function getTaskKindProductLabel(kind: TaskDto['kind']): string {
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

  function getTaskStatusLabel(status: TaskStatus): string {
    switch (status) {
      case 'Draft': return '待规划';
      case 'Ready': return '待执行';
      case 'Running': return '执行中';
      case 'Blocked': return '已暂停';
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

  function getTaskStatusTone(status: TaskStatus): string {
    if (status === 'AwaitingApproval' || status === 'Blocked') return '需要处理';
    if (status === 'Running' || status === 'Verifying' || status === 'Repairing') return '正在推进';
    if (status === 'Completed' || status === 'Skipped') return '已收束';
    if (status === 'Failed') return '需要修复';
    return '等待执行';
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

  let showPlanLedger = $state(true);

  const activePlanRecord = $derived.by(() => {
    const activePlanId = activePlanState?.planId;
    if (!activePlanId) {
      return null;
    }
    return planHistory.find((plan) => plan?.planId === activePlanId) || null;
  });

  const archivedPlans = $derived.by(() => {
    const activePlanId = activePlanState?.planId;
    return planHistory
      .filter((plan) => !!plan && plan.planId !== activePlanId)
      .slice(0, 6);
  });

  const activePlanProgress = $derived.by(() => {
    const plan = activePlanRecord;
    if (!plan || !Array.isArray(plan.items) || plan.items.length === 0) {
      return { total: 0, completed: 0, percent: 0 };
    }
    const total = plan.items.length;
    const completed = plan.items.filter((item) => item.status === 'completed' || item.status === 'skipped').length;
    return {
      total,
      completed,
      percent: Math.round((completed / total) * 100),
    };
  });

  function getPlanStatusLabel(status: string): string {
    // 后端 status 使用 snake_case（如 awaiting_confirmation），i18n key 使用 camelCase（如 awaitingConfirmation）
    const camelStatus = status.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
    const key = `tasks.planStatus.${camelStatus}`;
    const label = i18n.t(key);
    // 如果 key 没有匹配到翻译（返回原 key），则使用 status 或 '未知'
    return label !== key ? label : (status || i18n.t('tasks.planStatus.unknown'));
  }

  function getPlanStatusClass(status: string): string {
    if (status === 'completed') return 'is-completed';
    if (status === 'failed' || status === 'rejected') return 'is-failed';
    if (status === 'executing') return 'is-running';
    if (status === 'partially_completed') return 'is-partial';
    if (status === 'cancelled' || status === 'superseded') return 'is-cancelled';
    return 'is-pending';
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

  function normalizeAnchorText(raw: unknown): string {
    if (typeof raw !== 'string') {
      return '';
    }
    return raw.replace(/\s+/g, ' ').trim();
  }

  function extractUserInputText(message: Message): string {
    const content = normalizeAnchorText(message.content);
    if (content) {
      return content;
    }

    const blocks = Array.isArray(message.blocks) ? message.blocks : [];
    const text = blocks
      .filter((block) => block?.type === 'text' || block?.type === 'thinking')
      .map((block) => (typeof block?.content === 'string' ? block.content : ''))
      .join(' ');
    return normalizeAnchorText(text);
  }

  function isPrimaryUserInput(message: Message): boolean {
    if (message.type !== 'user_input') {
      return false;
    }
    return message?.metadata?.isSupplementary !== true;
  }

  function getTemporalAnchorScore(messageTimestamp: number, anchorTimestamp: number): number {
    const delta = anchorTimestamp - messageTimestamp;
    const isFuture = delta < -2000;
    return Math.abs(delta) + (isFuture ? 200000 : 0);
  }

  function matchUserInputByPromptDigest(messages: Message[], plan: PlanLedgerRecord): Message | null {
    const normalizedDigest = normalizeAnchorText(plan.promptDigest);
    if (!normalizedDigest || normalizedDigest === 'empty') {
      return null;
    }

    const hasEllipsis = normalizedDigest.endsWith('...');
    const digestPrefix = hasEllipsis ? normalizeAnchorText(normalizedDigest.slice(0, -3)) : normalizedDigest;
    if (!digestPrefix) {
      return null;
    }

    const anchorTs = Number.isFinite(plan.createdAt) ? plan.createdAt : Date.now();
    let bestMatch: Message | null = null;
    let bestScore = Number.POSITIVE_INFINITY;

    for (const message of messages) {
      const text = extractUserInputText(message);
      if (!text) {
        continue;
      }

      const exact = text === digestPrefix;
      const prefix = text.startsWith(digestPrefix);
      const include = !hasEllipsis && text.includes(digestPrefix);
      if (!exact && !prefix && !include) {
        continue;
      }

      const textScore = exact ? 0 : prefix ? 1 : 2;
      const score = textScore * 100000 + getTemporalAnchorScore(message.timestamp, anchorTs);
      if (score < bestScore) {
        bestScore = score;
        bestMatch = message;
      }
    }

    return bestMatch;
  }

  function matchUserInputByTimestamp(messages: Message[], anchorTimestamp: number): Message | null {
    let bestMatch: Message | null = null;
    let bestScore = Number.POSITIVE_INFINITY;

    for (const message of messages) {
      const score = getTemporalAnchorScore(message.timestamp, anchorTimestamp);
      if (score < bestScore) {
        bestScore = score;
        bestMatch = message;
      }
    }

    return bestMatch;
  }

  function resolvePlanAnchorMessageId(plan: PlanLedgerRecord): string | null {
    const userInputs = threadMessages.filter((message) => isPrimaryUserInput(message) && Number.isFinite(message.timestamp));
    if (userInputs.length === 0) {
      return null;
    }

    const anchorTs = Number.isFinite(plan.createdAt) ? plan.createdAt : Date.now();
    const normalizedTurnId = typeof plan.turnId === 'string' ? plan.turnId.trim() : '';
    if (normalizedTurnId) {
      const byTurn = userInputs.filter((message) => {
        const metadataTurnId = typeof message?.metadata?.turnId === 'string'
          ? message.metadata.turnId.trim()
          : '';
        return metadataTurnId === normalizedTurnId && message.type === 'user_input';
      });
      if (byTurn.length > 0) {
        const digestMatch = matchUserInputByPromptDigest(byTurn, plan);
        if (digestMatch?.id) {
          return digestMatch.id;
        }
        const byTurnTime = matchUserInputByTimestamp(byTurn, anchorTs);
        if (byTurnTime?.id) {
          return byTurnTime.id;
        }
      }
    }

    const digestMatch = matchUserInputByPromptDigest(userInputs, plan);
    if (digestMatch?.id) {
      return digestMatch.id;
    }

    return matchUserInputByTimestamp(userInputs, anchorTs)?.id || null;
  }

  function jumpToPlanConversation(plan: PlanLedgerRecord): void {
    setCurrentTopTab('thread');
    setCurrentBottomTab('thread');
    const anchorMessageId = resolvePlanAnchorMessageId(plan);
    if (!anchorMessageId) {
      return;
    }
    requestMessageJump(anchorMessageId);
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
            <h3 class="task-overview-title">{proj.root_task.title}</h3>
            {#if proj.root_task.goal && proj.root_task.goal !== proj.root_task.title}
              <p class="task-overview-goal">{proj.root_task.goal}</p>
            {/if}
          </div>
          <span class="tg-status-badge tg-status--{getTaskStatusModifier(proj.aggregate_status)}">
            {getTaskStatusLabel(proj.aggregate_status)}
          </span>
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
          </div>
        {/if}

        {#if currentFocusTask}
          <div class="task-focus-card">
            <span class="task-focus-label">当前焦点</span>
            <div class="task-focus-main">
              <span class="task-focus-title">{currentFocusTask.title}</span>
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
      </section>

      <div class="task-section-header">
        <span>执行结构</span>
        <span class="task-section-meta">{projectionTasks.length} 个节点</span>
      </div>

      <div class="tg-tree" role="tree" aria-label="任务执行结构">
        {#each taskTreeRows as row (row.task.task_id)}
          {@const isExpanded = expandedGraphNodes.has(row.task.task_id)}
          {@const statusIcon = getProjectionStatusIcon(row.task.status)}
          <div
            class="tg-tree-row tg-tree-row--{getTaskStatusModifier(row.task.status)}"
            class:tg-tree-row--focus={currentFocusTask?.task_id === row.task.task_id}
            role="treeitem"
            aria-level={row.depth + 1}
            aria-expanded={row.hasChildren ? isExpanded : undefined}
            aria-selected="false"
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
            <span class="tg-kind-badge">{getTaskKindProductLabel(row.task.kind)}</span>
            <span class="tg-tree-status-icon tg-status-icon--{getTaskStatusModifier(row.task.status)}">
              {#if statusIcon.spinning}
                <Icon name={statusIcon.name} size={14} class="spinning" />
              {:else}
                <Icon name={statusIcon.name} size={14} />
              {/if}
            </span>
            <span class="tg-tree-content">
              <span class="tg-tree-title">{row.task.title}</span>
              {#if row.task.goal && row.task.goal !== row.task.title}
                <span class="tg-tree-goal">{row.task.goal}</span>
              {/if}
            </span>
            <span class="tg-tree-side">
              <span class="tg-tree-state">{getTaskStatusLabel(row.task.status)}</span>
              {#if row.task.kind === 'WorkPackage' && row.childCount > 0}
                <span class="tg-tree-count">{completedChildCount(row.task.task_id)}/{row.childCount}</span>
              {:else if row.childCount > 0}
                <span class="tg-tree-count">{row.childCount}</span>
              {/if}
            </span>
          </div>
        {/each}
      </div>

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
                <span class="tg-attention-title">{task.title}</span>
                <span class="tg-attention-meta">
                  {#if task.kind === 'Decision' && task.decision_payload?.blocked_reason}
                    {task.decision_payload.blocked_reason}
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
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      {#if taskGraph.error}
        <div class="tg-error">{taskGraph.error}</div>
      {/if}
    {/if}
  {/if}

  {#if activePlanState || archivedPlans.length > 0}
    <div class="plan-ledger-card">
      <button
        type="button"
        class="plan-ledger-toggle"
        aria-expanded={showPlanLedger}
        onclick={() => showPlanLedger = !showPlanLedger}
      >
        <span class="plan-ledger-title-wrap">
          <span class="plan-ledger-title">{i18n.t('tasks.planLedger.title')}</span>
          {#if activePlanState}
            <span class="plan-ledger-badge">{i18n.t('tasks.planLedger.currentPlan')}</span>
          {:else}
            <span class="plan-ledger-count">{i18n.t('tasks.planLedger.historyCount', { count: archivedPlans.length })}</span>
          {/if}
        </span>
        <span class="plan-ledger-chevron" class:expanded={showPlanLedger}>
          <Icon name="chevron-right" size={12} />
        </span>
      </button>

      {#if showPlanLedger}
        {#if activePlanState}
          <div class="plan-ledger-current">
            <div class="plan-ledger-summary">
              <span>{activePlanRecord?.summary || i18n.t('tasks.planLedger.executingFallback')}</span>
              {#if activePlanRecord}
                <span class="plan-status {getPlanStatusClass(activePlanRecord.status)}">
                  {getPlanStatusLabel(activePlanRecord.status)}
                </span>
              {/if}
            </div>
            <div class="plan-ledger-meta">
              {#if activePlanRecord}
                <span>{i18n.t('tasks.planLedger.modeLabel', { mode: activePlanRecord.mode === 'deep' ? i18n.t('tasks.planLedger.modeDeep') : i18n.t('tasks.planLedger.modeShallow') })}</span>
                <span>{i18n.t('tasks.planLedger.versionLabel', { version: activePlanRecord.version })}</span>
                <span>{i18n.t('tasks.planLedger.updatedLabel', { time: formatTimestamp(activePlanRecord.updatedAt) })}</span>
              {:else}
                <span>{i18n.t('tasks.planLedger.updatedLabel', { time: formatTimestamp(activePlanState.updatedAt) })}</span>
              {/if}
            </div>
            {#if activePlanProgress.total > 0}
              <div class="plan-ledger-progress-wrap">
                <span class="plan-ledger-progress-label">{activePlanProgress.completed}/{activePlanProgress.total}</span>
                <div class="plan-ledger-progress">
                  <div class="plan-ledger-progress-fill" style="width: {activePlanProgress.percent}%"></div>
                </div>
              </div>
            {/if}
          </div>
        {/if}

        {#if archivedPlans.length > 0}
          <div class="plan-history-list">
            {#each archivedPlans as plan (plan.planId)}
              <button
                type="button"
                class="plan-history-item clickable"
                title={i18n.t('tasks.planLedger.jumpTitle')}
                onclick={() => jumpToPlanConversation(plan)}
              >
                <div class="plan-history-main">
                  <span class="plan-history-summary">{plan.summary || i18n.t('tasks.planLedger.unnamedPlan')}</span>
                  <span class="plan-status {getPlanStatusClass(plan.status)}">
                    {getPlanStatusLabel(plan.status)}
                  </span>
                </div>
                <div class="plan-history-meta">
                  <span>{plan.mode === 'deep' ? i18n.t('tasks.planLedger.modeDeep') : i18n.t('tasks.planLedger.modeShallow')}</span>
                  <span>v{plan.version}</span>
                  <span>{formatTimestamp(plan.updatedAt)}</span>
                </div>
              </button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  {#if !hasTaskProjection}
    <div class="empty-state">
      <div class="empty-icon-wrap">
        <Icon name="circleOutline" size={32} class="empty-icon" />
      </div>
      <div class="empty-text">{i18n.t('tasks.empty.title')}</div>
      {#if activePlanState || archivedPlans.length > 0}
        <div class="empty-hint">{i18n.t('tasks.empty.hintWithPlan')}</div>
      {:else}
        <div class="empty-hint">{i18n.t('tasks.empty.hintNoPlan')}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* ========== 面板容器 ========== */
  .tasks-panel {
    /* panel-content-scrollable 已经包含了 padding, flex, overflow */
    gap: var(--space-4);
  }

  .plan-ledger-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-sm);
  }

  .plan-ledger-toggle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    width: 100%;
    border: none;
    background: transparent;
    color: inherit;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: left;
  }

  .plan-ledger-toggle:hover {
    background: var(--surface-hover);
  }

  .plan-ledger-title-wrap {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .plan-ledger-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .plan-ledger-badge {
    font-size: var(--text-2xs);
    color: var(--primary);
    background: var(--primary-muted);
    border: 1px solid color-mix(in srgb, var(--primary) 30%, var(--border));
    border-radius: 999px;
    padding: 2px 8px;
  }

  .plan-ledger-count {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .plan-ledger-chevron {
    display: inline-flex;
    align-items: center;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .plan-ledger-chevron.expanded {
    transform: rotate(90deg);
  }

  .plan-ledger-current {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2) var(--space-2);
  }

  .plan-ledger-summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    font-size: var(--text-sm);
    color: var(--foreground);
  }

  .plan-ledger-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .plan-status {
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
  }

  .plan-status.is-running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .plan-status.is-completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .plan-status.is-failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .plan-status.is-partial {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .plan-status.is-cancelled {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .plan-status.is-pending {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .plan-ledger-progress-wrap {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .plan-ledger-progress-label {
    min-width: 50px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .plan-ledger-progress {
    flex: 1;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .plan-ledger-progress-fill {
    height: 100%;
    border-radius: inherit;
    background: var(--primary);
    transition: width 200ms ease;
  }

  .plan-history-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    font-size: var(--text-xs);
  }

  .plan-history-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
  }

  .plan-history-item.clickable {
    width: 100%;
    text-align: left;
    color: inherit;
    cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .plan-history-item.clickable:hover {
    background: var(--surface-hover);
    border-color: color-mix(in srgb, var(--primary) 28%, var(--border));
  }

  .plan-history-main {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .plan-history-summary {
    font-size: var(--text-sm);
    color: var(--foreground);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .plan-history-meta {
    display: flex;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  /* ========== 空状态 ========== */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
    width: 100%;
    box-sizing: border-box;
  }

  .empty-icon-wrap {
    opacity: 0.2;
    margin-bottom: var(--space-4);
  }

  .empty-text {
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.6;
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

  .tg-status--blocked {
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

  .tg-error {
    font-size: var(--text-xs);
    color: var(--error);
    padding: var(--space-2) var(--space-3);
    background: var(--error-muted);
    border: 1px solid color-mix(in srgb, var(--error) 32%, var(--border));
    border-radius: var(--radius-md);
  }

</style>
