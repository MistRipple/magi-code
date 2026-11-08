import { AsyncLocalStorage } from 'node:async_hooks';

// 只把当前 Magi 调用的真实会话身份与执行目录投影给上游业务。
// 不创建 DSH Agent/Session，不提供消息、调度或持久化的影子实现。
const invocations = new AsyncLocalStorage();

export async function withInvocation(context, callback) {
  const scope = { context, closed: false };
  context.signal?.throwIfAborted();
  return invocations.run(scope, async () => {
    try { return await callback(); }
    finally { scope.closed = true; }
  });
}

export function invocationAgent() {
  const scope = invocations.getStore();
  if (!scope) throw new Error('没有活动的插件业务调用');
  const check = () => {
    if (scope.closed || invocations.getStore() !== scope) throw new Error('会话执行投影已失效或不属于当前调用');
    scope.context.signal?.throwIfAborted();
  };
  check();
  if (scope.agent) return scope.agent;
  const { owner, execution } = scope.context;
  if (!owner?.sessionId) return undefined;
  if (typeof owner.sessionId !== 'string' || !execution
    || !(execution.workingDirectory === null || typeof execution.workingDirectory === 'string')) {
    throw new Error('daemon 未提供有效的会话执行上下文');
  }
  const project = (name, values) => new Proxy(Object.freeze(values), {
    get(target, key) {
      check();
      if (Object.hasOwn(target, key)) return target[key];
      if (key === 'then' || typeof key === 'symbol') return undefined;
      throw new Error(`${name}.${String(key)} 尚未接入 Magi 原生服务；本次调用只提供会话身份与执行目录`);
    },
  });
  const header = project('Session.header', { cwd: execution.workingDirectory });
  const session = project('Session', { id: owner.sessionId, header });
  scope.agent = project('Agent', { id: owner.sessionId, session });
  return scope.agent;
}
