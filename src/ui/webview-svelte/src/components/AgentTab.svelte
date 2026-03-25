<script lang="ts">
  import type { TimelineRenderItem } from '../types/message';
  import MessageList from './MessageList.svelte';
  import { i18n } from '../stores/i18n.svelte';

  // Props
  interface Props {
    workerName?: 'claude' | 'codex' | 'gemini';
    renderItems: TimelineRenderItem[];
    isActive?: boolean;
  }

  let { workerName, renderItems, isActive = false }: Props = $props();

  // Worker Tab 专用的空状态配置
  const emptyState = $derived({
    icon: 'message-square',
    title: i18n.t('agentTab.empty.title'),
    hint: i18n.t('agentTab.empty.hint')
  });
</script>

<div class="agent-tab">
  <div class="agent-message-list">
    <!-- 复用 MessageList 组件，displayContext='worker' 标识 Worker 面板 -->
    <!-- Worker 面板中的生命周期卡片与执行流统一按语义时间轴渲染 -->
    <MessageList workerName={workerName} {renderItems} {emptyState} displayContext="worker" {isActive} />
  </div>
</div>

<style>
  .agent-tab {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }

  .agent-message-list {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
</style>
