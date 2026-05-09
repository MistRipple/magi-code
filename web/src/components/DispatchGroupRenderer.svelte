<script lang="ts">
  import type { ContentBlock, DispatchGroupLane, WorkerLaneStatus } from '../types/message';
  import type { IconName } from '../lib/icons';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';
  import { getEnabledAgents, messagesState } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    block: ContentBlock;
    isStreaming?: boolean;
    readOnly?: boolean;
  }

  let { block }: Props = $props();

  type StatusTone = 'pending' | 'running' | 'paused' | 'success' | 'danger';

  interface StatusConfig {
    key: string;
    tone: StatusTone;
    icon: IconName;
  }

  interface FallbackLaneViewModel {
    key: string;
    displayIndex: number;
    title: string;
    status: WorkerLaneStatus;
    summary: string;
    toolUseCount: number;
    fileChangeCount: number;
    workerDisplayLabel: string;
    workerColor: string;
    workerMuted: string;
    workerIcon: IconName;
  }

  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(messagesState.settingsRegistrySnapshot);

  const lanes = $derived.by(() => (
    Array.isArray(block.lanes)
      ? block.lanes.filter((lane): lane is DispatchGroupLane => Boolean(
        lane && typeof lane === 'object' && typeof lane.laneId === 'string',
      )).map((lane, index) => ({ lane, index }))
        .sort((left, right) => resolveLaneOrder(left.lane, left.index) - resolveLaneOrder(right.lane, right.index)
          || left.lane.laneId.localeCompare(right.lane.laneId))
        .map((entry) => entry.lane)
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
        return { key: 'dispatchGroupCard.status.running', tone: 'running', icon: 'play' };
      case 'awaiting_approval':
        return { key: 'dispatchGroupCard.status.awaitingApproval', tone: 'paused', icon: 'alert-circle' };
      case 'review_required':
        return { key: 'dispatchGroupCard.status.reviewRequired', tone: 'paused', icon: 'alert-circle' };
      case 'blocked':
        return { key: 'dispatchGroupCard.status.blocked', tone: 'danger', icon: 'alert-circle' };
      case 'completed':
        return { key: 'dispatchGroupCard.status.completed', tone: 'success', icon: 'check-circle' };
      case 'failed':
        return { key: 'dispatchGroupCard.status.failed', tone: 'danger', icon: 'x-circle' };
      case 'cancelled':
        return { key: 'dispatchGroupCard.status.cancelled', tone: 'danger', icon: 'x-circle' };
      case 'pending':
      default:
        return { key: 'dispatchGroupCard.status.pending', tone: 'pending', icon: 'hourglass' };
    }
  }

  function resolveLaneOrder(lane: DispatchGroupLane, fallback: number): number {
    const taskSeq = Array.isArray(lane.tasks)
      ? lane.tasks.find((task) => typeof task.seq === 'number' && Number.isFinite(task.seq))?.seq
      : undefined;
    if (typeof taskSeq === 'number' && Number.isFinite(taskSeq)) {
      return taskSeq;
    }
    if (typeof lane.laneVersion === 'number' && Number.isFinite(lane.laneVersion)) {
      return lane.laneVersion;
    }
    return fallback;
  }

  function resolvePositiveCount(value: unknown): number {
    return typeof value === 'number' && Number.isFinite(value) && value > 0
      ? Math.floor(value)
      : 0;
  }

  function compactSummary(value: string): string {
    const firstLine = value
      .split(/\n\s*\n/)
      .map((part) => part.trim())
      .find(Boolean) || '';
    return firstLine.length > 140 ? `${firstLine.slice(0, 137)}...` : firstLine;
  }

  function resolveFallbackLaneTitle(lane: DispatchGroupLane): string {
    return normalizeText(lane.title)
      || normalizeText(lane.description)
      || i18n.t('dispatchGroupCard.stageFallback');
  }

  function resolveFallbackLaneWorkerId(lane: DispatchGroupLane): string {
    return normalizeText(lane.jumpTarget?.workerTabId)
      || normalizeText(lane.worker)
      || 'orchestrator';
  }

  function resolveWorkerMeta(workerId: string) {
    const normalizedWorkerId = normalizeText(workerId) || 'orchestrator';
    const roleSource = normalizedWorkerId === 'orchestrator'
      ? null
      : resolveWorkerRoleSource(normalizedWorkerId, enabledAgents, registrySnapshot);
    const displayWorkerId = normalizeText(roleSource?.templateId) || normalizedWorkerId;
    const displayName = normalizedWorkerId === 'orchestrator'
      ? i18n.t('workerBadge.role.orchestrator')
      : resolveWorkerDisplayName(displayWorkerId, enabledAgents, registrySnapshot, (key) => i18n.t(key));
    const visualInfo = getAgentVisualInfo(displayWorkerId, roleSource?.colorToken);
    return {
      workerDisplayLabel: displayName || displayWorkerId,
      workerColor: visualInfo.color,
      workerMuted: visualInfo.muted,
      workerIcon: visualInfo.icon,
    };
  }

  const fallbackRows = $derived.by<FallbackLaneViewModel[]>(() => (
    lanes.map((lane, index) => {
      const workerInfo = resolveWorkerMeta(resolveFallbackLaneWorkerId(lane));
      return {
        key: lane.laneId,
        displayIndex: index + 1,
        title: resolveFallbackLaneTitle(lane),
        status: normalizeStatus(lane.status),
        summary: compactSummary(normalizeText(lane.summary) || normalizeText(lane.liveActivity)),
        toolUseCount: resolvePositiveCount(lane.toolUseCount),
        fileChangeCount: resolvePositiveCount(lane.fileChangeCount),
        ...workerInfo,
      };
    })
  ));

  const fallbackStatus = $derived.by(() => fallbackRows.reduce<WorkerLaneStatus>(
    (status, lane) => mergeLaneStatus(status, lane.status),
    fallbackRows.length > 0 ? 'completed' : 'pending',
  ));

  const groupStatusConfig = $derived.by(() => resolveStatusConfig(fallbackStatus));
  const fallbackSummary = $derived(i18n.t('dispatchGroupCard.dispatchSummary', {
    laneCount: fallbackRows.length,
  }));
</script>

{#if fallbackRows.length > 0}
  <section class="dispatch-group-card" data-dispatch-wave-id={block.dispatchWaveId}>
    <div class="dispatch-group-card__accent" aria-hidden="true"></div>
    <div class="dispatch-group-card__body">
      <div class="dispatch-group-card__header">
        <div class="dispatch-group-card__title-wrap">
          <span class="dispatch-group-card__icon">
            <Icon name="profile" size={14} />
          </span>
          <div class="dispatch-group-card__title-text">
            <span class="dispatch-group-card__title">{i18n.t('dispatchGroupCard.dispatchTitle')}</span>
            <span class="dispatch-group-card__subtitle">{fallbackSummary}</span>
          </div>
        </div>
        <div class="dispatch-group-card__statusline">
          <span class={`dispatch-group-card__status dispatch-group-card__status--${groupStatusConfig.tone}`}>
            <Icon name={groupStatusConfig.icon} size={12} />
            <span>{i18n.t(groupStatusConfig.key)}</span>
          </span>
        </div>
      </div>

      <div class="dispatch-group-card__stages">
        {#each fallbackRows as row (row.key)}
          {@const statusConfig = resolveStatusConfig(row.status)}
          <div
            class="dispatch-group-card__stage-row"
            style={`--worker-color:${row.workerColor};--worker-muted:${row.workerMuted};`}
          >
            <span class={`dispatch-group-card__stage-index dispatch-group-card__stage-index--${statusConfig.tone}`}>
              {row.displayIndex}
            </span>
            <span class="dispatch-group-card__stage-main">
              <span class="dispatch-group-card__stage-topline">
                <span class="dispatch-group-card__stage-title">{row.title}</span>
                <span class={`dispatch-group-card__mini-status dispatch-group-card__mini-status--${statusConfig.tone}`}>
                  <Icon name={statusConfig.icon} size={11} />
                  <span>{i18n.t(statusConfig.key)}</span>
                </span>
              </span>
              {#if row.summary}
                <span class="dispatch-group-card__summary">{row.summary}</span>
              {/if}
              <span class="dispatch-group-card__owner">
                <span class="dispatch-group-card__owner-icon">
                  <Icon name={row.workerIcon} size={11} />
                </span>
                <span>{i18n.t('dispatchGroupCard.owner', { workerLabel: row.workerDisplayLabel })}</span>
              </span>
            </span>
            {#if row.toolUseCount > 0 || row.fileChangeCount > 0}
              <span class="dispatch-group-card__stage-metrics">
                {#if row.toolUseCount > 0}
                  <span>{i18n.t('dispatchGroupCard.toolCallCount', { count: row.toolUseCount })}</span>
                {/if}
                {#if row.fileChangeCount > 0}
                  <span>{i18n.t('dispatchGroupCard.fileChangeCount', { count: row.fileChangeCount })}</span>
                {/if}
              </span>
            {/if}
          </div>
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

  .dispatch-group-card__icon {
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
  .dispatch-group-card__stage-main {
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
  .dispatch-group-card__stage-metrics {
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
  .dispatch-group-card__stage-index--pending {
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
  }

  .dispatch-group-card__status--running,
  .dispatch-group-card__mini-status--running,
  .dispatch-group-card__stage-index--running {
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  .dispatch-group-card__status--paused,
  .dispatch-group-card__mini-status--paused,
  .dispatch-group-card__stage-index--paused {
    color: var(--warning);
    background: color-mix(in srgb, var(--warning) 14%, transparent);
  }

  .dispatch-group-card__status--success,
  .dispatch-group-card__mini-status--success,
  .dispatch-group-card__stage-index--success {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }

  .dispatch-group-card__status--danger,
  .dispatch-group-card__mini-status--danger,
  .dispatch-group-card__stage-index--danger {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 14%, transparent);
  }

  .dispatch-group-card__stages {
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--border) 82%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface) 78%, transparent);
  }

  .dispatch-group-card__stage-row {
    min-width: 0;
    width: 100%;
    display: grid;
    grid-template-columns: 26px minmax(0, 1fr) auto;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 0;
    border-radius: 0;
    background: color-mix(in srgb, var(--assistant-message-bg) 92%, var(--foreground) 8%);
    color: inherit;
    text-align: left;
  }

  .dispatch-group-card__stage-index {
    width: 22px;
    height: 22px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    font-size: 11px;
    font-weight: var(--font-semibold);
    line-height: 1;
  }

  .dispatch-group-card__stage-topline {
    min-width: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .dispatch-group-card__stage-title {
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    line-height: 1.35;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dispatch-group-card__owner {
    min-width: 0;
    width: fit-content;
    max-width: 100%;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-top: 3px;
    padding: 2px 7px 2px 5px;
    border-radius: 999px;
    color: var(--worker-color);
    background: var(--worker-muted);
    font-size: 11px;
    line-height: 1.25;
  }

  .dispatch-group-card__owner-icon {
    width: 15px;
    height: 15px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .dispatch-group-card__owner span:last-child {
    min-width: 0;
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

  .dispatch-group-card__stage-metrics {
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

    .dispatch-group-card__stage-row {
      grid-template-columns: auto minmax(0, 1fr);
    }

    .dispatch-group-card__stage-metrics {
      grid-column: 2;
      align-items: flex-start;
      flex-direction: row;
      flex-wrap: wrap;
      white-space: normal;
    }
  }
</style>
