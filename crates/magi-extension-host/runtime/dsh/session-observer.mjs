import { isAbsolute } from 'node:path';
import { scopeTarget } from '@deepseek-ai/dsh-scope';
import { SESSION_FORMAT_VERSION } from '@deepseek-ai/dsh-session/types';

// An observation carries the native session identity. Live Session and Agent
// handles are separate contracts, backed by revisioned daemon snapshots.
export function publishCommittedSessionEvent(ctx, { sessionId, eventSeq, occurredAt, observation }) {
  if (typeof sessionId !== 'string' || !sessionId || !Number.isSafeInteger(occurredAt) || occurredAt < 0) {
    throw new TypeError('Invalid committed session event');
  }
  if (observation?.kind === 'sessionCreated') {
    if (eventSeq !== undefined) throw new TypeError('Session creation is not a turn-log event');
    const { cwd } = observation;
    if (cwd !== undefined && (typeof cwd !== 'string' || !isAbsolute(cwd))) {
      throw new TypeError('Invalid initial working directory');
    }
    const header = Object.freeze({
      version: SESSION_FORMAT_VERSION, id: sessionId, createdAt: occurredAt, isSeeded: false,
      ...(cwd === undefined ? {} : { cwd }),
    });
    const session = Object.freeze({ id: sessionId, header });
    emitSessionObservation(ctx, session, 'session/created');
    return;
  }
  if (!Number.isSafeInteger(eventSeq) || eventSeq <= 0) {
    throw new TypeError('Invalid committed session event sequence');
  }
  const session = Object.freeze({ id: sessionId });
  const event = Object.freeze({ seq: eventSeq, time: occurredAt, ...toSessionEvent(observation) });
  emitSessionObservation(ctx, session, 'session/event', event);
}

function emitSessionObservation(ctx, session, name, ...args) {
  const callbacks = ctx.events.dispatch('emit', [scopeTarget(session, undefined), name, session, ...args]);
  for (const callback of callbacks) {
    try {
      Promise.resolve(callback(session, ...args)).catch(error => ctx.logger.warn(error));
    } catch (error) {
      ctx.logger.warn(error);
    }
  }
}

export function toSessionEvent(observation) {
  if (observation?.kind === 'textInput') {
    const { messageId, text } = observation;
    if (typeof messageId !== 'string' || !messageId || typeof text !== 'string' || !text) {
      throw new TypeError('Invalid committed text input');
    }
    return {
      type: 'user/message',
      surfaceOp: 'append',
      data: Object.freeze({
        id: messageId, role: 'user', source: Object.freeze({ kind: 'user' }),
        content: Object.freeze([Object.freeze({ type: 'text', text })]),
      }),
    };
  }
  if (observation?.kind === 'requestHeader') return toRequestHeaderEvent(observation);
  if (observation?.kind === 'requestContext') return toRequestContextEvent(observation);
  if (observation?.kind === 'subagentCatalog') return toSubagentCatalogEvent(observation);
  if (observation?.kind === 'subagentDescriptor') return toSubagentDescriptorEvent(observation);
  if (observation?.kind === 'commandRun') return toCommandRunEvent(observation);
  if (observation?.kind === 'commandDone') return toCommandDoneEvent(observation);
  if (observation?.kind === 'sessionTitle') return toSessionTitleEvent(observation);
  const { kind, turn, outcome } = observation ?? {};
  if (!Number.isSafeInteger(turn) || turn <= 0) throw new TypeError('Invalid committed turn number');
  if (kind === 'turnStarted') return { type: 'turn/start', data: Object.freeze({ turn }) };
  if (kind === 'toolCall' || kind === 'toolResult') return toToolEvent(observation);
  if (kind === 'assistantMessage') return toAssistantEvent(observation);
  if (kind === 'assistantAttempt') return toAssistantAttemptEvent(observation);
  if (kind === 'systemMessage') return toSystemMessageEvent(observation);
  if (kind === 'stepStarted' || kind === 'stepEnded') {
    if (!Number.isSafeInteger(observation.step) || observation.step <= 0) throw new TypeError('Invalid committed step number');
    return { type: kind === 'stepStarted' ? 'step/start' : 'step/end', data: Object.freeze({ turn, step: observation.step }) };
  }
  if (kind !== 'turnEnded') throw new TypeError('Invalid committed turn event');
  let reason;
  switch (outcome?.kind) {
    case 'completed': reason = { kind: 'completed' }; break;
    case 'userInterrupted': reason = { kind: 'aborted', reason: Object.freeze({ kind: 'user' }) }; break;
    case 'failed':
      if (typeof outcome.message !== 'string' || !outcome.message) throw new TypeError('Missing committed failure message');
      reason = { kind: 'error', error: Object.freeze({ message: outcome.message, code: outcome.code ?? 'UNKNOWN' }) };
      break;
    default: throw new TypeError('Invalid committed turn outcome');
  }
  return { type: 'turn/end', data: Object.freeze({ turn, reason: Object.freeze(reason) }) };
}

function toToolEvent({ kind, turn, step, callId, name, arguments: args, messageId, content, failed, error }) {
  if (!Number.isSafeInteger(step) || step <= 0 || typeof callId !== 'string' || !callId) {
    throw new TypeError('Invalid committed model tool identity');
  }
  if (kind === 'toolCall') {
    if (typeof name !== 'string' || !name || typeof args !== 'string') {
      throw new TypeError('Invalid committed tool call');
    }
    return { type: 'tool/call', data: Object.freeze({ turn, step, callId, name, arguments: args }) };
  }
  if (typeof messageId !== 'string' || !messageId || typeof content !== 'string' || typeof failed !== 'boolean') {
    throw new TypeError('Invalid committed tool result');
  }
  const message = Object.freeze({
    id: messageId, role: 'tool', toolCallId: callId,
    source: Object.freeze({ kind: 'tool', callId }),
    content: Object.freeze([Object.freeze({ type: 'text', text: content })]),
    ...(failed ? { isError: true } : {}),
  });
  return {
    type: 'tool/result', surfaceOp: 'append',
    data: Object.freeze({ turn, step, message, ...(error === undefined ? {} : { error: Object.freeze(error) }) }),
  };
}

function toAssistantEvent({ turn, step, messageId, text, toolCalls, provider, model, usage, stream }) {
  if (!Number.isSafeInteger(step) || step <= 0 || typeof messageId !== 'string' || !messageId
    || typeof text !== 'string' || !Array.isArray(toolCalls)
    || typeof provider !== 'string' || !provider || typeof model !== 'string' || !model) {
    throw new TypeError('Invalid committed assistant message');
  }
  const content = [];
  if (text) content.push(Object.freeze({ type: 'text', text }));
  for (const call of toolCalls) {
    if (typeof call?.id !== 'string' || !call.id || typeof call.name !== 'string' || !call.name
      || typeof call.arguments !== 'string') {
      throw new TypeError('Invalid committed assistant tool call');
    }
    content.push(Object.freeze({ type: 'tool-call', id: call.id, name: call.name, arguments: call.arguments }));
  }
  if (content.length === 0) throw new TypeError('Empty committed assistant message');
  // The compact attempt stream the daemon committed for this response; absent (not guessed) when none was recorded.
  if (stream !== undefined) assertValidStream(stream, 'Invalid committed assistant stream');
  let tokenUsage;
  if (usage !== undefined) {
    const counts = ['inputTokens', 'outputTokens'];
    const optional = ['cacheReadTokens', 'cacheWriteTokens'];
    if (typeof usage !== 'object' || usage === null
      || !counts.every(key => Number.isSafeInteger(usage[key]) && usage[key] >= 0)
      || !optional.every(key => usage[key] === undefined || (Number.isSafeInteger(usage[key]) && usage[key] >= 0))
      || Object.keys(usage).some(key => !counts.includes(key) && !optional.includes(key))) {
      throw new TypeError('Invalid committed assistant usage');
    }
    tokenUsage = Object.freeze({ ...usage });
  }
  return {
    type: 'assistant/message', surfaceOp: 'append',
    data: Object.freeze({
      turn, step,
      message: Object.freeze({
        id: messageId, role: 'assistant',
        source: Object.freeze({ kind: 'model', provider, model }),
        content: Object.freeze(content),
      }),
      stream: stream === undefined ? Object.freeze([]) : deepFreeze(stream),
      ...(tokenUsage === undefined ? {} : { usage: tokenUsage }),
    }),
  };
}

function toAssistantAttemptEvent({ turn, step, stream }) {
  if (!Number.isSafeInteger(step) || step <= 0) {
    throw new TypeError('Invalid committed assistant attempt step');
  }
  assertValidStream(stream, 'Invalid committed assistant attempt stream');
  return {
    type: 'assistant/attempt',
    data: Object.freeze({ turn, step, stream: deepFreeze(stream) }),
  };
}

function toRequestHeaderEvent({ header, reason }) {
  if (!['initial', 'resume', 'change', 'series'].includes(reason)) {
    throw new TypeError('Invalid committed request/header reason');
  }
  if (typeof header !== 'object' || header === null || Array.isArray(header) || Object.hasOwn(header, 'system')) {
    throw new TypeError('Invalid committed request/header payload');
  }
  const config = header.config;
  if (typeof config !== 'object' || config === null || Array.isArray(config)
    || typeof config.provider !== 'string' || !config.provider
    || typeof config.model !== 'string' || !config.model) {
    throw new TypeError('Invalid committed request/header config');
  }
  if (config.reasoningEffort !== undefined && (typeof config.reasoningEffort !== 'string' || !config.reasoningEffort)) {
    throw new TypeError('Invalid committed request/header reasoningEffort');
  }
  if (config.temperature !== undefined && (typeof config.temperature !== 'number' || !Number.isFinite(config.temperature))) {
    throw new TypeError('Invalid committed request/header temperature');
  }
  if (config.maxTokens !== undefined && (!Number.isSafeInteger(config.maxTokens) || config.maxTokens <= 0)) {
    throw new TypeError('Invalid committed request/header maxTokens');
  }
  if (config.stop !== undefined && (!Array.isArray(config.stop) || config.stop.some(item => typeof item !== 'string'))) {
    throw new TypeError('Invalid committed request/header stop');
  }
  const normalizedConfig = {
    provider: config.provider,
    model: config.model,
    ...(config.reasoningEffort === undefined ? {} : { reasoningEffort: config.reasoningEffort }),
    ...(config.temperature === undefined ? {} : { temperature: config.temperature }),
    ...(config.maxTokens === undefined ? {} : { maxTokens: config.maxTokens }),
    ...(config.stop === undefined || config.stop.length === 0 ? {} : { stop: [...config.stop] }),
  };
  let normalizedTools;
  if (header.tools !== undefined) {
    if (!Array.isArray(header.tools) || header.tools.length === 0) {
      throw new TypeError('Invalid committed request/header tools');
    }
    normalizedTools = header.tools.map(tool => {
      if (typeof tool !== 'object' || tool === null || Array.isArray(tool)
        || typeof tool.name !== 'string' || !tool.name
        || typeof tool.description !== 'string'
        || typeof tool.parameters !== 'object' || tool.parameters === null || Array.isArray(tool.parameters)) {
        throw new TypeError('Invalid committed request/header tool schema');
      }
      return {
        name: tool.name,
        description: tool.description,
        parameters: structuredClone(tool.parameters),
      };
    });
  }
  const normalizedHeader = {
    config: normalizedConfig,
    ...(normalizedTools === undefined ? {} : { tools: normalizedTools }),
  };
  return {
    type: 'request/header',
    data: Object.freeze({ header: deepFreeze(normalizedHeader), reason }),
  };
}

function toRequestContextEvent({ provider, model, contextWindow }) {
  if (typeof provider !== 'string' || !provider || typeof model !== 'string' || !model) {
    throw new TypeError('Invalid committed request/context route');
  }
  if (contextWindow !== undefined && (!Number.isSafeInteger(contextWindow) || contextWindow <= 0)) {
    throw new TypeError('Invalid committed request/context contextWindow');
  }
  return {
    type: 'request/context',
    data: Object.freeze({
      provider,
      model,
      ...(contextWindow === undefined ? {} : { contextWindow }),
    }),
  };
}

function toSystemMessageEvent({ turn, step, messageId, text }) {
  if (!Number.isSafeInteger(step) || step <= 0
    || typeof messageId !== 'string' || !messageId
    || typeof text !== 'string') {
    throw new TypeError('Invalid committed system message');
  }
  const content = text ? Object.freeze([Object.freeze({ type: 'text', text })]) : Object.freeze([]);
  return {
    type: 'system/message',
    surfaceOp: 'append',
    data: Object.freeze({
      turn,
      step,
      message: Object.freeze({
        id: messageId,
        role: 'system',
        source: Object.freeze({ kind: 'system-prompt' }),
        content,
      }),
    }),
  };
}

function toSubagentCatalogEvent({ childId, childCreatedAt, mode, label }) {
  if (typeof childId !== 'string' || !childId
    || !Number.isSafeInteger(childCreatedAt) || childCreatedAt < 0
    || mode !== 'one-shot') {
    throw new TypeError('Invalid committed subagent/catalog observation');
  }
  if (label !== undefined && typeof label !== 'string') {
    throw new TypeError('Invalid committed subagent/catalog label');
  }
  return {
    type: 'subagent/catalog',
    data: Object.freeze({
      version: 1,
      childId,
      childCreatedAt,
      mode: 'one-shot',
      ...(label ? { label } : {}),
    }),
  };
}

function toSubagentDescriptorEvent({ version, mode, provider, label }) {
  if (version !== 3 || mode !== 'one-shot' || typeof provider !== 'string' || !provider) {
    throw new TypeError('Invalid committed subagent/descriptor observation');
  }
  if (label !== undefined && typeof label !== 'string') {
    throw new TypeError('Invalid committed subagent/descriptor label');
  }
  return {
    type: 'subagent/descriptor',
    data: Object.freeze({
      version: 3,
      mode: 'one-shot',
      provider,
      ...(label ? { label } : {}),
    }),
  };
}

function toCommandRunEvent({ commandId, name, args }) {
  if (typeof commandId !== 'string' || !commandId || typeof name !== 'string' || !name) {
    throw new TypeError('Invalid committed command/run observation');
  }
  if (args !== undefined && typeof args !== 'string') {
    throw new TypeError('Invalid committed command/run args');
  }
  return {
    type: 'command/run',
    data: Object.freeze({
      commandId,
      name,
      ...(args === undefined ? {} : { args }),
      source: Object.freeze({ kind: 'user' }),
    }),
  };
}

function toCommandDoneEvent({ commandId, outcome, text, sourceEventSeq }) {
  if (typeof commandId !== 'string' || !commandId || (outcome !== 'success' && outcome !== 'error')) {
    throw new TypeError('Invalid committed command/done observation');
  }
  if (text !== undefined && typeof text !== 'string') {
    throw new TypeError('Invalid committed command/done text');
  }
  if (outcome === 'error' && (!text || !text.trim())) {
    throw new TypeError('Committed command/done error requires non-empty text');
  }
  if (sourceEventSeq !== undefined) {
    if (outcome !== 'success' || !Number.isSafeInteger(sourceEventSeq) || sourceEventSeq < 0) {
      throw new TypeError('Invalid committed command/done sourceEventSeq');
    }
  }
  return {
    type: 'command/done',
    data: Object.freeze({
      commandId,
      kind: outcome,
      ...(text === undefined ? {} : { text }),
      ...(sourceEventSeq === undefined ? {} : { sourceEventSeq }),
    }),
  };
}

function toSessionTitleEvent({ title }) {
  if (typeof title !== 'string' || !title.trim()) {
    throw new TypeError('Invalid committed session/title observation');
  }
  return {
    type: 'session/title',
    data: Object.freeze({
      title,
      messageSeqs: Object.freeze([]),
      source: Object.freeze({ kind: 'user' }),
    }),
  };
}

function assertValidStream(stream, message) {
  if (!Array.isArray(stream) || stream.length === 0
    || stream.some(record => typeof record?.type !== 'string')) {
    throw new TypeError(message);
  }
}

function deepFreeze(value) {
  if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const child of Object.values(value)) deepFreeze(child);
  }
  return value;
}
