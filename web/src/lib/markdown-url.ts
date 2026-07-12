import {
  defaultSanitizeUrl,
  type SanitizeContext,
} from '@humanspeak/svelte-markdown';
import { normalizeFileReferenceTarget } from './file-reference';

export function sanitizeMarkdownUrl(url: string, context: SanitizeContext): string {
  if (context.type === 'link' && normalizeFileReferenceTarget(url)) {
    return url.trim();
  }
  return defaultSanitizeUrl(url, context);
}
