<script lang="ts">
  import { setCurrentBottomTab } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { formatDuration } from '../lib/utils';
  import type { IconName } from '../lib/icons';
  import type { WaitForWorkersResult } from '../types/message';
  import type { WorkerRuntimeStatus } from '../lib/worker-panel-state';
  import { i18n } from '../stores/i18n.svelte';

  // Worker 状态类型（与 MessageHub subTaskCard 状态值对齐）
  type WorkerStatus = 'pending' | 'running' | 'completed' | 'failed' | 'stopped' | 'skipped';

  interface SummaryCard {
    title?: string;
    status?: WorkerStatus;
    description?: string;
    executor?: string;
    agent?: string;
    worker?: string;
    duration?: number | string;
    startedAt?: number;
    changes?: string[];
    verification?: string[];
    error?: string;
    failureCode?: string;
    recoverable?: boolean;
    toolCount?: number;
    // 新增：Session 相关（提案 4.1）
    sessionId?: string;
    isResumed?: boolean;
    // 新增：Evidence 相关（提案 4.2）
    evidence?: {
      commandsRun?: number;
      testsPassed?: boolean;
      typeCheckPassed?: boolean;
      filesChanged?: number;
    };
    // 新增：Wave 相关（提案 4.6）
    waveIndex?: number;
  }

  // 状态徽章配置：颜色变量、图标、标签、是否旋转
  interface StatusBadgeConfig {
    colorVar: string;
    icon: IconName;
    label: string;
    spinning?: boolean;
  }

  const statusBadgeMap: Record<WorkerStatus, Omit<StatusBadgeConfig, 'label'> & { labelKey: string }> = {
    pending: { colorVar: '--warning', icon: 'hourglass', labelKey: 'subTaskSummaryCard.status.pending' },
    running: { colorVar: '--info', icon: 'loader', labelKey: 'subTaskSummaryCard.status.running', spinning: true },
    completed: { colorVar: '--success', icon: 'check', labelKey: 'subTaskSummaryCard.status.completed' },
    failed: { colorVar: '--error', icon: 'x', labelKey: 'subTaskSummaryCard.status.failed' },
    stopped: { colorVar: '--warning', icon: 'stop', labelKey: 'subTaskSummaryCard.status.stopped' },
    skipped: { colorVar: '--foreground-muted', icon: 'skip-forward', labelKey: 'subTaskSummaryCard.status.skipped' },
  };

  interface Props {
    card: SummaryCard;
    readOnly?: boolean;
    messageTimestamp?: number;
    statusOverride?: WorkerStatus;
    startedAtOverride?: number;
    runtimeStatus?: WorkerRuntimeStatus;
    waitResult?: WaitForWorkersResult | null;
  }

  let { card, readOnly = false, messageTimestamp, statusOverride, startedAtOverride, runtimeStatus, waitResult }: Props = $props();

  // 展开/收起状态
  let isExpanded = $state(false);

  function mapRuntimeStatusToCard(status?: WorkerRuntimeStatus): WorkerStatus | undefined {
    if (!status) return undefined;
    switch (status) {
      case 'pending':
        return 'pending';
      case 'running':
        return 'running';
      case 'blocked':
        return 'pending';
      case 'failed':
        return 'failed';
      case 'completed':
        return 'completed';
      default:
        return undefined;
    }
  }

  // 获取当前状态的徽章配置（任务状态优先，其次 runtime）
  const runtimeStatusOverride = $derived(mapRuntimeStatusToCard(runtimeStatus));
  const currentStatus = $derived((statusOverride || card.status || runtimeStatusOverride || 'pending') as WorkerStatus);
  const statusConfig = $derived(statusBadgeMap[currentStatus] || statusBadgeMap.completed);

  // 优化 executor 显示：支持更多来源字段，并统一使用中文
  const rawExecutor = $derived(card.executor || card.agent || card.worker || '');
  const executor = $derived(rawExecutor || i18n.t('subTaskSummaryCard.defaultExecutor'));

  // Worker 类型和颜色映射
  type WorkerType = 'claude' | 'codex' | 'gemini' | 'orchestrator' | 'default';

  const workerColorMap: Record<WorkerType, { colorVar: string; icon: string; label: string }> = {
    claude: { colorVar: '--color-claude', icon: '🧠', label: 'Claude' },
    codex: { colorVar: '--color-codex', icon: '⚡', label: 'Codex' },
    gemini: { colorVar: '--color-gemini', icon: '✨', label: 'Gemini' },
    orchestrator: { colorVar: '--color-orchestrator', icon: '🎯', label: 'Orchestrator' },
    default: { colorVar: '--foreground-muted', icon: '🤖', label: 'Worker' },
  };

  // 解析 worker 类型
  function getWorkerType(name: string): WorkerType {
    const lower = name.toLowerCase();
    if (lower.includes('claude')) return 'claude';
    if (lower.includes('codex')) return 'codex';
    if (lower.includes('gemini')) return 'gemini';
    if (lower.includes('orchestrator') || lower === 'unknown') return 'orchestrator';
    return 'default';
  }

  const workerType = $derived(getWorkerType(executor));
  const workerConfig = $derived(workerColorMap[workerType]);

  let runningStartAt = $state<number | null>(null);
  let runningElapsedMs = $state(0);
  let runningTimer: ReturnType<typeof setInterval> | null = null;

  $effect(() => {
    const status = currentStatus;
    if (status === 'running') {
      if (runningStartAt === null) {
        const startedAt = typeof startedAtOverride === 'number'
          ? startedAtOverride
          : (typeof card.startedAt === 'number' ? card.startedAt : undefined);
        const fallback = typeof messageTimestamp === 'number' ? messageTimestamp : Date.now();
        runningStartAt = startedAt ?? fallback;
      }
    } else {
      runningStartAt = null;
    }
  });

  $effect(() => {
    if (currentStatus === 'running' && runningStartAt) {
      runningElapsedMs = Math.max(0, Date.now() - runningStartAt);
      runningTimer = setInterval(() => {
        runningElapsedMs = Math.max(0, Date.now() - runningStartAt);
      }, 1000);
    } else {
      runningElapsedMs = 0;
    }
    return () => {
      if (runningTimer) {
        clearInterval(runningTimer);
        runningTimer = null;
      }
    };
  });

  // 点击跳转到对应的 worker tab
  function handleCardClick(e: MouseEvent) {
    // 如果点击的是展开按钮，不跳转
    if ((e.target as HTMLElement).closest('.expand-btn')) {
      return;
    }
    // 只有 Worker 类型可以跳转，编排者和默认类型不跳转
    if (workerType !== 'default' && workerType !== 'orchestrator') {
      setCurrentBottomTab(workerType);
    }
  }

  // 切换展开状态
  function toggleExpand(e: MouseEvent) {
    e.stopPropagation();
    isExpanded = !isExpanded;
  }

  // 是否可点击跳转
  const isClickable = $derived(workerType !== 'default' && workerType !== 'orchestrator');

  // 是否有详情可展开
  const hasDetails = $derived(
    (card.changes && card.changes.length > 0) ||
    (card.verification && card.verification.length > 0) ||
    card.evidence !== undefined
  );

  // 是否有 Evidence 信息
  const hasEvidence = $derived(card.evidence !== undefined);

  // wait_for_workers 结果展示
  const waitData = $derived(waitResult || null);
  const waitIsComplete = $derived(Boolean(waitData && waitData.wait_status === 'completed' && !waitData.timed_out));
  const completedDuration = $derived.by(() => {
    if (!waitData) return '';
    const startedAt = typeof startedAtOverride === 'number'
      ? startedAtOverride
      : (typeof card.startedAt === 'number' ? card.startedAt : messageTimestamp);
    const completedAt = typeof waitData.updatedAt === 'number' ? waitData.updatedAt : 0;
    if (startedAt && completedAt && completedAt >= startedAt) {
      return formatDuration(completedAt - startedAt);
    }
    return '';
  });

  const displayDuration = $derived.by(() => {
    if (typeof card.duration === 'number') {
      return formatDuration(card.duration);
    }
    if (typeof card.duration === 'string' && card.duration.trim().length > 0) {
      return card.duration;
    }
    if (currentStatus === 'running' && runningStartAt) {
      return formatDuration(runningElapsedMs);
    }
    if (completedDuration) {
      return completedDuration;
    }
    return '';
  });

</script>

<button
  class="worker-progress-card"
  class:pending={currentStatus === 'pending'}
  class:running={currentStatus === 'running'}
  class:completed={currentStatus === 'completed'}
  class:failed={currentStatus === 'failed'}
  class:stopped={currentStatus === 'stopped'}
  class:skipped={currentStatus === 'skipped'}
  class:clickable={isClickable}
  class:expanded={isExpanded}
  style="--worker-color: var({workerConfig.colorVar}); --status-color: var({statusConfig.colorVar})"
  onclick={handleCardClick}
  title={isClickable ? i18n.t('subTaskSummaryCard.clickToView', { workerLabel: workerConfig.label }) : ''}
>
  <!-- 卡片头部：worker 图标 + 标题 + 状态 -->
  <div class="card-header">
    <div class="worker-info">
      <span class="worker-icon">{workerConfig.icon}</span>
      <span class="worker-name">{executor}</span>
      {#if typeof card.waveIndex === 'number'}
        <span class="wave-badge" title={i18n.t('subTaskSummaryCard.waveTitle', { index: card.waveIndex + 1 })}>W{card.waveIndex + 1}</span>
      {/if}
      {#if card.isResumed}
        <span class="resumed-badge" title={i18n.t('subTaskSummaryCard.sessionResumed')}>{i18n.t('subTaskSummaryCard.resumedBadge')}</span>
      {/if}
    </div>
    <div class="card-meta">
      {#if displayDuration}
        <span class="duration">{displayDuration}</span>
      {/if}
      <!-- 状态徽章：图标 + 文字 -->
      <span
        class="status-badge"
        class:pending={currentStatus === 'pending'}
        class:running={currentStatus === 'running'}
        class:completed={currentStatus === 'completed'}
        class:failed={currentStatus === 'failed'}
        class:stopped={currentStatus === 'stopped'}
        class:skipped={currentStatus === 'skipped'}
      >
        <span class="status-icon" class:spinning={statusConfig.spinning}>
          <Icon name={statusConfig.icon} size={12} />
        </span>
        <span class="status-text">{i18n.t(statusConfig.labelKey)}</span>
      </span>
      {#if hasDetails && !readOnly}
        <span
          class="expand-btn"
          role="button"
          tabindex="0"
          onclick={toggleExpand}
          onkeydown={(e) => e.key === 'Enter' && toggleExpand(e as unknown as MouseEvent)}
          title={isExpanded ? i18n.t('subTaskSummaryCard.collapseDetails') : i18n.t('subTaskSummaryCard.expandDetails')}
        >
          <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} />
        </span>
      {/if}
      {#if isClickable}
        <span class="jump-hint">
          <Icon name="chevron-right" size={14} />
        </span>
      {/if}
    </div>
  </div>

  <!-- 任务描述 -->
  {#if card.title || card.description}
    <div class="card-body" title={card.title || card.description}>
      {(card.title || card.description || '').replace(/^#{1,6}\s+/gm, '').trim()}
    </div>
  {/if}

  <!-- 统计信息行 -->
  <div class="card-stats">
    {#if typeof card.toolCount === 'number'}
      <span class="stat-item">
        <Icon name="tool" size={12} />
        {i18n.t('subTaskSummaryCard.toolCallCount', { count: card.toolCount })}
      </span>
    {/if}
    {#if card.changes && card.changes.length > 0}
      <span class="stat-item">
        <Icon name="file" size={12} />
        {i18n.t('subTaskSummaryCard.fileChangeCount', { count: card.changes.length })}
      </span>
    {/if}
  </div>

  <!-- 统一的任务报告渲染区域（取代 WaitResultCard） -->
  {#if waitData?.results && waitData.results.length > 0}
    <div class="wait-result" class:waiting={!waitIsComplete} class:timeout={waitData.timed_out}>
      <div class="wait-result-header">
        <span class="wait-title">
          {i18n.t('waitResultCard.reportTitle')}
        </span>
      </div>
      <div class="wait-result-list">
        {#each waitData.results as result, i (result.task_id || i)}
          <div class="wait-result-item">
            {#if result.summary}
              <span class="result-summary">{result.summary}</span>
            {/if}
            <div class="result-meta">
              {#if result.modified_files && result.modified_files.length > 0}
                <span class="meta-tag"><Icon name="file" size={11} />{i18n.t('waitResultCard.fileChangeCount', { count: result.modified_files.length })}</span>
              {/if}
              {#if result.errors && result.errors.length > 0}
                <span class="meta-tag error"><Icon name="alert-circle" size={11} />{i18n.t('waitResultCard.errorCount', { count: result.errors.length })}</span>
              {/if}
            </div>
          </div>
        {/each}
      </div>
      {#if waitData.pending_task_ids && waitData.pending_task_ids.length > 0}
        <div class="wait-pending">
          <Icon name="hourglass" size={12} />
          <span>{i18n.t('waitResultCard.pendingTasks', { count: waitData.pending_task_ids.length })}</span>
        </div>
      {/if}
    </div>
  {:else if runtimeStatus === 'running'}
    <div class="wait-result waiting-only">
      <span class="wait-hint">{i18n.t('waitResultCard.waitingHint')}</span>
    </div>
  {/if}

  <!-- 展开的详情面板 -->
  {#if isExpanded && hasDetails}
    <div class="card-details">
      {#if card.changes && card.changes.length > 0}
        <div class="detail-section">
          <div class="detail-title">
            <Icon name="file" size={12} />
            {i18n.t('subTaskSummaryCard.detail.fileChanges')}
          </div>
          <ul class="file-list">
            {#each card.changes as file, i (file || i)}
              <li class="file-item">{file}</li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if card.verification && card.verification.length > 0}
        <div class="detail-section">
          <div class="detail-title">
            <Icon name="check-circle" size={12} />
            {i18n.t('subTaskSummaryCard.detail.verificationResults')}
          </div>
          <ul class="verification-list">
            {#each card.verification as item, i (item || i)}
              <li class="verification-item">{item}</li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if hasEvidence && card.evidence}
        <div class="detail-section">
          <div class="detail-title">
            <Icon name="shield" size={12} />
            {i18n.t('subTaskSummaryCard.detail.evidence')}
          </div>
          <div class="evidence-grid">
            {#if typeof card.evidence.commandsRun === 'number'}
              <div class="evidence-item">
                <span class="evidence-label">{i18n.t('subTaskSummaryCard.evidence.commandsRun')}</span>
                <span class="evidence-value">{i18n.t('subTaskSummaryCard.evidence.commandsRunCount', { count: card.evidence.commandsRun })}</span>
              </div>
            {/if}
            {#if typeof card.evidence.testsPassed === 'boolean'}
              <div class="evidence-item">
                <span class="evidence-label">{i18n.t('subTaskSummaryCard.evidence.tests')}</span>
                <span class="evidence-value" class:success={card.evidence.testsPassed} class:error={!card.evidence.testsPassed}>
                  {card.evidence.testsPassed ? i18n.t('subTaskSummaryCard.evidence.testsPassed') : i18n.t('subTaskSummaryCard.evidence.testsFailed')}
                </span>
              </div>
            {/if}
            {#if typeof card.evidence.typeCheckPassed === 'boolean'}
              <div class="evidence-item">
                <span class="evidence-label">{i18n.t('subTaskSummaryCard.evidence.typeCheck')}</span>
                <span class="evidence-value" class:success={card.evidence.typeCheckPassed} class:error={!card.evidence.typeCheckPassed}>
                  {card.evidence.typeCheckPassed ? i18n.t('subTaskSummaryCard.evidence.typeCheckPassed') : i18n.t('subTaskSummaryCard.evidence.typeCheckFailed')}
                </span>
              </div>
            {/if}
            {#if typeof card.evidence.filesChanged === 'number'}
              <div class="evidence-item">
                <span class="evidence-label">{i18n.t('subTaskSummaryCard.evidence.filesChanged')}</span>
                <span class="evidence-value">{i18n.t('subTaskSummaryCard.evidence.filesChangedCount', { count: card.evidence.filesChanged })}</span>
              </div>
            {/if}
          </div>
        </div>
      {/if}
    </div>
  {/if}

  <!-- 错误信息（如果有） -->
  {#if card.error}
    <div class="card-error">
      <Icon name="x-circle" size={14} />
      {card.error}
      {#if card.failureCode}
        <span class="error-code">[{card.failureCode}]</span>
      {/if}
    </div>
  {/if}
</button>

<style>
  /* Worker 进度卡片 - 使用 worker 颜色作为边框和微色背景 */
  .worker-progress-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    border-radius: var(--radius-md);
    /* 使用 worker 颜色 */
    background: color-mix(in srgb, var(--worker-color) 8%, var(--surface-1));
    border: 1px solid color-mix(in srgb, var(--worker-color) 30%, var(--border));
    border-left: 3px solid var(--worker-color);
    /* 按钮重置 */
    text-align: left;
    cursor: default;
    font-family: inherit;
    font-size: inherit;
    color: inherit;
    width: 100%;
    transition: all var(--transition-fast);
  }

  .worker-progress-card.clickable {
    cursor: pointer;
  }

  .worker-progress-card.clickable:hover {
    background: color-mix(in srgb, var(--worker-color) 12%, var(--surface-1));
    border-color: color-mix(in srgb, var(--worker-color) 50%, var(--border));
  }

  /* 根据状态覆盖 worker 颜色 */
  .worker-progress-card.failed {
    --worker-color: var(--error);
  }

  .worker-progress-card.stopped {
    --worker-color: var(--warning);
  }

  .worker-progress-card.skipped {
    --worker-color: var(--foreground-muted);
  }

  .worker-progress-card.running {
    --worker-color: var(--info);
  }

  .worker-progress-card.pending {
    --worker-color: var(--warning);
  }

  /* 卡片头部 */
  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .worker-info {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .worker-icon {
    font-size: var(--text-base);
  }

  .worker-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--worker-color);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .duration {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  /* 状态徽章 - 带图标和边框 */
  .status-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    font-weight: 500;
    /* 默认完成状态 */
    background: color-mix(in srgb, var(--success) 15%, transparent);
    color: var(--success);
    border: 1px solid color-mix(in srgb, var(--success) 30%, transparent);
  }

  .status-badge.pending {
    background: color-mix(in srgb, var(--warning) 15%, transparent);
    color: var(--warning);
    border-color: color-mix(in srgb, var(--warning) 30%, transparent);
  }

  .status-badge.running {
    background: color-mix(in srgb, var(--info) 15%, transparent);
    color: var(--info);
    border-color: color-mix(in srgb, var(--info) 30%, transparent);
  }

  .status-badge.completed {
    background: color-mix(in srgb, var(--success) 15%, transparent);
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 30%, transparent);
  }

  .status-badge.failed {
    background: color-mix(in srgb, var(--error) 15%, transparent);
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 30%, transparent);
  }

  .status-badge.stopped {
    background: color-mix(in srgb, var(--warning) 15%, transparent);
    color: var(--warning);
    border-color: color-mix(in srgb, var(--warning) 30%, transparent);
  }

  .status-badge.skipped {
    background: color-mix(in srgb, var(--foreground-muted) 15%, transparent);
    color: var(--foreground-muted);
    border-color: color-mix(in srgb, var(--foreground-muted) 30%, transparent);
  }

  .status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .status-icon.spinning {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }

  .status-text {
    white-space: nowrap;
  }

  .jump-hint {
    display: flex;
    align-items: center;
    color: var(--foreground-muted);
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .worker-progress-card.clickable:hover .jump-hint {
    opacity: 1;
  }

  /* 卡片内容 */
  .card-body {
    font-size: var(--text-sm);
    color: var(--foreground);
    line-height: 1.5;
    line-clamp: 2;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* 统计信息 */
  .card-stats {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }

  .stat-item {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .wait-result {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    border: 1px dashed color-mix(in srgb, var(--worker-color) 35%, var(--border));
    background: color-mix(in srgb, var(--worker-color) 6%, var(--surface-1));
  }

  .wait-result.waiting-only {
    flex-direction: row;
    align-items: center;
    gap: var(--space-2);
  }

  .wait-result-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .wait-title {
    font-weight: 600;
    color: var(--foreground);
  }

  .wait-result-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .wait-result-item {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-2);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--surface-1) 85%, transparent);
    border: 1px solid var(--border);
  }

  .result-summary {
    font-size: var(--text-sm);
    color: var(--foreground);
  }

  .result-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .result-meta .meta-tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .result-meta .meta-tag.error {
    color: var(--error);
  }

  .wait-pending {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .wait-hint {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  /* 展开按钮 */
  .expand-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding: 0;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .expand-btn:hover {
    color: var(--foreground);
    background: var(--surface-2);
  }

  /* 详情面板 */
  .card-details {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding-top: var(--space-2);
    border-top: 1px solid var(--border);
    animation: slideDown 0.2s ease-out;
  }

  @keyframes slideDown {
    from {
      opacity: 0;
      transform: translateY(-8px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .detail-title {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }

  .file-list,
  .verification-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .file-item,
  .verification-item {
    font-size: var(--text-xs);
    color: var(--foreground);
    padding: 2px 0;
    padding-left: var(--space-4);
    position: relative;
  }

  .file-item::before {
    content: '•';
    position: absolute;
    left: var(--space-1);
    color: var(--foreground-muted);
  }

  .verification-item::before {
    content: '✓';
    position: absolute;
    left: var(--space-1);
    color: var(--success);
  }

  /* 错误信息 */
  .card-error {
    line-clamp: 3;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
    text-overflow: ellipsis;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--error) 10%, transparent);
    color: var(--error);
    font-size: var(--text-sm);
    line-height: 1.4;
  }

  .error-code {
    margin-left: var(--space-2);
    font-family: var(--font-mono);
    font-size: var(--text-2xs);
    opacity: 0.85;
  }

  /* Wave 和 Session 徽章 */
  .wave-badge {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--primary) 20%, transparent);
    color: var(--primary);
    font-weight: 500;
  }

  .resumed-badge {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--warning) 20%, transparent);
    color: var(--warning);
    font-weight: 500;
  }

  /* Evidence 网格 */
  .evidence-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: var(--space-2);
    padding: var(--space-2);
    background: var(--surface-2);
    border-radius: var(--radius-sm);
  }

  .evidence-item {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .evidence-label {
    font-size: 10px;
    color: var(--foreground-muted);
    text-transform: uppercase;
  }

  .evidence-value {
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--foreground);
  }

  .evidence-value.success {
    color: var(--success);
  }

  .evidence-value.error {
    color: var(--error);
  }
</style>
