// Session gateway: exposes the DSH `session` Typert namespace backed by the daemon's authoritative
// session facts (`host.session.list` / `host.session.get` / `host.session.changed` / `host.session.committed`).
// Tracks the last published directory snapshot to emit dense `api-session/*` deltas and serves session creation,
// turn submission, queue updates, cancellation, renaming (with title event seq), pagination, live follow,
// projections, and read-only subagent views.
import { AttachmentError } from '@deepseek-ai/dsh-attachment';
import { RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol';
import { AssistantStreamAccumulator } from '@deepseek-ai/dsh-llm/assistant-stream';
import { SESSION_FORMAT_VERSION } from '@deepseek-ai/dsh-session/types';
import { isAppendSurfaceEvent } from '@deepseek-ai/dsh-session';
import { foldSessionTitle } from '@deepseek-ai/dsh-session-title';
import { markRemoteMethod } from './remote-methods.mjs';
import { toSessionEvent } from './session-observer.mjs';

const MAX_CHANGES = 256;
const RETRY_MS = 1000;

// Server-side `SessionSummary` of the DSH session controller, from one daemon directory fact.
// Only members with a fact are present: the title appears once the session has a real one (the daemon omits the
// new-session placeholder), and there is no parent or origin.
export function toSessionSummary(fact) {
  if (typeof fact?.sessionId !== 'string' || !fact.sessionId
    || !Number.isSafeInteger(fact.updatedAt) || fact.updatedAt < 0
    || !Number.isSafeInteger(fact.messageCount) || fact.messageCount < 0
    || typeof fact.isRunning !== 'boolean' || typeof fact.status !== 'string') {
    throw new TypeError('Invalid session directory fact');
  }
  return Object.freeze({
    agentAvailable: fact.status === 'Active',
    formatStatus: 'current',
    sessionId: fact.sessionId,
    updatedAt: fact.updatedAt,
    running: fact.isRunning,
    blank: fact.messageCount === 0,
    ...(typeof fact.title === 'string' && fact.title ? { title: fact.title } : {}),
    ...(typeof fact.cwd === 'string' && fact.cwd ? { cwd: fact.cwd } : {}),
  });
}

// Message budget of a history request that omits `maxMessages`
// (`@deepseek-ai/dsh-api-session-controller` 0.2.2-rc.3, src/history.ts).
export const DEFAULT_MAX_MESSAGES = 50;
const MESSAGE_TYPES = new Set(['user/message', 'assistant/message']);
const FOLLOW_QUEUE_LIMIT = 4096;
const badRequest = message => new RemoteError('gateway/bad-request', message, {});

// Validate and resolve a `page`/`follow` window exactly as the original session controller does.
export function resolveHistoryRequest(request) {
  const { throughSeq, beforeSeq, maxMessages, turnWindow } = request;
  if (throughSeq !== undefined && (!Number.isSafeInteger(throughSeq) || throughSeq < -1 || Object.is(throughSeq, -0))) {
    throw badRequest('throughSeq must be an integer greater than or equal to -1');
  }
  if (beforeSeq !== undefined && (!Number.isSafeInteger(beforeSeq) || beforeSeq < 0 || Object.is(beforeSeq, -0))) {
    throw badRequest('beforeSeq must be a non-negative safe integer');
  }
  if (maxMessages !== undefined && (!Number.isSafeInteger(maxMessages) || maxMessages <= 0)) {
    throw badRequest('maxMessages must be a positive safe integer');
  }
  if (turnWindow !== undefined) {
    if (!Number.isSafeInteger(turnWindow.minMessages) || turnWindow.minMessages <= 0
      || turnWindow.minMessages > (maxMessages ?? DEFAULT_MAX_MESSAGES)) {
      throw badRequest('turnWindow.minMessages must be a positive safe integer no greater than maxMessages');
    }
    if (!Number.isSafeInteger(turnWindow.minTurns) || turnWindow.minTurns <= 0) {
      throw badRequest('turnWindow.minTurns must be a positive safe integer');
    }
  }
  return { throughSeq, beforeSeq, turnWindow, maxMessages: maxMessages ?? DEFAULT_MAX_MESSAGES };
}

// One message-aligned backwards page of a contiguous event log that starts at seq 0: walk back from the
// cut, count append-origin user/assistant messages, and stop at the budget (or, with a turn window, at a
// turn start after both minima). Same rule as `paginate` in the original session controller.
export function paginate(events, beforeSeq, maxMessages, throughSeq, turnWindow) {
  const end = Math.min(throughSeq + 1, beforeSeq ?? throughSeq + 1);
  let count = 0;
  let turns = 0;
  let cut = 0;
  for (let index = end - 1; index >= 0; index--) {
    const event = events[index];
    if (turnWindow !== undefined && event.type === 'turn/start') {
      turns++;
      if (count >= turnWindow.minMessages && turns >= turnWindow.minTurns) {
        cut = index;
        break;
      }
    }
    if (!MESSAGE_TYPES.has(event.type) || !isAppendSurfaceEvent(event)) continue;
    count++;
    let groupStart = event.seq;
    for (const source of event.sourceEventSeqs ?? []) if (source < groupStart) groupStart = source;
    if (count >= maxMessages) {
      cut = groupStart;
      break;
    }
  }
  return { events: events.slice(cut, end), hasMore: cut > 0 };
}

const entryFor = event => ({ type: 'event', event });

// A bounded queue of committed observations and (optionally) live assistant-stream frames for one followed session.
class FollowFeed {
  #items = [];
  #wake;
  #failure;
  #closed = false;
  constructor({ assistantStream = false } = {}) {
    this.assistantStream = assistantStream;
  }
  push(item) {
    if (this.#closed || this.#failure !== undefined) return;
    if (this.#items.length >= FOLLOW_QUEUE_LIMIT) {
      this.#failure = new RemoteError('gateway/overflow', 'session follow fell too far behind; reopen it', {});
    } else {
      this.#items.push(item);
    }
    this.#wake?.();
  }
  fail(error) {
    if (this.#closed || this.#failure !== undefined) return;
    this.#failure = error;
    this.#wake?.();
  }
  close() {
    this.#closed = true;
    this.#wake?.();
  }
  async *items(signal) {
    // One abort listener for the whole iteration, removed on exit, instead of one per wait.
    const onAbort = () => this.#wake?.();
    signal.addEventListener('abort', onAbort);
    try {
      while (true) {
        if (this.#failure) throw this.#failure;
        const item = this.#items.shift();
        if (item !== undefined) { yield item; continue; }
        if (this.#closed || signal.aborted) return;
        await new Promise(resolve => { this.#wake = resolve; });
        this.#wake = undefined;
      }
    } finally {
      signal.removeEventListener('abort', onAbort);
    }
  }
}

function startedAfterSeqFromSnapshot(snapshot, afterEventSeq) {
  const { events, sourceSequences, through } = snapshot;
  if (Array.isArray(sourceSequences) && sourceSequences.length === events.length) {
    for (let index = events.length - 1; index >= 0; index--) {
      if (sourceSequences[index] <= afterEventSeq) return events[index].seq;
    }
    return -1;
  }
  return afterEventSeq >= through ? events.length - 1 : -1;
}

function snapshotContainsSettlement(events, startedAfterSeq, turn, step) {
  for (let index = events.length - 1; index >= 0; index--) {
    const event = events[index];
    if (event.seq <= startedAfterSeq) break;
    const isMessage = event.type === 'assistant/message' && isAppendSurfaceEvent(event);
    const isAttempt = event.type === 'assistant/attempt';
    if ((isMessage || isAttempt) && event.data?.turn === turn && event.data?.step === step) {
      return true;
    }
  }
  return false;
}

function foldSubagentCatalog(events) {
  const entries = [];
  for (const event of events) {
    if (event.type !== 'subagent/catalog' || !event.data) continue;
    const { childId, childCreatedAt, mode, label, name } = event.data;
    if (typeof childId !== 'string' || !childId || !Number.isSafeInteger(childCreatedAt)) continue;
    entries.push({
      id: childId,
      createdAt: childCreatedAt,
      mode,
      ...(label !== undefined ? { label } : {}),
      ...(name !== undefined ? { name } : {}),
    });
  }
  return entries;
}

function foldSubagentProjections(events) {
  let identity = null;
  let createdAt;
  let descriptorSeen = false;
  let settledMs = 0;
  let active;
  let pendingTurnStart;
  let lastTurnCompleted;
  for (const event of events) {
    if (event.type === 'subagent/descriptor' && event.data?.version === 3
      && (event.data.mode === 'one-shot' || event.data.mode === 'continuable')) {
      identity = {
        mode: event.data.mode,
        ...(event.data.label !== undefined ? { label: event.data.label } : {}),
        seq: event.seq,
      };
      if (createdAt === undefined) createdAt = event.time;
      const activeSince = active?.since ?? pendingTurnStart;
      descriptorSeen = true;
      settledMs = 0;
      active = activeSince === undefined ? undefined : { since: activeSince, through: event.time };
      pendingTurnStart = undefined;
      lastTurnCompleted = undefined;
      continue;
    }
    if (event.type === 'turn/start') {
      lastTurnCompleted = undefined;
      if (descriptorSeen) active = { since: event.time, through: event.time };
      else pendingTurnStart = event.time;
      continue;
    }
    if (event.type === 'turn/end') {
      if (!descriptorSeen) {
        pendingTurnStart = undefined;
        continue;
      }
      if (active !== undefined) {
        settledMs += Math.max(0, event.time - active.since);
        active = undefined;
        lastTurnCompleted = event.data?.reason?.kind === 'completed';
      }
      continue;
    }
    if (active !== undefined) active = { since: active.since, through: event.time };
  }
  const subagentTiming = {
    settledMs,
    ...(active === undefined ? {} : { active }),
    ...(lastTurnCompleted === undefined ? {} : { lastTurnCompleted }),
  };
  return { subagent: identity, subagentTiming, createdAt };
}

export function createSessionGateway({ root, requestOwner, hostGeneration, readLog, resolveAgent, onDiagnostic = () => {} }) {
  // Open follows by feed key (`sessionId` for main sessions, `${parentId}\0subagent\0${childId}` for subagents).
  const feeds = new Map();
  // Active `session.control` streams receiving live projection updates.
  const controlFeeds = new Set();
  const broadcastControl = frame => {
    for (const controlFeed of controlFeeds) controlFeed.push(frame);
  };
  const customProjectionUnsub = root?.get?.('sessionProjections')?.onEventChange?.((session, changes, seq) => {
    if (!session?.id || !Array.isArray(changes) || changes.length === 0) return;
    broadcastControl({
      type: 'projection',
      sessionId: session.id,
      seq,
      changes: changes.map(c => ({ key: c.key, val: c.val })),
    });
  });
  function mergeCustomProjections(sessionId, builtinValues) {
    const registry = root?.get?.('sessionProjections');
    const session = root?.get?.('sessions')?.get?.(sessionId);
    if (!registry || !session) return builtinValues;
    try {
      const snap = registry.snapshot(session);
      if (snap?.values && typeof snap.values === 'object') {
        return { ...snap.values, ...builtinValues };
      }
    } catch {
      // Unloaded session history or no custom units registered; return authoritative builtin values.
    }
    return builtinValues;
  }
  // Process-local live assistant stream state per session (single owner of contiguous revision & reconnect baseline).
  const liveStreams = new Map();
  const liveStreamOf = sessionId => {
    let state = liveStreams.get(sessionId);
    if (state === undefined) {
      state = { revision: 0, ordinal: 0, activeAttempt: undefined };
      liveStreams.set(sessionId, state);
    }
    return state;
  };
  const fanOutLive = (sessionId, state, frame, extra = {}) => {
    state.ordinal += 1;
    const ordinal = state.ordinal;
    const registered = feeds.get(sessionId);
    if (registered !== undefined) {
      const item = { kind: 'assistant-stream', ordinal, frame, ...extra };
      for (const feed of registered) if (feed.assistantStream) feed.push(item);
    }
    return ordinal;
  };
  const overflowLiveFeeds = (sessionId, message) => {
    const registered = feeds.get(sessionId);
    if (registered === undefined) return;
    const error = new RemoteError('gateway/overflow', message, {});
    for (const feed of registered) if (feed.assistantStream) feed.fail(error);
  };
  // The last published directory, keyed by session id; undefined until a client has listed.
  let published;
  let known = -1;
  let queue = Promise.resolve();
  let retry;
  let closed = false;
  const serially = task => {
    const run = queue.then(() => (closed ? undefined : task()));
    queue = run.catch(() => {});
    return run;
  };

  // The committed log of one user session (or a read-only subagent projected from its parent log) as a
  // dense DSH event log, together with the daemon's watermark it covers.
  async function openLog(sessionId, signal, subagentTaskId) {
    if (typeof sessionId !== 'string' || !sessionId) throw badRequest('sessionId must not be empty');
    const reply = await requestOwner('host.session.get', { sessionId });
    signal?.throwIfAborted();
    const fact = reply?.session;
    if (!fact) throw new RemoteError('session/not-found', `session "${sessionId}" not found`, { sessionId });
    let snapshot;
    try {
      snapshot = await readLog({
        id: sessionId,
        createdAt: fact.createdAt,
        initialDirectory: fact.cwd ?? null,
        ...(subagentTaskId ? { subagentTaskId } : {}),
      }, signal);
    } catch (error) {
      if (signal?.aborted) throw error;
      throw new RemoteError('gateway/internal', String(error?.message ?? error), {});
    }
    return { snapshot, fact };
  }

  function resolveAddress(request) {
    const address = request?.address;
    if (address?.kind === 'session' && typeof address.sessionId === 'string' && address.sessionId) {
      return { kind: 'session', sessionId: address.sessionId, feedKey: address.sessionId };
    }
    if (address?.kind === 'subagent'
      && typeof address.parentSessionId === 'string' && address.parentSessionId
      && typeof address.childSessionId === 'string' && address.childSessionId) {
      const mode = address.mode ?? 'unknown';
      if (mode !== 'unknown' && mode !== 'one-shot') {
        throw new RemoteError('subagent/unauthorized', 'subagent mode does not match read-only one-shot mapping', {
          childSessionId: address.childSessionId,
        });
      }
      return {
        kind: 'subagent',
        parentSessionId: address.parentSessionId,
        childSessionId: address.childSessionId,
        feedKey: `${address.parentSessionId}\0subagent\0${address.childSessionId}`,
      };
    }
    throw badRequest('address must name a session or subagent');
  }

  async function openAddressLog(address, signal) {
    if (address.kind === 'session') {
      const { snapshot, fact } = await openLog(address.sessionId, signal);
      return { snapshot, fact, subagent: undefined };
    }
    let opened;
    try {
      opened = await openLog(address.parentSessionId, signal, address.childSessionId);
    } catch (error) {
      if (error instanceof RemoteError && error.code === 'session/not-found') {
        throw new RemoteError('subagent/not-found', 'subagent is unavailable', {
          parentSessionId: address.parentSessionId,
          childSessionId: address.childSessionId,
        });
      }
      throw error;
    }
    const subagent = foldSubagentProjections(opened.snapshot.events);
    if (subagent.subagent === null) {
      throw new RemoteError('subagent/not-found', 'subagent is unavailable', {
        parentSessionId: address.parentSessionId,
        childSessionId: address.childSessionId,
      });
    }
    return { ...opened, subagent };
  }

  const emit = (...args) => { if (!closed) root.emit(...args); };

  async function readDirectory() {
    const reply = await requestOwner('host.session.list', {});
    if (!Number.isSafeInteger(reply?.factsSequence) || !Array.isArray(reply.sessions)) {
      throw new TypeError('Invalid session directory');
    }
    const summaries = new Map();
    for (const fact of reply.sessions) {
      const summary = toSessionSummary(fact);
      if (summaries.has(summary.sessionId)) throw new TypeError('Duplicate session in the directory');
      summaries.set(summary.sessionId, summary);
    }
    return { sequence: reply.factsSequence, summaries };
  }

  // Emit what differs between the published directory and `next`, then adopt it.
  function publish(next, sequence) {
    const previous = published ?? new Map();
    for (const [id, summary] of next) {
      const before = previous.get(id);
      if (before === undefined) emit('api-session/added', summary);
      else {
        if (before.running !== summary.running) emit('api-session/status', id, summary.running);
        if (before.updatedAt !== summary.updatedAt) emit('api-session/activity', id, summary.updatedAt);
      }
    }
    for (const id of previous.keys()) if (!next.has(id)) emit('api-session/removed', id);
    published = next;
    known = Math.max(known, sequence);
  }

  const scheduleRetry = () => {
    if (retry !== undefined || closed) return;
    retry = setTimeout(() => {
      retry = undefined;
      void serially(resync).catch(scheduleRetry);
    }, RETRY_MS);
    retry.unref?.();
  };

  async function resync() {
    const { sequence, summaries } = await readDirectory();
    publish(summaries, sequence);
  }

  // Re-read one session. The directory sequence only advances by the notification being applied: a read can
  // already reflect later notifications for other sessions, which must still be applied when they arrive.
  async function reconcileOn(directory, sessionId) {
    const reply = await requestOwner('host.session.get', { sessionId });
    if (!Number.isSafeInteger(reply?.factsSequence)) throw new TypeError('Invalid session fact');
    const next = new Map(directory);
    // The directory lists active sessions only; an archived or missing session leaves it.
    const summary = reply.session === null ? undefined : toSessionSummary(reply.session);
    if (summary === undefined || summary.agentAvailable === false) next.delete(sessionId);
    else next.set(sessionId, summary);
    return next;
  }

  class SessionController extends TypertRemoteService {
    constructor(ctx) {
      super(ctx, 'sessionController', { namespace: 'session' });
    }

    // The contract's reserved list request is named `_request`; the gateway matches arguments by name.
    async list(_request, signal) {
      signal?.throwIfAborted();
      if (_request?.cursor !== undefined) {
        throw new RemoteError('gateway/bad-request', 'The session directory is returned whole; it has no cursor', {});
      }
      const items = await serially(async () => {
        const { sequence, summaries } = await readDirectory();
        publish(summaries, sequence);
        return [...summaries.values()];
      });
      return { items };
    }

    // Create an empty user session. The scope is the optional workspace; the directory is always that scope's,
    // and the daemon does not select the new session for the user. A caller-chosen directory, session identity or
    // agent preset has no Magi equivalent, so they are refused rather than ignored.
    async create(request, signal) {
      signal?.throwIfAborted();
      const unsupported = ['cwd', 'sessionId', 'agentPreset'].filter(key => request?.[key] !== undefined);
      if (unsupported.length > 0) {
        throw new RemoteError('gateway/bad-request', `create does not support ${unsupported.join(', ')}`, {});
      }
      if (request?.workspaceId !== undefined && (typeof request.workspaceId !== 'string' || !request.workspaceId)) {
        throw new RemoteError('gateway/bad-request', 'workspaceId must be a non-empty string', {});
      }
      let reply;
      try {
        reply = await requestOwner('host.session.create', request?.workspaceId === undefined ? {} : { workspaceId: request.workspaceId });
      } catch (error) {
        throw new RemoteError('gateway/bad-request', String(error?.message ?? error), {});
      }
      if (typeof reply?.sessionId !== 'string' || !reply.sessionId) throw new TypeError('Invalid created session');
      if (typeof resolveAgent === 'function') {
        await resolveAgent(reply.sessionId).catch(() => {});
      }
      return { sessionId: reply.sessionId };
    }

    // Submit user input on behalf of the page. Text blocks are joined in order (blank-line separated),
    // encoded images ride with the turn, and staged `file` upload receipts resolve through `ctx.fileUploads`.
    // `clientTimeZone` has no Magi counterpart and is not forwarded.
    async prompt(request, signal) {
      signal?.throwIfAborted();
      const bad = message => new RemoteError('gateway/bad-request', message, {});
      if (typeof request?.sessionId !== 'string' || !request.sessionId) throw bad('prompt needs a sessionId');
      if (typeof request.requestId !== 'string' || !request.requestId) throw bad('prompt needs a requestId');
      if (request.mode !== 'queue' && request.mode !== 'steer') throw bad('prompt mode must be queue or steer');
      if (!Array.isArray(request.content) || request.content.length === 0) throw bad('prompt needs content');
      const texts = [];
      const images = [];
      let cachedAgent;
      for (const part of request.content) {
        if (part?.type === 'text' && typeof part.text === 'string') texts.push(part.text);
        else if (part?.type === 'image' && typeof part.mediaType === 'string' && typeof part.data === 'string') {
          images.push({ mediaType: part.mediaType, data: part.data, ...(typeof part.name === 'string' && part.name ? { name: part.name } : {}) });
        } else if (part?.type === 'file') {
          const uploads = root?.get?.('fileUploads');
          const attachments = root?.get?.('attachments');
          if (!uploads || typeof part.receiptId !== 'string' || !part.receiptId) {
            throw new RemoteError('session/attachment-invalid', 'file upload receipt is invalid or unavailable', {
              reason: 'INVALID_FILE_RECEIPT',
            });
          }
          if (cachedAgent === undefined) {
            cachedAgent = typeof resolveAgent === 'function'
              ? await resolveAgent(request.sessionId)
              : root?.get?.('agents')?.get?.(request.sessionId);
          }
          const staged = cachedAgent ? uploads.resolve(cachedAgent, part.receiptId) : undefined;
          if (!staged) {
            throw new RemoteError('session/attachment-invalid', `unknown upload receipt "${part.receiptId}"`, {
              reason: 'UPLOAD_RECEIPT_NOT_FOUND',
            });
          }
          if (staged.type === 'image' && attachments) {
            const stored = await attachments.readImage(staged.attachment, signal);
            images.push({
              mediaType: stored.ref.mediaType,
              data: Buffer.from(stored.data).toString('base64'),
              ...(stored.ref.name ? { name: stored.ref.name } : {}),
            });
          } else if (staged.type === 'file') {
            const hostPath = attachments?.fileHostPath?.(staged.attachment);
            const label = staged.attachment?.name ?? part.receiptId;
            texts.push(hostPath ? `[Attached file: ${label} (${hostPath})]` : `[Attached file: ${label}]`);
          } else {
            throw new RemoteError('session/attachment-invalid', 'unsupported staged attachment', {
              reason: 'UNSUPPORTED_STAGED_ATTACHMENT',
            });
          }
        }
        else throw bad('unsupported prompt content');
      }
      const text = texts.join('\n\n');
      if (text.trim() === '' && images.length === 0) throw bad('prompt content is empty');
      try {
        await requestOwner('host.session.prompt', {
          sessionId: request.sessionId, requestId: request.requestId, steer: request.mode === 'steer', text, images,
        });
      } catch (error) {
        throw bad(String(error?.message ?? error));
      }
      return { accepted: true };
    }

    // Change a queued input before it is picked up: replace its text, remove it, or steer it into the running turn.
    // The item identity is the queue's own `queueId`; an edit keeps everything but the text (images, references,
    // command, goal mode and access profile come from the original input).
    async updateQueue(request, signal) {
      signal?.throwIfAborted();
      const bad = message => new RemoteError('gateway/bad-request', message, {});
      if (typeof request?.sessionId !== 'string' || !request.sessionId) throw bad('updateQueue needs a sessionId');
      if (typeof request.itemId !== 'string' || !request.itemId) throw bad('updateQueue needs an itemId');
      let action;
      switch (request.action?.kind) {
        case 'remove': action = { kind: 'remove' }; break;
        case 'steer': action = { kind: 'steer' }; break;
        case 'edit': {
          const blocks = Array.isArray(request.action.content) ? request.action.content : [];
          if (blocks.length === 0 || blocks.some(block => block?.type !== 'text' || typeof block.text !== 'string')) {
            throw bad('edit needs text content');
          }
          const text = blocks.map(block => block.text).join('\n\n');
          if (text.trim() === '') throw bad('edit content is empty');
          action = { kind: 'edit', text };
          break;
        }
        default: throw bad('unsupported queue action');
      }
      try {
        await requestOwner('host.session.queue', { sessionId: request.sessionId, queueId: request.itemId, action });
      } catch (error) {
        throw bad(String(error?.message ?? error));
      }
      return { accepted: true };
    }

    // Stop the session's current turn. The daemon resolves the session's own scope; the caller names only the session.
    async cancel(request, signal) {
      signal?.throwIfAborted();
      if (typeof request?.sessionId !== 'string' || !request.sessionId) {
        throw new RemoteError('gateway/bad-request', 'cancel needs a sessionId', {});
      }
      try {
        await requestOwner('host.session.cancel', { sessionId: request.sessionId });
      } catch (error) {
        throw new RemoteError('gateway/bad-request', String(error?.message ?? error), {});
      }
      return { accepted: true };
    }

    // The title is committed as a session event in the daemon's log; `seq` is that event's sequence in the session log.
    async rename(request, signal) {
      signal?.throwIfAborted();
      if (typeof request?.sessionId !== 'string' || !request.sessionId || typeof request.title !== 'string') {
        throw new RemoteError('gateway/bad-request', 'rename needs a sessionId and a title', {});
      }
      let result;
      try {
        result = await requestOwner('host.session.rename', { sessionId: request.sessionId, title: request.title });
      } catch (error) {
        throw new RemoteError('gateway/bad-request', String(error?.message ?? error), {});
      }
      if (typeof result?.title !== 'string' || !Number.isSafeInteger(result.seq) || result.seq <= 0) {
        throw new RemoteError('gateway/unavailable', 'the daemon returned an invalid rename result', {});
      }
      return { title: result.title, seq: result.seq };
    }

    // Read one durable image using the supplied reference from the shared host attachment store.
    async attachment(request, signal) {
      signal?.throwIfAborted();
      if (typeof request?.sessionId !== 'string' || !request.sessionId
        || typeof request.attachment !== 'object' || request.attachment === null) {
        throw badRequest('attachment requires sessionId and an image attachment reference');
      }
      const store = root.get?.('attachments');
      if (!store) {
        throw new RemoteError('gateway/unavailable', 'Attachment store is unavailable', {});
      }
      try {
        const stored = await store.readImage(request.attachment, signal);
        return {
          attachment: stored.ref,
          data: Buffer.from(stored.data).toString('base64'),
        };
      } catch (error) {
        if (error instanceof AttachmentError) {
          throw new RemoteError('session/attachment-invalid', error.message, { reason: error.code });
        }
        throw new RemoteError('gateway/internal', 'Unable to read image attachment.', {});
      }
    }

    // One message-aligned page of older history at an exact cut (ordinary session or read-only subagent).
    async page(request, signal) {
      signal?.throwIfAborted();
      const address = resolveAddress(request);
      const spec = resolveHistoryRequest(request);
      const { snapshot } = await openAddressLog(address, signal);
      const observed = snapshot.events.length - 1;
      const cursor = spec.throughSeq ?? observed;
      if (cursor > observed) throw badRequest(`session history cut ${cursor} exceeds ${observed}`);
      const page = paginate(snapshot.events, spec.beforeSeq, spec.maxMessages, cursor, spec.turnWindow);
      return { cursor, records: page.events.map(entryFor), hasMore: page.hasMore };
    }

    // Exact sequenced projections derived from the committed log (`title` & `subagentCatalog` for parent sessions).
    async projections(request, signal) {
      signal?.throwIfAborted();
      if (typeof request?.sessionId !== 'string' || !request.sessionId) throw badRequest('sessionId must not be empty');
      try {
        const { snapshot } = await openLog(request.sessionId, signal);
        return {
          kind: 'sequenced',
          asOfSeq: snapshot.events.length - 1,
          values: mergeCustomProjections(request.sessionId, {
            title: foldSessionTitle(snapshot.events)?.title ?? null,
            subagentCatalog: foldSubagentCatalog(snapshot.events),
          }),
        };
      } catch (error) {
        if (error instanceof RemoteError && error.code === 'session/not-found') return null;
        throw error;
      }
    }

    // The opening window, then every later committed event (and optional live assistant-stream frames)
    // in order. Supports both ordinary sessions and read-only subagent addresses.
    async *follow(request, signal) {
      signal.throwIfAborted();
      const address = resolveAddress(request);
      const spec = resolveHistoryRequest(request);
      const wantsAssistantStream = request.assistantStream === true;
      const isSubagent = address.kind === 'subagent';
      const feed = new FollowFeed({ assistantStream: wantsAssistantStream && !isSubagent });
      let registered = feeds.get(address.feedKey);
      if (registered === undefined) feeds.set(address.feedKey, registered = new Set());
      registered.add(feed);
      try {
        const { snapshot, subagent } = await openAddressLog(address, signal);
        const events = snapshot.events;
        const cursor = events.length - 1;
        const page = paginate(events, undefined, spec.maxMessages, cursor, spec.turnWindow);
        let assistantStream;
        let ordinalCut = 0;
        let settledAttemptId;
        if (wantsAssistantStream) {
          const state = isSubagent ? undefined : liveStreams.get(address.sessionId);
          if (state === undefined) {
            assistantStream = { revision: 0 };
            ordinalCut = 0;
          } else if (state.activeAttempt === undefined) {
            assistantStream = { revision: state.revision };
            ordinalCut = state.ordinal;
          } else if (state.activeAttempt.afterEventSeq > snapshot.through) {
            assistantStream = { revision: state.activeAttempt.startBeforeRevision };
            ordinalCut = state.activeAttempt.startOrdinal - 1;
          } else {
            const active = state.activeAttempt;
            const startedAfterSeq = startedAfterSeqFromSnapshot(snapshot, active.afterEventSeq);
            if (snapshotContainsSettlement(events, startedAfterSeq, active.turn, active.step)) {
              assistantStream = { revision: state.revision };
              ordinalCut = state.ordinal;
              settledAttemptId = active.attemptId;
            } else {
              assistantStream = {
                revision: state.revision,
                activeAttempt: {
                  attemptId: active.attemptId,
                  startedAfterSeq,
                  turn: active.turn,
                  step: active.step,
                  nextIndex: active.nextIndex,
                  stream: active.stream.snapshot(),
                },
              };
              ordinalCut = state.ordinal;
            }
          }
        }
        const trackedEvents = isSubagent ? [...events] : [...events];
        const projectionValues = isSubagent
          ? { subagent: subagent.subagent, subagentTiming: subagent.subagentTiming }
          : mergeCustomProjections(address.sessionId, {
              title: foldSessionTitle(events)?.title ?? null,
              subagentCatalog: foldSubagentCatalog(events),
            });
        const header = isSubagent
          ? {
              version: SESSION_FORMAT_VERSION,
              id: address.childSessionId,
              createdAt: subagent.createdAt ?? snapshot.createdAt,
              origin: 'subagent',
              parentSession: address.parentSessionId,
              isSeeded: false,
              ...(snapshot.initialDirectory === null ? {} : { cwd: snapshot.initialDirectory }),
            }
          : {
              version: SESSION_FORMAT_VERSION,
              id: address.sessionId,
              createdAt: snapshot.createdAt,
              isSeeded: false,
              ...(snapshot.initialDirectory === null ? {} : { cwd: snapshot.initialDirectory }),
            };
        yield {
          type: 'snapshot',
          header,
          cursor,
          records: page.events.map(entryFor),
          hasMore: page.hasMore,
          projections: { asOfSeq: cursor, values: projectionValues },
          ...(wantsAssistantStream ? { assistantStream } : {}),
        };
        let next = events.length;
        const settlementsBySource = new Map();
        for await (const item of feed.items(signal)) {
          if (item.kind === 'assistant-stream') {
            if (!wantsAssistantStream || isSubagent || item.ordinal <= ordinalCut) continue;
            if (settledAttemptId !== undefined && item.frame.attemptId === settledAttemptId) continue;
            if (item.frame.type === 'start') {
              yield {
                type: 'assistant-stream',
                frame: { ...item.frame, startedAfterSeq: next - 1 },
              };
            } else if (item.frame.type === 'chunk') {
              yield { type: 'assistant-stream', frame: item.frame };
            } else if (item.frame.type === 'end') {
              if (item.frame.outcome.kind === 'abandoned') {
                yield { type: 'assistant-stream', frame: item.frame };
              } else {
                const matched = settlementsBySource.get(item.sourceEventSeq);
                settlementsBySource.delete(item.sourceEventSeq);
                if (matched === undefined || matched.eventType !== item.frame.outcome.eventType) {
                  throw new RemoteError('gateway/overflow', 'assistant stream settlement mismatch; reopen follow', {});
                }
                yield {
                  type: 'assistant-stream',
                  frame: {
                    ...item.frame,
                    outcome: { kind: 'committed', eventType: matched.eventType, seq: matched.seq },
                  },
                };
              }
            }
            continue;
          }
          if (item.eventSeq <= snapshot.through) continue;
          const event = Object.freeze({ seq: next, time: item.occurredAt, ...toSessionEvent(item.observation) });
          next++;
          trackedEvents.push(event);
          if (wantsAssistantStream && !isSubagent
            && ((event.type === 'assistant/message' && isAppendSurfaceEvent(event))
              || event.type === 'assistant/attempt')) {
            settlementsBySource.set(item.eventSeq, { seq: event.seq, eventType: event.type });
            if (settlementsBySource.size > 32) {
              const oldest = settlementsBySource.keys().next().value;
              settlementsBySource.delete(oldest);
            }
          }
          if (!isSubagent && event.type === 'subagent/catalog') {
            broadcastControl({
              type: 'projection',
              sessionId: address.sessionId,
              seq: event.seq,
              changes: [{ key: 'subagentCatalog', val: foldSubagentCatalog(trackedEvents) }],
            });
          } else if (!isSubagent && event.type === 'session/title') {
            broadcastControl({
              type: 'projection',
              sessionId: address.sessionId,
              seq: event.seq,
              changes: [{ key: 'title', val: event.data.title }],
            });
          } else if (isSubagent && (event.type === 'subagent/descriptor' || event.type === 'turn/start' || event.type === 'turn/end')) {
            const updated = foldSubagentProjections(trackedEvents);
            broadcastControl({
              type: 'projection',
              sessionId: address.childSessionId,
              seq: event.seq,
              changes: [
                { key: 'subagent', val: updated.subagent },
                { key: 'subagentTiming', val: updated.subagentTiming },
              ],
            });
          }
          yield entryFor(event);
        }
      } finally {
        feed.close();
        registered.delete(feed);
        if (registered.size === 0) feeds.delete(address.feedKey);
      }
    }

    // Host-wide control stream: opening baseline followed by live projection updates.
    async *control(signal) {
      signal.throwIfAborted();
      const controlFeed = new FollowFeed();
      controlFeeds.add(controlFeed);
      try {
        yield { type: 'baseline', value: { projections: {} } };
        for await (const frame of controlFeed.items(signal)) yield frame;
      } finally {
        controlFeed.close();
        controlFeeds.delete(controlFeed);
      }
    }
  }
  markRemoteMethod(SessionController.prototype, 'list');
  markRemoteMethod(SessionController.prototype, 'create');
  markRemoteMethod(SessionController.prototype, 'prompt');
  markRemoteMethod(SessionController.prototype, 'updateQueue');
  markRemoteMethod(SessionController.prototype, 'cancel');
  markRemoteMethod(SessionController.prototype, 'rename');
  markRemoteMethod(SessionController.prototype, 'attachment');
  markRemoteMethod(SessionController.prototype, 'control', { mode: 'stream' });
  if (readLog !== undefined) {
    markRemoteMethod(SessionController.prototype, 'page');
    markRemoteMethod(SessionController.prototype, 'projections');
    markRemoteMethod(SessionController.prototype, 'follow', { mode: 'stream' });
  }

  return {
    SessionController,
    // `host.session.changed`: reconcile only what changed; a lost notification, an overflow or a resync flag
    // re-reads the whole directory. Notifications before the first listing are ignored: the listing is authoritative.
    observeChanged(params) {
      if (params?.hostGeneration !== hostGeneration || !Number.isSafeInteger(params.factsSequence)) return;
      const changes = Array.isArray(params.changes) ? params.changes : [];
      void serially(async () => {
        if (published === undefined || params.factsSequence <= known) return;
        const ids = [...new Set(changes.map(change => change?.sessionId))];
        if (params.resync === true || params.factsSequence !== known + 1 || changes.length > MAX_CHANGES
          || ids.some(id => typeof id !== 'string' || !id)) {
          await resync();
          return;
        }
        let next = published;
        for (const id of ids) next = await reconcileOn(next, id);
        publish(next, params.factsSequence);
      }).catch(error => {
        onDiagnostic(`Session directory change could not be applied: ${error?.message ?? error}`);
        scheduleRetry();
      });
    },
    // `host.session.committed`: hand each committed observation to the sessions (or read-only subagents)
    // being followed, and settle any matching live assistant-stream attempt right after the durable observation.
    observeCommitted(params) {
      if (closed || typeof params?.sessionId !== 'string' || !Number.isSafeInteger(params.eventSeq)) return;
      const sessionId = params.sessionId;
      const subagentTaskId = typeof params.subagentTaskId === 'string' && params.subagentTaskId
        ? params.subagentTaskId
        : undefined;
      const feedKey = subagentTaskId ? `${sessionId}\0subagent\0${subagentTaskId}` : sessionId;
      const registered = feeds.get(feedKey);
      if (registered !== undefined) {
        const item = { kind: 'committed', eventSeq: params.eventSeq, occurredAt: params.occurredAt, observation: params.observation };
        for (const feed of registered) feed.push(item);
      }
      if (subagentTaskId !== undefined) return;
      const state = liveStreams.get(sessionId);
      const active = state?.activeAttempt;
      if (active === undefined) return;
      const obs = params.observation;
      if ((obs?.kind === 'assistantMessage' || obs?.kind === 'assistantAttempt')
        && obs.turn === active.turn && obs.step === active.step && params.eventSeq > active.afterEventSeq) {
        const eventType = obs.kind === 'assistantMessage' ? 'assistant/message' : 'assistant/attempt';
        const { attemptId, nextIndex: index } = active;
        state.activeAttempt = undefined;
        state.revision += 1;
        fanOutLive(sessionId, state, {
          type: 'end',
          attemptId,
          revision: state.revision,
          index,
          outcome: { kind: 'committed', eventType },
        }, { sourceEventSeq: params.eventSeq });
      } else if (obs?.kind === 'turnEnded' && obs.turn === active.turn) {
        const { attemptId, nextIndex: index } = active;
        state.activeAttempt = undefined;
        state.revision += 1;
        fanOutLive(sessionId, state, {
          type: 'end',
          attemptId,
          revision: state.revision,
          index,
          outcome: { kind: 'abandoned' },
        });
      }
    },
    // `host.session.assistantStream`: maintain contiguous per-session revision & reconnect accumulator,
    // and fan out live frames to `follow({ assistantStream: true })` streams.
    observeAssistantStream(params) {
      if (closed || params?.hostGeneration !== hostGeneration || typeof params.sessionId !== 'string' || !params.sessionId) return;
      const frame = params.frame;
      if (typeof frame !== 'object' || frame === null) return;
      const sessionId = params.sessionId;
      const state = liveStreamOf(sessionId);
      switch (frame.kind) {
        case 'overflow': {
          state.activeAttempt = undefined;
          overflowLiveFeeds(sessionId, 'assistant stream overflowed; reopen follow');
          return;
        }
        case 'start': {
          if (typeof frame.attemptId !== 'string' || !frame.attemptId
            || !Number.isSafeInteger(frame.turn) || frame.turn <= 0
            || !Number.isSafeInteger(frame.step) || frame.step <= 0
            || !Number.isSafeInteger(frame.afterEventSeq) || frame.afterEventSeq < 0) return;
          if (state.activeAttempt !== undefined) {
            const previous = state.activeAttempt;
            state.activeAttempt = undefined;
            state.revision += 1;
            fanOutLive(sessionId, state, {
              type: 'end',
              attemptId: previous.attemptId,
              revision: state.revision,
              index: previous.nextIndex,
              outcome: { kind: 'abandoned' },
            });
          }
          const startBeforeRevision = state.revision;
          state.revision += 1;
          const startOrdinal = fanOutLive(sessionId, state, {
            type: 'start',
            attemptId: frame.attemptId,
            revision: state.revision,
            turn: frame.turn,
            step: frame.step,
          }, { afterEventSeq: frame.afterEventSeq });
          state.activeAttempt = {
            attemptId: frame.attemptId,
            turn: frame.turn,
            step: frame.step,
            afterEventSeq: frame.afterEventSeq,
            startOrdinal,
            startBeforeRevision,
            nextIndex: 0,
            stream: new AssistantStreamAccumulator(),
          };
          return;
        }
        case 'chunk': {
          if (typeof frame.attemptId !== 'string' || !frame.attemptId
            || !Number.isSafeInteger(frame.index) || frame.index < 0
            || !Number.isSafeInteger(frame.time) || frame.time < 0
            || typeof frame.chunk !== 'object' || frame.chunk === null) return;
          const active = state.activeAttempt;
          if (active === undefined || active.attemptId !== frame.attemptId || frame.index !== active.nextIndex) {
            state.activeAttempt = undefined;
            overflowLiveFeeds(sessionId, 'assistant stream frame gap; reopen follow');
            return;
          }
          state.revision += 1;
          active.stream.push({ time: frame.time, chunk: frame.chunk });
          active.nextIndex += 1;
          fanOutLive(sessionId, state, {
            type: 'chunk',
            attemptId: frame.attemptId,
            revision: state.revision,
            index: frame.index,
            time: frame.time,
            chunk: frame.chunk,
          });
          return;
        }
        case 'abandon': {
          if (typeof frame.attemptId !== 'string' || !frame.attemptId) return;
          const active = state.activeAttempt;
          if (active === undefined || active.attemptId !== frame.attemptId) return;
          const index = active.nextIndex;
          state.activeAttempt = undefined;
          state.revision += 1;
          fanOutLive(sessionId, state, {
            type: 'end',
            attemptId: frame.attemptId,
            revision: state.revision,
            index,
            outcome: { kind: 'abandoned' },
          });
          return;
        }
      }
    },
    close() {
      closed = true;
      customProjectionUnsub?.();
      clearTimeout(retry);
      liveStreams.clear();
      for (const controlFeed of controlFeeds) controlFeed.close();
      controlFeeds.clear();
      for (const registered of feeds.values()) for (const feed of registered) feed.close();
    },
  };
}
