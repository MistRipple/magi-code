<script lang="ts">
  import type { Message, TimelineRenderItem } from '../../types/message';
  import type {
    AgentContextPackageDto,
    AgentContextReferenceKind,
    AgentProjectionDto,
    TaskDto,
  } from '../../shared/rust-backend-types';
  import { agentTerminalOutput, type AgentTerminalOutput } from '../../lib/agent-output';
  import { buildTimelineRenderItems } from '../../lib/timeline-render-items';
  import { getTaskDisplayGoal, getTaskDisplayText, getTaskDisplayTitle } from '../../lib/task-labels';
  import { agentRuntimeTiming } from '../../lib/active-agent-center';
  import { messagesState } from '../../stores/messages.svelte';
  import { getAgentRunState } from '../../stores/agent-run-store.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import MessageList from '../MessageList.svelte';

  interface Props {
    /** 代理运行 ID —— 与后端 AgentRun 对齐，用于过滤该代理运行的 transcript artifacts */
    agentRunId: string;
    workspaceId?: string | null;
    workspacePath?: string | null;
    sessionId?: string | null;
  }

  let {
    agentRunId,
    workspaceId = null,
    workspacePath = null,
    sessionId = null,
  }: Props = $props();
  let contextExpanded = $state(false);

  interface AgentAssignmentSummary {
    title: string;
    goal: string;
  }

  function normalizeString(value: unknown): string {
    return typeof value === 'string' ? value.trim() : '';
  }

  const scopedSessionId = $derived(normalizeString(sessionId));
  const scopedWorkspaceId = $derived(normalizeString(workspaceId));
  const scopedWorkspacePath = $derived(normalizeString(workspacePath));
  const scopeMatchesCurrentSession = $derived.by(() => {
    if (!scopedSessionId) {
      return false;
    }
    if (normalizeString(messagesState.currentSessionId) !== scopedSessionId) {
      return false;
    }
    return !scopedWorkspaceId || normalizeString(messagesState.currentWorkspaceId) === scopedWorkspaceId;
  });
  const scopedTimelineProjection = $derived.by(() => {
    if (!scopeMatchesCurrentSession) {
      return null;
    }
    const projection = messagesState.canonicalTimelineProjection;
    if (!projection || normalizeString(projection.sessionId) !== scopedSessionId) {
      return null;
    }
    return projection;
  });

  const agentRunState = $derived(getAgentRunState(
    scopedSessionId,
    scopedWorkspaceId,
  ));

  const projectionTask = $derived.by<TaskDto | null>(() => {
    const projection = agentRunState.projection;
    if (!agentRunId || !projection) {
      return null;
    }
    if (projection.root_task.task_id === agentRunId) {
      return projection.root_task;
    }
    return projection.tasks.find((task) => task.task_id === agentRunId) ?? null;
  });

  const agentProjection = $derived.by<AgentProjectionDto | null>(() => {
    const projection = agentRunState.projection;
    if (!agentRunId || !projection?.agents) {
      return null;
    }
    return projection.agents.find((agent) => agent.agentRunId === agentRunId) ?? null;
  });

  const assignment = $derived.by<AgentAssignmentSummary | null>(() => {
    const agent = agentProjection;
    if (agent) {
      return {
        title: getTaskDisplayText(agent.displayName) || '代理任务',
        goal: getTaskDisplayText(agent.goal),
      };
    }
    const task = projectionTask;
    if (task) {
      return {
        title: getTaskDisplayTitle(task) || '代理任务',
        goal: getTaskDisplayGoal(task) || '',
      };
    }
    return null;
  });

  const agentRuntimeActive = $derived.by(() => {
    const lifecycle = normalizeString(agentProjection?.lifecycle).toLowerCase();
    if (lifecycle) {
      return lifecycle === 'queued' || lifecycle === 'running';
    }
    const status = normalizeString(projectionTask?.status).toLowerCase();
    return status === 'pending' || status === 'running';
  });

  const agentRuntimeStartedAt = $derived.by(() => {
    const startedAt = agentProjection?.startedAt;
    if (typeof startedAt === 'number' && Number.isFinite(startedAt) && startedAt > 0) {
      return startedAt;
    }
    const createdAt = projectionTask?.created_at;
    return typeof createdAt === 'number' && Number.isFinite(createdAt) && createdAt > 0
      ? createdAt
      : assignmentTimestamp();
  });

  const agentRuntimeTimingState = $derived.by(() => (
    agentProjection ? agentRuntimeTiming(agentProjection, Date.now()) : null
  ));
  const agentRuntimeCompletedAt = $derived.by(() => {
    const timing = agentRuntimeTimingState;
    return timing && !timing.active ? timing.completedAt : 0;
  });
  const agentRuntimeDurationMs = $derived.by(() => {
    const timing = agentRuntimeTimingState;
    return timing && !timing.active ? timing.durationMs : 0;
  });

  const contextRuntime = $derived.by(() => {
    const payload = projectionTask?.runtime_payload;
    return payload?.kind === 'agent_context' ? payload : null;
  });
  const contextPackage = $derived.by<AgentContextPackageDto | null>(() => (
    contextRuntime?.package ?? null
  ));
  const contextAccesses = $derived(contextRuntime?.accesses ?? []);
  const contextReadCount = $derived(
    contextAccesses.filter((access) => access.operation === 'read').length,
  );
  const contextAccessTokens = $derived(
    contextAccesses.reduce((total, access) => total + access.estimatedTokens, 0),
  );

  function contextKindLabel(kind: AgentContextReferenceKind): string {
    const labels: Record<AgentContextReferenceKind, string> = {
      conversation_turn: i18n.locale === 'zh-CN' ? '对话' : 'Turn',
      task_output: i18n.locale === 'zh-CN' ? '任务输出' : 'Task output',
      task_evidence: i18n.locale === 'zh-CN' ? '任务证据' : 'Task evidence',
      file: i18n.locale === 'zh-CN' ? '文件' : 'File',
      knowledge: i18n.locale === 'zh-CN' ? '知识' : 'Knowledge',
      other: i18n.locale === 'zh-CN' ? '其他' : 'Other',
    };
    return labels[kind];
  }

  function formatTokens(tokens: number): string {
    return Number.isFinite(tokens) ? Math.max(0, Math.round(tokens)).toLocaleString() : '0';
  }

  function assignmentMessageContent(summary: AgentAssignmentSummary): string {
    const title = normalizeString(summary.title);
    const goal = normalizeString(summary.goal);
    if (title && goal && title !== goal) {
      return `**${title}**\n\n${goal}`;
    }
    return goal || title;
  }

  function assignmentTimestamp(): number {
    const task = projectionTask;
    if (typeof task?.created_at === 'number' && task.created_at > 0) {
      return task.created_at;
    }
    const projection = scopedTimelineProjection;
    if (!agentRunId || !projection) {
      return Date.now();
    }
    for (const artifact of projection.artifacts || []) {
      if (artifact.message?.metadata?.taskId === agentRunId && artifact.timestamp > 0) {
        return artifact.timestamp;
      }
    }
    return Date.now();
  }

  function terminalStatus(status: unknown): boolean {
    const normalized = normalizeString(status).toLowerCase();
    return normalized === 'completed' || normalized === 'failed' || normalized === 'killed';
  }

  function hasSameAssistantContent(items: TimelineRenderItem[], content: string): boolean {
    const normalized = normalizeString(content);
    if (!normalized) {
      return false;
    }
    return items.some((item) => (
      item.message.role === 'assistant'
      && normalizeString(item.message.content) === normalized
    ));
  }

  const assignmentRenderItem = $derived.by<TimelineRenderItem | null>(() => {
    const summary = assignment;
    const content = summary ? assignmentMessageContent(summary) : '';
    if (!agentRunId || !summary || !content) {
      return null;
    }
    const message: Message = {
      id: `agent-assignment:${agentRunId}`,
      role: 'user',
      source: 'user',
      content,
      timestamp: assignmentTimestamp(),
      isStreaming: false,
      isComplete: true,
      type: 'user_input',
      metadata: {
        taskId: agentRunId,
        agentAssignment: true,
      },
    };
    return {
      key: `assignment:${agentRunId}`,
      message,
      workspaceId: scopedWorkspaceId || undefined,
      workspacePath: scopedWorkspacePath || undefined,
      sessionId: scopedSessionId || undefined,
    };
  });

  const renderItems = $derived.by<TimelineRenderItem[]>(() => {
    const projection = scopedTimelineProjection;
    const task = projectionTask;
    const terminalOutput = agentProjectionTerminalOutput(agentProjection) || agentTerminalOutput(task);
    if (!agentRunId || !projection) {
      const baseItems = assignmentRenderItem ? [assignmentRenderItem] : [];
      if (!task || !terminalStatus(task.status) || !terminalOutput || hasSameAssistantContent(baseItems, terminalOutput.text)) {
        return baseItems;
      }
      return [
        ...baseItems,
        terminalOutputRenderItem(task, terminalOutput.text, terminalOutput.sourceRefIndex),
      ];
    }
    const items = buildTimelineRenderItems(projection, 'task', agentRunId, {
      workspaceId: scopedWorkspaceId,
      workspacePath: scopedWorkspacePath,
      sessionId: scopedSessionId,
    });
    const baseItems = assignmentRenderItem ? [assignmentRenderItem, ...items] : items;
    if (!task || !terminalStatus(task.status) || !terminalOutput || hasSameAssistantContent(baseItems, terminalOutput.text)) {
      return baseItems;
    }
    return [
      ...baseItems,
      terminalOutputRenderItem(task, terminalOutput.text, terminalOutput.sourceRefIndex),
    ];
  });

  function terminalOutputRenderItem(task: TaskDto, content: string, sourceRefIndex: number): TimelineRenderItem {
    const message: Message = {
      id: `agent-terminal-output:${agentRunId}:${task.updated_at}:${sourceRefIndex}`,
      role: 'assistant',
      source: normalizeString(task.executor_binding?.target_role) || 'agent',
      content,
      timestamp: typeof task.updated_at === 'number' && task.updated_at > 0 ? task.updated_at : Date.now(),
      isStreaming: false,
      isComplete: true,
      type: 'result',
      metadata: {
        taskId: agentRunId,
        roleId: normalizeString(task.executor_binding?.target_role) || undefined,
        turnItemKind: 'task_output_ref',
        canonical: false,
      },
    };
    return {
      key: `agent-terminal-output:${agentRunId}:${task.updated_at}:${sourceRefIndex}`,
      message,
      workspaceId: scopedWorkspaceId || undefined,
      workspacePath: scopedWorkspacePath || undefined,
      sessionId: scopedSessionId || undefined,
    };
  }

  function agentProjectionTerminalOutput(agent: AgentProjectionDto | null): AgentTerminalOutput | null {
    const text = normalizeString(agent?.result?.finalText);
    if (!agent?.result || !text) {
      return null;
    }
    return {
      text,
      sourceRefIndex: Math.max(0, agent.result.outputRefCount - 1),
      truncated: Boolean(agent.result.truncated),
    };
  }

</script>

<div class="agent-tab-content">
  {#if contextPackage}
    <section class="context-observer" class:expanded={contextExpanded}>
      <button
        type="button"
        class="context-summary"
        aria-expanded={contextExpanded}
        onclick={() => contextExpanded = !contextExpanded}
      >
        <span class="context-summary-main">
          <span class="context-title">{i18n.t('agentTab.context.title')}</span>
          <span class="context-revision">r{contextPackage.revision}</span>
        </span>
        <span class="context-metrics">
          <span>{i18n.t('agentTab.context.package')} · 1</span>
          <span>{i18n.t('agentTab.context.references')} · {contextPackage.references.length}</span>
          <span>{i18n.t('agentTab.context.reads')} · {contextReadCount}</span>
          <span>{i18n.t('agentTab.context.tokens')} · {formatTokens(contextAccessTokens)}</span>
          <span class="context-chevron" aria-hidden="true">{contextExpanded ? '⌃' : '⌄'}</span>
        </span>
      </button>

      {#if contextExpanded}
        <div class="context-details">
          <div class="context-copy">
            <p>{contextPackage.summary}</p>
            <div class="context-detail-block">
              <strong>{i18n.t('agentTab.context.expectedOutput')}</strong>
              <span>{contextPackage.expectedOutput}</span>
            </div>
            {#if contextPackage.constraints.length > 0}
              <div class="context-detail-block">
                <strong>{i18n.t('agentTab.context.constraints')}</strong>
                <ul>
                  {#each contextPackage.constraints as constraint}
                    <li>{constraint}</li>
                  {/each}
                </ul>
              </div>
            {/if}
          </div>

          <div class="context-list-block">
            <strong>{i18n.t('agentTab.context.sources')}</strong>
            <div class="context-source-list">
              {#each contextPackage.references as reference (reference.referenceId)}
                <div class="context-source-row">
                  <span class="context-kind">{contextKindLabel(reference.kind)}</span>
                  <span class="context-source-title">{reference.title}</span>
                  <span class="context-source-tokens">~{formatTokens(reference.estimatedTokens)}</span>
                </div>
              {/each}
            </div>
          </div>

          <div class="context-list-block">
            <strong>{i18n.t('agentTab.context.accesses')}</strong>
            {#if contextAccesses.length === 0}
              <span class="context-empty">{i18n.t('agentTab.context.noAccesses')}</span>
            {:else}
              <div class="context-source-list">
                {#each contextAccesses as access (access.recordId)}
                  <div class="context-source-row">
                    <span class="context-kind">{access.operation}</span>
                    <span class="context-source-title">{access.query || access.referenceIds.join(', ')}</span>
                    <span class="context-source-tokens">~{formatTokens(access.estimatedTokens)}</span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        </div>
      {/if}
    </section>
  {/if}
  <MessageList
    taskId={agentRunId}
    renderItems={renderItems}
    displayContext="task"
    runtimeActive={agentRuntimeActive}
    runtimeStartedAt={agentRuntimeStartedAt}
    runtimeCompletedAt={agentRuntimeCompletedAt}
    runtimeDurationMs={agentRuntimeDurationMs}
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

  .context-observer {
    flex: 0 0 auto;
    margin: 8px 12px 0;
    border: 1px solid color-mix(in srgb, var(--border) 82%, transparent);
    border-radius: 10px;
    background: color-mix(in srgb, var(--surface-2) 94%, transparent);
    overflow: hidden;
  }

  .context-summary {
    width: 100%;
    min-height: 38px;
    padding: 7px 10px;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    cursor: pointer;
    text-align: left;
  }

  .context-summary:hover {
    background: color-mix(in srgb, var(--primary) 7%, transparent);
  }

  .context-summary-main,
  .context-metrics {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .context-title {
    color: var(--foreground);
    font-size: 12px;
    font-weight: 600;
  }

  .context-revision,
  .context-kind {
    border-radius: 999px;
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: var(--primary);
    font-size: 10px;
    line-height: 18px;
    padding: 0 6px;
    white-space: nowrap;
  }

  .context-metrics {
    font-size: 11px;
    white-space: nowrap;
  }

  .context-chevron {
    color: var(--foreground-muted);
    opacity: 0.72;
    font-size: 13px;
  }

  .context-details {
    max-height: min(42vh, 420px);
    overflow: auto;
    padding: 10px;
    border-top: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
    display: grid;
    gap: 12px;
    font-size: 12px;
    color: var(--foreground-muted);
  }

  .context-copy p {
    margin: 0;
    color: var(--foreground);
    line-height: 1.55;
  }

  .context-detail-block,
  .context-list-block {
    display: grid;
    gap: 5px;
  }

  .context-detail-block {
    margin-top: 9px;
  }

  .context-detail-block strong,
  .context-list-block > strong {
    color: var(--foreground);
    font-size: 11px;
  }

  .context-detail-block ul {
    margin: 0;
    padding-left: 18px;
  }

  .context-source-list {
    display: grid;
    gap: 4px;
  }

  .context-source-row {
    min-height: 28px;
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    padding: 4px 7px;
    border-radius: 7px;
    background: color-mix(in srgb, var(--background) 72%, transparent);
  }

  .context-source-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .context-source-tokens,
  .context-empty {
    color: var(--foreground-muted);
    opacity: 0.72;
    font-size: 10px;
  }

  @media (max-width: 720px) {
    .context-observer {
      margin-inline: 8px;
    }

    .context-summary {
      align-items: flex-start;
    }

    .context-metrics {
      max-width: 62%;
      flex-wrap: wrap;
      justify-content: flex-end;
      gap: 3px 7px;
    }
  }
</style>
