<script lang="ts">
  import type { ContentBlock, DispatchGroupLane, WorkerLaneStatus } from '../types/message';
  import type { IconName } from '../lib/icons';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';
  import { getEnabledAgents, getState, setCurrentBottomTab } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block, readOnly = false }: Props = $props();

  type StatusTone = 'pending' | 'running' | 'paused' | 'success' | 'danger';

  interface StatusConfig {
    key: string;
    tone: StatusTone;
    icon: IconName;
  }

  interface StageViewModel {
    laneId: string;
    title: string;
    status: WorkerLaneStatus;
    isCurrent: boolean;
  }

  interface WorkerGroupViewModel {
    key: string;
    tabId: string;
    displayName: string;
    color: string;
    muted: string;
    icon: IconName;
    status: WorkerLaneStatus;
    stages: StageViewModel[];
    totalCount: number;
    completedCount: number;
    currentTitle: string;
    summary: string;
    toolUseCount: number;
    fileChangeCount: number;
    isClickable: boolean;
  }

  const appState = getState();
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);

  const lanes = $derived.by(() => (
    Array.isArray(block.lanes)
      ? block.lanes.filter((lane): lane is DispatchGroupLane => Boolean(
        lane && typeof lane === 'object' && typeof lane.laneId === 'string',
      ))
      : []
  ));

  function normalizeText(value: unknown): string {
    return typeof value === 'string' ? value.trim() : '';
  }

  function normalizeStatus(status: unknown): WorkerLaneStatus {
    switch (status) {
      case 'running':
      case 'blocked':
      case 'awaiting_approval':
      case 'review_required':
      case 'completed':
      case 'failed':
      case 'cancelled':
      case 'pending':
        return status;
      default:
        return 'pending';
    }
  }

  function mergeLaneStatus(current: WorkerLaneStatus, next: WorkerLaneStatus): WorkerLaneStatus {
    if (next === 'failed' || current === 'failed') return 'failed';
    if (next === 'blocked' || current === 'blocked') return 'blocked';
    if (next === 'cancelled' || current === 'cancelled') return 'cancelled';
    if (next === 'awaiting_approval' || current === 'awaiting_approval') return 'awaiting_approval';
    if (next === 'review_required' || current === 'review_required') return 'review_required';
    if (next === 'running' || current === 'running') return 'running';
    if (next === 'pending' || current === 'pending') return 'pending';
    return 'completed';
  }

  function resolveStatusConfig(status: WorkerLaneStatus): StatusConfig {
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
      case 'pending':
      default:
        return { key: 'subTaskSummaryCard.status.pending', tone: 'pending', icon: 'hourglass' };
    }
  }

  function resolveWorkerMeta(workerId: string) {
    const roleSource = resolveWorkerRoleSource(workerId, enabledAgents, registrySnapshot);
    const tabId = normalizeText(roleSource?.templateId) || workerId;
    const displayName = workerId === 'orchestrator'
      ? i18n.t('workerBadge.role.orchestrator')
      : resolveWorkerDisplayName(tabId, enabledAgents, registrySnapshot, (key) => i18n.t(key));
    const visualInfo = getAgentVisualInfo(tabId, roleSource?.colorToken);
    return { tabId, displayName, visualInfo };
  }

  function resolveLaneWorkerId(lane: DispatchGroupLane): string {
    return normalizeText(lane.jumpTarget?.workerTabId)
      || normalizeText(lane.worker)
      || 'orchestrator';
  }

  function resolveLaneTitle(lane: DispatchGroupLane): string {
    return normalizeText(lane.title)
      || normalizeText(lane.description)
      || i18n.t('dispatchGroupCard.stageFallback');
  }

  function resolveLaneStages(lane: DispatchGroupLane): StageViewModel[] {
    const laneStatus = normalizeStatus(lane.status);
    if (Array.isArray(lane.tasks) && lane.tasks.length > 0) {
      return lane.tasks
        .map((task, index) => ({
          laneId: `${lane.laneId}:${task.taskId || index}`,
          title: normalizeText(task.title) || resolveLaneTitle(lane),
          status: normalizeStatus(task.status || laneStatus),
          isCurrent: Boolean(task.isCurrent),
        }))
        .filter((task) => task.title.length > 0);
    }
    return [{
      laneId: lane.laneId,
      title: resolveLaneTitle(lane),
      status: laneStatus,
      isCurrent: laneStatus === 'running' || laneStatus === 'pending',
    }];
  }

  function compactSummary(value: string): string {
    const firstLine = value
      .split(/\n\s*\n/)
      .map((part) => part.trim())
      .find(Boolean) || '';
    return firstLine.length > 140 ? `${firstLine.slice(0, 137)}...` : firstLine;
  }

  const workerGroups = $derived.by<WorkerGroupViewModel[]>(() => {
    const grouped = new Map<string, WorkerGroupViewModel>();
    for (const lane of lanes) {
      const workerId = resolveLaneWorkerId(lane);
      const { tabId, displayName, visualInfo } = resolveWorkerMeta(workerId);
      const key = tabId || workerId;
      const laneStatus = normalizeStatus(lane.status);
      const stages = resolveLaneStages(lane);
      const existing = grouped.get(key);
      const group: WorkerGroupViewModel = existing || {
        key,
        tabId: key,
        displayName,
        color: visualInfo.color,
        muted: visualInfo.muted,
        icon: visualInfo.icon,
        status: laneStatus,
        stages: [],
        totalCount: 0,
        completedCount: 0,
        currentTitle: '',
        summary: '',
        toolUseCount: 0,
        fileChangeCount: 0,
        isClickable: key !== 'orchestrator',
      };

      group.status = existing ? mergeLaneStatus(group.status, laneStatus) : laneStatus;
      group.stages.push(...stages);
      group.totalCount = group.stages.length;
      group.completedCount = group.stages.filter((stage) => stage.status === 'completed').length;
      const currentStage = group.stages.find((stage) => stage.isCurrent)
        || group.stages.find((stage) => stage.status === 'running' || stage.status === 'pending')
        || group.stages[group.stages.length - 1];
      group.currentTitle = normalizeText(currentStage?.title);
      const laneSummary = compactSummary(normalizeText(lane.summary));
      if (laneSummary) {
        group.summary = laneSummary;
      }
      if (typeof lane.toolUseCount === 'number' && Number.isFinite(lane.toolUseCount) && lane.toolUseCount > 0) {
        group.toolUseCount += Math.floor(lane.toolUseCount);
      }
      if (typeof lane.fileChangeCount === 'number' && Number.isFinite(lane.fileChangeCount) && lane.fileChangeCount > 0) {
        group.fileChangeCount += Math.floor(lane.fileChangeCount);
      }
      grouped.set(key, group);
    }
    return Array.from(grouped.values());
  });

  const totalStageCount = $derived.by(() => workerGroups.reduce((total, group) => total + group.totalCount, 0));
  const completedStageCount = $derived.by(() => workerGroups.reduce((total, group) => total + group.completedCount, 0));
  const groupStatus = $derived.by(() => workerGroups.reduce<WorkerLaneStatus>(
    (status, group) => mergeLaneStatus(status, group.status),
    workerGroups.length > 0 ? 'completed' : 'pending',
  ));
  const groupStatusConfig = $derived.by(() => resolveStatusConfig(groupStatus));
  const groupSummary = $derived.by(() => i18n.t('dispatchGroupCard.summary', {
    workerCount: workerGroups.length,
    laneCount: totalStageCount,
  }));

  function openWorkerTab(tabId: string) {
    const normalizedTabId = normalizeText(tabId);
    if (readOnly || !normalizedTabId || normalizedTabId === 'orchestrator') {
      return;
    }
    setCurrentBottomTab(normalizedTabId);
  }
</script>

{#if lanes.length > 0}
  <section class="dispatch-group-card" data-dispatch-wave-id={block.dispatchWaveId}>
    <div class="dispatch-group-card__accent" aria-hidden="true"></div>
    <div class="dispatch-group-card__body">
      <div class="dispatch-group-card__header">
        <div class="dispatch-group-card__title-wrap">
          <span class="dispatch-group-card__icon">
            <Icon name="bot" size={14} />
          </span>
          <div class="dispatch-group-card__title-text">
            <span class="dispatch-group-card__title">{i18n.t('dispatchGroupCard.title')}</span>
            <span class="dispatch-group-card__subtitle">{groupSummary}</span>
          </div>
        </div>
        <div class="dispatch-group-card__statusline">
          <span class="dispatch-group-card__progress">{i18n.t('dispatchGroupCard.progress', { completed: completedStageCount, total: totalStageCount })}</span>
          <span class={`dispatch-group-card__status dispatch-group-card__status--${groupStatusConfig.tone}`}>
            <Icon name={groupStatusConfig.icon} size={12} />
            <span>{i18n.t(groupStatusConfig.key)}</span>
          </span>
        </div>
      </div>

      <div class="dispatch-group-card__workers">
        {#each workerGroups as group (group.key)}
          {@const statusConfig = resolveStatusConfig(group.status)}
          <button
            type="button"
            class="dispatch-group-card__worker-row"
            class:is-clickable={group.isClickable && !readOnly}
            style={`--worker-color:${group.color};--worker-muted:${group.muted};`}
            disabled={readOnly || !group.isClickable}
            onclick={() => openWorkerTab(group.tabId)}
            aria-label={group.isClickable ? i18n.t('subTaskSummaryCard.clickToView', { workerLabel: group.displayName }) : undefined}
          >
            <span class="dispatch-group-card__worker-icon">
              <Icon name={group.icon} size={13} />
            </span>
            <span class="dispatch-group-card__worker-main">
              <span class="dispatch-group-card__worker-topline">
                <span class="dispatch-group-card__worker-name">{group.displayName}</span>
                <span class={`dispatch-group-card__mini-status dispatch-group-card__mini-status--${statusConfig.tone}`}>
                  <Icon name={statusConfig.icon} size={11} />
                  <span>{i18n.t(statusConfig.key)}</span>
                </span>
              </span>
              {#if group.currentTitle}
                <span class="dispatch-group-card__current">{group.currentTitle}</span>
              {/if}
              {#if group.summary}
                <span class="dispatch-group-card__summary">{group.summary}</span>
              {/if}
              <span class="dispatch-group-card__stage-list" aria-hidden="true">
                {#each group.stages.slice(0, 5) as stage (stage.laneId)}
                  {@const stageStatusConfig = resolveStatusConfig(stage.status)}
                  <span class={`dispatch-group-card__stage dispatch-group-card__stage--${stageStatusConfig.tone}`}>
                    <Icon name={stageStatusConfig.icon} size={10} />
                    <span>{stage.title}</span>
                  </span>
                {/each}
                {#if group.stages.length > 5}
                  <span class="dispatch-group-card__stage dispatch-group-card__stage--pending">
                    {i18n.t('dispatchGroupCard.moreStages', { count: group.stages.length - 5 })}
                  </span>
                {/if}
              </span>
            </span>
            <span class="dispatch-group-card__worker-metrics">
              <span>{i18n.t('subTaskSummaryCard.laneProgress', { current: group.completedCount || Math.min(1, group.totalCount), total: group.totalCount })}</span>
              {#if group.toolUseCount > 0}
                <span>{i18n.t('subTaskSummaryCard.toolCallCount', { count: group.toolUseCount })}</span>
              {/if}
              {#if group.fileChangeCount > 0}
                <span>{i18n.t('subTaskSummaryCard.fileChangeCount', { count: group.fileChangeCount })}</span>
              {/if}
            </span>
          </button>
        {/each}
      </div>
    </div>
  </section>
{/if}

<style>
  .dispatch-group-card {
    width: 100%;
    display: grid;
    grid-template-columns: 3px minmax(0, 1fr);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--assistant-message-bg);
    overflow: hidden;
  }

  .dispatch-group-card__accent {
    min-height: 100%;
    background: var(--primary);
  }

  .dispatch-group-card__body {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
  }

  .dispatch-group-card__header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .dispatch-group-card__title-wrap {
    min-width: 0;
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
  }

  .dispatch-group-card__icon,
  .dispatch-group-card__worker-icon {
    width: 24px;
    height: 24px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .dispatch-group-card__icon {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  .dispatch-group-card__title-text,
  .dispatch-group-card__worker-main {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .dispatch-group-card__title {
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    line-height: 1.35;
  }

  .dispatch-group-card__subtitle,
  .dispatch-group-card__progress,
  .dispatch-group-card__current,
  .dispatch-group-card__summary,
  .dispatch-group-card__worker-metrics {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.35;
  }

  .dispatch-group-card__statusline {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .dispatch-group-card__status,
  .dispatch-group-card__mini-status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border-radius: 999px;
    white-space: nowrap;
    font-weight: var(--font-medium);
  }

  .dispatch-group-card__status {
    padding: 2px 8px;
    font-size: var(--text-xs);
  }

  .dispatch-group-card__mini-status {
    padding: 1px 6px;
    font-size: 11px;
  }

  .dispatch-group-card__status--pending,
  .dispatch-group-card__mini-status--pending,
  .dispatch-group-card__stage--pending {
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
  }

  .dispatch-group-card__status--running,
  .dispatch-group-card__mini-status--running,
  .dispatch-group-card__stage--running {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  .dispatch-group-card__status--paused,
  .dispatch-group-card__mini-status--paused,
  .dispatch-group-card__stage--paused {
    color: var(--warning);
    background: color-mix(in srgb, var(--warning) 14%, transparent);
  }

  .dispatch-group-card__status--success,
  .dispatch-group-card__mini-status--success,
  .dispatch-group-card__stage--success {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }

  .dispatch-group-card__status--danger,
  .dispatch-group-card__mini-status--danger,
  .dispatch-group-card__stage--danger {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 14%, transparent);
  }

  .dispatch-group-card__workers {
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--border) 82%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface) 78%, transparent);
  }

  .dispatch-group-card__worker-row {
    min-width: 0;
    width: 100%;
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 0;
    border-radius: 0;
    background: color-mix(in srgb, var(--assistant-message-bg) 92%, var(--foreground) 8%);
    color: inherit;
    text-align: left;
  }

  .dispatch-group-card__worker-row:disabled {
    cursor: default;
    opacity: 1;
  }

  .dispatch-group-card__worker-row.is-clickable:not(:disabled):hover {
    background: color-mix(in srgb, var(--assistant-message-bg) 86%, var(--worker-color) 14%);
  }

  .dispatch-group-card__worker-icon {
    color: var(--worker-color);
    background: var(--worker-muted);
  }

  .dispatch-group-card__worker-topline {
    min-width: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .dispatch-group-card__worker-name {
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    line-height: 1.35;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dispatch-group-card__current,
  .dispatch-group-card__summary {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dispatch-group-card__stage-list {
    min-width: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-1);
    padding-top: 3px;
  }

  .dispatch-group-card__stage {
    max-width: 22ch;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 6px;
    border-radius: 999px;
    font-size: 11px;
    line-height: 1.25;
  }

  .dispatch-group-card__stage span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dispatch-group-card__worker-metrics {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 3px;
    white-space: nowrap;
  }

  @media (max-width: 640px) {
    .dispatch-group-card__header {
      flex-direction: column;
    }

    .dispatch-group-card__statusline {
      justify-content: flex-start;
    }

    .dispatch-group-card__worker-row {
      grid-template-columns: auto minmax(0, 1fr);
    }

    .dispatch-group-card__worker-metrics {
      grid-column: 2;
      align-items: flex-start;
      flex-direction: row;
      flex-wrap: wrap;
      white-space: normal;
    }
  }
</style>
