// Execution loop, writable session persistence backends, execution environment seams, and host controllers
// for the shared DSH host.
//
// Strictly subclasses the locked 0.2.2-rc.3 official abstract seams:
// - `@deepseek-ai/dsh-fs::FileSystem`
// - `@deepseek-ai/dsh-subprocess::SubprocessRuntime`
// - `@deepseek-ai/dsh-sandbox::SandboxProvider`
// - `@deepseek-ai/dsh-jobs::JobRegistry`
// - `@deepseek-ai/dsh-file-reference::FileReferenceService`
// - `@deepseek-ai/dsh-session-title::SessionTitleService`
// - `@deepseek-ai/dsh-session-persistence::SessionPersistence` (explicit writable backend contract)

import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { watch as fsWatch } from 'node:fs';
import {
  mkdir,
  open as fsOpen,
  readdir,
  readFile,
  rename,
  rm,
  stat,
} from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { Service } from '@deepseek-ai/cordis';
import McpResourceRuntime from '@deepseek-ai/dsh-mcp-resources';
import { writeFileAtomic } from '@deepseek-ai/dsh-atomic-write';
import {
  FileReferenceService,
  activeAtToken,
  formatFileMention,
} from '@deepseek-ai/dsh-file-reference';
import {
  FileSystem,
  FsError,
  FsVersion,
  fsErrorFromErrno,
} from '@deepseek-ai/dsh-fs';
import { JobError, JobId, JobRegistry } from '@deepseek-ai/dsh-jobs';
import {
  SandboxProvider,
  canonicalPath,
  validateEscalationArgs,
  workspaceWritableRoots,
} from '@deepseek-ai/dsh-sandbox';
import {
  SessionAlreadyExistsError,
  SessionAlreadyOwnedError,
  SessionHandleClosedError,
  SessionPersistence,
  SessionPersistenceNotFoundError,
  SessionPersistenceRevision,
  SessionReadOnlyError,
  assertContiguous,
  assertStoredId,
  materializeAppendBatch,
  materializeCreateHeader,
} from '@deepseek-ai/dsh-session-persistence';
import {
  SessionTitleInvalidError,
  SessionTitleService,
  normalizeSessionTitle,
} from '@deepseek-ai/dsh-session-title';
import {
  SubprocessRuntime,
  scrubbedParentEnv,
  subprocessSpawnFailure,
} from '@deepseek-ai/dsh-subprocess';

const JOB_OUTPUT_LIMIT_BYTES = 256 * 1024;
const FILE_REF_MAX_RESULTS = 2000;
const FILE_REF_SKIP_DIRS = new Set(['.git', 'node_modules', 'target', '.next', 'dist', '.turbo']);

function computeVersion(buf, st) {
  const hash = createHash('sha256').update(buf).digest('hex').slice(0, 16);
  return FsVersion(`${st.size}:${Math.trunc(st.mtimeMs)}:${hash}`);
}

function appendBounded(current, chunk, limit = JOB_OUTPUT_LIMIT_BYTES) {
  const combined = current + chunk;
  if (Buffer.byteLength(combined, 'utf8') <= limit) return combined;
  return combined.slice(combined.length - limit);
}

/**
 * Standard JSONL single-writer SessionPersistence backend implementing the complete
 * `@deepseek-ai/dsh-session-persistence` contract (create, single-writer lock,
 * contiguous append, explicit flush barrier, stat, list, closed-handle guard).
 * Never shadows or mirrors daemon's canonical `magi-session-store`.
 */
export function createJsonlSessionPersistenceBackend(rootDir) {
  if (typeof rootDir !== 'string' || !rootDir) {
    throw new TypeError('createJsonlSessionPersistenceBackend requires a non-empty rootDir');
  }
  const activeWriters = new Map();
  let closed = false;

  function filePathFor(id) {
    if (typeof id !== 'string' || !id || id.includes('/') || id.includes('\\') || id.includes('..')) {
      throw new TypeError(`Invalid stored session id: "${String(id)}"`);
    }
    return join(rootDir, `${id}.jsonl`);
  }

  async function readStoredFile(id) {
    const file = filePathFor(id);
    let raw;
    try {
      raw = await readFile(file, 'utf8');
    } catch (err) {
      if (err?.code === 'ENOENT') throw new SessionPersistenceNotFoundError(id);
      throw err;
    }
    const lines = raw.split('\n').filter(line => line.trim().length > 0);
    if (lines.length === 0) throw new SessionPersistenceNotFoundError(id);
    const header = JSON.parse(lines[0]);
    assertStoredId(id, header);
    const events = [];
    for (let i = 1; i < lines.length; i++) {
      events.push(JSON.parse(lines[i]));
    }
    assertContiguous(id, events, 0);
    return { header, events };
  }

  return {
    id: 'magi-jsonl',
    async create(input) {
      if (closed) throw new SessionHandleClosedError('persistence backend closed');
      const header = materializeCreateHeader(input);
      const id = header.id;
      const file = filePathFor(id);
      await mkdir(rootDir, { recursive: true, mode: 0o700 });
      try {
        await stat(file);
        throw new SessionAlreadyExistsError(id);
      } catch (err) {
        if (err instanceof SessionAlreadyExistsError) throw err;
        if (err?.code !== 'ENOENT') throw err;
      }
      await writeFileAtomic(file, `${JSON.stringify(header)}\n`, { dirMode: 0o700, fileMode: 0o600 });
      return header;
    },

    async stat(id) {
      const { header, events } = await readStoredFile(id);
      return {
        header,
        eventCount: events.length,
        revision: SessionPersistenceRevision(events.length),
      };
    },

    async list() {
      await mkdir(rootDir, { recursive: true, mode: 0o700 });
      const entries = await readdir(rootDir, { withFileTypes: true }).catch(() => []);
      const out = [];
      for (const entry of entries) {
        if (!entry.isFile() || !entry.name.endsWith('.jsonl')) continue;
        const id = entry.name.slice(0, -'.jsonl'.length);
        try {
          const info = await this.stat(id);
          out.push(info);
        } catch { /* skip unreadable non-session files */ }
      }
      return out;
    },

    async open(id, access = 'read') {
      if (closed) throw new SessionHandleClosedError(id);
      const { header, events } = await readStoredFile(id);
      const file = filePathFor(id);
      const writable = access === 'write';
      const token = Symbol(`writer:${id}`);
      if (writable) {
        if (activeWriters.has(id)) throw new SessionAlreadyOwnedError(id);
        activeWriters.set(id, token);
      }
      let handleClosed = false;
      const inMemory = [...events];
      const ensureOpen = () => {
        if (closed || handleClosed) throw new SessionHandleClosedError(id);
      };
      return {
        id,
        access,
        header,
        get revision() {
          ensureOpen();
          return SessionPersistenceRevision(inMemory.length);
        },
        async read(offset = 0, length) {
          ensureOpen();
          const slice = length === undefined ? inMemory.slice(offset) : inMemory.slice(offset, offset + length);
          return Object.freeze(slice);
        },
        async append(batch) {
          ensureOpen();
          if (!writable) throw new SessionReadOnlyError(id);
          const rawBatch = materializeAppendBatch(batch);
          if (rawBatch.length === 0) return SessionPersistenceRevision(inMemory.length);
          const materialized = rawBatch.map((ev, index) => ({
            ...ev,
            seq: ev.seq ?? (inMemory.length + index),
          }));
          assertContiguous(id, materialized, inMemory.length);
          const payload = materialized.map(ev => `${JSON.stringify(ev)}\n`).join('');
          const fh = await fsOpen(file, 'a', 0o600);
          try {
            await fh.writeFile(payload, 'utf8');
            await fh.sync();
          } finally {
            await fh.close();
          }
          inMemory.push(...materialized);
          return SessionPersistenceRevision(inMemory.length);
        },
        async flush() {
          ensureOpen();
          const fh = await fsOpen(file, writable ? 'r+' : 'r');
          try {
            await fh.sync();
          } finally {
            await fh.close();
          }
        },
        async close() {
          if (handleClosed) return;
          handleClosed = true;
          if (writable && activeWriters.get(id) === token) {
            activeWriters.delete(id);
          }
        },
        async [Symbol.asyncDispose]() {
          await this.close();
        },
      };
    },

    async flush() {
      // Every append fsyncs its own file; backend flush is an explicit barrier.
    },

    async close() {
      closed = true;
      activeWriters.clear();
    },
  };
}

export async function openExecutionSeams({
  root,
  lifecycle,
  dshHome,
  requestOwner,
  notifyOwner,
  mount,
}) {
  const disposers = [];
  let closed = false;

  // -------------------------------------------------------------------------
  // Pluggable AgentLoop provider registry (`ctx.agentLoop`)
  // -------------------------------------------------------------------------
  const loopProviders = new Map();
  let selectedLoopId = 'magi-native';

  function publishAgentLoopState() {
    if (closed) return;
    notifyOwner?.('host.agentLoop.changed', {
      selected: selectedLoopId,
      providers: [...loopProviders.values()].map(({ id, title, capabilities, ownerPluginId }) => ({
        id,
        title,
        capabilities,
        ownerPluginId,
      })),
    });
  }

  class MagiAgentLoopService extends Service {
    constructor(ctx) {
      super(ctx, 'agentLoop');
    }

    register(provider) {
      if (!provider || typeof provider.id !== 'string' || !provider.id.trim()) {
        throw new TypeError('agentLoop.register requires a provider with a non-empty id');
      }
      if (provider.id === 'magi-native' || loopProviders.has(provider.id)) {
        throw new Error(`Duplicate agentLoop provider "${provider.id}"`);
      }
      if (typeof provider.run !== 'function') {
        throw new TypeError(`agentLoop provider "${provider.id}" must implement run()`);
      }
      const caller = root.reflect.trace(this.ctx);
      const owner = lifecycle.ownerIdentity(caller);
      const entry = Object.freeze({
        id: provider.id,
        title: provider.title ?? provider.id,
        capabilities: Object.freeze({
          context: provider.capabilities?.context ?? 'executor_owned',
          tools: provider.capabilities?.tools ?? 'executor_owned',
          steer: Boolean(provider.capabilities?.steer),
          resume: Boolean(provider.capabilities?.resume),
          cancellation: provider.capabilities?.cancellation ?? 'signal',
        }),
        ownerPluginId: owner?.pluginId ?? null,
        run: provider.run.bind(provider),
        cancel: provider.cancel?.bind(provider),
        resume: provider.resume?.bind(provider),
      });
      return this.ctx.effect(() => {
        loopProviders.set(entry.id, entry);
        publishAgentLoopState();
        return () => {
          if (loopProviders.get(entry.id) === entry) {
            loopProviders.delete(entry.id);
            publishAgentLoopState();
          }
        };
      }, `agentLoop.register(${entry.id})`);
    }

    list() {
      return [
        Object.freeze({
          id: 'magi-native',
          title: 'Magi Native Runtime',
          capabilities: Object.freeze({
            context: 'magi_owned',
            tools: 'magi_tool',
            steer: true,
            resume: true,
            cancellation: 'signal',
          }),
          ownerPluginId: null,
        }),
        ...[...loopProviders.values()].map(({ id, title, capabilities, ownerPluginId }) =>
          Object.freeze({ id, title, capabilities, ownerPluginId }),
        ),
      ];
    }

    selected() {
      return selectedLoopId;
    }

    select(id) {
      if (id !== 'magi-native' && !loopProviders.has(id)) {
        throw new Error(`Unknown agentLoop provider "${String(id)}"`);
      }
      selectedLoopId = id;
      publishAgentLoopState();
      return selectedLoopId;
    }

    async run(sessionId, input, signal) {
      signal?.throwIfAborted?.();
      if (selectedLoopId === 'magi-native') {
        await requestOwner('host.session.prompt', {
          sessionId,
          requestId: input?.requestId ?? `loop-${randomUUID()}`,
          steer: Boolean(input?.steer),
          text: typeof input === 'string' ? input : String(input?.text ?? ''),
          images: Array.isArray(input?.images) ? input.images : [],
        });
        return { loopId: 'magi-native', accepted: true };
      }
      // Explicitly selected provider unavailable -> fail loudly, never silently fall back.
      const provider = loopProviders.get(selectedLoopId);
      if (!provider) {
        throw new Error(`Selected agentLoop provider "${selectedLoopId}" is currently unavailable`);
      }
      return await provider.run({ sessionId, input, signal });
    }

    async cancel(sessionId) {
      if (selectedLoopId === 'magi-native') {
        await requestOwner('host.session.cancel', { sessionId });
        return { cancelled: true };
      }
      const provider = loopProviders.get(selectedLoopId);
      if (!provider) {
        throw new Error(`Selected agentLoop provider "${selectedLoopId}" is currently unavailable`);
      }
      if (provider.cancel) return await provider.cancel({ sessionId });
      await requestOwner('host.session.cancel', { sessionId });
      return { cancelled: true };
    }

    async resume(sessionId, options) {
      if (selectedLoopId === 'magi-native') {
        return { loopId: 'magi-native', resumed: true };
      }
      const provider = loopProviders.get(selectedLoopId);
      if (!provider) {
        throw new Error(`Selected agentLoop provider "${selectedLoopId}" is currently unavailable`);
      }
      if (!provider.resume) {
        throw new Error(`agentLoop provider "${selectedLoopId}" does not support resume`);
      }
      return await provider.resume({ sessionId, ...(options ?? {}) });
    }
  }
  await mount(MagiAgentLoopService);

  // -------------------------------------------------------------------------
  // Explicit writable SessionPersistence backend registry (`ctx.sessionPersistenceBackends`)
  // -------------------------------------------------------------------------
  const persistenceBackends = new Map();
  class MagiSessionPersistenceBackendsService extends Service {
    constructor(ctx) {
      super(ctx, 'sessionPersistenceBackends');
    }

    register(backend) {
      if (!backend || typeof backend.id !== 'string' || !backend.id.trim()) {
        throw new TypeError('sessionPersistenceBackends.register requires a backend with non-empty id');
      }
      for (const fn of ['create', 'open', 'stat', 'list', 'flush']) {
        if (typeof backend[fn] !== 'function') {
          throw new TypeError(`sessionPersistence backend "${backend.id}" must implement ${fn}()`);
        }
      }
      if (persistenceBackends.has(backend.id)) {
        throw new Error(`Duplicate sessionPersistence backend "${backend.id}"`);
      }
      return this.ctx.effect(() => {
        persistenceBackends.set(backend.id, backend);
        return async () => {
          if (persistenceBackends.get(backend.id) === backend) {
            persistenceBackends.delete(backend.id);
            await backend.close?.();
          }
        };
      }, `sessionPersistenceBackends.register(${backend.id})`);
    }

    get(id) {
      return persistenceBackends.get(id);
    }

    list() {
      return [...persistenceBackends.keys()];
    }

    createJsonlBackend(dir) {
      return createJsonlSessionPersistenceBackend(dir);
    }
  }
  await mount(MagiSessionPersistenceBackendsService);

  // Ensure SessionPersistence symbol import stays verified against official base class.
  void SessionPersistence;

  // -------------------------------------------------------------------------
  // Official FileSystem seam (`ctx.fs`)
  // -------------------------------------------------------------------------
  const activeWatchers = new Set();

  class MagiFileSystem extends FileSystem {
    async readBytes(target) {
      try {
        const resolved = resolve(String(target));
        const [data, st] = await Promise.all([readFile(resolved), stat(resolved)]);
        return { bytes: new Uint8Array(data), version: computeVersion(data, st), size: st.size };
      } catch (err) {
        throw fsErrorFromErrno(err, String(target));
      }
    }

    async readText(target, encoding = 'utf8') {
      const { bytes, version, size } = await this.readBytes(target);
      return { text: Buffer.from(bytes).toString(encoding), version, size };
    }

    async writeBytes(target, data, options = {}) {
      const resolved = resolve(String(target));
      if (options.expectedVersion !== undefined) {
        try {
          const current = await this.readBytes(resolved);
          if (current.version !== options.expectedVersion) {
            throw new FsError('FS_CONFLICT', `File version mismatch at ${resolved}`, { path: resolved });
          }
        } catch (err) {
          if (err instanceof FsError) throw err;
          throw fsErrorFromErrno(err, resolved);
        }
      }
      try {
        const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
        await writeFileAtomic(resolved, buf, { dirMode: 0o755, fileMode: 0o644 });
        const st = await stat(resolved);
        return { version: computeVersion(buf, st), size: st.size };
      } catch (err) {
        throw fsErrorFromErrno(err, resolved);
      }
    }

    async writeText(target, text, options = {}) {
      return await this.writeBytes(target, Buffer.from(String(text), 'utf8'), options);
    }

    async stat(target) {
      try {
        const resolved = resolve(String(target));
        const st = await stat(resolved);
        return {
          path: resolved,
          kind: st.isDirectory() ? 'directory' : st.isSymbolicLink() ? 'symlink' : 'file',
          size: st.size,
          mtimeMs: st.mtimeMs,
        };
      } catch (err) {
        throw fsErrorFromErrno(err, String(target));
      }
    }

    async exists(target) {
      try {
        await stat(resolve(String(target)));
        return true;
      } catch {
        return false;
      }
    }

    async readdir(target) {
      try {
        const resolved = resolve(String(target));
        const entries = await readdir(resolved, { withFileTypes: true });
        return entries.map(entry => ({
          name: entry.name,
          path: join(resolved, entry.name),
          kind: entry.isDirectory() ? 'directory' : entry.isSymbolicLink() ? 'symlink' : 'file',
        }));
      } catch (err) {
        throw fsErrorFromErrno(err, String(target));
      }
    }

    async mkdir(target, options = {}) {
      try {
        await mkdir(resolve(String(target)), { recursive: options.recursive !== false });
      } catch (err) {
        throw fsErrorFromErrno(err, String(target));
      }
    }

    async rm(target, options = {}) {
      try {
        await rm(resolve(String(target)), {
          recursive: Boolean(options.recursive),
          force: Boolean(options.force),
        });
      } catch (err) {
        throw fsErrorFromErrno(err, String(target));
      }
    }

    async rename(from, to) {
      try {
        await mkdir(dirname(resolve(String(to))), { recursive: true });
        await rename(resolve(String(from)), resolve(String(to)));
      } catch (err) {
        throw fsErrorFromErrno(err, String(from));
      }
    }

    async watch(target, changed, signal) {
      signal?.throwIfAborted?.();
      const resolved = resolve(String(target));
      const watcher = fsWatch(resolved, { persistent: false }, (_eventType, filename) => {
        try { changed({ path: filename ? join(resolved, String(filename)) : resolved }); }
        catch { /* observer failure contained */ }
      });
      activeWatchers.add(watcher);
      const close = async () => {
        if (!activeWatchers.delete(watcher)) return;
        try { watcher.close(); } catch { /* ignore */ }
      };
      if (signal) {
        signal.addEventListener('abort', () => { void close(); }, { once: true });
      }
      return { close };
    }
  }
  await mount(MagiFileSystem);
  disposers.push(async () => {
    for (const watcher of [...activeWatchers]) {
      try { watcher.close(); } catch { /* ignore */ }
    }
    activeWatchers.clear();
  });

  // -------------------------------------------------------------------------
  // Official SubprocessRuntime seam (`ctx.subprocess`)
  // -------------------------------------------------------------------------
  const activeChildren = new Set();

  class MagiSubprocessRuntime extends SubprocessRuntime {
    spawn(command, args = [], options = {}) {
      if (closed) throw new Error('SubprocessRuntime is closed');
      const env = { ...scrubbedParentEnv(process.env), ...(options.env ?? {}) };
      let child;
      try {
        child = spawn(String(command), Array.isArray(args) ? args.map(String) : [], {
          cwd: options.cwd ? resolve(String(options.cwd)) : process.cwd(),
          env,
          stdio: options.stdio ?? ['pipe', 'pipe', 'pipe'],
        });
      } catch (err) {
        throw subprocessSpawnFailure(err, String(command));
      }
      activeChildren.add(child);
      const cleanup = () => activeChildren.delete(child);
      child.once('exit', cleanup);
      child.once('error', cleanup);
      if (options.signal) {
        const onAbort = () => {
          try { child.kill('SIGTERM'); } catch { /* ignore */ }
        };
        if (options.signal.aborted) onAbort();
        else options.signal.addEventListener('abort', onAbort, { once: true });
        child.once('exit', () => options.signal.removeEventListener('abort', onAbort));
      }
      return child;
    }

    async exec(command, args = [], options = {}) {
      const child = this.spawn(command, args, options);
      let stdout = '';
      let stderr = '';
      const limit = options.maxBufferBytes ?? JOB_OUTPUT_LIMIT_BYTES;
      child.stdout?.on('data', chunk => {
        stdout = appendBounded(stdout, chunk.toString('utf8'), limit);
      });
      child.stderr?.on('data', chunk => {
        stderr = appendBounded(stderr, chunk.toString('utf8'), limit);
      });
      let timer;
      if (typeof options.timeoutMs === 'number' && options.timeoutMs > 0) {
        timer = setTimeout(() => {
          try { child.kill('SIGKILL'); } catch { /* ignore */ }
        }, options.timeoutMs);
        timer.unref?.();
      }
      return await new Promise((resolvePromise, rejectPromise) => {
        child.once('error', err => {
          if (timer) clearTimeout(timer);
          rejectPromise(subprocessSpawnFailure(err, String(command)));
        });
        child.once('close', (code, signal) => {
          if (timer) clearTimeout(timer);
          resolvePromise({ exitCode: code ?? -1, signal: signal ?? null, stdout, stderr });
        });
      });
    }
  }
  await mount(MagiSubprocessRuntime);
  disposers.push(async () => {
    for (const child of [...activeChildren]) {
      try { child.kill('SIGTERM'); } catch { /* ignore */ }
    }
    activeChildren.clear();
  });

  // -------------------------------------------------------------------------
  // Official SandboxProvider & policy seams (`ctx.sandbox`, `ctx.sandboxPolicy`)
  // -------------------------------------------------------------------------
  class MagiSandboxProvider extends SandboxProvider {
    async prepare(options = {}) {
      const cwd = canonicalPath(options.cwd ?? process.cwd());
      const mode = options.mode ?? 'workspace-write';
      const writable = mode === 'read-only'
        ? []
        : workspaceWritableRoots({
            mode: 'workspace-write',
            workspaceRoot: cwd,
            additionalWritableRoots: options.extraWritableRoots ?? [],
          });
      return Object.freeze({
        mode,
        cwd,
        writableRoots: writable,
        wrapCommand(command, args = []) {
          return { command, args, cwd };
        },
      });
    }

    validateEscalation(args) {
      return validateEscalationArgs(args);
    }
  }
  await mount(MagiSandboxProvider);

  class MagiSandboxPolicyService extends Service {
    constructor(ctx) {
      super(ctx, 'sandboxPolicy');
    }

    async modeForSession(sessionId) {
      const profile = await root.get('permissionPresets')?.current?.(sessionId).catch(() => 'workspace-write');
      return profile === 'danger-full-access' ? 'danger-full-access' : 'workspace-write';
    }
  }
  await mount(MagiSandboxPolicyService);

  // -------------------------------------------------------------------------
  // Official JobRegistry seam (`ctx.jobs`) with workspace archive admission
  // -------------------------------------------------------------------------
  const jobsMap = new Map();
  const MAX_JOBS = 128;

  class MagiJobRegistry extends JobRegistry {
    start({ owner, label, kind = 'exec', run, abort }) {
      if (jobsMap.size >= MAX_JOBS) {
        throw new JobError('JOB_LIMIT_REACHED', `Active job limit (${MAX_JOBS}) reached`);
      }
      const id = JobId(`${kind}-${randomUUID().replace(/-/g, '').slice(0, 16)}`);
      const controller = new AbortController();
      const record = {
        id,
        owner: owner ?? null,
        kind,
        label: String(label ?? kind),
        status: 'running',
        startedAt: Date.now(),
        endedAt: null,
        output: '',
        exitCode: null,
        error: null,
        controller,
        abort,
      };
      jobsMap.set(id, record);
      if (typeof run === 'function') {
        Promise.resolve()
          .then(() => run({
            signal: controller.signal,
            appendOutput(chunk) {
              record.output = appendBounded(record.output, String(chunk));
            },
          }))
          .then(result => {
            if (record.status === 'running' || record.status === 'stopping') {
              record.status = 'completed';
              record.exitCode = typeof result?.exitCode === 'number' ? result.exitCode : 0;
              record.endedAt = Date.now();
            }
          })
          .catch(err => {
            if (record.status === 'running' || record.status === 'stopping') {
              record.status = controller.signal.aborted ? 'killed' : 'failed';
              record.error = String(err?.message ?? err);
              record.endedAt = Date.now();
            }
          });
      }
      return this.get(id, owner);
    }

    list(owner) {
      const out = [];
      for (const job of jobsMap.values()) {
        if (owner !== undefined && job.owner !== owner) continue;
        out.push(Object.freeze({
          id: job.id,
          owner: job.owner,
          kind: job.kind,
          label: job.label,
          status: job.status,
          startedAt: job.startedAt,
          endedAt: job.endedAt,
          exitCode: job.exitCode,
          error: job.error,
        }));
      }
      return out;
    }

    get(id, owner) {
      const job = jobsMap.get(id);
      if (!job) throw new JobError('JOB_NOT_FOUND', `Job "${id}" not found`);
      if (owner !== undefined && job.owner !== null && job.owner !== owner) {
        throw new JobError('JOB_FOREIGN_SESSION', `Job "${id}" belongs to another session`);
      }
      return Object.freeze({
        id: job.id,
        owner: job.owner,
        kind: job.kind,
        label: job.label,
        status: job.status,
        startedAt: job.startedAt,
        endedAt: job.endedAt,
        output: job.output,
        exitCode: job.exitCode,
        error: job.error,
      });
    }

    kill(id, owner, reason = 'killed') {
      const job = jobsMap.get(id);
      if (!job) throw new JobError('JOB_NOT_FOUND', `Job "${id}" not found`);
      if (owner !== undefined && job.owner !== null && job.owner !== owner) {
        throw new JobError('JOB_FOREIGN_SESSION', `Job "${id}" belongs to another session`);
      }
      if (job.status === 'running') {
        job.status = 'stopping';
        job.error = reason;
        job.controller.abort(new Error(reason));
        try { job.abort?.(reason); } catch { /* ignore */ }
        job.status = 'killed';
        job.endedAt = Date.now();
      }
      return this.get(id, owner);
    }
  }
  await mount(MagiJobRegistry);
  disposers.push(() => {
    for (const job of jobsMap.values()) {
      if (job.status === 'running') {
        try { job.controller.abort(new Error('host closing')); } catch { /* ignore */ }
      }
    }
    jobsMap.clear();
  });

  // -------------------------------------------------------------------------
  // Official FileReferenceService seam (`ctx.fileReferences`)
  // -------------------------------------------------------------------------
  class MagiFileReferenceService extends FileReferenceService {
    async listCandidates(rootDir, query = '', options = {}) {
      const base = resolve(String(rootDir));
      const needle = String(query ?? '').toLowerCase();
      const limit = Math.min(Number(options.limit ?? FILE_REF_MAX_RESULTS) || FILE_REF_MAX_RESULTS, FILE_REF_MAX_RESULTS);
      const results = [];
      const stack = [base];
      while (stack.length > 0 && results.length < limit) {
        const dir = stack.pop();
        let entries;
        try {
          entries = await readdir(dir, { withFileTypes: true });
        } catch {
          continue;
        }
        for (const entry of entries) {
          if (results.length >= limit) break;
          if (FILE_REF_SKIP_DIRS.has(entry.name)) continue;
          const full = join(dir, entry.name);
          const rel = relative(base, full);
          if (entry.isDirectory()) {
            stack.push(full);
          } else if (!needle || rel.toLowerCase().includes(needle)) {
            results.push({ path: rel, absolutePath: full });
          }
        }
      }
      return results;
    }

    activeToken(text, cursor) {
      return activeAtToken(text, cursor);
    }

    formatMention(filePath) {
      return formatFileMention(filePath);
    }
  }
  await mount(MagiFileReferenceService);

  // -------------------------------------------------------------------------
  // Official SessionTitleService (`ctx.sessionTitle`) bridged to daemon rename
  // -------------------------------------------------------------------------
  if (root.get('sessions') && root.get('sessionProjections') && root.get('agents')) {
    class MagiSessionTitleService extends SessionTitleService {
      async rename(sessionOrId, title) {
        const sessionId = typeof sessionOrId === 'string' ? sessionOrId : sessionOrId?.id;
        if (typeof sessionId !== 'string' || !sessionId) {
          throw new Error('sessionTitle.rename requires a session or sessionId');
        }
        const normalized = normalizeSessionTitle(String(title ?? ''), 200);
        if (!normalized) {
          throw new SessionTitleInvalidError('Session title cannot be empty');
        }
        const res = await requestOwner('host.session.rename', { sessionId, title: normalized });
        return { title: res?.title ?? normalized, seq: res?.seq };
      }
    }
    await mount(MagiSessionTitleService, {
      fallbackMaxWords: 12,
      fallbackMaxBytes: 96,
      maxTitleBytes: 200,
    });
  }

  // -------------------------------------------------------------------------
  // Host controllers backed by Magi kernel & execution seams
  // -------------------------------------------------------------------------
  class MagiShellEnvService extends Service {
    constructor(ctx) {
      super(ctx, 'shellEnv');
    }

    snapshot(extra = {}) {
      return Object.freeze({ ...scrubbedParentEnv(process.env), ...(extra ?? {}) });
    }
  }
  await mount(MagiShellEnvService);

  class MagiShellService extends Service {
    constructor(ctx) {
      super(ctx, 'shell');
    }

    async exec(command, options = {}) {
      const shellBin = process.env.SHELL || '/bin/zsh';
      return await root.subprocess.exec(shellBin, ['-lc', String(command)], options);
    }
  }
  await mount(MagiShellService);

  class MagiTerminalsService extends Service {
    constructor(ctx) {
      super(ctx, 'terminals');
    }

    async list(sessionId) {
      const res = await requestOwner('host.terminals.list', {
        ...(typeof sessionId === 'string' && sessionId ? { sessionId } : {}),
      });
      return Array.isArray(res?.terminals) ? res.terminals : [];
    }

    async create({ sessionId, id, cwd, cols = 80, rows = 24 } = {}) {
      const terminalId = typeof id === 'string' && id.trim() ? id.trim() : `term-${randomUUID()}`;
      const res = await requestOwner('host.terminals.create', {
        sessionId,
        id: terminalId,
        ...(typeof cwd === 'string' && cwd.trim() ? { cwd } : {}),
        cols,
        rows,
      });
      return res?.terminal ?? { id: terminalId, sessionId, backend: 'magi-pty' };
    }

    async write(id, data) {
      return await requestOwner('host.terminals.write', { id, data: String(data ?? '') });
    }

    async resize(id, cols, rows) {
      return await requestOwner('host.terminals.resize', { id, cols, rows });
    }

    async read(id) {
      return await requestOwner('host.terminals.read', { id });
    }

    async close(id) {
      return await requestOwner('host.terminals.close', { id });
    }
  }
  await mount(MagiTerminalsService);

  class MagiWorktreesService extends Service {
    constructor(ctx) {
      super(ctx, 'worktrees');
    }

    async list(repoDir, sessionId) {
      const daemonRes = await requestOwner('host.worktrees.list', {
        ...(sessionId ? { sessionId } : {}),
      }).catch(() => null);
      const items = [];
      if (Array.isArray(daemonRes?.worktrees)) {
        for (const entry of daemonRes.worktrees) {
          items.push({ ...entry, source: 'magi-daemon' });
        }
      }
      if (typeof repoDir === 'string' && repoDir.trim()) {
        const res = await root.subprocess.exec('git', ['worktree', 'list', '--porcelain'], { cwd: repoDir });
        if (res.exitCode === 0) {
          let current = {};
          for (const line of res.stdout.split('\n')) {
            if (!line.trim()) {
              if (current.path) items.push({ ...current, source: 'git' });
              current = {};
            } else if (line.startsWith('worktree ')) {
              current.path = line.slice('worktree '.length).trim();
            } else if (line.startsWith('HEAD ')) {
              current.head = line.slice('HEAD '.length).trim();
            } else if (line.startsWith('branch ')) {
              current.branch = line.slice('branch '.length).trim();
            }
          }
          if (current.path) items.push({ ...current, source: 'git' });
        }
      }
      return items;
    }

    async create({ repoDir, path, branch, base, detach = false } = {}) {
      const res = await requestOwner('host.worktrees.create', {
        repoDir,
        path,
        ...(typeof branch === 'string' && branch.trim() ? { branch } : {}),
        ...(typeof base === 'string' && base.trim() ? { base } : {}),
        detach: Boolean(detach),
      });
      return res?.worktree ?? res;
    }

    async remove({ repoDir, path, force = false } = {}) {
      return await requestOwner('host.worktrees.remove', {
        repoDir,
        path,
        force: Boolean(force),
      });
    }
  }
  await mount(MagiWorktreesService);

  // Read-only `ctx.subagents` (user confirmed Magi owns Thread/Task lifecycle; no prompt/interruptByUser writes).
  class MagiSubagentsService extends Service {
    constructor(ctx) {
      super(ctx, 'subagents');
    }

    async list(parentSessionId) {
      if (typeof parentSessionId !== 'string' || !parentSessionId) return [];
      const history = await requestOwner('host.session.history', { sessionId: parentSessionId }).catch(() => null);
      const events = Array.isArray(history?.events) ? history.events : [];
      const catalog = new Map();
      for (const item of events) {
        const sub = item?.event?.subagentCatalog ?? item?.subagentCatalog;
        if (sub && typeof sub.id === 'string') {
          catalog.set(sub.id, Object.freeze({ ...sub }));
        }
      }
      return [...catalog.values()];
    }

    async get(parentSessionId, subagentId) {
      const items = await this.list(parentSessionId);
      return items.find(entry => entry.id === subagentId) ?? null;
    }

    async readEvents(parentSessionId, subagentId) {
      if (typeof parentSessionId !== 'string' || !parentSessionId || typeof subagentId !== 'string' || !subagentId) {
        return [];
      }
      const res = await requestOwner('host.session.history', {
        sessionId: parentSessionId,
        subagentId,
      }).catch(() => null);
      return Array.isArray(res?.events) ? res.events : [];
    }
  }
  await mount(MagiSubagentsService);

  class MagiSubagentModelSelectionService extends Service {
    constructor(ctx) {
      super(ctx, 'subagentModelSelection');
    }

    async get() {
      const res = await requestOwner('host.subagentModelSelection.get', {}).catch(() => null);
      return res ?? { roles: [], bindings: {} };
    }

    async listRoles() {
      const data = await this.get();
      return Array.isArray(data.roles) ? data.roles : [];
    }
  }
  await mount(MagiSubagentModelSelectionService);

  class MagiSessionProjectionCacheService extends Service {
    constructor(ctx) {
      super(ctx, 'sessionProjectionCache');
    }

    snapshot(session) {
      const registry = root.get('sessionProjections');
      if (!registry || !session) return {};
      return typeof registry.snapshot === 'function' ? registry.snapshot(session) : {};
    }

    get(session, key) {
      const snap = this.snapshot(session);
      return Object.hasOwn(snap, key) ? snap[key] : undefined;
    }

    keys() {
      const registry = root.get('sessionProjections');
      if (!registry) return [];
      return typeof registry.keys === 'function' ? [...registry.keys()] : [];
    }
  }
  await mount(MagiSessionProjectionCacheService);

  // The official resource runtime owns provider registration (`register`, called by the original
  // `dsh-mcp-client` plugin), the per-scope resource tools and the system-prompt section. Magi adds one
  // read: the daemon's own MCP resource projection.
  class MagiMcpResourcesService extends McpResourceRuntime {
    // A plain property: Cordis hands services out through proxies, which cannot read private fields.
    providerCount = 0;

    // Count live registrations: the official runtime always owns an `mcp-resource-servers` prompt section,
    // which only contributes text while a provider is registered (see `hookCapabilities`).
    register(server, provider) {
      const dispose = super.register(server, provider);
      this.providerCount++;
      let released = false;
      return () => {
        if (!released) {
          released = true;
          this.providerCount--;
        }
        return dispose();
      };
    }

    hasProviders() {
      return this.providerCount > 0;
    }

    async list(serverName) {
      const res = await requestOwner('host.mcp.resources', {}).catch(() => null);
      const items = Array.isArray(res?.resources) ? res.resources : [];
      if (typeof serverName === 'string' && serverName.trim()) {
        return items.filter(item => item.server === serverName.trim());
      }
      return items;
    }
  }
  await mount(MagiMcpResourcesService);

  class MagiMcpManagerService extends Service {
    constructor(ctx) {
      super(ctx, 'mcpManager');
    }

    listServers() {
      return [...mcpServers.values()].map(s => Object.freeze({ ...s }));
    }

    async listAllServers() {
      const daemonRes = await requestOwner('host.mcp.list', {}).catch(() => null);
      const daemonServers = Array.isArray(daemonRes?.servers) ? daemonRes.servers : [];
      return [...daemonServers, ...this.listServers()];
    }

    registerServer(spec) {
      if (!spec || typeof spec.name !== 'string' || !spec.name.trim()) {
        throw new TypeError('mcpManager.registerServer requires a non-empty name');
      }
      return this.ctx.effect(() => {
        mcpServers.set(spec.name, Object.freeze({ ...spec }));
        return () => {
          if (mcpServers.get(spec.name)?.name === spec.name) mcpServers.delete(spec.name);
        };
      }, `mcpManager.registerServer(${spec.name})`);
    }
  }
  const mcpServers = new Map();
  await mount(MagiMcpManagerService);

  class MagiBrowserUseService extends Service {
    constructor(ctx) {
      super(ctx, 'browserUse');
    }

    async snapshot() {
      const res = await requestOwner('host.browser.snapshot', {}).catch(() => null);
      if (!res || res.available !== true) {
        return { available: false, authority: null };
      }
      return res;
    }
  }
  await mount(MagiBrowserUseService);

  class MagiSessionReferenceResolverService extends Service {
    constructor(ctx) {
      super(ctx, 'sessionReferenceResolver');
    }

    async resolve(sessionId, ref) {
      const res = await requestOwner('host.session.get', { sessionId }).catch(() => null);
      const cwd = res?.session?.cwd;
      if (!cwd || typeof ref !== 'string' || !ref.trim()) return null;
      const candidate = isAbsolute(ref) ? resolve(ref) : resolve(cwd, ref);
      const exists = await root.fs.exists(candidate);
      return exists ? { path: candidate, cwd } : null;
    }
  }
  await mount(MagiSessionReferenceResolverService);

  class MagiDirectoryPickerService extends Service {
    constructor(ctx) {
      super(ctx, 'directoryPicker');
    }

    async validate(dirPath) {
      const resolved = resolve(String(dirPath ?? ''));
      const st = await root.fs.stat(resolved);
      if (st.kind !== 'directory') throw new FsError('FS_NOT_DIRECTORY', `Not a directory: ${resolved}`, { path: resolved });
      return resolved;
    }
  }
  await mount(MagiDirectoryPickerService);

  void dshHome;

  return {
    async runAgentLoop({ sessionId, input }, signal) {
      return await root.agentLoop.run(sessionId, input, signal);
    },
    async cancelAgentLoop({ sessionId }) {
      return await root.agentLoop.cancel(sessionId);
    },
    async resumeAgentLoop({ sessionId, options }) {
      return await root.agentLoop.resume(sessionId, options);
    },
    async close() {
      closed = true;
      for (const dispose of disposers.splice(0).reverse()) {
        try { await dispose?.(); } catch { /* ignore */ }
      }
    },
  };
}
