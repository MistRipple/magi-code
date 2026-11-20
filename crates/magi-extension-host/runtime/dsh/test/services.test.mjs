import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import WebSocket from 'ws';
import { credentialKey } from '@deepseek-ai/dsh-credentials';
import { LlmAdapter, ToolCallId } from '@deepseek-ai/dsh-llm';
import { defineTool } from '@deepseek-ai/dsh-tools';
import z from '@deepseek-ai/schemastery';
import { CommittedTree } from '../committed-tree.mjs';
import { prepareConfiguration } from '../configuration.mjs';
import { prepareConfigurationEdit } from '../configuration-edit.mjs';
import { PluginLifecycle } from '../lifecycle.mjs';
import { openPluginManager } from '../plugin-manager.mjs';
import { openHostServices } from '../services.mjs';
import { createSessionReader } from '../session-reader.mjs';
import { openWebRoutes } from '../web-routes.mjs';

const sha256 = text => createHash('sha256').update(text).digest('hex');

// Minimal 1x1 transparent PNG
const TINY_PNG = new Uint8Array(
  Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
    'base64',
  ),
);

test('storage services enforce durable credential semantics and raster/verbatim attachment verification', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'magi-dsh-storage-'));
  const host = new PluginLifecycle({
    hostGeneration: 1,
    onStatus: () => {},
    onDiagnostic: () => {},
  });
  const tree = new CommittedTree({
    lifecycle: host,
    hostGeneration: 1,
    onDiagnostic: () => {},
    requestOwner: async () => ({}),
  });
  let services;
  try {
    services = await openHostServices({
      lifecycle: host,
      tree,
      dshHome: join(dir, 'dsh-home'),
      requestOwner: async () => ({}),
      notifyOwner: () => {},
    });
    const root = host.context;
    assert.equal(root.get('profileContext'), undefined);
    assert.equal(root.get('hmr'), undefined);
    assert.equal(process.env.DSH_HOME, join(dir, 'dsh-home'));

    // Credentials ref & record semantics
    assert.deepEqual(await root.credentials.describe('MY_API_KEY'), {
      configured: false,
      writable: true,
    });
    await root.credentials.set('MY_API_KEY', 'sk-secret-value-123456');
    const desc = await root.credentials.describe('MY_API_KEY');
    assert.equal(desc.configured, true);
    assert.equal(desc.source, 'file');
    assert.equal(desc.masked, 'sk-••••456');
    const resolved = await root.credentials.resolve('MY_API_KEY');
    assert.deepEqual(resolved, { value: 'sk-secret-value-123456', source: 'file' });
    await root.credentials.unset('MY_API_KEY');
    assert.equal(await root.credentials.resolve('MY_API_KEY'), undefined);

    const recKey = credentialKey('test-plugin', 'oauth');
    const created = await root.credentials.modifyRecord(recKey, async current => {
      assert.equal(current, undefined);
      return { kind: 'grant', payload: { token: 'abc' } };
    });
    assert.deepEqual(created, { kind: 'grant', payload: { token: 'abc' } });
    // Returning undefined leaves stored record intact and returns it
    const preserved = await root.credentials.modifyRecord(recKey, async current => {
      assert.deepEqual(current, { kind: 'grant', payload: { token: 'abc' } });
      return undefined;
    });
    assert.deepEqual(preserved, { kind: 'grant', payload: { token: 'abc' } });

    // Attachments: image validation + readImageRequest + verbatim file stream
    const [imgRef] = await root.attachments.saveImages([
      { data: TINY_PNG, mediaType: 'image/png', name: '../pixel.png' },
    ]);
    assert.equal(imgRef.width, 1);
    assert.equal(imgRef.height, 1);
    assert.equal(imgRef.name, 'pixel.png');
    const imgReq = await root.attachments.readImageRequest(imgRef, {});
    assert.equal(imgReq.bytes, TINY_PNG.byteLength);
    assert.equal(imgReq.hasAlpha, true);

    await assert.rejects(
      () => root.attachments.saveImages([{ data: TINY_PNG, mediaType: 'image/jpeg' }]),
      err => err.code === 'IMAGE_TYPE_MISMATCH',
    );

    const fileBytes = Buffer.from('hello verbatim stream payload', 'utf8');
    const fileRef = await root.attachments.saveFileStream({
      name: 'nested/report.txt',
      data: (async function* () {
        yield fileBytes.subarray(0, 10);
        yield fileBytes.subarray(10);
      })(),
    });
    assert.equal(fileRef.name, 'report.txt');
    assert.equal(fileRef.bytes, fileBytes.byteLength);
    const chunks = [];
    for await (const chunk of root.attachments.readFileStream(fileRef)) {
      chunks.push(Buffer.from(chunk));
    }
    assert.equal(Buffer.concat(chunks).toString('utf8'), 'hello verbatim stream payload');
  } finally {
    await services?.close();
    await tree.close();
    await host.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test('host services wire tools, web volatile settings, commands, skills, bidirectional llm, and Typert gateway', async () => {
  const dir = await realpath(await mkdtemp(join(tmpdir(), 'magi-dsh-services-')));
  const runtimeRoot = dirname(dirname(fileURLToPath(import.meta.url)));
  const notifications = [];
  const ownerCalls = [];
  let hostState = {
    revision: 0,
    sources: [],
    packages: [],
    projectDir: '',
    sourceDir: '',
    graph: null,
  };

  const host = new PluginLifecycle({
    hostGeneration: 1,
    onStatus: () => {},
    onDiagnostic: () => {},
  });
  let tree;

  const sessionDirectory = new Map([
    ['s1', { sessionId: 's1', status: 'Active', createdAt: 5, updatedAt: 10, messageCount: 2, isRunning: false, cwd: '/work/p' }],
  ]);
  const requestOwner = async (method, params) => {
    ownerCalls.push({ method, params });
    if (method === 'host.session.queue') {
      if (params.queueId === 'gone') throw new Error('queue item not found');
      return { accepted: true };
    }
    if (method === 'host.session.prompt') {
      if (!sessionDirectory.has(params.sessionId)) throw new Error('session not found');
      return { accepted: true };
    }
    if (method === 'host.session.create') {
      if (params.workspaceId === 'missing') throw new Error('workspace not found');
      sessionDirectory.set('s-new', { sessionId: 's-new', status: 'Active', updatedAt: 30, messageCount: 0, isRunning: false });
      return { sessionId: 's-new' };
    }
    if (method === 'host.session.cancel') {
      if (!sessionDirectory.has(params.sessionId)) throw new Error('session not found');
      return { accepted: true };
    }
    if (method === 'host.session.rename') {
      if (!sessionDirectory.has(params.sessionId)) throw new Error('session not found');
      if (params.title.trim() === '') throw new Error('会话名称不能为空');
      return { title: params.title.trim(), seq: 7 };
    }
    if (method === 'host.session.history') {
      const fact = sessionDirectory.get(params.sessionId);
      if (!fact) throw new Error('session not found');
      const observations = [
        { kind: 'turnStarted', turn: 1 },
        { kind: 'textInput', messageId: 'u1', text: '你好' },
        { kind: 'turnEnded', turn: 1, outcome: { kind: 'completed' } },
      ];
      return {
        session: { id: fact.sessionId, createdAt: fact.createdAt, cwd: fact.cwd },
        through: 30, nextAfter: null,
        // Like the daemon, only events after the cursor are returned.
        events: observations
          .map((observation, seq) => ({ seq, sourceSequence: 10 + seq, occurredAt: 1000 + seq, observation }))
          .filter(item => item.sourceSequence > (params.after ?? 0)),
      };
    }
    if (method === 'host.session.list') return { factsSequence: 0, sessions: [...sessionDirectory.values()] };
    if (method === 'host.session.get') return { factsSequence: 1, session: sessionDirectory.get(params.sessionId) ?? null };
    if (method === 'host.llm.invoke') {
      return {
        content: `host-echo:${params.provider}:${params.model}:${params.messages.at(-1)?.content ?? ''}`,
        reasoning: 'thought-step',
        usage: { inputTokens: 11, outputTokens: 7 },
      };
    }
    if (method === 'loading.profilePatch.edit') {
      const proposal = await prepareConfigurationEdit({
        projectDirectory: hostState.projectDir,
        sourceDirectory: hostState.sourceDir,
        sources: hostState.sources,
        base: 'cordis.yml',
        overlays: ['bundle.patch.yml', 'cordis.patch.yml'],
        packages: hostState.packages,
        previous: hostState.graph,
        operation: {
          kind: 'profilePatchConfig',
          id: params.id,
          name: params.name,
          nextConfig: params.nextConfig,
          clearOverride: params.clearOverride,
        },
      });
      const editedByName = new Map(proposal.edits.map(e => [e.name, e.content]));
      hostState.sources = hostState.sources.map(src => editedByName.has(src.name)
        ? { name: src.name, content: editedByName.get(src.name), sha256: sha256(editedByName.get(src.name)) }
        : src);
      hostState.revision += 1;
      hostState.graph = proposal.plan.graph;
      const configuration = {
        revision: hostState.revision,
        sources: hostState.sources,
        sourcesSha256: proposal.plan.sourcesSha256,
        sourceRoot: hostState.sourceDir,
        preparation: proposal.plan.preparation,
        graph: proposal.plan.graph,
        executables: proposal.plan.executables,
      };
      const activations = proposal.plan.executables.map((exe, idx) => ({
        executable: exe,
        hostGeneration: 1,
        activationGeneration: 1,
        activationOrder: idx + 1,
      }));
      await tree.apply({
        token: { hostGeneration: 1, sequence: hostState.revision, revision: hostState.revision },
        configuration,
        activations,
      });
      return { revision: hostState.revision };
    }
    if (method.startsWith('pluginManager.')) {
      return { bundles: [], plugins: [], registries: [] };
    }
    return {};
  };

  tree = new CommittedTree({
    lifecycle: host,
    hostGeneration: 1,
    onDiagnostic: () => {},
    requestOwner,
  });

  const proof = 'a'.repeat(64);
  let webRoutes;
  let pluginMgr;
  let services;
  try {
    const publicOrigin = 'http://127.0.0.1:39999';
    webRoutes = await openWebRoutes(host, proof, publicOrigin);
    pluginMgr = await openPluginManager({
      lifecycle: host,
      tree,
      hostGeneration: 1,
      requestOwner,
    });
    services = await openHostServices({
      lifecycle: host,
      tree,
      dshHome: join(dir, 'dsh-home'),
      initialHostConfig: {
        agentDefaultModel: { provider: 'custom/local', model: 'qwen-coder' },
        hostModelSources: [
          {
            provider: 'custom/local',
            name: 'Local Custom',
            models: [{ id: 'qwen-coder', displayName: 'Qwen Coder', contextWindowTokens: 65536 }],
          },
        ],
      },
      requestOwner,
      sessionReader: createSessionReader(requestOwner),
      notifyOwner: (method, payload) => notifications.push({ method, payload }),
    });
    // Prepare a real committed plugin package in the tree so configEditor / settings / tools / web / llm all run under real plugin ownership!
    const projectDir = join(dir, 'project');
    const sourceDir = join(dir, 'sources');
    const pkgDir = join(projectDir, 'node_modules', 'sample-suite');
    await mkdir(pkgDir, { recursive: true });
    await mkdir(sourceDir, { recursive: true });
    await writeFile(join(projectDir, 'package.json'), '{"private":true}');
    await symlink(join(runtimeRoot, 'node_modules', '@deepseek-ai'), join(projectDir, 'node_modules', '@deepseek-ai'));
    const manifestText = JSON.stringify({
      name: 'sample-suite',
      version: '1.0.0',
      type: 'module',
      exports: { '.': './index.mjs' },
      dsh: { meta: { title: 'Sample Suite', description: 'End-to-end sample' } },
    });
    await writeFile(join(pkgDir, 'package.json'), manifestText);
    await writeFile(join(pkgDir, 'index.mjs'), `
import z from '@deepseek-ai/schemastery';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
import { defineTool } from '@deepseek-ai/dsh-tools';

export const inject = ['tools', 'web', 'commands', 'skills', 'llm', 'settings'];
export const Config = z.object({
  greeting: z.string().default('hello').volatile(),
  apiKey: z.string().role('secret').volatile(),
});

export function apply(ctx, config) {
  ctx.tools.register(defineTool({
    name: 'sample_greet',
    description: 'Greet someone',
    parameters: {
      who: { type: 'string', required: true, description: 'Target name' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: { text: { type: 'string', required: true } },
      },
      render(_args, val) {
        return [{ type: 'text', text: val.text }];
      },
    },
    async execute(args) {
      return { text: config.greeting.get() + ', ' + args.who + '!' };
    },
  }));

  ctx.web.registerSearchProvider({
    id: 'sample-search',
    title: 'Sample Search',
    available: () => true,
    async search(req) {
      return {
        sources: [{ title: 'Hit for ' + req.query, url: 'https://example.com/hit', snippet: 'ok' }],
      };
    },
  });
  ctx.web.registerFetchProvider({
    id: 'sample-fetch',
    title: 'Sample Fetch',
    available: () => true,
    async fetch(req) {
      return { status: 200, url: req.url, finalUrl: req.url, contentType: 'text/markdown', content: '# ' + req.url };
    },
  });

  ctx.commands.register({
    name: 'ping',
    description: 'Ping command',
    async handler(inv) {
      return { kind: 'success', text: 'pong:' + inv.rawInput.trim() };
    },
  });

  ctx.skills.register({
    name: 'sample-skill',
    description: 'Sample runtime skill',
    source: 'sample-suite',
    content: 'Follow sample steps.',
  });

  class SampleAdapter extends LlmAdapter {
    providerInfo() {
      return { id: 'sample-llm', name: 'Sample LLM' };
    }
    async listModels() {
      return [{ provider: 'sample-llm', id: 's1', name: 'Sample Model 1', inputModalities: ['text', 'image'] }];
    }
    async resolveModel(_p, model) {
      return {
        provider: 'sample-llm',
        id: model,
        name: 'Sample Model 1',
        context: { contextWindow: 128000 },
        inputModalities: ['text', 'image'],
      };
    }
    async *stream(opts) {
      const userText = opts.messages.at(-1)?.content.find(b => b.type === 'text')?.text ?? '';
      yield { type: 'block-start', index: 0, blockType: 'text' };
      yield { type: 'text-delta', index: 0, text: 'plugin-reply:' + userText };
      yield { type: 'block-end', index: 0, block: { type: 'text', text: 'plugin-reply:' + userText } };
      yield { type: 'usage', usage: { inputTokens: 5, outputTokens: 4 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    }
  }
  const disposeAdapter = ctx.llm.registerAdapter(['sample-llm'], new SampleAdapter());
  ctx.effect(() => disposeAdapter);

  void ctx.settings.mutate('web', [
    { op: 'set', path: ['searchProvider'], value: 'sample-search' },
    { op: 'set', path: ['fetchProvider'], value: 'sample-fetch' },
  ]);
}
`);

    const cordisYml = '[]\n';
    const bundlePatchYml = '- insert:\n    - id: sample\n      name: sample-suite\n';
    const userPatchYml = '[]\n';
    await writeFile(join(sourceDir, 'cordis.yml'), cordisYml);
    await writeFile(join(sourceDir, 'bundle.patch.yml'), bundlePatchYml);
    await writeFile(join(sourceDir, 'cordis.patch.yml'), userPatchYml);

    const sources = [
      { name: 'cordis.yml', content: cordisYml, sha256: sha256(cordisYml) },
      { name: 'bundle.patch.yml', content: bundlePatchYml, sha256: sha256(bundlePatchYml) },
      { name: 'cordis.patch.yml', content: userPatchYml, sha256: sha256(userPatchYml) },
    ];
    const packages = [{
      projectDirectory: projectDir,
      directory: pkgDir,
      name: 'sample-suite',
      version: '1.0.0',
      manifestSha256: sha256(manifestText),
    }];
    const plan = await prepareConfiguration({
      projectDirectory: projectDir,
      sourceDirectory: sourceDir,
      sources,
      base: 'cordis.yml',
      overlays: ['bundle.patch.yml', 'cordis.patch.yml'],
      packages,
    });
    hostState = {
      revision: 1,
      sources,
      packages,
      projectDir,
      sourceDir,
      graph: plan.graph,
    };
    await tree.apply({
      token: { hostGeneration: 1, sequence: 1, revision: 1 },
      configuration: {
        revision: 1,
        sources,
        sourcesSha256: plan.sourcesSha256,
        sourceRoot: sourceDir,
        preparation: plan.preparation,
        graph: plan.graph,
        executables: plan.executables,
      },
      activations: plan.executables.map((exe, idx) => ({
        executable: exe,
        hostGeneration: 1,
        activationGeneration: 1,
        activationOrder: idx + 1,
      })),
    });
    await host.settled();
    // Allow queued microtasks (llm publish & volatile web mutation) to settle
    await new Promise(resolve => setTimeout(resolve, 30));

    // 1. Tool execution
    const toolRes = await services.executeTool({ name: 'sample_greet', args: { who: 'Magi' } });
    assert.equal(toolRes.isError, false);
    assert.deepEqual(toolRes.content, [{ type: 'text', text: 'hello, Magi!' }]);

    // 2. Web search & fetch after plugin mutated 'web' settings live without restarting WebRuntime!
    assert.equal(host.context.web.config.searchProvider.get(), 'sample-search');
    assert.equal(host.context.web.config.fetchProvider.get(), 'sample-fetch');
    const searchRes = await services.webSearch({ query: 'cordis' });
    assert.equal(searchRes.sources[0].title, 'Hit for cordis');
    const fetchRes = await services.webFetch({ url: 'https://example.com/doc' });
    assert.equal(fetchRes.content, '# https://example.com/doc');

    // 3. Commands & Skills
    const cmds = host.context.commands.list();
    assert.equal(cmds.some(c => c.name === 'ping'), true);
    assert.ok(host.context.get('agents'));
    assert.equal(host.context.agents.get('s1')?.id, 's1');
    const sessionEvents = [];
    const stopObserving = host.context.on('session/event', (...args) => sessionEvents.push(args));
    const cmdOut = await services.executeCommand({
      sessionId: 'sess-1', line: '/ping world',
      expectedOwner: { pluginId: 'sample-suite', name: 'ping' },
    });
    assert.deepEqual(cmdOut.result, { kind: 'success', text: 'pong:world' });
    assert.deepEqual(sessionEvents, []);
    stopObserving();
    await assert.rejects(host.context.commands.execute({ id: 'unknown' }, '/ping', [], new AbortController().signal),
      /through a Magi session/);
    await assert.rejects(services.executeCommand({
      sessionId: 'sess-1', line: '/ping world',
      expectedOwner: { pluginId: 'retired-package', name: 'ping' },
    }), /no longer registered by its owner/);

    const skills = await services.listSkills();
    assert.equal(skills.some(s => s.name === 'sample-skill'), true);
    const skillDef = await services.getSkill({ name: 'sample-skill' });
    assert.equal(skillDef.content, 'Follow sample steps.');

    // 4. Bidirectional LLM: Plugin -> Magi OpenAI bridge (streaming SSE & non-streaming JSON)
    const llmNotice = notifications.findLast(n => n.method === 'host.llm.providersChanged');
    assert.ok(llmNotice, 'expected host.llm.providersChanged notification');
    const sampleProv = llmNotice.payload.providers.find(p => p.provider === 'sample-llm');
    assert.ok(sampleProv);
    assert.equal(sampleProv.pluginId, 'sample-suite');
    assert.equal(sampleProv.models[0].id, 's1');

    const jsonChatResp = await fetch(`${sampleProv.baseUrl}/chat/completions`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${sampleProv.apiKey}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({
        model: 's1',
        stream: false,
        messages: [{ role: 'user', content: 'hi-bridge' }],
      }),
    });
    assert.equal(jsonChatResp.status, 200);
    const jsonChatBody = await jsonChatResp.json();
    assert.equal(jsonChatBody.choices[0].message.content, 'plugin-reply:hi-bridge');

    const sseChatResp = await fetch(`${sampleProv.baseUrl}/chat/completions`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${sampleProv.apiKey}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({
        model: 's1',
        stream: true,
        messages: [{ role: 'user', content: 'hi-stream' }],
      }),
    });
    assert.equal(sseChatResp.status, 200);
    const sseText = await sseChatResp.text();
    assert.match(sseText, /plugin-reply:hi-stream/);
    assert.match(sseText, /\[DONE\]/);

    // Magi -> Plugin LLM invocation (`ctx.llm.prepareCall` on host route)
    const selection = host.context.agentDefaultModel.currentSelection();
    assert.deepEqual(selection, { provider: 'custom/local', model: 'qwen-coder' });
    const hostPrepared = await host.context.llm.prepareCall(selection);
    const chunks = [];
    for await (const item of hostPrepared.stream({
      ...hostPrepared.config,
      messages: [{
        id: 'm1',
        role: 'user',
        content: [{ type: 'text', text: 'diagnose-this' }],
        source: { kind: 'user' },
      }],
    })) {
      if (item.type === 'text-delta') chunks.push(item.text);
    }
    assert.equal(chunks.join(''), 'host-echo:custom/local:qwen-coder:diagnose-this');

    // 5. TypertGateway HTTP RPC + cross-site fence + live profilePatchConfig hot update!
    const hostHeaders = {
      host: '127.0.0.1:39999',
      origin: publicOrigin,
      'content-type': 'application/json',
      'x-magi-route-proof': proof,
      'x-magi-route-instance': Buffer.from('$host').toString('base64url'),
      'x-magi-route-generation': '1',
      'x-magi-route-epoch': '1',
    };

    const sendGatewayPost = (pathname, headers, bodyObj) => new Promise((resolve, reject) => {
      const payload = Buffer.from(JSON.stringify(bodyObj), 'utf8');
      const req = http.request({
        hostname: '127.0.0.1',
        port: webRoutes.port,
        path: pathname,
        method: 'POST',
        headers: { ...headers, 'content-length': String(payload.byteLength) },
      }, res => {
        const chunks = [];
        res.on('data', chunk => chunks.push(chunk));
        res.on('end', () => resolve({
          status: res.statusCode,
          text: Buffer.concat(chunks).toString('utf8'),
        }));
      });
      req.once('error', reject);
      req.end(payload);
    });

    // Cross-site Origin must be rejected with 403 by HostConnectionService
    const crossSite = await sendGatewayPost(
      '/api/settings/describe',
      { ...hostHeaders, origin: 'https://evil.example' },
      { type: 'client-request', rpcId: 'r0', method: 'settings/describe', payload: { args: {} } },
    );
    assert.equal(crossSite.status, 403);

    const callRpc = async (endpoint, args) => {
      const resp = await sendGatewayPost(
        `/api/${endpoint}`,
        hostHeaders,
        { type: 'client-request', rpcId: 'r1', method: endpoint, payload: { args } },
      );
      assert.equal(resp.status, 200);
      return JSON.parse(resp.text);
    };

    const inventory = await callRpc('pluginInventory/list', {});
    assert.equal(inventory.result.ok, true);
    assert.equal(inventory.result.value.managementAvailable, true);
    assert.equal(inventory.result.value.entries[0].moduleName, 'sample-suite');
    assert.equal(inventory.result.value.entries[0].fiberPhase, 'active');

    // The session namespace is answered from the daemon's directory facts through the real gateway.
    const listed = await callRpc('session/list', {});
    assert.equal(listed.result.ok, true);
    assert.deepEqual(listed.result.value.items, [{
      agentAvailable: true, formatStatus: 'current', sessionId: 's1', updatedAt: 10, running: false, blank: false, cwd: '/work/p',
    }]);
    const refused = await callRpc('session/list', { _request: { cursor: 'next' } });
    assert.equal(refused.result.ok, false);
    assert.match(refused.result.error.message, /no cursor/);

    const created = await callRpc('session/create', { request: { workspaceId: 'w1' } });
    assert.deepEqual(created.result, { ok: true, value: { sessionId: 's-new' } });
    for (const request of [{ cwd: '/tmp' }, { sessionId: 'mine' }, { agentPreset: 'x' }, { workspaceId: 'missing' }]) {
      assert.equal((await callRpc('session/create', { request })).result.ok, false, JSON.stringify(request));
    }
    sessionDirectory.delete('s-new');
    const promptRequest = content => ({ request: { requestId: 'r1', sessionId: 's1', mode: 'queue', content } });
    const prompted = await callRpc('session/prompt', promptRequest([{ type: 'text', text: '你好' }, { type: 'text', text: '再见' }]));
    assert.deepEqual(prompted.result, { ok: true, value: { accepted: true } });
    const forwarded = ownerCalls.filter(call => call.method === 'host.session.prompt').at(-1).params;
    assert.deepEqual(forwarded, { sessionId: 's1', requestId: 'r1', steer: false, text: '你好\n\n再见', images: [] });
    await callRpc('session/prompt', { request: { requestId: 'r2', sessionId: 's1', mode: 'steer', content: [
      { type: 'text', text: '看图' }, { type: 'image', mediaType: 'image/png', data: 'AAAA', name: 'a.png' },
    ] } });
    const steered = ownerCalls.filter(call => call.method === 'host.session.prompt').at(-1).params;
    assert.deepEqual(steered.images, [{ mediaType: 'image/png', data: 'AAAA', name: 'a.png' }]);
    assert.equal(steered.steer, true);
    for (const content of [[], [{ type: 'file', receiptId: 'x' }], [{ type: 'text', text: '  ' }]]) {
      assert.equal((await callRpc('session/prompt', promptRequest(content))).result.ok, false, JSON.stringify(content));
    }
    assert.equal((await callRpc('session/prompt', { request: { requestId: 'r', sessionId: 'nope', mode: 'queue', content: [{ type: 'text', text: 'x' }] } })).result.ok, false);
    const queueRequest = action => ({ request: { sessionId: 's1', itemId: 'q1', action } });
    for (const action of [{ kind: 'remove' }, { kind: 'steer' }, { kind: 'edit', content: [{ type: 'text', text: '新文本' }] }]) {
      assert.deepEqual((await callRpc('session/updateQueue', queueRequest(action))).result, { ok: true, value: { accepted: true } });
    }
    const queued = ownerCalls.filter(call => call.method === 'host.session.queue').map(call => call.params);
    assert.deepEqual(queued.at(-1), { sessionId: 's1', queueId: 'q1', action: { kind: 'edit', text: '新文本' } });
    for (const action of [{ kind: 'edit', content: [] }, { kind: 'edit', content: [{ type: 'text', text: ' ' }] }, { kind: 'reorder' }]) {
      assert.equal((await callRpc('session/updateQueue', queueRequest(action))).result.ok, false, JSON.stringify(action));
    }
    assert.equal((await callRpc('session/updateQueue', { request: { sessionId: 's1', itemId: 'gone', action: { kind: 'remove' } } })).result.ok, false);
    const cancelled = await callRpc('session/cancel', { request: { sessionId: 's1' } });
    assert.deepEqual(cancelled.result, { ok: true, value: { accepted: true } });
    const unknown = await callRpc('session/cancel', { request: { sessionId: 'nope' } });
    assert.equal(unknown.result.ok, false);
    assert.equal((await callRpc('session/cancel', { request: {} })).result.ok, false);
    // history: a page of the committed log and the exact empty projection baseline, through the real gateway.
    const history = await callRpc('session/page', { request: { address: { kind: 'session', sessionId: 's1' }, maxMessages: 5 } });
    assert.equal(history.result.ok, true, JSON.stringify(history.result));
    assert.equal(history.result.value.cursor, 2);
    assert.equal(history.result.value.hasMore, false);
    assert.deepEqual(history.result.value.records.map(record => [record.event.seq, record.event.type]),
      [[0, 'turn/start'], [1, 'user/message'], [2, 'turn/end']]);
    const projected = await callRpc('session/projections', { request: { sessionId: 's1' } });
    assert.deepEqual(projected.result.value,
      {
        kind: 'sequenced',
        asOfSeq: 2,
        values: {
          agentPreset: null,
          title: null,
          subagentCatalog: [],
          userQuestions: { active: [], settled: [] },
        },
      });
    assert.equal((await callRpc('session/page', { request: { address: { kind: 'session', sessionId: 'nope' } } })).result.ok, false);
    assert.equal((await callRpc('session/page', { request: { address: { kind: 'session', sessionId: 's1' }, maxMessages: 0 } })).result.ok, false);

    // Official AgentPresetRegistry (`ctx.agentPresets`) registration, inventory & cleanup
    assert.ok(host.context.get('agentPresets'), 'ctx.agentPresets should be mounted');
    const disposePreset = await host.context.agentPresets.register({
      id: 'research-preset',
      name: 'Research Preset',
      description: 'Focused research preset',
      plugins: [],
    });
    const invWithPreset = await callRpc('pluginInventory/list', {});
    assert.equal(invWithPreset.result.ok, true);
    assert.ok(
      invWithPreset.result.value.agentPresets.some(p => p.id === 'research-preset' && p.name === 'Research Preset'),
      'pluginInventory/list should expose registered agentPresets',
    );
    await disposePreset();
    const invAfterPreset = await callRpc('pluginInventory/list', {});
    assert.equal(
      invAfterPreset.result.value.agentPresets.some(p => p.id === 'research-preset'),
      false,
      'disposing preset should remove it from inventory',
    );

    // session/attachment: reads durable images from the shared host attachment store and maps AttachmentError.
    const tinyPngBase64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+X2dIAAAAASUVORK5CYII=';
    const [admittedImg] = await host.context.attachments.saveImages([{
      mediaType: 'image/png',
      data: Buffer.from(tinyPngBase64, 'base64'),
      name: 'dot.png',
    }]);
    const fetchedImg = await callRpc('session/attachment', {
      request: { sessionId: 's1', attachment: admittedImg },
    });
    assert.equal(fetchedImg.result.ok, true, JSON.stringify(fetchedImg.result));
    assert.equal(fetchedImg.result.value.data, tinyPngBase64);
    assert.equal(fetchedImg.result.value.attachment.attachmentId, admittedImg.attachmentId);
    const missingImg = await callRpc('session/attachment', {
      request: {
        sessionId: 's1',
        attachment: { ...admittedImg, attachmentId: `sha256-${'0'.repeat(64)}` },
      },
    });
    assert.equal(missingImg.result.ok, false);
    assert.equal(missingImg.result.error.code, 'session/attachment-invalid');

    // Backend `ctx.sessions`, read-only `ctx.sessionPersistence`, and official `ctx.sessionQuery`.
    const unloadedHandle = host.context.sessions.get('s1');
    assert.ok(unloadedHandle);
    assert.equal(unloadedHandle.header.id, 's1');
    assert.equal(unloadedHandle.seq, 3); // already warmed by session/page above
    const queryList = await host.context.sessionQuery.listSessions();
    assert.equal(queryList.some(item => item.header.id === 's1' && item.live && item.persisted), true);
    const queryLog = await host.context.sessionQuery.readSession('s1');
    assert.equal(queryLog.events.length, 3);
    const surface = await host.context.sessionQuery.readSurface('s1');
    assert.equal(surface.events.length, 1);
    assert.equal(surface.events[0].type, 'user/message');
    const inspectedSeq = await host.context.sessionQuery.inspectSession('s1', s => s.seq);
    assert.equal(inspectedSeq, 3);
    await assert.rejects(
      host.context.sessionQuery.searchSessions({ query: 'hello' }),
      err => err?.code === 'SESSION_QUERY_SEARCH_DISABLED',
    );
    await assert.rejects(
      host.context.sessionPersistence.open('s1', 'write'),
      /read-only/i,
    );

    // Official storage + storageDomain (durable JSON backend), sessionProjections, workspaceRegistry & fileUploads.
    const { defineDomain, domainTable } = await import('@deepseek-ai/dsh-storage-domain');
    const { z } = await import('zod');
    const notesDomain = await host.context.storageDomain.open(defineDomain({
      name: 'test_notes',
      version: 1,
      tables: {
        items: domainTable(z.object({ text: z.string() })),
      },
    }));
    await notesDomain.table('items').put('k1', { text: 'persisted note' });
    assert.deepEqual(notesDomain.table('items').get('k1'), { text: 'persisted note' });
    await notesDomain.close();
    const reopenedNotes = await host.context.storageDomain.open(defineDomain({
      name: 'test_notes',
      version: 1,
      tables: {
        items: domainTable(z.object({ text: z.string() })),
      },
    }));
    assert.deepEqual(reopenedNotes.table('items').get('k1'), { text: 'persisted note' });
    await reopenedNotes.close();

    // Custom plugin projection via official ctx.sessionProjections -> visible through session/projections.
    const projHandle = await host.context.sessionProjections.register({
      key: 'customCounter',
      stateSchema: z.number(),
      stateVersion: 1,
      init: () => 0,
      apply: (count, event) => (event.type === 'user/message' ? count + 1 : count),
      wire: {
        viewSchema: z.number(),
        view: count => count,
      },
    });
    const projWithCustom = await callRpc('session/projections', { request: { sessionId: 's1' } });
    assert.equal(projWithCustom.result.ok, true, JSON.stringify(projWithCustom.result));
    assert.equal(projWithCustom.result.value.values.customCounter, 1);
    await projHandle.dispose();

    // WorkspaceRegistry is mounted and backed by storageDomain + sessionPersistence.
    assert.ok(host.context.get('workspaceRegistry'));
    assert.equal(Array.isArray(host.context.workspaceRegistry.list()), true);

    // Official FileUploads -> upload receipt -> session/prompt with `{ type: 'file', receiptId }`.
    const uploadedFile = await callRpc('fileUploads/upload', {
      agentId: 's1',
      request: { data: Buffer.from('staged payload', 'utf8').toString('base64'), name: 'notes.txt' },
    });
    assert.equal(uploadedFile.result.ok, true, JSON.stringify(uploadedFile.result));
    const receiptId = uploadedFile.result.value.receiptId;
    assert.ok(receiptId);
    const promptWithFile = await callRpc('session/prompt', {
      request: {
        sessionId: 's1',
        requestId: 'req-with-file',
        mode: 'queue',
        content: [
          { type: 'text', text: 'check attachment' },
          { type: 'file', receiptId },
        ],
      },
    });
    assert.equal(promptWithFile.result.ok, true, JSON.stringify(promptWithFile.result));
    const lastPromptCall = ownerCalls.filter(c => c.method === 'host.session.prompt').at(-1);
    assert.equal(lastPromptCall.params.text.includes('notes.txt'), true, JSON.stringify(lastPromptCall.params));
    const badFilePrompt = await callRpc('session/prompt', {
      request: {
        sessionId: 's1',
        requestId: 'req-bad-file',
        mode: 'queue',
        content: [{ type: 'file', receiptId: 'missing-receipt' }],
      },
    });
    assert.equal(badFilePrompt.result.ok, false);
    assert.equal(badFilePrompt.result.error.code, 'session/attachment-invalid');
    // rename: the accepted title and the durable sequence of its log event come from the daemon unchanged.
    const renamed = await callRpc('session/rename', { request: { sessionId: 's1', title: ' 新标题 ' } });
    assert.deepEqual(renamed.result, { ok: true, value: { title: '新标题', seq: 7 } });
    assert.deepEqual(ownerCalls.filter(call => call.method === 'host.session.rename').at(-1).params, { sessionId: 's1', title: ' 新标题 ' });
    assert.equal((await callRpc('session/rename', { request: { sessionId: 'nope', title: 'x' } })).result.ok, false);
    assert.equal((await callRpc('session/rename', { request: { sessionId: 's1', title: '  ' } })).result.ok, false);
    assert.equal((await callRpc('session/rename', { request: { sessionId: 's1' } })).result.ok, false);

    // commands: real Typert lookup('agent') + persisted command/run and command/done lifecycle events.
    const cmdList = await callRpc('commands/list', { agentId: 's1' });
    assert.equal(cmdList.result.ok, true, JSON.stringify(cmdList.result));
    assert.equal(cmdList.result.value.some(c => c.name === 'ping'), true);
    assert.equal((await callRpc('commands/list', { agentId: 'nope' })).result.ok, false);

    const beforeCommandEvents = ownerCalls.filter(call => call.method === 'host.session.appendCommandEvent').length;
    const cmdExec = await callRpc('commands/execute', { agentId: 's1', line: '/ping gateway', submittedAttachments: [] });
    assert.equal(cmdExec.result.ok, true, JSON.stringify(cmdExec.result));
    assert.equal(typeof cmdExec.result.value.commandId, 'string');
    assert.deepEqual(cmdExec.result.value.result, { kind: 'success', text: 'pong:gateway' });
    const appendedCmdEvents = ownerCalls
      .filter(call => call.method === 'host.session.appendCommandEvent')
      .slice(beforeCommandEvents)
      .map(call => call.params);
    assert.deepEqual(appendedCmdEvents, [
      {
        sessionId: 's1',
        event: { kind: 'run', commandId: cmdExec.result.value.commandId, name: 'ping', args: ' gateway' },
      },
      {
        sessionId: 's1',
        event: { kind: 'done', commandId: cmdExec.result.value.commandId, outcome: 'success', text: 'pong:gateway' },
      },
    ]);
    // Unknown command returns undefined without writing lifecycle events.
    const cmdMiss = await callRpc('commands/execute', { agentId: 's1', line: '/nonexistent', submittedAttachments: [] });
    assert.equal(cmdMiss.result.ok, true, JSON.stringify(cmdMiss.result));
    assert.equal(cmdMiss.result.value, undefined);
    assert.equal(
      ownerCalls.filter(call => call.method === 'host.session.appendCommandEvent').length,
      beforeCommandEvents + 2,
    );
    assert.equal((await callRpc('commands/execute', { agentId: 'nope', line: '/ping x', submittedAttachments: [] })).result.ok, false);

    // Open WebSocket mux to verify `$events` stream receives `settings/document-updated`
    const wsEvents = [];
    const ws = new WebSocket(`ws://127.0.0.1:${webRoutes.port}/api/remote.mux`, {
      headers: {
        host: '127.0.0.1:39999',
        origin: publicOrigin,
        'x-magi-route-proof': proof,
        'x-magi-route-instance': Buffer.from('$host').toString('base64url'),
        'x-magi-route-generation': '1',
        'x-magi-route-epoch': '1',
      },
    });
    await new Promise((resolve, reject) => {
      ws.once('open', resolve);
      ws.once('error', reject);
    });
    const readyPromise = new Promise(resolve => {
      ws.on('message', raw => {
        const msg = JSON.parse(String(raw));
        wsEvents.push(msg);
        if (msg.type === 'item' && msg.value?.type === 'ready') resolve();
      });
    });
    ws.send(JSON.stringify({ type: 'open', streamId: 's-events', endpoint: '$events', payload: { args: {} } }));
    await readyPromise;

    // Mutate plugin entry `sample` via `/api/settings/mutate` (triggers `loading.profilePatch.edit` + volatile live update!)
    const mutated = await callRpc('settings/mutate', {
      ns: 'sample',
      ops: [
        { op: 'set', path: ['greeting'], value: 'bonjour' },
        { op: 'set', path: ['apiKey'], value: 'top-secret-token' },
      ],
    });
    assert.equal(mutated.result.ok, true);
    assert.equal(mutated.result.value.value.greeting, 'bonjour');
    assert.equal(mutated.result.value.value.apiKey, undefined); // redacted!
    assert.deepEqual(mutated.result.value.secrets, [{ path: ['apiKey'], set: true }]);

    // Tool immediately observes updated volatile greeting without restarting fiber!
    const toolResAfter = await services.executeTool({ name: 'sample_greet', args: { who: 'Magi' } });
    assert.deepEqual(toolResAfter.content, [{ type: 'text', text: 'bonjour, Magi!' }]);

    await new Promise(resolve => setTimeout(resolve, 25));
    assert.equal(
      wsEvents.some(m => m.type === 'item' && m.value?.type === 'emit' && m.value.event === 'settings/document-updated'),
      true,
    );

    // A directory change notification becomes the DSH increment event on the same stream.
    sessionDirectory.set('s1', { ...sessionDirectory.get('s1'), isRunning: true });
    services.observeSessionChanged({ hostGeneration: 1, factsSequence: 1, changes: [{ kind: 'changed', sessionId: 's1' }] });
    await new Promise(resolve => setTimeout(resolve, 50));
    assert.equal(
      wsEvents.some(m => m.type === 'item' && m.value?.type === 'emit' && m.value.event === 'api-session/status'),
      true,
    );

    // The page peer is the owner: the real DSH access service answers an owner baseline on its follow
    // stream, and the same snapshot is what it would inject into a served index.
    const baseline = new Promise(resolve => {
      ws.on('message', raw => {
        const msg = JSON.parse(String(raw));
        if (msg.streamId === 's-access' && msg.type === 'item') resolve(msg.value);
      });
    });
    ws.send(JSON.stringify({ type: 'open', streamId: 's-access', endpoint: 'remoteAccess/follow', payload: { args: {} } }));
    assert.deepEqual(await baseline, { type: 'baseline', owner: true });

    // The session control stream opens with an empty projection baseline (Magi registers no session projection).
    const control = new Promise(resolve => {
      ws.on('message', raw => {
        const msg = JSON.parse(String(raw));
        if (msg.streamId === 's-control' && msg.type === 'item') resolve(msg.value);
      });
    });
    ws.send(JSON.stringify({ type: 'open', streamId: 's-control', endpoint: 'session/control', payload: { args: {} } }));
    assert.deepEqual(await control, { type: 'baseline', value: { projections: {} } });

    // follow: the opening snapshot, then an event committed afterwards continues the dense log.
    const followed = [];
    const secondFrame = new Promise(resolve => {
      ws.on('message', raw => {
        const msg = JSON.parse(String(raw));
        if (msg.streamId === 's-follow' && msg.type === 'item') {
          followed.push(msg.value);
          if (followed.length === 2) resolve();
        }
      });
    });
    ws.send(JSON.stringify({
      type: 'open', streamId: 's-follow', endpoint: 'session/follow',
      payload: { args: { request: { address: { kind: 'session', sessionId: 's1' }, assistantStream: true } } },
    }));
    for (let attempt = 0; attempt < 100 && followed.length === 0; attempt++) await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(followed[0]?.type, 'snapshot');
    assert.equal(followed[0].cursor, 2);
    assert.deepEqual(followed[0].assistantStream, { revision: 0 });
    services.observeSessionCommitted({ hostGeneration: 1, sessionId: 's1', eventSeq: 31, occurredAt: 2000, observation: { kind: 'turnStarted', turn: 2 } });
    await secondFrame;
    assert.deepEqual(followed[1], { type: 'event', event: { seq: 3, time: 2000, type: 'turn/start', data: { turn: 2 } } });
    ws.close();
  } finally {
    await pluginMgr?.close();
    await tree.close();
    await services?.close();
    await host.close();
    await webRoutes?.close();
    await rm(dir, { recursive: true, force: true });
  }
});
