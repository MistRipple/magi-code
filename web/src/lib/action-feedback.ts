import {
  directIncidentError,
  incidentErrorDiagnostics,
  reportIncident,
  showFeedback,
  type FeedbackOptions,
} from './notifications';
import type { IncidentScope } from './notification-policy';

export interface ActionFeedbackOptions<T> {
  actionLabel: string;
  successMessage?: string | ((result: T) => string | undefined);
  successTitle?: string;
  successToast?: FeedbackOptions;
  errorTitle?: string;
  errorToast?: FeedbackOptions;
  incidentScope?: IncidentScope;
  onError?: (errorMessage: string, error: unknown) => void | Promise<void>;
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
    showFeedback('success', resolveSuccessMessage(result, options), {
      title: options.successTitle,
      source: 'web-action',
      ...(options.successToast || {}),
    });
    return result;
  } catch (error) {
    const errorMessage = `${options.actionLabel}失败`;
    await options.onError?.(errorMessage, error);
    const directError = directIncidentError(error, errorMessage);
    reportIncident(directError, {
      scope: options.incidentScope || 'workspace',
      title: options.errorTitle || errorMessage,
      ...incidentErrorDiagnostics(error, directError),
      failureStage: 'web_action',
      source: options.errorToast?.source || 'web-action',
      duration: options.errorToast?.duration,
    });
    return null;
  }
}
