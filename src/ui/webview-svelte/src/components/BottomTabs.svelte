<script lang="ts">
  import type { AgentType, MissionPlan, Task } from '../types/message';
  import { getState, messagesState } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { deriveWorkerPanelState } from '../lib/worker-panel-state';
  import { ensureArray } from '../lib/utils';

  interface Props {
    activeTab: 'thread' | 'claude' | 'codex' | 'gemini';
    onTabChange: (tab: 'thread' | 'claude' | 'codex' | 'gemini') => void;
  }

  let { activeTab, onTabChange }: Props = $props();

  const appState = getState();

  // 模型连接状态（使用全局统一的 modelStatus）
  const modelStatus = $derived(appState.modelStatus);
  const tasks = $derived(ensureArray(appState.tasks) as Task[]);
  const missionPlans = $derived.by(() => Array.from(appState.missionPlan.values()) as MissionPlan[]);

  // 判断模型是否可用（available 或 connected 都表示可用）
  function isModelAvailable(worker: string): boolean {
    const status = modelStatus[worker]?.status;
    return status === 'available' || status === 'connected';
  }

  const pendingRequestIds = $derived.by(() => Array.from(messagesState.pendingRequests));
  const workerActivityState = $derived.by(() => {
    const pendingIds = pendingRequestIds;
    return {
      claude: deriveWorkerPanelState({
        messages: messagesState.agentOutputs.claude,
        workerName: 'claude',
        pendingRequestIds: pendingIds,
        tasks,
        missionPlans,
      }),
      codex: deriveWorkerPanelState({
        messages: messagesState.agentOutputs.codex,
        workerName: 'codex',
        pendingRequestIds: pendingIds,
        tasks,
        missionPlans,
      }),
      gemini: deriveWorkerPanelState({
        messages: messagesState.agentOutputs.gemini,
        workerName: 'gemini',
        pendingRequestIds: pendingIds,
        tasks,
        missionPlans,
      }),
    };
  });

  // Worker 颜色映射
  const workerColors: Record<string, string> = {
    claude: 'var(--color-claude)',
    codex: 'var(--color-codex)',
    gemini: 'var(--color-gemini)',
  };

  function isExecuting(worker: AgentType): boolean {
    return workerActivityState[worker].workerHasCurrentRequestActivity;
  }
</script>

<div class="bt-bar">
  <button
    class="bt-tab"
    class:active={activeTab === 'thread'}
    onclick={() => onTabChange('thread')}
  >
    <Icon name="chat" size={12} />
    {i18n.t('bottomTabs.thread')}
  </button>
  <button
    class="bt-tab bt-worker"
    class:active={activeTab === 'claude'}
    class:is-executing={isExecuting('claude')}
    style="--w-color: {workerColors.claude}"
    onclick={() => onTabChange('claude')}
  >
    <span class="bt-dot-wrap">
      {#if isExecuting('claude')}
        <span class="bt-dot on executing"></span>
      {:else}
        <span class="bt-dot" class:on={isModelAvailable('claude')}></span>
      {/if}
    </span>
    Claude
  </button>
  <button
    class="bt-tab bt-worker"
    class:active={activeTab === 'codex'}
    class:is-executing={isExecuting('codex')}
    style="--w-color: {workerColors.codex}"
    onclick={() => onTabChange('codex')}
  >
    <span class="bt-dot-wrap">
      {#if isExecuting('codex')}
        <span class="bt-dot on executing"></span>
      {:else}
        <span class="bt-dot" class:on={isModelAvailable('codex')}></span>
      {/if}
    </span>
    Codex
  </button>
  <button
    class="bt-tab bt-worker"
    class:active={activeTab === 'gemini'}
    class:is-executing={isExecuting('gemini')}
    style="--w-color: {workerColors.gemini}"
    onclick={() => onTabChange('gemini')}
  >
    <span class="bt-dot-wrap">
      {#if isExecuting('gemini')}
        <span class="bt-dot on executing"></span>
      {:else}
        <span class="bt-dot" class:on={isModelAvailable('gemini')}></span>
      {/if}
    </span>
    Gemini
  </button>
</div>

<style>
  /* ============================================
     BottomTabs - Agent 切换栏
     设计参考: Cursor 底部 worker 状态栏
     ============================================ */
  .bt-bar {
    display: flex;
    padding: 0 var(--space-3);
    background: var(--background);
    border-top: 1px solid var(--border);
    flex-shrink: 0;
  }

  .bt-tab {
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
    transition: color var(--transition-fast);
    white-space: nowrap;
  }

  .bt-tab:hover {
    color: var(--foreground);
  }

  .bt-tab.active {
    color: var(--foreground);
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

  .bt-dot.on {
    background: var(--success);
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

