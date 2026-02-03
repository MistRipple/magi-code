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

<div class="bottom-tabs">
  <button
    class="bottom-tab"
    class:active={activeTab === 'thread'}
    onclick={() => onTabChange('thread')}
  >
    对话
  </button>
  <button
    class="bottom-tab worker-tab"
    class:active={activeTab === 'claude'}
    style="--tab-worker-color: {workerColors.claude}"
    onclick={() => onTabChange('claude')}
  >
    <span class="status-indicator">
      {#if executionStatus.claude === 'executing'}
        <Icon name="loader" size={14} class="spinning" />
      {:else if executionStatus.claude === 'completed'}
        <Icon name="check-circle" size={14} class="status-success" />
      {:else if executionStatus.claude === 'failed'}
        <Icon name="x-circle" size={14} class="status-error" />
      {:else}
        <span class="dot" class:available={isModelAvailable('claude')}></span>
      {/if}
    </span>
    Claude
  </button>
  <button
    class="bottom-tab worker-tab"
    class:active={activeTab === 'codex'}
    style="--tab-worker-color: {workerColors.codex}"
    onclick={() => onTabChange('codex')}
  >
    <span class="status-indicator">
      {#if executionStatus.codex === 'executing'}
        <Icon name="loader" size={14} class="spinning" />
      {:else if executionStatus.codex === 'completed'}
        <Icon name="check-circle" size={14} class="status-success" />
      {:else if executionStatus.codex === 'failed'}
        <Icon name="x-circle" size={14} class="status-error" />
      {:else}
        <span class="dot" class:available={isModelAvailable('codex')}></span>
      {/if}
    </span>
    Codex
  </button>
  <button
    class="bottom-tab worker-tab"
    class:active={activeTab === 'gemini'}
    style="--tab-worker-color: {workerColors.gemini}"
    onclick={() => onTabChange('gemini')}
  >
    <span class="status-indicator">
      {#if executionStatus.gemini === 'executing'}
        <Icon name="loader" size={14} class="spinning" />
      {:else if executionStatus.gemini === 'completed'}
        <Icon name="check-circle" size={14} class="status-success" />
      {:else if executionStatus.gemini === 'failed'}
        <Icon name="x-circle" size={14} class="status-error" />
      {:else}
        <span class="dot" class:available={isModelAvailable('gemini')}></span>
      {/if}
    </span>
    Gemini
  </button>
</div>

<style>
  .bottom-tabs {
    display: flex;
    gap: var(--space-1);
    padding: 0 var(--space-4);
    background: var(--background);
    border-top: 1px solid var(--border);
    flex-shrink: 0;
  }

  .bottom-tab {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-top: 2px solid transparent;
    cursor: pointer;
    transition: all var(--transition-fast);
    position: relative;
  }

  .bottom-tab:hover {
    color: var(--foreground);
    background: var(--surface-1);
  }

  .bottom-tab.active {
    color: var(--primary);
    border-top-color: var(--primary);
  }

  /* Worker Tab 激活时使用对应颜色 */
  .bottom-tab.worker-tab.active {
    color: var(--tab-worker-color);
    border-top-color: var(--tab-worker-color);
  }

  .status-indicator {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    flex-shrink: 0;
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
    background: var(--foreground-muted);
    transition: background var(--transition-fast);
    flex-shrink: 0;
  }

  .dot.available {
    background: var(--success);
  }

  /* 执行中旋转动画 */
  :global(.status-indicator .spinning) {
    animation: spin 1s linear infinite;
    color: var(--tab-worker-color, var(--primary));
  }

  /* 成功状态 */
  :global(.status-indicator .status-success) {
    color: var(--success);
    animation: fadeIn 0.2s ease-out;
  }

  /* 失败状态 */
  :global(.status-indicator .status-error) {
    color: var(--error);
    animation: fadeIn 0.2s ease-out;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: scale(0.8); }
    to { opacity: 1; transform: scale(1); }
  }
</style>

