<script lang="ts">
  import type { Message, Task } from '../types/message';
  import MessageList from './MessageList.svelte';
  import InstructionCard from './InstructionCard.svelte';
  import { getState, messagesState } from '../stores/messages.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { deriveWorkerPanelState } from '../lib/worker-panel-state';
  import { ensureArray } from '../lib/utils';

  // Props
  interface Props {
    workerName?: 'claude' | 'codex' | 'gemini';
    messages: Message[];
    isActive?: boolean;
  }

  let { workerName, messages, isActive = false }: Props = $props();
  const appState = getState();
  const workerRuntimeMap = $derived(appState.workerRuntime);
  const workerRuntime = $derived.by(() => (workerName ? workerRuntimeMap[workerName] : null));

  // Worker Tab 专用的空状态配置
  const emptyState = $derived({
    icon: 'message-square',
    title: i18n.t('agentTab.empty.title'),
    hint: i18n.t('agentTab.empty.hint')
  });

  const pendingRequestIds = $derived.by(() => Array.from(messagesState.pendingRequests));
  const tasks = $derived(ensureArray(appState.tasks) as Task[]);
  const workerPanelState = $derived.by(() => deriveWorkerPanelState({
    messages,
    workerName,
    pendingRequestIds,
    tasks,
  }));

  const activeInstructionMessage = $derived.by(() => {
    if (!workerRuntime?.isExecuting) {
      return null;
    }
    return workerPanelState.latestRunningInstructionMessage || workerPanelState.latestInstructionMessage;
  });
</script>

<div class="agent-tab">
  {#if activeInstructionMessage}
    <div class="active-task-card-shell">
      <InstructionCard
        content={activeInstructionMessage.content}
        targetWorker={(activeInstructionMessage.metadata?.worker || workerName) as string | undefined}
        isStreaming={activeInstructionMessage.isStreaming}
        metadata={activeInstructionMessage.metadata as Record<string, unknown> | undefined}
      />
    </div>
  {/if}

  <div class="agent-message-list">
    <!-- 复用 MessageList 组件，displayContext='worker' 标识 Worker 面板 -->
    <MessageList {workerName} {messages} {emptyState} displayContext="worker" {isActive} />
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

  .active-task-card-shell {
    flex-shrink: 0;
    padding: var(--space-3) var(--space-4) var(--space-2);
    background: var(--background);
    border-bottom: 1px solid var(--border);
    box-shadow: 0 8px 20px rgba(15, 23, 42, 0.06);
    position: relative;
    z-index: 1;
  }

  .active-task-card-shell :global(.instruction-card) {
    margin-right: var(--space-2);
  }

  .agent-message-list {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
</style>
