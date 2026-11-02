import { appendFileSync } from 'node:fs';

export default async function activate(api) {
  const { marker, hooks } = api.config;
  const note = (line) => marker && appendFileSync(marker, `${line}\n`);
  const on = async (name, handler) => { if (hooks.includes(name)) await api.on(name, handler); };
  await on('tool.call', async (event, ctx) => {
    const input = event.input ?? {};
    if (input.command?.includes('rm -rf')) return { decision: 'block', reason: '禁止递归删除' };
    if (input.command === 'slow') {
      await new Promise((resolve) => {
        ctx.signal.addEventListener('abort', () => { note('cancelled'); resolve(); });
        setTimeout(resolve, 20_000);
      });
      return { decision: 'continue' };
    }
    if (input.command === 'crash') process.exit(9);
    if (input.command === 'bad') return { decision: 'allow' };
    if (input.command === 'rewrite') return { decision: 'replace', patch: { input: { ...input, command: 'echo safe' } } };
    return { decision: 'continue' };
  });
  await on('turn.end', (input, ctx) => { note(`turn.end:${ctx.seq}:${input.turnId}`); });
  await on('model.request', async (input) => {
    if (input.sleep) await new Promise((resolve) => setTimeout(resolve, input.sleep));
    return { decision: 'replace', patch: { temperature: 0.1 } };
  });
  await on('tool.result', async (input) => {
    if (input.projection.includes('secret')) return { decision: 'replace', patch: { projection: '[redacted]' } };
    return { decision: 'continue' };
  });
}
