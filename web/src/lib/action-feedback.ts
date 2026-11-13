import { i18n } from '../stores/i18n.svelte';
import {
  directIncidentError,
  incidentErrorDiagnostics,
  reportIncident,
  showFeedback,
  type FeedbackOptions,
} from './notifications';
import type { IncidentScope } from './notification-policy';
import { messagesState } from '../stores/messages.svelte';

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
    return options.successMessage(result)?.trim() || i18n.t('common.actionSucceeded', { action: options.actionLabel });
  }
  if (typeof options.successMessage === 'string' && options.successMessage.trim()) {
    return options.successMessage.trim();
  }
  return i18n.t('common.actionSucceeded', { action: options.actionLabel });
}

export async function runActionWithFeedback<T>(
  action: () => Promise<T>,
  options: ActionFeedbackOptions<T>,
): Promise<T | null> {
  const context = {
    workspaceId: messagesState.currentWorkspaceId || undefined,
    workspacePath: messagesState.currentWorkspacePath || undefined,
    sessionId: messagesState.currentSessionId || undefined,
  };
  try {
    const result = await action();
    showFeedback('success', resolveSuccessMessage(result, options), {
      title: options.successTitle,
      source: 'web-action',
      ...(options.successToast || {}),
    });
    return result;
  } catch (error) {
    const errorMessage = i18n.t('common.actionFailed', { action: options.actionLabel });
    await options.onError?.(errorMessage, error);
    const directError = directIncidentError(error, errorMessage);
    if (!options.onError) {
      // 无就地错误呈现的操作仍需即时反馈；异常记录负责持久诊断，不替代操作回执。
      showFeedback('error', directError, {
        title: options.errorTitle || errorMessage,
        source: 'web-action',
        ...options.errorToast,
      });
    }
    reportIncident(directError, {
      scope: options.incidentScope || (context.workspaceId ? 'workspace' : 'app'),
      context,
      title: options.errorTitle || errorMessage,
      ...incidentErrorDiagnostics(error, directError),
      failureStage: 'web_action',
      source: options.errorToast?.source || 'web-action',
      duration: options.errorToast?.duration,
    });
    return null;
  }
}
