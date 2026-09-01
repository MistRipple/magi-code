export interface DesktopOverlayRectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface DesktopOverlayStatePayload {
  overlayId: string;
  kind: 'menu' | 'annotation';
  phase: 'menu' | 'select' | 'comment';
  ownerId: string;
  placement: 'right-pane-add' | 'browser-viewport' | 'browser-annotations';
  popupBounds: DesktopOverlayRectangle | null;
  title: string;
  items: Array<{
    id: string;
    label: string;
    icon: string | null;
    selected: boolean;
    disabled: boolean;
  }>;
  fields: Array<{
    id: string;
    label: string;
    type: 'number' | 'text';
    value: string;
    min: number | null;
    max: number | null;
  }>;
}

export interface DesktopOverlayIdentityPayload {
  overlayId: string;
  ownerId: string;
}

/**
 * Svelte 的深层响应式对象不能直接穿过 Electron IPC。这里建立唯一的
 * Overlay 传输边界，避免把 Proxy 作为菜单状态或重排状态发送到 Main。
 */
export function toDesktopOverlayState(
  state: DesktopOverlayStatePayload,
): DesktopOverlayStatePayload {
  return {
    overlayId: state.overlayId,
    kind: state.kind,
    phase: state.phase,
    ownerId: state.ownerId,
    placement: state.placement,
    popupBounds: state.popupBounds ? { ...state.popupBounds } : null,
    title: state.title,
    items: state.items.map((item) => ({
      id: item.id,
      label: item.label,
      icon: item.icon,
      selected: item.selected,
      disabled: item.disabled,
    })),
    fields: state.fields.map((field) => ({
      id: field.id,
      label: field.label,
      type: field.type,
      value: field.value,
      min: field.min,
      max: field.max,
    })),
  };
}

export function toDesktopOverlayIdentity(
  identity: DesktopOverlayIdentityPayload,
): DesktopOverlayIdentityPayload {
  return {
    overlayId: identity.overlayId,
    ownerId: identity.ownerId,
  };
}
