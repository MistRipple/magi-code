import { parseToolIdentity } from './tool-identity';

export const INTERNAL_REDACTED_DISPLAY_VALUES = new Set(['[path]', '[redacted]']);

const SINGLE_PATH_KEYS = [
  'path',
  'file_path',
  'filePath',
  'filepath',
  'image_path',
  'imagePath',
  'dir_path',
  'dirPath',
  'target_path',
  'targetPath',
  'input',
];

const CHANGED_PATH_KEYS = [
  'changed_paths',
  'changedPaths',
  'paths',
  'file_paths',
  'filePaths',
  'target_paths',
  'targetPaths',
];

const APPLY_PATCH_TEXT_KEYS = ['patch', 'input', 'text'];
const SINGLE_FILE_TOOLS = new Set([
  'file_view',
  'file_read',
  'view_image',
  'image_view',
  'file_create',
  'file_write',
  'file_edit',
  'file_insert',
  'file_patch',
  'file_mkdir',
]);

export interface ToolCardTarget {
  primaryPath?: string;
  paths: string[];
}

export interface ResolveToolCardTargetOptions {
  toolName: string;
  input: unknown;
  output?: unknown;
  explicitFilepath?: unknown;
  directoryView?: boolean;
}

export function normalizeToolDisplayText(value: unknown): string {
  if (typeof value !== 'string') return '';
  const trimmed = value.trim();
  return INTERNAL_REDACTED_DISPLAY_VALUES.has(trimmed.toLowerCase()) ? '' : trimmed;
}

export function firstToolDisplayText(...values: unknown[]): string {
  for (const value of values) {
    const normalized = normalizeToolDisplayText(value);
    if (normalized) return normalized;
  }
  return '';
}

export function sanitizeToolDisplayPayload(value: unknown): unknown {
  if (typeof value === 'string') {
    return normalizeToolDisplayText(value) || undefined;
  }
  if (Array.isArray(value)) {
    const items = value
      .map((item) => sanitizeToolDisplayPayload(item))
      .filter((item) => item !== undefined);
    return items.length > 0 ? items : undefined;
  }
  if (value && typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>)
      .map(([key, entryValue]) => [key, sanitizeToolDisplayPayload(entryValue)] as const)
      .filter(([, entryValue]) => entryValue !== undefined);
    return entries.length > 0 ? Object.fromEntries(entries) : undefined;
  }
  return value;
}

export function hasInternalRedactedDisplayValue(value: unknown): boolean {
  if (typeof value === 'string') {
    return INTERNAL_REDACTED_DISPLAY_VALUES.has(value.trim().toLowerCase());
  }
  if (Array.isArray(value)) {
    return value.some(hasInternalRedactedDisplayValue);
  }
  if (value && typeof value === 'object') {
    return Object.values(value as Record<string, unknown>).some(hasInternalRedactedDisplayValue);
  }
  return false;
}

export function coerceToolArgumentsRecord(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  const rawInput = normalizeToolDisplayText(value);
  return rawInput ? { input: rawInput } : {};
}

export function resolveToolCardTarget(options: ResolveToolCardTargetOptions): ToolCardTarget {
  const explicitPath = normalizeToolDisplayText(options.explicitFilepath);
  if (explicitPath && !options.directoryView) {
    return { primaryPath: explicitPath, paths: [explicitPath] };
  }

  const parsedTool = parseToolIdentity(options.toolName);
  if (parsedTool.source !== 'builtin') {
    return emptyTarget();
  }

  const toolName = parsedTool.baseName;
  const inputRecord = coerceToolArgumentsRecord(options.input);
  const outputRecord = parseToolPayloadRecord(options.output);

  if (toolName === 'apply_patch') {
    const paths = uniqueStrings([
      ...pathsFromRecord(outputRecord, CHANGED_PATH_KEYS),
      ...pathsFromRecord(inputRecord, CHANGED_PATH_KEYS),
      ...extractApplyPatchPathsFromText(firstRecordText(inputRecord, APPLY_PATCH_TEXT_KEYS)),
    ]);
    return targetFromPaths(paths);
  }

  if (toolName === 'file_copy' || toolName === 'file_move') {
    return emptyTarget();
  }

  if (toolName === 'file_remove') {
    const paths = uniqueStrings([
      ...pathsFromRecord(outputRecord, CHANGED_PATH_KEYS),
      ...pathsFromRecord(inputRecord, CHANGED_PATH_KEYS),
      ...pathsFromRecord(inputRecord, SINGLE_PATH_KEYS),
    ]);
    return targetFromPaths(paths);
  }

  if (isSingleFileTool(toolName) && !options.directoryView) {
    const path = firstPathFromRecord(inputRecord, SINGLE_PATH_KEYS)
      || firstPathFromRecord(outputRecord, SINGLE_PATH_KEYS);
    return path ? { primaryPath: path, paths: [path] } : emptyTarget();
  }

  return emptyTarget();
}

export function extractApplyPatchPathsFromText(text: string): string[] {
  if (!text) return [];
  const paths: string[] = [];
  for (const line of text.split(/\r?\n/u)) {
    const fileMatch = line.match(/^\*\*\* (?:Add|Delete|Update) File:\s+(.+)$/u);
    if (fileMatch) {
      paths.push(fileMatch[1]);
      continue;
    }
    const moveMatch = line.match(/^\*\*\* Move to:\s+(.+)$/u);
    if (moveMatch) {
      paths.push(moveMatch[1]);
    }
  }
  return uniqueStrings(paths.map(cleanPatchPath));
}

function isSingleFileTool(toolName: string): boolean {
  return SINGLE_FILE_TOOLS.has(toolName);
}

function parseToolPayloadRecord(value: unknown): Record<string, unknown> | undefined {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  const text = normalizeToolDisplayText(value);
  if (!text || !text.startsWith('{')) {
    return undefined;
  }
  try {
    const parsed = JSON.parse(text);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : undefined;
  } catch {
    return undefined;
  }
}

function firstRecordText(record: Record<string, unknown> | undefined, keys: readonly string[]): string {
  if (!record) return '';
  for (const key of keys) {
    const value = normalizeToolDisplayText(record[key]);
    if (value) return value;
  }
  return '';
}

function firstPathFromRecord(record: Record<string, unknown> | undefined, keys: readonly string[]): string {
  const paths = pathsFromRecord(record, keys);
  return paths[0] || '';
}

function pathsFromRecord(record: Record<string, unknown> | undefined, keys: readonly string[]): string[] {
  if (!record) return [];
  const paths: string[] = [];
  for (const key of keys) {
    paths.push(...pathsFromValue(record[key]));
  }
  return uniqueStrings(paths);
}

function pathsFromValue(value: unknown): string[] {
  const direct = normalizeToolDisplayText(value);
  if (direct) return [direct];
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => normalizeToolDisplayText(item))
    .filter(Boolean);
}

function targetFromPaths(paths: string[]): ToolCardTarget {
  const uniquePaths = uniqueStrings(paths);
  if (uniquePaths.length === 1) {
    return { primaryPath: uniquePaths[0], paths: uniquePaths };
  }
  return { paths: uniquePaths };
}

function emptyTarget(): ToolCardTarget {
  return { paths: [] };
}

function cleanPatchPath(path: string): string {
  const trimmed = normalizeToolDisplayText(path);
  if (!trimmed || trimmed === '/dev/null') return '';
  return trimmed;
}

function uniqueStrings(values: string[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const value of values) {
    const normalized = normalizeToolDisplayText(value);
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    result.push(normalized);
  }
  return result;
}
