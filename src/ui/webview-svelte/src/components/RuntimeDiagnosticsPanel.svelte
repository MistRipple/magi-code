<script lang="ts">
  import type {
    OrchestratorRuntimeDiagnostics,
    OrchestratorRuntimeDecisionTraceEntry,
  } from '../types/message';
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    diagnostics: OrchestratorRuntimeDiagnostics | null;
  }

  let { diagnostics }: Props = $props();

  const recentTrace = $derived.by(() => {
    const trace = diagnostics?.runtimeDecisionTrace;
    if (!Array.isArray(trace) || trace.length === 0) {
      return [] as OrchestratorRuntimeDecisionTraceEntry[];
    }
    return trace.slice(-8);
  });

  // 状态图标
  const statusIcon = $derived.by((): IconName => {
    switch (diagnostics?.finalStatus) {
      case 'completed': return 'taskComplete';
      case 'failed': return 'taskFailed';
      case 'cancelled': return 'stop';
      case 'paused': return 'taskPending';
      default: return 'loader';
    }
  });

  // 状态翻译文本
  const statusLabel = $derived.by(() => {
    switch (diagnostics?.finalStatus) {
      case 'completed': return i18n.t('runtimeDiagnostics.status.completed');
      case 'failed': return i18n.t('runtimeDiagnostics.status.failed');
      case 'cancelled': return i18n.t('runtimeDiagnostics.status.cancelled');
      case 'paused': return i18n.t('runtimeDiagnostics.status.paused');
      default: return i18n.t('runtimeDiagnostics.status.pending');
    }
  });

  // 状态对应的 CSS modifier
  const statusModifier = $derived.by(() => {
    switch (diagnostics?.finalStatus) {
      case 'completed': return 'completed';
      case 'failed': return 'failed';
      case 'cancelled': return 'cancelled';
      case 'paused': return 'paused';
      default: return 'pending';
    }
  });

  // 任务进度计算
  const taskProgress = $derived.by(() => {
    const snap = diagnostics?.runtimeSnapshot;
    if (!snap) return null;
    const total = snap.requiredTotal ?? 0;
    const failed = snap.failedRequired ?? 0;
    const running = snap.runningOrPendingRequired ?? 0;
    const completed = Math.max(0, total - failed - running);
    const percent = total > 0 ? Math.round((completed / total) * 100) : 0;
    return { completed, failed, running, total, percent };
  });

  function formatTimestamp(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleTimeString();
  }

  function formatDuration(ms: number | undefined): string {
    if (!ms || !Number.isFinite(ms)) return '--';
    if (ms < 1000) return `${ms}ms`;
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m${s % 60}s`;
  }

  function formatTokens(n: number | undefined): string {
    if (!n || !Number.isFinite(n)) return '--';
    if (n < 1000) return `${n}`;
    return `${(n / 1000).toFixed(1)}k`;
  }

  // 决策轨迹 phase → 文字标签
  function phaseLabel(phase: string): string {
    switch (phase) {
      case 'tool': return i18n.t('runtimeDiagnostics.phase.tool');
      case 'handoff': return i18n.t('runtimeDiagnostics.phase.handoff');
      case 'finalize': return i18n.t('runtimeDiagnostics.phase.finalize');
      case 'no_tool': return i18n.t('runtimeDiagnostics.phase.noTool');
      default: return phase;
    }
  }

  // 决策轨迹 phase → 样式类
  function phaseClass(phase: string): string {
    switch (phase) {
      case 'tool': return 'phase--tool';
      case 'handoff': return 'phase--handoff';
      case 'finalize': return 'phase--finalize';
      case 'no_tool': return 'phase--idle';
      default: return '';
    }
  }

  // 决策轨迹 action → 样式类
  function actionClass(action: string): string {
    switch (action) {
      case 'continue':
      case 'continue_with_prompt': return 'action--continue';
      case 'handoff': return 'action--handoff';
      case 'terminate': return 'action--terminate';
      case 'fallback': return 'action--fallback';
      default: return '';
    }
  }
</script>

{#if diagnostics}
  <details class="runtime-diagnostics runtime-diagnostics--{statusModifier}">
    <summary>
      <Icon name={statusIcon} size={13} class="summary__icon" />
      <span class="summary__title">{i18n.t('runtimeDiagnostics.title')}</span>
      <span class="summary__badge summary__badge--{statusModifier}">{statusLabel}</span>
      <span class="summary__time">{formatTimestamp(diagnostics.updatedAt)}</span>
    </summary>
    <div class="runtime-diagnostics__content">
      {#if diagnostics.runtimeSnapshot}
        {@const snap = diagnostics.runtimeSnapshot}
        <div class="metrics-grid">
          {#if taskProgress && taskProgress.total > 0}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="list" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.taskProgress')}</span>
              </div>
              <div class="metric-card__value">{taskProgress.completed}/{taskProgress.total}</div>
              <div class="progress-bar">
                <div class="progress-bar__fill" style="width: {taskProgress.percent}%"></div>
              </div>
              {#if taskProgress.failed > 0}
                <div class="metric-card__sub metric-card__sub--warn">{i18n.t('runtimeDiagnostics.failedCount', { count: taskProgress.failed })}</div>
              {/if}
            </div>
          {/if}

          {#if snap.reviewState}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="check-circle" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.review')}</span>
              </div>
              <div class="metric-card__value">
                {snap.reviewState.accepted ?? 0}/{snap.reviewState.total ?? 0}
              </div>
              <div class="metric-card__sub"
                   class:metric-card__sub--ok={(snap.reviewState.accepted ?? 0) >= (snap.reviewState.total ?? 0) && (snap.reviewState.total ?? 0) > 0}>
                {#if (snap.reviewState.accepted ?? 0) >= (snap.reviewState.total ?? 0) && (snap.reviewState.total ?? 0) > 0}
                  {i18n.t('runtimeDiagnostics.allPassed')}
                {:else}
                  {i18n.t('runtimeDiagnostics.inProgress')}
                {/if}
              </div>
            </div>
          {/if}

          {#if snap.blockerState}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name={(snap.blockerState.open ?? 0) > 0 ? 'alert-triangle' : 'check-circle'} size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.blocker')}</span>
              </div>
              <div class="metric-card__value"
                   class:metric-card__value--warn={(snap.blockerState.open ?? 0) > 0}>
                {snap.blockerState.open ?? 0}
              </div>
              {#if (snap.blockerState.externalWaitOpen ?? 0) > 0}
                <div class="metric-card__sub metric-card__sub--warn">
                  {i18n.t('runtimeDiagnostics.externalWait', { count: snap.blockerState.externalWaitOpen ?? 0 })}
                </div>
              {:else}
                <div class="metric-card__sub metric-card__sub--ok">{i18n.t('runtimeDiagnostics.noBlocker')}</div>
              {/if}
            </div>
          {/if}

          {#if snap.budgetState}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="clock" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.budget')}</span>
              </div>
              <div class="metric-card__value">{formatDuration(snap.budgetState.elapsedMs)}</div>
              <div class="metric-card__sub">
                {i18n.t('runtimeDiagnostics.tokens', { value: formatTokens(snap.budgetState.tokenUsed) })}
                {#if snap.budgetState.errorRate != null && snap.budgetState.errorRate > 0}
                  · {i18n.t('runtimeDiagnostics.errorRate', { rate: Math.round(snap.budgetState.errorRate * 100) })}
                {/if}
              </div>
            </div>
          {/if}
        </div>
      {/if}

      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.decisionTrace')}</div>
        {#if recentTrace.length > 0}
          <div class="trace-list">
            {#each recentTrace as item}
              <div class="trace-item">
                <span class="trace-item__round">R{item.round}</span>
                <span class="trace-item__phase {phaseClass(item.phase)}">{phaseLabel(item.phase)}</span>
                <span class="trace-item__arrow">→</span>
                <span class="trace-item__action {actionClass(item.action)}">{item.action}</span>
                {#if item.requiredTotal > 0}
                  <span class="trace-item__meta">({item.requiredTotal})</span>
                {/if}
                {#if item.reason || item.note}
                  <span class="trace-item__note">{item.reason || item.note}</span>
                {/if}
              </div>
            {/each}
          </div>
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noTrace')}</div>
        {/if}
      </div>
    </div>
  </details>
{/if}

<style>
  .runtime-diagnostics {
    margin: 8px 12px 0;
    border: 1px solid var(--vscode-editorWidget-border, #3c3c3c);
    border-radius: 8px;
    background: var(--vscode-editorWidget-background, #252526);
    color: var(--vscode-foreground, #ccc);
    overflow: hidden;
    border-left: 3px solid var(--vscode-editorWidget-border, #3c3c3c);
  }

  /* 卡片左边框根据终态着色 */
  .runtime-diagnostics--completed { border-left-color: #4ec995; }
  .runtime-diagnostics--failed    { border-left-color: var(--vscode-editorError-foreground, #f48771); }
  .runtime-diagnostics--cancelled { border-left-color: var(--vscode-editorWidget-border, #6c6c6c); }
  .runtime-diagnostics--paused    { border-left-color: var(--vscode-editorWarning-foreground, #cca700); }
  .runtime-diagnostics--pending   { border-left-color: var(--vscode-progressBar-background, #0e70c0); }

  .runtime-diagnostics > summary {
    cursor: pointer;
    padding: 8px 10px;
    font-size: 12px;
    user-select: none;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  :global(.summary__icon) {
    opacity: 0.9;
    flex-shrink: 0;
  }

  .summary__title {
    font-weight: 500;
  }

  .summary__badge {
    font-size: 10px;
    font-weight: 500;
    padding: 1px 6px;
    border-radius: 3px;
  }

  .summary__badge--completed {
    background: rgba(78, 201, 149, 0.18);
    color: #4ec995;
  }
  .summary__badge--failed {
    background: rgba(244, 135, 113, 0.18);
    color: var(--vscode-editorError-foreground, #f48771);
  }
  .summary__badge--cancelled {
    background: rgba(140, 140, 140, 0.18);
    color: #999;
  }
  .summary__badge--paused {
    background: rgba(204, 167, 0, 0.18);
    color: var(--vscode-editorWarning-foreground, #cca700);
  }
  .summary__badge--pending {
    background: rgba(14, 112, 192, 0.18);
    color: var(--vscode-textLink-foreground, #3794ff);
  }

  .summary__time {
    margin-left: auto;
    font-size: 11px;
    opacity: 0.5;
    font-variant-numeric: tabular-nums;
  }

  .runtime-diagnostics__content {
    border-top: 1px solid var(--vscode-editorWidget-border, #3c3c3c);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 8px;
  }

  .metric-card {
    padding: 8px;
    border-radius: 6px;
    background: var(--vscode-editor-background, #1e1e1e);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .metric-card__header {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    opacity: 0.8;
  }

  :global(.metric-card__icon) {
    opacity: 0.9;
  }

  .metric-card__title {
    font-size: 11px;
  }

  .metric-card__value {
    font-size: 16px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .metric-card__value--warn {
    color: var(--vscode-editorWarning-foreground, #cca700);
  }

  .metric-card__sub {
    font-size: 11px;
    opacity: 0.7;
  }

  .metric-card__sub--ok {
    color: #4ec995;
    opacity: 1;
  }

  .metric-card__sub--warn {
    color: var(--vscode-editorWarning-foreground, #cca700);
    opacity: 1;
  }

  .progress-bar {
    height: 4px;
    border-radius: 2px;
    background: var(--vscode-editorWidget-border, #3c3c3c);
    overflow: hidden;
  }

  .progress-bar__fill {
    height: 100%;
    border-radius: 2px;
    background: var(--vscode-progressBar-background, #0e70c0);
    transition: width 0.3s ease;
  }

  .runtime-diagnostics__block {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .runtime-diagnostics__label {
    font-size: 11px;
    opacity: 0.8;
    margin-bottom: 2px;
  }

  .trace-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .trace-item {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    line-height: 1.4;
    padding: 2px 0;
  }

  .trace-item__round {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    min-width: 24px;
    opacity: 0.7;
  }

  .trace-item__phase {
    font-size: 10px;
    font-weight: 500;
    padding: 1px 5px;
    border-radius: 3px;
    min-width: 42px;
    text-align: center;
  }

  .phase--tool {
    background: rgba(14, 112, 192, 0.12);
    color: var(--vscode-textLink-foreground, #3794ff);
  }
  .phase--handoff {
    background: rgba(204, 167, 0, 0.12);
    color: var(--vscode-editorWarning-foreground, #cca700);
  }
  .phase--finalize {
    background: rgba(78, 201, 149, 0.12);
    color: #4ec995;
  }
  .phase--idle {
    background: rgba(140, 140, 140, 0.12);
    color: #999;
  }

  .trace-item__arrow {
    opacity: 0.55;
    font-size: 11px;
  }

  .trace-item__action {
    font-weight: 500;
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 10px;
  }

  .action--continue {
    background: rgba(14, 112, 192, 0.2);
    color: var(--vscode-textLink-foreground, #3794ff);
  }

  .action--handoff {
    background: rgba(204, 167, 0, 0.15);
    color: var(--vscode-editorWarning-foreground, #cca700);
  }

  .action--terminate {
    background: rgba(51, 153, 102, 0.2);
    color: #4ec995;
  }

  .action--fallback {
    background: rgba(204, 51, 51, 0.15);
    color: var(--vscode-editorError-foreground, #f48771);
  }

  .trace-item__meta {
    opacity: 0.65;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
  }

  .trace-item__note {
    opacity: 0.65;
    font-size: 10px;
    margin-left: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 160px;
  }

  .runtime-diagnostics__empty {
    font-size: 11px;
    opacity: 0.7;
  }
</style>
