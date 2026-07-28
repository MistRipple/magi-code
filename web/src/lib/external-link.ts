import { isDesktopRuntime } from './desktop-updater';

const EXTERNAL_WEB_PROTOCOLS = new Set(['http:', 'https:']);

export function normalizeExternalWebUrl(rawUrl: string): string | null {
  const candidate = rawUrl.trim();
  if (!candidate) {
    return null;
  }
  try {
    const url = new URL(candidate);
    return EXTERNAL_WEB_PROTOCOLS.has(url.protocol) ? url.toString() : null;
  } catch {
    return null;
  }
}

export async function openExternalWebUrl(rawUrl: string): Promise<void> {
  const url = normalizeExternalWebUrl(rawUrl);
  if (!url) {
    throw new Error('仅支持打开 HTTP 或 HTTPS 网页');
  }

  if (isDesktopRuntime()) {
    const { openUrl } = await import('@tauri-apps/plugin-opener');
    await openUrl(url);
    return;
  }

  window.open(url, '_blank', 'noopener,noreferrer');
}
