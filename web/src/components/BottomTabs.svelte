<script lang="ts">
  import { getState, getEnabledAgents } from '../stores/messages.svelte';
  import {
    resolveAgentIndicatorVariant,
    resolveWorkerRuntimeIndicatorVariant,
  } from '../lib/agent-status-indicator';
  import { isWorkerExecutingStatus, selectWorkerRuntime } from '../lib/worker-panel-state';

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
  const workerRuntimeMap = $derived(appState.workerRuntime);

  function collectCurrentTurnWorkerRoles(): CurrentTurnWorkerRoleTab[] {
    const roleById = new Map<string, CurrentTurnWorkerRoleTab>();
    const upsertRole = (role: Partial<CurrentTurnWorkerRoleTab> & { roleId?: string; order?: number }) => {
      const rawRoleId = typeof role.roleId === 'string' ? role.roleId.trim() : '';
      const roleSource = resolveWorkerRoleSource(rawRoleId, enabledAgents, registrySnapshot);
      const roleId = roleSource?.templateId || rawRoleId;
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
      });
    };

    const artifacts = Array.isArray(timelineProjection?.artifacts) ? timelineProjection.artifacts : [];
    let hasStructuredTurnLanes = false;
    for (const artifact of artifacts) {
      const laneMeta = artifact.message?.metadata?.currentTurnWorkerLanes;
      if (Array.isArray(laneMeta) && laneMeta.length > 0) {
        hasStructuredTurnLanes = true;
        for (const lane of laneMeta as CurrentTurnWorkerRoleMeta[]) {
          const roleId = typeof lane?.roleId === 'string' ? lane.roleId.trim() : '';
          if (!roleId || lane?.isPrimary === true) {
            continue;
          }
          const order = typeof lane?.laneSeq === 'number' && Number.isFinite(lane.laneSeq)
            ? Math.max(0, Math.floor(lane.laneSeq))
            : Number.MAX_SAFE_INTEGER;
          const workerId = typeof lane?.worker === 'string' ? lane.worker.trim() : '';
          upsertRole({ roleId, order, workerId });
        }
      }
    }
    if (hasStructuredTurnLanes) {
      return Array.from(roleById.values()).sort((left, right) => {
        if (left.order !== right.order) {
          return left.order - right.order;
        }
        return left.roleId.localeCompare(right.roleId);
      });
    }

    for (const artifact of artifacts) {
      for (const roleId of artifact.workerTabs || []) {
        upsertRole({
          roleId,
          workerId: artifact.worker || '',
        });
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

  function resolveWorkerRuntime(roleId: string) {
    return selectWorkerRuntime(workerRuntimeMap, roleId);
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
      {@const roleSource = resolveWorkerRoleSource(role.roleId, enabledAgents, registrySnapshot)}
      {@const agentColorPair = getAgentColor(role.roleId, roleSource?.colorToken)}
      {@const workerStatus = getWorkerModelStatus(role.roleId)}
      {@const runtime = resolveWorkerRuntime(role.roleId)}
      {@const isExecuting = isWorkerExecutingStatus(runtime?.status)}
      {@const runtimeIndicatorVariant = resolveWorkerRuntimeIndicatorVariant(runtime?.status)}
      {@const indicatorVariant = runtimeIndicatorVariant || resolveAgentIndicatorVariant(workerStatus)}


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
