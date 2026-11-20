// Bidirectional bridge between DSH ctx.llm / ctx.agentDefaultModel and Magi model catalog.
import http from 'node:http';
import { randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import {
  BlockAssembler,
  LlmAdapter,
  LlmError,
  LlmRuntime,
  ReasoningEffortId,
  ToolCallId,
} from '@deepseek-ai/dsh-llm';
import { Context } from '@deepseek-ai/cordis';
import { deepEqual, updateVolatile, volatileEntries } from '@deepseek-ai/cosmokit';
import { AgentDefaultModelConfig } from '@deepseek-ai/dsh-agent-default-model';

const VALID_MAGI_EFFORTS = new Set(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']);
const MIN_CONTEXT_WINDOW = 4096;
const MAX_CONTEXT_WINDOW = 10_000_000;

function sameBearer(authHeader, expectedToken) {
  if (typeof authHeader !== 'string' || !authHeader.startsWith('Bearer ')) return false;
  const candidate = Buffer.from(authHeader.slice('Bearer '.length), 'utf8');
  const expected = Buffer.from(expectedToken, 'utf8');
  return candidate.byteLength === expected.byteLength && timingSafeEqual(candidate, expected);
}

function parseDataUrl(url) {
  const match = /^data:(image\/(?:png|jpeg|webp|gif));base64,([A-Za-z0-9+/=]+)$/.exec(url);
  if (!match) return null;
  return { mediaType: match[1], data: new Uint8Array(Buffer.from(match[2], 'base64')) };
}

async function convertOpenAiMessagesToDsh(rawMessages, attachments) {
  let systemText = '';
  const messages = [];
  for (const msg of Array.isArray(rawMessages) ? rawMessages : []) {
    if (!msg || typeof msg !== 'object') continue;
    const role = String(msg.role ?? 'user');
    if (role === 'system') {
      const part = typeof msg.content === 'string'
        ? msg.content
        : Array.isArray(msg.content)
          ? msg.content.filter(b => b?.type === 'text' && typeof b.text === 'string').map(b => b.text).join('\n\n')
          : '';
      if (part) systemText = systemText ? `${systemText}\n\n${part}` : part;
      continue;
    }
    if (role === 'tool') {
      const text = typeof msg.content === 'string' ? msg.content : JSON.stringify(msg.content ?? '');
      const callId = ToolCallId(String(msg.tool_call_id ?? randomUUID()));
      messages.push({
        id: randomUUID(),
        role: 'tool',
        toolCallId: callId,
        isError: false,
        content: [{ type: 'text', text }],
        source: { kind: 'tool', callId },
      });
      continue;
    }
    if (role === 'assistant') {
      const blocks = [];
      if (typeof msg.reasoning_content === 'string' && msg.reasoning_content.length > 0) {
        blocks.push({ type: 'reasoning', text: msg.reasoning_content });
      }
      if (typeof msg.content === 'string' && msg.content.length > 0) {
        blocks.push({ type: 'text', text: msg.content });
      } else if (Array.isArray(msg.content)) {
        for (const part of msg.content) {
          if (part?.type === 'text' && typeof part.text === 'string' && part.text.length > 0) {
            blocks.push({ type: 'text', text: part.text });
          }
        }
      }
      if (Array.isArray(msg.tool_calls)) {
        for (const call of msg.tool_calls) {
          if (call?.function?.name) {
            blocks.push({
              type: 'tool-call',
              id: ToolCallId(String(call.id ?? randomUUID())),
              name: String(call.function.name),
              arguments: String(call.function.arguments ?? '{}'),
            });
          }
        }
      }
      if (blocks.length === 0) blocks.push({ type: 'text', text: '' });
      messages.push({
        id: randomUUID(),
        role: 'assistant',
        content: blocks,
        source: { kind: 'model', provider: 'magi-bridge', model: 'history' },
      });
      continue;
    }
    const effectiveRole = role === 'developer' ? 'developer' : 'user';
    const blocks = [];
    if (typeof msg.content === 'string') {
      blocks.push({ type: 'text', text: msg.content });
    } else if (Array.isArray(msg.content)) {
      for (const part of msg.content) {
        if (part?.type === 'text' && typeof part.text === 'string') {
          blocks.push({ type: 'text', text: part.text });
        } else if (part?.type === 'image_url' && typeof part.image_url?.url === 'string' && attachments) {
          const decoded = parseDataUrl(part.image_url.url);
          if (decoded) {
            const [ref] = await attachments.saveImages([decoded]);
            blocks.push({ type: 'image', attachment: ref });
          }
        }
      }
    }
    if (blocks.length === 0) blocks.push({ type: 'text', text: '' });
    messages.push({
      id: randomUUID(),
      role: effectiveRole,
      content: blocks,
      source: { kind: 'user' },
    });
  }
  return { system: systemText || undefined, messages };
}

function convertOpenAiToolsToDsh(rawTools) {
  if (!Array.isArray(rawTools) || rawTools.length === 0) return undefined;
  const tools = [];
  for (const item of rawTools) {
    const fn = item?.type === 'function' ? item.function : item;
    if (!fn || typeof fn.name !== 'string' || !fn.name) continue;
    tools.push({
      name: fn.name,
      description: typeof fn.description === 'string' ? fn.description : '',
      parameters: fn.parameters && typeof fn.parameters === 'object' ? fn.parameters : { type: 'object', properties: {} },
    });
  }
  return tools.length ? tools : undefined;
}

function mapFinishReason(kind) {
  switch (kind) {
    case 'tool-use': return 'tool_calls';
    case 'max-tokens': return 'length';
    default: return 'stop';
  }
}

export async function openLlmBridge({ lifecycle, root, initialHostConfig, requestOwner, notifyOwner }) {
  const bridgeToken = randomBytes(32).toString('hex');
  const providerOwners = new Map();
  let hostRoutes = new Map();
  let hostAdapterHandle;
  let syncQueued = false;
  let closed = false;

  class MagiLlmRuntime extends LlmRuntime {
    registerAdapter(providers, adapter) {
      const caller = root.reflect.trace(this.ctx);
      const ownerId = lifecycle.ownerIdentity(caller);
      const handle = super.registerAdapter(providers, adapter);
      if (ownerId) {
        for (const p of providers) providerOwners.set(p, ownerId);
        const origDispose = handle;
        const origReplace = handle.replace.bind(handle);
        const wrapped = () => {
          for (const [prov, rec] of providerOwners) {
            if (rec === ownerId) providerOwners.delete(prov);
          }
          origDispose();
        };
        wrapped.replace = next => {
          for (const [prov, rec] of providerOwners) {
            if (rec === ownerId) providerOwners.delete(prov);
          }
          origReplace(next);
          for (const p of next) providerOwners.set(p, ownerId);
        };
        return wrapped;
      }
      return handle;
    }
  }

  const llmFiber = root.plugin(MagiLlmRuntime);
  await llmFiber.await();
  const llm = root.llm;

  let hostDefaultModelEntry;
  class MagiAgentDefaultModelConfig extends AgentDefaultModelConfig {
    async saveSelection(next) {
      if (!hostDefaultModelEntry) return;
      const editor = this.ctx.get('configEditor');
      if (!editor) return;
      const config = {
        provider: next.provider,
        model: next.model,
        ...(next.reasoningEffort === undefined ? {} : { reasoningEffort: String(next.reasoningEffort) }),
      };
      const saved = this.saves.then(() => editor.edit(hostDefaultModelEntry, () => config));
      this.saves = saved.catch(() => {});
      await saved;
    }
  }

  const initialDefault = initialHostConfig?.agentDefaultModel ?? {};
  const defaultModelConfig = {
    provider: typeof initialDefault.provider === 'string' && initialDefault.provider ? initialDefault.provider : 'magi',
    model: typeof initialDefault.model === 'string' && initialDefault.model ? initialDefault.model : 'default',
    ...(typeof initialDefault.reasoningEffort === 'string' && initialDefault.reasoningEffort
      ? { reasoningEffort: initialDefault.reasoningEffort } : {}),
  };
  const agentDefaultModelFiber = root.plugin(MagiAgentDefaultModelConfig, defaultModelConfig);
  await agentDefaultModelFiber.await();

  class HostMagiAdapter extends LlmAdapter {
    providerInfo(provider) {
      const entry = hostRoutes.get(provider);
      return { id: provider, name: entry?.name ?? provider };
    }

    async listModels(provider) {
      const entry = hostRoutes.get(provider);
      if (!entry) return [];
      return (entry.models ?? []).map(m => ({
        provider,
        id: m.id,
        name: m.displayName || m.id,
        ...(Array.isArray(m.inputModalities) ? { inputModalities: [...m.inputModalities] } : {}),
      }));
    }

    async resolveModel(provider, model) {
      const entry = hostRoutes.get(provider);
      const found = entry?.models?.find(m => m.id === model);
      return {
        provider,
        id: model,
        name: found?.displayName || model,
        ...(found?.contextWindowTokens ? { context: { contextWindow: found.contextWindowTokens } } : {}),
        ...(Array.isArray(found?.inputModalities) ? { inputModalities: [...found.inputModalities] } : {}),
      };
    }

    async *stream(options) {
      options.signal?.throwIfAborted();
      const serializedMessages = (options.messages ?? []).map(msg => {
        const texts = [];
        const images = [];
        const toolCalls = [];
        for (const block of msg.content ?? []) {
          if (block.type === 'text' && typeof block.text === 'string') texts.push(block.text);
          else if (block.type === 'tool-call') {
            toolCalls.push({
              id: String(block.id),
              type: 'function',
              function: { name: block.name, arguments: block.arguments },
            });
          }
        }
        return {
          role: msg.role,
          content: texts.join('\n\n'),
          images,
          toolCalls,
          ...(msg.role === 'tool' && msg.toolCallId ? { toolCallId: String(msg.toolCallId) } : {}),
        };
      });
      if (typeof options.system === 'string' && options.system.length > 0) {
        serializedMessages.unshift({
          role: 'system',
          content: options.system,
          images: [],
          toolCalls: [],
        });
      }
      const response = await requestOwner('host.llm.invoke', {
        provider: options.provider,
        model: options.model,
        messages: serializedMessages,
        ...(options.maxTokens !== undefined ? { maxOutputTokens: options.maxTokens } : {}),
        ...(options.temperature !== undefined ? { temperature: options.temperature } : {}),
        ...(options.stop !== undefined ? { stop: options.stop } : {}),
        ...(options.reasoningEffort !== undefined ? { reasoningEffort: String(options.reasoningEffort) } : {}),
      });
      options.signal?.throwIfAborted();
      if (response?.error) {
        throw new LlmError(String(response.error), 'SERVER');
      }
      let index = 0;
      if (typeof response?.reasoning === 'string' && response.reasoning.length > 0) {
        yield { type: 'block-start', index, blockType: 'reasoning' };
        yield { type: 'reasoning-delta', index, text: response.reasoning };
        yield { type: 'block-end', index, block: { type: 'reasoning', text: response.reasoning } };
        index += 1;
      }
      const text = typeof response?.content === 'string' ? response.content : '';
      if (text.length > 0) {
        yield { type: 'block-start', index, blockType: 'text' };
        yield { type: 'text-delta', index, text };
        yield { type: 'block-end', index, block: { type: 'text', text } };
        index += 1;
      }
      const toolCalls = Array.isArray(response?.toolCalls) ? response.toolCalls : [];
      for (const call of toolCalls) {
        const id = ToolCallId(String(call.id ?? randomUUID()));
        const argsText = typeof call.arguments === 'string' ? call.arguments : JSON.stringify(call.arguments ?? {});
        yield { type: 'block-start', index, blockType: 'tool-call' };
        yield { type: 'tool-call-delta', index, id, name: String(call.name ?? ''), argumentsDelta: argsText };
        yield { type: 'block-end', index, block: { type: 'tool-call', id, name: String(call.name ?? ''), arguments: argsText } };
        index += 1;
      }
      if (response?.usage) {
        yield {
          type: 'usage',
          usage: {
            inputTokens: Number(response.usage.inputTokens ?? 0),
            outputTokens: Number(response.usage.outputTokens ?? 0),
          },
        };
      }
      yield {
        type: 'finish',
        reason: { kind: toolCalls.length > 0 ? 'tool-use' : 'stop' },
      };
    }
  }

  const hostAdapter = new HostMagiAdapter();

  function applyHostRoutes(sources = []) {
    const next = new Map();
    next.set('magi', { name: 'Magi Default', models: [] });
    for (const src of sources) {
      if (!src || typeof src.provider !== 'string' || !src.provider) continue;
      if (providerOwners.has(src.provider)) continue;
      next.set(src.provider, {
        name: typeof src.name === 'string' && src.name ? src.name : src.provider,
        models: Array.isArray(src.models) ? src.models : [],
      });
    }
    hostRoutes = next;
    const keys = [...hostRoutes.keys()];
    if (!hostAdapterHandle) {
      hostAdapterHandle = llm.registerAdapter(keys, hostAdapter);
    } else {
      hostAdapterHandle.replace(keys);
    }
  }

  applyHostRoutes(initialHostConfig?.hostModelSources ?? []);

  const server = http.createServer(async (req, res) => {
    try {
      if (!sameBearer(req.headers.authorization, bridgeToken)) {
        res.writeHead(401, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: { message: 'unauthorized' } }));
        return;
      }
      const url = new URL(req.url ?? '/', 'http://127.0.0.1');
      const parts = url.pathname.split('/').filter(Boolean);
      const provider = parts[0] ? decodeURIComponent(parts[0]) : '';
      const tail = parts.slice(1).join('/');
      if (!provider || !providerOwners.has(provider)) {
        res.writeHead(404, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: { message: `unknown plugin provider: ${provider}` } }));
        return;
      }
      if (req.method === 'GET' && (tail === 'models' || tail === 'v1/models')) {
        const models = await llm.listModels(provider);
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({
          object: 'list',
          data: models.map(m => ({ id: m.id, object: 'model', owned_by: provider })),
        }));
        return;
      }
      if (req.method !== 'POST' || (tail !== 'chat/completions' && tail !== 'v1/chat/completions')) {
        res.writeHead(404, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: { message: 'not found' } }));
        return;
      }
      const chunks = [];
      let total = 0;
      for await (const chunk of req) {
        total += chunk.byteLength;
        if (total > 64 * 1024 * 1024) {
          res.writeHead(413, { 'content-type': 'application/json' });
          res.end(JSON.stringify({ error: { message: 'payload too large' } }));
          return;
        }
        chunks.push(chunk);
      }
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}');
      const abort = new AbortController();
      res.on('close', () => {
        if (!res.writableEnded) abort.abort(new Error('Client disconnected'));
      });
      const model = String(body.model ?? '');
      const { system, messages } = await convertOpenAiMessagesToDsh(body.messages, root.get('attachments'));
      const tools = convertOpenAiToolsToDsh(body.tools);
      const maxTokens = Number.isSafeInteger(body.max_completion_tokens)
        ? body.max_completion_tokens
        : Number.isSafeInteger(body.max_tokens) ? body.max_tokens : undefined;
      const callConfig = {
        provider,
        model,
        ...(typeof body.temperature === 'number' ? { temperature: body.temperature } : {}),
        ...(maxTokens !== undefined ? { maxTokens } : {}),
        ...(Array.isArray(body.stop) ? { stop: body.stop.map(String) } : typeof body.stop === 'string' ? { stop: [body.stop] } : {}),
        ...(typeof body.reasoning_effort === 'string' && body.reasoning_effort && body.reasoning_effort !== 'none'
          ? { reasoningEffort: ReasoningEffortId(body.reasoning_effort) } : {}),
      };
      const prepared = await llm.prepareCall(callConfig, abort.signal);
      const streamOptions = {
        ...prepared.config,
        messages,
        ...(system !== undefined ? { system } : {}),
        ...(tools !== undefined ? { tools } : {}),
        signal: abort.signal,
      };
      const completionId = `chatcmpl-${randomUUID()}`;
      const created = Math.floor(Date.now() / 1000);

      if (body.stream === true) {
        let headersSent = false;
        const sendSse = payload => {
          if (!headersSent) {
            headersSent = true;
            res.writeHead(200, {
              'content-type': 'text/event-stream; charset=utf-8',
              'cache-control': 'no-cache',
              connection: 'close',
            });
          }
          res.write(`data: ${JSON.stringify(payload)}\n\n`);
        };
        let toolOrdinal = -1;
        const toolIndices = new Map();
        for await (const chunk of prepared.stream(streamOptions)) {
          if (chunk.type === 'text-delta') {
            sendSse({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model,
              choices: [{ index: 0, delta: { content: chunk.text }, finish_reason: null }],
            });
          } else if (chunk.type === 'reasoning-delta') {
            sendSse({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model,
              choices: [{ index: 0, delta: { reasoning_content: chunk.text }, finish_reason: null }],
            });
          } else if (chunk.type === 'tool-call-delta') {
            let tcIdx = toolIndices.get(chunk.index);
            const isFirst = tcIdx === undefined;
            if (isFirst) {
              toolOrdinal += 1;
              tcIdx = toolOrdinal;
              toolIndices.set(chunk.index, tcIdx);
            }
            sendSse({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model,
              choices: [{
                index: 0,
                delta: {
                  tool_calls: [{
                    index: tcIdx,
                    ...(isFirst ? { id: String(chunk.id), type: 'function' } : {}),
                    function: {
                      ...(chunk.name ? { name: chunk.name } : {}),
                      arguments: chunk.argumentsDelta ?? '',
                    },
                  }],
                },
                finish_reason: null,
              }],
            });
          } else if (chunk.type === 'usage') {
            sendSse({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model,
              choices: [],
              usage: {
                prompt_tokens: chunk.usage.inputTokens,
                completion_tokens: chunk.usage.outputTokens,
                total_tokens: chunk.usage.inputTokens + chunk.usage.outputTokens,
              },
            });
          } else if (chunk.type === 'finish') {
            if (chunk.reason.kind === 'error' || chunk.reason.kind === 'aborted') {
              const msg = chunk.reason.failure?.message ?? 'Plugin LLM stream failed';
              if (!headersSent) {
                res.writeHead(502, { 'content-type': 'application/json' });
                res.end(JSON.stringify({ error: { message: msg, code: chunk.reason.failure?.code } }));
                return;
              }
              res.write(`data: ${JSON.stringify({ error: { message: msg, code: chunk.reason.failure?.code } })}\n\n`);
              res.end();
              return;
            }
            sendSse({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model,
              choices: [{ index: 0, delta: {}, finish_reason: mapFinishReason(chunk.reason.kind) }],
            });
          }
        }
        if (!headersSent) {
          res.writeHead(200, { 'content-type': 'text/event-stream; charset=utf-8' });
        }
        res.write('data: [DONE]\n\n');
        res.end();
        return;
      }

      const assembler = new BlockAssembler();
      for await (const chunk of prepared.stream(streamOptions)) {
        assembler.push(chunk);
        if (chunk.type === 'finish' && (chunk.reason.kind === 'error' || chunk.reason.kind === 'aborted')) {
          res.writeHead(502, { 'content-type': 'application/json' });
          res.end(JSON.stringify({
            error: { message: chunk.reason.failure?.message ?? 'Plugin LLM call failed', code: chunk.reason.failure?.code },
          }));
          return;
        }
      }
      const assembled = assembler.assembled();
      const textParts = [];
      const reasoningParts = [];
      const toolCalls = [];
      for (const block of assembled.blocks) {
        if (block.type === 'text') textParts.push(block.text);
        else if (block.type === 'reasoning') reasoningParts.push(block.text);
        else if (block.type === 'tool-call') {
          toolCalls.push({
            id: String(block.id),
            type: 'function',
            function: { name: block.name, arguments: block.arguments },
          });
        }
      }
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({
        id: completionId,
        object: 'chat.completion',
        created,
        model,
        choices: [{
          index: 0,
          message: {
            role: 'assistant',
            content: textParts.join(''),
            ...(reasoningParts.length ? { reasoning_content: reasoningParts.join('') } : {}),
            ...(toolCalls.length ? { tool_calls: toolCalls } : {}),
          },
          finish_reason: mapFinishReason(assembler.finish?.kind ?? 'stop'),
        }],
        ...(assembler._usage ? {
          usage: {
            prompt_tokens: assembler._usage.inputTokens,
            completion_tokens: assembler._usage.outputTokens,
            total_tokens: assembler._usage.inputTokens + assembler._usage.outputTokens,
          },
        } : {}),
      }));
    } catch (error) {
      if (!res.headersSent) {
        res.writeHead(500, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: { message: error instanceof Error ? error.message : String(error) } }));
      } else {
        res.destroy();
      }
    }
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      server.off('error', reject);
      resolve();
    });
  });
  const port = server.address().port;

  async function publishPluginProviders() {
    if (closed) return;
    const providers = [];
    for (const info of llm.listProviders()) {
      const ownerId = providerOwners.get(info.id);
      if (!ownerId) continue;
      let catalog = [];
      try {
        catalog = await llm.listModels(info.id);
      } catch {
        catalog = [];
      }
      const models = [];
      for (const item of catalog) {
        let detailed;
        try {
          detailed = await llm.resolveModelInfo(info.id, item.id);
        } catch {
          detailed = undefined;
        }
        const rawWindow = detailed?.context?.contextWindow;
        const contextWindowTokens = Number.isSafeInteger(rawWindow)
          && rawWindow >= MIN_CONTEXT_WINDOW && rawWindow <= MAX_CONTEXT_WINDOW
          ? rawWindow : undefined;
        const modalitiesRaw = detailed?.inputModalities ?? item.inputModalities;
        const inputModalities = Array.isArray(modalitiesRaw) && modalitiesRaw.includes('text')
          ? [...new Set(modalitiesRaw.filter(m => m === 'text' || m === 'image'))]
          : undefined;
        let reasoning;
        if (detailed?.reasoning?.efforts?.length) {
          const supportedEfforts = detailed.reasoning.efforts
            .map(e => String(e.id))
            .filter(e => VALID_MAGI_EFFORTS.has(e));
          if (supportedEfforts.length > 0) {
            const def = detailed.reasoning.defaultEffort && supportedEfforts.includes(String(detailed.reasoning.defaultEffort))
              ? String(detailed.reasoning.defaultEffort) : undefined;
            reasoning = {
              supportedEfforts,
              ...(def ? { defaultEffort: def } : {}),
              transport: 'openai',
              source: 'plugin',
            };
          }
        }
        models.push({
          id: item.id,
          displayName: item.name || item.id,
          ...(contextWindowTokens !== undefined ? { contextWindowTokens } : {}),
          ...(inputModalities !== undefined ? { inputModalities } : {}),
          ...(reasoning !== undefined ? { reasoning } : {}),
        });
      }
      providers.push({
        pluginId: ownerId.pluginId,
        instanceId: ownerId.instanceId,
        provider: info.id,
        name: info.name,
        baseUrl: `http://127.0.0.1:${port}/${encodeURIComponent(info.id)}/v1`,
        apiKey: bridgeToken,
        apiProtocol: 'openai_chat',
        models,
      });
    }
    if (!closed) {
      notifyOwner('host.llm.providersChanged', { providers });
    }
  }

  function schedulePublish() {
    if (syncQueued || closed) return;
    syncQueued = true;
    queueMicrotask(() => {
      syncQueued = false;
      void publishPluginProviders().catch(() => {});
    });
  }

  const offAdapters = root.on('llm/adapters-updated', schedulePublish);
  const offModels = root.on('llm/models-updated', schedulePublish);

  return {
    agentDefaultModelFiber,
    defaultModelConfig: structuredClone(defaultModelConfig),
    bindDefaultModelEntry(entry) {
      hostDefaultModelEntry = entry;
    },
    async syncHostModels({ defaultSelection, sources } = {}) {
      applyHostRoutes(Array.isArray(sources) ? sources : []);
      if (defaultSelection && typeof defaultSelection.provider === 'string' && defaultSelection.provider
        && typeof defaultSelection.model === 'string' && defaultSelection.model) {
        const next = {
          provider: defaultSelection.provider,
          model: defaultSelection.model,
          ...(typeof defaultSelection.reasoningEffort === 'string' && defaultSelection.reasoningEffort
            ? { reasoningEffort: defaultSelection.reasoningEffort } : {}),
        };
        const rawFiber = agentDefaultModelFiber.ctx.fiber;
        const candidate = rawFiber.resolveConfig(next);
        const refs = volatileEntries(rawFiber.effectiveConfig);
        const paths = refs.flatMap(({ path, ref }) => {
          const source = path.reduce((value, key) => Reflect.get(value, key), candidate);
          if (deepEqual(ref.get(), source.get(), true)) return [];
          updateVolatile(ref, source);
          return [path];
        });
        rawFiber.config = structuredClone(next);
        if (hostDefaultModelEntry) {
          hostDefaultModelEntry.options.config = structuredClone(next);
          hostDefaultModelEntry.override = structuredClone(next);
        }
        if (paths.length) {
          const target = Object.create(rawFiber.ctx);
          target[Context.filter] = owner => owner.fiber === rawFiber;
          rawFiber.ctx.emit(target, 'loader/volatile-update', paths);
        }
        root.get('settings')?.describe();
      }
    },
    async refreshProviders() {
      await publishPluginProviders();
      return { ok: true };
    },
    async close() {
      closed = true;
      offAdapters();
      offModels();
      hostAdapterHandle?.();
      await new Promise(resolve => server.close(() => resolve()));
      await agentDefaultModelFiber.dispose();
      await llmFiber.dispose();
    },
  };
}
