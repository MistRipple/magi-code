<script lang="ts">
  import type { Message, TimelineRenderItem } from '../../types/message';
  import type { AgentProjectionDto, TaskDto } from '../../shared/rust-backend-types';
  import { agentTerminalOutput, type AgentTerminalOutput } from '../../lib/agent-output';
  import { buildTimelineRenderItems } from '../../lib/timeline-render-items';
  import { getTaskDisplayGoal, getTaskDisplayText, getTaskDisplayTitle } from '../../lib/task-labels';
  import { messagesState } from '../../stores/messages.svelte';
  import { getTaskProjectionState } from '../../stores/task-projection-store.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import MessageList from '../MessageList.svelte';

  interface Props {
    /** 代理 taskId —— 用于按 metadata.taskId 过滤 projection artifacts */
    taskId: string;
    workspaceId?: string | null;
    workspacePath?: string | null;
    sessionId?: string | null;
  }

  let {
    taskId,
    workspaceId = null,
    workspacePath = null,
    sessionId = null,
  }: Props = $props();

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

  const taskProjection = $derived(getTaskProjectionState(
    scopedSessionId,
    scopedWorkspaceId,
  ));

  const projectionTask = $derived.by<TaskDto | null>(() => {
    const projection = taskProjection.projection;
    if (!taskId || !projection) {
      return null;
    }
    if (projection.root_task.task_id === taskId) {
      return projection.root_task;
    }
    return projection.tasks.find((task) => task.task_id === taskId) ?? null;
  });

  const agentProjection = $derived.by<AgentProjectionDto | null>(() => {
    const projection = taskProjection.projection;
    if (!taskId || !projection?.agents) {
      return null;
    }
    return projection.agents.find((agent) => agent.taskId === taskId) ?? null;
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
    if (!taskId || !projection) {
      return Date.now();
    }
    for (const artifact of projection.artifacts || []) {
      if (artifact.message?.metadata?.taskId === taskId && artifact.timestamp > 0) {
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
    if (!taskId || !summary || !content) {
      return null;
    }
    const message: Message = {
      id: `agent-assignment:${taskId}`,
      role: 'user',
      source: 'user',
      content,
      timestamp: assignmentTimestamp(),
      isStreaming: false,
      isComplete: true,
      type: 'user_input',
      metadata: {
        taskId,
        agentAssignment: true,
      },
    };
    return {
      key: `assignment:${taskId}`,
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
    if (!taskId || !projection) {
      const baseItems = assignmentRenderItem ? [assignmentRenderItem] : [];
      if (!task || !terminalStatus(task.status) || !terminalOutput || hasSameAssistantContent(baseItems, terminalOutput.text)) {
        return baseItems;
      }
      return [
        ...baseItems,
        terminalOutputRenderItem(task, terminalOutput.text, terminalOutput.sourceRefIndex),
      ];
    }
    const items = buildTimelineRenderItems(projection, 'task', taskId, {
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
      id: `agent-terminal-output:${taskId}:${task.updated_at}:${sourceRefIndex}`,
      role: 'assistant',
      source: normalizeString(task.executor_binding?.target_role) || 'agent',
      content,
      timestamp: typeof task.updated_at === 'number' && task.updated_at > 0 ? task.updated_at : Date.now(),
      isStreaming: false,
      isComplete: true,
      type: 'result',
      metadata: {
        taskId,
        roleId: normalizeString(task.executor_binding?.target_role) || undefined,
        turnItemKind: 'task_output_ref',
        canonical: false,
      },
    };
    return {
      key: `agent-terminal-output:${taskId}:${task.updated_at}:${sourceRefIndex}`,
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

  function modelLabel(agent: AgentProjectionDto | null): string {
    const model = normalizeString(agent?.model);
    if (model) {
      return model;
    }
    const source = normalizeString(agent?.modelSource);
    if (source === 'inherited_orchestrator') {
      return '继承主模型';
    }
    return '未配置模型';
  }

  function accessModeLabel(value: unknown): string {
    switch (normalizeString(value)) {
      case 'read_only':
        return '只读';
      case 'full_access':
        return '完全授权';
      case 'restricted':
        return '受限执行';
      default:
        return normalizeString(value) || '受限执行';
    }
  }

  function lifecycleClass(agent: AgentProjectionDto | null): string {
    const lifecycle = normalizeString(agent?.lifecycle).toLowerCase();
    return lifecycle || 'unknown';
  }

  const showAgentHeader = $derived(Boolean(agentProjection || projectionTask || assignment));
</script>

<div class="agent-tab-content">
  {#if showAgentHeader}
    <section class="agent-summary" aria-label="代理运行摘要">
      <div class="agent-summary-main">
        <div class="agent-title-row">
          <span class={`agent-status-dot ${lifecycleClass(agentProjection)}`}></span>
          <h2>{assignment?.title || '代理任务'}</h2>
          <span class={`agent-status-pill ${lifecycleClass(agentProjection)}`}>
            {agentProjection?.statusLabel || projectionTask?.status || '待同步'}
          </span>
        </div>
        {#if assignment?.goal}
          <p>{assignment.goal}</p>
        {/if}
      </div>
      <div class="agent-meta-grid">
        <div class="agent-meta-item">
          <span>角色</span>
          <strong>{agentProjection?.role || projectionTask?.executor_binding?.target_role || 'agent'}</strong>
        </div>
        <div class="agent-meta-item">
          <span>模型</span>
          <strong>{modelLabel(agentProjection)}</strong>
        </div>
        <div class="agent-meta-item">
          <span>权限</span>
          <strong>{accessModeLabel(agentProjection?.accessMode || projectionTask?.policy_snapshot?.access_profile)}</strong>
        </div>
        {#if agentProjection?.workerId || agentProjection?.threadId}
          <div class="agent-meta-item mono">
            <span>运行身份</span>
            <strong>{agentProjection?.workerId || agentProjection?.threadId}</strong>
          </div>
        {/if}
      </div>
    </section>
  {/if}
  <MessageList
    taskId={taskId}
    renderItems={renderItems}
    displayContext="task"
    runtimeActive={agentRuntimeActive}
    runtimeStartedAt={agentRuntimeStartedAt}
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

  .agent-summary {
    flex: 0 0 auto;
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 16px;
    padding: 14px 16px;
    border-bottom: 1px solid var(--border-subtle, rgba(148, 163, 184, 0.24));
    background: var(--surface-elevated, rgba(15, 23, 42, 0.03));
  }

  .agent-summary-main {
    min-width: 0;
  }

  .agent-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .agent-title-row h2 {
    min-width: 0;
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 14px;
    font-weight: 650;
    color: var(--text-primary, #111827);
  }

  .agent-summary-main p {
    margin: 6px 0 0;
    max-width: 820px;
    color: var(--text-secondary, #4b5563);
    font-size: 12px;
    line-height: 1.5;
  }

  .agent-status-dot {
    width: 8px;
    height: 8px;
    border-radius: 999px;
    background: var(--text-tertiary, #94a3b8);
  }

  .agent-status-dot.running,
  .agent-status-dot.queued {
    background: var(--accent, #2563eb);
  }

  .agent-status-dot.completed {
    background: var(--success, #16a34a);
  }

  .agent-status-dot.failed,
  .agent-status-dot.degraded,
  .agent-status-dot.killed {
    background: var(--danger, #dc2626);
  }

  .agent-status-pill {
    flex: 0 0 auto;
    border-radius: 999px;
    padding: 2px 8px;
    font-size: 11px;
    line-height: 18px;
    color: var(--text-secondary, #475569);
    background: var(--surface-muted, rgba(148, 163, 184, 0.16));
  }

  .agent-status-pill.running,
  .agent-status-pill.queued {
    color: var(--accent, #2563eb);
  }

  .agent-status-pill.completed {
    color: var(--success, #15803d);
  }

  .agent-status-pill.failed,
  .agent-status-pill.degraded,
  .agent-status-pill.killed {
    color: var(--danger, #b91c1c);
  }

  .agent-meta-grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(88px, auto));
    gap: 8px;
    align-items: stretch;
  }

  .agent-meta-item {
    min-width: 0;
    border: 1px solid var(--border-subtle, rgba(148, 163, 184, 0.22));
    border-radius: 6px;
    padding: 7px 9px;
    background: var(--surface-base, rgba(255, 255, 255, 0.74));
  }

  .agent-meta-item span {
    display: block;
    margin-bottom: 3px;
    color: var(--text-tertiary, #64748b);
    font-size: 10px;
    line-height: 1.2;
  }

  .agent-meta-item strong {
    display: block;
    max-width: 150px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-primary, #111827);
    font-size: 12px;
    font-weight: 600;
  }

  .agent-meta-item.mono strong {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
  }

  @media (max-width: 860px) {
    .agent-summary {
      grid-template-columns: 1fr;
    }

    .agent-meta-grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
