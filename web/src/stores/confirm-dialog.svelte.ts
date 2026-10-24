/**
 * 应用内确认对话框：取代浏览器原生 `window.confirm`。
 *
 * 原生确认框样式与应用不一致、无法本地化按钮、在 Electron 里会抢走整个窗口焦点。
 * 调用方 `await confirmAction(...)`，对话框由 `ConfirmDialog.svelte` 在 App 根部渲染；
 * 同一时间只有一个确认请求，新请求会让尚未答复的旧请求按「取消」结束。
 */

import { i18n } from './i18n.svelte';

export interface ConfirmRequest {
  /** 轻量确认的触发按钮；不存在时使用阻塞 Modal。只保存本次界面引用。 */
  anchor?: HTMLElement;
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  /** danger：删除、清除、回收等破坏性操作，确认按钮用警示色。 */
  tone?: 'default' | 'danger';
  /** 第二个可选动作（例如「另存为新角色」）；设置后对话框有三个出口：取消、第二动作、确认。 */
  secondaryLabel?: string;
}

export type ConfirmChoice = 'confirm' | 'secondary' | 'cancel';

export const confirmDialogState = $state<{ request: ConfirmRequest | null }>({ request: null });

let pending: ((choice: ConfirmChoice) => void) | null = null;

/** 有第二动作的确认：Esc、点遮罩和「取消」都是 cancel，不会误触发任何一个动作。 */
export function chooseAction(request: ConfirmRequest): Promise<ConfirmChoice> {
  settle('cancel');
  confirmDialogState.request = request;
  return new Promise<ConfirmChoice>((resolve) => {
    pending = resolve;
  });
}

export async function confirmAction(request: ConfirmRequest): Promise<boolean> {
  return (await chooseAction(request)) === 'confirm';
}

/**
 * 短确认：调用方必须提供动作化确认文案，可附带锚点显示为 Popconfirm。
 * 需要更具体的标题或按钮文字时使用 `confirmAction`。
 */
export function confirmMessage(
  message: string,
  options: { title?: string; confirmLabel: string; tone?: 'default' | 'danger'; anchor?: HTMLElement },
): Promise<boolean> {
  return confirmAction({
    anchor: options.anchor,
    title: options.title ?? i18n.t('common.confirmTitle'),
    message,
    confirmLabel: options.confirmLabel,
    cancelLabel: i18n.t('common.cancel'),
    tone: options.tone ?? 'default',
  });
}

export function settleConfirm(choice: ConfirmChoice): void {
  settle(choice);
}

function settle(choice: ConfirmChoice): void {
  const resolve = pending;
  pending = null;
  confirmDialogState.request = null;
  resolve?.(choice);
}
