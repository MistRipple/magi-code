export interface FilePreviewEventDetail {
  filepath: string;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
  contentKind?: import('../types/message').EditContentKind;
  size?: number;
  mime?: string;
  symlinkTarget?: string;
  headSummary?: string;
  tailSummary?: string;
}

export type FilePreviewScope = Pick<FilePreviewEventDetail, 'workspaceId' | 'workspacePath' | 'sessionId'>;
export type FilePreviewScopeReader = () => FilePreviewScope | undefined;
export const FILE_PREVIEW_SCOPE_CONTEXT = Symbol('magi-file-preview-scope');

export type FileReferenceTextSegment =
  | { kind: 'text'; text: string }
  | { kind: 'file'; text: string; target: string };

interface DirectoryTreeEntry {
  depth: number;
  beforeEntry: string;
  entry: string;
  suffix: string;
}

const INLINE_FILE_EXTENSION_VALUES = [
  'bash',
  'bmp',
  'c',
  'cc',
  'cjs',
  'conf',
  'config',
  'cpp',
  'cs',
  'css',
  'csv',
  'cxx',
  'doc',
  'docx',
  'env',
  'fish',
  'gif',
  'go',
  'gql',
  'gradle',
  'graphql',
  'h',
  'hpp',
  'htm',
  'html',
  'ini',
  'java',
  'jpeg',
  'jpg',
  'js',
  'json',
  'jsonc',
  'jsx',
  'kt',
  'kts',
  'less',
  'lock',
  'log',
  'md',
  'mdx',
  'mjs',
  'pdf',
  'php',
  'plist',
  'png',
  'proto',
  'py',
  'rb',
  'rs',
  'sass',
  'scss',
  'sh',
  'sql',
  'svelte',
  'swift',
  'toml',
  'ts',
  'tsx',
  'tsv',
  'txt',
  'vue',
  'xml',
  'webp',
  'xls',
  'xlsx',
  'yaml',
  'yml',
  'zsh',
];

const INLINE_FILE_EXTENSIONS = INLINE_FILE_EXTENSION_VALUES
  .sort((left, right) => right.length - left.length)
  .join('|');

const HIDDEN_FILE_NAMES = [
  'dockerignore',
  'editorconfig',
  'env',
  'eslintignore',
  'eslintrc',
  'gitignore',
  'npmrc',
  'prettierignore',
  'prettierrc',
  'yarnrc',
].join('|');

const KNOWN_EXTENSIONLESS_FILE_NAMES = /^(?:Dockerfile|Makefile|README|LICENSE|NOTICE|CHANGELOG)$/u;
const HIDDEN_FILE_NAME_PATTERN = new RegExp(String.raw`^\.(?:${HIDDEN_FILE_NAMES})$`, 'iu');
const INLINE_FILE_BASENAME_PATTERN = new RegExp(
  String.raw`^[^\\/]+\.(?:${INLINE_FILE_EXTENSIONS})$`,
  'iu',
);
const INLINE_FILE_REFERENCE_PATTERN = new RegExp([
  String.raw`file:\/\/[^\s<>"'()[\]{}]+`,
  String.raw`file:[^\s<>"'()[\]{}]+`,
  String.raw`(?:\.{1,2}[\\/]|\/(?:Users|Volumes|tmp|private|var|opt|home)[\\/]|[a-zA-Z]:[\\/]|[a-zA-Z0-9_.@+-]+[\\/])[^\s<>"'()[\]{}]+`,
  String.raw`[a-zA-Z0-9_@+-][a-zA-Z0-9_.@+-]*\.(?:${INLINE_FILE_EXTENSIONS})(?::\d+(?::\d+)?)?`,
  String.raw`\.(?:${HIDDEN_FILE_NAMES})(?::\d+(?::\d+)?)?`,
  String.raw`(?:Dockerfile|Makefile|README|LICENSE|NOTICE|CHANGELOG)(?::\d+(?::\d+)?)?`,
].join('|'), 'giu');

function trimWrappingAngles(value: string): string {
  const trimmed = value.trim();
  if (trimmed.startsWith('<') && trimmed.endsWith('>')) {
    return trimmed.slice(1, -1).trim();
  }
  return trimmed;
}

function decodePath(value: string): string {
  try {
    return decodeURI(value);
  } catch {
    return value;
  }
}

function removeHashOrQuery(value: string): string {
  const markerIndex = value.search(/[?#]/);
  return markerIndex >= 0 ? value.slice(0, markerIndex) : value;
}

function removeEditorLineSuffix(value: string): string {
  return value.replace(/:\d+(?::\d+)?$/u, '');
}

function hasExternalProtocol(value: string): boolean {
  if (/^file:/i.test(value)) return false;
  if (/^[a-zA-Z]:[\\/]/u.test(value)) return false;
  return /^[a-zA-Z][a-zA-Z0-9+.-]*:/u.test(value);
}

function fileUrlToPath(value: string): string | null {
  try {
    const url = new URL(value);
    if (url.protocol !== 'file:') return null;
    return decodeURIComponent(url.pathname);
  } catch {
    return null;
  }
}

function hasGlobOrPlaceholder(value: string): boolean {
  return /[*?[\]{}]/u.test(value) || value.includes('...');
}

function getBasename(value: string): string {
  const normalized = removeEditorLineSuffix(removeHashOrQuery(value.trim()));
  return normalized.split(/[\\/]/u).pop() || normalized;
}

function hasKnownFileBasename(value: string): boolean {
  const basename = getBasename(value);
  if (KNOWN_EXTENSIONLESS_FILE_NAMES.test(basename)) {
    return true;
  }
  if (HIDDEN_FILE_NAME_PATTERN.test(basename)) {
    return true;
  }
  return INLINE_FILE_BASENAME_PATTERN.test(basename);
}

function isLikelyFilePath(value: string): boolean {
  if (!value) return false;
  if (/^https?:\/\//i.test(value)) return false;
  if (hasExternalProtocol(value)) return false;
  return (
    value.startsWith('./')
    || value.startsWith('../')
    || value.startsWith('/Users/')
    || value.startsWith('/Volumes/')
    || value.startsWith('/tmp/')
    || value.startsWith('/private/')
    || value.startsWith('/var/')
    || value.startsWith('/opt/')
    || value.startsWith('/home/')
    || /^[a-zA-Z]:[\\/]/u.test(value)
    || hasKnownFileBasename(value)
    || /[\\/][^\\/]+\.[a-zA-Z0-9]{1,12}$/u.test(value)
    || /^[^\\/]+\.[a-zA-Z0-9]{1,12}$/u.test(value)
  );
}

function splitTrailingSentencePunctuation(value: string): { candidate: string; suffix: string } {
  let candidate = value;
  let suffix = '';
  while (/[，。；：、,.;:!?！？]/u.test(candidate.at(-1) || '')) {
    suffix = `${candidate.at(-1)}${suffix}`;
    candidate = candidate.slice(0, -1);
  }
  return { candidate, suffix };
}

function isUnsafeInlineMatchContext(text: string, index: number): boolean {
  const previous = index > 0 ? text[index - 1] : '';
  const prefix = text.slice(Math.max(0, index - 3), index);
  return (
    prefix === '://'
    || prefix.endsWith('//')
    || /[\w@.-]/u.test(previous)
  );
}

export function normalizeFileReferenceTarget(reference: string): string | null {
  let value = trimWrappingAngles(reference);
  if (!value || value.startsWith('#')) return null;

  if (/^file:\/\//i.test(value)) {
    value = fileUrlToPath(value) ?? '';
  } else if (/^file:/i.test(value)) {
    value = value.slice('file:'.length).trim();
  } else if (hasExternalProtocol(value)) {
    return null;
  }

  value = removeEditorLineSuffix(removeHashOrQuery(decodePath(value)).trim());
  if (hasGlobOrPlaceholder(value)) return null;
  if (!isLikelyFilePath(value)) return null;
  return value;
}

export function normalizeInlineFileReferenceTarget(reference: string): string | null {
  const target = normalizeFileReferenceTarget(reference);
  if (!target) return null;
  if (hasKnownFileBasename(target)) return target;
  if (/[\\/]/u.test(target) && /\.[a-zA-Z0-9]{1,12}$/u.test(getBasename(target))) {
    return target;
  }
  return null;
}

/**
 * 隐式识别只能接受带确定基准的路径。裸文件名可能属于工作区、Skill 或工具目录，
 * 在没有明确目录时生成链接会打开错误文件，甚至把不存在的路径呈现为可用操作。
 */
export function normalizeImplicitFileReferenceTarget(reference: string): string | null {
  const target = normalizeInlineFileReferenceTarget(reference);
  if (!target) return null;
  return (
    target.startsWith('./')
    || target.startsWith('../')
    || /^(?:[a-zA-Z]:[\\/]|[\\/])/u.test(target)
    || /[\\/]/u.test(target)
  ) ? target : null;
}

export function isLikelyFileReference(reference: string): boolean {
  return normalizeFileReferenceTarget(reference) !== null;
}

export function splitFileReferenceText(text: string): FileReferenceTextSegment[] {
  if (!text) {
    return [];
  }

  const segments: FileReferenceTextSegment[] = [];
  let cursor = 0;

  for (const match of text.matchAll(INLINE_FILE_REFERENCE_PATTERN)) {
    const rawMatch = match[0];
    const index = match.index ?? 0;
    if (!rawMatch || index < cursor || isUnsafeInlineMatchContext(text, index)) {
      continue;
    }

    const { candidate, suffix } = splitTrailingSentencePunctuation(rawMatch);
    const target = normalizeImplicitFileReferenceTarget(candidate);
    if (!target) {
      continue;
    }

    if (index > cursor) {
      segments.push({ kind: 'text', text: text.slice(cursor, index) });
    }
    segments.push({ kind: 'file', text: candidate, target });
    if (suffix) {
      segments.push({ kind: 'text', text: suffix });
    }
    cursor = index + rawMatch.length;
  }

  if (cursor < text.length) {
    segments.push({ kind: 'text', text: text.slice(cursor) });
  }
  return segments.length > 0 ? segments : [{ kind: 'text', text }];
}

function splitLineEnding(line: string): { content: string; lineEnding: string } {
  if (line.endsWith('\r\n')) {
    return { content: line.slice(0, -2), lineEnding: '\r\n' };
  }
  if (line.endsWith('\n')) {
    return { content: line.slice(0, -1), lineEnding: '\n' };
  }
  return { content: line, lineEnding: '' };
}

function parseDirectoryTreeEntry(line: string): DirectoryTreeEntry | null {
  const match = line.match(/^(\s*(?:(?:[│|]\s*)|\s{2,})*)(├──|└──|\+--|`--|\|--)(\s*)(.+)$/u);
  if (!match) {
    return null;
  }

  const leading = match[1] ?? '';
  const connector = match[2] ?? '';
  const separator = match[3] ?? '';
  const rest = match[4] ?? '';
  const commentIndex = rest.search(/\s+#/u);
  const entryPart = (commentIndex >= 0 ? rest.slice(0, commentIndex) : rest).trimEnd();
  const suffix = rest.slice(entryPart.length);
  const pipeDepth = (leading.match(/[│|]/gu) ?? []).length;
  const spaceDepth = Math.floor(leading.length / 4);
  const depth = Math.max(pipeDepth, spaceDepth);

  return {
    depth,
    beforeEntry: `${leading}${connector}${separator}`,
    entry: entryPart.trim(),
    suffix,
  };
}

function normalizeDirectoryEntry(entry: string): string {
  const normalized = entry.trim().replace(/[\\/]+$/u, '');
  if (!normalized || normalized === '.' || normalized === '..') {
    return '';
  }
  if (hasGlobOrPlaceholder(normalized)) {
    return '';
  }
  return normalized;
}

function resolveTreeFileTarget(entry: string, directoryStack: string[], depth: number): string | null {
  if (!entry || entry.endsWith('/') || hasGlobOrPlaceholder(entry)) {
    return null;
  }

  const directTarget = normalizeInlineFileReferenceTarget(entry);
  if (!directTarget) {
    return null;
  }
  if (/[\\/]/u.test(directTarget) || /^(?:[a-zA-Z]:[\\/]|\/)/u.test(directTarget)) {
    return directTarget;
  }

  const parentPath = directoryStack.slice(0, depth).filter(Boolean).join('/');
  return parentPath ? `${parentPath}/${directTarget}` : directTarget;
}

export function splitPlainTextFileReferenceText(text: string): FileReferenceTextSegment[] {
  if (!text) {
    return [];
  }

  const segments: FileReferenceTextSegment[] = [];
  const directoryStack: string[] = [];
  const lines = text.match(/[^\n]*(?:\n|$)/g)?.filter((line) => line.length > 0) ?? [];

  for (const rawLine of lines) {
    const { content: line, lineEnding } = splitLineEnding(rawLine);
    const treeEntry = parseDirectoryTreeEntry(line);
    if (!treeEntry) {
      segments.push(...splitFileReferenceText(line));
      if (lineEnding) {
        segments.push({ kind: 'text', text: lineEnding });
      }
      continue;
    }

    if (treeEntry.entry.endsWith('/')) {
      const directoryName = normalizeDirectoryEntry(treeEntry.entry);
      if (directoryName) {
        directoryStack.length = treeEntry.depth;
        directoryStack[treeEntry.depth] = directoryName;
      }
      segments.push({ kind: 'text', text: line });
      if (lineEnding) {
        segments.push({ kind: 'text', text: lineEnding });
      }
      continue;
    }

    const target = resolveTreeFileTarget(treeEntry.entry, directoryStack, treeEntry.depth);
    if (!target) {
      segments.push({ kind: 'text', text: line });
      if (lineEnding) {
        segments.push({ kind: 'text', text: lineEnding });
      }
      continue;
    }

    segments.push({ kind: 'text', text: treeEntry.beforeEntry });
    segments.push({ kind: 'file', text: treeEntry.entry, target });
    if (treeEntry.suffix) {
      segments.push({ kind: 'text', text: treeEntry.suffix });
    }
    if (lineEnding) {
      segments.push({ kind: 'text', text: lineEnding });
    }
  }

  return segments.length > 0 ? segments : [{ kind: 'text', text }];
}

export function dispatchFilePreviewEvent(detail: FilePreviewEventDetail): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  const filepath = detail.filepath.trim();
  if (!filepath) {
    return false;
  }
  const event = new CustomEvent('magi:previewFile', {
    detail: { ...detail, filepath },
    cancelable: true,
  });
  window.dispatchEvent(event);
  return event.defaultPrevented;
}
