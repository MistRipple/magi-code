import type { ClientBridge, ClientBridgeMessage, SupportedLocale } from './client-bridge';

let activeBridge: ClientBridge | null = null;

export function setClientBridge(bridge: ClientBridge): void {
  activeBridge = bridge;
}

export function getClientBridge(): ClientBridge {
  if (!activeBridge) {
   throw new Error('[bridge-runtime] ClientBridge 未初始化。必须先调用 setClientBridge()。');
  }
  return activeBridge;
}

export function postBridgeMessage(message: ClientBridgeMessage): void {
  getClientBridge().postMessage(message);
}

export function onBridgeMessage(listener: (message: ClientBridgeMessage) => void): () => void {
  return getClientBridge().onMessage(listener);
}

export function getBridgeState<T>(): T | undefined {
  return getClientBridge().getState<T>();
}

export function setBridgeState<T>(state: T): void {
  getClientBridge().setState(state);
}

export function getInitialBridgeSessionId(): string {
  return getClientBridge().getInitialSessionId();
}

export function getInitialBridgeLocale(): SupportedLocale {
  return getClientBridge().getInitialLocale();
}

export function notifyBridgeReady(): void {
  getClientBridge().notifyReady();
}

export function getBridgeKind() {
  return getClientBridge().kind;
}
