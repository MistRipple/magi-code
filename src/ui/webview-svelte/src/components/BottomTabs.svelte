<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    activeTab: 'thread' | 'claude' | 'codex' | 'gemini';
    onTabChange: (tab: 'thread' | 'claude' | 'codex' | 'gemini') => void;
  }

  let { activeTab, onTabChange }: Props = $props();

  const appState = getState();

  // 模型连接状态（使用全局统一的 modelStatus）
  const modelStatus = $derived(appState.modelStatus);

  // 判断模型是否可用（available 或 connected 都表示可用）
  function isModelAvailable(worker: string): boolean {
    const status = modelStatus[worker]?.status;
    return status === 'available' || status === 'connected';
  }

  // Worker 执行状态
  const executionStatus = $derived(appState.workerExecutionStatus || {
    claude: 'idle',
    codex: 'idle',
    gemini: 'idle'
  });

  // Worker 颜色映射
  const workerColors: Record<string, string> = {
    claude: 'var(--color-claude)',
    codex: 'var(--color-codex)',
    gemini: 'var(--color-gemini)',
  };
</script>

<div class="bt-bar">
  <button
    class="bt-tab"
    class:active={activeTab === 'thread'}
    onclick={() => onTabChange('thread')}
  >
    <Icon name="chat" size={12} />
    主线
  </button>
  <button
    class="bt-tab bt-worker"
    class:active={activeTab === 'claude'}
    style="--w-color: {workerColors.claude}"
    onclick={() => onTabChange('claude')}
  >
    <span class="bt-dot-wrap">
      {#if executionStatus.claude === 'executing'}
        <Icon name="loader" size={12} class="spinning" />
      {:else if executionStatus.claude === 'completed'}
        <Icon name="check-circle" size={12} class="bt-ok" />
      {:else if executionStatus.claude === 'failed'}
        <Icon name="x-circle" size={12} class="bt-err" />
      {:else}
        <span class="bt-dot" class:on={isModelAvailable('claude')}></span>
      {/if}
    </span>
    Claude
  </button>
  <button
    class="bt-tab bt-worker"
    class:active={activeTab === 'codex'}
    style="--w-color: {workerColors.codex}"
    onclick={() => onTabChange('codex')}
  >
    <span class="bt-dot-wrap">
      {#if executionStatus.codex === 'executing'}
        <Icon name="loader" size={12} class="spinning" />
      {:else if executionStatus.codex === 'completed'}
        <Icon name="check-circle" size={12} class="bt-ok" />
      {:else if executionStatus.codex === 'failed'}
        <Icon name="x-circle" size={12} class="bt-err" />
      {:else}
        <span class="bt-dot" class:on={isModelAvailable('codex')}></span>
      {/if}
    </span>
    Codex
  </button>
  <button
    class="bt-tab bt-worker"
    class:active={activeTab === 'gemini'}
    style="--w-color: {workerColors.gemini}"
    onclick={() => onTabChange('gemini')}
  >
    <span class="bt-dot-wrap">
      {#if executionStatus.gemini === 'executing'}
        <Icon name="loader" size={12} class="spinning" />
      {:else if executionStatus.gemini === 'completed'}
        <Icon name="check-circle" size={12} class="bt-ok" />
      {:else if executionStatus.gemini === 'failed'}
        <Icon name="x-circle" size={12} class="bt-err" />
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
    border-top: 2px solid transparent;
    cursor: pointer;
    transition: color var(--transition-fast), border-color var(--transition-fast);
    white-space: nowrap;
  }

  .bt-tab:hover {
    color: var(--foreground);
  }

  .bt-tab.active {
    color: var(--foreground);
    border-top-color: var(--primary);
  }

  /* Worker Tab 激活时使用品牌色 */
  .bt-worker.active {
    color: var(--w-color);
    border-top-color: var(--w-color);
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

  /* 执行状态动画 */
  :global(.bt-dot-wrap .spinning) {
    animation: bt-spin 1s linear infinite;
    color: var(--w-color, var(--primary));
  }

  :global(.bt-ok) {
    color: var(--success);
  }

  :global(.bt-err) {
    color: var(--error);
  }

  @keyframes bt-spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
</style>

