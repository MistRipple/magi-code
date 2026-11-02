// 工具夹具：echo 返回输入，slow 等待取消，boom 抛错。
export default async function activate(api) {
  await api.tool('echo', async (input, ctx) => ({ echoed: input, invocation: ctx.invocationId }));
  await api.tool('slow', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
  await api.command('hello', async (input) => `你好，${input.text || '世界'}`);
  await api.command('bad', async () => { throw new Error('命令失败'); });
  await api.command('slow-cmd', async (_input, ctx) => {
    await new Promise((resolve) => ctx.signal.addEventListener('abort', resolve, { once: true }));
    return 'aborted';
  });
  await api.tool('boom', async () => { throw new Error('boom failed'); });
}
