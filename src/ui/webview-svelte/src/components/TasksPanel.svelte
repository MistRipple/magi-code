<script lang="ts">
  import { getState, requestMessageJump, setCurrentBottomTab, setCurrentTopTab } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import type { Task, SubTaskItem, ActivePlanState, PlanLedgerRecord, Message } from '../types/message';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import ErrorDetailPopover from './ErrorDetailPopover.svelte';
  import { i18n } from '../stores/i18n.svelte';

  const appState = getState();

  // 数据源：task.subTasks 是唯一的 todo 数据源（来自 stateUpdate 全量同步）
  // missionPlanMap 按 missionId 存储 Worker 分组元信息（workerId、responsibility）
  const tasks = $derived(ensureArray(appState.tasks) as Task[]);
  const missionPlanMap = $derived(appState.missionPlan);
  const appPayload = $derived((appState.appState || {}) as Record<string, unknown>);
  const activePlanState = $derived((appPayload.activePlan || null) as ActivePlanState | null);
  const planHistory = $derived(ensureArray(appPayload.planHistory) as PlanLedgerRecord[]);
  const threadMessages = $derived(ensureArray(appState.threadMessages) as Message[]);

  // 折叠状态
  let expandedTasks = $state<Set<string>>(new Set());
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

  // 概览统计
  const stats = $derived.by(() => {
    const total = tasks.length;
    const completed = tasks.filter(t => t.status === 'completed').length;
    const running = tasks.filter(t => t.status === 'running').length;
    const failed = tasks.filter(t => t.status === 'failed').length;
    const deliveryFailed = tasks.filter(t => t.deliveryStatus === 'failed').length;
    const pending = total - completed - running - failed;
    const progress = total > 0 ? Math.round((completed / total) * 100) : 0;
    return { total, completed, running, failed, pending, progress, deliveryFailed };
  });

  // 将 task.subTasks 按 assignmentId 分组，并关联 missionPlan 的 Worker 元信息
  const enrichedTasks = $derived.by(() => {
    return tasks.map(task => {
      const taskId = task.id;
      const subTasks = task.subTasks;

      // 按 assignmentId 分组 subTasks
      const groupMap = new Map<string, { workerId: string; responsibility: string; todos: SubTaskItem[] }>();
      for (const st of subTasks) {
        const aid = st.assignmentId || '_default';
        if (!groupMap.has(aid)) {
          groupMap.set(aid, { workerId: st.assignedWorker || '', responsibility: '', todos: [] });
        }
        groupMap.get(aid)!.todos.push(st);
      }

      // 用 missionPlan 的 assignment 补充 Worker 元信息（responsibility）
      const plan = missionPlanMap.get(taskId) || missionPlanMap.get(task.missionId);
      if (plan?.assignments) {
        for (const a of plan.assignments) {
          const group = groupMap.get(a.id);
          if (group) {
            group.workerId = group.workerId || a.workerId;
            group.responsibility = a.responsibility || '';
          }
        }
      }

      // 转换为数组
      const workerGroups = Array.from(groupMap.entries()).map(([aid, g]) => ({
        id: aid,
        workerId: g.workerId,
        responsibility: g.responsibility,
        todos: g.todos,
      }));

      // 统计
      const todoTotal = subTasks.length;
      const todoCompleted = subTasks.filter(t => t.status === 'completed' || t.status === 'skipped').length;
      const todoProgress = todoTotal > 0 ? Math.round((todoCompleted / todoTotal) * 100) : task.progress;

      return { ...task, workerGroups, todoTotal, todoCompleted, todoProgress };
    });
  });

  const workerColors: Record<string, string> = {
    claude: 'var(--color-claude)',
    codex: 'var(--color-codex)',
    gemini: 'var(--color-gemini)',
  };

  function toggleExpand(taskId: string) {
    const next = new Set(expandedTasks);
    if (next.has(taskId)) next.delete(taskId);
    else next.add(taskId);
    expandedTasks = next;
  }

  // 自动展开 running 任务（仅在有新增 running task 需要展开时才写入，避免高频 stateUpdate 触发无效渲染）
  $effect(() => {
    const runningIds = tasks.filter(t => t.status === 'running').map(t => t.id);
    const needsExpand = runningIds.some(id => !expandedTasks.has(id));
    if (needsExpand) {
      const next = new Set(expandedTasks);
      for (const id of runningIds) next.add(id);
      expandedTasks = next;
    }
  });

  function startTask(taskId: string) {
    vscode.postMessage({ type: 'startTask', taskId });
  }

  function deleteTask(taskId: string) {
    vscode.postMessage({ type: 'deleteTask', taskId });
  }

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

  function getDeliveryStatusLabel(status?: string): string {
    if (!status) return '';
    const key = `tasks.deliveryStatus.${status}`;
    const label = i18n.t(key);
    return label !== key ? label : status;
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

<div class="tasks-panel">
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

  {#if enrichedTasks.length === 0}
    <!-- 空状态 -->
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
  {:else}
    <!-- 概览统计条 -->
    {#if stats.total > 0}
      <div class="overview-bar">
        <div class="overview-stats">
          <span class="overview-label">{i18n.t('tasks.overview.completed', { completed: stats.completed, total: stats.total })}</span>
          {#if stats.running > 0}
            <span class="overview-dot running"></span>
            <span class="overview-running">{i18n.t('tasks.overview.running', { count: stats.running })}</span>
          {/if}
          {#if stats.failed > 0}
            <span class="overview-dot failed"></span>
            <span class="overview-failed">{i18n.t('tasks.overview.failed', { count: stats.failed })}</span>
          {/if}
          {#if stats.deliveryFailed > 0}
            <span class="overview-dot delivery-failed"></span>
            <span class="overview-delivery-failed">{i18n.t('tasks.overview.deliveryFailed', { count: stats.deliveryFailed })}</span>
          {/if}
        </div>
        <div class="overview-progress">
          <div class="overview-progress-fill" style="width: {stats.progress}%"></div>
        </div>
      </div>
    {/if}

    <!-- 任务列表 -->
    <div class="tasks-list">
      {#each enrichedTasks as task, taskIdx (task.id)}
        {@const isExpanded = expandedTasks.has(task.id)}
        {@const hasChildren = task.todoTotal > 0}
        <div class="task-card" class:is-running={task.status === 'running'} class:is-done={task.status === 'completed'} class:is-failed={task.status === 'failed'} class:is-cancelled={task.status === 'cancelled'}>
          <!-- 任务头部 -->
          <div class="task-header" role="button" tabindex="0" onclick={() => hasChildren && toggleExpand(task.id)} onkeydown={(e) => e.key === 'Enter' && hasChildren && toggleExpand(task.id)}>
            <!-- 折叠箭头 -->
            <span class="task-chevron" class:expanded={isExpanded} class:hidden={!hasChildren}>
              <Icon name="chevron-right" size={12} />
            </span>
            <!-- 任务序号 -->
            <span class="task-seq">{taskIdx + 1}.</span>
            <!-- 状态图标 -->
            <span class="task-status-icon status-{task.status}">
              {#if task.status === 'running'}
                <Icon name="loader" size={14} class="spinning" />
              {:else if task.status === 'completed'}
                <Icon name="check-circle" size={14} />
              {:else if task.status === 'failed'}
                <Icon name="x-circle" size={14} />
              {:else if task.status === 'cancelled'}
                <Icon name="skip-forward" size={14} />
              {:else}
                <Icon name="circleOutline" size={14} />
              {/if}
            </span>
            <!-- 任务信息 -->
            <div class="task-body">
              <span class="task-name">{task.name || task.prompt || i18n.t('tasks.task.unnamed')}</span>
              {#if task.todoTotal > 0}
                <span class="task-count">{task.todoCompleted}/{task.todoTotal}</span>
              {/if}
              {#if task.deliveryStatus === 'failed' || task.deliveryStatus === 'blocked'}
                <span class="task-delivery status-{task.deliveryStatus}">
                  {getDeliveryStatusLabel(task.deliveryStatus)}
                  {#if task.deliveryDetails || task.deliverySummary}
                    <ErrorDetailPopover text={task.deliveryDetails || task.deliverySummary} maxInlineChars={72} />
                  {/if}
                </span>
              {/if}
              {#if task.status === 'failed' && task.failureReason}
                <span class="task-error">
                  <ErrorDetailPopover text={task.failureReason} maxInlineChars={72} />
                </span>
              {/if}
            </div>
            <!-- 操作按钮（hover 显示） -->
            <div class="task-actions">
              {#if task.status === 'pending' || task.status === 'paused'}
                <button class="action-btn" title={i18n.t('tasks.task.startTitle')} onclick={(e) => { e.stopPropagation(); startTask(task.id); }}>
                  <Icon name="play" size={12} />
                </button>
              {/if}
              {#if task.status !== 'running'}
                <button class="action-btn danger" title={i18n.t('tasks.task.deleteTitle')} onclick={(e) => { e.stopPropagation(); deleteTask(task.id); }}>
                  <Icon name="trash" size={12} />
                </button>
              {/if}
            </div>
          </div>

          <!-- 进度条（有子任务时显示） -->
          {#if task.todoTotal > 0}
            <div class="task-progress-bar">
              <div class="task-progress-fill" style="width: {task.todoProgress}%"></div>
            </div>
          {/if}

          <!-- 展开的子任务/Todo -->
          {#if isExpanded && hasChildren}
            <div class="task-children">
              {#each task.workerGroups as group (group.id)}
                <!-- Worker 分组标题（多 Worker 时显示） -->
                {#if task.workerGroups.length > 1}
                  <div class="worker-group-label">
                    <span class="worker-dot" style="background: {workerColors[group.workerId] || 'var(--foreground-muted)'}"></span>
                    <span>{group.workerId}</span>
                    <span class="worker-responsibility">{group.responsibility || ''}</span>
                  </div>
                {/if}
                {#each group.todos as todo, todoIdx (todo.id)}
                  {@const seqNum = todoIdx + 1}
                  <div class="todo-row" class:is-done={todo.status === 'completed' || todo.status === 'skipped'} class:is-failed={todo.status === 'failed'} class:is-running={todo.status === 'running' || todo.status === 'in_progress'}>
                    <span class="todo-seq">{seqNum}.</span>
                    <span class="todo-check">
                      {#if todo.status === 'completed' || todo.status === 'skipped'}
                        <Icon name="check" size={12} />
                      {:else if todo.status === 'running' || todo.status === 'in_progress'}
                        <Icon name="loader" size={12} class="spinning" />
                      {:else if todo.status === 'failed'}
                        <Icon name="x-circle" size={12} />
                      {:else if todo.status === 'blocked'}
                        <Icon name="alert-circle" size={12} />
                      {:else}
                        <span class="todo-circle"></span>
                      {/if}
                    </span>
                    <span class="todo-text">{todo.description || i18n.t('tasks.subtask.unnamed')}</span>
                    {#if task.workerGroups.length <= 1 && group.workerId}
                      <span class="todo-worker" style="color: {workerColors[group.workerId] || 'var(--foreground-muted)'}">{group.workerId}</span>
                    {/if}
                  </div>
                {/each}
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* ========== 面板容器 ========== */
  .tasks-panel {
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow-y: auto;
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .plan-ledger-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
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

  /* ========== 概览统计条 ========== */
  .overview-bar {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .overview-stats {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .overview-label {
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .overview-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .overview-dot.running { background: var(--primary); }
  .overview-dot.failed { background: var(--error); }
  .overview-dot.delivery-failed { background: var(--warning); }

  .overview-running { color: var(--primary); }
  .overview-failed { color: var(--error); }
  .overview-delivery-failed { color: var(--warning); }

  .overview-progress {
    height: 3px;
    background: var(--surface-3);
    border-radius: var(--radius-full);
    overflow: hidden;
  }

  .overview-progress-fill {
    height: 100%;
    background: var(--success);
    border-radius: var(--radius-full);
    transition: width var(--transition-normal);
  }

  /* ========== 任务列表 ========== */
  .tasks-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  /* ========== 任务卡片 ========== */
  .task-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    transition: all var(--transition-fast);
    overflow: hidden;
  }

  .task-card:hover {
    border-color: color-mix(in srgb, var(--foreground) 20%, var(--border));
  }

  .task-card.is-running {
    border-color: color-mix(in srgb, var(--primary) 40%, var(--border));
  }

  .task-card.is-done {
    opacity: 0.6;
  }

  .task-card.is-failed {
    border-color: color-mix(in srgb, var(--error) 30%, var(--border));
  }

  .task-card.is-cancelled {
    opacity: 0.5;
  }

  /* ========== 任务头部（可点击） ========== */
  .task-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-3);
    width: 100%;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    user-select: none;
  }

  .task-header:hover .task-actions {
    opacity: 1;
  }

  /* ========== 折叠箭头 ========== */
  .task-chevron {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .task-chevron.expanded {
    transform: rotate(90deg);
  }

  .task-chevron.hidden {
    visibility: hidden;
  }

  /* ========== 任务序号 ========== */
  .task-seq {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    min-width: 18px;
    flex-shrink: 0;
    font-weight: var(--font-medium);
    font-variant-numeric: tabular-nums;
  }

  /* ========== 状态图标 ========== */
  .task-status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 18px;
    height: 18px;
  }

  .task-status-icon.status-running { color: var(--primary); }
  .task-status-icon.status-completed { color: var(--success); }
  .task-status-icon.status-failed { color: var(--error); }
  .task-status-icon.status-cancelled { color: var(--foreground-muted); }
  .task-status-icon.status-pending { color: var(--foreground-muted); }
  .task-status-icon.status-paused { color: var(--warning); }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ========== 任务体 ========== */
  .task-body {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .task-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .task-count {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    padding: 1px 5px;
    background: var(--surface-3);
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .task-delivery {
    font-size: var(--text-2xs);
    color: var(--warning);
    background: var(--warning-muted);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, var(--border));
    padding: 1px 6px;
    border-radius: var(--radius-full);
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    max-width: 60%;
    min-width: 0;
  }

  .task-error {
    font-size: var(--text-2xs);
    color: var(--error);
    max-width: 60%;
    min-width: 0;
    display: inline-flex;
    align-items: center;
  }

  /* ========== 操作按钮 ========== */
  .task-actions {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    opacity: 0;
    transition: opacity var(--transition-fast);
    flex-shrink: 0;
  }

  .action-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: none;
    background: var(--surface-3);
    color: var(--foreground-muted);
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .action-btn:hover {
    background: var(--surface-active);
    color: var(--foreground);
  }

  .action-btn.danger:hover {
    background: var(--error-muted);
    color: var(--error);
  }

  /* ========== 进度条 ========== */
  .task-progress-bar {
    height: 2px;
    background: var(--surface-2);
  }

  .task-progress-fill {
    height: 100%;
    background: var(--primary);
    transition: width var(--transition-normal);
  }

  /* ========== 子任务区域 ========== */
  .task-children {
    padding: 0 var(--space-3) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  /* ========== Worker 分组 ========== */
  .worker-group-label {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    padding: var(--space-2) 0 var(--space-1);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .worker-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .worker-responsibility {
    opacity: 0.7;
    font-style: italic;
    text-transform: none;
    letter-spacing: normal;
  }

  /* ========== Todo 行（Checklist 风格） ========== */
  .todo-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-xs);
    font-size: var(--text-sm);
    color: var(--foreground);
    transition: background var(--transition-fast);
  }

  .todo-row:hover {
    background: var(--surface-hover);
  }

  .todo-row.is-done {
    opacity: 0.5;
  }

  .todo-row.is-done .todo-text {
    text-decoration: line-through;
  }

  .todo-row.is-failed {
    color: var(--error);
  }

  .todo-row.is-running {
    color: var(--primary);
  }

  .todo-seq {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    min-width: 18px;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .todo-check {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    color: inherit;
  }

  .todo-row.is-done .todo-check {
    color: var(--success);
  }

  .todo-circle {
    width: 8px;
    height: 8px;
    border: 1.5px solid var(--foreground-muted);
    border-radius: var(--radius-full);
  }

  .todo-text {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .todo-worker {
    font-size: var(--text-2xs);
    flex-shrink: 0;
    font-weight: var(--font-medium);
  }

</style>
