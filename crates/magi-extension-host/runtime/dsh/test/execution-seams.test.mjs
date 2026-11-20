import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { FsError } from '@deepseek-ai/dsh-fs';
import { JobError } from '@deepseek-ai/dsh-jobs';
import {
  SessionAlreadyOwnedError,
  SessionHandleClosedError,
  SessionReadOnlyError,
} from '@deepseek-ai/dsh-session-persistence';
import { SessionTitleInvalidError } from '@deepseek-ai/dsh-session-title';
import { CommittedTree } from '../committed-tree.mjs';
import { createJsonlSessionPersistenceBackend } from '../execution-seams.mjs';
import { PluginLifecycle } from '../lifecycle.mjs';
import { openHostServices } from '../services.mjs';
import { createSessionReader } from '../session-reader.mjs';

async function createSeamsHost(requestOwner = async () => ({})) {
  const dir = await mkdtemp(join(tmpdir(), 'magi-dsh-seams-'));
  const host = new PluginLifecycle({
    hostGeneration: 1,
    onStatus: () => {},
    onDiagnostic: () => {},
  });
  const tree = new CommittedTree({
    lifecycle: host,
    hostGeneration: 1,
    onDiagnostic: () => {},
    requestOwner,
  });
  const sessionReader = createSessionReader(requestOwner);
  const services = await openHostServices({
    lifecycle: host,
    tree,
    dshHome: join(dir, 'dsh-home'),
    requestOwner,
    sessionReader,
    notifyOwner: () => {},
  });
  return {
    dir,
    root: host.context,
    services,
    async close() {
      await services.close();
      await host.close?.();
      await rm(dir, { recursive: true, force: true });
    },
  };
}

test('agentLoop enforces provider registration, selection, fail-loudly on unavailable, cancel and resume', async () => {
  const ownerCalls = [];
  const env = await createSeamsHost(async (method, params) => {
    ownerCalls.push({ method, params });
    if (method === 'host.session.list') return { factsSequence: 1, sessions: [] };
    return { accepted: true };
  });
  try {
    const { root } = env;
    assert.equal(root.agentLoop.selected(), 'magi-native');
    await root.agentLoop.run('s1', { text: 'hello native' });
    assert.ok(ownerCalls.some(c => c.method === 'host.session.prompt' && c.params.text === 'hello native'));

    const runs = [];
    const disposeLoop = root.agentLoop.register({
      id: 'custom-loop',
      title: 'Custom Loop',
      capabilities: { context: 'executor_owned', steer: true, resume: true },
      async run({ sessionId, input }) {
        runs.push({ kind: 'run', sessionId, input });
        return { ok: true };
      },
      async cancel({ sessionId }) {
        runs.push({ kind: 'cancel', sessionId });
        return { cancelled: true };
      },
      async resume({ sessionId }) {
        runs.push({ kind: 'resume', sessionId });
        return { resumed: true };
      },
    });
    assert.ok(root.agentLoop.list().some(p => p.id === 'custom-loop'));
    root.agentLoop.select('custom-loop');
    await root.agentLoop.run('s1', { text: 'via custom' });
    await root.agentLoop.cancel('s1');
    await root.agentLoop.resume('s1');
    assert.equal(runs.length, 3);

    // After provider unregisters, selected provider MUST fail loudly rather than silently fallback
    disposeLoop();
    await assert.rejects(
      () => root.agentLoop.run('s1', { text: 'should fail' }),
      /currently unavailable/,
    );
    root.agentLoop.select('magi-native');
  } finally {
    await env.close();
  }
});

test('writable JSONL SessionPersistence enforces single-writer lock, contiguous append, flush barrier, and closed-handle guard', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'magi-dsh-persist-'));
  const backend = createJsonlSessionPersistenceBackend(dir);
  try {
    const header = await backend.create({
      id: 'sess-jsonl-1',
      cwd: '/tmp/work',
      createdAt: 1000,
    });
    assert.equal(header.id, 'sess-jsonl-1');

    const writer = await backend.open('sess-jsonl-1', 'write');
    // Second concurrent writer MUST throw SessionAlreadyOwnedError
    await assert.rejects(() => backend.open('sess-jsonl-1', 'write'), SessionAlreadyOwnedError);

    // Read handle can read concurrently, but cannot append
    const reader = await backend.open('sess-jsonl-1', 'read');
    await assert.rejects(
      () => reader.append([{ type: 'turn/start', time: 1001, data: { turn: 1 } }]),
      SessionReadOnlyError,
    );

    const rev = await writer.append([
      { type: 'turn/start', time: 1001, data: { turn: 1 } },
      { type: 'turn/end', time: 1002, data: { turn: 1, reason: { kind: 'completed' } } },
    ]);
    assert.equal(rev, 2);
    await writer.flush();
    await writer.close();
    await assert.rejects(() => writer.read(0), SessionHandleClosedError);
    await reader.close();

    const info = await backend.stat('sess-jsonl-1');
    assert.equal(info.eventCount, 2);
    const listed = await backend.list();
    assert.equal(listed.length, 1);

    const reopen = await backend.open('sess-jsonl-1', 'read');
    const events = await reopen.read(0);
    assert.equal(events.length, 2);
    assert.deepEqual(events.map(e => [e.seq, e.type]), [[0, 'turn/start'], [1, 'turn/end']]);
    await reopen.close();
  } finally {
    await backend.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test('execution seams deliver fs, subprocess, sandbox, jobs, fileReferences, sessionTitle, shell, terminals, mcpManager and directoryPicker', async () => {
  const renamed = [];
  const env = await createSeamsHost(async (method, params) => {
    if (method === 'host.session.list') return { factsSequence: 1, sessions: [] };
    if (method === 'host.session.rename') {
      renamed.push(params);
      return { title: params.title, seq: 9 };
    }
    return {};
  });
  try {
    const { dir, root } = env;

    // 1. ctx.fs: atomic write, optimistic version conflict, stat, readdir, watch
    const file = join(dir, 'sample.txt');
    const w1 = await root.fs.writeText(file, 'hello v1');
    const r1 = await root.fs.readText(file);
    assert.equal(r1.text, 'hello v1');
    assert.equal(r1.version, w1.version);
    await root.fs.writeText(file, 'hello v2', { expectedVersion: w1.version });
    await assert.rejects(
      () => root.fs.writeText(file, 'stale', { expectedVersion: w1.version }),
      FsError,
    );
    assert.equal((await root.fs.stat(file)).kind, 'file');
    assert.ok((await root.fs.readdir(dir)).some(e => e.name === 'sample.txt'));

    // 2. ctx.subprocess & ctx.shellEnv
    const envSnap = root.shellEnv.snapshot({ MAGI_TEST_VAR: 'ok' });
    assert.equal(envSnap.MAGI_TEST_VAR, 'ok');
    const execRes = await root.subprocess.exec(process.execPath, ['-e', 'process.stdout.write("sub-ok")']);
    assert.equal(execRes.exitCode, 0);
    assert.equal(execRes.stdout, 'sub-ok');

    // 3. ctx.sandbox & ctx.sandboxPolicy
    const sb = await root.sandbox.prepare({ cwd: dir, mode: 'workspace-write' });
    assert.equal(sb.mode, 'workspace-write');
    assert.ok(sb.writableRoots.length >= 1);
    assert.equal(await root.sandboxPolicy.modeForSession('s1'), 'workspace-write');

    // 4. ctx.jobs isolation & kill
    const job = root.jobs.start({
      owner: 'sess-a',
      label: 'sleepy',
      async run({ signal, appendOutput }) {
        appendOutput('started');
        await new Promise((_res, rej) => signal.addEventListener('abort', () => rej(new Error('aborted'))));
      },
    });
    assert.equal(root.jobs.list('sess-a').length, 1);
    assert.throws(() => root.jobs.get(job.id, 'sess-b'), JobError);
    const killed = root.jobs.kill(job.id, 'sess-a', 'done');
    assert.equal(killed.status, 'killed');

    // 5. ctx.fileReferences
    const candidates = await root.fileReferences.listCandidates(dir, 'sample');
    assert.ok(candidates.some(c => c.path === 'sample.txt'));

    // 6. ctx.sessionTitle -> bridges directly to daemon host.session.rename
    const titleOut = await root.sessionTitle.rename('sess-title-1', '  Clean Title  ');
    assert.equal(titleOut.title, 'Clean Title');
    assert.equal(renamed.length, 1);
    assert.equal(renamed[0].title, 'Clean Title');
    await assert.rejects(() => root.sessionTitle.rename('sess-title-1', '   '), SessionTitleInvalidError);

    // 7. ctx.mcpManager, ctx.mcpResources, ctx.browserUse, ctx.terminals, ctx.worktrees, ctx.subagents, ctx.subagentModelSelection, ctx.sessionProjectionCache, ctx.directoryPicker & ctx.sessionPersistenceBackends
    const offMcp = root.mcpManager.registerServer({ name: 'local-docs', transport: 'stdio' });
    assert.equal(root.mcpManager.listServers().length, 1);
    const allMcp = await root.mcpManager.listAllServers();
    assert.ok(allMcp.some(s => s.name === 'local-docs'));
    offMcp();
    assert.equal(root.mcpManager.listServers().length, 0);
    assert.ok(Array.isArray(await root.mcpResources.list()));
    // The service is the official runtime: the original `dsh-mcp-client` plugin registers a provider through
    // `register(server, provider)` and gets back its disposer. A shell without `register` fails every plugin that
    // uses an MCP client (the fixed Rea 1.2.0 sample did).
    assert.equal(typeof root.mcpResources.register, 'function');
    assert.equal(root.mcpResources.hasProviders(), false);
    const offProvider = root.mcpResources.register('docs', { request: async () => ({ resources: [] }) });
    assert.equal(root.mcpResources.hasProviders(), true);
    assert.throws(() => root.mcpResources.register('docs', { request: async () => ({}) }), /already registered/);
    offProvider();
    offProvider();
    assert.equal(root.mcpResources.hasProviders(), false, 'disposal is idempotent and releases the provider');
    const browserSnap = await root.browserUse.snapshot();
    assert.equal(browserSnap.available, false);
    assert.equal(typeof root.terminals.create, 'function');
    assert.equal(typeof root.terminals.write, 'function');
    assert.equal(typeof root.worktrees.create, 'function');
    assert.equal(typeof root.worktrees.remove, 'function');
    // ctx.subagents is strictly read-only (no prompt/interruptByUser)
    assert.equal(typeof root.subagents.list, 'function');
    assert.equal(typeof root.subagents.get, 'function');
    assert.equal('prompt' in root.subagents, false);
    assert.equal('interruptByUser' in root.subagents, false);
    const modelSel = await root.subagentModelSelection.get();
    assert.ok(modelSel && typeof modelSel === 'object');
    assert.ok(Array.isArray(root.sessionProjectionCache.keys()));
    assert.equal(await root.directoryPicker.validate(dir), dir);
    await assert.rejects(() => root.directoryPicker.validate(file), FsError);
    assert.ok(typeof root.sessionPersistenceBackends.createJsonlBackend === 'function');
  } finally {
    await env.close();
  }
});
