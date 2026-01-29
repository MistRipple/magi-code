<script lang="ts">
  import type { TabType, AgentType } from '../types/message';
  import { getState, setCurrentBottomTab } from '../stores/messages.svelte';
  import MessageList from './MessageList.svelte';
  import AgentTab from './AgentTab.svelte';
  import { ensureArray } from '../lib/utils';

  const appState = getState();

  // Tab 定义（使用 SVG 图标标识）
  const tabs: { id: TabType; label: string; color: string }[] = [
    { id: 'thread', label: '对话', color: '#888' },
    { id: 'claude', label: 'Claude', color: '#8b5cf6' },
    { id: 'codex', label: 'Codex', color: '#22c55e' },
    { id: 'gemini', label: 'Gemini', color: '#3b82f6' },
  ];

  const currentTab = $derived(appState.currentBottomTab);

  function selectTab(tabId: TabType) {
    setCurrentBottomTab(tabId);
  }
</script>

<div class="tab-container">
  <div class="tab-bar">
    {#each tabs as tab (tab.id)}
      <button
        class="tab-button"
        class:active={currentTab === tab.id}
        onclick={() => selectTab(tab.id)}
      >
        <span class="tab-dot" style="background: {tab.color}"></span>
        <span class="tab-label">{tab.label}</span>
        {#if tab.id !== 'thread'}
          {@const count = ensureArray(appState.agentOutputs?.[tab.id as AgentType]).length}
          {#if count > 0}
            <span class="tab-badge">{count}</span>
          {/if}
        {/if}
      </button>
    {/each}
  </div>

  <div class="tab-content">
    {#if currentTab === 'thread'}
      <MessageList messages={ensureArray(appState.threadMessages)} />
    {:else if currentTab === 'claude' || currentTab === 'codex' || currentTab === 'gemini'}
      <AgentTab
        messages={ensureArray(appState.agentOutputs?.[currentTab as AgentType])}
      />
    {/if}
  </div>
</div>

<style>
  .tab-container {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .tab-bar {
    display: flex;
    gap: 2px;
    padding: var(--spacing-xs) var(--spacing-sm);
    background: var(--vscode-editorGroupHeader-tabsBackground, #252526);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    overflow-x: auto;
  }

  .tab-button {
    display: flex;
    align-items: center;
    gap: var(--spacing-xs);
    padding: var(--spacing-xs) var(--spacing-md);
    font-size: var(--font-size-sm);
    color: var(--vscode-tab-inactiveForeground, #888);
    background: transparent;
    border-radius: var(--radius-sm) var(--radius-sm) 0 0;
    transition: all var(--transition-fast);
    white-space: nowrap;
  }

  .tab-button:hover {
    background: var(--vscode-tab-hoverBackground, rgba(255,255,255,0.1));
    color: var(--foreground);
  }

  .tab-button.active {
    background: var(--vscode-tab-activeBackground, var(--background));
    color: var(--vscode-tab-activeForeground, var(--foreground));
    border-bottom: 2px solid var(--vscode-tab-activeBorderTop, var(--primary));
  }

  .tab-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .tab-label {
    font-weight: 500;
  }

  .tab-badge {
    font-size: 10px;
    padding: 1px 5px;
    background: var(--vscode-badge-background, #4d4d4d);
    color: var(--vscode-badge-foreground, #fff);
    border-radius: 8px;
    min-width: 16px;
    text-align: center;
  }

  .tab-content {
    flex: 1;
    overflow: hidden;
  }
</style>
