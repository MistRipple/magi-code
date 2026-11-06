// 非法路由名：路径必须以 / 开头，且不含查询。
export default async function activate(api) {
  await api.route('no-slash', async () => ({ body: 1 }));
}
