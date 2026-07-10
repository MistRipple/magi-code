<script lang="ts">
  import type { Message, TimelineRenderItem } from '../../types/message';
  import type { AgentProjectionDto, TaskDto } from '../../shared/rust-backend-types';
  import { agentTerminalOutput, type AgentTerminalOutput } from '../../lib/agent-output';
  import { buildTimelineRenderItems } from '../../lib/timeline-render-items';
  import { getTaskDisplayGoal, getTaskDisplayText, getTaskDisplayTitle } from '../../lib/task-labels';
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
  <MessageList
    taskId={agentRunId}
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
</style>
