import { i18n } from '../stores/i18n.svelte';
const EXTERNAL_WEB_PROTOCOLS = new Set(['http:', 'https:']);
/**
 * 能交给操作系统默认处理器的协议。
 *
 * 与「能进内置浏览器」是两件事：内置 BrowserSurface 只承载 HTTP(S) 文档，
 * 而 `mailto:` 这类由系统邮件客户端处理的地址仍然应该可用，不能因为
 * 进不了内置浏览器就变成一条点了没反应的死链。
 */
const EXTERNAL_HANDLER_PROTOCOLS = new Set(['http:', 'https:', 'mailto:']);

/** 只接受可交给内置浏览器或系统浏览器的网页地址。 */
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

/** 接受网页与系统处理器协议（含 `mailto:`）；其余协议一律拒绝。 */
export function normalizeExternalHandlerUrl(rawUrl: string): string | null {
  const candidate = rawUrl.trim();
  if (!candidate) {
    return null;
  }
  try {
    const url = new URL(candidate);
    return EXTERNAL_HANDLER_PROTOCOLS.has(url.protocol) ? url.toString() : null;
  } catch {
    return null;
  }
}

export async function openExternalUrl(rawUrl: string): Promise<void> {
  const url = normalizeExternalHandlerUrl(rawUrl);
  if (!url) {
    throw new Error(i18n.t('externalLink.unsupported'));
  }

  if (window.magiDesktop) {
    await window.magiDesktop.openExternal(url);
    return;
  }

  window.open(url, '_blank', 'noopener,noreferrer');
}
