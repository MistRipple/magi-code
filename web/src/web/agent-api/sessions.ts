import { getTransport } from '../../shared/transport';
import type { MessagesResponseDto } from '../../shared/rust-backend-types';
import type { CanonicalTurnItem } from '../../shared/protocol/canonical-turn';
import { i18n } from '../../stores/i18n.svelte';
import type { AgentBindingOverride } from '../agent-binding-context';
import type { AgentNotificationScope, AgentNotificationsPayload, AgentSessionSummary, AgentUnavailableSession, AgentWorkspaceSessionsSnapshot, RawAgentSessionSummary, RawAgentWorkspaceSummary, TurnItemsPageDto } from './types';
import { AgentApiError, agentUrl, buildBoundQueryWithOverride, createNotificationBindingOverride, findCachedWorkspaceSummary, normalizeSessionSummary, normalizeUnavailableSessions, normalizeWorkspaceSummary, parseAgentJson, postBoundJson } from './core';

export async function getWorkspaceSessions(
  workspaceId: string,
  workspacePath?: string,
): Promise<AgentWorkspaceSessionsSnapshot> {
  try {
    const query = new URLSearchParams({ workspaceId });
    if (workspacePath?.trim()) {
      query.set('workspacePath', workspacePath.trim());
    }
    const response = await getTransport().request(
      agentUrl('/api/workspaces/sessions', query.toString())
    );
    const payload = await parseAgentJson<{
      runtimeEpoch?: string;
      eventStreamNextSequence?: number;
      workspace?: RawAgentWorkspaceSummary;
      sessions?: RawAgentSessionSummary[];
      unavailableSessions?: unknown;
    }>(response, 'workspace sessions');
    const sessions = Array.isArray(payload.sessions)
      ? payload.sessions.map((session) => normalizeSessionSummary(session))
      : [];
    const runtimeEpoch = payload.runtimeEpoch?.trim() || '';
    const eventStreamNextSequence = payload.eventStreamNextSequence;
    if (
      !runtimeEpoch
      || typeof eventStreamNextSequence !== 'number'
      || !Number.isFinite(eventStreamNextSequence)
      || eventStreamNextSequence < 1
    ) {
      throw new Error('workspace sessions 缺少有效的运行时代际或事件游标');
    }
    return {
      runtimeEpoch,
      eventStreamNextSequence: Math.floor(eventStreamNextSequence),
      workspace: payload.workspace
        ? normalizeWorkspaceSummary(payload.workspace)
        : findCachedWorkspaceSummary(workspaceId),
      sessions,
      unavailableSessions: normalizeUnavailableSessions(payload.unavailableSessions),
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getPersonalSessions(): Promise<{
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  sessions: AgentSessionSummary[];
  unavailableSessions: AgentUnavailableSession[];
}> {
  const response = await getTransport().request(agentUrl('/api/sessions/personal'));
  const payload = await parseAgentJson<{
    runtimeEpoch?: string;
    eventStreamNextSequence?: number;
    sessions?: RawAgentSessionSummary[];
    unavailableSessions?: unknown;
  }>(response, 'personal sessions');
  const runtimeEpoch = payload.runtimeEpoch?.trim() || '';
  const eventStreamNextSequence = payload.eventStreamNextSequence;
  if (!runtimeEpoch || typeof eventStreamNextSequence !== 'number' || !Number.isFinite(eventStreamNextSequence)) {
    throw new Error('personal sessions 缺少有效的运行时代际或事件游标');
  }
  return {
    runtimeEpoch,
    eventStreamNextSequence: Math.floor(eventStreamNextSequence),
    sessions: Array.isArray(payload.sessions) ? payload.sessions.map(normalizeSessionSummary) : [],
    unavailableSessions: normalizeUnavailableSessions(payload.unavailableSessions),
  };
}

export async function getAgentSessionMessages(options: {
  sessionId: string;
  scope: 'personal' | 'workspace';
  workspaceId?: string;
  workspacePath?: string;
  canonicalBeforeCursor?: string | null;
  limit?: number;
}): Promise<MessagesResponseDto> {
  const sessionId = options.sessionId.trim();
  const workspaceId = options.workspaceId?.trim() || '';
  if (!sessionId || (options.scope === 'workspace' && !workspaceId && !options.workspacePath?.trim())) {
    throw new AgentApiError(400, '会话作用域不完整', 'load older session messages');
  }
  try {
    const bindingOverride: AgentBindingOverride = options.scope === 'workspace'
      ? {
          scope: 'workspace',
          sessionId,
          workspaceId,
          workspacePath: options.workspacePath?.trim() || '',
        }
      : { scope: 'personal', sessionId };
    const query = buildBoundQueryWithOverride(
      {
        ...(options.limit ? { limit: String(options.limit) } : {}),
        ...(options.canonicalBeforeCursor
          ? { canonicalBeforeCursor: options.canonicalBeforeCursor }
          : {}),
      },
      bindingOverride,
    );
    const response = await getTransport().request(agentUrl('/api/messages', query));
    return await parseAgentJson<MessagesResponseDto>(response, 'load older session messages');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

/** 补齐被历史窗口折叠的 turn 内更早条目（回合内向前翻页）。 */
export async function getAgentTurnItems(options: {
  sessionId: string;
  turnId: string;
  beforeItemSeq: number;
  limit?: number;
}): Promise<TurnItemsPageDto> {
  try {
    const query = buildBoundQueryWithOverride(
      {
        turnId: options.turnId,
        beforeItemSeq: String(options.beforeItemSeq),
        ...(options.limit ? { limit: String(options.limit) } : {}),
      },
      { sessionId: options.sessionId.trim() },
    );
    const response = await getTransport().request(agentUrl('/api/messages/turn-items', query));
    return await parseAgentJson<TurnItemsPageDto>(response, 'load earlier turn items');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

/** 取回单个 canonical item 的完整事实（历史分页会截断超长工具输出）。 */
export async function getAgentMessageItem(options: {
  sessionId: string;
  turnId: string;
  itemId: string;
}): Promise<CanonicalTurnItem> {
  try {
    const query = buildBoundQueryWithOverride(
      { turnId: options.turnId, itemId: options.itemId },
      { sessionId: options.sessionId.trim() },
    );
    const response = await getTransport().request(agentUrl('/api/messages/item', query));
    const payload = await parseAgentJson<{ item: CanonicalTurnItem }>(response, 'load message item');
    return payload.item;
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function deleteAgentSession(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<unknown> {
  return await postBoundJson<unknown>(
    '/api/session/delete',
    { sessionId },
    'delete session',
    bindingOverride,
  );
}

export async function markAgentSessionViewed(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<{
  runtimeEpoch: string;
  eventStreamNextSequence: number;
  sessionId: string;
  hasUnreadCompletion: boolean;
}> {
  const result = await postBoundJson<{
    runtimeEpoch: string;
    eventStreamNextSequence: number;
    sessionId: string;
    workspaceId?: string | null;
    hasUnreadCompletion: boolean;
  }>(
    '/api/session/viewed',
    { sessionId },
    'mark session viewed',
    bindingOverride,
  );
  if (
    !result.runtimeEpoch?.trim()
    || !Number.isFinite(result.eventStreamNextSequence)
    || result.eventStreamNextSequence < 1
  ) {
    throw new Error('mark session viewed 缺少有效的运行时代际或事件游标');
  }
  return {
    ...result,
    runtimeEpoch: result.runtimeEpoch.trim(),
    eventStreamNextSequence: Math.floor(result.eventStreamNextSequence),
  };
}

export async function renameAgentSession(
  sessionId: string,
  name: string,
  bindingOverride?: AgentBindingOverride,
): Promise<unknown> {
  return await postBoundJson<unknown>(
    '/api/session/rename',
    { sessionId, name },
    'rename session',
    bindingOverride,
  );
}

export async function closeAgentSession(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<unknown> {
  return await postBoundJson<unknown>(
    '/api/session/close',
    { sessionId },
    'close session',
    bindingOverride,
  );
}

export async function getAgentNotifications(scope: AgentNotificationScope): Promise<AgentNotificationsPayload> {
  const query = buildBoundQueryWithOverride({}, createNotificationBindingOverride(scope));
  const response = await getTransport().request(agentUrl('/api/notifications', query));
  return await parseAgentJson<AgentNotificationsPayload>(response, 'load notifications');
}

export async function reportAgentIncident(
  incident: Record<string, unknown>,
  scope: AgentNotificationScope,
): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/report',
    { ...incident },
    'report incident',
    createNotificationBindingOverride(scope),
  );
}

export async function markAllAgentNotificationsRead(scope: AgentNotificationScope): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/mark-all-read',
    {},
    'mark all notifications read',
    createNotificationBindingOverride(scope),
  );
}

export async function clearAgentNotifications(scope: AgentNotificationScope): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/clear',
    {},
    'clear notifications',
    createNotificationBindingOverride(scope),
  );
}

export async function removeAgentNotification(
  notificationId: string,
  scope: AgentNotificationScope,
): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/remove',
    { notificationId },
    'remove notification',
    createNotificationBindingOverride(scope),
  );
}

export async function resolveAgentNotification(
  notificationId: string,
  scope: AgentNotificationScope,
): Promise<AgentNotificationsPayload> {
  return await postBoundJson<AgentNotificationsPayload>(
    '/api/notifications/resolve',
    { notificationId },
    'resolve notification',
    createNotificationBindingOverride(scope),
  );
}
