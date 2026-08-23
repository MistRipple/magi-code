<script lang="ts">
  import type { OrchestratorRuntimeState } from '../types/message';
  import {
    messagesState,
  } from '../stores/messages.svelte';
  import {
    buildTimelineRenderItems,
  } from '../lib/timeline-render-items';
  import {
    buildConversationRuntimeRecords,
    resolveCurrentConversationTurnStartedAt,
  } from '../lib/conversation-runtime-records';
  import MessageList from './MessageList.svelte';
  import ConversationApprovalTray from './ConversationApprovalTray.svelte';
  import InputArea from './InputArea.svelte';
  import RuntimeStatePanel from './RuntimeStatePanel.svelte';
  import GoalRunDrawers from './GoalRunDrawers.svelte';
  import ActiveAgentCenter from './ActiveAgentCenter.svelte';
  import { parseToolApprovalPayload } from '../lib/tool-error-payload';
  import {
    syncToolApprovals,
    toolApprovalState,
  } from '../stores/tool-approval-store.svelte';

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
  const conversationRecords = $derived.by(() => buildConversationRuntimeRecords(
    threadRenderItems,
    {
      isProcessing: messagesState.isProcessing,
      processingStartedAt: messagesState.thinkingStartAt,
    },
  ));
  const conversationStartedAt = $derived(
    resolveCurrentConversationTurnStartedAt(threadRenderItems),
  );
  const anchoredApprovalIds = $derived.by(() => {
    const ids = new Set<string>();
    for (const item of threadRenderItems) {
      for (const block of item.message.blocks || []) {
        if (!block || typeof block !== 'object') continue;
        const toolCall = block.toolCall;
        if (!toolCall) continue;
        const approval = parseToolApprovalPayload(toolCall.result)
          || parseToolApprovalPayload(toolCall.error)
          || parseToolApprovalPayload(toolCall.standardized?.message)
          || parseToolApprovalPayload(block.content);
        if (approval) ids.add(approval.approvalId);
      }
    }
    return ids;
  });
  const unanchoredApprovals = $derived.by(() => (
    toolApprovalState.pending.filter((approval) => !anchoredApprovalIds.has(approval.approvalId))
  ));
  let lastApprovalSyncKey = '';
  $effect(() => {
    if (!messagesState.bootstrapped) return;
    const sessionId = messagesState.currentSessionId?.trim() || '';
    const workspaceId = messagesState.currentWorkspaceId?.trim() || '';
    const workspacePath = messagesState.currentWorkspacePath?.trim() || '';
    const scope = workspaceId || workspacePath ? 'workspace' : 'personal';
    const approvalIds = [...anchoredApprovalIds].sort().join('|');
    const syncKey = `${scope}:${workspaceId}:${workspacePath}:${sessionId}:${approvalIds}`;
    if (syncKey === lastApprovalSyncKey) return;
    lastApprovalSyncKey = syncKey;
    void syncToolApprovals(sessionId, scope === 'workspace'
      ? { scope, workspaceId, workspacePath }
      : { scope });
  });
</script>

<div class="thread-panel" data-desktop-drop-zone="conversation">
  <RuntimeStatePanel
    {runtimeState}
    {conversationRecords}
    {conversationStartedAt}
    isProcessing={messagesState.isProcessing}
    processingStartedAt={messagesState.thinkingStartAt}
  />
  <div class="main-content">
    <MessageList renderItems={threadRenderItems} isActive={isTopActive} />
    <ConversationApprovalTray approvals={unanchoredApprovals} />
    <ActiveAgentCenter />
  </div>

  <GoalRunDrawers />

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
    position: relative;
    flex: 1;
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
