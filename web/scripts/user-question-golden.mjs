import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const q = await server.ssrLoadModule('/src/shared/user-question.ts');
  const single = {
    question: '用哪种数据库？', header: '数据库', multiSelect: false,
    options: [{ label: 'SQLite（推荐）', description: '零运维' }, { label: 'Postgres' }],
  };
  const multi = {
    question: '覆盖哪些平台？', header: '平台', multiSelect: true,
    options: [{ label: 'macOS' }, { label: 'Windows' }, { label: 'Linux' }],
  };
  const questions = [single, multi];
  let drafts = q.emptyUserQuestionDrafts(questions);
  assert.equal(q.areUserQuestionsAnswered(drafts), false, '没有任何选择时不能提交');

  // 单选：点选替换；点“其他”取代预设选项；再点预设选项取消“其他”。
  drafts[0] = q.toggleUserQuestionOption(single, drafts[0], 'SQLite（推荐）');
  drafts[0] = q.toggleUserQuestionOption(single, drafts[0], 'Postgres');
  assert.deepEqual(drafts[0].selected, ['Postgres']);
  drafts[0] = q.toggleUserQuestionOther(single, drafts[0]);
  assert.deepEqual(drafts[0].selected, [], '单选时“其他”取代预设选项');
  assert.equal(q.isUserQuestionAnswered(drafts[0]), false, '“其他”没写内容不算已回答');
  drafts[0] = q.setUserQuestionOtherText(drafts[0], '  MySQL  ');
  assert.equal(q.isUserQuestionAnswered(drafts[0]), true);
  drafts[0] = q.toggleUserQuestionOption(single, drafts[0], 'Postgres');
  assert.equal(drafts[0].otherActive, false, '单选点预设选项会取消“其他”');

  // 多选：切换，和“其他”并存。
  drafts[1] = q.toggleUserQuestionOption(multi, drafts[1], 'Linux');
  drafts[1] = q.toggleUserQuestionOption(multi, drafts[1], 'macOS');
  drafts[1] = q.toggleUserQuestionOption(multi, drafts[1], 'Linux');
  drafts[1] = q.toggleUserQuestionOther(multi, drafts[1]);
  drafts[1] = q.setUserQuestionOtherText(drafts[1], 'FreeBSD');
  assert.deepEqual(drafts[1].selected, ['macOS']);
  assert.equal(q.areUserQuestionsAnswered(drafts), true);

  // 不存在的选项被忽略。
  assert.equal(q.toggleUserQuestionOption(single, drafts[0], '不存在'), drafts[0]);

  // 回答：选中项按选项原顺序；“其他”只在选中且非空时带上并去掉首尾空白。
  drafts[0] = q.toggleUserQuestionOther(single, drafts[0]);
  drafts[0] = q.setUserQuestionOtherText(drafts[0], '  MySQL  ');
  assert.deepEqual(q.buildUserQuestionAnswers(questions, drafts), [
    { selected: [], other: 'MySQL' },
    { selected: ['macOS'], other: 'FreeBSD' },
  ]);

  // 工具结果摘要。
  const answered = q.parseUserQuestionResult(JSON.stringify({
    tool: 'ask_user_question', status: 'answered',
    answers: [{ question: '用哪种数据库？', header: '数据库', selected: ['Postgres'], other: null }],
  }));
  assert.deepEqual(answered, {
    status: 'answered',
    items: [{ header: '数据库', question: '用哪种数据库？', selected: ['Postgres'], other: '' }],
  });
  assert.deepEqual(q.parseUserQuestionResult('{"status":"skipped"}'), { status: 'skipped' });
  assert.deepEqual(q.parseUserQuestionResult({ status: 'awaiting_user_input', question_id: 'q1' }), { status: 'awaiting', questionId: 'q1' });
  assert.equal(q.parseUserQuestionResult('not json'), null);
  assert.equal(q.parseUserQuestionResult('{"status":"failed"}'), null);

  // 接线：事件桥接、线程面板与显示名都必须接上。
  const bridge = await readFile(new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url), 'utf8');
  assert.match(bridge, /user\.question\.requested/);
  const input = await readFile(new URL('../src/components/InputArea.svelte', import.meta.url), 'utf8');
  assert.match(input, /<UserQuestionPanel/, '提问卡片必须渲染在输入区容器里，才能和输入框相接');
  const names = await server.ssrLoadModule('/src/lib/tool-display-name.ts');
  const translations = { t: (key) => key === 'toolCall.displayName.askUserQuestion' ? '向你提问' : key };
  assert.equal(names.resolveToolDisplayName('ask_user_question', translations), '向你提问');
  const visibility = await server.ssrLoadModule('/src/shared/tool-visibility.ts');
  assert.equal(visibility.isRuntimeInternalTool('ask_user_question'), false, '提问必须出现在对话流里');

  console.log('user question golden passed');
});
