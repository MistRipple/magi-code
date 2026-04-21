import {
  addToast,
  type ToastOptions,
} from '../stores/messages.svelte';

export interface ActionFeedbackOptions<T> {
  actionLabel: string;
  successMessage?: string | ((result: T) => string | undefined);
  successTitle?: string;
  successToast?: ToastOptions;
  errorTitle?: string;
  errorToast?: ToastOptions;
  onError?: (errorMessage: string, error: unknown) => void | Promise<void>;
}

function normalizeActionMessage(value: unknown): string {
  if (value instanceof Error) {
    return value.message.trim();
  }
  if (typeof value === 'string') {
    return value.trim();
  }
  return '';
}

function resolveSuccessMessage<T>(
  result: T,
  options: ActionFeedbackOptions<T>,
): string {
  if (typeof options.successMessage === 'function') {
    return options.successMessage(result)?.trim() || `${options.actionLabel}成功`;
  }
  if (typeof options.successMessage === 'string' && options.successMessage.trim()) {
    return options.successMessage.trim();
  }
  return `${options.actionLabel}成功`;
}

export async function runActionWithFeedback<T>(
  action: () => Promise<T>,
  options: ActionFeedbackOptions<T>,
): Promise<T | null> {
  try {
    const result = await action();
    addToast(
      'success',
      resolveSuccessMessage(result, options),
      options.successTitle,
      {
        category: 'audit',
        source: 'web-action',
        actionRequired: false,
        persistToCenter: true,
        countUnread: false,
        displayMode: 'toast',
        ...(options.successToast || {}),
      },
    );
    return result;
  } catch (error) {
    const detail = normalizeActionMessage(error);
    const errorMessage = detail ? `${options.actionLabel}失败：${detail}` : `${options.actionLabel}失败`;
    await options.onError?.(errorMessage, error);
    addToast(
      'error',
      errorMessage,
      options.errorTitle,
      {
        category: 'incident',
        source: 'web-action',
        actionRequired: true,
        persistToCenter: true,
        countUnread: true,
        displayMode: 'toast',
        ...(options.errorToast || {}),
      },
    );
    return null;
  }
}
