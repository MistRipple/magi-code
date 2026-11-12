import { getTransport } from '../../shared/transport';
import type { SessionTurnCommand } from '../../shared/app-server-protocol.generated';
import type { SessionInterruptResponseDto, SessionToolApprovalResponseDto, SessionToolApprovalsResponseDto, SessionUserQuestionResponseDto, SessionUserQuestionsResponseDto, UserQuestionResponseDto, ToolApprovalDecision, SessionTurnQueueResponseDto } from '../../shared/rust-backend-types';
import type { CanonicalTurn, CanonicalTurnItem } from '../../shared/protocol/canonical-turn';
import { i18n } from '../../stores/i18n.svelte';
import type { MessageBrowserNodeSelection } from '../../types/message';
import type { AgentBindingOverride } from '../agent-binding-context';
import type { AgentSessionTurnImagePayload, AgentSessionTurnResult, RawAgentSessionSummary } from './types';
import { AgentApiError, agentUrl, buildBoundQueryWithOverride, normalizeSessionSummary, parseAgentJson, postBoundJson, resolveBindingWithOverride } from './core';

export async function submitSessionTurn(
  payload: {
    sourceFolderId?: string | null;
    text?: string | null;
    skillName?: string | null;
    locale?: string;
    goalMode?: boolean;
    resume?: boolean;
    command?: SessionTurnCommand | null;
    pluginCommandId?: string | null;
    images: AgentSessionTurnImagePayload[];
    contextReferences?: Array<{
      kind: 'file' | 'directory';
      path: string;
      pathRef?: string;
      sourceFolderId?: string;
      name: string;
    }>;
    browserAnnotationRefs?: string[];
    browserNodeSelections?: MessageBrowserNodeSelection[];
    accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
    orchestratorSessionConfig?: Record<string, unknown> | null;
    requestId?: string | null;
    userMessageId?: string | null;
    placeholderMessageId?: string | null;
    steerCurrentTurn?: boolean;
    expectedTurnId?: string | null;
    replaceTurnId?: string | null;
    interruptTurnId?: string | null;
  },
  bindingOverride?: AgentBindingOverride,
): Promise<AgentSessionTurnResult> {
  try {
    const binding = resolveBindingWithOverride(bindingOverride);
    const response = await getTransport().request(agentUrl('/api/session/turn'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sessionId: binding.sessionId || null,
        scope: binding.scope,
        workspaceId: binding.scope === 'workspace' ? binding.workspaceId || null : null,
        workspacePath: binding.scope === 'workspace' ? binding.workspacePath || null : null,
        sourceFolderId: payload.sourceFolderId || null,
        text: payload.text ?? null,
        skillName: payload.skillName ?? null,
        locale: payload.locale ?? i18n.locale,
        goalMode: payload.goalMode === true,
        resume: payload.resume === true,
        command: payload.command ?? null,
        pluginCommandId: payload.pluginCommandId ?? null,
        accessProfile: payload.accessProfile ?? null,
        requestId: payload.requestId ?? null,
        userMessageId: payload.userMessageId ?? null,
        placeholderMessageId: payload.placeholderMessageId ?? null,
        steerCurrentTurn: payload.steerCurrentTurn === true,
        expectedTurnId: payload.expectedTurnId ?? null,
        replaceTurnId: payload.replaceTurnId ?? null,
        interruptTurnId: payload.interruptTurnId ?? null,
        images: payload.images.map((image) => ({
          name: image.name,
          dataUrl: image.dataUrl,
        })),
        contextReferences: (payload.contextReferences ?? []).map((reference) => ({
          kind: reference.kind,
          path: reference.path,
          ...(reference.pathRef ? { pathRef: reference.pathRef } : {}),
          ...(reference.sourceFolderId ? { sourceFolderId: reference.sourceFolderId } : {}),
          name: reference.name,
        })),
        browserAnnotationRefs: (payload.browserAnnotationRefs ?? [])
          .map((annotationId) => annotationId.trim())
          .filter(Boolean),
        browserNodeSelections: (payload.browserNodeSelections ?? []).map((selection) => ({
          ...selection,
          attributes: { ...selection.attributes },
          bounds: selection.bounds ? { ...selection.bounds } : selection.bounds,
        })),
        orchestratorSessionConfig: payload.orchestratorSessionConfig ?? null,
      }),
    });
    const raw = await parseAgentJson<{
      sessionId: string;
      entryId: string;
      eventId: string;
      acceptedAt: number;
      runtimeEpoch: string;
      eventStreamNextSequence: number;
      createdSession: boolean;
      route: 'chat' | 'execute' | 'task' | 'continue' | 'steer';
      turnId?: string | null;
      requestId?: string | null;
      executionProfile?: 'conversation' | 'task' | null;
      status?: string | null;
      eventSequence?: number | null;
      sessionSummary?: RawAgentSessionSummary | null;
      rootTaskId?: string | null;
      actionTaskId?: string | null;
      executionChainRef?: string | null;
      userMessageItemId?: string | null;
      queued?: boolean;
      queueId?: string | null;
      queuePosition?: number | null;
      canonicalSchemaVersion?: string | null;
      canonicalEventKind?: string | null;
      canonicalEventId?: string | null;
      canonicalEventSeq?: number | null;
      canonicalOccurredAt?: number | null;
      canonicalTurn?: CanonicalTurn | null;
      canonicalItem?: CanonicalTurnItem | null;
      steeredTurnId?: string | null;
    }>(response, 'submit session turn');
    const runtimeEpoch = typeof raw.runtimeEpoch === 'string' ? raw.runtimeEpoch.trim() : '';
    const eventStreamNextSequence = typeof raw.eventStreamNextSequence === 'number'
      && Number.isFinite(raw.eventStreamNextSequence)
      ? Math.floor(raw.eventStreamNextSequence)
      : 0;
    if (!runtimeEpoch || eventStreamNextSequence < 1) {
      throw new Error('submit session turn 缺少有效的运行时代际或事件游标');
    }
    return {
      sessionId: raw.sessionId,
      entryId: raw.entryId,
      eventId: raw.eventId,
      acceptedAt: raw.acceptedAt,
      runtimeEpoch,
      eventStreamNextSequence,
      createdSession: raw.createdSession,
      route: raw.route,
      turnId: typeof raw.turnId === 'string' && raw.turnId.trim() ? raw.turnId.trim() : null,
      requestId: typeof raw.requestId === 'string' && raw.requestId.trim() ? raw.requestId.trim() : null,
      executionProfile: raw.executionProfile === 'task' || raw.executionProfile === 'conversation'
        ? raw.executionProfile
        : null,
      status: typeof raw.status === 'string' && raw.status.trim() ? raw.status.trim() : null,
      eventSequence: typeof raw.eventSequence === 'number' && Number.isFinite(raw.eventSequence)
        ? Math.floor(raw.eventSequence)
        : null,
      sessionSummary: raw.sessionSummary ? normalizeSessionSummary(raw.sessionSummary) : null,
      rootTaskId: typeof raw.rootTaskId === 'string' && raw.rootTaskId.trim() ? raw.rootTaskId.trim() : null,
      actionTaskId: typeof raw.actionTaskId === 'string' && raw.actionTaskId.trim()
        ? raw.actionTaskId.trim()
        : null,
      executionChainRef: typeof raw.executionChainRef === 'string' && raw.executionChainRef.trim()
        ? raw.executionChainRef.trim()
        : null,
      userMessageItemId: typeof raw.userMessageItemId === 'string' && raw.userMessageItemId.trim()
        ? raw.userMessageItemId.trim()
        : null,
      queued: raw.queued === true,
      queueId: typeof raw.queueId === 'string' && raw.queueId.trim()
        ? raw.queueId.trim()
        : null,
      queuePosition: typeof raw.queuePosition === 'number' && Number.isFinite(raw.queuePosition)
        ? Math.max(1, Math.floor(raw.queuePosition))
        : null,
      canonicalSchemaVersion: typeof raw.canonicalSchemaVersion === 'string' && raw.canonicalSchemaVersion.trim()
        ? raw.canonicalSchemaVersion.trim()
        : null,
      canonicalEventKind: typeof raw.canonicalEventKind === 'string' && raw.canonicalEventKind.trim()
        ? raw.canonicalEventKind.trim()
        : null,
      canonicalEventId: typeof raw.canonicalEventId === 'string' && raw.canonicalEventId.trim()
        ? raw.canonicalEventId.trim()
        : null,
      canonicalEventSeq: typeof raw.canonicalEventSeq === 'number' && Number.isFinite(raw.canonicalEventSeq)
        ? Math.floor(raw.canonicalEventSeq)
        : null,
      canonicalOccurredAt: typeof raw.canonicalOccurredAt === 'number' && Number.isFinite(raw.canonicalOccurredAt)
        ? Math.floor(raw.canonicalOccurredAt)
        : null,
      canonicalTurn: raw.canonicalTurn ?? null,
      canonicalItem: raw.canonicalItem ?? null,
      steeredTurnId: typeof raw.steeredTurnId === 'string' && raw.steeredTurnId.trim()
        ? raw.steeredTurnId.trim()
        : null,
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentSessionTurnQueue(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'get session turn queue');
  }
  const binding = resolveBindingWithOverride(bindingOverride);
  const query = new URLSearchParams({
    sessionId: normalizedSessionId,
  });
  if (binding.scope === 'workspace') {
    if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
    if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
    if (binding.sourceFolderId) query.set('sourceFolderId', binding.sourceFolderId);
  }
  const response = await getTransport().request(agentUrl('/api/session/queue', query.toString()));
  return await parseAgentJson<SessionTurnQueueResponseDto>(response, 'get session turn queue');
}

export async function removeAgentSessionTurnQueueItem(
  sessionId: string,
  queueId: string,
  expectedRevision: number,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  const normalizedSessionId = sessionId.trim();
  const normalizedQueueId = queueId.trim();
  if (!normalizedSessionId || !normalizedQueueId) {
    throw new AgentApiError(400, 'sessionId 和 queueId 不能为空', 'remove session turn queue item');
  }
  return await postBoundJson<SessionTurnQueueResponseDto>(
    '/api/session/queue/remove',
    { sessionId: normalizedSessionId, queueId: normalizedQueueId, expectedRevision },
    'remove session turn queue item',
    bindingOverride,
  );
}

export async function updateAgentSessionTurnQueueItem(
  sessionId: string,
  queueId: string,
  expectedRevision: number,
  request: import('../../shared/app-server-protocol.generated').TurnStartParams,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  return postBoundJson<SessionTurnQueueResponseDto>(
    '/api/session/queue/update',
    { sessionId: sessionId.trim(), queueId: queueId.trim(), expectedRevision, request },
    'update session turn queue item',
    bindingOverride,
  );
}

export async function retryAgentSessionTurnQueueItem(
  sessionId: string,
  queueId: string,
  expectedRevision: number,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  return postBoundJson<SessionTurnQueueResponseDto>(
    '/api/session/queue/retry',
    { sessionId, queueId, expectedRevision },
    'retry session turn queue item',
    bindingOverride,
  );
}

export async function guideAgentSessionTurnQueueItem(
  sessionId: string,
  queueId: string,
  expectedTurnId: string,
  expectedRevision: number,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionTurnQueueResponseDto> {
  const normalizedSessionId = sessionId.trim();
  const normalizedQueueId = queueId.trim();
  const normalizedTurnId = expectedTurnId.trim();
  if (!normalizedSessionId || !normalizedQueueId || !normalizedTurnId) {
    throw new AgentApiError(400, 'sessionId、queueId 和 expectedTurnId 不能为空', 'guide session turn queue item');
  }
  return await postBoundJson<SessionTurnQueueResponseDto>(
    '/api/session/queue/guide',
    { sessionId: normalizedSessionId, queueId: normalizedQueueId, expectedTurnId: normalizedTurnId, expectedRevision },
    'guide session turn queue item',
    bindingOverride,
  );
}

export async function interruptAgentSession(
  sessionId: string,
): Promise<SessionInterruptResponseDto> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'interrupt session turn');
  }
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort('interrupt_request_timeout'), 10_000);
  try {
    return await postBoundJson<SessionInterruptResponseDto>(
      '/api/session/interrupt',
      { sessionId: normalizedSessionId },
      'interrupt session turn',
      undefined,
      controller.signal,
    );
  } finally {
    window.clearTimeout(timeout);
  }
}

export async function getAgentSessionToolApprovals(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionToolApprovalsResponseDto> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'load tool approvals');
  }
  const query = buildBoundQueryWithOverride(
    {},
    bindingOverride
      ? { ...bindingOverride, sessionId: normalizedSessionId }
      : { sessionId: normalizedSessionId },
    { includeScope: false, includeSession: true },
  );
  const response = await getTransport().request(
    agentUrl('/api/session/tool-approvals', query),
    { cache: 'no-store' },
  );
  return parseAgentJson<SessionToolApprovalsResponseDto>(response, 'load tool approvals');
}

export async function resolveAgentToolApproval(
  sessionId: string,
  approvalId: string,
  decision: ToolApprovalDecision,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionToolApprovalResponseDto> {
  const normalizedSessionId = sessionId.trim();
  const normalizedApprovalId = approvalId.trim();
  if (!normalizedSessionId || !normalizedApprovalId) {
    throw new AgentApiError(400, 'sessionId 和 approvalId 不能为空', 'resolve tool approval');
  }
  return await postBoundJson<SessionToolApprovalResponseDto>(
    '/api/session/tool-approval',
    {
      sessionId: normalizedSessionId,
      approvalId: normalizedApprovalId,
      decision,
    },
    'resolve tool approval',
    bindingOverride
      ? { ...bindingOverride, sessionId: normalizedSessionId }
      : { sessionId: normalizedSessionId },
  );
}

export async function getAgentSessionUserQuestions(
  sessionId: string,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionUserQuestionsResponseDto> {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) {
    throw new AgentApiError(400, 'sessionId 不能为空', 'load user questions');
  }
  const query = buildBoundQueryWithOverride(
    {},
    bindingOverride
      ? { ...bindingOverride, sessionId: normalizedSessionId }
      : { sessionId: normalizedSessionId },
    { includeScope: false, includeSession: true },
  );
  const response = await getTransport().request(
    agentUrl('/api/session/user-questions', query),
    { cache: 'no-store' },
  );
  return parseAgentJson<SessionUserQuestionsResponseDto>(response, 'load user questions');
}

export async function resolveAgentUserQuestion(
  sessionId: string,
  questionId: string,
  responsePayload: UserQuestionResponseDto,
  bindingOverride?: AgentBindingOverride,
): Promise<SessionUserQuestionResponseDto> {
  const normalizedSessionId = sessionId.trim();
  const normalizedQuestionId = questionId.trim();
  if (!normalizedSessionId || !normalizedQuestionId) {
    throw new AgentApiError(400, 'sessionId 和 questionId 不能为空', 'resolve user question');
  }
  return await postBoundJson<SessionUserQuestionResponseDto>(
    '/api/session/user-question',
    {
      sessionId: normalizedSessionId,
      questionId: normalizedQuestionId,
      response: responsePayload,
    },
    'resolve user question',
    bindingOverride
      ? { ...bindingOverride, sessionId: normalizedSessionId }
      : { sessionId: normalizedSessionId },
  );
}
