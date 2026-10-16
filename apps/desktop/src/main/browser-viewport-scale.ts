export interface BrowserDisplaySize {
  width: number;
  height: number;
}

/** 计算 Chromium 设备画布完整容纳于当前内容槽所需的显示比例。 */
export function fitBrowserViewportScale(
  viewport: BrowserDisplaySize,
  display: BrowserDisplaySize,
): number {
  const ratio = Math.min(
    1,
    display.width / viewport.width,
    display.height / viewport.height,
  );
  // 向下保留六位，避免浮点舍入让设备画布越过内容槽边界。
  return Math.floor(ratio * 1_000_000) / 1_000_000;
}

/** CDP 原生鼠标坐标属于 guest 显示画布；Worker 始终传页面 CSS 坐标。 */
export function browserInputDisplayParams(
  method: string,
  params: Record<string, unknown>,
  scale: number,
): Record<string, unknown> {
  if (method !== "Input.dispatchMouseEvent" || scale === 1) return params;
  return {
    ...params,
    ...(typeof params.x === "number" ? { x: params.x * scale } : {}),
    ...(typeof params.y === "number" ? { y: params.y * scale } : {}),
  };
}
