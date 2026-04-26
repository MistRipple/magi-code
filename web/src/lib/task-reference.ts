export type TaskReferenceActionKind = 'file' | 'diff' | 'copy';

export interface TaskReferenceDescriptor {
  raw: string;
  displayLabel: string;
  title: string;
  actionKind: TaskReferenceActionKind;
  actionTarget: string;
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

function isLikelyFilePath(value: string): boolean {
  if (!value) return false;
  if (/^https?:\/\//i.test(value)) return false;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(value)) return false;
  return (
    value.startsWith('./')
    || value.startsWith('../')
    || value.startsWith('/')
    || /^[a-zA-Z]:[\\/]/.test(value)
    || /[\\/]/.test(value)
    || /\.[a-zA-Z0-9]{1,6}$/.test(value)
  );
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
    const filePath = stripKnownPrefix(reference, 'file:');
    return {
      displayLabel: shortenMiddle(filePath),
      actionTarget: filePath,
      actionKind: 'file',
      title: filePath,
    };
  }
  if (reference.startsWith('diff:')) {
    const filePath = stripKnownPrefix(reference, 'diff:');
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

  if (isLikelyFilePath(normalized)) {
    return {
      raw: normalized,
      displayLabel: shortenMiddle(normalized),
      title: normalized,
      actionKind: preferredAction === 'diff' ? 'diff' : 'file',
      actionTarget: normalized,
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
