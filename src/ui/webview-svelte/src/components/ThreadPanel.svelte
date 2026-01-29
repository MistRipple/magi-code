<script lang="ts">
  import type { Message } from '../types/message';
  import { getState, setCurrentBottomTab } from '../stores/messages.svelte';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import PhaseIndicator from './PhaseIndicator.svelte';
  import BottomTabs from './BottomTabs.svelte';
  import AgentTab from './AgentTab.svelte';
  import { ensureArray } from '../lib/utils';

  const appState = getState();

  // 底部 Tab: 使用 store 中的状态，支持从其他组件跳转
  const activeBottomTab = $derived(appState.currentBottomTab as 'thread' | 'claude' | 'codex' | 'gemini');

  function handleBottomTabChange(tab: 'thread' | 'claude' | 'codex' | 'gemini') {
    setCurrentBottomTab(tab);
  }

  // 获取消息列表
  const messages = $derived(ensureArray(appState.threadMessages) as Message[]);
  const agentOutputs = $derived({
    claude: ensureArray(appState.agentOutputs?.claude) as Message[],
    codex: ensureArray(appState.agentOutputs?.codex) as Message[],
    gemini: ensureArray(appState.agentOutputs?.gemini) as Message[],
  });
</script>

<div class="thread-panel">
  <!-- 阶段进度指示器 -->
  <PhaseIndicator />

  <!-- 消息内容区域（使用 position: relative 让滚动按钮相对于消息区域定位） -->
  <div class="main-content">
    {#if activeBottomTab === 'thread'}
      <MessageList {messages} />
    {:else}
      <AgentTab messages={agentOutputs[activeBottomTab] || []} />
    {/if}
  </div>

  <!-- 底部 Agent Tab 栏 - 在输入框上方 -->
  <BottomTabs activeTab={activeBottomTab} onTabChange={handleBottomTabChange} />

  <!-- 输入区域 -->
  <InputArea />
</div>

<style>
  .thread-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .main-content {
    flex: 1;
    overflow: hidden;
    position: relative;
  }
</style>
