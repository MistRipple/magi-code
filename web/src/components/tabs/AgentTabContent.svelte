<script lang="ts">
  import type { TimelineRenderItem } from '../../types/message';
  import { buildTimelineRenderItems } from '../../lib/timeline-render-items';
  import { messagesState } from '../../stores/messages.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import MessageList from '../MessageList.svelte';

  interface Props {
    /** 子代理 taskId —— 用于按 metadata.taskId 过滤 projection artifacts */
    taskId: string;
  }

  let { taskId }: Props = $props();

  const renderItems = $derived.by<TimelineRenderItem[]>(() => {
    const projection = messagesState.canonicalTimelineProjection;
    if (!taskId || !projection) {
      return [];
    }
    return buildTimelineRenderItems(projection, 'task', taskId);
  });
</script>

<div class="agent-tab-content">
  <MessageList
    taskId={taskId}
    renderItems={renderItems}
    displayContext="task"
    emptyState={{
      icon: 'clock',
      title: i18n.t('agentTab.empty.title'),
      hint: i18n.t('agentTab.empty.hint'),
    }}
  />
</div>

<style>
  .agent-tab-content {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }
</style>
