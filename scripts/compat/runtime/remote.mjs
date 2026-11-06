// 插件私有的 Typert JSON unary 适配。服务、装饰器和 effect 仍由真实 Cordis 拥有；
// 传输复用 Magi 已授权的 route.call，不启动 DSH Gateway、Session 或第二条权限入口。
import ts from 'typescript';
import { isRemoteJsonValue, isTypertRemoteSegment, remoteErrorOf, remoteMethods } from '@deepseek-ai/dsh-typert-protocol';

export const REMOTE_ROUTE = '/__magi_compat/remote';
const fail = (code, message) => Object.assign(new Error(message), { code });
const parameters = new WeakMap();

// 对齐上游 SRC fallback 的简单具名参数；不猜测解构、默认参数、rest 或 Host 对象。
function parameterNames(fn) {
  if (parameters.has(fn)) return parameters.get(fn);
  const source = ts.createSourceFile('remote.js', `({${Function.prototype.toString.call(fn)}})`, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const expression = source.statements[0]?.expression?.expression;
  const method = expression && ts.isObjectLiteralExpression(expression) ? expression.properties[0] : undefined;
  if (source.parseDiagnostics.length || !method || !ts.isMethodDeclaration(method)) throw fail('gateway/unsupported', 'Remote 方法缺少可验证的具名参数');
  const names = method.parameters.map((parameter) => {
    if (!ts.isIdentifier(parameter.name) || parameter.dotDotDotToken || parameter.initializer) throw fail('gateway/unsupported', 'Remote 不支持未生成契约的解构、默认值或 rest 参数');
    return parameter.name.text;
  });
  if (new Set(names).size !== names.length || names.some((name) => ['agent', 'session'].includes(name))) throw fail('gateway/unsupported', 'Remote Host 对象必须经过 Magi 原生服务，不能用 JSON 冒充');
  parameters.set(fn, names);
  return names;
}

function resolveMethod(ctx, endpoint) {
  const segments = typeof endpoint === 'string' ? endpoint.split('/') : [];
  if (segments.length !== 2 || !segments.every(isTypertRemoteSegment)) throw fail('gateway/bad-request', 'Remote endpoint 格式无效');
  const [namespace, name] = segments;
  const matches = [];
  for (const [key, property] of Object.entries(ctx.reflect.props)) {
    if (property.type !== 'service') continue;
    const service = ctx.get(key);
    const binding = service?.typertRemote;
    if (!binding || binding.namespace !== namespace || binding.serviceKey !== key) continue;
    const marker = remoteMethods(service).find((item) => (item.exportName ?? item.method) === name);
    if (marker) matches.push({ service, marker });
  }
  if (matches.length !== 1) throw fail('gateway/method-unavailable', matches.length ? 'Remote namespace 重复注册' : `插件未提供 Remote 方法 ${endpoint}`);
  return matches[0];
}

export async function invokeRemote(ctx, input, signal) {
  try {
    signal?.throwIfAborted();
    if (!input || !isRemoteJsonValue(input) || Object.keys(input).some((key) => !['endpoint', 'args'].includes(key))) throw fail('gateway/bad-request', 'Remote 请求必须是 JSON endpoint/args');
    const { service, marker } = resolveMethod(ctx, input.endpoint);
    if (marker.invocation.kind !== 'direct' || marker.mode === 'stream' || marker.resources) throw fail('gateway/unsupported', '尚未接入 Remote 流、资源解析或 Host Context；拒绝伪造执行');
    // Cordis 返回的服务方法是绑定代理（toString 为 native code）。签名取真实
    // prototype 描述符，执行仍以当前活跃的服务代理为 receiver，保留依赖上下文。
    let prototype = Object.getPrototypeOf(service), fn;
    while (prototype && !fn) {
      fn = Object.getOwnPropertyDescriptor(prototype, marker.method)?.value;
      prototype = Object.getPrototypeOf(prototype);
    }
    if (typeof fn !== 'function') throw fail('gateway/method-unavailable', 'Remote 方法已被撤销');
    const names = parameterNames(fn);
    const cancellable = names.at(-1) === 'signal';
    const fields = cancellable ? names.slice(0, -1) : names;
    const args = input.args;
    if (!args || Array.isArray(args) || typeof args !== 'object' || Object.keys(args).length !== fields.length || fields.some((name) => !Object.hasOwn(args, name))) throw fail('gateway/bad-request', `Remote 参数必须精确包含：${fields.join(', ')}`);
    const values = fields.map((name) => args[name]);
    if (cancellable) values.push(signal);
    const value = await fn.apply(service, values);
    signal?.throwIfAborted();
    if (value !== undefined && !isRemoteJsonValue(value)) throw fail('gateway/unsupported', 'Remote 结果不是可无损传输的 JSON');
    return { ok: true, ...(value === undefined ? {} : { value }) };
  } catch (error) {
    const remote = remoteErrorOf(error);
    return { ok: false, error: {
      code: signal?.aborted ? 'gateway/cancelled' : remote?.code ?? (error?.code?.startsWith('gateway/') ? error.code : 'gateway/internal'),
      message: String(error?.message ?? error),
      ...(remote && isRemoteJsonValue(remote.details) ? { details: remote.details } : {}),
    } };
  }
}

export async function mountRemote(api, ctx) {
  const release = await api.route(REMOTE_ROUTE, async (input, invocation) => ({
    status: input.method === 'POST' ? 200 : 405,
    body: input.method === 'POST' ? await invokeRemote(ctx, input.body, invocation.signal) : { ok: false, error: { code: 'gateway/bad-request', message: 'Remote 仅接受 POST' } },
    contentType: 'application/json',
  }), { match: 'exact' });
  ctx.effect(() => release);
}
