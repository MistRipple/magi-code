import type { Session } from '../types/message';
import type { McpExternalSession } from '../web/agent-api';

/**
 * 把外部 MCP 会话投影成侧栏可排序的会话条目。
 *
 * 外部会话仍由 MCP 专用接口提供概要，普通会话由工作区目录接口提供；
 * 侧栏只在展示前合并这两个事实源，并按各自最近活动时间统一排序。
 */
export function mergeExternalSessionsIntoSidebar(
  sessions: Session[],
  external: McpExternalSession[],
  workspaceId: string,
): Session[] {
  const externalSessions = external
    .filter((session) => session.workspaceId === workspaceId)
    .map((session): Session => ({
      id: session.sessionId,
      workspaceId: session.workspaceId ?? undefined,
      name: session.clientName,
      createdAt: session.lastActivityAtMs,
      updatedAt: session.lastActivityAtMs,
      messageCount: 0,
      isRunning: session.running > 0,
      runningTaskCount: session.running,
    }));

  return [...sessions, ...externalSessions]
    .map((session, index) => ({ session, index }))
    .sort((left, right) => {
      const byActivity = right.session.updatedAt - left.session.updatedAt;
      return byActivity || left.index - right.index;
    })
    .map(({ session }) => session);
}
