type ContextAction = () => void | Promise<void>;

export type DesktopFileScope = {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
};

export type DesktopContextMenuDescriptor =
  | {
      kind: 'link';
      url: string;
      open: ContextAction;
    }
  | {
      kind: 'file';
      filePath: string;
      open?: ContextAction;
      fileScope?: DesktopFileScope;
    }
  | {
      kind: 'code';
      content: string;
      filePath?: string;
      openFile?: ContextAction;
      fileScope?: DesktopFileScope;
    }
  | {
      kind: 'image';
      source?: string;
      filePath?: string;
      open?: ContextAction;
      fileScope?: DesktopFileScope;
    };

export type DesktopContextMenuKind =
  | 'editable'
  | 'readonly'
  | 'selection'
  | 'link'
  | 'link-selection'
  | 'file'
  | 'file-copy'
  | 'code'
  | 'code-selection'
  | 'code-file'
  | 'code-file-selection'
  | 'image-open'
  | 'image-source'
  | 'image-file';

export type DesktopContextMenuRequest = {
  kind: DesktopContextMenuKind;
  descriptor: DesktopContextMenuDescriptor | null;
};

const descriptors = new WeakMap<HTMLElement, DesktopContextMenuDescriptor>();
const EDITABLE_INPUT_TYPES = new Set(['', 'email', 'number', 'password', 'search', 'tel', 'text', 'url']);

export function desktopContextMenu(
  node: HTMLElement,
  descriptor: DesktopContextMenuDescriptor,
): { update: (next: DesktopContextMenuDescriptor) => void; destroy: () => void } {
  descriptors.set(node, descriptor);
  return {
    update(next) {
      descriptors.set(node, next);
    },
    destroy() {
      descriptors.delete(node);
    },
  };
}

function textEditingModeFromPath(path: EventTarget[]): 'editable' | 'readonly' | null {
  for (const target of path) {
    if (!(target instanceof HTMLElement)) continue;
    if (target instanceof HTMLTextAreaElement) {
      if (target.disabled) return null;
      return target.readOnly ? 'readonly' : 'editable';
    }
    if (target instanceof HTMLInputElement) {
      if (!EDITABLE_INPUT_TYPES.has(target.type) || target.disabled) return null;
      return target.readOnly ? 'readonly' : 'editable';
    }
    if (target.isContentEditable) {
      return 'editable';
    }
  }
  return null;
}

function descriptorFromPath(path: EventTarget[]): DesktopContextMenuDescriptor | null {
  for (const target of path) {
    if (!(target instanceof HTMLElement)) continue;
    const descriptor = descriptors.get(target);
    if (descriptor) return descriptor;
  }
  return null;
}

function hasTextSelection(target: EventTarget | null): boolean {
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
    return typeof target.selectionStart === 'number'
      && typeof target.selectionEnd === 'number'
      && target.selectionEnd > target.selectionStart;
  }
  const selection = window.getSelection();
  return Boolean(
    selection
    && !selection.isCollapsed
    && selection.toString().trim()
    && target instanceof Node
    && selection.containsNode(target, true),
  );
}

export function resolveDesktopContextMenuRequest(event: MouseEvent): DesktopContextMenuRequest | null {
  const path = event.composedPath();
  const textEditingMode = textEditingModeFromPath(path);
  if (textEditingMode) {
    return { kind: textEditingMode, descriptor: null };
  }

  const descriptor = descriptorFromPath(path);
  const selection = hasTextSelection(event.target);
  if (!descriptor) {
    return selection ? { kind: 'selection', descriptor: null } : null;
  }

  if (descriptor.kind === 'link') {
    return { kind: selection ? 'link-selection' : 'link', descriptor };
  }
  if (descriptor.kind === 'file') {
    return { kind: descriptor.open ? 'file' : 'file-copy', descriptor };
  }
  if (descriptor.kind === 'image') {
    if (descriptor.filePath) return { kind: 'image-file', descriptor };
    if (descriptor.source) return { kind: 'image-source', descriptor };
    return descriptor.open ? { kind: 'image-open', descriptor } : null;
  }

  const fileSuffix = descriptor.filePath ? '-file' : '';
  const selectionSuffix = selection ? '-selection' : '';
  return {
    kind: `code${fileSuffix}${selectionSuffix}` as DesktopContextMenuKind,
    descriptor,
  };
}
