import { AsyncLocalStorage } from 'node:async_hooks';

// 只把当前 Magi 调用的真实会话身份与执行目录投影给上游业务。
// 不创建 DSH Agent/Session，不提供消息、调度或持久化的影子实现。
const invocations = new AsyncLocalStorage();

export async function withInvocation(context, callback, api, settlement) {
  const scope = { context, closed: false, messages: [], hasMessageApi:typeof api?.session?.message==='function' };
  context.signal?.throwIfAborted();
  return invocations.run(scope, async () => {
    try {
      const result = await callback();
      context.signal?.throwIfAborted();
      let deliveryError;
      try {
        for (const message of settlement?.contexts(result) ?? []) {
          const agent = invocationAgent();
          if (!agent) throw new Error('工具补充上下文缺少当前会话');
          agent.inject(message);
        }
        for (const message of scope.messages) {
          context.signal?.throwIfAborted();
          const receipt = await api.session.message(message);
          if (!receipt?.inputId || !['pending','attached','consumed'].includes(receipt.state)) throw new Error('daemon 未接纳插件消息');
        }
      } catch (error) {
        context.signal?.throwIfAborted();
        // 工具业务已结算：交付失败不能覆盖原结果，也不能据此重放业务。
        // 命令仍沿其原错误合同；工具工厂保留实际结果并提交显式失败。
        if (!settlement) throw error;
        deliveryError = error;
      }
      return settlement ? settlement.complete(result, deliveryError) : result;
    }
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
      throw new Error(`${name}.${String(key)} 尚未接入 Magi 原生服务；仅可使用当前调用明确提供的宿主能力`);
    },
  });
  const header = project('Session.header', { cwd: execution.workingDirectory });
  const session = project('Session', { id: owner.sessionId, header });
  const deliver = (message, delivery) => {
    check();
    if (!scope.hasMessageApi || !message || typeof message !== 'object' || message.role && message.role !== 'user'
      || typeof message.id !== 'string' || !message.id.trim() || message.id.length>128
      || !Array.isArray(message.content) || !message.content.length
      || message.content.some(part=>part?.type!=='text' || typeof part.text!=='string')
      || typeof message.source?.kind!=='string' || !message.source.kind.trim() || message.source.kind.length>128
      || /[\u0000-\u001f\u007f-\u009f]/u.test(message.source.kind)
      || ['user','system','assistant'].includes(message.source.kind)) throw new Error('插件消息必须包含原生用户角色的文本、稳定身份与独立来源');
    const text = message.content.map(part=>part.text).join('\n');
    if (!text.trim() || text.length>200000) throw new Error('插件消息为空或超过本次投递上限');
    const item={messageId:message.id,text,sourceKind:message.source.kind,delivery};
    const previous=scope.messages.find(item=>item.messageId===message.id);
    if (previous) { if (JSON.stringify(previous)!==JSON.stringify(item)) throw new Error('同一消息身份的内容发生变化'); return; }
    if (scope.messages.length>=32) throw new Error('插件消息超过本次投递上限');
    scope.messages.push(item);
  };
  scope.agent = project('Agent', { id: owner.sessionId, session,
    status: scope.context.activity==='command' ? 'idle' : 'running',
    inject: message=>deliver(message,'context'), steer: message=>deliver(message,'steer'), followup: message=>deliver(message,'followup'),
  });
  return scope.agent;
}
