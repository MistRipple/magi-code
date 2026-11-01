
import { AgentApiError, listMcpExternalSessions, type McpExternalSession } from '../web/agent-api';
import { MCP_ACTIVITY_EVENT } from '../lib/mcp-server-events';

/** 外部会话 id 的前缀（与 daemon 一致）。 */
export const EXTERNAL_SESSION_ID_PREFIX = 'external-tool-';

export function isExternalSessionId(sessionId: string | null | undefined): boolean {
  return typeof sessionId === 'string' && sessionId.startsWith(EXTERNAL_SESSION_ID_PREFIX);
}

export const externalSessions = $state({
  /** 有会话的外部客户端，最近活动的在前。 */
  sessions: [] as McpExternalSession[],
});

let refreshRevision = 0;

/** 读取外部会话概要；没有权限（插件未激活、经隧道访问）时视为没有外部会话。 */
export async function refreshExternalSessions(): Promise<void> {
  const revision = ++refreshRevision;
  if (refreshTimer !== null) {
    window.clearTimeout(refreshTimer);
    refreshTimer = null;
  }
  try {
    const result = await listMcpExternalSessions();
    if (revision !== refreshRevision) return;
    externalSessions.sessions = result.sessions;
  } catch (error) {
    if (revision !== refreshRevision) return;
    if (error instanceof AgentApiError && [401, 403, 404, 501].includes(error.status)) {
      externalSessions.sessions = [];
    } else {
      // 首次读取失败时也要恢复，不能等下一次 MCP 调用或用户切换工作区。
      if (typeof window !== 'undefined') scheduleRefresh(3000);
    }
  }
}

export function externalSessionById(sessionId: string | null | undefined): McpExternalSession | undefined {
  return externalSessions.sessions.find((session) => session.sessionId === sessionId);
}

/** 请求在主对话区打开某个外部会话；由侧栏所在的工作台执行切换（它持有工作区与导航）。 */
export const OPEN_EXTERNAL_SESSION_EVENT = 'magi:open-external-session';

export function openExternalSession(session: McpExternalSession): void {
  window.dispatchEvent(new CustomEvent(OPEN_EXTERNAL_SESSION_EVENT, { detail: { session } }));
}

let refreshTimer: number | null = null;

function scheduleRefresh(delayMs = 300): void {
  if (refreshTimer !== null) return;
  refreshTimer = window.setTimeout(() => {
    refreshTimer = null;
    void refreshExternalSessions();
  }, delayMs);
}

if (typeof window !== 'undefined') {
  window.addEventListener(MCP_ACTIVITY_EVENT, () => scheduleRefresh());
}
