import {
  defaultSanitizeUrl,
  type SanitizeContext,
} from '@humanspeak/svelte-markdown';
import { normalizeFileReferenceTarget } from './file-reference';

export function sanitizeMarkdownUrl(url: string, context: SanitizeContext): string {
  const isFileProtocol = /^file:/iu.test(url.trim());
  const isLocalReference = context.type === 'link'
    || (context.type === 'image' && !isFileProtocol);
  if (isLocalReference && normalizeFileReferenceTarget(url)) {
    return url.trim();
  }
  if (
    context.type === 'image'
    && /^data:image\/(?:png|jpeg|gif|webp|avif|bmp);base64,[a-z0-9+/=\s]+$/iu.test(url.trim())
  ) {
    return url.trim();
  }
  return defaultSanitizeUrl(url, context);
}
