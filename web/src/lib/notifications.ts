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
  presentation?: 'default' | 'toast';
}

export interface ReportIncidentOptions extends FeedbackOptions {
  scope: IncidentScope;
  level?: 'warning' | 'error';
  actionRequired?: boolean;
  detail?: string;
  errorCode?: string;
  failureStage?: string;
  taskId?: string;
  requestId?: string;
}

export function directIncidentError(error: unknown, fallback: string): string {
  if (error && typeof error === 'object') {
    const candidate = error as { detail?: unknown; message?: unknown };
    const detail = candidate.detail;
    if (typeof detail === 'string' && detail.trim()) {
      return detail.trim();
    }
    if (typeof candidate.message === 'string' && candidate.message.trim()) {
      return candidate.message.trim();
    }
  }
  const text = typeof error === 'string' ? error.trim() : '';
  return text || fallback;
}

export function incidentErrorDiagnostics(
  error: unknown,
  primaryMessage: string,
): Pick<ReportIncidentOptions, 'detail' | 'errorCode'> {
  if (!error || typeof error !== 'object') {
    return {};
  }
  const candidate = error as { detail?: unknown; errorCode?: unknown; stack?: unknown };
  const explicitDetail = typeof candidate.detail === 'string' ? candidate.detail.trim() : '';
  const stack = typeof candidate.stack === 'string' ? candidate.stack.trim() : '';
  const detail = explicitDetail && explicitDetail !== primaryMessage
    ? explicitDetail
    : (!explicitDetail && stack && stack !== primaryMessage ? stack : undefined);
  const errorCode = typeof candidate.errorCode === 'string' && candidate.errorCode.trim()
    ? candidate.errorCode.trim()
    : undefined;
  return { detail, errorCode };
}

export function showFeedback(
  level: FeedbackLevel,
  message: string,
  options: FeedbackOptions = {},
): void {
  const policy = resolveFeedbackPolicy(level);
  const forceVisible = options.presentation === 'toast';
  if (!forceVisible && policy.displayMode === 'silent') {
    return;
  }
  addToast(level, message, options.title, {
    source: options.source,
    actionRequired: policy.actionRequired,
    duration: options.duration,
    forceVisible,
  });
}

export function reportIncident(
  message: string,
  options: ReportIncidentOptions,
): boolean {
  const policy = resolveIncidentPolicy({ scope: options.scope });
  const level = options.level || 'error';

  try {
    const incident = buildIncidentRequest(
      {
        scope: options.scope,
        level,
        message,
        detail: options.detail,
        errorCode: options.errorCode,
        failureStage: options.failureStage,
        taskId: options.taskId,
        requestId: options.requestId,
        title: options.title,
        source: options.source,
        actionRequired: options.actionRequired ?? policy.actionRequired,
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
