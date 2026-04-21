<script lang="ts">
  import type {
    OrchestratorRuntimeState,
    SessionTimelineProjection,
    TimelineNode,
    TimelineRenderItem,
    WorkerLaneStatus,
  } from '../types/message';
  import {
    messagesState,
    setCurrentBottomTab,
  } from '../stores/messages.svelte';
  import {
    buildTimelineRenderItems,
  } from '../lib/timeline-render-items';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import BottomTabs from './BottomTabs.svelte';
  import AgentTab from './AgentTab.svelte';
  import RuntimeStatePanel from './RuntimeStatePanel.svelte';

  interface Props {
    isTopActive?: boolean;
  }
  let { isTopActive = true }: Props = $props();

  const ACTIVE_LANE_STATUSES = new Set<WorkerLaneStatus>([
    'pending',
    'running',
    'blocked',
    'awaiting_approval',
    'review_required',
  ]);

  function hasActiveDispatchLane(source: SessionTimelineProjection | TimelineNode[] | null | undefined): boolean {
    if (!source) {
      return false;
    }
    const messages = Array.isArray(source)
      ? source.map((node) => node?.message).filter(Boolean)
      : (Array.isArray(source.artifacts) ? source.artifacts.map((artifact) => artifact?.message).filter(Boolean) : []);
    return messages.some((message) => Array.isArray(message?.blocks) && message.blocks.some((block) => (
      block?.type === 'dispatch_group'
      && Array.isArray(block.lanes)
      && block.lanes.some((lane) => ACTIVE_LANE_STATUSES.has(lane.status as WorkerLaneStatus))
    )));
  }

  // 底部 Tab: 使用 store 中的状态，支持从其他组件跳转
  const activeBottomTab = $derived(messagesState.currentBottomTab as string);
  function handleBottomTabChange(tab: string) {
    setCurrentBottomTab(tab);
  }

  const activeWorkerTab = $derived.by(() => (
    isTopActive && activeBottomTab !== 'thread' ? activeBottomTab : null
  ));
  const timelineSource = $derived(messagesState.timelineProjection || messagesState.timelineNodes);
  const threadRenderItems = $derived.by(() => (
    isTopActive && activeBottomTab === 'thread'
      ? (buildTimelineRenderItems(
          timelineSource,
          'thread',
        ) as TimelineRenderItem[])
      : []
  ));
  const activeWorkerRenderItems = $derived.by(() => (
    activeWorkerTab
      ? (buildTimelineRenderItems(
          timelineSource,
          'worker',
          activeWorkerTab,
        ) as TimelineRenderItem[])
      : []
  ));
  const runtimeState = $derived.by<OrchestratorRuntimeState | null>(() => {
    const current = messagesState.orchestratorRuntimeState;
    if (!current) {
      return null;
    }
    if (current.status === 'completed' && hasActiveDispatchLane(timelineSource)) {
      return {
        ...current,
        status: 'running',
      };
    }
    return current;
  });
</script>

<div class="thread-panel">
  <RuntimeStatePanel {runtimeState} />
  <div class="main-content">
    {#if activeBottomTab === 'thread'}
      <MessageList renderItems={threadRenderItems} isActive={isTopActive && activeBottomTab === 'thread'} />
    {:else}
      <AgentTab workerName={activeBottomTab} renderItems={activeWorkerRenderItems} isActive={isTopActive} />
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
    display: flex;
    flex-direction: column;
  }
</style>
