import type { Session } from '../types/message';

interface SessionProjectionLike {
  workspaceId?: string | null;
  sessions: readonly Session[];
}

export interface SessionTitleContext {
  sessionId: string | null | undefined;
  workspaceId: string | null | undefined;
  workspacePath?: string | null | undefined;
  workspaceSessions?: SessionProjectionLike | null;
  workspaceSessionProjections?: Readonly<Record<string, SessionProjectionLike | undefined>>;
  personalSessions: readonly Session[];
}

/** 从当前作用域的会话投影中解析用户可见标题。 */
export function resolveSessionTitle(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
  workspaceSessions: readonly Session[],
  personalSessions: readonly Session[],
): string {
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  if (!normalizedSessionId) return '';
  const normalizedWorkspaceId = typeof workspaceId === 'string' ? workspaceId.trim() : '';
  const sessions = normalizedWorkspaceId ? workspaceSessions : personalSessions;
  const session = sessions.find((candidate) => candidate.id === normalizedSessionId);
  return session?.name?.trim() || '';
}

/** 从当前会话上下文选择正确作用域，再解析用户可见标题。 */
export function resolveCurrentSessionTitle(context: SessionTitleContext): string {
  const workspaceId = typeof context.workspaceId === 'string'
    ? context.workspaceId.trim()
    : '';
  const workspacePath = typeof context.workspacePath === 'string'
    ? context.workspacePath.trim()
    : '';
  const workspaceScope = Boolean(workspaceId || workspacePath);
  const workspaceProjection = workspaceScope
    ? context.workspaceSessionProjections?.[workspaceId] ?? context.workspaceSessions
    : null;
  const projectionWorkspaceId = typeof workspaceProjection?.workspaceId === 'string'
    ? workspaceProjection.workspaceId.trim()
    : '';
  if (workspaceId && projectionWorkspaceId && projectionWorkspaceId !== workspaceId) {
    return '';
  }
  return resolveSessionTitle(
    context.sessionId,
    workspaceScope ? workspaceId || workspacePath : '',
    workspaceProjection?.sessions ?? [],
    context.personalSessions,
  );
}
