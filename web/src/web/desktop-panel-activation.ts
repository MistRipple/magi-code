/**
 * 右栏激活协议的纯状态判断。
 *
 * Renderer 的本地 Tab 状态是用户意图，Desktop snapshot 是 Main 的确认。
 * 这里不触碰 IPC，只给 WebWorkbenchShell 一个可测试的唯一收敛规则，避免
 * Browser 与非 Browser 面板各自发送异步请求后产生乱序覆盖。
 */
export type DesktopPanelKind = 'agent' | 'browser' | 'code' | 'terminal' | null;

export interface DesktopPanelActivationTarget {
  scopeKey: string;
  kind: DesktopPanelKind;
  tabId: string | null;
  browserSessionId?: string | null;
}

export interface DesktopPanelActivationSnapshot {
  layout: {
    activePanelKind: DesktopPanelKind;
    activeTabId: string | null;
    activeSurfaceId: string | null;
  };
}

export type DesktopPanelActivationDecision = 'acknowledged' | 'wait_for_in_flight' | 'dispatch';

export function desktopPanelTargetKey(target: DesktopPanelActivationTarget): string {
  return [
    target.scopeKey,
    target.kind ?? 'empty',
    target.tabId ?? '',
    target.browserSessionId ?? '',
  ].join('\u0000');
}

export function sameDesktopPanelTarget(
  left: DesktopPanelActivationTarget | null,
  right: DesktopPanelActivationTarget,
): boolean {
  return left !== null && desktopPanelTargetKey(left) === desktopPanelTargetKey(right);
}

export function desktopPanelTargetAcknowledged(
  snapshot: DesktopPanelActivationSnapshot,
  target: DesktopPanelActivationTarget,
): boolean {
  const layout = snapshot.layout;
  if (layout.activePanelKind !== target.kind || layout.activeTabId !== target.tabId) {
    return false;
  }
  return target.kind === 'browser'
    ? Boolean(layout.activeSurfaceId)
    : layout.activeSurfaceId === null;
}

/**
 * 只允许一条 Main 激活请求在途。即使旧请求的快照先到达，也要等其 Promise
 * 收口后才派发最新意图，保证 Main 中的 Browser Surface 最终与 Renderer Tab 一致。
 */
export function decideDesktopPanelActivation(
  snapshot: DesktopPanelActivationSnapshot,
  target: DesktopPanelActivationTarget,
  hasInFlightRequest: boolean,
): DesktopPanelActivationDecision {
  if (desktopPanelTargetAcknowledged(snapshot, target)) return 'acknowledged';
  return hasInFlightRequest ? 'wait_for_in_flight' : 'dispatch';
}
