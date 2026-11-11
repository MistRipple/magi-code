import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const role = await server.ssrLoadModule('/src/shared/agent-role-save.ts');

  const input = {
    role_id: 'data-analyst', display_name: ' 数据分析师 ', description: '分析数据', system_prompt: '你负责分析数据。',
    capabilities: ['general_engineering', 42, ' data_engineering '], constraints: ['read-only'], parallelism_limit: 2,
  };
  const draft = role.parseAgentRoleDraft(JSON.stringify(input));
  assert.equal(draft.roleId, 'data-analyst');
  assert.equal(draft.displayName, '数据分析师', '首尾空白被去掉');
  assert.deepEqual(draft.capabilities, ['general_engineering', 'data_engineering'], '非字符串项被忽略');
  assert.deepEqual(draft.constraints, ['read-only']);
  assert.deepEqual(draft.focus, [], '未提供的动态字段为空列表');
  assert.equal(draft.parallelismLimit, 2);
  assert.equal(draft.expectedRevision, null, '新建没有版本号');
  assert.deepEqual(role.parseAgentRoleDraft(input), draft, '对象与 JSON 字符串输入一致');
  assert.equal(role.parseAgentRoleDraft({ ...input, expected_role_revision: 3 }).expectedRevision, 3);
  assert.equal(role.parseAgentRoleDraft({ ...input, expected_role_revision: 0 }).expectedRevision, null, '无效版本按未提供处理');
  assert.equal(role.parseAgentRoleDraft('not json'), null);
  assert.equal(role.parseAgentRoleDraft({ display_name: 'x' }), null, '没有 role_id 不构成草稿');
  assert.equal(role.parseAgentRoleDraft([]), null);

  // 结果只识别成功：失败、拒绝和等待授权交给通用错误与授权区域。
  assert.deepEqual(role.parseAgentRoleSaveResult(JSON.stringify({ status: 'ok', mode: 'created', role_revision: 1 })), { mode: 'created', revision: 1 });
  assert.deepEqual(role.parseAgentRoleSaveResult({ status: 'ok', mode: 'updated', role_revision: 4 }), { mode: 'updated', revision: 4 });
  assert.deepEqual(role.parseAgentRoleSaveResult({ status: 'ok', mode: 'unchanged' }), { mode: 'unchanged', revision: null });
  for (const payload of [
    { status: 'failed', error_code: 'role_conflict' },
    { status: 'needs_approval' },
    { status: 'ok', mode: 'other' },
    'oops',
    null,
  ]) assert.equal(role.parseAgentRoleSaveResult(payload), null, JSON.stringify(payload));

  const labels = role.agentRoleCapabilityLabels([
    { capabilities: [{ id: 'general_engineering', displayName: '通用工程' }, { id: 'database', displayName: '数据库' }] },
    { capabilities: [{ id: 'general_engineering', displayName: '重复' }, { id: 'broken', displayName: '' }] },
  ]);
  assert.equal(labels.get('general_engineering'), '通用工程', '同一能力取首个显示名');
  assert.equal(labels.has('broken'), false, '没有显示名的能力不进入映射');
  assert.equal(role.agentRoleCapabilityLabels(undefined).size, 0);

  console.log('agent role save display golden replay passed');
});
