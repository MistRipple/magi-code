import {
  normalizeFileReferenceTarget,
  type FilePreviewScope,
  type FilePreviewScopeReader,
} from './file-reference';

export interface MarkdownImageContext {
  baseFilePath?: string;
  readFilePreviewScope?: FilePreviewScopeReader;
}

export const MARKDOWN_IMAGE_CONTEXT = Symbol('magi-markdown-image-context');

function normalizeSeparators(value: string): string {
  return value.replace(/\\/g, '/');
}

function normalizePath(value: string): string {
  const normalized = normalizeSeparators(value);
  const isUnc = normalized.startsWith('//');
  const drive = normalized.match(/^([a-zA-Z]):(?:\/|$)/u)?.[1];
  const isAbsolute = isUnc || normalized.startsWith('/') || Boolean(drive);
  const prefix = isUnc ? '//' : drive ? `${drive}:/` : normalized.startsWith('/') ? '/' : '';
  const body = isUnc
    ? normalized.slice(2)
    : drive
      ? normalized.slice(2)
      : normalized.startsWith('/')
        ? normalized.slice(1)
        : normalized;
  const parts: string[] = [];

  for (const part of body.split('/')) {
    if (!part || part === '.') continue;
    if (part === '..') {
      const last = parts.at(-1);
      if (last && last !== '..') {
        parts.pop();
      } else if (!isAbsolute) {
        parts.push(part);
      }
      continue;
    }
    parts.push(part);
  }

  return `${prefix}${parts.join('/')}` || (isAbsolute ? prefix : '.');
}

function isAbsolutePath(value: string): boolean {
  return value.startsWith('/')
    || value.startsWith('\\')
    || value.startsWith('//')
    || /^[a-zA-Z]:[\\/]/u.test(value);
}

/**
 * 将 Markdown 图片引用解析为后端文件 API 可识别的工作区路径。
 * 相对图片以当前 Markdown 文件所在目录为基准；聊天消息没有文件基准时，
 * 保持工作区相对路径交给后端解析。
 */
export function resolveMarkdownImageFilePath(
  reference: string,
  baseFilePath?: string,
): string | null {
  const target = normalizeFileReferenceTarget(reference);
  if (!target) return null;
  if (isAbsolutePath(target) || !baseFilePath?.trim()) {
    return target;
  }

  const base = normalizeSeparators(baseFilePath.trim());
  const lastSlash = base.lastIndexOf('/');
  if (lastSlash < 0) return target;
  return normalizePath(`${base.slice(0, lastSlash)}/${target}`);
}

export function markdownImageScope(
  readScope: FilePreviewScopeReader | undefined,
): FilePreviewScope {
  return readScope?.() ?? {};
}
