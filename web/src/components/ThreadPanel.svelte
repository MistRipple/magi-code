<script lang="ts">
  import type { OrchestratorRuntimeState } from '../types/message';
  import {
    messagesState,
  } from '../stores/messages.svelte';
  import {
    buildTimelineRenderItems,
  } from '../lib/timeline-render-items';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import RuntimeStatePanel from './RuntimeStatePanel.svelte';

  interface Props {
    isTopActive?: boolean;
  }
  let { isTopActive = true }: Props = $props();

  const threadRenderItems = $derived.by(() => (
    !isTopActive
      ? []
      : messagesState.canonicalTimelineProjection
        ? buildTimelineRenderItems(
            messagesState.canonicalTimelineProjection,
            'thread',
            undefined,
            {
              workspaceId: messagesState.currentWorkspaceId,
              workspacePath: messagesState.currentWorkspacePath,
            },
          )
        : []
  ));
  const runtimeState = $derived.by<OrchestratorRuntimeState | null>(() => messagesState.orchestratorRuntimeState);
</script>

<div class="thread-panel">
  <RuntimeStatePanel
    {runtimeState}
    isProcessing={messagesState.isProcessing}
    processingStartedAt={messagesState.thinkingStartAt}
  />
  <div class="main-content">
    <MessageList renderItems={threadRenderItems} isActive={isTopActive} />
  </div>

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
