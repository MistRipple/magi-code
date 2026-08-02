import { isDesktopRuntime } from './desktop-updater';
import { i18n } from '../stores/i18n.svelte';
import { resolveAgentFileRevealTarget, type AgentFileRevealTarget } from '../web/agent-api';
import type { PredefinedMenuItemOptions } from '@tauri-apps/api/menu';
import {
  resolveDesktopContextMenuRequest,
  type DesktopContextMenuDescriptor,
  type DesktopContextMenuKind,
  type DesktopContextMenuRequest,
} from './desktop-context-menu-contract';

type ContextAction = () => void | Promise<void>;

type TauriMenu = Awaited<ReturnType<typeof import('@tauri-apps/api/menu').Menu.new>>;

const menuCache = new Map<string, Promise<TauriMenu>>();
const REVEAL_TARGET_TIMEOUT_MS = 800;
let activeDescriptor: DesktopContextMenuDescriptor | null = null;
let activeRevealTarget: AgentFileRevealTarget | null = null;
let removeDocumentListener: (() => void) | null = null;
let requestSequence = 0;

function runContextAction(action: ContextAction | undefined, label: string): void {
  if (!action) return;
  void Promise.resolve(action()).catch((error) => {
    console.error(`[DesktopContextMenu] ${label}失败:`, error);
  });
}

function copyContextValue(value: string, label: string): void {
  if (!value) return;
  runContextAction(() => navigator.clipboard.writeText(value), label);
}

function currentDescriptor<T extends DesktopContextMenuDescriptor['kind']>(kind: T): Extract<DesktopContextMenuDescriptor, { kind: T }> | null {
  return activeDescriptor?.kind === kind
    ? activeDescriptor as Extract<DesktopContextMenuDescriptor, { kind: T }>
    : null;
}

function descriptorFileContext(descriptor: DesktopContextMenuDescriptor | null): {
  filePath: string;
  fileScope: NonNullable<Extract<DesktopContextMenuDescriptor, { kind: 'file' | 'code' | 'image' }>['fileScope']>;
} | null {
  if (!descriptor || !['file', 'code', 'image'].includes(descriptor.kind)) return null;
  const fileDescriptor = descriptor as Extract<DesktopContextMenuDescriptor, { kind: 'file' | 'code' | 'image' }>;
  const filePath = fileDescriptor.filePath?.trim() ?? '';
  const fileScope = fileDescriptor.fileScope;
  if (!filePath || !fileScope) return null;
  if (!fileScope.workspaceId?.trim() && !fileScope.workspacePath?.trim()) return null;
  return { filePath, fileScope };
}

function openActiveWorkspaceFolder(): void {
  const workspacePathRef = currentDescriptor('workspace')?.workspacePathRef.trim() ?? '';
  if (!workspacePathRef) return;
  runContextAction(async () => {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('open_workspace_folder', {
      request: { workspaceRootPathRef: workspacePathRef },
    });
  }, i18n.t('contextMenu.openFolder'));
}

async function resolveRevealTarget(
  descriptor: DesktopContextMenuDescriptor | null,
): Promise<AgentFileRevealTarget | null> {
  const context = descriptorFileContext(descriptor);
  if (!context) return null;
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), REVEAL_TARGET_TIMEOUT_MS);
  try {
    return await resolveAgentFileRevealTarget(context.filePath, context.fileScope, controller.signal);
  } catch {
    return null;
  } finally {
    window.clearTimeout(timeout);
  }
}

function revealActiveFile(): void {
  if (!activeRevealTarget) return;
  const request = {
    targetPathRef: activeRevealTarget.targetPathRef,
    workspaceRootPathRef: activeRevealTarget.workspaceRootPathRef,
  };
  runContextAction(async () => {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('reveal_workspace_file', { request });
  }, i18n.t('contextMenu.revealFile'));
}

async function createMenu(kind: DesktopContextMenuKind, revealAvailable: boolean): Promise<TauriMenu> {
  const { Menu, MenuItem, PredefinedMenuItem } = await import('@tauri-apps/api/menu');
  const predefined = (item: PredefinedMenuItemOptions['item'], key: string) =>
    PredefinedMenuItem.new({ item, text: i18n.t(key) });
  const separator = () => PredefinedMenuItem.new({ item: 'Separator' });
  const custom = (key: string, action: () => void) =>
    MenuItem.new({ text: i18n.t(key), action });

  if (kind === 'editable') {
    return Menu.new({
      items: await Promise.all([
        predefined('Undo', 'contextMenu.undo'),
        predefined('Redo', 'contextMenu.redo'),
        separator(),
        predefined('Cut', 'contextMenu.cut'),
        predefined('Copy', 'contextMenu.copy'),
        predefined('Paste', 'contextMenu.paste'),
        separator(),
        predefined('SelectAll', 'contextMenu.selectAll'),
      ]),
    });
  }

  if (kind === 'selection') {
    return Menu.new({ items: [await predefined('Copy', 'contextMenu.copy')] });
  }

  if (kind === 'readonly') {
    return Menu.new({
      items: await Promise.all([
        predefined('Copy', 'contextMenu.copy'),
        separator(),
        predefined('SelectAll', 'contextMenu.selectAll'),
      ]),
    });
  }

  if (kind === 'workspace') {
    return Menu.new({
      items: [await custom('contextMenu.openFolder', openActiveWorkspaceFolder)],
    });
  }

  const items: Array<Awaited<ReturnType<typeof MenuItem.new>> | Awaited<ReturnType<typeof PredefinedMenuItem.new>>> = [];
  if (kind.endsWith('-selection')) {
    items.push(await predefined('Copy', 'contextMenu.copy'));
    items.push(await separator());
  }

  if (kind.startsWith('image')) {
    const openImage = () => {
      const descriptor = currentDescriptor('image');
      runContextAction(descriptor?.open, i18n.t('contextMenu.openImage'));
    };
    if (currentDescriptor('image')?.open || kind !== 'image-open') {
      items.push(await custom('contextMenu.openImage', openImage));
    }
    if (revealAvailable) {
      items.push(await custom('contextMenu.revealFile', revealActiveFile));
    }
    if (kind === 'image-file') {
      items.push(await custom('contextMenu.copyPath', () => {
        copyContextValue(currentDescriptor('image')?.filePath ?? '', i18n.t('contextMenu.copyPath'));
      }));
    } else if (kind === 'image-source') {
      items.push(await custom('contextMenu.copyImageAddress', () => {
        copyContextValue(currentDescriptor('image')?.source ?? '', i18n.t('contextMenu.copyImageAddress'));
      }));
    }
  } else if (kind.startsWith('link')) {
    items.push(await custom('contextMenu.openLink', () => {
      const descriptor = currentDescriptor('link');
      runContextAction(descriptor?.open, i18n.t('contextMenu.openLink'));
    }));
    items.push(await custom('contextMenu.copyLink', () => {
      copyContextValue(currentDescriptor('link')?.url ?? '', i18n.t('contextMenu.copyLink'));
    }));
  } else if (kind === 'file' || kind === 'file-copy') {
    if (kind === 'file') {
      items.push(await custom('contextMenu.openFile', () => {
        const descriptor = currentDescriptor('file');
        runContextAction(descriptor?.open, i18n.t('contextMenu.openFile'));
      }));
    }
    if (revealAvailable) {
      items.push(await custom('contextMenu.revealFile', revealActiveFile));
    }
    items.push(await custom('contextMenu.copyPath', () => {
      const descriptor = currentDescriptor('file');
      copyContextValue(descriptor?.filePath ?? '', i18n.t('contextMenu.copyPath'));
    }));
  } else {
    if (!kind.endsWith('-selection')) {
      items.push(await custom('contextMenu.copyCode', () => {
        copyContextValue(currentDescriptor('code')?.content ?? '', i18n.t('contextMenu.copyCode'));
      }));
    }
    if (kind.includes('-file')) {
      items.push(await separator());
      items.push(await custom('contextMenu.openFile', () => {
        const descriptor = currentDescriptor('code');
        runContextAction(descriptor?.openFile, i18n.t('contextMenu.openFile'));
      }));
      if (revealAvailable) {
        items.push(await custom('contextMenu.revealFile', revealActiveFile));
      }
      items.push(await custom('contextMenu.copyPath', () => {
        copyContextValue(currentDescriptor('code')?.filePath ?? '', i18n.t('contextMenu.copyPath'));
      }));
    }
  }

  return Menu.new({ items });
}

function menuFor(kind: DesktopContextMenuKind, revealAvailable: boolean): Promise<TauriMenu> {
  const cacheKey = `${i18n.locale}:${kind}:${revealAvailable ? 'reveal' : 'plain'}`;
  let menu = menuCache.get(cacheKey);
  if (!menu) {
    menu = createMenu(kind, revealAvailable);
    menuCache.set(cacheKey, menu);
  }
  return menu;
}

async function showDesktopContextMenu(request: DesktopContextMenuRequest): Promise<void> {
  const sequence = ++requestSequence;
  const revealTarget = await resolveRevealTarget(request.descriptor);
  if (sequence !== requestSequence) return;
  activeDescriptor = request.descriptor;
  activeRevealTarget = revealTarget;
  const menu = await menuFor(request.kind, Boolean(revealTarget));
  if (sequence !== requestSequence) return;
  await menu.popup();
}

export function installDesktopContextMenu(): () => void {
  if (!isDesktopRuntime() || removeDocumentListener) {
    return removeDocumentListener ?? (() => undefined);
  }

  const handleContextMenu = (event: MouseEvent) => {
    event.preventDefault();
    const request = resolveDesktopContextMenuRequest(event);
    if (!request) return;
    event.stopPropagation();
    void showDesktopContextMenu(request).catch((error) => {
      console.error('[DesktopContextMenu] 原生菜单打开失败:', error);
    });
  };

  document.addEventListener('contextmenu', handleContextMenu, true);
  removeDocumentListener = () => {
    document.removeEventListener('contextmenu', handleContextMenu, true);
    removeDocumentListener = null;
    activeDescriptor = null;
    activeRevealTarget = null;
    requestSequence += 1;
  };
  return removeDocumentListener;
}
