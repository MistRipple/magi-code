// 把 DSH LlmRuntime 上已注册的适配器暴露成仅本机、带随机 Bearer 的 OpenAI Chat 兼容端点。
// Magi 的主对话数据源只认标准协议；任何 DSH 适配器（无论它的真实上游是什么）都经这里接入，
// 适配器自身的重试、换号、推理档位与流式语义保持不变。
//
// 只实现 Magi 实际发送的请求形状（见 crates/magi-bridge-client/src/protocol/openai_chat.rs）；
// 不认识的字段与内容块明确报 400，不静默丢弃。
import { createServer } from 'node:http';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { upstreamReasoningEffort } from './reasoning.mjs';

const MAX_BODY = 32 * 1024 * 1024;
const ALLOWED_FIELDS = new Set(['model', 'messages', 'stream', 'stream_options', 'max_tokens', 'temperature', 'stop', 'tools', 'tool_choice', 'reasoning_effort', 'prompt_cache_key']);

class RequestError extends Error {
  constructor(status, code, message) { super(message); this.status = status; this.code = code; }
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (chunk) => {
      size += chunk.length;
      if (size > MAX_BODY) { reject(new RequestError(413, 'request_too_large', '请求体超过上限')); req.destroy(); return; }
      chunks.push(chunk);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

function textOf(content, where) {
  if (typeof content === 'string') return content;
  if (content === null || content === undefined) return '';
  if (!Array.isArray(content)) throw new RequestError(400, 'invalid_request', `${where} 的 content 必须是字符串或数组`);
  return content.map((part) => {
    if (part?.type === 'text' && typeof part.text === 'string') return part.text;
    throw new RequestError(400, 'unsupported_content', `${where} 含不支持的内容块：${part?.type}`);
  }).join('');
}

/**
 * OpenAI Chat 消息 → DSH 请求消息。图片需要 DSH 附件服务承载，由 attachments 回调保存。
 * @param {any[]} messages
 * @param {{ saveImage?: (dataUrl: string) => Promise<object> }} options
 */
export async function toDshMessages(messages, options = {}) {
  if (!Array.isArray(messages) || messages.length === 0) throw new RequestError(400, 'invalid_request', 'messages 必须是非空数组');
  const out = [];
  let system = [];
  for (const [index, message] of messages.entries()) {
    const where = `messages[${index}]`;
    switch (message?.role) {
      case 'system':
      case 'developer':
        system.push(textOf(message.content, where));
        break;
      case 'user': {
        const blocks = [];
        if (typeof message.content === 'string') blocks.push({ type: 'text', text: message.content });
        else if (Array.isArray(message.content)) {
          for (const part of message.content) {
            if (part?.type === 'text') blocks.push({ type: 'text', text: part.text });
            else if (part?.type === 'image_url') {
              if (!options.saveImage) throw new RequestError(400, 'unsupported_content', '该模型桥没有附件服务，不能接收图片');
              blocks.push({ type: 'image', attachment: await options.saveImage(part.image_url?.url) });
            } else throw new RequestError(400, 'unsupported_content', `${where} 含不支持的内容块：${part?.type}`);
          }
        } else throw new RequestError(400, 'invalid_request', `${where} 的 content 无效`);
        out.push({ role: 'user', content: blocks });
        break;
      }
      case 'assistant': {
        const blocks = [];
        const reasoning = message.reasoning_content ?? message.reasoning_text ?? message.reasoning;
        if (typeof reasoning === 'string' && reasoning) blocks.push({ type: 'reasoning', text: reasoning });
        const text = textOf(message.content, where);
        if (text) blocks.push({ type: 'text', text });
        for (const call of message.tool_calls ?? []) {
          if (call?.type !== 'function') throw new RequestError(400, 'invalid_request', `${where} 含不支持的工具调用类型`);
          blocks.push({ type: 'tool-call', id: call.id, name: call.function.name, arguments: call.function.arguments ?? '' });
        }
        out.push({ role: 'assistant', content: blocks, source: { kind: 'model', provider: 'magi', model: 'magi' } });
        break;
      }
      case 'tool':
        out.push({ role: 'tool', content: [{ type: 'text', text: textOf(message.content, where) }], toolCallId: message.tool_call_id, source: { kind: 'tool', callId: message.tool_call_id } });
        break;
      default:
        throw new RequestError(400, 'invalid_request', `${where} 的 role 不受支持：${message?.role}`);
    }
  }
  return { messages: out, system: system.length ? system.join('\n\n') : undefined };
}

function finishReasonOf(reason) {
  switch (reason?.kind) {
    case 'stop': return 'stop';
    case 'tool-calls': return 'tool_calls';
    case 'max-tokens': return 'length';
    default: return null;
  }
}

/**
 * 把 DSH 流块转成 OpenAI Chat SSE 块。每次调用只服务一个响应。
 * @returns {{ push: (chunk: any) => object[], error: Error|null }}
 */
export function createChunkTranslator(model) {
  const id = `chatcmpl-${randomBytes(8).toString('hex')}`;
  const toolIndex = new Map();
  const base = () => ({ id, object: 'chat.completion.chunk', created: Math.floor(Date.now() / 1000), model });
  const state = { error: null };
  state.push = (chunk) => {
    switch (chunk.type) {
      case 'text-delta':
        return [{ ...base(), choices: [{ index: 0, delta: { content: chunk.text }, finish_reason: null }] }];
      case 'reasoning-delta':
        return [{ ...base(), choices: [{ index: 0, delta: { reasoning_content: chunk.text }, finish_reason: null }] }];
      case 'tool-call-delta': {
        let index = toolIndex.get(chunk.id);
        const first = index === undefined;
        if (first) { index = toolIndex.size; toolIndex.set(chunk.id, index); }
        const call = { index, function: { arguments: chunk.argumentsDelta ?? '' } };
        if (first) { call.id = chunk.id; call.type = 'function'; }
        if (chunk.name) call.function.name = chunk.name;
        return [{ ...base(), choices: [{ index: 0, delta: { tool_calls: [call] }, finish_reason: null }] }];
      }
      case 'usage': {
        const usage = chunk.usage;
        // DSH 的三类输入互不重叠；OpenAI prompt_tokens 是完整输入。
        // 保留实际零值与缺席，不能把缓存量从 inputTokens 再扣一次。
        const prompt = usage.inputTokens + (usage.cacheReadTokens ?? 0) + (usage.cacheWriteTokens ?? 0);
        const out = { prompt_tokens: prompt, completion_tokens: usage.outputTokens, total_tokens: usage.totalTokens ?? prompt + usage.outputTokens };
        if (usage.cacheReadTokens !== undefined) out.prompt_tokens_details = { cached_tokens: usage.cacheReadTokens };
        if (usage.cacheWriteTokens !== undefined) out.cache_creation_input_tokens = usage.cacheWriteTokens;
        if (usage.reasoningTokens !== undefined) out.completion_tokens_details = { reasoning_tokens: usage.reasoningTokens };
        return [{ ...base(), choices: [], usage: out }];
      }
      case 'finish': {
        if (chunk.reason?.kind === 'error' || chunk.reason?.kind === 'aborted') {
          const failure = chunk.reason.failure ?? {};
          state.error = Object.assign(new Error(failure.message || '上游失败'), { code: failure.code, status: failure.status });
          return [];
        }
        return [{ ...base(), choices: [{ index: 0, delta: {}, finish_reason: finishReasonOf(chunk.reason) }] }];
      }
      default:
        return []; // block-start / block-end 只是组装信号，OpenAI 流不需要。
    }
  };
  return state;
}

/**
 * @param {{ llm: any, provider: string, saveImage?: Function, log?: (m: string) => void }} options
 * @returns {Promise<{ baseUrl: string, apiKey: string, close: () => Promise<void> }>}
 */
export async function startOpenAiBridge(options) {
  const { llm, provider } = options;
  const secret = randomBytes(32).toString('base64url');
  const expected = Buffer.from(`Bearer ${secret}`);
  const authorized = (header) => {
    if (typeof header !== 'string') return false;
    const presented = Buffer.from(header.trim());
    return presented.length === expected.length && timingSafeEqual(presented, expected);
  };
  const fail = (res, status, code, message) => {
    if (res.destroyed || res.writableEnded) return;
    const error = { message, type: code, code };
    if (res.headersSent) { res.end(`data: ${JSON.stringify({ error })}\n\n`); return; }
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ error }));
  };
  const serve = async (req, res) => {
    const controller = new AbortController();
    res.on('close', () => { if (!res.writableFinished) controller.abort(); });
    try {
      if (!authorized(req.headers.authorization)) return fail(res, 401, 'unauthorized', '缺少或错误的 Bearer');
      const url = (req.url ?? '/').replace(/\/+$/, '');
      if (req.method === 'GET' && url === '/v1/models') {
        const models = await llm.listModels(provider);
        res.writeHead(200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ object: 'list', data: models.map((m) => ({ id: m.id, object: 'model', created: 0, owned_by: provider })) }));
      }
      if (req.method !== 'POST' || url !== '/v1/chat/completions') return fail(res, 404, 'not_found', `${req.method} ${url}`);
      let body;
      try { body = JSON.parse(await readBody(req)); } catch (error) {
        if (error instanceof RequestError) throw error;
        throw new RequestError(400, 'invalid_json', '请求体不是 JSON');
      }
      for (const key of Object.keys(body)) if (!ALLOWED_FIELDS.has(key)) throw new RequestError(400, 'unsupported_field', `不支持的请求字段：${key}`);
      if (typeof body.model !== 'string' || !body.model) throw new RequestError(400, 'invalid_request', 'model 必填');
      const { messages, system } = await toDshMessages(body.messages, { saveImage: options.saveImage });
      const tools = (body.tools ?? []).map((tool) => {
        if (tool?.type !== 'function') throw new RequestError(400, 'unsupported_field', '只支持 function 工具');
        return { name: tool.function.name, description: tool.function.description ?? '', parameters: tool.function.parameters ?? { type: 'object' } };
      });
      if (body.tool_choice !== undefined && body.tool_choice !== 'auto' && tools.length > 0) {
        throw new RequestError(400, 'unsupported_field', 'DSH 适配器不支持强制 tool_choice');
      }
      let reasoningEffort;
      if (body.reasoning_effort !== undefined) {
        const info = await llm.resolveModelInfo(provider, body.model, controller.signal);
        try { reasoningEffort = upstreamReasoningEffort((info.reasoning?.efforts ?? []).map(effort => effort.id), body.reasoning_effort); }
        catch (error) { throw new RequestError(400, 'invalid_request', error.message); }
      }
      const request = {
        provider, model: body.model, messages, signal: controller.signal,
        ...(system === undefined ? {} : { system }),
        ...(tools.length ? { tools } : {}),
        ...(body.max_tokens === undefined ? {} : { maxTokens: body.max_tokens }),
        ...(body.temperature === undefined ? {} : { temperature: body.temperature }),
        ...(body.stop === undefined ? {} : { stop: body.stop }),
        ...(reasoningEffort === undefined ? {} : { reasoningEffort }),
      };
      const translator = createChunkTranslator(body.model);
      if (body.stream !== true) throw new RequestError(400, 'unsupported_field', '模型桥只支持流式请求（stream: true）');
      // 正文与用量立即交付；成功终态等待上游迭代器正常退出，错误退出自动回收它。
      let terminal;
      let started = false;
      const start = () => {
        if (started) return;
        started = true;
        res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' });
      };
      for await (const chunk of llm.stream(request)) {
        controller.signal.throwIfAborted();
        const out = translator.push(chunk);
        if (translator.error) {
          const error = translator.error;
          const status = Number.isInteger(error.status) && error.status >= 400 && error.status <= 599 ? error.status : 502;
          throw new RequestError(status, error.code ?? 'upstream_error', error.message);
        }
        if (chunk.type === 'finish') {
          terminal = out[0];
          if (!terminal?.choices[0].finish_reason) throw new RequestError(502, 'invalid_finish', '上游返回未知结束原因');
          continue;
        }
        for (const item of out) { start(); res.write(`data: ${JSON.stringify(item)}\n\n`); }
      }
      controller.signal.throwIfAborted();
      if (!terminal) throw new RequestError(502, 'incomplete_stream', '上游流缺少结束信号');
      start();
      res.end(`data: ${JSON.stringify(terminal)}\n\ndata: [DONE]\n\n`);
    } catch (error) {
      if (error instanceof RequestError) return fail(res, error.status, error.code, error.message);
      options.log?.(`模型桥内部错误：${error?.stack ?? error}`);
      fail(res, 500, 'internal', String(error?.message ?? error));
    }
  };
  const server = createServer((req, res) => {
    const run = options.withRequest ?? ((callback) => callback());
    void Promise.resolve(run(() => serve(req, res))).catch((error) => fail(res, 500, 'internal', String(error?.message ?? error)));
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const { port } = server.address();
  return {
    baseUrl: `http://127.0.0.1:${port}/v1`,
    apiKey: secret,
    close: () => new Promise((resolve) => { server.close(() => resolve()); server.closeAllConnections(); }),
  };
}
