<script lang="ts">
  import { getState, getEnabledAgents } from '../stores/messages.svelte';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';

  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getAgentColor } from '../lib/agent-colors';
  import { resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';

  interface CurrentTurnWorkerRoleMeta {
    laneId?: string;
    laneSeq?: number;
    worker?: string;
    roleId?: string;
    status?: string;
    title?: string;
    isPrimary?: boolean;
  }

  interface CurrentTurnWorkerRoleTab {
    roleId: string;
    order: number;
    workerId: string;
    status: string;
  }

  interface Props {
    activeTab: string;
    onTabChange: (tab: string) => void;
  }

  let { activeTab, onTabChange }: Props = $props();

  const appState = getState();

  const modelStatus = $derived(appState.modelStatus);
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);
  const timelineProjection = $derived(appState.timelineProjection);

  const WORKER_STATUS_PRIORITY: Record<string, number> = {
    running: 6,
    pending: 5,
    ready: 5,
    awaiting_approval: 4,
    review_required: 4,
    repairing: 4,
    verifying: 4,
    blocked: 3,
    failed: 3,
    error: 3,
    cancelled: 3,
    completed: 1,
    success: 1,
    skipped: 1,
    idle: 1,
  };

  function resolveStrongerStatus(existing: string, next: string): string {
    const normalizedExisting = existing.trim();
    const normalizedNext = next.trim();
    if (!normalizedExisting) return normalizedNext;
    if (!normalizedNext) return normalizedExisting;
    const existingPriority = WORKER_STATUS_PRIORITY[normalizedExisting] ?? 0;
    const nextPriority = WORKER_STATUS_PRIORITY[normalizedNext] ?? 0;
    return nextPriority > existingPriority ? normalizedNext : normalizedExisting;
  }

  function collectCurrentTurnWorkerRoles(): CurrentTurnWorkerRoleTab[] {
    const roleById = new Map<string, CurrentTurnWorkerRoleTab>();
    const upsertRole = (role: Partial<CurrentTurnWorkerRoleTab> & { roleId?: string; order?: number }) => {
      const roleId = typeof role.roleId === 'string' ? role.roleId.trim() : '';
      if (!roleId) {
        return;
      }
      const existing = roleById.get(roleId);
      const order = typeof role.order === 'number' && Number.isFinite(role.order)
        ? Math.max(0, Math.floor(role.order))
        : existing?.order ?? Number.MAX_SAFE_INTEGER;
      roleById.set(roleId, {
        roleId,
        order: Math.min(existing?.order ?? Number.MAX_SAFE_INTEGER, order),
        workerId: existing?.workerId || role.workerId || '',
        status: resolveStrongerStatus(existing?.status || '', role.status || ''),
      });
    };

    const artifacts = Array.isArray(timelineProjection?.artifacts) ? timelineProjection.artifacts : [];
    for (const artifact of artifacts) {
      const laneMeta = artifact.message?.metadata?.currentTurnWorkerLanes;
      if (Array.isArray(laneMeta) && laneMeta.length > 0) {
        for (const lane of laneMeta as CurrentTurnWorkerRoleMeta[]) {
          const roleId = typeof lane?.roleId === 'string' ? lane.roleId.trim() : '';
          if (!roleId || lane?.isPrimary === true) {
            continue;
          }
          const order = typeof lane?.laneSeq === 'number' && Number.isFinite(lane.laneSeq)
            ? Math.max(0, Math.floor(lane.laneSeq))
            : Number.MAX_SAFE_INTEGER;
          const workerId = typeof lane?.worker === 'string' ? lane.worker.trim() : '';
          const status = typeof lane?.status === 'string' ? lane.status.trim() : '';
          upsertRole({ roleId, order, workerId, status });
        }
      }
      for (const roleId of artifact.workerTabs || []) {
        upsertRole({
          roleId,
          workerId: artifact.worker || '',
        });
      }
      for (const item of artifact.executionItems || []) {
        const metadata = item.message?.metadata && typeof item.message.metadata === 'object'
          ? item.message.metadata as Record<string, unknown>
          : {};
        for (const roleId of item.workerTabs || []) {
          const order = typeof metadata.laneSeq === 'number' && Number.isFinite(metadata.laneSeq)
            ? Math.max(0, Math.floor(metadata.laneSeq))
            : Number.MAX_SAFE_INTEGER;
          const workerId = typeof metadata.worker === 'string' ? metadata.worker.trim() : (item.worker || '');
          const status = typeof metadata.laneStatus === 'string'
            ? metadata.laneStatus.trim()
            : (typeof metadata.status === 'string' ? metadata.status.trim() : '');
          upsertRole({ roleId, order, workerId, status });
        }
      }
    }
    return Array.from(roleById.values()).sort((left, right) => {
      if (left.order !== right.order) {
        return left.order - right.order;
      }
      return left.roleId.localeCompare(right.roleId);
    });
  }

  // 模型连接状态（使用全局统一的 modelStatus）

  const currentTurnWorkerRoles = $derived.by(() => collectCurrentTurnWorkerRoles());

  // 底部 tab 只表达角色参与状态，lane/task 只在 worker 面板内容中展示。
  const workerRoleStatusMap = $derived.by(() => {
    const map = new Map<string, string>();
    for (const role of currentTurnWorkerRoles) {
      if (!role.status) continue;
      map.set(role.roleId, role.status);
    }
    return map;
  });

  function resolveEffectiveWorkerExecuting(roleId: string): boolean {
    const roleStatus = workerRoleStatusMap.get(roleId);
    return roleStatus === 'running'
      || roleStatus === 'pending'
      || roleStatus === 'ready'
      || roleStatus === 'repairing'
      || roleStatus === 'verifying';
  }

  function resolveRoleIndicatorVariant(roleId: string): 'brand' | 'warning' | 'error' | 'disabled' | null {
    const roleStatus = workerRoleStatusMap.get(roleId);
    switch (roleStatus) {
      case 'running':
      case 'pending':
      case 'ready':
      case 'repairing':
      case 'verifying':
      case 'completed':
      case 'success':
      case 'skipped':
      case 'idle':
        return 'brand';
      case 'awaiting_approval':
      case 'review_required':
        return 'warning';
      case 'blocked':
      case 'failed':
      case 'error':
      case 'cancelled':
        return 'error';
      case 'disabled':
        return 'disabled';
      default:
        return null;
    }
  }

  function getWorkerModelStatus(workerId: string): string {
    if (!workerId) {
      return 'not_configured';
    }
    const roleSource = resolveWorkerRoleSource(workerId, enabledAgents, registrySnapshot);
    if (roleSource?.enabled === false) {
      return 'disabled';
    }
    const lookupKey = roleSource?.modelSource === 'engine'
      ? (roleSource.engineId || workerId)
      : 'orchestrator';
    return modelStatus[lookupKey]?.status || 'not_configured';
  }

  function resolveRoleDisplayName(role: CurrentTurnWorkerRoleTab, locale: string): string {
    void locale;
    return resolveWorkerDisplayName(role.roleId, enabledAgents, registrySnapshot, (key) => i18n.t(key))
      || role.roleId;
  }

</script>

<div class="bt-bar">
  <button
    class="bt-tab fixed-thread-tab"
    class:active={activeTab === 'thread'}
    data-tab-id="thread"
    onclick={() => onTabChange('thread')}
  >
    <Icon name="chat" size={12} />
    {i18n.t('bottomTabs.thread')}
  </button>
  <div class="bt-workers-scroll">
    {#each currentTurnWorkerRoles as role (role.roleId)}
      {@const locale = i18n.locale}
      {@const isExecuting = resolveEffectiveWorkerExecuting(role.roleId)}
      {@const roleSource = resolveWorkerRoleSource(role.roleId, enabledAgents, registrySnapshot)}
      {@const agentColorPair = getAgentColor(role.roleId, roleSource?.colorToken)}
      {@const workerStatus = getWorkerModelStatus(role.roleId)}
      {@const roleIndicatorVariant = resolveRoleIndicatorVariant(role.roleId)}
      {@const indicatorVariant = roleIndicatorVariant || resolveAgentIndicatorVariant(workerStatus)}


      <button
        class="bt-tab bt-worker"
        class:active={activeTab === role.roleId}
        class:is-executing={isExecuting}
        style="--w-color: {agentColorPair.color}"
        data-tab-id={role.roleId}
        onclick={() => onTabChange(role.roleId)}
        >
        <span class="bt-dot-wrap">
          <span
            class="bt-dot"
            class:brand={indicatorVariant === 'brand'}
            class:disabled={indicatorVariant === 'disabled'}
            class:warning={indicatorVariant === 'warning'}
            class:error={indicatorVariant === 'error'}
            class:executing={isExecuting}
          ></span>
        </span>
        {resolveRoleDisplayName(role, locale)}
      </button>
    {/each}
  </div>
</div>

<style>
  /* ============================================
     BottomTabs - Agent 切换栏
     设计参考: Apple HIG 次级导航栏
     ============================================ */
  .bt-bar {
    display: flex;
    background: var(--glass-bg);
    backdrop-filter: blur(12px);
    -webkit-backdrop-filter: blur(12px);
    border-top: 1px solid var(--border);
    flex-shrink: 0;
    width: 100%;
    overflow: hidden;
  }

  .fixed-thread-tab {
    position: relative;
    padding-left: var(--space-4) !important;
    z-index: 10;
    box-shadow: 2px 0 6px -4px rgba(0,0,0,0.08);
  }

  /* 适配不同主题的边框线分隔 */
  :global([data-theme="dark"]) .fixed-thread-tab {
    border-right: 1px solid rgba(255,255,255,0.05);
  }
  :global([data-theme="light"]) .fixed-thread-tab {
    border-right: 1px solid rgba(0,0,0,0.06);
  }

  .bt-workers-scroll {
    display: flex;
    flex: 1;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
    -ms-overflow-style: none;
    padding-right: var(--space-3);
  }


  .bt-workers-scroll::-webkit-scrollbar {
    display: none;
  }

  .bt-tab {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: 5px var(--space-3);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    cursor: pointer;
    transition: color var(--transition-fast), background var(--transition-fast);
    white-space: nowrap;
    flex-shrink: 0;
    border-radius: var(--radius-sm);
  }

  .bt-tab.active::after {
    content: '';
    position: absolute;
    left: var(--space-2);
    right: var(--space-2);
    bottom: 0;
    height: 2px;
    background: currentColor;
    border-radius: var(--radius-full);
  }

  .bt-tab:hover {
    background: color-mix(in srgb, var(--surface-hover) 55%, transparent);
    color: var(--foreground);
  }

  .bt-tab.active {
    color: var(--foreground);
    background: color-mix(in srgb, var(--surface-active) 42%, transparent);
  }

  /* Worker Tab 激活时使用品牌色 */
  .bt-worker.active {
    color: var(--w-color);
  }

  .bt-worker.is-executing {
    color: color-mix(in srgb, var(--w-color) 82%, var(--foreground));
  }

  .bt-dot-wrap {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 12px;
    height: 12px;
    flex-shrink: 0;
  }

  .bt-dot {
    width: 5px;
    height: 5px;
    border-radius: var(--radius-full);
    background: var(--foreground-muted);
    opacity: 0.4;
    transition: all var(--transition-fast);
  }

  .bt-dot.brand {
    background: var(--w-color);
    opacity: 1;
  }

  .bt-dot.disabled {
    background: var(--foreground-subtle, #94a3b8);
    opacity: 1;
  }

  .bt-dot.warning {
    background: var(--warning, #d97706);
    opacity: 1;
  }

  .bt-dot.error {
    background: var(--error, #dc2626);
    opacity: 1;
  }

  .bt-dot.executing {
    position: relative;
    width: 6px;
    height: 6px;
    background: var(--w-color);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--w-color) 30%, transparent);
  }

  .bt-dot.executing::before,
  .bt-dot.executing::after {
    content: '';
    position: absolute;
    inset: -4px;
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, var(--w-color) 68%, transparent);
    opacity: 0;
    transform: scale(0.55);
    animation: bt-breathe 1.8s ease-out infinite;
  }

  .bt-dot.executing::after {
    animation-delay: 0.9s;
  }

  @keyframes bt-breathe {
    0% {
      opacity: 0;
      transform: scale(0.55);
    }
    30% {
      opacity: 0.65;
    }
    100% {
      opacity: 0;
      transform: scale(1.9);
    }
  }

</style>
