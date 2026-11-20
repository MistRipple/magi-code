// Shared DSH host service closure (storage, storageDomain, credentials, authorization,
// attachments, systemPrompt, sessionProjections, sessions, workingDirectory,
// sessionPersistence, sessionQuery, workspaceRegistry, agents, tools, web, commands,
// skills, llm, agentDefaultModel, configEditor, settings, userQuestions, typert,
// remoteAccess, connection, typertGateway, api-remotes, fileUploads,
// pluginRegistryProbe, pluginInventory, and sessionController).
import { randomUUID } from 'node:crypto';
import { existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { pathToFileURL } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import { Context, Service } from '@deepseek-ai/cordis';
import { applyEntryPatches, entryListSchema } from '@deepseek-ai/cordis-plugin-include';
import { deepEqual, updateVolatile, volatileEntries } from '@deepseek-ai/cosmokit';
import { AgentRegistry, emitAgentEvent } from '@deepseek-ai/dsh-agent';
import { AgentPresetRegistry } from '@deepseek-ai/dsh-agent-preset-registry';
import { TypertGatewayService } from '@deepseek-ai/dsh-api-gateway';
import { RemoteAccessService } from '@deepseek-ai/dsh-api-remote-access';
import * as apiRemotes from '@deepseek-ai/dsh-api-remotes';
import { readPluginMeta } from '@deepseek-ai/dsh-app-boot';
import { AuthorizationService } from '@deepseek-ai/dsh-authorization';
import {
  API_PATH,
  CONNECT_PATH,
  Config as ConnectionConfigSchema,
  HostConnectionService,
} from '@deepseek-ai/dsh-client-connection';
import { FileUploads } from '@deepseek-ai/dsh-client-file-upload';
import PluginRegistryProbe from '@deepseek-ai/dsh-client-ui-plugin-manager';
import { CommandRuntime, parseCommand } from '@deepseek-ai/dsh-commands';
import { TYPERT as commandsTypert } from '@deepseek-ai/dsh-commands/typert';
import { ToolCallId } from '@deepseek-ai/dsh-llm';
import { createScope } from '@deepseek-ai/dsh-scope';
import { SessionProjectionRegistry } from '@deepseek-ai/dsh-session-projection';
import { SettingsConflictError, SettingsForms } from '@deepseek-ai/dsh-settings';
import { SkillRegistry } from '@deepseek-ai/dsh-skill';
import { Storage, storageBackendServiceKey } from '@deepseek-ai/dsh-storage';
import * as storageDomainPlugin from '@deepseek-ai/dsh-storage-domain';
import { SystemPrompt } from '@deepseek-ai/dsh-system-prompt';
import { ToolsService, projectToolSchema } from '@deepseek-ai/dsh-tools';
import { RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol';
import { TypertRegistry } from '@deepseek-ai/dsh-typert-registry';
import { UserQuestionError, UserQuestionService } from '@deepseek-ai/dsh-user-questions';
import { snapshotJsonValue } from '@deepseek-ai/dsh-util-values';
import { WebRuntime } from '@deepseek-ai/dsh-web';
import { WorkspaceRegistry } from '@deepseek-ai/dsh-workspace';
import yaml from 'js-yaml';
import { openExecutionSeams } from './execution-seams.mjs';
import { openLlmBridge } from './llm-bridge.mjs';
import { markRemoteMethod } from './remote-methods.mjs';
import { openRuntimePorts } from './runtime-ports.mjs';
import { createSessionGateway } from './session-gateway.mjs';
import {
  createAttachmentStoreClass,
  createCredentialServiceClass,
  createJsonFileStorageBackend,
} from './storage-services.mjs';

function loadYamlArray(content) {
  if (typeof content !== 'string' || content.trim() === '') return [];
  const parsed = yaml.load(content, { schema: entryListSchema });
  return Array.isArray(parsed) ? parsed : [];
}

function flattenEntryRows(rows) {
  const out = [];
  for (const row of Array.isArray(rows) ? rows : []) {
    if (!row || typeof row !== 'object') continue;
    out.push(row);
    if ((row.group || row.name === 'cordis:group') && Array.isArray(row.config)) {
      out.push(...flattenEntryRows(row.config));
    }
  }
  return out;
}

function commitVolatileFiber(hostEntry, next) {
  const rawFiber = hostEntry.fiber.ctx.fiber;
  const candidate = rawFiber.resolveConfig(next);
  const refs = volatileEntries(rawFiber.effectiveConfig);
  const paths = refs.flatMap(({ path, ref }) => {
    const source = path.reduce((value, key) => Reflect.get(value, key), candidate);
    if (deepEqual(ref.get(), source.get(), true)) return [];
    updateVolatile(ref, source);
    return [path];
  });
  rawFiber.config = structuredClone(next);
  hostEntry.options.config = structuredClone(next);
  hostEntry.override = structuredClone(next);
  if (paths.length) {
    const target = Object.create(rawFiber.ctx);
    target[Context.filter] = owner => owner.fiber === rawFiber;
    rawFiber.ctx.emit(target, 'loader/volatile-update', paths);
  }
}

async function bridgeNodeHttpToFetch(req, res, apiHandler, peer, maxRequestBodyBytes) {
  const abort = new AbortController();
  res.on('close', () => {
    if (!res.writableEnded) abort.abort();
  });
  const url = new URL(req.url ?? '/', 'http://dsh.internal');
  const method = req.method ?? 'GET';
  const headers = Object.fromEntries(
    Object.entries(req.headers).filter(([, value]) => typeof value === 'string'),
  );
  const bodyMode = apiHandler.requestBodyMode({ method, url });
  let request;
  if (bodyMode === 'buffered') {
    const declaredLength = req.headers['content-length'];
    if (declaredLength !== undefined && Number(declaredLength) > maxRequestBodyBytes) {
      res.writeHead(413, { connection: 'close' });
      res.end();
      req.destroy();
      return;
    }
    const chunks = [];
    let received = 0;
    for await (const chunk of req) {
      received += chunk.byteLength;
      if (received > maxRequestBodyBytes) {
        res.writeHead(413, { connection: 'close' });
        res.end();
        req.destroy();
        return;
      }
      chunks.push(chunk);
    }
    request = new Request(url, {
      method,
      headers,
      ...(chunks.length > 0 ? { body: Buffer.concat(chunks) } : {}),
      signal: abort.signal,
    });
  } else {
    request = new Request(url, {
      method,
      headers,
      body: Readable.toWeb(req),
      signal: abort.signal,
      duplex: 'half',
    });
  }
  const response = await apiHandler.fetch(request, peer);
  const requestUnread = bodyMode === 'streaming' && !req.readableEnded;
  const responseHeaders = Object.fromEntries(response.headers.entries());
  res.writeHead(response.status, requestUnread ? { ...responseHeaders, connection: 'close' } : responseHeaders);
  if (response.body === null) {
    res.end();
    if (requestUnread) req.destroy();
    return;
  }
  for await (const chunk of response.body) {
    if (abort.signal.aborted) continue;
    if (!res.write(chunk) && !res.destroyed) {
      await new Promise(resolve => {
        const done = () => {
          res.off('drain', done);
          res.off('close', done);
          resolve();
        };
        res.once('drain', done);
        res.once('close', done);
      });
    }
  }
  res.end();
  if (requestUnread) req.destroy();
}

export async function openHostServices({
  lifecycle,
  tree,
  dshHome,
  initialHostConfig = {},
  requestOwner,
  sessionReader,
  notifyOwner,
}) {
  if (typeof dshHome !== 'string' || !dshHome) {
    throw new Error('openHostServices requires a non-empty dshHome path');
  }
  mkdirSync(dshHome, { recursive: true });
  process.env.DSH_HOME = dshHome;

  await tree.ensurePublicLoader();

  const root = lifecycle.context;
  const fibers = [];
  const disposers = [];
  let closed = false;

  async function mount(plugin, config, parentCtx = root) {
    const fiber = parentCtx.plugin(plugin, config);
    fibers.push(fiber);
    await fiber.await();
    return fiber;
  }

  // 1. Storage hub + atomic JSON backend + official storageDomain, credentials, authorization, attachments & prompt
  await mount(Storage);
  const jsonStorageBackend = createJsonFileStorageBackend(join(dshHome, 'storage'));
  const disposeJsonBackend = root.storage.backend.register('json', jsonStorageBackend);
  disposers.push(async () => {
    disposeJsonBackend();
    await jsonStorageBackend.close();
  });
  root.provide(storageBackendServiceKey('json'), true);
  await mount(storageDomainPlugin, { backend: 'json' });

  const CredentialService = createCredentialServiceClass(dshHome);
  await mount(CredentialService);
  await mount(AuthorizationService);

  const AttachmentStoreService = createAttachmentStoreClass(dshHome);
  await mount(AttachmentStoreService);
  await mount(SystemPrompt, { includeHarnessIdentity: false });

  // 2. Official TypertRegistry, SessionProjectionRegistry, session readers, WorkspaceRegistry & AgentRegistry
  await mount(TypertRegistry);
  await root.typert.remotes.register({
    package: commandsTypert.package,
    descriptors: commandsTypert.invocations,
  });
  await root.typert.resources.register('session', { field: 'sessionId' });

  await mount(SessionProjectionRegistry);
  if (sessionReader) {
    await mount(sessionReader.Sessions);
    await mount(sessionReader.WorkingDirectory);
    await mount(sessionReader.SessionPersistence);
    await mount(sessionReader.SessionQuery);
    await sessionReader.syncDirectory().catch(() => {});
    await root.typert.lookups.register('session', {
      parameter: 'session',
      wire: 'sessionId',
      resource: 'session',
      hostTypeSymbol: '@deepseek-ai/dsh-session#Session',
      wireTypeSymbol: '@deepseek-ai/dsh-session/types#SessionId',
      resolve: async sessionId => {
        if (typeof sessionId !== 'string' || !sessionId) return undefined;
        await sessionReader.syncDirectory().catch(() => {});
        return root.get('sessions')?.get(sessionId);
      },
    });
    sessionReader.setBootingServices?.(true);
    try {
      await mount(WorkspaceRegistry);
    } finally {
      sessionReader.setBootingServices?.(false);
    }
  }
  await mount(AgentRegistry);
  await mount(AgentPresetRegistry, { default: 'default' });

  // Native calls carry a session identity after daemon admission. The scoped
  // object exists only during that call, without opening a live Agent.
  const admittedAgents = new WeakSet();
  const managedAgents = new Map();

  function invocationScope(sessionId) {
    if (typeof sessionId !== 'string' || !sessionId) throw new TypeError('Missing native session identity');
    const session = root.get('sessions')?.get(sessionId) ?? Object.freeze({ id: sessionId });
    const agent = { id: sessionId, session };
    const scope = createScope(root, agent);
    agent.ctx = scope.ctx;
    const frozen = Object.freeze(agent);
    admittedAgents.add(frozen);
    return { agent: frozen, dispose: scope.dispose };
  }

  async function disposeAgentScope(sessionId) {
    const existing = managedAgents.get(sessionId);
    if (!existing) return;
    managedAgents.delete(sessionId);
    try { existing.detach?.(); } catch { /* best-effort registry detach */ }
    try { await existing.disposeScope(); } catch { /* best-effort scope teardown */ }
  }

  async function upsertManagedAgent(fact) {
    const sessionId = fact?.sessionId;
    if (closed || typeof sessionId !== 'string' || !sessionId) return undefined;
    const nextStatus = fact.isRunning ? 'running' : 'idle';
    const existing = managedAgents.get(sessionId);
    if (existing) {
      if (existing.status !== nextStatus) {
        existing.status = nextStatus;
        emitAgentEvent(root, existing.agent, 'agent/status', {
          agent: existing.agent,
          status: nextStatus,
        });
      }
      return existing.agent;
    }
    const state = { status: nextStatus, agent: null, detach: null, disposeScope: null };
    const session = root.get('sessions')?.get(sessionId) ?? Object.freeze({ id: sessionId });
    const agent = {
      id: sessionId,
      session,
      get status() { return state.status; },
      async cancel() {
        await requestOwner('host.session.cancel', { sessionId });
      },
      async followup(message) {
        const text = typeof message === 'string'
          ? message
          : Array.isArray(message?.content)
            ? message.content.filter(p => p?.type === 'text' && typeof p.text === 'string').map(p => p.text).join('\n\n')
            : '';
        await requestOwner('host.session.prompt', {
          sessionId,
          requestId: randomUUID(),
          mode: 'queue',
          text,
          images: [],
        });
      },
      async steer(message) {
        const text = typeof message === 'string'
          ? message
          : Array.isArray(message?.content)
            ? message.content.filter(p => p?.type === 'text' && typeof p.text === 'string').map(p => p.text).join('\n\n')
            : '';
        await requestOwner('host.session.prompt', {
          sessionId,
          requestId: randomUUID(),
          mode: 'steer',
          text,
          images: [],
        });
      },
    };
    const scope = createScope(root, agent);
    agent.ctx = scope.ctx;
    admittedAgents.add(agent);
    state.agent = agent;
    state.disposeScope = scope.dispose;
    const detach = root.agents.enter(agent);
    state.detach = detach;
    managedAgents.set(sessionId, state);
    try {
      await root.agents.announce(agent, 'resume');
    } catch (err) {
      managedAgents.delete(sessionId);
      try { detach(); } catch { /* ignore */ }
      try { await scope.dispose(); } catch { /* ignore */ }
      throw err;
    }
    return agent;
  }

  // Authoritative cold-safe Agent lookup for Typert (`commands/*`, `fileUploads/*`).
  // Always validates session existence against daemon facts; registers into `ctx.agents` if missing.
  async function resolveAgentHandle(sessionId) {
    if (closed || typeof sessionId !== 'string' || !sessionId) return undefined;
    const live = root.agents.get(sessionId);
    if (live) return live;
    const response = await requestOwner('host.session.get', { sessionId });
    const fact = response?.session;
    if (!fact || fact.sessionId !== sessionId) {
      await disposeAgentScope(sessionId);
      return undefined;
    }
    if (sessionReader) {
      await sessionReader.syncDirectory().catch(() => {});
    }
    return await upsertManagedAgent(fact);
  }

  // Configure official `AgentRegistry`'s `'agent'` lookup and hostContext with daemon-verified cold resolution.
  root.typert.lookups.configure('agent', resolveAgentHandle);
  root.typert.contexts.configureHost('agent', async sessionId => (await resolveAgentHandle(sessionId))?.ctx);

  async function syncManagedAgentsFromDaemon() {
    if (closed) return;
    try {
      const res = await requestOwner('host.session.list', { includeArchived: false });
      const sessions = Array.isArray(res?.sessions) ? res.sessions : [];
      const seen = new Set();
      for (const fact of sessions) {
        if (!fact?.sessionId) continue;
        seen.add(fact.sessionId);
        await upsertManagedAgent(fact);
      }
      for (const id of [...managedAgents.keys()]) {
        if (!seen.has(id)) await disposeAgentScope(id);
      }
    } catch {
      // Initial sync is best-effort when requestOwner is a minimal unit-test stub.
    }
  }
  await syncManagedAgentsFromDaemon();

  disposers.push(async () => {
    for (const id of [...managedAgents.keys()]) {
      await disposeAgentScope(id);
    }
  });

  // 3. ToolsService (`ctx.tools`)
  const toolOwners = new WeakMap();
  class MagiToolsService extends ToolsService {
    register(definition, options = {}) {
      const caller = root.reflect.trace(this.ctx);
      const ownerId = lifecycle.ownerIdentity(caller);
      if (ownerId && definition && typeof definition === 'object') {
        toolOwners.set(definition, ownerId);
      }
      return super.register(definition, options);
    }
  }
  await mount(MagiToolsService);

  const runtimePorts = await openRuntimePorts({
    root,
    lifecycle,
    requestOwner,
    notifyOwner,
    invocationScope,
    resolveAgentHandle,
    mount,
  });
  disposers.push(() => runtimePorts.close());

  const executionSeams = await openExecutionSeams({
    root,
    lifecycle,
    dshHome,
    requestOwner,
    notifyOwner,
    mount,
  });
  disposers.push(() => executionSeams.close());

  const defaultToolExecutor = {
    describe(definitions) {
      return {
        schemas: definitions.map(def => projectToolSchema(def, true)),
      };
    },
    async execute(input) {
      const results = [];
      for (const call of input?.calls ?? []) {
        const outcome = await executeSingleTool({
          name: call.name,
          args: call.arguments,
          callId: call.callId,
        }, input?.signal);
        results.push(outcome);
      }
      return { results };
    },
  };
  disposers.push(root.tools.setExecutor(defaultToolExecutor));

  let toolsPublishQueued = false;
  function publishToolsNow() {
    if (closed) return;
    const tools = [];
    for (const def of root.tools.definitions()) {
      const ownerId = toolOwners.get(def);
      const schema = projectToolSchema(def, true);
      tools.push({
        pluginId: ownerId?.pluginId ?? null,
        instanceId: ownerId?.instanceId ?? null,
        name: schema.name,
        description: schema.description,
        parameters: schema.parameters,
        ...(schema.deferLoading === true ? { deferLoading: true } : {}),
        ...(typeof def.timeoutMs === 'number' ? { timeoutMs: def.timeoutMs } : {}),
      });
    }
    notifyOwner('host.tools.changed', { tools });
  }
  function publishTools() {
    if (closed || toolsPublishQueued) return;
    toolsPublishQueued = true;
    queueMicrotask(() => {
      toolsPublishQueued = false;
      publishToolsNow();
    });
  }
  disposers.push(root.on('tools/change', publishTools));

  async function serializeContentBlocks(blocks, signal) {
    const out = [];
    const attachments = root.get('attachments');
    for (const block of Array.isArray(blocks) ? blocks : []) {
      if (!block || typeof block !== 'object') continue;
      if (block.type === 'text' && typeof block.text === 'string') {
        out.push({ type: 'text', text: block.text });
      } else if (block.type === 'image' && block.attachment && attachments) {
        const stored = await attachments.readImage(block.attachment, signal);
        out.push({
          type: 'image',
          mediaType: stored.ref.mediaType,
          dataBase64: Buffer.from(stored.data).toString('base64'),
          width: stored.ref.width,
          height: stored.ref.height,
          bytes: stored.ref.bytes,
          hostPath: attachments.imageHostPath(stored.ref),
        });
      }
    }
    return out;
  }

  async function executeSingleTool({ name, args, callId, sessionId }, externalSignal) {
    const definition = root.tools.get(String(name ?? ''));
    if (!definition) {
      return {
        isError: true,
        error: { message: `Unknown plugin tool: ${name}`, code: 'UNKNOWN_TOOL' },
        content: [{ type: 'text', text: `Unknown plugin tool: ${name}` }],
      };
    }
    const ownerId = toolOwners.get(definition);
    const lease = ownerId ? lifecycle.acquire(ownerId) : null;
    const signals = [
      ...(externalSignal ? [externalSignal] : []),
      ...(lease ? [lease.signal] : []),
      ...(typeof definition.timeoutMs === 'number' && definition.timeoutMs > 0
        ? [AbortSignal.timeout(definition.timeoutMs)] : []),
    ];
    const signal = signals.length > 0 ? AbortSignal.any(signals) : new AbortController().signal;
    const frozenArgs = snapshotJsonValue(args ?? {}) ?? {};
    const resolvedCallId = ToolCallId(String(callId ?? randomUUID()));
    const scope = sessionId == null ? undefined : invocationScope(sessionId);
    const additionalContexts = [];
    let concludesTurn = false;
    const token = Symbol('magi.tool.exec');
    const execBase = Object.freeze({
      callId: resolvedCallId,
      rootCallId: resolvedCallId,
      name: definition.name,
      arguments: frozenArgs,
      ...(scope ? { agent: scope.agent } : {}),
      signal,
      token,
    });
    const runContext = Object.freeze({
      ...execBase,
      deferContext(ctxMsg) {
        additionalContexts.push(ctxMsg);
      },
      concludeTurn() {
        concludesTurn = true;
      },
    });
    const projectContent = definition.projectContent?.bind(definition);
    const finalizeContent = definition.finalizeContent?.bind(definition);
    try {
      signal.throwIfAborted();
      lease?.assertCurrent();
      // Tool pre-execute pipeline: `tools/pre-execute` waterfall, approval check, and monotonic `tools.guard()`.
      const pre = await runtimePorts.runPreExecute({
        sessionId,
        callId: resolvedCallId,
        name: definition.name,
        arguments: frozenArgs,
      }, signal);
      if (pre.kind === 'deny') {
        return {
          isError: true,
          error: { message: pre.reason, code: pre.info?.code ?? 'TOOL_DENIED', ...(pre.info ? { info: pre.info } : {}) },
          content: [{ type: 'text', text: pre.reason }],
        };
      }
      if (pre.kind === 'cancel') {
        return {
          isError: true,
          error: { message: 'Tool execution was cancelled', code: 'TOOL_CANCELLED' },
          content: [{ type: 'text', text: 'Tool execution was cancelled' }],
        };
      }
      if (pre.kind === 'ask') {
        return {
          isError: true,
          error: { message: pre.reason ?? 'Approval required for tool execution', code: 'APPROVAL_REQUIRED' },
          content: [{ type: 'text', text: pre.reason ?? 'Approval required for tool execution' }],
        };
      }
      signal.throwIfAborted();
      lease?.assertCurrent();
      const effectiveArgs = pre.arguments ?? frozenArgs;
      const effectiveExecBase = pre.arguments !== undefined
        ? Object.freeze({ ...execBase, arguments: effectiveArgs })
        : execBase;
      const effectiveRunContext = pre.arguments !== undefined
        ? Object.freeze({ ...runContext, arguments: effectiveArgs })
        : runContext;
      const value = await root.waterfall(
        'tools/execute',
        effectiveExecBase,
        async () => await definition.execute(effectiveArgs, effectiveRunContext),
      );
      signal.throwIfAborted();
      lease?.assertCurrent();
      let content = definition.output.render(effectiveArgs, value);
      const meta = definition.output.presentationMeta?.(effectiveArgs, value);
      let resultObj = {
        isError: false,
        value,
        content,
        ...(meta !== undefined ? { meta } : {}),
        ...(additionalContexts.length ? { additionalContexts } : {}),
        ...(concludesTurn ? { concludesTurn: true } : {}),
      };
      if (projectContent) {
        const projected = projectContent(execBase, resultObj);
        if (projected !== undefined) {
          content = projected;
          resultObj = { ...resultObj, content };
        }
      }
      if (finalizeContent) {
        const finalized = finalizeContent(execBase, resultObj);
        if (finalized !== undefined) {
          content = finalized;
          resultObj = { ...resultObj, content };
        }
      }
      // Tool post-execute pipeline: `toolResultPruner`, `tools/post-execute` waterfall, and `tools/result` observer.
      const post = await runtimePorts.runPostExecute({
        sessionId,
        callId: resolvedCallId,
        name: definition.name,
        arguments: frozenArgs,
        isError: false,
        status: 'succeeded',
        value,
        content,
      }, signal);
      if (post.kind === 'block') {
        return {
          isError: true,
          error: { message: post.text || 'Blocked by tools/post-execute', code: post.info?.code ?? 'TOOL_POST_BLOCKED' },
          content: await serializeContentBlocks(post.feedback, signal),
        };
      }
      const finalContent = post.content ?? content;
      return {
        isError: false,
        value,
        content: await serializeContentBlocks(finalContent, signal),
        ...(meta !== undefined ? { meta } : {}),
        ...(concludesTurn ? { concludesTurn: true } : {}),
      };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const code = typeof error?.code === 'string' ? error.code : 'TOOL_EXECUTION_ERROR';
      let content = [{ type: 'text', text: message }];
      const failureObj = {
        isError: true,
        error: { message, info: { name: error instanceof Error ? error.name : 'Error', code } },
        content,
      };
      if (finalizeContent) {
        try {
          const finalized = finalizeContent(execBase, failureObj);
          if (finalized !== undefined) content = finalized;
        } catch { /* finalizeContent must not override failure */ }
      }
      return {
        isError: true,
        error: { message, code },
        content: await serializeContentBlocks(content, signal).catch(() => [{ type: 'text', text: message }]),
      };
    } finally {
      await scope?.dispose();
    }
  }

  // 4. WebRuntime (`ctx.web`)
  const searchProviderOwners = new Map();
  const fetchProviderOwners = new Map();
  const initialWebConfig = {};
  if (typeof initialHostConfig?.web?.searchProvider === 'string' && initialHostConfig.web.searchProvider) {
    initialWebConfig.searchProvider = initialHostConfig.web.searchProvider;
  }
  if (typeof initialHostConfig?.web?.fetchProvider === 'string' && initialHostConfig.web.fetchProvider) {
    initialWebConfig.fetchProvider = initialHostConfig.web.fetchProvider;
  }
  class MagiWebRuntime extends WebRuntime {
    registerSearchProvider(provider) {
      const caller = root.reflect.trace(this.ctx);
      const ownerId = lifecycle.ownerIdentity(caller);
      const dispose = super.registerSearchProvider(provider);
      if (ownerId) searchProviderOwners.set(provider.id, ownerId);
      return () => {
        searchProviderOwners.delete(provider.id);
        dispose();
      };
    }
    registerFetchProvider(provider) {
      const caller = root.reflect.trace(this.ctx);
      const ownerId = lifecycle.ownerIdentity(caller);
      const dispose = super.registerFetchProvider(provider);
      if (ownerId) fetchProviderOwners.set(provider.id, ownerId);
      return () => {
        fetchProviderOwners.delete(provider.id);
        dispose();
      };
    }
  }
  const webFiber = await mount(MagiWebRuntime, initialWebConfig);

  let webPublishQueued = false;
  function publishWebProvidersNow() {
    if (closed) return;
    const listed = root.web.listProviders();
    notifyOwner('host.web.changed', {
      searchProvider: root.web.config.searchProvider.get() ?? null,
      fetchProvider: root.web.config.fetchProvider.get() ?? null,
      search: listed.search.map(p => ({
        ...p,
        pluginId: searchProviderOwners.get(p.id)?.pluginId ?? null,
        instanceId: searchProviderOwners.get(p.id)?.instanceId ?? null,
      })),
      fetch: listed.fetch.map(p => ({
        ...p,
        pluginId: fetchProviderOwners.get(p.id)?.pluginId ?? null,
        instanceId: fetchProviderOwners.get(p.id)?.instanceId ?? null,
      })),
    });
  }
  function publishWebProviders() {
    if (closed || webPublishQueued) return;
    webPublishQueued = true;
    queueMicrotask(() => {
      webPublishQueued = false;
      publishWebProvidersNow();
    });
  }
  disposers.push(root.on('web/providers-changed', publishWebProviders));

  // 5. CommandRuntime (`ctx.commands`) & SkillRegistry (`ctx.skills`)
  const commandOwners = new Map();
  let pendingCommandId;
  const capturedCommandDone = new Map();

  async function appendCommandLifecycleEvent(sessionId, event) {
    await requestOwner('host.session.appendCommandEvent', { sessionId, event });
  }

  class MagiCommandRuntime extends CommandRuntime {
    register(definition) {
      const caller = root.reflect.trace(this.ctx);
      const ownerId = lifecycle.ownerIdentity(caller);
      const dispose = super.register(definition);
      if (ownerId && typeof definition?.name === 'string') {
        commandOwners.set(definition.name, ownerId);
      }
      return () => {
        if (typeof definition?.name === 'string') commandOwners.delete(definition.name);
        dispose();
      };
    }

    mintCommandId() {
      if (pendingCommandId !== undefined) {
        const id = pendingCommandId;
        pendingCommandId = undefined;
        return id;
      }
      return super.mintCommandId();
    }

    async execute(agent, line, submittedAttachments, signal) {
      if (!agent || !admittedAgents.has(agent)) {
        throw new RemoteError('command/native-admission-required', 'Submit the command through a Magi session');
      }
      const attachments = Array.isArray(submittedAttachments) ? submittedAttachments : [];
      const activeSignal = signal ?? new AbortController().signal;
      const parsed = typeof line === 'string' ? parseCommand(line) : undefined;
      if (parsed === undefined) return undefined;
      const definition = this.find(agent, parsed.name);
      if (definition === undefined) return undefined;
      const owner = commandOwners.get(parsed.name);
      const lease = owner ? lifecycle.acquire(owner) : null;
      const callSignal = lease ? AbortSignal.any([activeSignal, lease.signal]) : activeSignal;
      if (lease) lease.assertCurrent();
      if (callSignal.aborted) {
        throw callSignal.reason instanceof Error
          ? callSignal.reason
          : new Error(typeof callSignal.reason === 'string' ? callSignal.reason : 'command aborted');
      }
      const commandId = super.mintCommandId();
      await appendCommandLifecycleEvent(agent.id, {
        kind: 'run',
        commandId,
        name: parsed.name,
        ...(definition.recordInput === false ? {} : { args: parsed.rawInput }),
      });
      pendingCommandId = commandId;
      let execution;
      try {
        execution = await super.execute(agent, line, attachments, callSignal);
      } catch (error) {
        pendingCommandId = undefined;
        const done = capturedCommandDone.get(commandId);
        capturedCommandDone.delete(commandId);
        const text = done?.text
          ?? (error instanceof Error && error.message.trim() ? error.message : String(error || 'command failed'));
        try {
          await appendCommandLifecycleEvent(agent.id, {
            kind: 'done',
            commandId,
            outcome: 'error',
            text,
          });
        } catch (appendError) {
          root.logger.warn(`command "${parsed.name}": command/done append failed: ${String(appendError)}`);
        }
        throw error;
      }
      const done = capturedCommandDone.get(commandId);
      capturedCommandDone.delete(commandId);
      if (lease) lease.assertCurrent();
      if (done) {
        await appendCommandLifecycleEvent(agent.id, {
          kind: 'done',
          commandId,
          outcome: done.kind,
          ...(done.text === undefined ? {} : { text: done.text }),
          ...(done.kind === 'success' && done.sourceEventSeq !== undefined
            ? { sourceEventSeq: done.sourceEventSeq }
            : {}),
        });
      }
      return execution;
    }

    appendLifecycle(_session, type, data) {
      if (type === 'command/done' && data?.commandId) {
        capturedCommandDone.set(data.commandId, data);
      }
    }
  }
  await mount(MagiCommandRuntime);

  let commandsPublishQueued = false;
  function publishCommands() {
    if (closed || commandsPublishQueued) return;
    commandsPublishQueued = true;
    queueMicrotask(() => {
      commandsPublishQueued = false;
      if (closed) return;
      const list = root.commands.list();
      notifyOwner('host.commands.changed', {
        commands: list.map(cmd => ({
          ...cmd,
          pluginId: commandOwners.get(cmd.name)?.pluginId ?? null,
          instanceId: commandOwners.get(cmd.name)?.instanceId ?? null,
        })),
      });
    });
  }
  disposers.push(root.on('commands/change', publishCommands));

  class MagiSkillRegistry extends SkillRegistry {
    register(skill) {
      const dispose = super.register(skill);
      queueMicrotask(() => void publishSkills().catch(() => {}));
      return () => {
        dispose();
        queueMicrotask(() => void publishSkills().catch(() => {}));
      };
    }
    registerProvider(create) {
      const dispose = super.registerProvider(control => create({
        ...control,
        invalidate() {
          control.invalidate();
          queueMicrotask(() => void publishSkills().catch(() => {}));
        },
      }));
      queueMicrotask(() => void publishSkills().catch(() => {}));
      return () => {
        dispose();
        queueMicrotask(() => void publishSkills().catch(() => {}));
      };
    }
  }
  await mount(MagiSkillRegistry);

  async function publishSkills() {
    if (closed) return;
    const skills = await root.skills.list();
    if (!closed) notifyOwner('host.skills.changed', { skills });
  }

  // 6. Bidirectional LLM & AgentDefaultModel bridge (`ctx.llm`, `ctx.agentDefaultModel`)
  const llmBridge = await openLlmBridge({
    lifecycle,
    root,
    initialHostConfig,
    requestOwner,
    notifyOwner,
  });

  // 7. ConfigEditor (`ctx.configEditor`) + SettingsForms (`ctx.settings`) + Remote controllers
  const webHostEntry = {
    id: 'host:web',
    isHostEntry: true,
    options: { id: 'web', name: 'host:web', config: structuredClone(initialWebConfig) },
    inherited: {},
    override: structuredClone(initialWebConfig),
    fiber: webFiber,
  };
  const defaultModelHostEntry = {
    id: 'host:agentDefaultModel',
    isHostEntry: true,
    options: {
      id: 'agentDefaultModel',
      name: 'host:agentDefaultModel',
      config: structuredClone(llmBridge.defaultModelConfig),
    },
    inherited: structuredClone(llmBridge.defaultModelConfig),
    override: structuredClone(llmBridge.defaultModelConfig),
    fiber: llmBridge.agentDefaultModelFiber,
  };
  llmBridge.bindDefaultModelEntry(defaultModelHostEntry);
  const hostEntries = [webHostEntry, defaultModelHostEntry];

  let editQueue = Promise.resolve();
  function enqueueConfigEdit(fn) {
    const run = editQueue.then(fn, fn);
    editQueue = run.catch(() => {});
    return run;
  }

  function computeLayerValues(snapshot, entries) {
    const byName = new Map((snapshot.sources ?? []).map(s => [s.name, s.content]));
    const baseEntries = loadYamlArray(byName.get('cordis.yml'));
    const bundlePatches = loadYamlArray(byName.get('bundle.patch.yml'));
    const userPatches = loadYamlArray(byName.get('cordis.patch.yml'));
    const overriddenIds = new Set(
      userPatches
        .filter(patch => patch && patch.insert === undefined && Object.hasOwn(patch, 'config'))
        .map(patch => patch.id),
    );
    const composedWithoutOverride = new Map();
    if (entries.some(e => !overriddenIds.has(e.options.id))) {
      for (const row of flattenEntryRows(
        applyEntryPatches(baseEntries, [...bundlePatches, ...userPatches], () => {}),
      )) {
        if (row.id && !composedWithoutOverride.has(row.id)) composedWithoutOverride.set(row.id, row);
      }
    }
    const inheritedFor = entryId => {
      if (!overriddenIds.has(entryId)) {
        return structuredClone(composedWithoutOverride.get(entryId)?.config ?? {});
      }
      const strippedUser = userPatches.map(patch => {
        if (!patch || patch.id !== entryId || patch.insert !== undefined) return patch;
        const rest = { ...patch };
        Reflect.deleteProperty(rest, 'config');
        return rest;
      });
      const row = flattenEntryRows(
        applyEntryPatches(baseEntries, [...bundlePatches, ...strippedUser], () => {}),
      ).find(item => item.id === entryId);
      return structuredClone(row?.config ?? {});
    };
    return entries.map(entry => ({
      entry,
      inherited: inheritedFor(entry.options.id),
      override: structuredClone(
        userPatches.findLast(
          row => row && row.insert === undefined && row.id === entry.options.id && row.config !== undefined,
        )?.config ?? {},
      ),
    }));
  }

  class MagiConfigEditor extends Service {
    constructor(ctx) {
      super(ctx, 'configEditor');
    }

    get documentPath() {
      const snapshot = tree.configEditorSnapshot();
      return snapshot.sourceRoot ? join(snapshot.sourceRoot, 'cordis.patch.yml') : join(dshHome, 'cordis.patch.yml');
    }

    entries() {
      const pluginEntries = tree.configEditorSnapshot().entries;
      const pluginIds = new Set(pluginEntries.map(e => e.options.id));
      return [
        ...hostEntries.filter(h => !pluginIds.has(h.options.id)),
        ...pluginEntries,
      ];
    }

    configuration() {
      const snapshot = tree.configEditorSnapshot();
      const pluginEntries = snapshot.entries;
      const pluginIds = new Set(pluginEntries.map(e => e.options.id));
      const hostLayer = hostEntries
        .filter(h => !pluginIds.has(h.options.id))
        .map(entry => ({
          entry,
          inherited: structuredClone(entry.inherited),
          override: structuredClone(entry.override),
        }));
      return [
        ...hostLayer,
        ...computeLayerValues(snapshot, pluginEntries),
      ];
    }

    async edit(entry, change) {
      await enqueueConfigEdit(async () => {
        if (entry?.isHostEntry === true) {
          if (entry.fiber.state !== 2) throw new Error('Host configuration service is no longer active');
          const current = structuredClone(entry.options.config ?? {});
          const inherited = structuredClone(entry.inherited ?? {});
          const next = change(current, inherited);
          commitVolatileFiber(entry, next);
          notifyOwner('host.settings.hostChanged', {
            ns: entry.options.id,
            config: structuredClone(next),
          });
          if (entry.options.id === 'web') publishWebProviders();
          return;
        }
        const snapshot = tree.configEditorSnapshot();
        const liveEntry = snapshot.entries.find(e => e.id === entry?.id);
        if (!liveEntry || liveEntry.fiber === undefined || liveEntry.fiber.state !== 2) {
          throw new Error('Configuration plugin is no longer active');
        }
        const [layer] = computeLayerValues(snapshot, [liveEntry]);
        const current = structuredClone(liveEntry.options.config ?? {});
        const inherited = layer.inherited;
        const next = change(current, inherited);
        liveEntry.fiber.resolveConfig(next);
        const clearOverride = isDeepStrictEqual(next, inherited);
        await requestOwner('loading.profilePatch.edit', {
          expectedRevision: snapshot.revision,
          id: liveEntry.options.id,
          name: liveEntry.options.name,
          nextConfig: clearOverride ? null : next,
          clearOverride,
        });
        root.emit('app-boot/config-reload');
      });
    }
  }
  await mount(MagiConfigEditor);

  const settingsCtx = root.isolate('profileContext');
  settingsCtx.provide('profileContext', { home: dshHome, name: 'magi' });
  await mount(SettingsForms, undefined, settingsCtx);

  disposers.push(root.on('settings/document-updated', (ns, revision) => {
    if (ns === 'web') publishWebProviders();
    notifyOwner('host.settings.updated', { ns, revision });
  }));

  async function runSettingsWrite(ns, action) {
    try {
      await action();
    } catch (error) {
      if (error instanceof SettingsConflictError || error?.code === 'SETTINGS_CONFLICT') {
        throw new RemoteError('settings/conflict', error.message, {
          ns,
          expected: error.expected,
          actual: error.actual,
        });
      }
      throw error;
    }
    const updated = root.settings.describe({ redactSecrets: true }).find(row => row.ns === ns);
    if (!updated) throw new Error(`Settings namespace "${ns}" is unavailable after write`);
    return updated;
  }

  class SettingsControllerService extends TypertRemoteService {
    constructor(ctx) {
      super(ctx, 'settingsController', { namespace: 'settings' });
    }

    describe() {
      const docPath = root.settings.documentPath;
      return {
        writable: root.settings.writable,
        hasDocument: typeof docPath === 'string' && docPath.length > 0 && existsSync(docPath),
        namespaces: root.settings.describe({ redactSecrets: true }),
      };
    }

    async update(ns, patch, expectedRevision) {
      return await runSettingsWrite(String(ns), () => root.settings.update(String(ns), patch, expectedRevision));
    }

    async replace(ns, section, expectedRevision) {
      return await runSettingsWrite(String(ns), () => root.settings.replace(String(ns), section, expectedRevision));
    }

    async mutate(ns, ops, expectedRevision) {
      return await runSettingsWrite(String(ns), () => root.settings.mutate(String(ns), ops, expectedRevision));
    }

    async openSettingsDocument(signal) {
      signal?.throwIfAborted();
      const path = await root.settings.prepareDocument();
      await requestOwner('host.settings.openDocument', { path }).catch(() => {});
      return { opened: true };
    }
  }
  for (const method of ['describe', 'update', 'replace', 'mutate', 'openSettingsDocument']) {
    markRemoteMethod(SettingsControllerService.prototype, method);
  }
  await mount(SettingsControllerService);

  class CredentialsControllerService extends TypertRemoteService {
    constructor(ctx) {
      super(ctx, 'credentialsController', { namespace: 'credentials' });
    }

    async describe(refs) {
      const out = {};
      for (const ref of Array.isArray(refs) ? refs : []) {
        out[String(ref)] = await root.credentials.describe(String(ref));
      }
      return out;
    }

    async set(ref, value) {
      await root.credentials.set(String(ref), String(value));
    }

    async unset(ref) {
      await root.credentials.unset(String(ref));
    }
  }
  for (const method of ['describe', 'set', 'unset']) {
    markRemoteMethod(CredentialsControllerService.prototype, method);
  }
  await mount(CredentialsControllerService);

  // 8. UserQuestionService (`ctx.userQuestions`) + host fallback waterfall
  await mount(UserQuestionService);
  disposers.push(root.on('user-questions/request', async (request, next) => {
    try {
      return await next();
    } catch (error) {
      if (!(error instanceof UserQuestionError) || error.code !== 'NO_PROVIDER') throw error;
      if (request.signal?.aborted) throw error;
      const reply = await requestOwner('host.userQuestions.ask', {
        sessionId: request.agent?.id ?? null,
        callId: request.wait?.callId ?? null,
        timed: request.wait?.timed === true,
        questions: request.questions,
      });
      if (reply?.aborted) {
        throw new UserQuestionError('ask_user_question was aborted before the user answered', 'ASK_ABORTED');
      }
      if (reply?.timedOut) {
        throw new UserQuestionError('ask_user_question timed out before the user answered', 'ASK_TIMED_OUT');
      }
      if (!reply || !Array.isArray(reply.answers)) throw error;
      return { answers: reply.answers };
    }
  }));

  // 9. RemoteAccess, HostConnectionService, FileUploads, TypertGatewayService, api-remotes
  // The real DSH access service: every admitted page peer is the owner (the daemon admits the page
  // before a request reaches this host), so the injected baseline and the follow stream answer owner.
  await mount(RemoteAccessService);

  const resolvedConnConfig = ConnectionConfigSchema({
    trustedHosts: Array.isArray(initialHostConfig?.trustedHosts) ? initialHostConfig.trustedHosts : [],
  });
  const gatewayBrowserAuth = {
    isAuthenticated() { return true; },
    authenticateApi() { return undefined; },
    authorizeIndex() { return true; },
    authenticatedUrl(baseUrl) { return new URL(baseUrl).href; },
    authorizeConnect(_req, res) {
      res.writeHead(302, {
        'cache-control': 'no-store',
        location: './api/remote.mux',
        'referrer-policy': 'no-referrer',
      });
      res.end();
    },
  };

  function ConnectionHostPlugin(ctx) {
    const connection = new HostConnectionService(ctx, resolvedConnConfig.trustedHosts, gatewayBrowserAuth);
    ctx.inject(['webServer'], webCtx => {
      webCtx.on('webserver/index-inject', table => {
        table.push({
          kind: 'global',
          name: '__DSH_CONNECTION_RECOVERY__',
          value: resolvedConnConfig.recovery,
        });
      });
      const fetchHandler = connection.createSharedFetchHandler(API_PATH);
      webCtx.effect(() => webCtx.webServer.register({
        kind: 'prefix',
        path: API_PATH,
        handler: async (req, res) => {
          const admission = connection.admit(req);
          if ('rejection' in admission) {
            res.writeHead(admission.rejection);
            res.end(admission.rejection === 401 ? 'unauthorized' : 'forbidden');
            return;
          }
          await webCtx.waterfall(
            'connection/request',
            req,
            res,
            () => bridgeNodeHttpToFetch(req, res, fetchHandler, admission.peer, resolvedConnConfig.maxRequestBodyBytes),
          );
        },
      }), 'client-connection: /api route');
      webCtx.effect(() => webCtx.webServer.register({
        kind: 'exact',
        path: CONNECT_PATH,
        handler: (req, res) => {
          connection.authorizeConnect(req, res);
        },
      }), 'client-connection: /connect route');
    });
  }
  await mount(ConnectionHostPlugin);
  await mount(FileUploads);
  disposers.push(root.fileUploads.registerAgentResolver(async sessionId => {
    const agent = await resolveAgentHandle(sessionId);
    if (!agent) {
      throw new RemoteError('session/not-found', `session "${sessionId}" not found`, { sessionId });
    }
    return agent;
  }));
  await mount(TypertGatewayService, {});
  await mount(apiRemotes);
  await mount(PluginRegistryProbe, {});

  // 10. PluginInventoryService (`pluginInventory/list`)
  const metaCache = new Map();
  function resolveInventoryMeta(moduleName, packages, artifacts) {
    if (!moduleName) return undefined;
    const artifact = artifacts?.[moduleName];
    const pkg = (packages ?? []).find(item => item.name === moduleName);
    const dir = pkg?.directory;
    if (!dir) return undefined;
    const cacheKey = `${moduleName}@${artifact?.version ?? pkg?.version ?? ''}:${dir}`;
    if (metaCache.has(cacheKey)) return metaCache.get(cacheKey);
    let meta;
    try {
      meta = readPluginMeta(moduleName, pathToFileURL(join(dir, 'package.json')).href);
    } catch {
      meta = undefined;
    }
    metaCache.set(cacheKey, meta);
    return meta;
  }

  class PluginInventoryService extends TypertRemoteService {
    constructor(ctx) {
      super(ctx, 'pluginInventory');
    }

    async list() {
      const { entries, packages, artifacts } = tree.inventorySnapshot();
      const agentPresets = await root.get('agentPresets')?.compositionInventory?.().catch(() => []) ?? [];
      return {
        managementAvailable: Boolean(root.get('pluginManager')),
        entries: entries.map(row => {
          const meta = resolveInventoryMeta(row.moduleName, packages, artifacts);
          return {
            ...row,
            ...(meta !== undefined ? { meta } : {}),
          };
        }),
        agentPresets,
      };
    }
  }
  markRemoteMethod(PluginInventoryService.prototype, 'list');
  await mount(PluginInventoryService);

  // 11. Session gateway: the `session` namespace answered from the daemon's session facts.
  const sessionGateway = createSessionGateway({
    root,
    requestOwner,
    hostGeneration: lifecycle.generation,
    readLog: sessionReader?.readLog,
    resolveAgent: resolveAgentHandle,
    onDiagnostic: message => root.logger.warn(message),
  });
  disposers.push(() => sessionGateway.close());
  await mount(sessionGateway.SessionController);

  return {
    async executeTool(params, signal) {
      return await executeSingleTool(params ?? {}, signal);
    },
    async preExecuteTool(params, signal) {
      return await runtimePorts.runPreExecute(params ?? {}, signal);
    },
    async postExecuteTool(params, signal) {
      return await runtimePorts.runPostExecute(params ?? {}, signal);
    },
    async assembleSystemPrompt(params, signal) {
      return await runtimePorts.assembleSystemPrompt(params ?? {}, signal);
    },
    async askApproval(params, signal) {
      return await runtimePorts.askApproval(params ?? {}, signal);
    },
    async beforeCompaction(params, signal) {
      return await runtimePorts.beforeCompaction(params ?? {}, signal);
    },
    async projectMessages(params, signal) {
      return await runtimePorts.projectMessages(params ?? {}, signal);
    },
    async extendLlmRequest(params, signal) {
      return await runtimePorts.extendLlmRequest(params ?? {}, signal);
    },
    async interceptLlmStream(params, signal) {
      return await runtimePorts.interceptLlmStream(params ?? {}, signal);
    },
    async runAgentLoop(params, signal) {
      return await executionSeams.runAgentLoop(params ?? {}, signal);
    },
    async cancelAgentLoop(params) {
      return await executionSeams.cancelAgentLoop(params ?? {});
    },
    async resumeAgentLoop(params, signal) {
      return await executionSeams.resumeAgentLoop(params ?? {}, signal);
    },
    filterVisibleTools(params) {
      return runtimePorts.filterVisibleTools(params ?? {});
    },
    hookCapabilities() {
      return runtimePorts.hookCapabilities();
    },
    async webSearch(params, signal) {
      return await root.web.search(params ?? {}, signal);
    },
    async webFetch(params, signal) {
      return await root.web.fetch(params ?? {}, signal);
    },
    listWebProviders() {
      return root.web.listProviders();
    },
    async executeCommand({ sessionId, line, attachments = [], expectedOwner } = {}, signal = new AbortController().signal) {
      const parsed = typeof line === 'string' ? parseCommand(line) : undefined;
      const owner = parsed && commandOwners.get(parsed.name);
      if (!owner || parsed.name !== expectedOwner?.name || owner.pluginId !== expectedOwner.pluginId) {
        throw new RemoteError('command/revoked', 'Plugin command is no longer registered by its owner');
      }
      const lease = lifecycle.acquire(owner);
      const callSignal = AbortSignal.any([signal, lease.signal]);
      callSignal.throwIfAborted();
      const scope = invocationScope(sessionId);
      try {
        lease.assertCurrent();
        const result = await root.commands.execute(scope.agent, line, attachments, callSignal);
        lease.assertCurrent();
        return result;
      } finally {
        await scope.dispose();
      }
    },
    async listSkills(options = {}) {
      return await root.skills.list(options);
    },
    async getSkill({ name, options = {} } = {}) {
      return (await root.skills.get(String(name ?? ''), options)) ?? null;
    },
    async syncHostModels(params) {
      await llmBridge.syncHostModels(params);
      return {};
    },
    async refreshLlmProviders() {
      return await llmBridge.refreshProviders();
    },
    describeSettings() {
      return root.settingsController.describe();
    },
    async mutateSettings({ ns, ops, expectedRevision } = {}) {
      return await root.settingsController.mutate(ns, ops, expectedRevision);
    },
    async updateSettings({ ns, patch, expectedRevision } = {}) {
      return await root.settingsController.update(ns, patch, expectedRevision);
    },
    async replaceSettings({ ns, section, expectedRevision } = {}) {
      return await root.settingsController.replace(ns, section, expectedRevision);
    },
    observeSessionChanged(params) {
      sessionReader?.observeChanged?.(params);
      sessionGateway.observeChanged(params);
      if (params?.resync) {
        void syncManagedAgentsFromDaemon();
        return;
      }
      const changes = Array.isArray(params?.changes)
        ? params.changes
        : params?.sessionId
          ? [params]
          : [];
      for (const change of changes) {
        if (!change || typeof change.sessionId !== 'string') continue;
        if (change.kind === 'removed') {
          void disposeAgentScope(change.sessionId);
        } else {
          void requestOwner('host.session.get', { sessionId: change.sessionId })
            .then(res => (res?.session ? upsertManagedAgent(res.session) : disposeAgentScope(change.sessionId)))
            .catch(() => {});
        }
      }
    },
    observeSessionCommitted(params) {
      sessionGateway.observeCommitted(params);
      runtimePorts.observeCommittedUsage(params);
    },
    observeAssistantStream(params) {
      sessionGateway.observeAssistantStream(params);
    },
    async close() {
      if (closed) return;
      closed = true;
      for (const dispose of disposers.splice(0).reverse()) {
        try { await dispose?.(); } catch { /* best-effort listener teardown */ }
      }
      for (const fiber of fibers.splice(0).reverse()) {
        try { await fiber.dispose(); } catch { /* best-effort fiber teardown */ }
      }
      await llmBridge.close();
    },
  };
}
