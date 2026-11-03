// DSH（DeepSeek Harness）能力到 Magi 的对照表。按顺序匹配，先匹配者生效；
// 没有命中任何规则的条目一律为 unknown，由人工归类后补进本表。
//
// category 用于“必需能力”判定：翻译时用户声明要保留哪些类别（如 llm），
// 这些类别里只要有 planned/dropped/unknown 条目，翻译就被阻塞。

const rule = (kind, match, category, status, target, note = '') => ({ kind, match, category, status, target, note });

export const DSH_RULES = [
  // —— 上下文与生命周期（兼容层自己实现 Cordis 子集）——
  rule('calls', /^ctx\.(logger)(\.|$)/, 'runtime', 'shim', 'api.log', '日志降级为诊断日志'),
  rule('calls', /^ctx\.(effect|fiber|on|once|collect|dispose|scope)(\.|$)/, 'runtime', 'shim', '扩展关闭时执行清理', 'Cordis 生命周期子集'),
  rule('calls', /^ctx\.(inject|get|plugin|provide|root|parent)(\.|$)/, 'runtime', 'shim', '服务查找，仅限已登记服务', '未登记的服务会使对应功能丢失'),
  rule('calls', /^ctx\.settings(\.|$)/, 'settings', 'shim', 'api.settings + settingsSchema', '配置 schema 构建期转成 JSON Schema'),
  rule('calls', /^ctx\.llm\.(registerAdapter|registerConfigurableProviders|registerModelDiscovery)$/, 'llm', 'planned', '插件向主对话追加模型数据源', '目标 Magi `api.modelSource`（仅主对话、只追加）已交付；翻译器尚未生成对应的提交代码'),
  rule('calls', /^ctx\.llm(\.|$)/, 'llm', 'planned', '插件向主对话追加模型数据源', '其他 llm 服务需逐项评审'),
  rule('calls', /^ctx\.(tools)(\.|$)/, 'tools', 'planned', '模型工具（contributions.tools）', '工具必须在构建期静态声明'),
  rule('calls', /^ctx\.(commands)(\.|$)/, 'commands', 'planned', '用户命令（contributions.commands）', '命令必须在构建期静态声明'),
  rule('calls', /^ctx\.(webServer|server|router|http)(\.|$)/, 'ui', 'dropped', '无', 'Magi 扩展宿主不提供 HTTP 路由'),
  rule('calls', /^ctx\.(sessionProjections|attachments|session|sessions|messages|memory)(\.|$)/, 'session', 'dropped', '无', '会话/消息/附件由 Magi 内核独有，扩展只能走钩子与宿主服务'),
  rule('calls', /^ctx\.(slots|locale)(\.|$)/, 'ui', 'dropped', '无', '浏览器端界面槽位与多语言'),
  rule('calls', /^ctx\.systemPrompt(\.|$)/, 'prompt', 'planned', '钩子 prompt.contribute', '提示贡献由 Magi 带来源、稳定性与预算裁决'),
  rule('calls', /^ctx\.(\w+)/, 'other', 'unknown', '待评审'),

  // —— 服务名（inject）——
  rule('injects', /^(llm)$/, 'llm', 'planned', '插件向主对话追加模型数据源'),
  rule('injects', /^(settings)$/, 'settings', 'shim', 'api.settings'),
  rule('injects', /^(tools)$/, 'tools', 'planned', '模型工具'),
  rule('injects', /^(commands)$/, 'commands', 'planned', '用户命令'),
  rule('injects', /^(webServer|server)$/, 'ui', 'dropped', '无', 'Magi 扩展宿主不提供 HTTP 路由'),
  rule('injects', /^(sessionProjections|attachments)$/, 'session', 'dropped', '无'),
  rule('injects', /^(slots|locale)$/, 'ui', 'dropped', '无'),
  rule('injects', /^(systemPrompt)$/, 'prompt', 'planned', '钩子 prompt.contribute'),
  rule('injects', /.*/, 'other', 'unknown', '待评审'),

  // —— 导入 ——
  rule('imports', /^(type:)?node:/, 'runtime', 'bundled', 'Node 内置模块'),
  rule('imports', /^(type:)?\.{1,2}(\/|$)/, 'internal', 'bundled', '包内相对导入，构建期打包'),
  rule('imports', /^(type:)?@deepseek-ai\/dsh-client-/, 'ui', 'dropped', '无', '浏览器端界面；Magi 视图需另行实现'),
  rule('imports', /^(type:)?(react|react-dom)(\/|$)/, 'ui', 'dropped', '无', '浏览器端界面'),
  rule('imports', /^type:@deepseek-ai\//, 'types', 'bundled', '仅类型，构建期擦除'),
  rule('imports', /^@deepseek-ai\/dsh-llm-pi-ai$/, 'llm', 'planned', 'pi-ai 适配层（与 pi 翻译器共用）', '记录适配器选项以生成模型数据源'),
  rule('imports', /^@deepseek-ai\/dsh-llm$/, 'llm', 'planned', '模型描述类型与流式块', '只用类型与枚举时可由兼容层提供'),
  rule('imports', /^@deepseek-ai\/dsh-(atomic-write|home-paths)$/, 'runtime', 'shim', '兼容层提供的文件与目录工具'),
  rule('imports', /^@deepseek-ai\/dsh-settings$/, 'settings', 'shim', 'api.settings'),
  rule('imports', /^@deepseek-ai\/dsh-attachment$/, 'session', 'dropped', '无'),
  rule('imports', /^@deepseek-ai\/schemastery$/, 'settings', 'bundled', '纯 JS，构建期打包'),
  rule('imports', /^@deepseek-ai\/cordis$/, 'runtime', 'shim', '兼容层提供的 Cordis 子集'),
  rule('imports', /^(type:)?@earendil-works\/pi-ai(\/|$)/, 'llm', 'planned', 'pi-ai 适配层（与 pi 翻译器共用）'),
  rule('imports', /^(type:)?@deepseek-ai\//, 'other', 'unknown', '待评审'),
  rule('imports', /.*/, 'other', 'bundled', '第三方纯 JS 依赖，构建期打包', '含原生模块时需另行确认'),
];

export function classify(kind, name) {
  for (const candidate of DSH_RULES) {
    if (candidate.kind === kind && candidate.match.test(name)) return candidate;
  }
  return rule(kind, /.*/, 'other', 'unknown', '待评审');
}
