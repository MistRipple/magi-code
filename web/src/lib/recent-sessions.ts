import type { Session } from '../types/message';

/** 将已有权威目录投影合并为最近列表，不产生新的会话归属或缓存。 */
export function recentSessionList(personal: readonly Session[], projects: Record<string, Session[]>): Session[] {
  const sessions = new Map<string, Session>();
  for (const session of personal) sessions.set(session.id, session);
  for (const entries of Object.values(projects)) {
    for (const session of entries) sessions.set(session.id, session);
  }
  return [...sessions.values()].sort((left, right) => (
    (right.updatedAt || right.createdAt) - (left.updatedAt || left.createdAt)
    || left.id.localeCompare(right.id)
  ));
}
