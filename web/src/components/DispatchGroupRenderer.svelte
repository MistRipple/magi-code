<script lang="ts">
  import type {
    ContentBlock,
    DispatchGroupLane,
    WorkerLaneStatus,
  } from '../types/message';
  import type { IconName } from '../lib/icons';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';
  import {
    getEnabledAgents,
    messagesState,
  } from '../stores/messages.svelte';
  import { openWorkerDetailDrawer } from '../stores/worker-detail-drawer.svelte';
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

  interface LaneRow {
    key: string;
    displayIndex: number;
    title: string;
    status: WorkerLaneStatus;
    statusConfig: StatusConfig;
    tone: StatusTone;
    /**
     * running 态优先 liveActivity，其他终/中间态优先 summary；空串表示无摘要可显示。
     */
    body: string;
    /** 失败/阻塞态提示文案，置于 body 前；空串表示无特殊提示。 */
    bodyAccent: '' | 'error' | 'warning';
    /** 右上角的 owner（角色） badge。 */
    workerDisplayLabel: string;
    workerColor: string;
    workerMuted: string;
    workerIcon: IconName;
    /** running 态进度尾注，如 "3/4 剩 1 个任务"；空串不显示。 */
    progressNote: string;
    /** completed 态指标：工具调用次数 / 文件变更次数，0 表示不展示。 */
    toolUseCount: number;
    fileChangeCount: number;
    /** 行跳转目标：打开该 worker 的详情 Drawer；null 表示不支持跳转（行不可点击）。 */
    focusTarget: { workerTabId: string } | null;
  }

  interface GroupProgress {
    total: number;
    completed: number;
    running: number;
    failed: number;
    blocked: number;
    awaiting: number;
    pending: number;
    cancelled: number;
  }

  const STATUS_SEVERITY_ORDER: WorkerLaneStatus[] = [
    'failed',
    'blocked',
    'cancelled',
    'awaiting_approval',
    'review_required',
    'running',
    'pending',
    'completed',
  ];

  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(messagesState.settingsRegistrySnapshot);

  const lanes = $derived.by<DispatchGroupLane[]>(() => (
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

  function compact(value: string, max: number): string {
    const firstParagraph = value
      .split(/\n\s*\n/)
      .map((part) => part.trim())
      .find(Boolean) || '';
    const singleLine = firstParagraph.replace(/\s+/g, ' ').trim();
    return singleLine.length > max ? `${singleLine.slice(0, max - 1)}…` : singleLine;
  }

  function resolveFallbackLaneWorkerId(lane: DispatchGroupLane): string {
    // P1 身份契约：只认 roleId（jumpTarget.workerTabId 是它的搬运）。
    // 没有 roleId 的 lane 归到 orchestrator 兜底。
    return normalizeText(lane.jumpTarget?.workerTabId) || 'orchestrator';
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

  function resolveFocusWorkerTabId(lane: DispatchGroupLane): string | null {
    // lane 有 roleId ⇔ 行可点击 → drawer 可打开。没有 roleId 的 lane 不允许跳转。
    return normalizeText(lane.jumpTarget?.workerTabId) || null;
  }

  function resolveLaneBody(lane: DispatchGroupLane, status: WorkerLaneStatus): string {
    const summary = normalizeText(lane.summary);
    const liveActivity = normalizeText(lane.liveActivity);
    const description = normalizeText(lane.description);
    if (status === 'running') {
      return compact(liveActivity || description, 180);
    }
    if (status === 'pending') {
      return '';
    }
    return compact(summary || liveActivity || description, 220);
  }

  function resolveProgressNote(lane: DispatchGroupLane, status: WorkerLaneStatus): string {
    if (status !== 'running') {
      return '';
    }
    const progress = lane.progressSummary;
    if (!progress) {
      return '';
    }
    const total = typeof progress.totalTaskCount === 'number' && Number.isFinite(progress.totalTaskCount)
      ? Math.max(0, Math.floor(progress.totalTaskCount))
      : 0;
    if (total <= 0) {
      return '';
    }
    const completed = typeof progress.completedTaskCount === 'number' && Number.isFinite(progress.completedTaskCount)
      ? Math.max(0, Math.floor(progress.completedTaskCount))
      : 0;
    const remaining = Math.max(0, total - completed);
    return i18n.t('dispatchGroupCard.row.progressNote', {
      completed,
      total,
      remaining,
    });
  }

  function resolveBodyAccent(status: WorkerLaneStatus): '' | 'error' | 'warning' {
    if (status === 'failed' || status === 'blocked') {
      return 'error';
    }
    if (status === 'awaiting_approval' || status === 'review_required') {
      return 'warning';
    }
    return '';
  }

  const rows = $derived.by<LaneRow[]>(() => (
    lanes.map((lane, index) => {
      const status = normalizeStatus(lane.status);
      const statusConfig = resolveStatusConfig(status);
      const workerInfo = resolveWorkerMeta(resolveFallbackLaneWorkerId(lane));
      const focusWorkerTabId = resolveFocusWorkerTabId(lane);
      const body = resolveLaneBody(lane, status);
      return {
        key: lane.laneId,
        displayIndex: index + 1,
        title: normalizeText(lane.title)
          || normalizeText(lane.description)
          || i18n.t('dispatchGroupCard.stageFallback'),
        status,
        statusConfig,
        tone: statusConfig.tone,
        body,
        bodyAccent: resolveBodyAccent(status),
        workerDisplayLabel: workerInfo.workerDisplayLabel,
        workerColor: workerInfo.workerColor,
        workerMuted: workerInfo.workerMuted,
        workerIcon: workerInfo.workerIcon,
        progressNote: resolveProgressNote(lane, status),
        toolUseCount: status === 'completed' ? resolvePositiveCount(lane.toolUseCount) : 0,
        fileChangeCount: status === 'completed' ? resolvePositiveCount(lane.fileChangeCount) : 0,
        focusTarget: focusWorkerTabId ? { workerTabId: focusWorkerTabId } : null,
      };
    })
  ));

  const progress = $derived.by<GroupProgress>(() => {
    const base: GroupProgress = {
      total: rows.length,
      completed: 0,
      running: 0,
      failed: 0,
      blocked: 0,
      awaiting: 0,
      pending: 0,
      cancelled: 0,
    };
    for (const row of rows) {
      switch (row.status) {
        case 'completed': base.completed += 1; break;
        case 'running': base.running += 1; break;
        case 'failed': base.failed += 1; break;
        case 'blocked': base.blocked += 1; break;
        case 'awaiting_approval':
        case 'review_required': base.awaiting += 1; break;
        case 'cancelled': base.cancelled += 1; break;
        case 'pending':
        default: base.pending += 1; break;
      }
    }
    return base;
  });

  const dominantStatus = $derived.by<WorkerLaneStatus>(() => {
    const present = new Set(rows.map((row) => row.status));
    for (const candidate of STATUS_SEVERITY_ORDER) {
      if (present.has(candidate)) {
        return candidate;
      }
    }
    return 'pending';
  });

  const dominantConfig = $derived(resolveStatusConfig(dominantStatus));

  const progressPercent = $derived.by(() => {
    if (progress.total <= 0) return 0;
    const done = progress.completed + progress.failed + progress.cancelled;
    return Math.max(0, Math.min(100, Math.round((done / progress.total) * 100)));
  });

  const progressBreakdown = $derived.by<Array<{ key: string; label: string; tone: StatusTone }>>(() => {
    const items: Array<{ key: string; label: string; tone: StatusTone }> = [];
    if (progress.running > 0) {
      items.push({
        key: 'running',
        label: i18n.t('dispatchGroupCard.progress.running', { count: progress.running }),
        tone: 'running',
      });
    }
    if (progress.failed > 0) {
      items.push({
        key: 'failed',
        label: i18n.t('dispatchGroupCard.progress.failed', { count: progress.failed }),
        tone: 'danger',
      });
    }
    if (progress.blocked > 0) {
      items.push({
        key: 'blocked',
        label: i18n.t('dispatchGroupCard.progress.blocked', { count: progress.blocked }),
        tone: 'danger',
      });
    }
    if (progress.awaiting > 0) {
      items.push({
        key: 'awaiting',
        label: i18n.t('dispatchGroupCard.progress.awaiting', { count: progress.awaiting }),
        tone: 'paused',
      });
    }
    if (progress.pending > 0) {
      items.push({
        key: 'pending',
        label: i18n.t('dispatchGroupCard.progress.pending', { count: progress.pending }),
        tone: 'pending',
      });
    }
    if (progress.completed > 0) {
      items.push({
        key: 'completed',
        label: i18n.t('dispatchGroupCard.progress.completed', { count: progress.completed }),
        tone: 'success',
      });
    }
    if (progress.cancelled > 0) {
      items.push({
        key: 'cancelled',
        label: i18n.t('dispatchGroupCard.progress.cancelled', { count: progress.cancelled }),
        tone: 'danger',
      });
    }
    return items;
  });

  const subtitle = $derived.by(() => {
    const narrative = normalizeText(block.summaryText);
    if (narrative) {
      return compact(narrative, 160);
    }
    return i18n.t('dispatchGroupCard.dispatchSummary', { laneCount: rows.length });
  });

  function focusRow(row: LaneRow) {
    if (readOnly || !row.focusTarget) {
      return;
    }
    openWorkerDetailDrawer(row.focusTarget.workerTabId);
  }

  function rowAriaLabel(row: LaneRow): string {
    return i18n.t('dispatchGroupCard.row.ariaLabel', {
      index: row.displayIndex,
      total: rows.length,
      title: row.title,
      status: i18n.t(row.statusConfig.key),
      worker: row.workerDisplayLabel,
    });
  }
</script>

{#if rows.length > 0}
  <section class="dispatch-group-card" data-dispatch-wave-id={block.dispatchWaveId}>
    <div class="dispatch-group-card__accent" aria-hidden="true"></div>
    <div class="dispatch-group-card__body">
      <header class="dispatch-group-card__header">
        <div class="dispatch-group-card__title-wrap">
          <span class="dispatch-group-card__icon" aria-hidden="true">
            <Icon name="profile" size={14} />
          </span>
          <div class="dispatch-group-card__title-text">
            <span class="dispatch-group-card__title">{i18n.t('dispatchGroupCard.dispatchTitle')}</span>
            <span class="dispatch-group-card__subtitle">{subtitle}</span>
          </div>
        </div>
        <span class={`dispatch-group-card__status dispatch-group-card__status--${dominantConfig.tone}`} aria-hidden="true">
          <Icon name={dominantConfig.icon} size={12} />
          <span>{i18n.t(dominantConfig.key)}</span>
        </span>
      </header>

      <div class="dispatch-group-card__progress" role="group" aria-label={i18n.t('dispatchGroupCard.progress.ariaLabel')}>
        <div class="dispatch-group-card__progress-bar" role="progressbar"
          aria-valuemin="0" aria-valuemax={progress.total}
          aria-valuenow={progress.completed + progress.failed + progress.cancelled}
          aria-valuetext={i18n.t('dispatchGroupCard.progress.count', {
            done: progress.completed + progress.failed + progress.cancelled,
            total: progress.total,
          })}
        >
          <div class="dispatch-group-card__progress-fill" style={`width:${progressPercent}%;`}></div>
        </div>
        <div class="dispatch-group-card__progress-meta">
          <span class="dispatch-group-card__progress-count">
            {i18n.t('dispatchGroupCard.progress.count', {
              done: progress.completed + progress.failed + progress.cancelled,
              total: progress.total,
            })}
          </span>
          {#each progressBreakdown as item (item.key)}
            <span class={`dispatch-group-card__progress-chip dispatch-group-card__progress-chip--${item.tone}`}>{item.label}</span>
          {/each}
        </div>
      </div>

      <ul class="dispatch-group-card__rows">
        {#each rows as row (row.key)}
          {@const isInteractive = !readOnly && row.focusTarget !== null}
          <li class="dispatch-group-card__row-wrap">
            {#if isInteractive}
              <button
                type="button"
                class={[
                  'dispatch-group-card__row',
                  `dispatch-group-card__row--${row.tone}`,
                  'dispatch-group-card__row--interactive',
                  row.status === 'failed' || row.status === 'blocked' ? 'dispatch-group-card__row--danger' : '',
                ].filter(Boolean).join(' ')}
                style={`--worker-color:${row.workerColor};--worker-muted:${row.workerMuted};`}
                aria-label={rowAriaLabel(row)}
                onclick={() => focusRow(row)}
              >
                {@render rowContent(row)}
              </button>
            {:else}
              <div
                class={[
                  'dispatch-group-card__row',
                  `dispatch-group-card__row--${row.tone}`,
                  row.status === 'failed' || row.status === 'blocked' ? 'dispatch-group-card__row--danger' : '',
                ].filter(Boolean).join(' ')}
                style={`--worker-color:${row.workerColor};--worker-muted:${row.workerMuted};`}
              >
                {@render rowContent(row)}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  </section>
{/if}

{#snippet rowContent(row: LaneRow)}
  <span class={`dispatch-group-card__row-index dispatch-group-card__row-index--${row.tone}`} aria-hidden="true">
    <Icon name={row.statusConfig.icon} size={12} />
  </span>
  <span class="dispatch-group-card__row-main">
    <span class="dispatch-group-card__row-topline">
      <span class="dispatch-group-card__row-title">{row.title}</span>
      <span class="dispatch-group-card__row-owner" aria-hidden="true">
        <span class="dispatch-group-card__row-owner-icon">
          <Icon name={row.workerIcon} size={11} />
        </span>
        <span>{row.workerDisplayLabel}</span>
      </span>
    </span>
    {#if row.body}
      <span class={[
        'dispatch-group-card__row-body',
        row.bodyAccent ? `dispatch-group-card__row-body--${row.bodyAccent}` : '',
      ].filter(Boolean).join(' ')}>{row.body}</span>
    {/if}
    {#if row.progressNote || row.toolUseCount > 0 || row.fileChangeCount > 0}
      <span class="dispatch-group-card__row-meta">
        {#if row.progressNote}
          <span>{row.progressNote}</span>
        {/if}
        {#if row.toolUseCount > 0}
          <span>{i18n.t('dispatchGroupCard.toolCallCount', { count: row.toolUseCount })}</span>
        {/if}
        {#if row.fileChangeCount > 0}
          <span>{i18n.t('dispatchGroupCard.fileChangeCount', { count: row.fileChangeCount })}</span>
        {/if}
      </span>
    {/if}
  </span>
  {#if row.focusTarget}
    <span class="dispatch-group-card__row-chevron" aria-hidden="true">
      <Icon name="chevron-right" size={12} />
    </span>
  {/if}
{/snippet}

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
    color: var(--primary);
    background: color-mix(in srgb, var(--primary) 14%, transparent);
  }

  .dispatch-group-card__title-text {
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

  .dispatch-group-card__subtitle {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .dispatch-group-card__status {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
  }

  .dispatch-group-card__status--pending { color: var(--foreground-muted); background: color-mix(in srgb, var(--foreground-muted) 14%, transparent); }
  .dispatch-group-card__status--running { color: var(--primary); background: color-mix(in srgb, var(--primary) 14%, transparent); }
  .dispatch-group-card__status--paused { color: var(--warning); background: color-mix(in srgb, var(--warning) 14%, transparent); }
  .dispatch-group-card__status--success { color: var(--success); background: color-mix(in srgb, var(--success) 14%, transparent); }
  .dispatch-group-card__status--danger { color: var(--error); background: color-mix(in srgb, var(--error) 14%, transparent); }

  .dispatch-group-card__progress {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .dispatch-group-card__progress-bar {
    width: 100%;
    height: 4px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
    overflow: hidden;
  }

  .dispatch-group-card__progress-fill {
    height: 100%;
    background: var(--primary);
    border-radius: inherit;
    transition: width var(--transition-base, 200ms) ease-out;
  }

  .dispatch-group-card__progress-meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    line-height: 1.3;
  }

  .dispatch-group-card__progress-count {
    color: var(--foreground);
    font-weight: var(--font-semibold);
  }

  .dispatch-group-card__progress-chip {
    display: inline-flex;
    align-items: center;
    padding: 1px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: var(--font-medium);
    white-space: nowrap;
  }

  .dispatch-group-card__progress-chip--pending { color: var(--foreground-muted); background: color-mix(in srgb, var(--foreground-muted) 14%, transparent); }
  .dispatch-group-card__progress-chip--running { color: var(--primary); background: color-mix(in srgb, var(--primary) 14%, transparent); }
  .dispatch-group-card__progress-chip--paused { color: var(--warning); background: color-mix(in srgb, var(--warning) 14%, transparent); }
  .dispatch-group-card__progress-chip--success { color: var(--success); background: color-mix(in srgb, var(--success) 14%, transparent); }
  .dispatch-group-card__progress-chip--danger { color: var(--error); background: color-mix(in srgb, var(--error) 14%, transparent); }

  .dispatch-group-card__rows {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    border-radius: var(--radius-md);
    border: 1px solid color-mix(in srgb, var(--border) 82%, transparent);
    background: color-mix(in srgb, var(--surface) 78%, transparent);
    overflow: hidden;
  }

  .dispatch-group-card__row-wrap {
    display: block;
  }

  .dispatch-group-card__row {
    min-width: 0;
    width: 100%;
    display: grid;
    grid-template-columns: 26px minmax(0, 1fr) auto;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3);
    background: color-mix(in srgb, var(--assistant-message-bg) 92%, var(--foreground) 8%);
    color: inherit;
    text-align: left;
    border: 0;
    border-radius: 0;
  }

  .dispatch-group-card__row--interactive {
    cursor: pointer;
    transition: background var(--transition-base, 180ms) ease-out, transform 120ms ease-out;
  }

  .dispatch-group-card__row--interactive:hover {
    background: color-mix(in srgb, var(--assistant-message-bg) 78%, var(--foreground) 22%);
  }

  .dispatch-group-card__row--interactive:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: -2px;
  }

  .dispatch-group-card__row--danger {
    background: color-mix(in srgb, var(--error) 10%, var(--assistant-message-bg));
  }

  .dispatch-group-card__row--danger.dispatch-group-card__row--interactive:hover {
    background: color-mix(in srgb, var(--error) 16%, var(--assistant-message-bg));
  }

  .dispatch-group-card__row-index {
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

  .dispatch-group-card__row-index--pending { color: var(--foreground-muted); background: color-mix(in srgb, var(--foreground-muted) 14%, transparent); }
  .dispatch-group-card__row-index--running { color: var(--primary); background: color-mix(in srgb, var(--primary) 14%, transparent); }
  .dispatch-group-card__row-index--paused { color: var(--warning); background: color-mix(in srgb, var(--warning) 14%, transparent); }
  .dispatch-group-card__row-index--success { color: var(--success); background: color-mix(in srgb, var(--success) 14%, transparent); }
  .dispatch-group-card__row-index--danger { color: var(--error); background: color-mix(in srgb, var(--error) 14%, transparent); }

  .dispatch-group-card__row-main {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .dispatch-group-card__row-topline {
    min-width: 0;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .dispatch-group-card__row-title {
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    line-height: 1.35;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dispatch-group-card__row-owner {
    min-width: 0;
    width: fit-content;
    max-width: 100%;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 7px 2px 5px;
    border-radius: 999px;
    color: var(--worker-color);
    background: var(--worker-muted);
    font-size: 11px;
    line-height: 1.25;
  }

  .dispatch-group-card__row-owner-icon {
    width: 15px;
    height: 15px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .dispatch-group-card__row-owner span:last-child {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dispatch-group-card__row-body {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.45;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }

  .dispatch-group-card__row-body--error { color: var(--error); }
  .dispatch-group-card__row-body--warning { color: var(--warning); }

  .dispatch-group-card__row-meta {
    color: var(--foreground-muted);
    font-size: 11px;
    line-height: 1.3;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .dispatch-group-card__row-chevron {
    flex-shrink: 0;
    color: var(--foreground-muted);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    opacity: 0;
    transition: opacity 120ms ease-out, transform 120ms ease-out;
  }

  .dispatch-group-card__row--interactive:hover .dispatch-group-card__row-chevron,
  .dispatch-group-card__row--interactive:focus-visible .dispatch-group-card__row-chevron {
    opacity: 1;
    transform: translateX(2px);
  }

  @media (max-width: 640px) {
    .dispatch-group-card__header {
      flex-direction: column;
    }
    .dispatch-group-card__row {
      grid-template-columns: auto minmax(0, 1fr);
    }
    .dispatch-group-card__row-chevron {
      grid-column: 2;
      justify-self: flex-end;
    }
  }
</style>
