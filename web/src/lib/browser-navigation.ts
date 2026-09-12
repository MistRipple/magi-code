import { normalizeExternalWebUrl } from './external-link';
import { isHtmlFile } from './file-preview-utils';

export const OPEN_URL_IN_BROWSER_EVENT = 'magi:openUrlInBrowser';
export const OPEN_HTML_FILE_IN_BROWSER_EVENT = 'magi:openHtmlFileInBrowser';

export interface OpenUrlInBrowserRequest {
  url: string;
}

export interface OpenHtmlFileInBrowserRequest {
  filepath: string;
}

/** 只有真实 Electron App Renderer 才具备工作区 HTML 内嵌浏览器能力。 */
export function canOpenHtmlFileInMagiBrowser(): boolean {
  if (typeof window === 'undefined') return false;
  const desktop = window.magiDesktop;
  const surface = desktop?.surface
    ?? new URLSearchParams(window.location.search).get('desktopSurface');
  return desktop?.runtime === 'electron' && surface === 'app';
}

export function requestOpenUrlInBrowser(rawUrl: string): boolean {
  const url = normalizeExternalWebUrl(rawUrl);
  if (!url) return false;
  window.dispatchEvent(new CustomEvent<OpenUrlInBrowserRequest>(OPEN_URL_IN_BROWSER_EVENT, {
    detail: { url },
  }));
  return true;
}

/** 请求桌面端把工作区 HTML 文件作为可运行网页交给内置 Chromium。 */
export function requestOpenHtmlFileInBrowser(rawFilepath: string): boolean {
  if (!canOpenHtmlFileInMagiBrowser()) return false;
  const filepath = rawFilepath.trim();
  if (!filepath || !isHtmlFile(filepath)) return false;
  window.dispatchEvent(new CustomEvent<OpenHtmlFileInBrowserRequest>(OPEN_HTML_FILE_IN_BROWSER_EVENT, {
    detail: { filepath },
  }));
  return true;
}
