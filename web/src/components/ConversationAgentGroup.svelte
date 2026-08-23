<script lang="ts">
  import type { FilePreviewScope } from '../lib/file-reference';
  import type { TimelineRenderItem } from '../types/message';
  import { i18n } from '../stores/i18n.svelte';
  import { getAgentRunState } from '../stores/agent-run-store.svelte';
  import { parseToolPayloadRecord } from '../lib/tool-error-payload';
  import Icon from './Icon.svelte';
  import MessageItem from './MessageItem.svelte';

  interface Props {
    items: TimelineRenderItem[];
    readOnly?: boolean;
    displayContext?: 'thread' | 'task';
    filePreviewScopeForItem: (item: TimelineRenderItem) => FilePreviewScope;
    continueInterruptedSession: () => void;
  }

  let {
    items,
    readOnly = false,
    displayContext = 'thread',
    filePreviewScopeForItem,
    continueInterruptedSession,
  }: Props = $props();

  let expanded = $state(false);

  function childTaskId(item: TimelineRenderItem): string {
    for (const block of item.message.blocks || []) {
      if (!block || typeof block !== 'object') continue;
      if (!block.toolCall?.name.toLowerCase().includes('agent_spawn')) continue;
      const payload = parseToolPayloadRecord(block.toolCall.result);
      const taskId = typeof payload?.child_task_id === 'string' ? payload.child_task_id.trim() : '';
      if (taskId) return taskId;
    }
    return '';
  }

  function toolStatus(item: TimelineRenderItem): string {
    const taskId = childTaskId(item);
    if (taskId) {
      const scope = filePreviewScopeForItem(item);
      const projection = getAgentRunState(scope.sessionId, scope.workspaceId).projection;
      const status = projection?.agents.find((agent) => agent.agentRunId === taskId)?.status
        || projection?.tasks.find((task) => task.task_id === taskId)?.status;
      if (status === 'completed') return 'success';
      if (status === 'failed' || status === 'killed') return 'error';
      if (status === 'running' || status === 'pending') return status;
    }
    for (const block of item.message.blocks || []) {
      if (!block || typeof block !== 'object') continue;
      if (block.toolCall?.name && block.toolCall.name.toLowerCase().includes('agent_spawn')) {
        return block.toolCall.status || 'pending';
      }
    }
    return 'pending';
  }

  const statusCounts = $derived.by(() => {
    let running = 0;
    let completed = 0;
    let failed = 0;
    for (const item of items) {
      const status = toolStatus(item);
      if (status === 'success') completed += 1;
      else if (status === 'error') failed += 1;
      else running += 1;
    }
    return { running, completed, failed };
  });
  const contentId = $derived(`conversation-agent-group-${(items[0]?.key || 'empty').replace(/[^a-zA-Z0-9_-]/gu, '-')}`);
  const summary = $derived([
    statusCounts.completed > 0 ? i18n.t('messageList.agentGroup.completed', { count: statusCounts.completed }) : '',
    statusCounts.running > 0 ? i18n.t('messageList.agentGroup.running', { count: statusCounts.running }) : '',
    statusCounts.failed > 0 ? i18n.t('messageList.agentGroup.failed', { count: statusCounts.failed }) : '',
  ].filter(Boolean).join(' · '));
</script>

<section class="conversation-agent-group" class:expanded>
  <button
    type="button"
    class="agent-group-header"
    aria-expanded={expanded}
    aria-controls={contentId}
    onclick={() => { expanded = !expanded; }}
  >
    <span class="agent-group-icon"><Icon name="bot" size={14} /></span>
    <span class="agent-group-label">{i18n.t('messageList.agentGroup.label', { count: items.length })}</span>
    {#if summary}<span class="agent-group-summary">{summary}</span>{/if}
    <span class="agent-group-chevron" class:rotated={expanded}><Icon name="chevron-right" size={12} /></span>
  </button>

  {#if expanded}
    <div class="agent-group-list" id={contentId}>
      {#each items as item (item.key)}
        <MessageItem
          message={item.message}
          {readOnly}
          {displayContext}
          filePreviewScope={filePreviewScopeForItem(item)}
          onContinueInterrupted={continueInterruptedSession}
          hideResponseDuration
          presentationRole="delegation"
        />
      {/each}
    </div>
  {/if}
</section>

<style>
  .conversation-agent-group {
    min-width: 0;
    border-bottom: 1px solid var(--border);
  }

  .agent-group-header {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 42px;
    gap: 8px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    text-align: left;
    cursor: pointer;
  }

  .agent-group-header:hover,
  .agent-group-header:focus-visible {
    color: var(--foreground);
  }

  .agent-group-header:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 3px;
  }

  .agent-group-icon,
  .agent-group-chevron {
    display: inline-flex;
    flex: 0 0 auto;
  }

  .agent-group-icon {
    color: currentColor;
  }

  .agent-group-label {
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .agent-group-summary {
    min-width: 0;
    overflow: hidden;
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .agent-group-chevron {
    margin-left: auto;
    transition: transform var(--transition-fast);
  }

  .agent-group-chevron.rotated {
    transform: rotate(90deg);
  }

  .agent-group-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 2px 0 var(--space-3);
  }

  .agent-group-list :global(.message-item.assistant) {
    padding-inline: 0;
  }
</style>
