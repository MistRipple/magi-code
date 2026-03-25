import type { ClientBridgeMessage } from '../../../shared/bridges/client-bridge';
import {
  getBridgeKind,
  getBridgeState,
  getInitialBridgeLocale,
  getInitialBridgeSessionId,
  onBridgeMessage,
  postBridgeMessage,
  setBridgeState,
} from '../../../shared/bridges/bridge-runtime';

export type WebviewMessage = ClientBridgeMessage;

export function postMessage(message: WebviewMessage): void {
  postBridgeMessage(message);
}

export function getState<T>(): T | undefined {
  return getBridgeState<T>();
}

export function setState<T>(state: T): void {
  setBridgeState(state);
}

export function onMessage(listener: (message: WebviewMessage) => void): () => void {
  return onBridgeMessage(listener);
}

export const vscode = {
  postMessage,
  getState,
  setState,
  onMessage,
};

export function getInitialSessionId(): string {
  return getInitialBridgeSessionId();
}

export function getInitialLocale(): 'zh-CN' | 'en-US' | '' {
  return getInitialBridgeLocale();
}

export function getClientKind(): 'vscode' | 'web' {
  return getBridgeKind();
}
