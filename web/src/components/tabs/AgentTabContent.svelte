<script lang="ts">
  import type { ContentBlock, Message, TimelineRenderItem } from '../../types/message';
  import type { TaskDto } from '../../shared/rust-backend-types';
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

  function parseJsonRecord(value: unknown): Record<string, unknown> | null {
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      return value as Record<string, unknown>;
    }
    if (typeof value !== 'string' || !value.trim()) {
      return null;
    }
    try {
      const parsed = JSON.parse(value) as unknown;
      return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
        ? parsed as Record<string, unknown>
        : null;
    } catch {
      return null;
    }
  }

  function spawnResultChildTaskId(block: ContentBlock): string {
    const result = parseJsonRecord(block.toolCall?.result);
    return normalizeString(result?.child_task_id ?? result?.childTaskId);
  }

  const spawnAssignment = $derived.by<AgentAssignmentSummary | null>(() => {
    const projection = scopedTimelineProjection;
    if (!taskId || !projection) {
      return null;
    }
    for (const artifact of projection.artifacts || []) {
      const blocks = artifact.message?.blocks || [];
      for (const block of blocks) {
        if (block.type !== 'tool_call' || block.toolCall?.name !== 'agent_spawn') {
          continue;
        }
        if (spawnResultChildTaskId(block) !== taskId) {
          continue;
        }
        const args = block.toolCall.arguments || {};
        const result = parseJsonRecord(block.toolCall.result);
        return {
          title: getTaskDisplayText(normalizeString(result?.title) || normalizeString(args.display_name)) || '代理任务',
          goal: getTaskDisplayText(normalizeString(args.goal)),
        };
      }
    }
    return null;
  });

  const assignment = $derived.by<AgentAssignmentSummary | null>(() => {
    const task = projectionTask;
    if (task) {
      return {
        title: getTaskDisplayTitle(task) || spawnAssignment?.title || '代理任务',
        goal: getTaskDisplayGoal(task) || spawnAssignment?.goal || '',
      };
    }
    return spawnAssignment;
  });

  const agentRuntimeActive = $derived.by(() => {
    const status = normalizeString(projectionTask?.status).toLowerCase();
    return status === 'pending' || status === 'running';
  });

  const agentRuntimeStartedAt = $derived.by(() => {
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
    if (!taskId || !projection) {
      return assignmentRenderItem ? [assignmentRenderItem] : [];
    }
    const items = buildTimelineRenderItems(projection, 'task', taskId, {
      workspaceId: scopedWorkspaceId,
      workspacePath: scopedWorkspacePath,
      sessionId: scopedSessionId,
    });
    return assignmentRenderItem ? [assignmentRenderItem, ...items] : items;
  });
</script>

<div class="agent-tab-content">
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
</style>
