import { AsyncLocalStorage } from 'node:async_hooks';
import { isAbsolute } from 'node:path';
import { Service } from '@deepseek-ai/cordis';
import { Session } from '@deepseek-ai/dsh-session';
import { SESSION_FORMAT_VERSION } from '@deepseek-ai/dsh-session/types';
import SessionPersistence, {
  SessionHandleClosedError,
  SessionPersistenceNotFoundError,
  SessionPersistenceRevision,
  SessionReadOnlyError,
} from '@deepseek-ai/dsh-session-persistence';
import { SessionQueryEngine, SessionQueryError } from '@deepseek-ai/dsh-session-query';
import { toSessionEvent } from './session-observer.mjs';

function buildHeader(id, createdAt, initialDirectory) {
  return Object.freeze({
    version: SESSION_FORMAT_VERSION,
    id,
    createdAt,
    isSeeded: false,
    ...(typeof initialDirectory === 'string' && initialDirectory.length > 0 ? { cwd: initialDirectory } : {}),
  });
}

function normalizeFact(fact) {
  if (typeof fact?.sessionId !== 'string' || !fact.sessionId
    || !Number.isSafeInteger(fact.createdAt) || fact.createdAt < 0) {
    return null;
  }
  const cwd = typeof fact.cwd === 'string' && isAbsolute(fact.cwd) ? fact.cwd : null;
  return {
    sessionId: fact.sessionId,
    createdAt: fact.createdAt,
    updatedAt: Number.isSafeInteger(fact.updatedAt) ? fact.updatedAt : fact.createdAt,
    messageCount: Number.isSafeInteger(fact.messageCount) ? fact.messageCount : 0,
    status: typeof fact.status === 'string' ? fact.status : 'Active',
    cwd,
  };
}

// Unified read-only backend session reader, bounded history cache, `ctx.sessions`,
// read-only `ctx.sessionPersistence`, `ctx.workingDirectory`, and official `ctx.sessionQuery`.
export function createSessionReader(requestOwner) {
  const scope = new AsyncLocalStorage();
  const cache = new Map();
  const directory = new Map();
  const cacheSize = 16;

  async function read(identity, signal = new AbortController().signal) {
    const { id, createdAt, initialDirectory, subagentTaskId } = identity;
    const cacheKey = subagentTaskId ? `${id}\0subagent\0${subagentTaskId}` : id;
    const cached = cache.get(cacheKey);
    const previous = cached?.createdAt === createdAt && cached.initialDirectory === initialDirectory
      ? cached
      : undefined;
    const events = [];
    const sourceSequences = [];
    let through;
    let after = previous?.through ?? 0;
    do {
      signal.throwIfAborted();
      const page = await requestOwner('host.session.history', {
        sessionId: id,
        after,
        through,
        limit: 256,
        ...(subagentTaskId ? { subagentTaskId } : {}),
      });
      signal.throwIfAborted();
      if (page.session?.id !== id || page.session.createdAt !== createdAt
        || (page.session.cwd ?? null) !== initialDirectory) {
        throw new Error('Plugin route session is no longer current');
      }
      if (!Number.isSafeInteger(page.through) || page.through < after
        || (through !== undefined && through !== page.through)
        || !Array.isArray(page.events)) {
        throw new Error('Invalid committed session history page');
      }
      through = page.through;
      let lastSource = after;
      // One committed daemon event can yield several observations (they share its sourceSequence);
      // the first observation of a page must come from an event after the cursor.
      for (const [index, item] of page.events.entries()) {
        const seq = (previous?.events.length ?? 0) + events.length;
        if (item.seq !== seq || !Number.isSafeInteger(item.sourceSequence)
          || item.sourceSequence < lastSource || (index === 0 && item.sourceSequence <= after)
          || item.sourceSequence > through
          || !Number.isSafeInteger(item.occurredAt) || item.occurredAt < 0) {
          throw new Error('Invalid committed session event order');
        }
        lastSource = item.sourceSequence;
        sourceSequences.push(item.sourceSequence);
        events.push(Object.freeze({ seq, time: item.occurredAt, ...toSessionEvent(item.observation) }));
      }
      if (page.nextAfter === null) break;
      if (page.nextAfter !== lastSource || lastSource <= after || lastSource >= through) {
        throw new Error('Invalid committed session history cursor');
      }
      after = lastSource;
    } while (after < through);

    const snapshot = {
      createdAt,
      initialDirectory,
      through,
      events: previous && events.length === 0 ? previous.events : Object.freeze([...(previous?.events ?? []), ...events]),
      sourceSequences: previous && sourceSequences.length === 0
        ? previous.sourceSequences
        : Object.freeze([...(previous?.sourceSequences ?? []), ...sourceSequences]),
    };
    const current = cache.get(cacheKey);
    if (!current || current.createdAt !== createdAt || current.through <= through) {
      cache.delete(cacheKey);
      cache.set(cacheKey, snapshot);
      if (cache.size > cacheSize) cache.delete(cache.keys().next().value);
    }
    return snapshot;
  }

  async function syncDirectory(signal) {
    signal?.throwIfAborted();
    const reply = await requestOwner('host.session.list', { includeArchived: true });
    signal?.throwIfAborted();
    if (!Array.isArray(reply?.sessions)) return;
    const seen = new Set();
    for (const raw of reply.sessions) {
      const fact = normalizeFact(raw);
      if (!fact) continue;
      seen.add(fact.sessionId);
      directory.set(fact.sessionId, fact);
    }
    for (const id of [...directory.keys()]) {
      if (!seen.has(id)) {
        directory.delete(id);
        cache.delete(id);
      }
    }
  }

  async function fetchFact(sessionId, signal) {
    signal?.throwIfAborted();
    const reply = await requestOwner('host.session.get', { sessionId });
    signal?.throwIfAborted();
    const fact = normalizeFact(reply?.session);
    if (!fact) {
      directory.delete(sessionId);
      cache.delete(sessionId);
      return null;
    }
    directory.set(sessionId, fact);
    return fact;
  }

  async function ensureLoaded(sessionId, signal = new AbortController().signal) {
    const fact = await fetchFact(sessionId, signal);
    if (!fact) return null;
    const snapshot = await read({
      id: fact.sessionId,
      createdAt: fact.createdAt,
      initialDirectory: fact.cwd,
    }, signal);
    return { fact, snapshot };
  }

  function observeChanged(params) {
    const changes = Array.isArray(params?.changes) ? params.changes : [];
    if (params?.resync === true) {
      void syncDirectory().catch(() => {});
      return;
    }
    for (const change of changes) {
      const id = change?.sessionId;
      if (typeof id !== 'string' || !id) continue;
      if (change.kind === 'removed') {
        directory.delete(id);
        cache.delete(id);
      } else {
        void fetchFact(id).catch(() => {});
      }
    }
  }

  function makeBackendHandle(fact) {
    const header = buildHeader(fact.sessionId, fact.createdAt, fact.cwd);
    const requireLoaded = () => {
      const loaded = cache.get(fact.sessionId);
      if (!loaded || loaded.createdAt !== fact.createdAt || loaded.initialDirectory !== fact.cwd) {
        throw new Error(`Session "${fact.sessionId}" history is not loaded; read it through ctx.sessionQuery first`);
      }
      return loaded;
    };
    return Object.freeze({
      id: fact.sessionId,
      header,
      inheritedEventCount: 0,
      get revision() { return requireLoaded().through; },
      get seq() { return requireLoaded().events.length; },
      eventAt(seq) { return requireLoaded().events[seq]; },
      snapshotEvents(from = 0, to) {
        const events = requireLoaded().events;
        const end = to === undefined ? events.length : to;
        return from === 0 && end === events.length ? events : Object.freeze(events.slice(from, end));
      },
    });
  }

  function requireScope() {
    const current = scope.getStore();
    if (!current) throw new Error('Session reads require an admitted session call');
    current.signal.throwIfAborted();
    return current;
  }

  function persistenceSnapshotFor(fact) {
    const cached = cache.get(fact.sessionId);
    const rev = `${fact.updatedAt}:${fact.messageCount}:${cached?.through ?? ''}`;
    return Object.freeze({
      header: buildHeader(fact.sessionId, fact.createdAt, fact.cwd),
      formatStatus: 'current',
      revision: SessionPersistenceRevision(rev),
      ...(cached ? { eventCount: cached.events.length } : {}),
    });
  }

  const messageProjections = [];

  class Sessions extends Service {
    constructor(ctx) {
      super(ctx, 'sessions');
    }

    get messageProjections() {
      return messageProjections;
    }

    registerMessageProjection(projection) {
      if (messageProjections.some(item => item.type === projection.type)) {
        throw new Error(`session message projection "${projection.type}" is already registered`);
      }
      return this.ctx.effect(() => {
        messageProjections.push(projection);
        return () => {
          const idx = messageProjections.indexOf(projection);
          if (idx >= 0) messageProjections.splice(idx, 1);
        };
      }, 'sessions.registerMessageProjection()');
    }

    get(id) {
      const current = scope.getStore();
      if (current) {
        current.signal.throwIfAborted();
        if (id !== current.session.id) throw new Error('Session is outside the admitted call');
        return current.session;
      }
      const fact = directory.get(id);
      return fact ? makeBackendHandle(fact) : undefined;
    }

    list() {
      const current = scope.getStore();
      if (current) {
        current.signal.throwIfAborted();
        return [current.session];
      }
      return [...directory.values()].map(makeBackendHandle);
    }

    prepare(id, options = {}) {
      return Session.fromRestore(
        id,
        options.seed ?? [],
        options.meta,
        options.inheritedEventCount ?? 0,
        options.eventState ?? 'detached',
        messageProjections,
      );
    }
  }

  class WorkingDirectory extends Service {
    constructor(ctx) { super(ctx, 'workingDirectory'); }
    get(session) {
      const current = requireScope();
      if (session !== current.session) throw new Error('Session is outside the admitted call');
      return current.executionDirectory;
    }
  }

  let bootingServices = false;

  class MagiSessionPersistence extends SessionPersistence {
    async create() {
      throw new SessionReadOnlyError('Magi daemon owns session creation; backend persistence is read-only');
    }

    async flush() {}

    async stat(id, options) {
      const fact = await fetchFact(id, options?.signal);
      return fact ? persistenceSnapshotFor(fact) : undefined;
    }

    async list(options) {
      try {
        await syncDirectory(options?.signal);
      } catch (err) {
        if (bootingServices && directory.size === 0) return [];
        throw err;
      }
      return [...directory.values()].map(persistenceSnapshotFor);
    }

    async open(id, access, options) {
      if (access !== 'read') {
        throw new SessionReadOnlyError('Magi backend session persistence is read-only');
      }
      const loaded = await ensureLoaded(id, options?.signal);
      if (!loaded) {
        throw new SessionPersistenceNotFoundError(id);
      }
      const header = buildHeader(loaded.fact.sessionId, loaded.fact.createdAt, loaded.fact.cwd);
      const events = loaded.snapshot.events;
      let handleClosed = false;
      const assertOpen = () => {
        if (handleClosed) throw new SessionHandleClosedError(id);
      };
      return {
        id,
        header,
        inheritedEventCount: 0,
        access: 'read',
        async read(offset = 0, length, readOptions) {
          assertOpen();
          readOptions?.signal?.throwIfAborted();
          const start = Math.max(0, offset);
          const slice = length === undefined ? events.slice(start) : events.slice(start, start + length);
          return { eventState: 'shared-frozen', events: slice };
        },
        async append() {
          assertOpen();
          throw new SessionReadOnlyError('Magi backend session persistence is read-only');
        },
        async flush() {
          assertOpen();
          throw new SessionReadOnlyError('Magi backend session persistence is read-only');
        },
        async close() {
          handleClosed = true;
        },
        async [Symbol.asyncDispose]() {
          handleClosed = true;
        },
      };
    }
  }

  class MagiSessionQuery extends SessionQueryEngine {
    async listSessions(signal) {
      await syncDirectory(signal);
      return await super.listSessions(signal);
    }

    async observeSession(sessionId, options = {}) {
      await ensureLoaded(sessionId, options.signal);
      return await super.observeSession(sessionId, options);
    }

    async inspectSession(sessionId, reader, signal) {
      await ensureLoaded(sessionId, signal);
      return await super.inspectSession(sessionId, reader, signal);
    }

    async readSession(sessionId) {
      await ensureLoaded(sessionId);
      return await super.readSession(sessionId);
    }

    async readTitleSnapshots(sessionIds, signal) {
      const unique = [...new Set(sessionIds)];
      for (const id of unique) {
        try { await ensureLoaded(id, signal); } catch { /* per-session failure handled by super */ }
      }
      return await super.readTitleSnapshots(sessionIds, signal);
    }

    async listEvents(sessionId) {
      await ensureLoaded(sessionId);
      return await super.listEvents(sessionId);
    }

    async filterEvents(sessionId, filters) {
      await ensureLoaded(sessionId);
      return await super.filterEvents(sessionId, filters);
    }

    async readSurface(sessionId) {
      await ensureLoaded(sessionId);
      return await super.readSurface(sessionId);
    }

    async traceEvent(request, signal) {
      await ensureLoaded(request.sessionId, signal);
      return await super.traceEvent(request, signal);
    }

    async readEvent(request, signal) {
      await ensureLoaded(request.sessionId, signal);
      return await super.readEvent(request, signal);
    }

    async searchSessions() {
      throw new SessionQueryError(
        'Full-text session search is not enabled in Magi',
        'SESSION_QUERY_SEARCH_DISABLED',
      );
    }

    async searchEvents() {
      throw new SessionQueryError(
        'Full-text session search is not enabled in Magi',
        'SESSION_QUERY_SEARCH_DISABLED',
      );
    }
  }

  return {
    Sessions,
    WorkingDirectory,
    setBootingServices(flag) {
      bootingServices = Boolean(flag);
    },
    SessionPersistence: MagiSessionPersistence,
    SessionQuery: MagiSessionQuery,
    // The committed log of one session as a dense event log, read from the daemon (cached by waterline).
    readLog: read,
    syncDirectory,
    observeChanged,
    async run(session, signal, callback) {
      if (!session) return await callback();
      const { id, createdAt, initialDirectory, executionDirectory } = session;
      if (typeof id !== 'string' || !id || !Number.isSafeInteger(createdAt) || createdAt < 0
        || (initialDirectory !== null && (typeof initialDirectory !== 'string' || !isAbsolute(initialDirectory)))
        || typeof executionDirectory !== 'string' || !isAbsolute(executionDirectory)) {
        throw new Error('Invalid admitted plugin route session');
      }
      const { through, events } = await read(session, signal);
      const lifetime = new AbortController();
      const access = AbortSignal.any([signal, lifetime.signal]);
      const header = buildHeader(id, createdAt, initialDirectory);
      const handle = Object.freeze({
        get id() { access.throwIfAborted(); return id; },
        get header() { access.throwIfAborted(); return header; },
        get revision() { access.throwIfAborted(); return through; },
        get inheritedEventCount() { access.throwIfAborted(); return 0; },
        get seq() { access.throwIfAborted(); return events.length; },
        eventAt(seq) { access.throwIfAborted(); return events[seq]; },
        snapshotEvents(from = 0, to = events.length) {
          access.throwIfAborted();
          return from === 0 && to === events.length ? events : Object.freeze(events.slice(from, to));
        },
      });
      try {
        return await scope.run({ signal: access, session: handle, executionDirectory }, callback);
      } finally {
        lifetime.abort(new Error('Plugin session call has ended'));
      }
    },
  };
}
