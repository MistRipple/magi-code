export type SupportedLocale = 'zh-CN' | 'en-US' | '';
export type ClientBridgeKind = 'vscode' | 'web';

export interface ClientBridgeMessage {
  type: string;
  [key: string]: unknown;
}

export interface ClientBridge {
  kind: ClientBridgeKind;
  postMessage(message: ClientBridgeMessage): void;
  onMessage(listener: (message: ClientBridgeMessage) => void): () => void;
  getState<T>(): T | undefined;
  setState<T>(state: T): void;
  getInitialSessionId(): string;
  getInitialLocale(): SupportedLocale;
  notifyReady(): void;
}
