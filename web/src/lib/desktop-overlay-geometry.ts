export interface DesktopOverlayPopupBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

// 原生 Overlay 的菜单必须在触发按钮附近保持紧凑。此前固定 300px 会在
// 默认 480px 的右栏内几乎铺满整条工具栏下方，视觉上像脱离了触发按钮。
// 260px 足以容纳两个视口数字输入，同时保留明确的右侧锚定关系。
const MENU_WIDTH = 260;
const MENU_EDGE_INSET = 6;
const MENU_ANCHOR_GAP = 4;
const MENU_FRAME_HEIGHT = 12;
const MENU_ITEM_HEIGHT = 30;
const MENU_ITEM_GAP = 1;
const MENU_FIELDS_HEIGHT = 58;

/**
 * 菜单的原生 WebContentsView 只占菜单自身的最终像素区域。
 *
 * 触发按钮和右栏容器同属 App Renderer DOM，因此这里直接读取浏览器已完成
 * 排版后的矩形；Main 只复制该矩形到原生 View，绝不再推导偏移或尺寸。
 */
export function measureDesktopOverlayMenuBounds(
  anchor: HTMLElement | undefined,
  itemCount: number,
  fieldCount: number,
): DesktopOverlayPopupBounds | null {
  const pane = anchor?.closest<HTMLElement>('.right-pane');
  if (!anchor || !pane) return null;
  const anchorBounds = anchor.getBoundingClientRect();
  const paneBounds = pane.getBoundingClientRect();
  if (
    anchorBounds.width <= 0
    || anchorBounds.height <= 0
    || paneBounds.width <= MENU_EDGE_INSET * 2
    || paneBounds.height <= 0
  ) return null;

  const width = Math.min(MENU_WIDTH, paneBounds.width - MENU_EDGE_INSET * 2);
  const top = anchorBounds.bottom + MENU_ANCHOR_GAP;
  const availableHeight = paneBounds.bottom - top - MENU_EDGE_INSET;
  if (availableHeight <= 0) return null;

  const itemHeight = itemCount > 0
    ? itemCount * MENU_ITEM_HEIGHT + (itemCount - 1) * MENU_ITEM_GAP
    : 0;
  const naturalHeight = MENU_FRAME_HEIGHT
    + itemHeight
    + (fieldCount > 0 ? MENU_FIELDS_HEIGHT : 0);
  const height = Math.min(naturalHeight, availableHeight);
  const minLeft = paneBounds.left + MENU_EDGE_INSET;
  const maxLeft = paneBounds.right - MENU_EDGE_INSET - width;
  const left = Math.min(Math.max(anchorBounds.right - width, minLeft), maxLeft);

  return {
    x: Math.round(left),
    y: Math.round(top),
    width: Math.round(width),
    height: Math.round(height),
  };
}
