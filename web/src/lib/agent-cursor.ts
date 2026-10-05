/**
 * 内置浏览器中 Magi 代理光标的展示规则。
 *
 * Desktop Main 是光标位置与动作的唯一来源（agent_cursor 事件，坐标已换算为内容槽
 * CSS 像素）；这里只决定如何把相邻两次事件呈现成连贯的动作：滑行时长、动作标签和
 * 名牌在边缘处的朝向。
 */

export type AgentCursorAction = 'move' | 'click' | 'drag' | 'type' | 'scroll';

export interface AgentCursorPoint {
  x: number;
  y: number;
}

export interface AgentCursorBox {
  width: number;
  height: number;
}

/** 动作标签在最后一次同类动作后保留的时长。 */
export const AGENT_CURSOR_ACTION_HOLD_MILLIS = 1_400;
/** 连续这么久没有新动作时进入等待态（光标保持可见并轻微呼吸）。 */
export const AGENT_CURSOR_IDLE_MILLIS = 1_800;

const MIN_GLIDE_MILLIS = 160;
const MAX_GLIDE_MILLIS = 560;
const GLIDE_MILLIS_PER_PIXEL = 0.45;
/** 小于这个距离的位移视为原地（例如点击的 released 事件）。 */
const STATIONARY_DISTANCE = 2;
/** 名牌的大致占位，用来判断是否需要翻到光标另一侧以留在内容槽内。 */
const TAG_RESERVED_WIDTH = 128;
const TAG_RESERVED_HEIGHT = 44;

export function normalizeAgentCursorAction(value: unknown): AgentCursorAction | null {
  switch (value) {
    case 'move':
    case 'click':
    case 'drag':
    case 'type':
    case 'scroll':
      return value;
    default:
      return null;
  }
}

export function agentCursorPoint(x: unknown, y: unknown): AgentCursorPoint | null {
  return typeof x === 'number' && Number.isFinite(x) && typeof y === 'number' && Number.isFinite(y)
    ? { x, y }
    : null;
}

export function isStationaryMove(from: AgentCursorPoint | null, to: AgentCursorPoint): boolean {
  return from !== null && Math.hypot(to.x - from.x, to.y - from.y) < STATIONARY_DISTANCE;
}

/**
 * 从上一个落点滑向新落点的时长：距离越远越久，但有上下限，保证短距离也能看清、
 * 长距离不拖慢观感。首次出现或原地更新不滑行。
 */
export function agentCursorGlideMillis(from: AgentCursorPoint | null, to: AgentCursorPoint): number {
  if (from === null || isStationaryMove(from, to)) return 0;
  const distance = Math.hypot(to.x - from.x, to.y - from.y);
  return Math.round(
    Math.min(MAX_GLIDE_MILLIS, Math.max(MIN_GLIDE_MILLIS, MIN_GLIDE_MILLIS + distance * GLIDE_MILLIS_PER_PIXEL)),
  );
}

/** 靠近右侧或底部边缘时把名牌翻到光标左侧或上方，始终留在内容槽内。 */
export function agentCursorTagPlacement(
  point: AgentCursorPoint,
  box: AgentCursorBox,
): { flipX: boolean; flipY: boolean } {
  return {
    flipX: box.width > 0 && point.x > box.width - TAG_RESERVED_WIDTH,
    flipY: box.height > 0 && point.y > box.height - TAG_RESERVED_HEIGHT,
  };
}

/** 需要在名牌上说明的动作；普通移动只显示名字。 */
export function agentCursorActionLabelKey(action: AgentCursorAction | null): string | null {
  switch (action) {
    case 'click':
      return 'browser.agent.cursor.click';
    case 'type':
      return 'browser.agent.cursor.type';
    case 'scroll':
      return 'browser.agent.cursor.scroll';
    case 'drag':
      return 'browser.agent.cursor.drag';
    default:
      return null;
  }
}
