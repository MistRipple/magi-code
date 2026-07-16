import { isDesktopRuntime } from './desktop-updater';
import type { ResolvedAgentPath } from '../web/agent-api';
import type {
  ComposerContextReferenceInput,
  ComposerContextReferenceKind,
} from './composer-context-references';

export const DESKTOP_CONTEXT_DROP_EVENT = 'magi:desktop-context-drop';

export type DesktopDropZone = 'sidebar' | 'conversation';

export interface DesktopDropPoint {
  x: number;
  y: number;
}

export interface DesktopDropRect {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

export interface DesktopDropZones {
  sidebar?: DesktopDropRect | null;
  conversation?: DesktopDropRect | null;
}

export type DesktopDragDropEvent =
  | { type: 'enter'; paths: string[]; position: DesktopDropPoint }
  | { type: 'over'; position: DesktopDropPoint }
  | { type: 'drop'; paths: string[]; position: DesktopDropPoint }
  | { type: 'leave' };

interface DesktopWebviewDragDropSource {
  onDragDropEvent(
    handler: (event: { payload: DesktopDragDropEvent }) => void,
  ): Promise<() => void>;
}

export interface DesktopFileDropDependencies {
  isDesktopRuntime?: () => boolean;
  loadCurrentWebview?: () => Promise<DesktopWebviewDragDropSource>;
}

function isRootPath(path: string): boolean {
  return path === '/'
    || path === '\\'
    || /^[A-Za-z]:[\\/]$/u.test(path);
}

function normalizeDropPath(path: string): string {
  const trimmed = path.trim();
  if (!trimmed || isRootPath(trimmed)) return trimmed;
  return trimmed.replace(/[\\/]+$/u, '');
}

function pathName(path: string): string {
  return path.split(/[\\/]/u).filter(Boolean).pop() || path;
}

function containsPoint(rect: DesktopDropRect | null | undefined, point: DesktopDropPoint): boolean {
  return Boolean(
    rect
    && point.x >= rect.left
    && point.x <= rect.right
    && point.y >= rect.top
    && point.y <= rect.bottom,
  );
}

export function physicalToCssPoint(
  position: DesktopDropPoint,
  devicePixelRatio: number,
): DesktopDropPoint {
  const scale = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0
    ? devicePixelRatio
    : 1;
  return {
    x: position.x / scale,
    y: position.y / scale,
  };
}

export function resolveDesktopDropZone(
  point: DesktopDropPoint,
  zones: DesktopDropZones,
): DesktopDropZone | null {
  if (containsPoint(zones.sidebar, point)) return 'sidebar';
  if (containsPoint(zones.conversation, point)) return 'conversation';
  return null;
}

export function normalizeDesktopDropPaths(
  paths: string[],
  existingPaths: string[] = [],
  limit = Number.POSITIVE_INFINITY,
): string[] {
  const existing = new Set(existingPaths.map(normalizeDropPath).filter(Boolean));
  const seen = new Set<string>();
  const normalized: string[] = [];
  const boundedLimit = Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : Number.POSITIVE_INFINITY;
  if (boundedLimit === 0) return normalized;
  for (const value of paths) {
    const path = normalizeDropPath(value);
    if (!path || existing.has(path) || seen.has(path)) continue;
    seen.add(path);
    normalized.push(path);
    if (normalized.length >= boundedLimit) break;
  }
  return normalized;
}

export function resolveDesktopDroppedPath(
  requestedPath: string,
  result: ResolvedAgentPath,
): ComposerContextReferenceInput | null {
  const kind: ComposerContextReferenceKind = result.kind;
  const resolvedPath = normalizeDropPath(result.displayPath);
  if (!resolvedPath) return null;
  return {
    kind,
    path: resolvedPath,
    pathRef: result.pathRef,
    name: result.name || pathName(resolvedPath) || pathName(requestedPath),
  };
}

export async function registerDesktopFileDropListener(
  handler: (event: DesktopDragDropEvent) => void,
  dependencies: DesktopFileDropDependencies = {},
): Promise<() => void> {
  const detectDesktop = dependencies.isDesktopRuntime ?? isDesktopRuntime;
  if (!detectDesktop()) return () => {};
  const webview = dependencies.loadCurrentWebview
    ? await dependencies.loadCurrentWebview()
    : (await import('@tauri-apps/api/webview')).getCurrentWebview();
  return await webview.onDragDropEvent((event) => handler(event.payload));
}
