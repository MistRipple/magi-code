// DSH 使用真实 WebRuntime 注册和选择；全局选择只沿 daemon CAS 持久化。
import { WebRuntime } from '@deepseek-ai/dsh-web';
import { runtimeId } from '../core/identity.mjs';

export async function installWeb(ctx, api, catalog, emit, run = callback => callback()) {
  let snapshot = await api.web.read(), queue = Promise.resolve();
  const providers = { search: new Map(), fetch: new Map() };
  const selection = kind => snapshot[kind]?.providerId ?? (kind === 'search' ? 'deepseek-official' : 'http');
  const refs = Object.fromEntries(['search', 'fetch'].map(kind => [kind + 'Provider', { get: () => selection(kind) }]));
  await ctx.plugin(class extends WebRuntime {
    static Config = undefined;
    constructor(c) { super(c, refs); }
    registerSearchProvider(provider) { return this.register('search', provider, () => super.registerSearchProvider(provider)); }
    registerFetchProvider(provider) { return this.register('fetch', provider, () => super.registerFetchProvider(provider)); }
    register(kind, provider, original) {
      if (!provider || typeof provider.id !== 'string' || !provider.id || provider.id.length > 96
        || typeof provider.available !== 'function' || typeof provider[kind] !== 'function') throw new Error('联网提供方声明无效');
      const dispose = original(), id = runtimeId('tool', `web.${kind}.${provider.id}`);
      return this.ctx.effect(() => {
        providers[kind].set(provider.id, id);
        const release = catalog.register('tools', { id, title: `web_${kind}: ${provider.id}`,
          description: `Web ${kind} provider ${provider.id}`, readOnly: true, timeoutMs: 120000,
          inputSchema: { type: 'object', properties: { [kind === 'search' ? 'query' : 'url']: { type: 'string' } }, required: [kind === 'search' ? 'query' : 'url'], additionalProperties: false },
        }, () => api.tool(id, (input, context) => run(async () => {
          if (!provider.available()) throw new Error(`联网提供方 ${provider.id} 当前不可用`);
          const request = kind === 'search' ? { query: input.query, maxResults: 10 } : { url: input.url };
          const result = await provider[kind](request, context.signal);
          if (context.signal.aborted) throw context.signal.reason;
          if (kind === 'search') {
            if (!result || !Array.isArray(result.sources) || typeof result.truncated !== 'boolean'
              || result.sources.some(source => typeof source.url !== 'string' || !/^https?:\/\//.test(source.url))) throw new Error('搜索提供方返回无效来源');
            return { tool: 'web_search', status: 'succeeded', ...result, providerId: provider.id };
          }
          if (!result || !Number.isInteger(result.statusCode) || result.statusCode < 100 || result.statusCode > 599
            || !['html', 'text'].includes(result.body?.kind) || typeof result.body.content !== 'string'
            || typeof result.url !== 'string' || typeof result.truncated !== 'boolean') throw new Error('抓取提供方返回无效网页');
          return { tool: 'web_fetch', status: 'succeeded', ...result, providerId: provider.id };
        }, context.signal)));
        return async () => { providers[kind].delete(provider.id); dispose(); await release(); };
      });
    }
  });
  return {
    describe: () => ({ ns: 'web', value: { searchProvider: selection('search'), fetchProvider: selection('fetch') } }),
    mutate(operations, expectedRevision) {
      const work = queue.then(async () => {
        if (!Array.isArray(operations) || !operations.length) throw new Error('联网设置操作无效');
        await catalog.drain();
        snapshot = await api.web.read();
        if (expectedRevision !== undefined && expectedRevision !== snapshot.revision) throw new Error('联网设置版本冲突');
        const changes = operations.map(operation => {
          if (operation.op !== 'set' || operation.path?.length !== 1 || !['searchProvider', 'fetchProvider'].includes(operation.path[0])) throw new Error('联网设置只允许选择搜索和抓取提供方');
          const kind = operation.path[0] === 'searchProvider' ? 'search' : 'fetch', provider = operation.value;
          if (provider === (kind === 'search' ? 'deepseek-official' : 'http')) return { kind, providerId: null, contributionId: null };
          const contributionId = providers[kind].get(provider);
          if (!contributionId) throw new Error(`联网提供方 ${provider} 未由当前插件注册`);
          return { kind, providerId: provider, contributionId };
        });
        snapshot = await api.web.configure({ expectedRevision: snapshot.revision, changes });
        emit('web');
      });
      queue = work.catch(() => {}); work.catch(() => {});
      return work;
    },
    async run(callback) { snapshot = await api.web.read(); return callback(); },
    async drain() { await queue; },
  };
}
