<script lang="ts">
  import { getState } from '../stores/messages.svelte';

  interface Props {
    activeTab: 'thread' | 'claude' | 'codex' | 'gemini';
    onTabChange: (tab: 'thread' | 'claude' | 'codex' | 'gemini') => void;
  }

  let { activeTab, onTabChange }: Props = $props();

  const appState = getState();

  // 模型连接状态
  const modelStatus = $derived(appState.modelStatus || {
    claude: 'unavailable',
    codex: 'unavailable',
    gemini: 'unavailable'
  });
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
    class="bottom-tab"
    class:active={activeTab === 'claude'}
    onclick={() => onTabChange('claude')}
  >
    <span class="dot" class:available={modelStatus.claude === 'connected'}></span>
    Claude
  </button>
  <button
    class="bottom-tab"
    class:active={activeTab === 'codex'}
    onclick={() => onTabChange('codex')}
  >
    <span class="dot" class:available={modelStatus.codex === 'connected'}></span>
    Codex
  </button>
  <button
    class="bottom-tab"
    class:active={activeTab === 'gemini'}
    onclick={() => onTabChange('gemini')}
  >
    <span class="dot" class:available={modelStatus.gemini === 'connected'}></span>
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
</style>

