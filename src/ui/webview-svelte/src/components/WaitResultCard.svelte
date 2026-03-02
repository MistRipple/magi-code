<script lang="ts">
  import Icon from './Icon.svelte';
  import { setCurrentBottomTab } from '../stores/messages.svelte';
  import type { IconName } from '../lib/icons';

  /** 单个 Worker 完成结果 */
  interface WorkerResult {
    task_id: string;
    worker: string;
    status: 'completed' | 'failed' | 'skipped' | 'cancelled';
    summary: string;
    modified_files: string[];
    errors?: string[];
  }

  /** 审计结论 */
  interface AuditInfo {
    level: 'normal' | 'watch' | 'intervention';
    summary: { normal: number; watch: number; intervention: number };
    issues: Array<{ task_id: string; level: string; dimension: string; detail: string }>;
  }

  /** wait_for_workers 返回结构 */
  interface WaitResult {
    results: WorkerResult[];
    wait_status: 'completed' | 'timeout';
    timed_out: boolean;
    pending_task_ids: string[];
    waited_ms: number;
    audit?: AuditInfo;
  }

  interface Props {
    data: WaitResult;
  }

  let { data }: Props = $props();

  // Worker 颜色映射
  type WorkerType = 'claude' | 'codex' | 'gemini' | 'default';
  const workerMeta: Record<WorkerType, { icon: string; colorVar: string }> = {
    claude: { icon: '🧠', colorVar: '--color-claude' },
    codex: { icon: '⚡', colorVar: '--color-codex' },
    gemini: { icon: '✨', colorVar: '--color-gemini' },
    default: { icon: '🤖', colorVar: '--foreground-muted' },
  };

  function resolveWorkerType(name: string): WorkerType {
    const lower = name.toLowerCase();
    if (lower.includes('claude')) return 'claude';
    if (lower.includes('codex')) return 'codex';
    if (lower.includes('gemini')) return 'gemini';
    return 'default';
  }

  // 状态配置
  const statusMap: Record<string, { icon: IconName; label: string; colorVar: string }> = {
    completed: { icon: 'check-circle', label: '完成', colorVar: '--success' },
    failed: { icon: 'x-circle', label: '失败', colorVar: '--error' },
    skipped: { icon: 'skip-forward', label: '跳过', colorVar: '--foreground-muted' },
    cancelled: { icon: 'x', label: '取消', colorVar: '--warning' },
  };

  const isAllCompleted = $derived(data.wait_status === 'completed' && !data.timed_out);
  const totalCount = $derived(data.results.length);
  const successCount = $derived(data.results.filter(r => r.status === 'completed').length);
  const failCount = $derived(data.results.filter(r => r.status === 'failed').length);
  const waitedSeconds = $derived((data.waited_ms / 1000).toFixed(1));

  // 审计等级配色
  const auditColorMap: Record<string, { colorVar: string; icon: IconName; label: string }> = {
    normal: { colorVar: '--success', icon: 'check-circle', label: '正常' },
    watch: { colorVar: '--warning', icon: 'alert-triangle', label: '关注' },
    intervention: { colorVar: '--error', icon: 'alert-circle', label: '需介入' },
  };

  function handleWorkerClick(workerName: string) {
    const type = resolveWorkerType(workerName);
    if (type !== 'default') {
      setCurrentBottomTab(type);
    }
  }
</script>

<div class="wait-result-card">
  <!-- 顶部总状态横幅 -->
  <div class="result-header" class:timeout={!isAllCompleted}>
    <div class="header-left">
      <Icon name={isAllCompleted ? 'check-circle' : 'alert-triangle'} size={16} />
      <span class="header-title">{isAllCompleted ? '任务完成报告' : '任务等待超时'}</span>
    </div>
    <div class="header-right">
      {#if totalCount > 1}
        <span class="header-stat">{successCount}/{totalCount} 完成</span>
      {/if}
      {#if failCount > 0}
        <span class="header-stat fail">{failCount} 失败</span>
      {/if}
      <span class="header-time"><Icon name="clock" size={12} />{waitedSeconds}s</span>
    </div>
  </div>

  <!-- Worker 结果列表 -->
  <div class="result-list">
    {#each data.results as result (result.task_id)}
      {@const wt = resolveWorkerType(result.worker)}
      {@const wm = workerMeta[wt]}
      {@const sm = statusMap[result.status] || statusMap.completed}
      <button
        class="worker-result-item"
        class:clickable={wt !== 'default'}
        class:failed={result.status === 'failed'}
        style="--worker-color: var({wm.colorVar}); --status-color: var({sm.colorVar})"
        onclick={() => handleWorkerClick(result.worker)}
      >
        <div class="item-header">
          <div class="item-worker">
            <span class="worker-icon">{wm.icon}</span>
            <span class="worker-name">{result.worker}</span>
          </div>
          <span class="item-status" style="color: var({sm.colorVar})">
            <Icon name={sm.icon} size={12} />
            <span>{sm.label}</span>
          </span>
        </div>
        {#if result.summary}
          <div class="item-summary">{result.summary}</div>
        {/if}
        <div class="item-meta">
          {#if result.modified_files && result.modified_files.length > 0}
            <span class="meta-tag"><Icon name="file" size={11} />{result.modified_files.length} 文件变更</span>
          {/if}
          {#if result.errors && result.errors.length > 0}
            <span class="meta-tag error"><Icon name="alert-circle" size={11} />{result.errors.length} 错误</span>
          {/if}
          {#if wt !== 'default'}
            <span class="jump-hint"><Icon name="chevron-right" size={12} /></span>
          {/if}
        </div>
      </button>
    {/each}
  </div>

  <!-- 超时时的 pending 列表 -->
  {#if data.pending_task_ids && data.pending_task_ids.length > 0}
    <div class="pending-section">
      <Icon name="hourglass" size={12} />
      <span>仍在等待 {data.pending_task_ids.length} 个任务</span>
    </div>
  {/if}

  <!-- 审计结论 -->
  {#if data.audit}
    {@const auditConfig = auditColorMap[data.audit.level] || auditColorMap.normal}
    <div class="audit-section" style="--audit-color: var({auditConfig.colorVar})">
      <div class="audit-header">
        <Icon name={auditConfig.icon} size={13} />
        <span class="audit-label">审计结论: {auditConfig.label}</span>
      </div>
      {#if data.audit.issues && data.audit.issues.length > 0}
        <div class="audit-issues">
          {#each data.audit.issues as issue (issue.task_id + issue.dimension)}
            <div class="audit-issue">
              <span class="issue-level" class:watch={issue.level === 'watch'} class:intervention={issue.level === 'intervention'}>
                {issue.level}
              </span>
              <span class="issue-detail">{issue.detail}</span>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .wait-result-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  /* 顶部状态横幅 */
  .result-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--success) 10%, var(--surface-2));
    font-size: var(--text-sm);
    gap: var(--space-3);
  }

  .result-header.timeout {
    background: color-mix(in srgb, var(--warning) 10%, var(--surface-2));
  }

  .result-header.timeout .header-left {
    color: var(--warning);
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--success);
    font-weight: 600;
  }

  .header-right {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .header-stat { font-weight: 500; color: var(--success); }
  .header-stat.fail { color: var(--error); }

  .header-time {
    display: flex;
    align-items: center;
    gap: 3px;
    color: var(--foreground-muted);
  }

  /* Worker 结果列表 */
  .result-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .worker-result-item {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--worker-color) 6%, var(--surface-2));
    border: 1px solid color-mix(in srgb, var(--worker-color) 20%, var(--border));
    border-left: 3px solid var(--worker-color);
    text-align: left;
    cursor: default;
    font-family: inherit;
    font-size: inherit;
    color: inherit;
    width: 100%;
    transition: background var(--transition-fast);
  }

  .worker-result-item.clickable { cursor: pointer; }
  .worker-result-item.clickable:hover {
    background: color-mix(in srgb, var(--worker-color) 12%, var(--surface-2));
  }
  .worker-result-item.failed { border-left-color: var(--error); }

  .item-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .item-worker {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .worker-icon { font-size: var(--text-base); }

  .worker-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--worker-color);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .item-status {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    font-weight: 500;
  }

  .item-summary {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: 1.4;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .item-meta {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }

  .meta-tag {
    display: flex;
    align-items: center;
    gap: 3px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .meta-tag.error { color: var(--error); }

  .jump-hint {
    display: flex;
    align-items: center;
    color: var(--foreground-muted);
    opacity: 0;
    margin-left: auto;
    transition: opacity var(--transition-fast);
  }

  .worker-result-item.clickable:hover .jump-hint { opacity: 1; }

  /* 超时 pending 信息 */
  .pending-section {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--warning) 10%, var(--surface-2));
    color: var(--warning);
    font-size: var(--text-xs);
    font-weight: 500;
  }

  /* 审计结论 */
  .audit-section {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--audit-color) 8%, var(--surface-2));
    border: 1px solid color-mix(in srgb, var(--audit-color) 25%, var(--border));
  }

  .audit-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--audit-color);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .audit-issues {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: var(--space-1);
  }

  .audit-issue {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .issue-level {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--success) 15%, transparent);
    color: var(--success);
    font-weight: 500;
    text-transform: uppercase;
    flex-shrink: 0;
  }

  .issue-level.watch {
    background: color-mix(in srgb, var(--warning) 15%, transparent);
    color: var(--warning);
  }

  .issue-level.intervention {
    background: color-mix(in srgb, var(--error) 15%, transparent);
    color: var(--error);
  }

  .issue-detail {
    line-height: 1.4;
  }
</style>