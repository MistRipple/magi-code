<script lang="ts">
  import type { WorkerRuntimeStatus } from '../lib/worker-panel-state';
  import type { CardWorkerStatus, WorkerTaskCardData } from '../lib/worker-card-view-model';
  import { getState, getEnabledAgents, setCurrentBottomTab } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { formatDuration } from '../lib/utils';
  import { resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';



  interface Props {
    card: WorkerTaskCardData;
    readOnly?: boolean;
    compact?: boolean;
    messageTimestamp?: number;
    startedAtOverride?: number;
    runtimeStatus?: WorkerRuntimeStatus;
  }

  let {
    card,
    readOnly: _readOnly = false,
    compact = false,
    messageTimestamp,
    startedAtOverride,
    runtimeStatus,
  }: Props = $props();

  let nowTick = $state(Date.now());





  const appState = getState();
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);
  const currentLocale = $derived(i18n.locale);

  function normalizeText(value: unknown): string {
    return typeof value === 'string' ? value.trim() : '';
  }

  function normalizeCardStatus(
    status: CardWorkerStatus | WorkerRuntimeStatus | null | undefined,
  ): CardWorkerStatus | null {
    switch (status) {
      case 'pending':
      case 'awaiting_approval':
      case 'review_required':
      case 'running':
      case 'blocked':
      case 'completed':
      case 'failed':
      case 'cancelled':
      case 'skipped':
        return status;
      default:
        return null;
    }
  }

  function resolveStatusConfig(status: CardWorkerStatus | null): {
    key: string;
    tone: 'pending' | 'running' | 'paused' | 'success' | 'danger';
    icon: 'hourglass' | 'play' | 'alert-circle' | 'check-circle' | 'x-circle' | 'skip-forward';
  } {
    switch (status) {
      case 'running':
        return { key: 'subTaskSummaryCard.status.running', tone: 'running', icon: 'play' };
      case 'awaiting_approval':
        return { key: 'subTaskSummaryCard.status.awaitingApproval', tone: 'paused', icon: 'alert-circle' };
      case 'review_required':
        return { key: 'subTaskSummaryCard.status.reviewRequired', tone: 'paused', icon: 'alert-circle' };
      case 'blocked':
        return { key: 'subTaskSummaryCard.status.blocked', tone: 'danger', icon: 'alert-circle' };
      case 'completed':
        return { key: 'subTaskSummaryCard.status.completed', tone: 'success', icon: 'check-circle' };
      case 'failed':
        return { key: 'subTaskSummaryCard.status.failed', tone: 'danger', icon: 'x-circle' };
      case 'cancelled':
        return { key: 'subTaskSummaryCard.status.cancelled', tone: 'danger', icon: 'x-circle' };
      case 'skipped':
        return { key: 'subTaskSummaryCard.status.skipped', tone: 'pending', icon: 'skip-forward' };
      case 'pending':
      default:
        return { key: 'subTaskSummaryCard.status.pending', tone: 'pending', icon: 'hourglass' };
    }
  }

  function resolveStatusLabel(status: CardWorkerStatus | null): string {
    return i18n.t(resolveStatusConfig(status).key);
  }

  const rawWorker = $derived.by(() => (
    normalizeText(card.workerTabId)
    || normalizeText(card.worker)
    || 'orchestrator'
  ));
  const workerRoleSource = $derived.by(() => (
    rawWorker === 'orchestrator'
      ? null
      : resolveWorkerRoleSource(rawWorker, enabledAgents, registrySnapshot)
  ));
  const displayWorkerId = $derived.by(() => (
    normalizeText(workerRoleSource?.templateId)
    || rawWorker
  ));

  const workerDisplayName = $derived.by(() => {
    const locale = currentLocale;
    if (rawWorker === 'orchestrator') {
      return locale === 'en-US' ? 'Orchestrator' : i18n.t('workerBadge.role.orchestrator');
    }
    return resolveWorkerDisplayName(displayWorkerId, enabledAgents, registrySnapshot, (key) => i18n.t(key));
  });
  const visualInfo = $derived.by(() => getAgentVisualInfo(displayWorkerId, workerRoleSource?.colorToken));

  const instructionText = $derived.by(() => (
    normalizeText(card.instruction) === 'worker_task_queue'
      ? i18n.t('subTaskSummaryCard.workerTaskQueue')
      : (
        normalizeText(card.instruction)
        || (normalizeText(card.description) === 'worker_task_queue'
          ? i18n.t('subTaskSummaryCard.workerTaskQueue')
          : normalizeText(card.description))
      )
  ));

  const workerIdentifiers = $derived.by(() => new Set([
    normalizeText(card.worker),
    normalizeText(card.workerTabId),
    rawWorker,
    displayWorkerId,
    normalizeText(workerRoleSource?.engineId),
    displayWorkerId ? `task-worker-${displayWorkerId}` : '',
  ].filter(Boolean)));

  function resolveVisibleTaskText(value: unknown): string {
    const text = normalizeText(value);
    if (!text || workerIdentifiers.has(text) || text.startsWith('task-worker-')) {
      return instructionText || i18n.t('provider.subTaskFallbackTitle');
    }
    return text;
  }

  const titleText = $derived.by(() => {
    return resolveVisibleTaskText(card.title);
  });





  const currentStatus = $derived.by(() => (
    normalizeCardStatus(runtimeStatus)
    || normalizeCardStatus(card.status)
    || 'pending'
  ));

  const statusConfig = $derived.by(() => resolveStatusConfig(currentStatus));

  $effect(() => {
    const startedAt = typeof startedAtOverride === 'number' && Number.isFinite(startedAtOverride) && startedAtOverride > 0
      ? startedAtOverride
      : (typeof card.startedAt === 'number' && Number.isFinite(card.startedAt) && card.startedAt > 0
        ? card.startedAt
        : null);
    if (currentStatus !== 'running' || !startedAt) {
      return;
    }
    nowTick = Date.now();
    const timer = window.setInterval(() => {
      nowTick = Date.now();
    }, 1000);
    return () => {
      window.clearInterval(timer);
    };
  });



  const liveActivityText = $derived.by(() => {
    const text = normalizeText(card.liveActivity);
    return (text && currentStatus === 'running') ? text : '';
  });

  const toolUseCountValue = $derived.by(() =>
    typeof card.toolUseCount === 'number' && card.toolUseCount > 0
      ? card.toolUseCount
      : 0
  );

  const taskQueue = $derived.by(() => (
    Array.isArray(card.taskQueue)
      ? card.taskQueue
        .filter((item) => normalizeText(item?.title).length > 0)
        .map((item) => ({
          ...item,
          title: resolveVisibleTaskText(item.title),
          status: normalizeCardStatus(item.status) || 'pending',
        }))
      : []
  ));

  const currentTask = $derived.by(() => (
    taskQueue.find((item) => item.isCurrent)
    || taskQueue.find((item) => item.status === 'running')
    || taskQueue.find((item) => item.status === 'pending')
    || null
  ));

  const queueProgressText = $derived.by(() => {
    const totalFromSummary = card.progressSummary?.totalTaskCount;
    const total = typeof totalFromSummary === 'number' && Number.isFinite(totalFromSummary) && totalFromSummary > 0
      ? Math.floor(totalFromSummary)
      : taskQueue.length;
    if (total <= 0) {
      return '';
    }
    const currentIndex = currentTask
      ? taskQueue.findIndex((item) => item === currentTask) + 1
      : 0;
    const completedFromSummary = card.progressSummary?.completedTaskCount;
    const completedCount = typeof completedFromSummary === 'number' && Number.isFinite(completedFromSummary)
      ? Math.max(0, Math.floor(completedFromSummary))
      : taskQueue.filter((item) => item.status === 'completed').length;
    const current = currentIndex > 0
      ? currentIndex
      : Math.min(total, Math.max(1, completedCount));
    return i18n.t('subTaskSummaryCard.laneProgress', { current, total });
  });

  const currentTaskText = $derived.by(() => (
    liveActivityText
    || normalizeText(currentTask?.title)
  ));

  const summaryText = $derived.by(() => normalizeText(card.summary));

  const fileChangeCount = $derived.by(() => (
    typeof card.fileChangeCount === 'number' && Number.isFinite(card.fileChangeCount) && card.fileChangeCount > 0
      ? Math.floor(card.fileChangeCount)
      : 0
  ));

  const resolvedDuration = $derived.by(() => {
    if (typeof card.duration === 'number' && Number.isFinite(card.duration) && card.duration >= 0) {
      return card.duration;
    }
    if (typeof card.duration === 'string' && card.duration.trim().length > 0) {
      return card.duration.trim();
    }
    const startedAt = typeof startedAtOverride === 'number' && Number.isFinite(startedAtOverride) && startedAtOverride > 0
      ? startedAtOverride
      : (typeof card.startedAt === 'number' && Number.isFinite(card.startedAt) && card.startedAt > 0
        ? card.startedAt
        : null);
    if (!startedAt) {
      return '';
    }
    const endAt = currentStatus === 'running'
      ? nowTick
      : (typeof messageTimestamp === 'number' && Number.isFinite(messageTimestamp) && messageTimestamp > 0
        ? messageTimestamp
        : Date.now());
    if (endAt <= startedAt) {
      return '';
    }
    return formatDuration(endAt - startedAt);
  });




  const targetWorkerTab = $derived.by(() => (
    normalizeText(resolveWorkerRoleSource(normalizeText(card.workerTabId), enabledAgents, registrySnapshot)?.templateId)
    || normalizeText(card.workerTabId)
    || displayWorkerId
  ));

  const isClickable = $derived(targetWorkerTab !== 'orchestrator' && targetWorkerTab.length > 0);

  function openWorkerTab() {
    if (!isClickable) {
      return;
    }
    setCurrentBottomTab(targetWorkerTab);
  }
</script>

<button
  type="button"
  class="worker-progress-card"
  class:compact
  class:is-clickable={isClickable}
  data-worker={rawWorker}
  data-status={currentStatus}
  onclick={openWorkerTab}
  disabled={!isClickable}
  aria-label={isClickable ? i18n.t('subTaskSummaryCard.clickToView', { workerLabel: workerDisplayName }) : undefined}
>
  <div class="worker-progress-card__accent" style={`background:${visualInfo.color};`} aria-hidden="true"></div>

  <div class="worker-progress-card__body">
    <div class="worker-progress-card__header">
      <div class="worker-progress-card__identity">
        <span class="worker-progress-card__icon" style={`color:${visualInfo.color}; background:${visualInfo.muted};`}>
          <Icon name="bot" size={14} />
        </span>
        <div class="worker-progress-card__meta">
          <span class="worker-progress-card__worker">{workerDisplayName || i18n.t('subTaskSummaryCard.defaultExecutor')}</span>
          <span class="worker-progress-card__title">{titleText}</span>
        </div>
      </div>

      <div class="worker-progress-card__statusline">
        {#if card.isResumed}
          <span class="worker-progress-card__tag">{i18n.t('subTaskSummaryCard.resumedBadge')}</span>
        {/if}
        {#if typeof card.waveIndex === 'number' && Number.isFinite(card.waveIndex)}
          <span class="worker-progress-card__tag">{i18n.t('subTaskSummaryCard.waveTitle', { index: card.waveIndex + 1 })}</span>
        {/if}
        {#if resolvedDuration}
          <span class="worker-progress-card__duration">{resolvedDuration}</span>
        {/if}
        <span class={`worker-progress-card__status worker-progress-card__status--${statusConfig.tone}`}>
          <Icon name={statusConfig.icon} size={12} />
          <span>{i18n.t(statusConfig.key)}</span>
        </span>
      </div>
    </div>

    {#if liveActivityText}
      <div class="worker-progress-card__activity">
        <span class="worker-progress-card__activity-dot" aria-hidden="true"></span>
        <span class="worker-progress-card__activity-text">{liveActivityText}</span>
        {#if toolUseCountValue > 0}
          <span class="worker-progress-card__stat">{toolUseCountValue} tools</span>
        {/if}
      </div>
    {:else if toolUseCountValue > 0 && currentStatus === 'running'}
      <div class="worker-progress-card__activity">
        <span class="worker-progress-card__activity-dot" aria-hidden="true"></span>
        <span class="worker-progress-card__stat">{toolUseCountValue} tool uses</span>
      </div>
    {/if}

    {#if instructionText}
      <div class="worker-progress-card__instruction">
        <span class="worker-progress-card__instruction-label">{i18n.t('subTaskSummaryCard.section.instruction')}</span>
        <p class="worker-progress-card__instruction-text">{instructionText}</p>
      </div>
    {/if}

    {#if currentTaskText || queueProgressText}
      <div class="worker-progress-card__facts">
        {#if currentTaskText}
          <span class="worker-progress-card__fact">
            <span class="worker-progress-card__fact-label">{i18n.t('subTaskSummaryCard.currentTask')}</span>
            <span class="worker-progress-card__fact-value">{currentTaskText}</span>
          </span>
        {/if}
        {#if queueProgressText}
          <span class="worker-progress-card__fact">{queueProgressText}</span>
        {/if}
      </div>
    {/if}

    {#if taskQueue.length > 0}
      <div class="worker-progress-card__queue">
        <div class="worker-progress-card__section-head">
          <span>{i18n.t('subTaskSummaryCard.section.taskQueue')}</span>
          {#if queueProgressText}
            <span>{queueProgressText}</span>
          {/if}
        </div>
        <div class="worker-progress-card__queue-list">
          {#each taskQueue as task (`${task.taskId || task.title}:${task.seq || 0}`)}
            {@const taskStatusConfig = resolveStatusConfig(task.status)}
            <div class="worker-progress-card__queue-row" class:is-current={task.isCurrent}>
              <span class={`worker-progress-card__queue-icon worker-progress-card__queue-icon--${taskStatusConfig.tone}`}>
                <Icon name={taskStatusConfig.icon} size={12} />
              </span>
              <span class="worker-progress-card__queue-title">{task.title}</span>
              <span class="worker-progress-card__queue-status">{resolveStatusLabel(task.status)}</span>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    {#if summaryText || fileChangeCount > 0}
      <div class="worker-progress-card__summary">
        {#if summaryText}
          <div class="worker-progress-card__section-head">
            <span>{i18n.t('subTaskSummaryCard.section.summary')}</span>
          </div>
          <p class="worker-progress-card__summary-text">{summaryText}</p>
        {/if}
        {#if fileChangeCount > 0}
          <span class="worker-progress-card__stat">{i18n.t('subTaskSummaryCard.fileChangeCount', { count: fileChangeCount })}</span>
        {/if}
      </div>
    {/if}
  </div>
</button>

<style>
  .worker-progress-card {
    width: 100%;
    display: grid;
    grid-template-columns: 3px minmax(0, 1fr);
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    text-align: left;
    color: inherit;
    overflow: hidden;
    transition: border-color 0.18s ease, background 0.18s ease, transform 0.18s ease;
  }

  .worker-progress-card:disabled {
    cursor: default;
    opacity: 1;
  }

  .worker-progress-card.is-clickable:not(:disabled):hover {
    border-color: var(--primary);
    background: color-mix(in srgb, var(--assistant-message-bg) 88%, var(--primary) 12%);
  }

  .worker-progress-card__accent {
    min-height: 100%;
  }

  .worker-progress-card__body {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
  }

  .worker-progress-card.compact .worker-progress-card__body {
    gap: var(--space-2);
    padding: var(--space-3);
  }

  .worker-progress-card__header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .worker-progress-card__identity {
    min-width: 0;
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
  }

  .worker-progress-card__icon {
    width: 24px;
    height: 24px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .worker-progress-card__meta {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .worker-progress-card__worker {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: 1.2;
  }

  .worker-progress-card__title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    line-height: 1.4;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }

  .worker-progress-card__statusline {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: var(--space-2);
    flex-shrink: 0;
  }

  .worker-progress-card__tag,
  .worker-progress-card__duration {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .worker-progress-card__tag {
    padding: 2px 6px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--background-elevated);
  }

  .worker-progress-card__status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
  }

  .worker-progress-card__status--pending {
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
  }

  .worker-progress-card__status--running {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  .worker-progress-card__status--paused {
    color: var(--warning);
    background: color-mix(in srgb, var(--warning) 14%, transparent);
  }

  .worker-progress-card__status--success {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }

  .worker-progress-card__status--danger {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 14%, transparent);
  }

  /* Live Activity 指示器 */
  .worker-progress-card__activity {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .worker-progress-card__activity-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--primary);
    flex-shrink: 0;
    animation: worker-dot-pulse 1.5s ease-in-out infinite;
  }

  @keyframes worker-dot-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.4; transform: scale(0.85); }
  }

  .worker-progress-card__activity-text {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .worker-progress-card__stat {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    padding: 1px 5px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--foreground-muted) 10%, transparent);
  }

  .worker-progress-card__instruction {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .worker-progress-card__instruction-label {
    font-size: 11px;
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .worker-progress-card__instruction-text {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: 1.5;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }

  .worker-progress-card__facts {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .worker-progress-card__fact {
    display: inline-flex;
    align-items: center;
    min-width: 0;
    gap: 4px;
  }

  .worker-progress-card__fact-label {
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .worker-progress-card__fact-value {
    min-width: 0;
    max-width: 42ch;
    color: var(--foreground);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .worker-progress-card__queue,
  .worker-progress-card__summary {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
  }

  .worker-progress-card__section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  .worker-progress-card__queue-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--border) 82%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface) 78%, transparent);
  }

  .worker-progress-card__queue-row {
    min-width: 0;
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    padding: 6px 8px;
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--assistant-message-bg) 92%, var(--foreground) 8%);
  }

  .worker-progress-card__queue-row.is-current {
    color: var(--foreground);
    background: color-mix(in srgb, var(--primary) 9%, var(--assistant-message-bg));
  }

  .worker-progress-card__queue-icon {
    width: 18px;
    height: 18px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, currentColor 14%, transparent);
  }

  .worker-progress-card__queue-icon--pending {
    color: var(--foreground-muted);
  }

  .worker-progress-card__queue-icon--running {
    color: var(--primary);
  }

  .worker-progress-card__queue-icon--paused {
    color: var(--warning);
  }

  .worker-progress-card__queue-icon--success {
    color: var(--success);
  }

  .worker-progress-card__queue-icon--danger {
    color: var(--error);
  }

  .worker-progress-card__queue-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
  }

  .worker-progress-card__queue-status {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .worker-progress-card__summary-text {
    margin: 0;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.5;
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }

  .worker-progress-card.compact .worker-progress-card__instruction-text {
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }

  @media (max-width: 720px) {
    .worker-progress-card__header {
      flex-direction: column;
      align-items: stretch;
    }

    .worker-progress-card__statusline {
      justify-content: flex-start;
    }
  }
</style>
