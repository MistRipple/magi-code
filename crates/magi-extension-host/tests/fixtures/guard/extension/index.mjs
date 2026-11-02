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
  await on('input', async (event) =>
    event.raw?.text?.includes('forbidden') ? { decision: 'block', reason: '输入违规' } : { decision: 'continue' });
  await on('subagent.spawn', async (event) =>
    event.role === 'reviewer' ? { decision: 'block', reason: '禁止评审代理' } : { decision: 'continue' });
  await on('session.created', (event) => { note(`session.created:${event.sessionId}`); });
  await on('usage.update', (event) => { note(`usage:${event.model}:${event.inputTokens}`); });
  await on('context.assemble', async (event) => {
    const messages = event.messages;
    if (messages.length >= 2 && messages[0].text.includes('OLD')) {
      return {
        decision: 'replace',
        patch: { rewrites: [{
          from: messages[0].index,
          to: messages[1].index + 1,
          replacement: [{ role: 'assistant', text: '[扩展摘要]' }],
        }] },
      };
    }
    return { decision: 'continue' };
  });
  await on('compaction.before', async (event) => {
    if (event.source.includes('CANCELME')) return { decision: 'block', reason: '稍后再压缩' };
    if (event.source.includes('CUSTOMSUMMARY')) {
      return { decision: 'replace', patch: { summary: event.source.includes('BADFORM') ? '太短' : '## 目标与完成标准\n- 扩展摘要。\n## 约束与权限\n- 无。\n## 工作区事实\n- 无。\n## 工具与外部操作\n- 无。\n## 代理状态\n- 无。\n## 已确认决策\n- 无。\n## 阻塞与风险\n- 无。\n## 下一步\n- 继续。\n## 禁止重复\n- 无。' } };
    }
    return { decision: 'continue' };
  });
  await on('prompt.contribute', async (event) => {
    if (event.phase === 'start') {
      return { decision: 'replace', patch: { fragments: [
        { id: 'house-style', stability: 'stable', priority: 1, text: '团队约定：提交信息使用中文。' },
        { id: 'oops', stability: 'volatile', text: '启动阶段不接受易变片段' },
      ] } };
    }
    return { decision: 'replace', patch: { fragments: [
      { id: 'round-note', stability: 'volatile', text: `第 ${event.round} 轮上下文提醒` },
    ] } };
  });
}
