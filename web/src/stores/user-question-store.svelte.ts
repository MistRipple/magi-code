import type { PendingUserQuestionDto, UserQuestionResponseDto } from '../shared/rust-backend-types';
import {
  getAgentSessionUserQuestions,
  resolveAgentUserQuestion,
} from '../web/agent-api';
import type { AgentBindingOverride } from '../web/agent-binding-context';

type UserQuestionBinding =
  | { scope: 'personal' }
  | { scope: 'workspace'; workspaceId: string; workspacePath: string };

/** 模型向用户提出、尚未回答的选择题（`ask_user_question`）。权威事实在 daemon，这里只是投影。 */
export const userQuestionState = $state({
  sessionId: '',
  binding: { scope: 'personal' } as UserQuestionBinding,
  pending: [] as PendingUserQuestionDto[],
  hydrated: false,
  error: '',
});

let syncRevision = 0;

function bindingOverride(sessionId: string): AgentBindingOverride {
  return userQuestionState.binding.scope === 'workspace'
    ? {
        scope: 'workspace',
        workspaceId: userQuestionState.binding.workspaceId,
        workspacePath: userQuestionState.binding.workspacePath,
        sessionId,
      }
    : { scope: 'personal', sessionId };
}

export async function syncUserQuestions(
  sessionId: string,
  binding: UserQuestionBinding = { scope: 'personal' },
): Promise<void> {
  const normalizedSessionId = sessionId.trim();
  const revision = ++syncRevision;
  if (!normalizedSessionId) {
    userQuestionState.sessionId = '';
    userQuestionState.binding = { scope: 'personal' };
    userQuestionState.pending = [];
    userQuestionState.hydrated = true;
    userQuestionState.error = '';
    return;
  }
  userQuestionState.sessionId = normalizedSessionId;
  userQuestionState.binding = binding.scope === 'workspace'
    ? {
        scope: 'workspace',
        workspaceId: binding.workspaceId?.trim() || '',
        workspacePath: binding.workspacePath?.trim() || '',
      }
    : { scope: 'personal' };
  userQuestionState.error = '';
  try {
    const response = await getAgentSessionUserQuestions(
      normalizedSessionId,
      bindingOverride(normalizedSessionId),
    );
    if (revision !== syncRevision || userQuestionState.sessionId !== normalizedSessionId) return;
    userQuestionState.pending = response.pendingQuestions;
    userQuestionState.hydrated = true;
  } catch (error) {
    if (revision !== syncRevision || userQuestionState.sessionId !== normalizedSessionId) return;
    userQuestionState.error = error instanceof Error ? error.message : 'load user questions failed';
  }
}

/** 这个问题是否仍在等用户回答。投影还没就绪或属于别的会话时按“仍在等待”处理，避免误判。 */
export function isUserQuestionPending(sessionId: string, questionId: string): boolean {
  if (!userQuestionState.hydrated || userQuestionState.sessionId !== sessionId.trim()) return true;
  return userQuestionState.pending.some((question) => question.questionId === questionId);
}

export function hasPendingUserQuestion(sessionId: string): boolean {
  return userQuestionState.sessionId === sessionId.trim() && userQuestionState.pending.length > 0;
}

export async function resolvePendingUserQuestion(
  sessionId: string,
  questionId: string,
  response: UserQuestionResponseDto,
): Promise<void> {
  const normalizedSessionId = sessionId.trim();
  await resolveAgentUserQuestion(
    normalizedSessionId,
    questionId,
    response,
    bindingOverride(normalizedSessionId),
  );
  if (userQuestionState.sessionId === normalizedSessionId) {
    userQuestionState.pending = userQuestionState.pending.filter(
      (question) => question.questionId !== questionId.trim(),
    );
    userQuestionState.hydrated = true;
  }
}
