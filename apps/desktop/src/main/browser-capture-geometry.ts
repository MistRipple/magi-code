export interface BrowserCaptureSize {
  width: number;
  height: number;
}

export interface BrowserCaptureClip {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * 将 CDP 页面 CSS 坐标映射到 WebContentsView 的原生 DIP 坐标。
 * auto 视口的 scale 始终为 1；fixed 视口使用 Chromium Emulation 的
 * compositor scale，因此标记截图必须使用相同的比例，不能直接裁切原生 View。
 */
export function mapBrowserCaptureClipToNativeRect(
  nativeSize: BrowserCaptureSize,
  clip: BrowserCaptureClip | null,
  compositorScale: number,
): BrowserCaptureClip {
  const nativeWidth = positiveDimension(nativeSize.width, "width");
  const nativeHeight = positiveDimension(nativeSize.height, "height");
  const scale = normalizedScale(compositorScale);
  if (!clip) return { x: 0, y: 0, width: nativeWidth, height: nativeHeight };

  const x = finiteNumber(clip.x, "x") * scale;
  const y = finiteNumber(clip.y, "y") * scale;
  const width = positiveDimension(clip.width, "clip.width") * scale;
  const height = positiveDimension(clip.height, "clip.height") * scale;
  const left = Math.max(0, Math.min(nativeWidth - 1, Math.floor(x)));
  const top = Math.max(0, Math.min(nativeHeight - 1, Math.floor(y)));
  return {
    x: left,
    y: top,
    width: Math.max(1, Math.min(nativeWidth - left, Math.ceil(width))),
    height: Math.max(1, Math.min(nativeHeight - top, Math.ceil(height))),
  };
}

function normalizedScale(value: number): number {
  if (Number.isFinite(value) && value > 0) return value;
  throw new Error("browser_capture_scale_invalid");
}

function finiteNumber(value: number, name: string): number {
  if (Number.isFinite(value)) return value;
  throw new Error(`browser_capture_dimension_invalid:${name}`);
}

function positiveDimension(value: number, name: string): number {
  const dimension = Math.floor(value);
  if (Number.isFinite(value) && value > 0 && dimension > 0) return dimension;
  throw new Error(`browser_capture_dimension_invalid:${name}`);
}
