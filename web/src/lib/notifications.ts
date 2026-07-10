import { addToast, messagesState } from '../stores/messages.svelte';
import { vscode } from './vscode-bridge';
import {
  buildIncidentRequest,
  resolveFeedbackPolicy,
  resolveIncidentPolicy,
  type IncidentScope,
} from './notification-policy';

export type FeedbackLevel = 'info' | 'success' | 'warning' | 'error';

export interface FeedbackOptions {
  title?: string;
  source?: string;
  duration?: number;
}

export interface ReportIncidentOptions extends FeedbackOptions {
  scope: IncidentScope;
  level?: 'warning' | 'error';
  fingerprint?: string;
  actionRequired?: boolean;
  notificationId?: string;
}

export function showFeedback(
  level: FeedbackLevel,
  message: string,
  options: FeedbackOptions = {},
): void {
  const policy = resolveFeedbackPolicy();
  addToast(level, message, options.title, {
    source: options.source,
    actionRequired: policy.actionRequired,
    duration: options.duration,
  });
}

export function reportIncident(
  message: string,
  options: ReportIncidentOptions,
): boolean {
  const policy = resolveIncidentPolicy({ scope: options.scope });
  const level = options.level || 'error';
  addToast(level, message, options.title, {
    source: options.source,
    actionRequired: options.actionRequired ?? policy.actionRequired,
    duration: options.duration,
  });

  try {
    const incident = buildIncidentRequest(
      {
        scope: options.scope,
        level,
        message,
        title: options.title,
        source: options.source,
        fingerprint: options.fingerprint,
        actionRequired: options.actionRequired ?? policy.actionRequired,
        notificationId: options.notificationId,
      },
      {
        workspaceId: messagesState.currentWorkspaceId || undefined,
        workspacePath: messagesState.currentWorkspacePath || undefined,
        sessionId: messagesState.currentSessionId || undefined,
      },
    );
    vscode.postMessage({ type: 'reportIncident', incident });
    return true;
  } catch (error) {
    console.warn('[notifications] 无法持久化异常记录:', error);
    return false;
  }
}
