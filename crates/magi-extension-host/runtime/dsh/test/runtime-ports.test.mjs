import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { defineTool } from '@deepseek-ai/dsh-tools';
import z from '@deepseek-ai/schemastery';
import { CommittedTree } from '../committed-tree.mjs';
import { PluginLifecycle } from '../lifecycle.mjs';
import { openHostServices } from '../services.mjs';
import { createSessionReader } from '../session-reader.mjs';

async function createTestHost(requestOwner = async () => ({})) {
  const dir = await mkdtemp(join(tmpdir(), 'magi-dsh-ports-'));
  const notifications = [];
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
    notifyOwner: (method, payload) => notifications.push({ method, payload }),
  });
  return {
    dir,
    host,
    root: host.context,
    services,
    notifications,
    async close() {
      await services.close();
      await host.close?.();
      await rm(dir, { recursive: true, force: true });
    },
  };
}

test('tool pipeline enforces pre-execute, approval, monotonic guards, execute wrap, post-execute, pruner, result, and restrict', async () => {
  const env = await createTestHost(async (method, params) => {
    const sessFact = {
      sessionId: 'sess-p3',
      title: 'Tool Pipeline',
      createdAt: 1,
      updatedAt: 1,
      status: 'idle',
      workspaceId: null,
      cwd: '/tmp/p3',
    };
    if (method === 'host.session.list') {
      return { factsSequence: 1, sessions: [sessFact] };
    }
    if (method === 'host.session.get' && (params?.sessionId === 'sess-p3' || params?.sessionId === 'sess-cold-restrict')) {
      return {
        factsSequence: 1,
        session: { ...sessFact, sessionId: params.sessionId },
      };
    }
    if (method === 'host.session.history') {
      return {
        session: { id: 'sess-p3', createdAt: 1, cwd: '/tmp/p3' },
        through: 0,
        nextAfter: null,
        events: [],
      };
    }
    return {};
  });
  try {
    const { root, services } = env;
    const observed = [];

    // Register a custom tool
    root.tools.register(defineTool({
      name: 'echo_tool',
      description: 'Echo text',
      parameters: {
        text: { type: 'string', required: true, description: 'Text to echo' },
      },
      output: {
        schema: {
          type: 'object',
          properties: { text: { type: 'string', required: true } },
          additionalProperties: false,
        },
        render(_args, value) {
          return [{ type: 'text', text: value.text }];
        },
      },
      async execute(args) {
        observed.push(`exec:${args.text}`);
        return { text: `echo:${args.text}` };
      },
    }));

    // 1. tools/pre-execute deny blocks execution before running
    const offDeny = root.on('tools/pre-execute', async (exec, next) => {
      if (exec.arguments?.text === 'forbidden') {
        return { kind: 'deny', reason: 'blocked by policy' };
      }
      return await next();
    });
    const denied = await services.preExecuteTool({
      sessionId: 'sess-p3',
      callId: 'call-1',
      name: 'echo_tool',
      arguments: { text: 'forbidden' },
    });
    assert.equal(denied.kind, 'deny');
    assert.equal(denied.reason, 'blocked by policy');
    offDeny();

    // 1b. tools/pre-execute argument rewrite flows through into executeTool
    const offRewrite = root.on('tools/pre-execute', async (exec, next) => {
      if (exec.arguments?.text === 'rewrite-me') {
        return { kind: 'allow', arguments: { text: 'rewritten-by-pre' } };
      }
      return await next();
    });
    const rewrittenOut = await services.executeTool({
      name: 'echo_tool',
      args: { text: 'rewrite-me' },
      sessionId: 'sess-p3',
    });
    assert.equal(rewrittenOut.isError, false);
    assert.equal(rewrittenOut.content[0].text, 'echo:rewritten-by-pre');
    observed.length = 0;
    offRewrite();

    // 2. tools/pre-execute ask -> ctx.approval allows once
    const offAsk = root.on('tools/pre-execute', async (exec, next) => {
      if (exec.arguments?.text === 'need-approval') {
        return { kind: 'ask', reason: 'confirm echo' };
      }
      return await next();
    });
    const offApproval = root.on('approval/request', async (req, next) => {
      if (req.toolName === 'echo_tool') return 'allowed-once';
      return await next();
    }, true);
    const approved = await services.preExecuteTool({
      sessionId: 'sess-p3',
      callId: 'call-2',
      name: 'echo_tool',
      arguments: { text: 'need-approval' },
    });
    assert.equal(approved.kind, 'allow');
    assert.equal(approved.approvedByPlugin, true);

    // 3. Monotonic tools.guard still rejects even after approval allows
    const offGuard = root.tools.guard(exec => {
      if (exec.arguments?.text === 'need-approval') return 'monotonic guard denial';
      return undefined;
    });
    const guardRejected = await services.preExecuteTool({
      sessionId: 'sess-p3',
      callId: 'call-3',
      name: 'echo_tool',
      arguments: { text: 'need-approval' },
    });
    assert.equal(guardRejected.kind, 'deny');
    assert.match(guardRejected.reason, /monotonic guard denial/);
    offGuard();
    offApproval();
    offAsk();

    // 4. Full executeTool runs tools/execute wrap + toolResultPruner + tools/post-execute + tools/result
    const offWrap = root.on('tools/execute', async (exec, next) => {
      observed.push(`wrap-before:${exec.name}`);
      const res = await next();
      observed.push(`wrap-after:${exec.name}`);
      return res;
    });
    const offPrune = root.toolResultPruner.register(item => ({
      ...item,
      text: `${item.text}|pruned`,
      content: [{ type: 'text', text: `${item.text}|pruned` }],
    }));
    const offPost = root.on('tools/post-execute', async (exec, result, next) => {
      if (exec.arguments?.text === 'block-post') {
        return { kind: 'block', feedback: [{ type: 'text', text: 'redacted by post hook' }] };
      }
      const upstream = await next();
      return upstream;
    });
    const offResult = root.on('tools/result', (exec, res) => {
      observed.push(`result:${exec.name}:${res.isError ? 'err' : 'ok'}`);
    });

    const okOut = await services.executeTool({
      name: 'echo_tool',
      args: { text: 'hello' },
      sessionId: 'sess-p3',
    });
    assert.equal(okOut.isError, false);
    assert.match(okOut.content[0].text, /echo:hello\|pruned/);
    assert.deepEqual(observed, [
      'wrap-before:echo_tool',
      'exec:hello',
      'wrap-after:echo_tool',
      'result:echo_tool:ok',
    ]);

    const postBlocked = await services.executeTool({
      name: 'echo_tool',
      args: { text: 'block-post' },
      sessionId: 'sess-p3',
    });
    assert.equal(postBlocked.isError, true);
    assert.equal(postBlocked.content[0].text, 'redacted by post hook');

    offResult();
    offPost();
    offPrune();
    offWrap();

    // 5. tools.restrict filters visible tools monotonically (must use scoped agent.ctx)
    const agent = root.get('agents')?.get?.('sess-p3');
    assert.ok(agent, 'managed agent for sess-p3 should exist');
    let offRestrict;
    await agent.ctx.inject(['tools'], scope => {
      offRestrict = scope.tools.restrict({ deny: ['echo_tool'] });
    });
    const filtered = await services.filterVisibleTools({
      sessionId: 'sess-p3',
      names: ['read_file', 'echo_tool'],
    });
    assert.deepEqual(filtered.names, ['read_file']);
    assert.equal(services.hookCapabilities().toolsRestrict, true);
    offRestrict?.();

    // 6. Cold session: plugin listening on agent/created restricts a global tool on demand
    const offCreated = root.on('agent/created', async ({ agent: created }) => {
      if (created.id === 'sess-cold-restrict') {
        await created.ctx.inject(['tools'], scope => {
          scope.tools.restrict({ deny: ['echo_tool'] });
        });
      }
    }, { global: true });
    assert.equal(services.hookCapabilities().toolsRestrict, true);
    const coldFiltered = await services.filterVisibleTools({
      sessionId: 'sess-cold-restrict',
      names: ['read_file', 'echo_tool'],
    });
    assert.deepEqual(coldFiltered.names, ['read_file']);
    offCreated();
  } finally {
    await env.close();
  }
});

test('runtime ports cover systemPrompt, permissionPresets, compaction, goals, schedule, tokenMeter, and llm extensions', async () => {
  const ownerCalls = [];
  const goals = new Map();
  let accessProfile = 'restricted';
  const env = await createTestHost(async (method, params) => {
    ownerCalls.push({ method, params });
    if (method === 'host.session.get') {
      return { session: { sessionId: params.sessionId, accessProfile } };
    }
    if (method === 'host.session.setAccessProfile') {
      accessProfile = params.accessProfile;
      return { accessProfile };
    }
    if (method === 'host.goals.create') {
      const goal = { goalId: 'goal-1', sessionId: params.sessionId, objective: params.objective };
      goals.set('goal-1', goal);
      return { goal };
    }
    if (method === 'host.goals.list') return { goals: [...goals.values()] };
    if (method === 'host.goals.get') return { goal: goals.get(params.goalId) ?? null };
    if (method === 'host.goals.update') {
      const next = { ...goals.get(params.goalId), objective: params.objective };
      goals.set(params.goalId, next);
      return { goal: next };
    }
    if (method === 'host.goals.delete') {
      goals.delete(params.goalId);
      return { deleted: true };
    }
    if (method === 'host.session.prompt') return { accepted: true };
    return {};
  });
  try {
    const { root, services } = env;

    // 1. systemPrompt.section & assembleSystemPrompt
    assert.equal(services.hookCapabilities().systemPromptAssemble, false);
    const offSec = root.systemPrompt.section({
      name: 'custom:policy',
      order: 5000,
      text: 'Always be concise and factual.',
    });
    assert.equal(services.hookCapabilities().systemPromptAssemble, true);
    const assembled = await services.assembleSystemPrompt({
      sessionId: 'sess-p5',
      basePrompt: 'base',
    });
    assert.match(assembled.text, /Always be concise and factual/);
    offSec();

    // 2. permissionPresets
    assert.equal(await root.permissionPresets.current('sess-p5'), 'workspace-write');
    await root.permissionPresets.select('sess-p5', 'danger-full-access');
    assert.equal(accessProfile, 'full_access');
    assert.equal(await root.permissionPresets.current('sess-p5'), 'danger-full-access');

    // 3. compaction/before
    const offCompact = root.on('compaction/before', async () => ({
      kind: 'summary',
      plugin: 'test-compactor',
      summary: 'Compact summary from plugin',
    }));
    const compactDecision = await services.beforeCompaction({ sessionId: 'sess-p5', reason: 'auto' });
    assert.equal(compactDecision.kind, 'summary');
    assert.equal(compactDecision.summary, 'Compact summary from plugin');
    offCompact();

    // 4. goals CRUD via daemon owner
    const createdGoal = await root.goals.create('sess-p5', { objective: 'Ship DSH alignment' });
    assert.equal(createdGoal.goalId, 'goal-1');
    assert.equal((await root.goals.list('sess-p5')).length, 1);
    const updatedGoal = await root.goals.update('sess-p5', 'goal-1', { objective: 'Shipped' });
    assert.equal(updatedGoal.objective, 'Shipped');
    await root.goals.delete('sess-p5', 'goal-1');
    assert.equal(await root.goals.get('sess-p5', 'goal-1'), null);

    // 5. schedule durable firing via storageDomain
    const job = await root.schedule.create({
      sessionId: 'sess-p5',
      text: 'Scheduled check-in',
      delayMs: 25,
    });
    assert.ok(job.id);
    await new Promise(resolve => setTimeout(resolve, 120));
    assert.ok(ownerCalls.some(c => c.method === 'host.session.prompt' && c.params.text === 'Scheduled check-in'));

    // 6. tokenMeter (manual record + automatic daemon committed usage sync with dedup)
    root.tokenMeter.record('sess-p5', { inputTokens: 120, outputTokens: 30, cacheReadTokens: 50 });
    services.observeSessionCommitted({
      sessionId: 'sess-p5',
      eventSeq: 42,
      observation: {
        kind: 'assistantMessage',
        usage: { inputTokens: 80, outputTokens: 20, cacheReadTokens: 10, cacheWriteTokens: 5 },
      },
    });
    // Duplicate eventSeq must be deduplicated
    services.observeSessionCommitted({
      sessionId: 'sess-p5',
      eventSeq: 42,
      observation: {
        kind: 'assistantMessage',
        usage: { inputTokens: 999, outputTokens: 999 },
      },
    });
    assert.deepEqual(root.tokenMeter.summary('sess-p5'), {
      inputTokens: 200,
      outputTokens: 50,
      cacheReadTokens: 60,
      cacheWriteTokens: 5,
      requests: 2,
    });
    assert.equal(root.tokenMeter.total().requests, 2);

    // 7. deepseekLlmApiExtensions, llm/stream intercept (replace + retry), & session.projectMessages
    const offExt = root.deepseekLlmApiExtensions.register(req => ({ ...req, extraHeader: 'x-dsh' }));
    const extended = await services.extendLlmRequest({ sessionId: 'sess-p5', request: { model: 'deepseek-chat' } });
    assert.equal(extended.request.extraHeader, 'x-dsh');
    offExt();

    const offStreamReplace = root.on('llm/stream', async function* (_opts, next) {
      for await (const chunk of next()) {
        if (chunk.type === 'text-delta') yield { ...chunk, text: chunk.text + '|stream-wrapped' };
        else yield chunk;
      }
    });
    assert.equal(services.hookCapabilities().llmStreamIntercept, true);
    const replaced = await services.interceptLlmStream({
      sessionId: 'sess-p5',
      provider: 'openai',
      model: 'mock',
      content: 'hello',
    });
    assert.deepEqual(replaced, { action: 'replace', content: 'hello|stream-wrapped' });
    offStreamReplace();

    const offStreamRetry = root.on('llm/stream', async function* (opts, next) {
      let text = '';
      for await (const chunk of next()) {
        if (chunk.type === 'text-delta') text += chunk.text;
      }
      if (text.includes('REFUSED')) {
        opts.appendUserMessage = 'Please answer directly';
        yield { type: 'retry', reason: 'refusal_detected', appendUserMessage: 'Please answer directly' };
        return;
      }
      yield { type: 'text-delta', index: 0, text };
      yield { type: 'finish', reason: { kind: 'stop' } };
    });
    const retried = await services.interceptLlmStream({
      sessionId: 'sess-p5',
      provider: 'openai',
      model: 'mock',
      content: 'REFUSED by filter',
    });
    assert.equal(retried.action, 'retry');
    assert.equal(retried.reason, 'refusal_detected');
    assert.equal(retried.appendUserMessage, 'Please answer directly');
    offStreamRetry();

    const offProj = root.sessions.registerMessageProjection(msgs => [...msgs, { role: 'user', content: 'projected' }]);
    const projected = await services.projectMessages({ sessionId: 'sess-p5', messages: [{ role: 'user', content: 'hi' }] });
    assert.equal(projected.messages.length, 2);
    assert.equal(projected.messages[1].content, 'projected');
    offProj();
  } finally {
    await env.close();
  }
});
