import { parseToolIdentity } from './tool-identity';

export interface ImageGenerationPreview {
  path: string;
  mime: string;
  bytes: number | undefined;
  revisedPrompt: string;
}

export function isImageGenerationTool(toolName: string): boolean {
  return parseToolIdentity(toolName).baseName === 'image_generate';
}

export function imageGenerationAspectRatio(input: unknown): string {
  if (!input || typeof input !== 'object' || Array.isArray(input)) return '1 / 1';
  const size = typeof (input as Record<string, unknown>).size === 'string'
    ? (input as Record<string, unknown>).size as string
    : '';
  const match = size.trim().match(/^(\d{2,5})\s*[x×]\s*(\d{2,5})$/i);
  if (!match) return '1 / 1';
  const width = Number(match[1]);
  const height = Number(match[2]);
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    return '1 / 1';
  }
  return `${width} / ${height}`;
}

export function parseImageGenerationPreview(
  toolName: string,
  content: unknown,
): ImageGenerationPreview | null {
  if (!isImageGenerationTool(toolName)) return null;
  const payload = parseJsonObjectValue(content);
  if (!payload || payload.status !== 'succeeded') return null;

  const path = typeof payload.path === 'string' ? payload.path.trim() : '';
  const mime = typeof payload.media_type === 'string'
    ? payload.media_type.trim()
    : typeof payload.mediaType === 'string'
      ? payload.mediaType.trim()
      : '';
  if (!path || !mime.startsWith('image/')) return null;

  return {
    path,
    mime,
    bytes: typeof payload.bytes === 'number' && Number.isFinite(payload.bytes)
      ? payload.bytes
      : undefined,
    revisedPrompt: typeof payload.revised_prompt === 'string'
      ? payload.revised_prompt.trim()
      : typeof payload.revisedPrompt === 'string'
        ? payload.revisedPrompt.trim()
        : '',
  };
}

export function formatImageGenerationToolOutput(toolName: string, content: unknown): string | null {
  if (!isImageGenerationTool(toolName)) return null;
  const preview = parseImageGenerationPreview(toolName, content);
  if (!preview) return null;
  return JSON.stringify({
    path: preview.path,
    media_type: preview.mime,
    bytes: preview.bytes,
    ...(preview.revisedPrompt ? { revised_prompt: preview.revisedPrompt } : {}),
  }, null, 2);
}

function parseJsonObjectValue(content: unknown): Record<string, unknown> | null {
  if (content && typeof content === 'object' && !Array.isArray(content)) {
    return content as Record<string, unknown>;
  }
  if (typeof content !== 'string') return null;
  const trimmed = content.trim();
  if (!trimmed.startsWith('{')) return null;
  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}
