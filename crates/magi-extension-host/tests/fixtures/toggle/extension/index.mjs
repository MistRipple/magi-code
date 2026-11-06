// 运行期上下架夹具：`toggle` 命令释放或重新绑定 `gated` 工具；`stale` 验证旧释放函数不影响新处理器。
export default async function activate(api) {
  let release = await api.tool('gated', async () => 'on');
  let previous = null;
  await api.command('toggle', async () => {
    if (release) {
      await release();
      previous = release;
      release = null;
      return 'off';
    }
    release = await api.tool('gated', async () => 'on-again');
    // 旧的释放函数已经用过一次；再次调用不能把新绑定收回。
    await previous?.();
    return 'on';
  });
  // 上游框架重新挂载时的顺序：新注册先发出，旧释放随后才执行；二者不等待彼此。
  await api.command('swap', async () => {
    const old = release;
    const next = api.tool('gated', async () => 'swapped');
    const done = old?.();
    await Promise.all([next, done]);
    release = await next;
    return 'swapped';
  });
}
