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
