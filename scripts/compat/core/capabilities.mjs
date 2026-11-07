// Magi 能承载的能力清单：翻译器与上游生态之间的唯一共享词汇。
//
// 这里只描述“Magi 公开接口能做什么”，不描述任何上游生态。每个生态的规则表（dsh/、pi/）把自己的 API
// 映射到这里的能力 ID，并声明保真度；“不可翻译”的原因因此只写在一处，生态之间不会各说各话。
//
// 每条能力都绑定到一个可校验的 Magi 事实（钩子目录条目或宿主 RPC 方法），`test.mjs` 会对照
// crates/magi-extension-host 的源码核对，能力清单与实现不一致时测试失败。
//
// 保真度（fidelity，写在生态规则里，不写在这里）：
//   full     上游契约的每个字段与返回值都有对应；
//   partial  契约里有字段或返回值没有对应；运行期遇到会被明确拒绝，不静默丢弃；
//   none     Magi 没有落点。

/** @typedef {{ group: string, magi: string, hook?: { name: string, mode: 'observe'|'advise'|'guard' }, rpc?: string, invariants?: string[] }} Capability */

/** @type {Record<string, Capability>} */
export const CAPABILITIES = {
  // —— 静态贡献（清单声明 + 运行期绑定处理器）——
  'tool.register': { group: 'tools', magi: '清单 contributions.tools + api.tool', rpc: 'register.tool', invariants: ['工具必须在构建期静态声明', '最终入参由 Magi 按 inputSchema 校验'] },
  'command.register': { group: 'commands', magi: '清单 contributions.commands + api.command', rpc: 'register.command' },
  'settings.schema': { group: 'settings', magi: '清单 settingsSchema + api.settings', rpc: 'settings.read' },
  'modelSource.publish': { group: 'llm', magi: 'api.modelSource（仅主对话、只追加、仅存运行期）', rpc: 'modelSource.set' },
  'skill.contribute': { group: 'prompt', magi: '清单 contributions.skills（静态）' },

  // —— 观察类钩子 ——
  'hook.session.created': { group: 'hooks', magi: '钩子 session.created（observe）', hook: { name: 'session.created', mode: 'observe' } },
  'hook.session.deleted': { group: 'hooks', magi: '钩子 session.deleted（observe）', hook: { name: 'session.deleted', mode: 'observe' } },
  'hook.turn.accepted': { group: 'hooks', magi: '钩子 turn.accepted（observe）', hook: { name: 'turn.accepted', mode: 'observe' } },
  'hook.turn.end': { group: 'hooks', magi: '钩子 turn.end（observe）', hook: { name: 'turn.end', mode: 'observe' } },
  'hook.usage.update': { group: 'hooks', magi: '钩子 usage.update（observe）', hook: { name: 'usage.update', mode: 'observe' } },
  'hook.model.request': { group: 'hooks', magi: '钩子 model.request（observe；advise 未接线）', hook: { name: 'model.request', mode: 'observe' } },
  'hook.model.response': { group: 'hooks', magi: '钩子 model.response（observe）', hook: { name: 'model.response', mode: 'observe' } },
  'hook.model.resolved': { group: 'hooks', magi: '钩子 model.resolved（observe）', hook: { name: 'model.resolved', mode: 'observe' } },

  // —— 拦截与贡献类钩子 ——
  'hook.input.guard': { group: 'hooks', magi: '钩子 input（guard：只能继续或阻止）', hook: { name: 'input', mode: 'guard' } },
  'hook.prompt.contribute': { group: 'prompt', magi: '钩子 prompt.contribute（advise：带稳定性与预算的参考片段）', hook: { name: 'prompt.contribute', mode: 'advise' }, invariants: ['片段以参考资料身份呈现，不得伪造原生系统段'] },
  'hook.context.assemble': { group: 'hooks', magi: '钩子 context.assemble（advise；需 contextRewrite:["history"] 授权）', hook: { name: 'context.assemble', mode: 'advise' }, invariants: ['原始会话不被替换', '改写来源与缓存归因进入 ModelStep'] },
  'hook.compaction.before': { group: 'hooks', magi: '钩子 compaction.before（advise：取消或给出摘要；需 contextRewrite 授权）', hook: { name: 'compaction.before', mode: 'advise' } },
  'hook.tool.call': { group: 'hooks', magi: '钩子 tool.call（guard/advise：阻止或改参）', hook: { name: 'tool.call', mode: 'guard' }, invariants: ['扩展只能阻止不能批准', '改写后的入参重新校验 schema 与权限'] },
  'hook.tool.result': { group: 'hooks', magi: '钩子 tool.result（guard/advise：只改模型可见投影）', hook: { name: 'tool.result', mode: 'advise' }, invariants: ['原始 canonical 回执不变'] },
  'hook.subagent.spawn.guard': { group: 'hooks', magi: '钩子 subagent.spawn（guard：只能阻止）', hook: { name: 'subagent.spawn', mode: 'guard' }, invariants: ['不能指定或覆盖子代理模型'] },

  // —— 反向服务（扩展调用 daemon）——
  'service.model': { group: 'services', magi: 'api.model.invoke（仅 prompt 文本）', rpc: 'model.invoke' },
  'service.notify': { group: 'services', magi: 'api.notify.user', rpc: 'notify.user' },
  'service.ask': { group: 'services', magi: 'api.ask.user（扩展不能代答审批）', rpc: 'ask.user' },
  'service.snapshot': { group: 'services', magi: 'api.session.snapshot（只读、有界）', rpc: 'session.snapshot' },
  'service.sessionDirectory': { group: 'services', magi: 'api.session.directory（需独立授权，只读用户会话元数据，每页最多 200 条）', rpc: 'session.directory' },
  'service.spawn': { group: 'services', magi: 'api.task.spawn（保留角色与模型绑定）', rpc: 'task.spawn' },
  'service.blob': { group: 'services', magi: 'api.blob.put/get（有界 JSON）', rpc: 'blob.put' },
  'service.tools': { group: 'services', magi: 'api.tools.call（只调公开原生工具，需活动 Task）', rpc: 'tool.call' },

  // —— 界面 ——
  'ui.view': { group: 'ui', magi: '清单 views + 视图桥（受限操作）' },
  'ui.route': { group: 'ui', magi: 'api.route + 视图桥 route.call（只服务本插件自己的视图）', rpc: 'register.route', invariants: ['路径只在本插件内有效', '请求经 daemon 转交，宿主不开放监听端口'] },
  'ui.native': { group: 'ui', magi: '上游自带的浏览器端界面原样挂载在设置视图里（页面壳提供其依赖的宿主服务）' },
};

export const CAPABILITY_IDS = Object.keys(CAPABILITIES);

/** 能力 ID 必须存在；规则表引用不存在的能力时在加载阶段就失败，而不是静默产生一个未登记的状态。 */
export function requireCapability(id) {
  const capability = CAPABILITIES[id];
  if (!capability) throw new Error(`规则引用了未登记的 Magi 能力：${id}`);
  return capability;
}
