import { i18n } from '../stores/i18n.svelte';
import { resolveAgentFileRevealTarget, type AgentFileRevealTarget } from '../web/agent-api';
import {
  resolveDesktopContextMenuRequest,
  type DesktopContextMenuDescriptor,
  type DesktopContextMenuRequest,
} from './desktop-context-menu-contract';

type ContextAction = () => void | Promise<void>;

const REVEAL_TARGET_TIMEOUT_MS = 800;
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

async function resolveRevealTarget(
  descriptor: DesktopContextMenuDescriptor | null,
): Promise<AgentFileRevealTarget | null> {
  const context = descriptorFileContext(descriptor);
  if (!context) return null;
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), REVEAL_TARGET_TIMEOUT_MS);
  try {
    return await resolveAgentFileRevealTarget(context.filePath, {
      scope: 'workspace',
      workspaceId: context.fileScope.workspaceId?.trim() || '',
      workspacePath: context.fileScope.workspacePath?.trim() || '',
      sessionId: context.fileScope.sessionId?.trim() || undefined,
    }, controller.signal);
  } catch {
    return null;
  } finally {
    window.clearTimeout(timeout);
  }
}

function role(role: MagiDesktopContextMenuRole): MagiDesktopContextMenuItem {
  return { type: 'role', role };
}

function separator(): MagiDesktopContextMenuItem {
  return { type: 'separator' };
}

function action(id: string, labelKey: string, enabled = true): MagiDesktopContextMenuItem {
  return { type: 'action', id, label: i18n.t(labelKey), enabled };
}

function buildMenuItems(
  request: DesktopContextMenuRequest,
  revealAvailable: boolean,
): MagiDesktopContextMenuItem[] {
  const { kind, descriptor } = request;
  if (kind === 'editable') {
    return [
      role('undo'), role('redo'), separator(),
      role('cut'), role('copy'), role('paste'), separator(),
      role('selectAll'),
    ];
  }
  if (kind === 'selection') return [role('copy')];
  if (kind === 'readonly') return [role('copy'), separator(), role('selectAll')];
  if (kind === 'workspace') return [action('open-workspace', 'contextMenu.openFolder')];

  const items: MagiDesktopContextMenuItem[] = [];
  if (kind.endsWith('-selection')) {
    items.push(role('copy'), separator());
  }

  if (descriptor?.kind === 'image') {
    if (descriptor.open) items.push(action('open-image', 'contextMenu.openImage'));
    if (revealAvailable) items.push(action('reveal-file', 'contextMenu.revealFile'));
    if (kind === 'image-file') {
      items.push(action('copy-path', 'contextMenu.copyPath'));
    } else if (kind === 'image-source') {
      items.push(action('copy-image-address', 'contextMenu.copyImageAddress'));
    }
    return items;
  }

  if (descriptor?.kind === 'link') {
    items.push(action('open-link', 'contextMenu.openLink'));
    items.push(action('copy-link', 'contextMenu.copyLink'));
    return items;
  }

  if (descriptor?.kind === 'file') {
    if (descriptor.open) items.push(action('open-file', 'contextMenu.openFile'));
    if (revealAvailable) items.push(action('reveal-file', 'contextMenu.revealFile'));
    items.push(action('copy-path', 'contextMenu.copyPath'));
    return items;
  }

  if (descriptor?.kind === 'code') {
    if (!kind.endsWith('-selection')) items.push(action('copy-code', 'contextMenu.copyCode'));
    if (descriptor.filePath) {
      if (items.length > 0) items.push(separator());
      if (descriptor.openFile) items.push(action('open-file', 'contextMenu.openFile'));
      if (revealAvailable) items.push(action('reveal-file', 'contextMenu.revealFile'));
      items.push(action('copy-path', 'contextMenu.copyPath'));
    }
  }
  return items;
}

async function executeMenuAction(
  actionId: string | null,
  descriptor: DesktopContextMenuDescriptor | null,
  revealTarget: AgentFileRevealTarget | null,
): Promise<void> {
  if (!actionId) return;
  const desktop = window.magiDesktop;
  if (!desktop) throw new Error('desktop_preload_bridge_unavailable');
  switch (actionId) {
    case 'open-workspace':
      if (descriptor?.kind === 'workspace') {
        await desktop.openWorkspaceFolder(descriptor.workspacePathRef);
      }
      return;
    case 'reveal-file':
      if (revealTarget) {
        await desktop.revealWorkspaceFile({
          targetPathRef: revealTarget.targetPathRef,
          workspaceRootPathRef: revealTarget.workspaceRootPathRef,
        });
      }
      return;
    case 'open-link':
      if (descriptor?.kind === 'link') runContextAction(descriptor.open, i18n.t('contextMenu.openLink'));
      return;
    case 'copy-link':
      if (descriptor?.kind === 'link') copyContextValue(descriptor.url, i18n.t('contextMenu.copyLink'));
      return;
    case 'open-image':
      if (descriptor?.kind === 'image') runContextAction(descriptor.open, i18n.t('contextMenu.openImage'));
      return;
    case 'copy-image-address':
      if (descriptor?.kind === 'image') copyContextValue(descriptor.source ?? '', i18n.t('contextMenu.copyImageAddress'));
      return;
    case 'open-file':
      if (descriptor?.kind === 'file') runContextAction(descriptor.open, i18n.t('contextMenu.openFile'));
      if (descriptor?.kind === 'code') runContextAction(descriptor.openFile, i18n.t('contextMenu.openFile'));
      return;
    case 'copy-path':
      if (descriptor?.kind === 'file' || descriptor?.kind === 'code' || descriptor?.kind === 'image') {
        copyContextValue(descriptor.filePath ?? '', i18n.t('contextMenu.copyPath'));
      }
      return;
    case 'copy-code':
      if (descriptor?.kind === 'code') copyContextValue(descriptor.content, i18n.t('contextMenu.copyCode'));
      return;
  }
}

async function showDesktopContextMenu(request: DesktopContextMenuRequest): Promise<void> {
  const desktop = window.magiDesktop;
  if (!desktop) return;
  const sequence = ++requestSequence;
  const revealTarget = await resolveRevealTarget(request.descriptor);
  if (sequence !== requestSequence) return;
  const items = buildMenuItems(request, Boolean(revealTarget));
  if (items.length === 0) return;
  const actionId = await desktop.showContextMenu({ items });
  if (sequence !== requestSequence) return;
  await executeMenuAction(actionId, request.descriptor, revealTarget);
}

export function installDesktopContextMenu(): () => void {
  if (!window.magiDesktop || removeDocumentListener) {
    return removeDocumentListener ?? (() => undefined);
  }

  const handleContextMenu = (event: MouseEvent) => {
    const request = resolveDesktopContextMenuRequest(event);
    if (!request) return;
    event.preventDefault();
    event.stopPropagation();
    void showDesktopContextMenu(request).catch((error) => {
      console.error('[DesktopContextMenu] 原生菜单打开失败:', error);
    });
  };

  document.addEventListener('contextmenu', handleContextMenu, true);
  removeDocumentListener = () => {
    document.removeEventListener('contextmenu', handleContextMenu, true);
    removeDocumentListener = null;
    requestSequence += 1;
  };
  return removeDocumentListener;
}
