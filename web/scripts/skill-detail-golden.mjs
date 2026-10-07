import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const skill = await server.ssrLoadModule('/src/lib/skill-instruction.ts');

  const parts = skill.splitSkillInstruction('---\nname: demo\ndescription: "演示 Skill"\n---\n\n# 标题\n正文');
  assert.deepEqual(parts.meta, [
    { key: 'name', value: 'demo' },
    { key: 'description', value: '演示 Skill' },
  ]);
  assert.equal(parts.body, '# 标题\n正文');

  // 没有 front matter、或 front matter 没闭合时，整段都是正文。
  assert.deepEqual(skill.splitSkillInstruction('# 只有正文'), { meta: [], body: '# 只有正文' });
  assert.deepEqual(skill.splitSkillInstruction('---\nname: x\n没有结束'), { meta: [], body: '---\nname: x\n没有结束' });
  // CRLF 与正文里的分隔线不会被误当成 front matter 结束。
  const crlf = skill.splitSkillInstruction('---\r\nname: a\r\n---\r\n正文\r\n---\r\n后文');
  assert.equal(crlf.meta[0].value, 'a');
  assert.equal(crlf.body, '正文\n---\n后文');

  assert.equal(skill.formatSkillFileSize(512), '512 B');
  assert.equal(skill.formatSkillFileSize(2048), '2.0 KB');
  assert.equal(skill.formatSkillFileSize(50 * 1024), '50 KB');
  assert.equal(skill.formatSkillFileSize(3 * 1024 * 1024), '3.0 MB');

  // 接线：设置页必须能展开 Skill 详情并调用详情接口。
  const tools = await readFile(new URL('../src/components/SettingsToolsTab.svelte', import.meta.url), 'utf8');
  assert.match(tools, /getAgentSkillDetail/);
  assert.match(tools, /skill-detail/);
  const api = await readFile(new URL('../src/web/agent-api.ts', import.meta.url), 'utf8');
  assert.match(api, /\/api\/settings\/skills\/detail/);

  console.log('skill detail golden passed');
});
