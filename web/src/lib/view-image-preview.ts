import { parseToolIdentity } from './tool-identity';

export interface ViewImagePreview {
  src: string;
  path: string;
  mime: string;
  bytes: number | undefined;
}

export function isViewImageTool(toolName: string): boolean {
  const parsed = parseToolIdentity(toolName);
  return parsed.baseName === 'view_image';
}

export function parseViewImagePreview(toolName: string, content: unknown): ViewImagePreview | null {
  if (!isViewImageTool(toolName)) return null;
  const payload = parseJsonObjectValue(content);
  if (!payload) return null;
  const modelContent = Array.isArray(payload.model_content) ? payload.model_content : [];
  for (const item of modelContent) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) continue;
    const source = (item as Record<string, unknown>).source;
    if (!source || typeof source !== 'object' || Array.isArray(source)) continue;
    const sourceObj = source as Record<string, unknown>;
    const mediaType = typeof sourceObj.media_type === 'string'
      ? sourceObj.media_type
      : typeof sourceObj.mediaType === 'string'
        ? sourceObj.mediaType
        : '';
    const data = typeof sourceObj.data === 'string' ? sourceObj.data : '';
    if (!mediaType.startsWith('image/') || !data.trim()) continue;
    const path = typeof payload.path === 'string' ? payload.path : '';
    const bytes = typeof payload.bytes === 'number' ? payload.bytes : undefined;
    return {
      src: `data:${mediaType};base64,${data}`,
      path,
      mime: mediaType,
      bytes,
    };
  }
  return null;
}

export function formatViewImageToolOutput(toolName: string, content: unknown): string | null {
  if (!isViewImageTool(toolName)) {
    return null;
  }
  const payload = parseJsonObjectValue(content);
  if (!payload) {
    return null;
  }
  return JSON.stringify(omitLargeImageData(payload), null, 2);
}

function parseJsonObjectValue(content: unknown): Record<string, unknown> | null {
  if (content && typeof content === 'object' && !Array.isArray(content)) {
    return content as Record<string, unknown>;
  }
  if (typeof content !== 'string') {
    return null;
  }
  const trimmed = content.trim();
  if (!trimmed.startsWith('{')) {
    return null;
  }
  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

function omitLargeImageData(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(omitLargeImageData);
  }
  if (!value || typeof value !== 'object') {
    return value;
  }
  const object = value as Record<string, unknown>;
  const next: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(object)) {
    if (key === 'data' && typeof child === 'string' && child.length > 256) {
      next[key] = `[base64 image data omitted: ${child.length} chars]`;
    } else {
      next[key] = omitLargeImageData(child);
    }
  }
  return next;
}
