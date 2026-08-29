export interface DesktopOverlayIdentity {
  overlayId: string;
  ownerId: string;
}

export type DesktopOverlayCloseReason = "closed" | "replaced";

export interface DesktopOverlayClosedIdentityEvent extends DesktopOverlayIdentity {
  reason: DesktopOverlayCloseReason;
  replacement: DesktopOverlayIdentity | null;
}

export function sameDesktopOverlayIdentity(
  left: DesktopOverlayIdentity,
  right: DesktopOverlayIdentity,
): boolean {
  return left.overlayId === right.overlayId && left.ownerId === right.ownerId;
}

export function createDesktopOverlayClosedIdentityEvent(
  identity: DesktopOverlayIdentity,
  reason: DesktopOverlayCloseReason,
  replacement: DesktopOverlayIdentity | null = null,
): DesktopOverlayClosedIdentityEvent {
  return {
    overlayId: identity.overlayId,
    ownerId: identity.ownerId,
    reason,
    replacement,
  };
}
