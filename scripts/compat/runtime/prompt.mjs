// 上游注册表只拥有本插件的贡献；最终提示、来源、预算与每轮接纳由 daemon 拥有。
// 使用真实 DSH 注册表保持片段排序、变量插值、重复注册与 Cordis effect 回收语义。
export async function installPromptContributions(ctx) {
  const { SystemPrompt, renderPrompt } = await import('@deepseek-ai/dsh-system-prompt');
  const { scopeOf } = await import('@deepseek-ai/dsh-scope');
  const unscoped = context => {
    if (scopeOf(context)) throw new Error('提示贡献的 Agent 作用域尚未接入 Magi；不能将局部指引应用到其他会话');
  };
  await ctx.plugin(class extends SystemPrompt {
    constructor(context) { super(context, { includeHarnessIdentity: false }); }
    section(section) {
      unscoped(this.ctx);
      if (!section || typeof section !== 'object' || Array.isArray(section)
        || Object.keys(section).some(key => !['name', 'order', 'text', 'interpolate', 'complete'].includes(key))
        || typeof section.name !== 'string' || !section.name.trim() || section.name.length > 128
        || !Number.isFinite(section.order) || !['string', 'function'].includes(typeof section.text)
        || section.interpolate !== undefined && typeof section.interpolate !== 'boolean') throw new TypeError('提示片段声明无效');
      if (section.complete !== undefined && section.complete !== false) throw new Error('插件不能替换 Magi 的完整系统提示；请注册本插件的业务指引');
      return super.section({ ...section });
    }
    variable(name, provider) {
      unscoped(this.ctx);
      if (typeof provider !== 'function') throw new TypeError('提示变量必须提供同步读取函数');
      return super.variable(name, provider);
    }
    context() { throw new Error('动态上下文贡献尚未接入 Magi；不能假装修改原生运行上下文'); }
    tools() { throw new Error('提示工具目录由 daemon 拥有；请通过 tools 服务注册工具'); }
    suppressRuntimeContext() { throw new Error('插件不能关闭 Magi 的原生运行上下文'); }
  });
  return async (event, context) => {
    context.signal?.throwIfAborted();
    // 上游允许在运行期间切换指引；每轮重新读取，不能把首次读取错误标成稳定前缀。
    if (event.phase === 'start') return { decision: 'continue' };
    if (event.phase !== 'round' || !Number.isSafeInteger(event.round) || event.round < 0
      || !Number.isSafeInteger(event.budget?.maxFragmentChars) || event.budget.maxFragmentChars <= 0
      || !Number.isSafeInteger(event.budget?.maxTotalChars) || event.budget.maxTotalChars <= 0) throw new Error('daemon 提示贡献请求无效');
    const assembly = await ctx.systemPrompt.assemble({ signal: context.signal, phase: event.phase, round: event.round });
    context.signal?.throwIfAborted();
    if (assembly.sections.some(section => typeof section.text !== 'string')
      || Object.values(assembly.variables).some(value => value !== undefined && typeof value !== 'string')) throw new TypeError('提示片段与变量必须同步返回字符串');
    const text = renderPrompt(assembly);
    if (!text.trim()) return { decision: 'continue' };
    const length = [...text].length;
    if (length > Math.min(event.budget.maxFragmentChars, event.budget.maxTotalChars)) throw new Error(`插件业务指引超过 daemon 提示预算（${length} 字符）；未截断或提交部分指引`);
    return { decision: 'replace', patch: { fragments: [{ id: 'dsh-sections', stability: 'volatile', text }] } };
  };
}
