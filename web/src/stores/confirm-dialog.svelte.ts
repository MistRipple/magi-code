/**
 * 应用内确认对话框：取代浏览器原生 `window.confirm`。
 *
 * 原生确认框样式与应用不一致、无法本地化按钮、在 Electron 里会抢走整个窗口焦点。
 * 调用方 `await confirmAction(...)`，对话框由 `ConfirmDialog.svelte` 在 App 根部渲染；
 * 同一时间只有一个确认请求，新请求会让尚未答复的旧请求按「取消」结束。
 */

export interface ConfirmRequest {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  /** danger：删除、清除、回收等破坏性操作，确认按钮用警示色。 */
  tone?: 'default' | 'danger';
}

export const confirmDialogState = $state<{ request: ConfirmRequest | null }>({ request: null });

let pending: ((confirmed: boolean) => void) | null = null;

export function confirmAction(request: ConfirmRequest): Promise<boolean> {
  settle(false);
  confirmDialogState.request = request;
  return new Promise<boolean>((resolve) => {
    pending = resolve;
  });
}

export function settleConfirm(confirmed: boolean): void {
  settle(confirmed);
}

function settle(confirmed: boolean): void {
  const resolve = pending;
  pending = null;
  confirmDialogState.request = null;
  resolve?.(confirmed);
}
