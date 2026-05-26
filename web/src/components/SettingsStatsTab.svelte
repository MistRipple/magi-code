<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { getAgentColor } from '../lib/agent-colors';
  import Icon from './Icon.svelte';
  import type { ModelStatusMap } from '../types/message';

  let {
    totalInputTokens,
    totalOutputTokens,
    totalTokens,
    isRefreshing,
    refreshConnections,
    showResetConfirmDialog,
    modelStatuses,
    getWorkerStats,
    getStatusClass,
    getWorkerDisplayName,
    statusTexts,
    statsDisplayKeys,
  } = $props<{
    totalInputTokens: number;
    totalOutputTokens: number;
    totalTokens: number;
    isRefreshing: boolean;
    refreshConnections: () => void;
    showResetConfirmDialog: () => void;
    modelStatuses: ModelStatusMap;
    getWorkerStats: (worker: string) => any;
    getStatusClass: (status: string) => string;
    getWorkerDisplayName: (worker: string) => string;
    statusTexts: Record<string, () => string>;
    statsDisplayKeys: string[];
  }>();

  type Perspective = 'role' | 'engine';
  let perspective = $state<Perspective>('role');
  let selectedKey = $state<string | null>(null);

  function formatTokens(tokens: number | undefined | null): string {
    if (tokens === undefined || tokens === null) return '--';
    if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
    if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}K`;
    return String(tokens);
  }

  function formatPct(rate: number | undefined | null): string {
    if (rate === undefined || rate === null) return '--';
    return `${Math.round(rate * 100)}%`;
  }

  function workerLabel(worker: string): string {
    if (worker === 'orchestrator') return i18n.t('settings.stats.orchestratorModel');
    if (worker === 'auxiliary') return i18n.t('settings.stats.auxiliaryModel');
    return getWorkerDisplayName(worker);
  }

  function workerModelLabel(worker: string): string {
    const stats = getWorkerStats(worker);
    const status = modelStatuses[worker];
    return (
      stats?.resolvedModel
      || status?.model
      || (status?.status === 'not_configured'
        ? i18n.t('settings.stats.notConfigured')
        : status?.status === 'disabled'
          ? i18n.t('settings.stats.disabled')
          : i18n.t('settings.stats.unknownModel'))
    );
  }

  // ============ 底层：每个角色（statsDisplayKeys）的原子统计 ============
  interface RoleAtom {
    worker: string;
    resolvedModel: string | null;
    label: string;
    modelLabel: string;
    statusObj: any;
    statusClass: string;
    statusKey: string;
    isError: boolean;
    errorMsg: string | null;
    totalIn: number;
    totalOut: number;
    totalTokens: number;
    calls: number;
    successCount: number;
    successRate: number | null;
  }

  const roleAtoms = $derived<RoleAtom[]>(
    statsDisplayKeys.map((worker: string): RoleAtom => {
      const stats = getWorkerStats(worker);
      const statusObj = modelStatuses[worker] || { status: stats ? 'configured' : 'checking' };
      const totalIn = stats?.totalInputTokens ?? 0;
      const totalOut = stats?.totalOutputTokens ?? 0;
      const calls = stats?.totalExecutions ?? 0;
      const successCount = stats?.successCount ?? 0;
      const resolvedModel = stats?.resolvedModel || statusObj?.model || null;
      return {
        worker,
        resolvedModel,
        label: workerLabel(worker),
        modelLabel: workerModelLabel(worker),
        statusObj,
        statusClass: getStatusClass(statusObj?.status || 'checking'),
        statusKey: statusObj?.status || 'checking',
        isError: statusObj?.status === 'error',
        errorMsg: statusObj?.error || null,
        totalIn,
        totalOut,
        totalTokens: totalIn + totalOut,
        calls,
        successCount,
        successRate: stats?.successRate ?? null,
      };
    })
  );

  // ============ 按引擎聚合：相同 resolvedModel 的角色合并成一行 ============
  interface EngineRow {
    key: string;
    rowKind: 'engine';
    resolvedModel: string | null;
    label: string;        // 模型 id 或 "未解析模型"
    subLabel: string;     // "N 个角色绑定"
    avatarSeed: string;   // 用首个 member 的 worker 做色卡 seed
    members: RoleAtom[];
    totalIn: number;
    totalOut: number;
    totalTokens: number;
    calls: number;
    successCount: number;
    successRate: number | null;
    isError: boolean;
    statusClass: string;
    statusKey: string;
    errorMsg: string | null;
  }

  const engineRows = $derived.by<EngineRow[]>(() => {
    const buckets = new Map<string, EngineRow>();
    for (const atom of roleAtoms) {
      // 没解析出 resolvedModel 时，每个角色独立成桶（用 worker key 兜底，避免与他人合并）
      const bucketKey = atom.resolvedModel
        ? `model:${atom.resolvedModel}`
        : `unbound:${atom.worker}`;
      let bucket = buckets.get(bucketKey);
      if (!bucket) {
        bucket = {
          key: bucketKey,
          rowKind: 'engine',
          resolvedModel: atom.resolvedModel,
          label: atom.resolvedModel || atom.modelLabel,
          subLabel: '',
          avatarSeed: atom.worker,
          members: [],
          totalIn: 0,
          totalOut: 0,
          totalTokens: 0,
          calls: 0,
          successCount: 0,
          successRate: null,
          isError: false,
          statusClass: atom.statusClass,
          statusKey: atom.statusKey,
          errorMsg: null,
        };
        buckets.set(bucketKey, bucket);
      }
      bucket.members.push(atom);
      bucket.totalIn += atom.totalIn;
      bucket.totalOut += atom.totalOut;
      bucket.totalTokens += atom.totalTokens;
      bucket.calls += atom.calls;
      bucket.successCount += atom.successCount;
      if (atom.isError) {
        bucket.isError = true;
        bucket.statusClass = 'error';
        bucket.statusKey = 'error';
        bucket.errorMsg = atom.errorMsg || bucket.errorMsg;
      }
    }
    return Array.from(buckets.values()).map((b): EngineRow => ({
      ...b,
      subLabel: i18n.t('settings.stats.engineRowMembers', { count: b.members.length }),
      successRate: b.calls > 0 ? b.successCount / b.calls : null,
    }));
  });

  // ============ 视角统一行 shape，下游只看 rows ============
  interface DisplayRow {
    key: string;
    label: string;
    subLabel: string;
    avatarSeed: string;
    totalIn: number;
    totalOut: number;
    totalTokens: number;
    calls: number;
    successRate: number | null;
    isError: boolean;
    statusClass: string;
    statusKey: string;
    errorMsg: string | null;
    isCore: boolean;
    // 仅在某个视角下使用：
    roleAtom?: RoleAtom;
    engineRow?: EngineRow;
  }

  const rows = $derived<DisplayRow[]>(
    perspective === 'engine'
      ? engineRows.map((er: EngineRow): DisplayRow => ({
          key: er.key,
          label: er.label,
          subLabel: er.subLabel,
          avatarSeed: er.avatarSeed,
          totalIn: er.totalIn,
          totalOut: er.totalOut,
          totalTokens: er.totalTokens,
          calls: er.calls,
          successRate: er.successRate,
          isError: er.isError,
          statusClass: er.statusClass,
          statusKey: er.statusKey,
          errorMsg: er.errorMsg,
          isCore: er.members.some((m: RoleAtom) => m.worker === 'orchestrator' || m.worker === 'auxiliary'),
          engineRow: er,
        }))
      : roleAtoms.map((atom: RoleAtom): DisplayRow => ({
          key: atom.worker,
          label: atom.label,
          subLabel: atom.modelLabel,
          avatarSeed: atom.worker,
          totalIn: atom.totalIn,
          totalOut: atom.totalOut,
          totalTokens: atom.totalTokens,
          calls: atom.calls,
          successRate: atom.successRate,
          isError: atom.isError,
          statusClass: atom.statusClass,
          statusKey: atom.statusKey,
          errorMsg: atom.errorMsg,
          isCore: atom.worker === 'orchestrator' || atom.worker === 'auxiliary',
          roleAtom: atom,
        }))
  );

  // perspective 切换 / rows 变化时重置选中
  $effect(() => {
    const list = rows;
    if (!list.length) {
      if (selectedKey !== null) selectedKey = null;
      return;
    }
    const exists = list.some((r) => r.key === selectedKey);
    if (!exists) {
      const sorted = [...list].sort((a, b) => b.totalTokens - a.totalTokens);
      selectedKey = sorted[0].key;
    }
  });

  function switchPerspective(target: Perspective) {
    if (perspective === target) return;
    perspective = target;
    selectedKey = null; // effect 会重新挑 Top
  }

  function selectRow(key: string) {
    selectedKey = key;
  }

  // ============ Insight 派生（始终按引擎维度算 Top，避免视角切换跳变）============
  const errorRoles = $derived(roleAtoms.filter((a) => a.isError));
  const topEngine = $derived(
    engineRows.length
      ? [...engineRows].sort((a, b) => b.totalTokens - a.totalTokens)[0]
      : null
  );
  const topShare = $derived(
    topEngine && totalTokens > 0 ? topEngine.totalTokens / totalTokens : 0
  );
  const topWorstRole = $derived.by(() => {
    // 失败率最高的角色（仅在 calls > 0 时排序）
    const candidates = roleAtoms.filter((a) => a.calls > 0 && a.successRate !== null);
    if (!candidates.length) return null;
    return [...candidates].sort((a, b) => (a.successRate ?? 1) - (b.successRate ?? 1))[0];
  });
  const showWarnCell = $derived(
    errorRoles.length > 0 || (topWorstRole !== null && topWorstRole.successRate !== null && topWorstRole.successRate < 0.7)
  );

  // ============ 选中切片数据 ============
  const selectedRow = $derived(rows.find((r) => r.key === selectedKey) || null);
  const selectedColor = $derived(selectedRow ? getAgentColor(selectedRow.avatarSeed) : null);

  // role 视角下 I/O bar 上限
  const barMaxIO = $derived(
    selectedRow ? Math.max(selectedRow.totalIn, selectedRow.totalOut, 1) : 1
  );

  // engine 视角下成员角色 token 占比 bar 上限
  const memberMaxToken = $derived.by(() => {
    if (!selectedRow?.engineRow) return 1;
    return Math.max(...selectedRow.engineRow.members.map((m) => m.totalTokens), 1);
  });
</script>

<div class="stats-tab-inner scroll-proxy">
  <div class="stats-scroll-panel">
    <!-- 顶部全局动作行（刷新 / 重置）-->
    <div class="actions-row">
      <button
        class="ghost-action"
        class:saving={isRefreshing}
        onclick={refreshConnections}
        disabled={isRefreshing}
        title={isRefreshing ? i18n.t('settings.stats.checking') : i18n.t('settings.stats.check')}
      >
        <Icon name="refresh" size={12} />
        <span>{isRefreshing ? i18n.t('settings.stats.checking') : i18n.t('settings.stats.check')}</span>
      </button>
      <button
        class="ghost-action danger"
        onclick={showResetConfirmDialog}
        title={i18n.t('settings.stats.resetTokens')}
      >
        <Icon name="trash" size={11} />
        <span>{i18n.t('settings.stats.resetTokens')}</span>
      </button>
    </div>

    <!-- Insight Strip · 3 个洞察单元 -->
    <div class="insight-strip">
      <div class="insight-cell">
        <span class="insight-kicker">{i18n.t('settings.stats.insightTopEngineKicker')}</span>
        {#if topEngine && topEngine.totalTokens > 0}
          <span class="insight-headline">{topEngine.label}</span>
          <span class="insight-meta">
            {i18n.t('settings.stats.insightTopEngineMeta', {
              pct: formatPct(topShare),
              calls: topEngine.calls,
              success: formatPct(topEngine.successRate),
            })}
          </span>
        {:else}
          <span class="insight-headline">{i18n.t('settings.stats.insightNoData')}</span>
          <span class="insight-meta">{i18n.t('settings.stats.insightNoDataMeta')}</span>
        {/if}
      </div>

      <div class="insight-cell">
        <span class="insight-kicker">{i18n.t('settings.stats.insightTotalKicker')}</span>
        <span class="insight-headline">{formatTokens(totalTokens)}</span>
        <span class="insight-meta">
          {i18n.t('settings.stats.insightTotalMeta', {
            input: formatTokens(totalInputTokens),
            output: formatTokens(totalOutputTokens),
          })}
        </span>
      </div>

      <div class="insight-cell" class:warn={showWarnCell}>
        {#if errorRoles.length > 0}
          <span class="insight-kicker">{i18n.t('settings.stats.insightWarnKicker')}</span>
          <span class="insight-headline">{errorRoles[0].label}</span>
          <span class="insight-meta">
            {i18n.t('settings.stats.insightWarnMeta', { count: errorRoles.length })}
          </span>
        {:else if topWorstRole && topWorstRole.successRate !== null && topWorstRole.successRate < 0.7}
          <span class="insight-kicker">{i18n.t('settings.stats.insightWarnKicker')}</span>
          <span class="insight-headline">{topWorstRole.label} · {i18n.t('settings.stats.failRate', { pct: formatPct(1 - topWorstRole.successRate) })}</span>
          <span class="insight-meta">{i18n.t('settings.stats.insightWorstSuccessMeta', { calls: topWorstRole.calls })}</span>
        {:else}
          <span class="insight-kicker">{i18n.t('settings.stats.insightHealthyKicker')}</span>
          <span class="insight-headline">{i18n.t('settings.stats.insightHealthyMeta', { count: roleAtoms.length })}</span>
          <span class="insight-meta">&nbsp;</span>
        {/if}
      </div>
    </div>

    <!-- 分屏：左视角列 / 右切片详情 -->
    <div class="stats-split">
      <!-- 左：视角列表 -->
      <div class="stats-left">
        <div class="stats-left-head">
          <span class="left-head-title">
            {perspective === 'engine'
              ? i18n.t('settings.stats.listTitleByEngine')
              : i18n.t('settings.stats.listTitleByRole')}
          </span>
          <div class="seg-toggle" role="tablist" aria-label={i18n.t('settings.stats.perspectiveAria')}>
            <button
              type="button"
              class="seg-pill"
              class:active={perspective === 'engine'}
              role="tab"
              aria-selected={perspective === 'engine'}
              onclick={() => switchPerspective('engine')}
            >{i18n.t('settings.stats.perspectiveByEngine')}</button>
            <button
              type="button"
              class="seg-pill"
              class:active={perspective === 'role'}
              role="tab"
              aria-selected={perspective === 'role'}
              onclick={() => switchPerspective('role')}
            >{i18n.t('settings.stats.perspectiveByRole')}</button>
          </div>
        </div>
        <div class="col-head">
          <span></span>
          <span>{perspective === 'engine' ? i18n.t('settings.stats.colEngine') : i18n.t('settings.stats.colRole')}</span>
          <span class="num">{i18n.t('settings.stats.colCalls')}</span>
          <span class="num">{i18n.t('settings.stats.colSuccess')}</span>
          <span class="num">{i18n.t('settings.stats.colToken')}</span>
        </div>
        <div class="stats-list">
          {#each rows as row (row.key)}
            {@const colorPair = getAgentColor(row.avatarSeed)}
            {@const isSelected = row.key === selectedKey}
            <div
              class="stats-row"
              class:selected={isSelected}
              class:is-error={row.isError}
              role="button"
              tabindex="0"
              onclick={() => selectRow(row.key)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectRow(row.key); } }}
            >
              <div class="row-avatar" style="background: {colorPair.muted}; color: {colorPair.color}">
                <Icon name="model" size={11} />
              </div>
              <div class="row-label-stack">
                <span class="row-name-txt">
                  {row.label}
                  {#if row.isCore}
                    <span class="badge-core">CORE</span>
                  {/if}
                </span>
                <span class="row-model-txt" title={row.subLabel}>{row.subLabel}</span>
              </div>
              <div class="row-metric num">{row.calls || '--'}</div>
              <div
                class="row-metric num"
                class:success={row.successRate != null && row.successRate >= 0.95}
                class:warn={row.successRate != null && row.successRate >= 0.7 && row.successRate < 0.95}
                class:danger={row.successRate != null && row.successRate < 0.7}
              >{formatPct(row.successRate)}</div>
              <div class="row-metric num">{formatTokens(row.totalTokens)}</div>
            </div>
          {/each}
        </div>
      </div>

      <!-- 右：选中切片详情 -->
      <div class="stats-right">
        {#if selectedRow}
          <div class="slice-head">
            <span class="slice-kicker">
              {perspective === 'engine'
                ? i18n.t('settings.stats.sliceKickerByRoleDist')
                : i18n.t('settings.stats.sliceKickerByEngineDist')}
            </span>
            <div class="slice-title-row">
              <div class="slice-avatar" style="background: {selectedColor?.muted}; color: {selectedColor?.color}">
                <Icon name="model" size={13} />
              </div>
              <span class="slice-title">{selectedRow.label}</span>
              <span class="slice-badge {selectedRow.statusClass}">
                <span class="apple-indicator {selectedRow.statusClass}"></span>
                {(statusTexts[selectedRow.statusKey] || statusTexts['checking'])()}
              </span>
            </div>
            <span class="slice-sub">{selectedRow.subLabel}</span>
          </div>

          <div class="slice-kpi">
            <div class="kpi-block">
              <span class="kpi-value">{formatTokens(selectedRow.totalTokens)}</span>
              <span class="kpi-label">{i18n.t('settings.stats.kpiToken')}</span>
            </div>
            <div class="kpi-block">
              <span class="kpi-value">{selectedRow.calls || '--'}</span>
              <span class="kpi-label">{i18n.t('settings.stats.kpiCalls')}</span>
            </div>
            <div class="kpi-block">
              <span class="kpi-value">{formatPct(selectedRow.successRate)}</span>
              <span class="kpi-label">{i18n.t('settings.stats.kpiSuccess')}</span>
            </div>
          </div>

          {#if perspective === 'engine' && selectedRow.engineRow}
            <!-- 引擎视角下：列出该引擎的成员角色（哪些角色在跑这个引擎） -->
            <div class="slice-section">
              <div class="slice-section-head">
                <span>{i18n.t('settings.stats.sectionEngineMembers')}</span>
                <span class="small-note">{i18n.t('settings.stats.sectionMonthly')}</span>
              </div>
              <div class="bar-list">
                {#each [...selectedRow.engineRow.members].sort((a, b) => b.totalTokens - a.totalTokens) as member (member.worker)}
                  {@const memberShare = selectedRow.engineRow.totalTokens > 0
                    ? member.totalTokens / selectedRow.engineRow.totalTokens
                    : 0}
                  <div class="bar-item">
                    <span class="bar-label" title={member.label}>{member.label}</span>
                    <div class="bar-track">
                      <div class="bar-fill" style="width: {(member.totalTokens / memberMaxToken) * 100}%"></div>
                    </div>
                    <span class="bar-meta">
                      <span class="strong">{formatTokens(member.totalTokens)}</span>
                      <span class="dim"> · {formatPct(memberShare)}</span>
                    </span>
                  </div>
                {/each}
              </div>
            </div>
          {:else}
            <!-- 角色视角下：保留 I/O 分布 -->
            <div class="slice-section">
              <div class="slice-section-head">
                <span>{i18n.t('settings.stats.sectionIO')}</span>
                <span class="small-note">{i18n.t('settings.stats.sectionMonthly')}</span>
              </div>
              <div class="bar-list">
                <div class="bar-item">
                  <span class="bar-label">{i18n.t('settings.stats.barInputLabel')}</span>
                  <div class="bar-track">
                    <div class="bar-fill" style="width: {(selectedRow.totalIn / barMaxIO) * 100}%"></div>
                  </div>
                  <span class="bar-meta">
                    <span class="strong">{formatTokens(selectedRow.totalIn)}</span>
                  </span>
                </div>
                <div class="bar-item muted">
                  <span class="bar-label">{i18n.t('settings.stats.barOutputLabel')}</span>
                  <div class="bar-track">
                    <div class="bar-fill" style="width: {(selectedRow.totalOut / barMaxIO) * 100}%"></div>
                  </div>
                  <span class="bar-meta">
                    <span class="strong">{formatTokens(selectedRow.totalOut)}</span>
                  </span>
                </div>
              </div>
            </div>
          {/if}

          {#if selectedRow.errorMsg}
            <div class="slice-error">
              <Icon name="warning" size={12} />
              <span title={selectedRow.errorMsg}>{selectedRow.errorMsg}</span>
            </div>
          {/if}
        {:else}
          <div class="slice-empty">
            <Icon name="model" size={28} />
            <span>{i18n.t('settings.stats.sliceEmptyTitle')}</span>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .stats-tab-inner {
    container-type: inline-size;
    container-name: stats-tab;
    display: flex;
    flex-direction: column;
    height: 100%;
    width: 100%;
    min-height: 0;
    overflow: hidden;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif;

    --ind-bg-card: rgba(255, 255, 255, 0.92);
    --ind-bg-card-elevated: #ffffff;
    --ind-border-card: rgba(60, 60, 67, 0.16);
    --ind-border-card-strong: rgba(60, 60, 67, 0.2);
    --ind-border-separator: rgba(60, 60, 67, 0.10);
    --ind-foreground: #1d1d1f;
    --ind-foreground-secondary: #515154;
    --ind-foreground-muted: #86868b;
    --ind-foreground-soft: #aeaeb2;
    --ind-radius-card: 12px;
    --ind-shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.04), 0 6px 18px rgba(0, 0, 0, 0.05);
    --ind-row-hover: rgba(0, 0, 0, 0.025);
    --ind-row-selected: color-mix(in srgb, var(--primary, #0a84ff) 8%, transparent);
  }

  :global(body.vscode-dark) .stats-tab-inner,
  :global(body.theme-dark) .stats-tab-inner,
  :global(:root.theme-dark) .stats-tab-inner {
    --ind-bg-card: rgba(255, 255, 255, 0.04);
    --ind-bg-card-elevated: rgba(255, 255, 255, 0.07);
    --ind-border-card: rgba(255, 255, 255, 0.14);
    --ind-border-card-strong: rgba(255, 255, 255, 0.20);
    --ind-border-separator: rgba(255, 255, 255, 0.08);
    --ind-foreground: var(--foreground);
    --ind-foreground-secondary: color-mix(in srgb, var(--foreground) 70%, var(--foreground-muted) 30%);
    --ind-foreground-muted: var(--foreground-muted);
    --ind-foreground-soft: color-mix(in srgb, var(--foreground-muted) 65%, transparent);
    --ind-row-hover: rgba(255, 255, 255, 0.04);
    --ind-row-selected: color-mix(in srgb, var(--primary, #0a84ff) 14%, transparent);
  }

  .scroll-proxy { min-height: 0; }
  .stats-scroll-panel {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 4px 4px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    scrollbar-width: none;
  }
  .stats-scroll-panel::-webkit-scrollbar { width: 0; }

  /* ---------- Action Row ---------- */
  .actions-row {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }
  .ghost-action {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 5px 10px;
    font-size: 11px;
    font-weight: 500;
    color: var(--ind-foreground-secondary);
    background: var(--ind-bg-card);
    border: 1px solid var(--ind-border-card);
    border-radius: 7px;
    cursor: pointer;
    transition: background 0.15s ease, border-color 0.15s ease, color 0.15s ease;
  }
  .ghost-action:hover:not(:disabled) {
    color: var(--ind-foreground);
    border-color: var(--ind-border-card-strong);
    background: var(--ind-bg-card-elevated);
  }
  .ghost-action:disabled { opacity: 0.55; cursor: default; }
  .ghost-action.saving :global(svg) { animation: stats-spin 1s linear infinite; }
  .ghost-action.danger { color: var(--error, #ff3b30); }
  .ghost-action.danger:hover {
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, transparent);
    border-color: color-mix(in srgb, var(--error, #ff3b30) 30%, var(--ind-border-card));
  }
  @keyframes stats-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* ---------- Insight Strip ---------- */
  .insight-strip {
    display: grid;
    grid-template-columns: 1.2fr 1fr 1fr;
    gap: 0;
    background: var(--ind-bg-card);
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    box-shadow: var(--ind-shadow-sm);
    overflow: hidden;
  }
  .insight-cell {
    padding: 12px 18px;
    border-right: 1px solid var(--ind-border-separator);
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .insight-cell:last-child { border-right: none; }
  .insight-kicker {
    font-size: 9.5px;
    font-weight: 600;
    color: var(--ind-foreground-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .insight-headline {
    font-size: 13px;
    font-weight: 600;
    color: var(--ind-foreground);
    letter-spacing: -0.01em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insight-meta {
    font-size: 10.5px;
    color: var(--ind-foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insight-cell.warn .insight-headline { color: var(--error, #ff3b30); }

  /* ---------- Stats Split Layout ---------- */
  .stats-split {
    display: grid;
    grid-template-columns: minmax(0, 1.05fr) minmax(0, 1.25fr);
    gap: 12px;
  }

  /* ---------- Left: Agents List ---------- */
  .stats-left {
    background: var(--ind-bg-card);
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    box-shadow: var(--ind-shadow-sm);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .stats-left-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 14px;
    border-bottom: 1px solid var(--ind-border-separator);
    gap: 10px;
  }
  .left-head-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--ind-foreground);
    letter-spacing: -0.01em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .seg-toggle {
    display: inline-flex;
    padding: 2px;
    border-radius: 7px;
    background: color-mix(in srgb, var(--ind-foreground) 5%, transparent);
    border: 1px solid var(--ind-border-separator);
    flex-shrink: 0;
  }
  .seg-pill {
    border: none;
    background: transparent;
    color: var(--ind-foreground-muted);
    font-size: 10.5px;
    font-weight: 600;
    padding: 3px 9px;
    border-radius: 5px;
    cursor: pointer;
    letter-spacing: -0.005em;
    transition: background 0.15s ease, color 0.15s ease, box-shadow 0.15s ease;
  }
  .seg-pill:hover:not(.active) {
    color: var(--ind-foreground);
  }
  .seg-pill.active {
    background: var(--ind-bg-card-elevated);
    color: var(--ind-foreground);
    box-shadow: 0 1px 1.5px rgba(0, 0, 0, 0.06);
  }
  :global(body.vscode-dark) .seg-pill.active,
  :global(body.theme-dark) .seg-pill.active,
  :global(:root.theme-dark) .seg-pill.active {
    box-shadow: 0 1px 1.5px rgba(0, 0, 0, 0.4);
  }

  .col-head {
    display: grid;
    grid-template-columns: 24px minmax(0, 1.2fr) 50px 50px 64px;
    padding: 6px 14px;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--ind-foreground-soft);
    text-transform: uppercase;
    gap: 10px;
    background: color-mix(in srgb, var(--ind-foreground) 3%, transparent);
    border-bottom: 1px solid var(--ind-border-separator);
  }
  .col-head .num { text-align: right; }
  .stats-list {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .stats-row {
    display: grid;
    grid-template-columns: 24px minmax(0, 1.2fr) 50px 50px 64px;
    align-items: center;
    gap: 10px;
    padding: 9px 14px;
    border-bottom: 1px solid var(--ind-border-separator);
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .stats-row:last-child { border-bottom: none; }
  .stats-row:hover { background: var(--ind-row-hover); }
  .stats-row.selected { background: var(--ind-row-selected); }
  .stats-row.is-error .row-name-txt::after {
    content: '';
    display: inline-block;
    width: 5px; height: 5px;
    border-radius: 50%;
    background: var(--error, #ff3b30);
    margin-left: 6px;
    vertical-align: 2px;
  }
  .row-avatar {
    width: 22px; height: 22px;
    border-radius: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }
  .row-label-stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
  }
  .row-name-txt {
    font-size: 12px;
    font-weight: 600;
    color: var(--ind-foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    letter-spacing: -0.01em;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .badge-core {
    font-size: 7.5px;
    font-weight: 700;
    padding: 1px 4px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--ind-foreground) 8%, transparent);
    color: var(--ind-foreground-soft);
    letter-spacing: 0.04em;
  }
  .row-model-txt {
    font-size: 10px;
    color: var(--ind-foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .row-metric {
    font-size: 11.5px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--ind-foreground);
    letter-spacing: -0.02em;
  }
  .row-metric.num { text-align: right; }
  .row-metric.success { color: var(--success, #34c759); }
  .row-metric.warn { color: var(--warning, #ff9500); }
  .row-metric.danger { color: var(--error, #ff3b30); }

  /* ---------- Right: Slice Detail ---------- */
  .stats-right {
    background: var(--ind-bg-card);
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    box-shadow: var(--ind-shadow-sm);
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }
  .slice-head {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .slice-kicker {
    font-size: 9.5px;
    font-weight: 600;
    color: var(--ind-foreground-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .slice-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .slice-avatar {
    width: 26px; height: 26px;
    border-radius: 7px;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }
  .slice-title {
    font-size: 14.5px;
    font-weight: 650;
    color: var(--ind-foreground);
    letter-spacing: -0.015em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
    min-width: 0;
  }
  .slice-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 7px;
    font-size: 9.5px;
    font-weight: 600;
    border-radius: 999px;
    background: color-mix(in srgb, var(--ind-foreground) 5%, transparent);
    color: var(--ind-foreground-secondary);
    flex-shrink: 0;
  }
  .slice-badge.success { color: var(--success, #34c759); background: color-mix(in srgb, var(--success, #34c759) 10%, transparent); }
  .slice-badge.error { color: var(--error, #ff3b30); background: color-mix(in srgb, var(--error, #ff3b30) 10%, transparent); }
  .slice-badge.warning { color: var(--warning, #ff9500); background: color-mix(in srgb, var(--warning, #ff9500) 10%, transparent); }
  .slice-badge.checking { color: var(--info, #0a84ff); background: color-mix(in srgb, var(--info, #0a84ff) 10%, transparent); }
  .apple-indicator {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .apple-indicator.checking { animation: stats-pulse 1.4s ease-in-out infinite; }
  @keyframes stats-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.45; } }
  .slice-sub {
    font-size: 10.5px;
    color: var(--ind-foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .slice-kpi {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 0;
    border-top: 1px solid var(--ind-border-separator);
    border-bottom: 1px solid var(--ind-border-separator);
    padding: 10px 0;
  }
  .kpi-block {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 0 14px;
    border-right: 1px solid var(--ind-border-separator);
  }
  .kpi-block:first-child { padding-left: 0; }
  .kpi-block:last-child { border-right: none; padding-right: 0; }
  .kpi-value {
    font-size: 18px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    color: var(--ind-foreground);
    letter-spacing: -0.4px;
  }
  .kpi-label {
    font-size: 9.5px;
    font-weight: 600;
    color: var(--ind-foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .slice-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .slice-section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 11px;
    font-weight: 600;
    color: var(--ind-foreground-secondary);
  }
  .slice-section-head .small-note {
    font-size: 10px;
    color: var(--ind-foreground-muted);
    font-weight: 500;
  }
  .bar-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .bar-item {
    display: grid;
    grid-template-columns: 88px minmax(0, 1fr) 96px;
    align-items: center;
    gap: 10px;
  }
  .bar-label {
    font-size: 11px;
    color: var(--ind-foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bar-track {
    height: 8px;
    background: color-mix(in srgb, var(--ind-foreground) 5%, transparent);
    border-radius: 999px;
    position: relative;
    overflow: hidden;
  }
  .bar-fill {
    position: absolute;
    top: 0; bottom: 0; left: 0;
    background: var(--ind-foreground);
    border-radius: 999px;
    transition: width 0.25s ease;
  }
  .bar-item.muted .bar-fill { background: var(--ind-foreground-muted); }
  .bar-meta {
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    color: var(--ind-foreground-muted);
    text-align: right;
  }
  .bar-meta .strong { color: var(--ind-foreground); font-weight: 600; }
  .bar-meta .dim { color: var(--ind-foreground-muted); }

  .slice-error {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 10px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, transparent);
    color: var(--error, #ff3b30);
    font-size: 11px;
    line-height: 1.4;
  }
  .slice-error span {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .slice-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--ind-foreground-muted);
    font-size: 12px;
    padding: 40px 20px;
  }
  .slice-empty :global(svg) { opacity: 0.5; }

  /* ---------- Container-driven Responsive ---------- */
  @container stats-tab (max-width: 760px) {
    .insight-strip {
      grid-template-columns: 1fr;
    }
    .insight-cell {
      border-right: none;
      border-bottom: 1px solid var(--ind-border-separator);
    }
    .insight-cell:last-child { border-bottom: none; }
    .stats-split {
      grid-template-columns: 1fr;
    }
  }
</style>
