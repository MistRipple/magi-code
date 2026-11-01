import type { Session } from '../types/message';

/** 无项目会话按活动时间排序；项目会话只属于项目分组，不进入这份列表。 */
export function personalSessionList(personal: readonly Session[]): Session[] {
  return personal
    .filter((session) => !session.workspaceId)
    .sort((left, right) => (
      (right.updatedAt || right.createdAt) - (left.updatedAt || left.createdAt)
      || left.id.localeCompare(right.id)
    ));
}
