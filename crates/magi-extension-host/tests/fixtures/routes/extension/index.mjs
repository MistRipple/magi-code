// 视图后端路由夹具：精确与前缀绑定、处理器抛错、取消、运行期释放。
export default async function activate(api) {
  await api.route('/status', async (input) => ({ body: { seen: input } }));
  await api.route('/items/', async (input) => ({ status: 201, contentType: 'application/x-ndjson', body: `${input.path}\n` }), { match: 'prefix' });
  await api.route('/items/special', async () => ({ body: 'exact-wins' }));
  await api.route('/boom', async () => { throw new Error('处理器失败'); });
  await api.route('/slow', async (_input, ctx) => new Promise((resolve, reject) => {
    ctx.signal.addEventListener('abort', () => reject(new Error('cancelled')));
    setTimeout(() => resolve({ body: 'late' }), 30_000);
  }));
  let release = await api.route('/gone', async () => ({ body: 'here' }));
  await api.route('/drop', async () => { await release?.(); release = null; return { body: 'dropped' }; });
}
