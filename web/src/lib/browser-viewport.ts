export type BrowserViewportDeviceType = 'desktop' | 'mobile';

export interface BrowserViewportSize {
  width: number;
  height: number;
}

export interface AutomaticBrowserViewport extends BrowserViewportSize {
  deviceType: BrowserViewportDeviceType;
}

const MIN_BROWSER_WIDTH = 320;
const MAX_BROWSER_WIDTH = 7_680;
const MIN_BROWSER_HEIGHT = 240;
const MAX_BROWSER_HEIGHT = 4_320;
const MOBILE_SURFACE_BREAKPOINT = 600;
const MOBILE_LOGICAL_WIDTH = 390;
const DESKTOP_LOGICAL_WIDTH = 1_280;

export function normalizeBrowserViewportSize(
  width: number,
  height: number,
): BrowserViewportSize {
  return {
    width: Math.min(MAX_BROWSER_WIDTH, Math.max(MIN_BROWSER_WIDTH, Math.round(width))),
    height: Math.min(MAX_BROWSER_HEIGHT, Math.max(MIN_BROWSER_HEIGHT, Math.round(height))),
  };
}

export function automaticBrowserViewport(
  surface: BrowserViewportSize,
): AutomaticBrowserViewport {
  const normalizedSurface = normalizeBrowserViewportSize(surface.width, surface.height);
  const mobile = normalizedSurface.width <= MOBILE_SURFACE_BREAKPOINT;
  const minimumLogicalWidth = mobile ? MOBILE_LOGICAL_WIDTH : DESKTOP_LOGICAL_WIDTH;
  const width = Math.max(normalizedSurface.width, minimumLogicalWidth);
  const height = Math.min(
    MAX_BROWSER_HEIGHT,
    Math.max(
      MIN_BROWSER_HEIGHT,
      Math.round(normalizedSurface.height * width / normalizedSurface.width),
    ),
  );
  return {
    width,
    height,
    deviceType: mobile ? 'mobile' : 'desktop',
  };
}
