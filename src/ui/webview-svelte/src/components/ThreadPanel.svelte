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
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
  }

  .main-content {
    flex: 1;
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
    position: relative;
  }
</style>
