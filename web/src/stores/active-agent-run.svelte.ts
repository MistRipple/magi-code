/**
 * 当前会话的代理运行视图模型。
 *
 * 状态条、对话里的恢复横幅和右栏的代理列表读取同一份投影，这里是它们共用的唯一入口：
 * - 运行投影只来自 daemon 的权威投影（agent-run-store），不从本地状态反推 root；
 * - 用户归档后清掉的只是本地“已关闭”标记，按会话持久化，换一轮运行（新的 root task）自动恢复；
 * - 运行视图的操作只有归档，由后端 `availableActions` 驱动；停止后的“继续”是会话级动作，
 *   由 daemon 的 `continuation` 判定，这里只把它转给界面（`continuation`），不自己推断。
 */
import type {
  AgentProjectionDto,
  AgentRunActionKind,
  AgentRunProjectionDto,
  SessionContinuation,
} from '../shared/rust-backend-types';
import {
  buildActiveAgentSummary,
  groupActiveAgents,
  resolveAgentProjectionPin,
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
  ensureAgentRunState,
  getAgentRunState,
  performAgentRunAction,
  selectAgentRun,
} from './agent-run-store.svelte';
import { addToast, messagesState } from './messages.svelte';
import { openAgentsTab, openAgentTab, rightPaneState } from './right-pane.svelte';

const STORAGE_PREFIX = 'magi.active-agent-center.v1';

interface PersistedAgentCenterState {
  dismissedRootTaskId: string;
}

let pinnedProjection = $state<AgentRunProjectionDto | null>(null);
let dismissedRootTaskId = $state('');
let activeScopeKey = '';

const currentSessionId = $derived(messagesState.currentSessionId);
const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);
const agentRunState = $derived(getAgentRunState(currentSessionId, currentWorkspaceId));
const currentProjection = $derived(agentRunState.projection);
const agentGroups = $derived.by<ActiveAgentGroups>(() => groupActiveAgents(pinnedProjection?.agents ?? []));
const summary = $derived.by<ActiveAgentSummary>(() => buildActiveAgentSummary(agentGroups));

function normalizedScopePart(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function agentCenterScopeKey(): string {
  const sessionId = normalizedScopePart(currentSessionId);
  if (!sessionId) return '';
  const workspace = normalizedScopePart(currentWorkspaceId) || normalizedScopePart(currentWorkspacePath);
  return workspace ? `${workspace}\u0000${sessionId}` : `session:${sessionId}`;
}

function storageKey(scopeKey: string): string {
  return `${STORAGE_PREFIX}:${scopeKey}`;
}

function readPersistedState(scopeKey: string): PersistedAgentCenterState {
  if (!scopeKey || typeof localStorage === 'undefined') {
    return { dismissedRootTaskId: '' };
  }
  try {
    const value = JSON.parse(localStorage.getItem(storageKey(scopeKey)) ?? '{}') as Partial<PersistedAgentCenterState>;
    return { dismissedRootTaskId: normalizedScopePart(value.dismissedRootTaskId) };
  } catch {
    localStorage.removeItem(storageKey(scopeKey));
    return { dismissedRootTaskId: '' };
  }
}

function persistState(scopeKey: string): void {
  if (!scopeKey || typeof localStorage === 'undefined') return;
  const value: PersistedAgentCenterState = { dismissedRootTaskId };
  localStorage.setItem(storageKey(scopeKey), JSON.stringify(value));
}

if (typeof window !== 'undefined') {
  $effect.root(() => {
    $effect(() => {
      ensureAgentRunState(currentSessionId, currentWorkspaceId, currentWorkspacePath);
    });

    $effect(() => {
      const scopeKey = agentCenterScopeKey();
      if (scopeKey === activeScopeKey) return;
      activeScopeKey = scopeKey;
      pinnedProjection = null;
      dismissedRootTaskId = '';
      if (!scopeKey) return;
      dismissedRootTaskId = readPersistedState(scopeKey).dismissedRootTaskId;
    });

    $effect(() => {
      const candidate = currentProjection;
      const scopeKey = agentCenterScopeKey();
      const rootTaskId = candidate?.root_task.task_id?.trim() ?? '';
      const agents = candidate?.agents ?? [];
      if (!candidate || !scopeKey) {
        return;
      }
      const pinDecision = resolveAgentProjectionPin(
        rootTaskId,
        agents.length,
        dismissedRootTaskId,
        candidate.outcome,
      );
      if (pinDecision === 'ignore') {
        return;
      }
      if (pinDecision === 'clear') {
        pinnedProjection = null;
        if (dismissedRootTaskId && dismissedRootTaskId !== rootTaskId) {
          dismissedRootTaskId = '';
        }
        persistState(scopeKey);
        return;
      }
      pinnedProjection = candidate;
      if (dismissedRootTaskId && dismissedRootTaskId !== rootTaskId) {
        dismissedRootTaskId = '';
      }
      persistState(scopeKey);
    });
  });
}

async function runAction(action: AgentRunActionKind): Promise<void> {
  const sessionId = normalizedScopePart(currentSessionId);
  const rootTaskId = pinnedProjection?.root_task.task_id?.trim()
    || agentRunState.rootTaskId?.trim()
    || '';
  if (!sessionId || !rootTaskId || agentRunState.actionInFlight) return;
  try {
    const response = await performAgentRunAction(
      sessionId,
      rootTaskId,
      action,
      currentWorkspaceId,
      currentWorkspacePath,
    );
    if (!response) return;
    dismissedRootTaskId = rootTaskId;
    pinnedProjection = null;
    persistState(agentCenterScopeKey());
    addToast('success', i18n.t(`activeAgentCenter.action.${action}Succeeded`));
  } catch (error) {
    const fallback = i18n.t(`activeAgentCenter.action.${action}Failed`);
    const message = directIncidentError(error, fallback);
    const diagnostics = incidentErrorDiagnostics(error, message);
    addToast('error', message);
    reportIncident(message, {
      scope: 'session',
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
  get hasRun(): boolean { return pinnedProjection !== null; },
  get outcome() { return pinnedProjection?.outcome ?? null; },
  get failureSummary() { return pinnedProjection?.failureSummary ?? null; },
  get availableActions(): AgentRunActionKind[] { return pinnedProjection?.availableActions ?? []; },
  /**
   * 本会话停止后的继续方式，来自 daemon 读模型；没有则现在不可继续。
   * 输入框的发送按钮、恢复横幅共用它，Web 不再自己判断“能不能继续”。
   */
  get continuation(): SessionContinuation | null {
    const runtime = messagesState.orchestratorRuntimeState;
    return runtime?.sessionId === currentSessionId ? (runtime.continuation ?? null) : null;
  },
  get tasks() { return pinnedProjection?.tasks ?? []; },
  get selectedAgentRunId() { return agentRunState.selectedAgentRunId; },
  get actionInFlight() { return agentRunState.actionInFlight; },
  get actionError() { return agentRunState.actionError; },
  get sessionId(): string { return normalizedScopePart(currentSessionId); },
  runAction,
  openAgent,
  openList,
};
