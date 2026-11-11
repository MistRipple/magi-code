/**
 * 当前会话的代理运行视图模型。
 *
 * 状态条、对话里的恢复横幅和右栏的代理列表读取同一份投影，这里是它们共用的唯一入口：
 * - 运行投影只来自 daemon 的权威投影（agent-run-store），不从本地状态反推 root；
 * - 列表直接派生自当前投影，归档由 daemon 解除执行链绑定，不另存投影或本地归档标记；
 * - 运行视图的操作只有归档，由后端 `availableActions` 驱动；停止后的“继续”是会话级动作，
 *   由 daemon 的 `continuation` 判定，这里只把它转给界面（`continuation`），不自己推断。
 */
import type {
  AgentProjectionDto,
  AgentRunActionKind,
  SessionContinuation,
} from '../shared/rust-backend-types';
import {
  buildActiveAgentSummary,
  groupActiveAgents,
  selectActiveAgentRunProjection,
  type ActiveAgentGroups,
  type ActiveAgentSummary,
} from '../lib/active-agent-center';
import {
  directIncidentError,
  incidentErrorDiagnostics,
  reportIncident,
} from '../lib/notifications';
import { i18n } from './i18n.svelte';
import {
  getAgentRunState,
  performAgentRunAction,
  selectAgentRun,
} from './agent-run-store.svelte';
import { addToast, messagesState } from './messages.svelte';
import { openAgentsTab, openAgentTab, rightPaneState } from './right-pane.svelte';

const currentSessionId = $derived(messagesState.currentSessionId);
const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);
const agentRunState = $derived(getAgentRunState(currentSessionId, currentWorkspaceId));
const visibleProjection = $derived(selectActiveAgentRunProjection(agentRunState.projection));
const agentGroups = $derived.by<ActiveAgentGroups>(() => groupActiveAgents(visibleProjection?.agents ?? []));
const summary = $derived.by<ActiveAgentSummary>(() => buildActiveAgentSummary(agentGroups));

function normalizedScopePart(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

async function runAction(action: AgentRunActionKind): Promise<void> {
  const sessionId = normalizedScopePart(currentSessionId);
  const workspaceId = currentWorkspaceId;
  const workspacePath = currentWorkspacePath;
  const rootTaskId = visibleProjection?.root_task.task_id ?? '';
  if (!sessionId || !rootTaskId || !visibleProjection?.availableActions.includes(action)) return;
  try {
    const response = await performAgentRunAction(
      sessionId,
      rootTaskId,
      action,
      workspaceId,
      workspacePath,
    );
    if (!response) return;
    addToast('success', i18n.t(`activeAgentCenter.action.${action}Succeeded`));
  } catch (error) {
    const fallback = i18n.t(`activeAgentCenter.action.${action}Failed`);
    const message = directIncidentError(error, fallback);
    const diagnostics = incidentErrorDiagnostics(error, message);
    addToast('error', message);
    reportIncident(message, {
      scope: 'session',
      context: { sessionId, workspaceId: workspaceId || undefined, workspacePath: workspacePath || undefined },
      source: 'active-agent-center',
      title: fallback,
      taskId: rootTaskId,
      failureStage: `agent_run_${action}`,
      ...diagnostics,
    });
  }
}

/** 与运行中心打开代理的方式一致：先选中这次运行，再在右栏打开它的 tab。 */
function openAgent(agent: Pick<AgentProjectionDto, 'agentRunId'>, label: string): void {
  const sessionId = normalizedScopePart(currentSessionId);
  if (!sessionId) return;
  selectAgentRun(sessionId, agent.agentRunId, currentWorkspaceId, currentWorkspacePath);
  openAgentTab(sessionId, agent.agentRunId, {
    label,
    workspaceId: currentWorkspaceId,
    workspacePath: currentWorkspacePath,
  });
}

/** 打开右栏的代理列表（已有就回到它）；默认作用域就是当前右栏。 */
function openList(scopeKey: string = rightPaneState.activeScopeKey): void {
  openAgentsTab(scopeKey);
}

export const activeAgentRun = {
  get groups(): ActiveAgentGroups { return agentGroups; },
  get summary(): ActiveAgentSummary { return summary; },
  /** 本会话是否有（未被用户归档的）一轮代理运行。 */
  get hasRun(): boolean { return visibleProjection !== null; },
  get outcome() { return visibleProjection?.outcome ?? null; },
  get failureSummary() { return visibleProjection?.failureSummary ?? null; },
  get availableActions(): AgentRunActionKind[] { return visibleProjection?.availableActions ?? []; },
  /**
   * 本会话停止后的继续方式，来自 daemon 读模型；没有则现在不可继续。
   * 输入框的发送按钮、恢复横幅共用它，Web 不再自己判断“能不能继续”。
   */
  get continuation(): SessionContinuation | null {
    const runtime = messagesState.orchestratorRuntimeState;
    return runtime?.sessionId === currentSessionId ? (runtime.continuation ?? null) : null;
  },
  get tasks() { return visibleProjection?.tasks ?? []; },
  get selectedAgentRunId() { return agentRunState.selectedAgentRunId; },
  get actionInFlight() { return agentRunState.actionInFlight; },
  get actionError() { return agentRunState.actionError; },
  get sessionId(): string { return normalizedScopePart(currentSessionId); },
  runAction,
  openAgent,
  openList,
};
