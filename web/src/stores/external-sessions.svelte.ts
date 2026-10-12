/**
 * 外部会话：外部 MCP 客户端（Cursor、Claude Desktop、远程 agent 等）通过 Magi MCP 调用本机工具的过程。
 *
 * 外部会话是一个真正的会话（类型为外部工具），调用过程在它的 canonical 时间线里，和内部对话、GPT Web 同一套
 * 事实与展示；点开它就是切换到这个会话。这里只缓存侧栏入口所需的概要（哪些客户端有会话），事实在 daemon，
 * 收到 `mcp.activity` 事件后重新读取。
 */
import { listMcpExternalSessions, type McpExternalSession } from '../web/agent-api';
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

/** 读取外部会话概要；没有权限（插件未激活、经隧道访问）时视为没有外部会话。 */
export async function refreshExternalSessions(): Promise<void> {
  try {
    externalSessions.sessions = (await listMcpExternalSessions()).sessions;
  } catch {
    externalSessions.sessions = [];
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

function scheduleRefresh(): void {
  if (refreshTimer !== null) return;
  refreshTimer = window.setTimeout(() => {
    refreshTimer = null;
    void refreshExternalSessions();
  }, 300);
}

if (typeof window !== 'undefined') {
  window.addEventListener(MCP_ACTIVITY_EVENT, scheduleRefresh);
}
