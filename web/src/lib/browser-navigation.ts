import { normalizeExternalWebUrl } from './external-link';

export const OPEN_URL_IN_BROWSER_EVENT = 'magi:openUrlInBrowser';

export interface OpenUrlInBrowserRequest {
  url: string;
}

export function requestOpenUrlInBrowser(rawUrl: string): boolean {
  const url = normalizeExternalWebUrl(rawUrl);
  if (!url) return false;
  window.dispatchEvent(new CustomEvent<OpenUrlInBrowserRequest>(OPEN_URL_IN_BROWSER_EVENT, {
    detail: { url },
  }));
  return true;
}
