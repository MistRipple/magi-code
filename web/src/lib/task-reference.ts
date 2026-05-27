import type { IconName } from './icons';
import type { ClientBridgeMessage } from '../shared/bridges/client-bridge';
import {
  dispatchFilePreviewEvent,
  normalizeFileReferenceTarget,
} from './file-reference';

export type TaskReferenceActionKind = 'file' | 'diff' | 'copy';

export interface TaskReferenceDescriptor {
  raw: string;
  displayLabel: string;
  title: string;
  actionKind: TaskReferenceActionKind;
  actionTarget: string;
}

export interface TaskReferenceActionRuntime {
  sessionId?: string | null;
  postMessage: (message: ClientBridgeMessage) => void;
  writeClipboard: (text: string) => Promise<void>;
  onCopySuccess?: () => void;
  onCopyFailure?: () => void;
}

interface ParsedTaskReference {
  displayLabel: string;
  actionTarget: string;
  actionKind: TaskReferenceActionKind;
  title: string;
}

function normalizeReference(ref: string): string {
  return typeof ref === 'string' ? ref.trim() : '';
}

function shortenMiddle(value: string, maxLength = 56): string {
  if (value.length <= maxLength) return value;
  const head = Math.max(12, Math.floor((maxLength - 1) / 2));
  const tail = Math.max(12, maxLength - head - 1);
  return `${value.slice(0, head)}…${value.slice(-tail)}`;
}

function stripKnownPrefix(value: string, prefix: string): string {
  return value.slice(prefix.length).trim();
}

function parseEvidenceReference(reference: string): ParsedTaskReference | null {
  if (!reference.startsWith('evidence://')) {
    return null;
  }
  try {
    const url = new URL(reference);
    const pathParts = url.pathname.split('/').filter(Boolean);
    const taskId = pathParts[0] || url.hostname || 'task';
    const outputIndex = pathParts.includes('output')
      ? pathParts[pathParts.indexOf('output') + 1]
      : pathParts[pathParts.length - 1] || '0';
    const underlyingRef = url.searchParams.get('ref')?.trim() || '';
    const resolved = describeTaskReference(underlyingRef, 'auto');
    return {
      displayLabel: `证据 · ${taskId} / 输出 ${outputIndex}`,
      actionTarget: resolved?.actionTarget || underlyingRef || reference,
      actionKind: resolved?.actionKind || 'copy',
      title: resolved
        ? `${reference}\n关联引用：${resolved.raw}`
        : reference,
    };
  } catch {
    return null;
  }
}

function parsePrefixedReference(reference: string): ParsedTaskReference | null {
  if (reference.startsWith('file:')) {
    const stripped = stripKnownPrefix(reference, 'file:');
    const filePath = normalizeFileReferenceTarget(stripped) ?? stripped;
    return {
      displayLabel: shortenMiddle(filePath),
      actionTarget: filePath,
      actionKind: 'file',
      title: filePath,
    };
  }
  if (reference.startsWith('diff:')) {
    const stripped = stripKnownPrefix(reference, 'diff:');
    const filePath = normalizeFileReferenceTarget(stripped) ?? stripped;
    return {
      displayLabel: shortenMiddle(filePath),
      actionTarget: filePath,
      actionKind: 'diff',
      title: filePath,
    };
  }
  return null;
}

export function describeTaskReference(
  ref: string,
  preferredAction: 'auto' | 'file' | 'diff' = 'auto',
): TaskReferenceDescriptor | null {
  const normalized = normalizeReference(ref);
  if (!normalized) return null;

  const evidenceReference = parseEvidenceReference(normalized);
  if (evidenceReference) {
    const nested = evidenceReference.actionTarget !== normalized
      ? describeTaskReference(evidenceReference.actionTarget, preferredAction)
      : null;
    if (nested) {
      return {
        raw: normalized,
        displayLabel: evidenceReference.displayLabel,
        title: evidenceReference.title,
        actionKind: nested.actionKind,
        actionTarget: nested.actionTarget,
      };
    }
    return {
      raw: normalized,
      ...evidenceReference,
    };
  }

  const prefixedReference = parsePrefixedReference(normalized);
  if (prefixedReference) {
    const actionKind = preferredAction === 'auto'
      ? prefixedReference.actionKind
      : preferredAction;
    return {
      raw: normalized,
      displayLabel: prefixedReference.displayLabel,
      title: prefixedReference.title,
      actionKind,
      actionTarget: prefixedReference.actionTarget,
    };
  }

  const fileTarget = normalizeFileReferenceTarget(normalized);
  if (fileTarget) {
    return {
      raw: normalized,
      displayLabel: shortenMiddle(fileTarget),
      title: fileTarget,
      actionKind: preferredAction === 'diff' ? 'diff' : 'file',
      actionTarget: fileTarget,
    };
  }

  return {
    raw: normalized,
    displayLabel: shortenMiddle(normalized),
    title: normalized,
    actionKind: 'copy',
    actionTarget: normalized,
  };
}

export function getTaskReferenceIconName(reference: TaskReferenceDescriptor): IconName {
  if (reference.actionKind === 'diff') return 'file-edit';
  if (reference.actionKind === 'file') return 'file-text';
  return 'copy';
}

export function getTaskReferenceActionLabel(reference: TaskReferenceDescriptor): string {
  if (reference.actionKind === 'diff') return '查看变更';
  if (reference.actionKind === 'file') return '打开文件';
  return '复制引用';
}

export async function executeTaskReferenceAction(
  reference: TaskReferenceDescriptor,
  runtime: TaskReferenceActionRuntime,
): Promise<void> {
  const sessionId = runtime.sessionId || undefined;
  if (reference.actionKind === 'diff') {
    runtime.postMessage({
      type: 'viewDiff',
      filePath: reference.actionTarget,
      sessionId,
    });
    return;
  }
  if (reference.actionKind === 'file') {
    if (dispatchFilePreviewEvent({ filepath: reference.actionTarget })) {
      return;
    }
    runtime.postMessage({
      type: 'openFile',
      filepath: reference.actionTarget,
      sessionId,
    });
    return;
  }
  try {
    await runtime.writeClipboard(reference.actionTarget);
    runtime.onCopySuccess?.();
  } catch {
    runtime.onCopyFailure?.();
  }
}
