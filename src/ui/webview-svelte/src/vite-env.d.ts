/// <reference types="svelte" />
/// <reference types="vite/client" />

// VS Code API 类型声明
interface VsCodeApi {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
}

declare function acquireVsCodeApi(): VsCodeApi;

// 全局 VS Code API 实例
declare const vscode: VsCodeApi;

