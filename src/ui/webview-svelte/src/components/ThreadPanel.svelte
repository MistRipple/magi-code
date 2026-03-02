<script lang="ts">
  import type { Message } from '../types/message';
  import {
    messagesState,
    setCurrentBottomTab
  } from '../stores/messages.svelte';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import BottomTabs from './BottomTabs.svelte';
  import AgentTab from './AgentTab.svelte';
  import { ensureArray } from '../lib/utils';

  // 直接使用 messagesState 对象，确保 Svelte 5 响应式追踪正常

  // 底部 Tab: 使用 store 中的状态，支持从其他组件跳转
  const activeBottomTab = $derived(messagesState.currentBottomTab as 'thread' | 'claude' | 'codex' | 'gemini');

  function handleBottomTabChange(tab: 'thread' | 'claude' | 'codex' | 'gemini') {
    setCurrentBottomTab(tab);
  }

  // 获取消息列表 - 直接访问 messagesState 属性以正确追踪响应式
  const messages = $derived(ensureArray(messagesState.threadMessages) as Message[]);
  const agentOutputs = $derived.by(() => {
    const outputs = messagesState.agentOutputs;
    return {
      claude: ensureArray(outputs?.claude) as Message[],
      codex: ensureArray(outputs?.codex) as Message[],
      gemini: ensureArray(outputs?.gemini) as Message[],
    };
  });
</script>

<div class="thread-panel">
  <!-- 消息内容区域：四个面板同时存在，CSS 控制显隐 -->
  <!-- 每个 Worker 拥有独立的 MessageList 实例和计时器状态 -->
  <div class="main-content">
    <div class="tab-pane" class:active={activeBottomTab === 'thread'}>
      <MessageList {messages} isActive={activeBottomTab === 'thread'} />
    </div>
    <div class="tab-pane" class:active={activeBottomTab === 'claude'}>
      <AgentTab workerName="claude" messages={agentOutputs.claude} isActive={activeBottomTab === 'claude'} />
    </div>
    <div class="tab-pane" class:active={activeBottomTab === 'codex'}>
      <AgentTab workerName="codex" messages={agentOutputs.codex} isActive={activeBottomTab === 'codex'} />
    </div>
    <div class="tab-pane" class:active={activeBottomTab === 'gemini'}>
      <AgentTab workerName="gemini" messages={agentOutputs.gemini} isActive={activeBottomTab === 'gemini'} />
    </div>
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
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
  }

  .main-content {
    flex: 1;
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
    position: relative;
  }

  /* 面板默认隐藏，激活时显示 */
  .tab-pane {
    display: none;
    height: 100%;
    min-height: 0;
  }

  .tab-pane.active {
    display: flex;
    flex-direction: column;
  }
</style>
