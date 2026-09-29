/**
 * 工作台外壳的全局 UI 投影：当前显示哪个顶层视图、当前唯一的顶层弹出面板。
 * 只保存视图状态；业务事实仍由 daemon 提供。侧栏与 Header 共用这里，
 * 才能保证设置只有一个入口、弹出面板互斥且共享同一套关闭规则。
 *
 * 设置与工作台是同级视图：`settingsOpen` 决定显示哪一个；
 * `settingsMounted` 表示设置视图已经创建（创建后一直保活，只切换显示）。
 */

export type ShellPopover = 'notifications' | 'lan';

export const shellUi = $state<{
  settingsOpen: boolean;
  settingsMounted: boolean;
  popover: ShellPopover | null;
}>({
  settingsOpen: false,
  settingsMounted: false,
  popover: null,
});

/** 预先创建设置视图（空闲时调用）：之后进入是瞬时的，不再等到点击才渲染。 */
export function mountSettings(): void {
  shellUi.settingsMounted = true;
}

export function openSettings(): void {
  shellUi.popover = null;
  shellUi.settingsMounted = true;
  shellUi.settingsOpen = true;
}

export function closeSettings(): void {
  shellUi.settingsOpen = false;
}

export function togglePopover(popover: ShellPopover): void {
  shellUi.popover = shellUi.popover === popover ? null : popover;
}

export function closePopover(popover?: ShellPopover): void {
  if (!popover || shellUi.popover === popover) {
    shellUi.popover = null;
  }
}

/**
 * 弹出面板统一关闭规则：点击面板与其触发器之外的区域，或按 Escape。
 * 触发器与面板用 `data-shell-popover="<name>"` 标记，避免各组件各写一套。
 */
export function installShellPopoverDismiss(): () => void {
  const onPointerDown = (event: PointerEvent) => {
    if (!shellUi.popover) return;
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest(`[data-shell-popover="${shellUi.popover}"]`)) return;
    shellUi.popover = null;
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'Escape' && shellUi.popover) {
      event.preventDefault();
      shellUi.popover = null;
    }
  };
  window.addEventListener('pointerdown', onPointerDown);
  window.addEventListener('keydown', onKeyDown);
  return () => {
    window.removeEventListener('pointerdown', onPointerDown);
    window.removeEventListener('keydown', onKeyDown);
  };
}
