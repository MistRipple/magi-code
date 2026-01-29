<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import PhaseIndicator from './PhaseIndicator.svelte';
  import BottomTabs from './BottomTabs.svelte';
  import AgentTab from './AgentTab.svelte';

  const appState = getState();

  // 底部 Tab: thread/claude/codex/gemini
  let activeBottomTab = $state<'thread' | 'claude' | 'codex' | 'gemini'>('thread');

  function handleBottomTabChange(tab: 'thread' | 'claude' | 'codex' | 'gemini') {
    activeBottomTab = tab;
  }

  // 获取消息列表
  const messages = $derived(appState.threadMessages || []);
  const agentOutputs = $derived(appState.agentOutputs || { claude: [], codex: [], gemini: [] });
</script>

<div class="thread-panel">
  <!-- 阶段进度指示器 -->
  <PhaseIndicator />

  <!-- 消息内容区域 -->
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
    overflow-y: auto;
    overflow-x: hidden;
  }
</style>

