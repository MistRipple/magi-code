import type { ContentBlock } from '../types/message';
import {
  coerceToolArgumentsRecord,
  normalizeToolDisplayText,
  resolveToolCardTarget,
} from './tool-call-display';

interface FileChangeProjectionInput {
  blockIdBase: string;
  sessionId: string;
  toolName: string;
  arguments: unknown;
  result?: unknown;
  status: 'pending' | 'running' | 'success' | 'error';
}

interface TextPatch {
  oldText: string;
  newText: string;
}

interface ApplyPatchOperation {
  path: string;
  oldPath?: string;
  changeType: 'create' | 'modify' | 'delete' | 'rename';
  diffLines: string[];
}

const FILE_MUTATION_TOOLS = new Set([
  'file_write',
  'file_create',
  'file_patch',
  'file_edit',
  'file_insert',
  'apply_patch',
  'file_remove',
]);

export function buildCanonicalToolFileChangeBlocks(input: FileChangeProjectionInput): ContentBlock[] {
  if (input.status !== 'success' || !FILE_MUTATION_TOOLS.has(input.toolName)) {
    return [];
  }
  if (input.toolName === 'apply_patch') {
    return buildApplyPatchFileChangeBlocks(input);
  }

  const args = coerceToolArgumentsRecord(input.arguments);
  const result = parseRecord(input.result);
  const target = resolveToolCardTarget({
    toolName: input.toolName,
    input: args,
    output: input.result,
  });
  const filePath = target.primaryPath || target.paths[0] || '';
  if (!filePath) {
    return [];
  }

  if (input.toolName === 'file_patch' || input.toolName === 'file_edit' || input.toolName === 'file_insert') {
    const patches = textPatchesFromFilePatchArgs(args);
    return [fileChangeBlock(input, 0, {
      filePath,
      changeType: 'modify',
      diff: buildTextPatchDiff(filePath, patches),
      additions: patches.reduce((total, patch) => total + countPatchLines(patch.newText), 0),
      deletions: patches.reduce((total, patch) => total + countPatchLines(patch.oldText), 0),
    })];
  }

  if (input.toolName === 'file_write' || input.toolName === 'file_create') {
    const content = firstString(args.content, args.text, args.data);
    const created = readBoolean(result?.created) ?? input.toolName === 'file_create';
    const additions = content ? countPatchLines(content) : 0;
    return [fileChangeBlock(input, 0, {
      filePath,
      changeType: created ? 'create' : 'modify',
      diff: created && content ? buildTextPatchDiff(filePath, [{ oldText: '', newText: content }]) : '',
      additions,
      deletions: 0,
    })];
  }

  if (input.toolName === 'file_remove') {
    return [fileChangeBlock(input, 0, {
      filePath,
      changeType: 'delete',
      diff: '',
      additions: 0,
      deletions: 0,
    })];
  }

  return [];
}

function buildApplyPatchFileChangeBlocks(input: FileChangeProjectionInput): ContentBlock[] {
  const args = coerceToolArgumentsRecord(input.arguments);
  const patchText = firstString(args.patch, args.input, args.text);
  const operations = parseApplyPatchOperations(patchText);
  if (operations.length === 0) {
    const target = resolveToolCardTarget({
      toolName: input.toolName,
      input: args,
      output: input.result,
    });
    return target.paths.map((filePath, index) => fileChangeBlock(input, index, {
      filePath,
      changeType: 'modify',
      diff: '',
      additions: 0,
      deletions: 0,
    }));
  }

  return operations.map((operation, index) => fileChangeBlock(input, index, {
    filePath: operation.path,
    oldPath: operation.oldPath,
    changeType: operation.changeType,
    diff: buildApplyPatchOperationDiff(operation),
    additions: operation.diffLines.filter((line) => line.startsWith('+')).length,
    deletions: operation.diffLines.filter((line) => line.startsWith('-')).length,
  }));
}

function fileChangeBlock(
  input: FileChangeProjectionInput,
  index: number,
  change: NonNullable<ContentBlock['fileChange']>,
): ContentBlock {
  return {
    id: `${input.blockIdBase}:file_change:${index}:${change.filePath}`,
    type: 'file_change',
    content: '',
    fileChange: {
      sessionId: input.sessionId,
      toolCallId: input.blockIdBase,
      contentKind: 'text',
      ...change,
    },
  };
}

function textPatchesFromFilePatchArgs(args: Record<string, unknown>): TextPatch[] {
  const patches = Array.isArray(args.patches)
    ? args.patches
      .filter((patch): patch is Record<string, unknown> => (
        Boolean(patch)
        && typeof patch === 'object'
        && !Array.isArray(patch)
      ))
      .map((patch) => ({
        oldText: firstString(patch.old_string, patch.old),
        newText: firstString(patch.new_string, patch.new),
      }))
      .filter((patch) => patch.oldText || patch.newText)
    : [];
  if (patches.length > 0) {
    return patches;
  }
  const oldText = firstString(args.old_string, args.old);
  const newText = firstString(args.new_string, args.new);
  return oldText || newText ? [{ oldText, newText }] : [];
}

function buildTextPatchDiff(filePath: string, patches: TextPatch[]): string {
  if (patches.length === 0) {
    return '';
  }
  return [
    `--- a/${filePath}`,
    `+++ b/${filePath}`,
    ...patches.flatMap((patch) => [
      '@@',
      ...prefixDiffLines('-', patch.oldText),
      ...prefixDiffLines('+', patch.newText),
    ]),
  ].join('\n');
}

function parseApplyPatchOperations(patchText: string): ApplyPatchOperation[] {
  if (!patchText) {
    return [];
  }
  const operations: ApplyPatchOperation[] = [];
  let current: ApplyPatchOperation | null = null;
  for (const line of patchText.split(/\r?\n/u)) {
    const header = line.match(/^\*\*\* (Add|Delete|Update) File:\s+(.+)$/u);
    if (header) {
      if (current) {
        operations.push(current);
      }
      const kind = header[1];
      current = {
        path: normalizeToolDisplayText(header[2]),
        changeType: kind === 'Add' ? 'create' : kind === 'Delete' ? 'delete' : 'modify',
        diffLines: [],
      };
      continue;
    }
    const moveTo = line.match(/^\*\*\* Move to:\s+(.+)$/u);
    if (moveTo && current) {
      current.oldPath = current.path;
      current.path = normalizeToolDisplayText(moveTo[1]);
      current.changeType = 'rename';
      continue;
    }
    if (!current || line.startsWith('*** Begin Patch') || line.startsWith('*** End Patch')) {
      continue;
    }
    if (line.startsWith('@@')) {
      continue;
    }
    if (line.startsWith('+') || line.startsWith('-') || line.startsWith(' ')) {
      current.diffLines.push(line);
    }
  }
  if (current) {
    operations.push(current);
  }
  return operations.filter((operation) => operation.path);
}

function buildApplyPatchOperationDiff(operation: ApplyPatchOperation): string {
  if (operation.diffLines.length === 0) {
    return '';
  }
  const oldPath = operation.oldPath || operation.path;
  return [
    `--- a/${oldPath}`,
    `+++ b/${operation.path}`,
    '@@',
    ...operation.diffLines,
  ].join('\n');
}

function prefixDiffLines(prefix: '+' | '-', text: string): string[] {
  if (!text) {
    return [];
  }
  const lines = text.replace(/\n$/u, '').split('\n');
  return lines.map((line) => `${prefix}${line}`);
}

function countPatchLines(text: string): number {
  return text ? text.replace(/\n$/u, '').split('\n').length : 0;
}

function parseRecord(value: unknown): Record<string, unknown> | undefined {
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

function firstString(...values: unknown[]): string {
  for (const value of values) {
    const normalized = normalizeToolDisplayText(value);
    if (normalized) return normalized;
  }
  return '';
}

function readBoolean(value: unknown): boolean | undefined {
  return typeof value === 'boolean' ? value : undefined;
}
