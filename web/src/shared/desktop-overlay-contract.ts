/**
 * App Renderer 内的阻塞覆盖层契约。
 *
 * 原生 Browser Surface 是 BaseWindow 的兄弟视图，不能依赖 DOM 的 z-index
 * 覆盖它。所有会阻塞主界面的 DOM overlay 都必须登记在这里；Browser Tab
 * 收到可见状态后撤下自己的 native slot，关闭后再恢复槽位。
 */
const activeOverlayIds = new Set<string>();
const listeners = new Set<(visible: boolean) => void>();

function notify(): void {
  const visible = activeOverlayIds.size > 0;
  for (const listener of listeners) listener(visible);
  const desktop = typeof window === 'undefined' ? undefined : window.magiDesktop;
  if (desktop?.surface === 'app') {
    void desktop.setBlockingOverlay({ active: visible }).catch((error) => {
      console.error('[DesktopOverlay] 同步原生阻塞层状态失败:', error);
    });
  }
}

export function setDesktopBlockingOverlay(id: string, visible: boolean): void {
  const key = id.trim();
  if (!key) return;
  const wasVisible = activeOverlayIds.size > 0;
  if (visible) activeOverlayIds.add(key);
  else activeOverlayIds.delete(key);
  if (wasVisible !== (activeOverlayIds.size > 0)) notify();
}

export function onDesktopBlockingOverlayChange(listener: (visible: boolean) => void): () => void {
  listeners.add(listener);
  listener(activeOverlayIds.size > 0);
  return () => listeners.delete(listener);
}

export function desktopBlockingOverlayVisible(): boolean {
  return activeOverlayIds.size > 0;
}
