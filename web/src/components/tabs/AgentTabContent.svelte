<script lang="ts">
  import type { AgentId, TimelineRenderItem } from '../../types/message';
  import { buildTimelineRenderItems } from '../../lib/timeline-render-items';
  import { messagesState } from '../../stores/messages.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import MessageList from '../MessageList.svelte';

  interface Props {
    workerTabId: string;
  }

  let { workerTabId }: Props = $props();

  const renderItems = $derived.by<TimelineRenderItem[]>(() => {
    const projection = messagesState.canonicalTimelineProjection;
    if (!workerTabId || !projection) {
      return [];
    }
    return buildTimelineRenderItems(projection, 'worker', workerTabId as AgentId);
  });

  const stageCounts = $derived.by(() => {
    const kinds = new Map<string, number>();
    for (const item of renderItems) {
      const kind = typeof item.message.metadata?.turnItemKind === 'string'
        ? item.message.metadata.turnItemKind
        : '';
      if (!kind) continue;
      kinds.set(kind, (kinds.get(kind) || 0) + 1);
    }
    return {
      toolCalls: kinds.get('tool_call') || 0,
      replies: kinds.get('assistant_text') || 0,
      thinking: kinds.get('assistant_thinking') || 0,
    };
  });

  const hasStats = $derived(
    stageCounts.toolCalls > 0 || stageCounts.replies > 0 || stageCounts.thinking > 0,
  );
</script>

<div class="agent-tab-content">
  {#if hasStats}
    <div class="agent-stats" aria-label={i18n.t('agentTab.eyebrow')}>
      {#if stageCounts.toolCalls > 0}
        <span class="agent-stats__chip">
          {i18n.t('agentTab.stats.toolCalls', { count: stageCounts.toolCalls })}
        </span>
      {/if}
      {#if stageCounts.replies > 0}
        <span class="agent-stats__chip">
          {i18n.t('agentTab.stats.replies', { count: stageCounts.replies })}
        </span>
      {/if}
      {#if stageCounts.thinking > 0}
        <span class="agent-stats__chip">
          {i18n.t('agentTab.stats.thinking', { count: stageCounts.thinking })}
        </span>
      {/if}
    </div>
  {/if}

  <div class="agent-tab-body">
    <MessageList
      workerName={workerTabId as AgentId}
      renderItems={renderItems}
      displayContext="worker"
      readOnly={true}
      emptyState={{
        icon: 'clock',
        title: i18n.t('agentTab.empty.title'),
        hint: i18n.t('agentTab.empty.hint'),
      }}
    />
  </div>
</div>

<style>
  .agent-tab-content {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }

  .agent-stats {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--surface) 60%, transparent);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    flex-shrink: 0;
  }

  .agent-stats__chip {
    display: inline-flex;
    align-items: center;
    padding: 2px var(--space-2);
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--foreground-muted) 14%, transparent);
    white-space: nowrap;
  }

  .agent-tab-body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
</style>
