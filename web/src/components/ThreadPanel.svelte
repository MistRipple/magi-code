<script lang="ts">
  import type { OrchestratorRuntimeState } from '../types/message';
  import {
    getLocalTurnSubmissionRenderItems,
    messagesState,
  } from '../stores/messages.svelte';
  import {
    buildTimelineRenderItems,
  } from '../lib/timeline-render-items';
  import {
    buildConversationRuntimeRecords,
  } from '../lib/conversation-runtime-records';
  import MessageList from './MessageList.svelte';
  import ConversationApprovalTray from './ConversationApprovalTray.svelte';
  import ExternalSessionBar from './ExternalSessionBar.svelte';
  import { isExternalSessionId } from '../stores/external-sessions.svelte';
  import InputArea from './InputArea.svelte';
  import RuntimeStatePanel from './RuntimeStatePanel.svelte';
  import AgentRunBanner from './AgentRunBanner.svelte';
  import { parseToolApprovalPayload } from '../lib/tool-error-payload';
  import {
    syncToolApprovals,
    toolApprovalState,
  } from '../stores/tool-approval-store.svelte';
  import { syncUserQuestions } from '../stores/user-question-store.svelte';

  interface Props {
    isTopActive?: boolean;
  }
  let { isTopActive = true }: Props = $props();

  const threadRenderItems = $derived.by(() => {
    if (!isTopActive) return [];
    const canonicalItems = messagesState.canonicalTimelineProjection
      ? buildTimelineRenderItems(
          messagesState.canonicalTimelineProjection,
          'thread',
          undefined,
          {
            workspaceId: messagesState.currentWorkspaceId,
            workspacePath: messagesState.currentWorkspacePath,
          },
        )
      : [];
    return [
      ...canonicalItems,
      ...getLocalTurnSubmissionRenderItems(messagesState.currentSessionId),
    ];
  });
  const runtimeState = $derived.by<OrchestratorRuntimeState | null>(() => messagesState.orchestratorRuntimeState);
  const conversationRecords = $derived.by(() => buildConversationRuntimeRecords(
    threadRenderItems,
    {
      isProcessing: runtimeState?.status === 'running',
      processingStartedAt: runtimeState?.startedAt,
    },
  ));
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
    void syncUserQuestions(sessionId, scope === 'workspace'
      ? { scope, workspaceId, workspacePath }
      : { scope });
  });
</script>

<div class="thread-panel" data-desktop-drop-zone="conversation">
  <RuntimeStatePanel
    {runtimeState}
    {conversationRecords}
  />
  <div class="main-content">
    <MessageList renderItems={threadRenderItems} isActive={isTopActive} />
    <ConversationApprovalTray approvals={unanchoredApprovals} />
    <AgentRunBanner />
  </div>

  <!-- 输入区域：外部会话只读，这里换成来源提示和待确认 -->
  {#if isExternalSessionId(messagesState.currentSessionId)}
    <ExternalSessionBar />
  {:else}
    <InputArea />
  {/if}
</div>

<style>
  .thread-panel {
    position: relative;
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
