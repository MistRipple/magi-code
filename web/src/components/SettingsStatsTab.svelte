<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { getAgentColor } from '../lib/agent-colors';
  import Icon from './Icon.svelte';
  import type { ModelStatus, ModelStatusMap } from '../types/message';
  import type {
    AgentExecutionModelStatsItem,
    AgentExecutionStatsItem,
  } from '../web/agent-api';

  let {
    totalInputTokens,
    totalOutputTokens,
    totalTokens,
    isRefreshing,
    refreshConnections,
    showResetConfirmDialog,
    modelStatuses,
    bindingUsageStats,
    modelUsageStats,
    getWorkerStats,
    getStatsRoleModelStatus,
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
    bindingUsageStats: AgentExecutionStatsItem[];
    modelUsageStats: AgentExecutionModelStatsItem[];
    getWorkerStats: (worker: string) => any;
    getStatsRoleModelStatus: (roleKey: string) => ModelStatus | undefined;
    getStatusClass: (status: string) => string;
    getWorkerDisplayName: (worker: string) => string;
    statusTexts: Record<string, () => string>;
    statsDisplayKeys: string[];
  }>();

  type Perspective = 'role' | 'engine';
  const LEGACY_IMAGE_MODEL = '__legacy_image_generation__';
  let perspective = $state<Perspective>('engine');
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
    if (worker === 'imageGeneration') return i18n.t('settings.stats.imageModel');
    return getWorkerDisplayName(worker);
  }

  function modelDisplayLabel(model: string): string {
    return model === LEGACY_IMAGE_MODEL
      ? i18n.t('settings.stats.legacyImageModel')
      : model;
  }

  function formatList(items: string[]): string {
    return new Intl.ListFormat(i18n.locale, { style: 'short', type: 'conjunction' }).format(items);
  }

  function workerModelLabel(worker: string): string {
    const stats = getWorkerStats(worker);
    if (!stats) {
      return i18n.t('settings.stats.noUsage');
    }
    const resolvedModels = Array.isArray(stats?.resolvedModels) ? stats.resolvedModels : [];
    if (resolvedModels.length === 1) {
      return i18n.t('settings.stats.usedModel', { model: modelDisplayLabel(resolvedModels[0]) });
    }
    if (resolvedModels.length > 1) {
      return i18n.t('settings.stats.usedModels', {
        count: resolvedModels.length,
        models: resolvedModels.map(modelDisplayLabel).join(' · '),
      });
    }
    return i18n.t('settings.stats.unknownModel');
  }

  // ============ 底层：每个角色（statsDisplayKeys）的原子统计 ============
  interface RoleAtom {
    worker: string;
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
    tokenOptional: boolean;
  }

  const roleAtoms = $derived<RoleAtom[]>(
    statsDisplayKeys.map((worker: string): RoleAtom => {
      const stats = getWorkerStats(worker);
      const statusObj = getStatsRoleModelStatus(worker) || { status: stats ? 'recorded' : 'checking' };
      const totalIn = stats?.totalInputTokens ?? 0;
      const totalOut = stats?.totalOutputTokens ?? 0;
      const calls = stats?.totalExecutions ?? 0;
      const successCount = stats?.successCount ?? 0;
      return {
        worker,
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
        tokenOptional: worker === 'imageGeneration',
      };
    })
  );

  function resolveModelRuntimeStatus(identityKeys: string[]) {
    const identitySet = new Set(identityKeys);
    const bindings: AgentExecutionStatsItem[] = bindingUsageStats.filter((item: AgentExecutionStatsItem) => (
      item.modelIdentityKey && identitySet.has(item.modelIdentityKey)
    ));
    const statusKeys: string[] = Array.from(new Set(bindings.map((binding: AgentExecutionStatsItem) => {
      if (binding.role === 'orchestrator') return 'orchestrator';
      if (binding.role === 'auxiliary') return 'auxiliary';
      if (binding.role === 'image_generation') return 'imageGeneration';
      return binding.engineId || 'orchestrator';
    })));
    const statuses = statusKeys
      .map((key) => modelStatuses[key])
      .filter((status): status is ModelStatus => Boolean(status));
    const status = statuses.find((item) => item.status === 'error')
      || statuses.find((item) => item.status === 'available' || item.status === 'connected')
      || statuses.find((item) => item.status === 'configured')
      || statuses[0];
    if (!status) {
      return {
        statusClass: 'recorded',
        statusKey: 'recorded',
        isError: false,
        errorMsg: null,
        isCore: bindings.some((binding: AgentExecutionStatsItem) => binding.role !== 'worker'),
      };
    }
    const statusKey = status?.status || 'recorded';
    const statusClass = getStatusClass(statusKey);
    return {
      statusClass,
      statusKey,
      isError: statusClass === 'error',
      errorMsg: status?.error || null,
      isCore: bindings.some((binding: AgentExecutionStatsItem) => binding.role !== 'worker'),
    };
  }

  // ============ 模型视角：直接使用后端按模型身份聚合的权威统计 ============
  interface EngineRow {
    key: string;
    rowKind: 'engine';
    resolvedModel: string;
    label: string;
    subLabel: string;
    avatarSeed: string;
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
    isCore: boolean;
    identityKeys: string[];
    connectionCount: number;
    sourceLabels: string[];
    tokenOptional: boolean;
  }

  interface ModelBucket {
    resolvedModel: string;
    identityKeys: string[];
    totalIn: number;
    totalOut: number;
    calls: number;
    successCount: number;
  }

  const engineRows = $derived.by<EngineRow[]>(() => {
    const buckets = new Map<string, ModelBucket>();
    for (const model of [...modelUsageStats].sort((left, right) => (
      right.totals.totalTokens - left.totals.totalTokens
      || left.resolvedModel.localeCompare(right.resolvedModel)
    ))) {
      const resolvedModel = model.resolvedModel.trim() || model.declaredModelSpec.trim() || i18n.t('settings.stats.unknownModel');
      const key = resolvedModel.toLocaleLowerCase();
      const bucket: ModelBucket = buckets.get(key) || {
        resolvedModel,
        identityKeys: [],
        totalIn: 0,
        totalOut: 0,
        calls: 0,
        successCount: 0,
      };
      bucket.identityKeys.push(model.modelIdentityKey);
      bucket.totalIn += model.totals.netInputTokens;
      bucket.totalOut += model.totals.netOutputTokens;
      bucket.calls += model.totals.llmCallCount;
      bucket.successCount += model.totals.successCount;
      buckets.set(key, bucket);
    }

    return Array.from(buckets.entries())
      .map(([key, bucket]): EngineRow => {
        const identitySet = new Set(bucket.identityKeys);
        const sourceBindings = bindingUsageStats
            .filter((binding: AgentExecutionStatsItem) => binding.modelIdentityKey && identitySet.has(binding.modelIdentityKey))
            .sort((left: AgentExecutionStatsItem, right: AgentExecutionStatsItem) => {
              const sourceOrder = (binding: AgentExecutionStatsItem) => {
                if (binding.role === 'orchestrator') return 0;
                if (binding.role === 'auxiliary') return 1;
                if (binding.role === 'image_generation') return 2;
                const roleIndex = statsDisplayKeys.indexOf(binding.templateId);
                return roleIndex >= 0 ? roleIndex + 3 : Number.MAX_SAFE_INTEGER;
              };
              return sourceOrder(left) - sourceOrder(right)
                || left.templateId.localeCompare(right.templateId);
            });
        const sourceLabels: string[] = Array.from(new Set<string>(
          sourceBindings.map((binding: AgentExecutionStatsItem) => workerLabel(bindingRoleKey(binding))),
        ));
        return {
          key,
          rowKind: 'engine',
          resolvedModel: bucket.resolvedModel,
          label: modelDisplayLabel(bucket.resolvedModel),
          subLabel: sourceLabels.length > 0
            ? i18n.t('settings.stats.modelUsedBy', { roles: formatList(sourceLabels) })
            : i18n.t('settings.stats.recordedUsage'),
          avatarSeed: key,
          totalIn: bucket.totalIn,
          totalOut: bucket.totalOut,
          totalTokens: bucket.totalIn + bucket.totalOut,
          calls: bucket.calls,
          successCount: bucket.successCount,
          successRate: bucket.calls > 0 ? bucket.successCount / bucket.calls : null,
          identityKeys: bucket.identityKeys,
          connectionCount: bucket.identityKeys.length,
          sourceLabels,
          tokenOptional: sourceBindings.some(
            (binding: AgentExecutionStatsItem) => binding.role === 'image_generation',
          ),
          ...resolveModelRuntimeStatus(bucket.identityKeys),
        };
      })
      .sort((a: EngineRow, b: EngineRow) => (
        b.totalTokens - a.totalTokens || b.calls - a.calls
      ));
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
    tokenOptional: boolean;
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
          isCore: er.isCore,
          tokenOptional: er.tokenOptional,
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
          isCore: atom.worker === 'orchestrator' || atom.worker === 'auxiliary' || atom.worker === 'imageGeneration',
          tokenOptional: atom.tokenOptional,
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
      const sorted = [...list].sort((a, b) => b.totalTokens - a.totalTokens || b.calls - a.calls);
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

  interface UsageBreakdownRow {
    key: string;
    label: string;
    avatarSeed: string;
    totalIn: number;
    totalOut: number;
    totalTokens: number;
    calls: number;
    successCount: number;
    successRate: number | null;
    tokenOptional: boolean;
  }

  function bindingRoleKey(binding: AgentExecutionStatsItem): string {
    if (binding.role === 'orchestrator') return 'orchestrator';
    if (binding.role === 'auxiliary') return 'auxiliary';
    if (binding.role === 'image_generation') return 'imageGeneration';
    return binding.templateId.trim();
  }

  function bindingModelLabel(binding: AgentExecutionStatsItem): string {
    const model = binding.resolvedModel?.trim()
      || binding.declaredModelSpec?.trim()
      || i18n.t('settings.stats.unknownModel');
    return modelDisplayLabel(model);
  }

  function aggregateUsageBreakdown(
    items: AgentExecutionStatsItem[],
    keyOf: (binding: AgentExecutionStatsItem) => string,
    labelOf: (binding: AgentExecutionStatsItem) => string,
  ): UsageBreakdownRow[] {
    const buckets = new Map<string, UsageBreakdownRow>();
    for (const binding of items) {
      const key = keyOf(binding);
      if (!key) continue;
      const bucket = buckets.get(key) || {
        key,
        label: labelOf(binding),
        avatarSeed: key,
        totalIn: 0,
        totalOut: 0,
        totalTokens: 0,
        calls: 0,
        successCount: 0,
        successRate: null,
        tokenOptional: false,
      };
      bucket.totalIn += binding.netInputTokens;
      bucket.totalOut += binding.netOutputTokens;
      bucket.totalTokens += binding.totalTokens;
      bucket.calls += binding.llmCallCount;
      bucket.successCount += binding.successCount;
      bucket.successRate = bucket.calls > 0 ? bucket.successCount / bucket.calls : null;
      bucket.tokenOptional ||= binding.role === 'image_generation';
      buckets.set(key, bucket);
    }
    return Array.from(buckets.values()).sort((left, right) => (
      right.totalTokens - left.totalTokens
      || right.calls - left.calls
      || left.label.localeCompare(right.label)
    ));
  }

  // ============ Insight 派生（始终按引擎维度算 Top，避免视角切换跳变）============
  const errorRoles = $derived(roleAtoms.filter((a) => a.isError));
  const topEngine = $derived(
    engineRows.length
      ? [...engineRows].sort((a, b) => b.totalTokens - a.totalTokens || b.calls - a.calls)[0]
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

  const selectedBreakdown = $derived.by<UsageBreakdownRow[]>(() => {
    if (!selectedRow) return [];

    if (perspective === 'engine') {
      const identityKeys = new Set(selectedRow.engineRow?.identityKeys || []);
      return aggregateUsageBreakdown(
        bindingUsageStats.filter((binding: AgentExecutionStatsItem) => (
          Boolean(binding.modelIdentityKey) && identityKeys.has(binding.modelIdentityKey as string)
        )),
        bindingRoleKey,
        (binding) => workerLabel(bindingRoleKey(binding)),
      );
    }

    const roleKey = selectedRow.roleAtom?.worker || selectedRow.key;
    return aggregateUsageBreakdown(
      bindingUsageStats.filter((binding: AgentExecutionStatsItem) => bindingRoleKey(binding) === roleKey),
      (binding) => bindingModelLabel(binding).toLocaleLowerCase(),
      bindingModelLabel,
    );
  });

  // role 视角下 I/O bar 上限
  const barMaxIO = $derived(
    selectedRow ? Math.max(selectedRow.totalIn, selectedRow.totalOut, 1) : 1
  );

  const selectedTokenUnavailable = $derived(
    Boolean(selectedRow && selectedRow.calls > 0 && selectedRow.totalTokens === 0 && selectedRow.tokenOptional)
  );

  function formatUsageTokens(tokens: number, calls: number, tokenOptional: boolean): string {
    if (calls > 0 && tokens === 0 && tokenOptional) {
      return i18n.t('settings.stats.tokenNotReported');
    }
    return formatTokens(tokens);
  }

</script>

<div class="stats-tab-inner scroll-proxy">
  <div class="stats-scroll-panel">
    <!-- Insight Strip · 3 个洞察单元 + 全局动作 -->
    <div class="insight-strip">
      <div class="insight-cell">
        <span class="insight-kicker">{i18n.t('settings.stats.insightTopEngineKicker')}</span>
        {#if topEngine && topEngine.calls > 0}
          <span class="insight-headline">{topEngine.label}</span>
          <span class="insight-meta">
            {topEngine.totalTokens > 0
              ? i18n.t('settings.stats.insightTopEngineMeta', {
                  pct: formatPct(topShare),
                  calls: topEngine.calls,
                  success: formatPct(topEngine.successRate),
                })
              : i18n.t('settings.stats.insightTopEngineCallsMeta', {
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
        <div class="insight-actions">
          <button
            class="ghost-action"
            class:saving={isRefreshing}
            onclick={refreshConnections}
            disabled={isRefreshing}
            aria-label={isRefreshing ? i18n.t('settings.stats.checking') : i18n.t('settings.stats.check')}
            title={isRefreshing ? i18n.t('settings.stats.checking') : i18n.t('settings.stats.check')}
          >
            <Icon name="refresh" size={14} />
          </button>
          <button
            class="ghost-action danger"
            onclick={showResetConfirmDialog}
            aria-label={i18n.t('settings.stats.resetWorkspace')}
            title={i18n.t('settings.stats.resetWorkspace')}
          >
            <Icon name="trash" size={13} />
          </button>
        </div>
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
          <span>{perspective === 'engine'
              ? i18n.t('settings.stats.colEngine')
              : i18n.t('settings.stats.colRole')}</span>
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
                <Icon name={perspective === 'role' ? 'bot' : 'model'} size={11} />
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
              <div class="row-metric num">{formatUsageTokens(row.totalTokens, row.calls, row.tokenOptional)}</div>
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
                ? i18n.t('settings.stats.sliceKickerByModelUsage')
                : i18n.t('settings.stats.sliceKickerByRoleUsage')}
            </span>
            <div class="slice-title-row">
              <div class="slice-avatar" style="background: {selectedColor?.muted}; color: {selectedColor?.color}">
                <Icon name={perspective === 'role' ? 'bot' : 'model'} size={13} />
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
              <span class="kpi-value">{formatUsageTokens(selectedRow.totalTokens, selectedRow.calls, selectedRow.tokenOptional)}</span>
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

          <div class="slice-section">
            <div class="slice-section-head">
              <span>{perspective === 'engine'
                ? i18n.t('settings.stats.breakdownByRole')
                : i18n.t('settings.stats.breakdownByModel')}</span>
              <span class="small-note">{i18n.t('settings.stats.sectionMonthly')}</span>
            </div>
            {#if selectedBreakdown.length > 0}
              <div class="breakdown-head">
                <span>{perspective === 'engine'
                  ? i18n.t('settings.stats.colRole')
                  : i18n.t('settings.stats.colEngine')}</span>
                <span class="num">{i18n.t('settings.stats.colCalls')}</span>
                <span class="num">{i18n.t('settings.stats.colSuccess')}</span>
                <span class="num">{i18n.t('settings.stats.colToken')}</span>
              </div>
              <div class="breakdown-list">
                {#each selectedBreakdown as breakdown (breakdown.key)}
                  {@const breakdownColor = getAgentColor(breakdown.avatarSeed)}
                  <div class="breakdown-row">
                    <div class="breakdown-label">
                      <span class="breakdown-avatar" style="background: {breakdownColor.muted}; color: {breakdownColor.color}">
                        <Icon name={perspective === 'engine' ? 'bot' : 'model'} size={10} />
                      </span>
                      <span title={breakdown.label}>{breakdown.label}</span>
                    </div>
                    <span class="breakdown-metric num">{breakdown.calls || '--'}</span>
                    <span class="breakdown-metric num">{formatPct(breakdown.successRate)}</span>
                    <span class="breakdown-metric num">{formatUsageTokens(breakdown.totalTokens, breakdown.calls, breakdown.tokenOptional)}</span>
                  </div>
                {/each}
              </div>
            {:else}
              <div class="breakdown-empty">{i18n.t('settings.stats.breakdownEmpty')}</div>
            {/if}
          </div>

          <div class="slice-section">
            <div class="slice-section-head">
              <span>{i18n.t('settings.stats.sectionIO')}</span>
              <span class="small-note">{i18n.t('settings.stats.sectionMonthly')}</span>
            </div>
            {#if selectedTokenUnavailable}
              <div class="token-usage-note">{i18n.t('settings.stats.imageUsageMetricHint')}</div>
            {:else}
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
            {/if}
          </div>

          {#if selectedRow.errorMsg}
            <div class="slice-error">
              <Icon name="warning" size={12} />
              <span title={i18n.t('settings.stats.connectionIssueTitle')}>{i18n.t('settings.stats.connectionIssue')}</span>
            </div>
          {/if}
        {:else}
          <div class="slice-empty">
            <Icon name={perspective === 'role' ? 'bot' : 'model'} size={28} />
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

  /* ---------- Overview Actions ---------- */
  .ghost-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    padding: 0;
    color: var(--ind-foreground-secondary);
    background: transparent;
    border: 1px solid transparent;
    border-radius: 6px;
    cursor: pointer;
    transition: background 0.15s ease, border-color 0.15s ease, color 0.15s ease;
  }
  .ghost-action:hover:not(:disabled) {
    color: var(--ind-foreground);
    border-color: var(--ind-border-separator);
    background: color-mix(in srgb, var(--ind-foreground) 5%, transparent);
  }
  .ghost-action:disabled { opacity: 0.55; cursor: default; }
  .ghost-action.saving :global(svg) { animation: stats-spin 1s linear infinite; }
  .ghost-action.danger { color: var(--ind-foreground-muted); }
  .ghost-action.danger:hover {
    color: var(--error, #ff3b30);
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, transparent);
    border-color: color-mix(in srgb, var(--error, #ff3b30) 30%, var(--ind-border-card));
  }
  @keyframes stats-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

  /* ---------- Insight Strip ---------- */
  .insight-strip {
    display: grid;
    grid-template-columns: minmax(220px, 1.35fr) minmax(140px, 0.8fr) minmax(250px, 1.15fr);
    gap: 0;
    background: var(--ind-bg-card);
    border: 1px solid var(--ind-border-card);
    border-radius: 8px;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.025);
    overflow: hidden;
  }
  .insight-cell {
    padding: 11px 16px 12px;
    border-right: 1px solid var(--ind-border-separator);
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 3px;
    min-width: 0;
  }
  .insight-actions {
    position: absolute;
    top: 7px;
    right: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 2px;
  }
  .insight-kicker {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 10px;
    font-weight: 600;
    color: var(--ind-foreground-muted);
    letter-spacing: 0;
  }
  .insight-headline {
    font-size: 14px;
    font-weight: 600;
    color: var(--ind-foreground);
    letter-spacing: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insight-cell:nth-child(2) .insight-headline {
    font-size: 16px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .insight-meta {
    font-size: 10.5px;
    color: var(--ind-foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insight-cell:nth-child(3) .insight-kicker::before {
    content: '';
    width: 6px;
    height: 6px;
    flex: 0 0 6px;
    border-radius: 50%;
    background: #30a46c;
    box-shadow: 0 0 0 3px color-mix(in srgb, #30a46c 13%, transparent);
  }
  .insight-cell:nth-child(3) {
    position: relative;
    padding-right: 78px;
    border-right: none;
  }
  .insight-cell.warn:nth-child(3) .insight-kicker::before {
    background: var(--error, #ff3b30);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--error, #ff3b30) 13%, transparent);
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
  .token-usage-note {
    padding: 9px 10px;
    border: 1px solid var(--ind-border-separator);
    border-radius: 7px;
    background: color-mix(in srgb, var(--ind-foreground) 2.5%, transparent);
    color: var(--ind-foreground-muted);
    font-size: 10.5px;
    line-height: 1.45;
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

  .breakdown-head,
  .breakdown-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 46px 52px 64px;
    align-items: center;
    gap: 8px;
  }
  .breakdown-head {
    padding: 0 8px 4px;
    color: var(--ind-foreground-soft);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .breakdown-head .num { text-align: right; }
  .breakdown-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .breakdown-row {
    min-height: 30px;
    padding: 4px 8px;
    border-radius: 7px;
    background: color-mix(in srgb, var(--ind-foreground) 3%, transparent);
  }
  .breakdown-label {
    display: flex;
    align-items: center;
    min-width: 0;
    gap: 7px;
    color: var(--ind-foreground);
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .breakdown-label > span:last-child {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .breakdown-avatar {
    width: 18px;
    height: 18px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    border-radius: 5px;
  }
  .breakdown-metric {
    color: var(--ind-foreground-secondary);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
  }
  .breakdown-metric.num { text-align: right; }
  .breakdown-empty {
    padding: 9px 8px;
    color: var(--ind-foreground-muted);
    font-size: 10.5px;
  }

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
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .insight-cell:nth-child(2) { border-right: none; }
    .insight-cell:nth-child(3) {
      grid-column: 1 / -1;
      border-right: none;
      border-top: 1px solid var(--ind-border-separator);
    }
    .stats-split {
      grid-template-columns: 1fr;
    }
  }

  @container stats-tab (max-width: 520px) {
    .insight-strip { grid-template-columns: 1fr; }
    .insight-cell,
    .insight-cell:nth-child(2),
    .insight-cell:nth-child(3) {
      grid-column: auto;
      border-right: none;
      border-top: none;
      border-bottom: none;
    }
  }
</style>
