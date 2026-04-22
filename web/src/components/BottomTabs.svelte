<script lang="ts">
  import { getState, getEnabledAgents } from '../stores/messages.svelte';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';

import Icon from './Icon.svelte';
import { i18n } from '../stores/i18n.svelte';
import { getAgentColor } from '../lib/agent-colors';
import { collectWorkerTabIds, resolveWorkerDisplayName, resolveWorkerRoleSource } from '../lib/worker-role-utils';

  interface Props {
    activeTab: string;
    onTabChange: (tab: string) => void;
  }

  let { activeTab, onTabChange }: Props = $props();

  const appState = getState();

  const modelStatus = $derived(appState.modelStatus);
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);

  // 模型连接状态（使用全局统一的 modelStatus）

  const projectionWorkerIds = $derived.by(() => {
    const workerIds = new Set<string>();
    for (const node of appState.timelineNodes || []) {
      for (const workerId of node.workerTabs || []) {
        if (typeof workerId === 'string' && workerId.trim()) {
          workerIds.add(workerId.trim());
        }
      }
      for (const item of node.executionItems || []) {
        for (const workerId of item.workerTabs || []) {
          if (typeof workerId === 'string' && workerId.trim()) {
            workerIds.add(workerId.trim());
          }
        }
      }
      // 从 dispatch_group blocks 的 lanes 提取 worker IDs
      const blocks = node.message?.blocks;
      if (Array.isArray(blocks)) {
        for (const block of blocks) {
          if (block?.type === 'dispatch_group' && Array.isArray(block.lanes)) {
            for (const lane of block.lanes) {
              const w = typeof lane?.worker === 'string' ? lane.worker.trim() : '';
              if (w) workerIds.add(w);
            }
          }
        }
      }
    }
    return Array.from(workerIds);
  });

  // 从 dispatch_group lanes 派生 Worker 运行状态（唯一真相源）
  const dispatchLaneStatusMap = $derived.by(() => {
    const map = new Map<string, string>();
    for (const node of appState.timelineNodes || []) {
      const blocks = node.message?.blocks;
      if (!Array.isArray(blocks)) continue;
      for (const block of blocks) {
        if (block?.type !== 'dispatch_group' || !Array.isArray(block.lanes)) continue;
        for (const lane of block.lanes) {
          const w = typeof lane?.worker === 'string' ? lane.worker.trim() : '';
          if (!w) continue;
          const existing = map.get(w);
          // 优先保留 running/pending 等活跃状态
          if (!existing || lane.status === 'running' || lane.status === 'pending') {
            map.set(w, lane.status || '');
          }
        }
      }
    }
    return map;
  });

  function resolveEffectiveWorkerExecuting(workerId: string): boolean {
    const laneStatus = dispatchLaneStatusMap.get(workerId);
    return laneStatus === 'running'
      || laneStatus === 'pending'
      || laneStatus === 'blocked'
      || laneStatus === 'awaiting_approval'
      || laneStatus === 'review_required';
  }

  function resolveLaneIndicatorVariant(workerId: string): 'brand' | 'warning' | 'error' | 'disabled' | null {
    const laneStatus = dispatchLaneStatusMap.get(workerId);
    switch (laneStatus) {
      case 'running':
      case 'pending':
        return 'brand';
      case 'awaiting_approval':
      case 'review_required':
        return 'warning';
      case 'blocked':
      case 'failed':
      case 'cancelled':
        return 'error';
      default:
        return null;
    }
  }

  // 底栏 tab 列表：已启用角色 + 主线 projection 中的真实参与者
  const workerTabs = $derived.by(() => collectWorkerTabIds(
    projectionWorkerIds,
    enabledAgents,
    registrySnapshot,
  ));




  function getWorkerModelStatus(workerId: string): string {
    const roleSource = resolveWorkerRoleSource(workerId, enabledAgents, registrySnapshot);
    if (roleSource?.enabled === false) {
      return 'disabled';
    }
    const lookupKey = roleSource?.modelSource === 'engine'
      ? (roleSource.engineId || workerId)
      : 'orchestrator';
    return modelStatus[lookupKey]?.status || 'not_configured';
  }

  function resolveLocalizedWorkerName(workerId: string, locale: string): string {
    void locale;
    return resolveWorkerDisplayName(workerId, enabledAgents, registrySnapshot, (key) => i18n.t(key));
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
    {#each workerTabs as workerId (workerId)}
      {@const locale = i18n.locale}
      {@const isExecuting = resolveEffectiveWorkerExecuting(workerId)}
      {@const roleSource = resolveWorkerRoleSource(workerId, enabledAgents, registrySnapshot)}
      {@const agentColorPair = getAgentColor(workerId, roleSource?.colorToken)}
      {@const workerStatus = getWorkerModelStatus(workerId)}
      {@const laneIndicatorVariant = resolveLaneIndicatorVariant(workerId)}
      {@const indicatorVariant = laneIndicatorVariant || resolveAgentIndicatorVariant(workerStatus)}


      <button
        class="bt-tab bt-worker"
        class:active={activeTab === workerId}
        class:is-executing={isExecuting}
        style="--w-color: {agentColorPair.color}"
        data-tab-id={workerId}
        onclick={() => onTabChange(workerId)}
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
        {resolveLocalizedWorkerName(workerId, locale)}
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
