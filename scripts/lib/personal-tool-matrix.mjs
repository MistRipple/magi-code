import assert from 'node:assert/strict';

// 通过生产 Turn/Provider 工具回灌驱动；只在测试隔离目录和本地 bare remote 中产生副作用。
export const personalMatrixMarkers = ['FILES', 'PROCESS', 'GIT', 'GOAL', 'AGENTS', 'CANCEL'].map(name => `PERSONAL_MATRIX_${name}`);

export function personalAgentStep(marker, results, child) {
  const last = name => results.findLast(result => result.tool === name);
  const task = () => last('agent_spawn').child_task_id;
  if (child) {
    if (marker.endsWith('CANCEL')) return results.length ? null : ['shell_exec', { command: 'sleep 60', access_mode: 'read_only' }];
    if (!last('context_search')) return ['context_search', { query: 'PERSONAL_MATRIX_AGENTS' }];
    if (!last('context_read')) return ['context_read', { reference_ids: [last('context_search').results[0].reference_id] }];
    if (!last('context_request')) return ['context_request', { request: '请补充验收标记', timeout_ms: 10000 }];
    assert.equal(last('context_request').status, 'resolved');
    return null;
  }
  if (!last('agent_spawn')) return ['agent_spawn', {
    task_name: 'personal_matrix_child', role: 'executor', display_name: '个人会话验收代理',
    goal: `${marker}_CHILD 验证个人执行目录和上下文工具。`,
    delegation: { purpose: 'user_requested', reason: '本次测试请求明确要求派发代理并收集结果。' },
    context_package: { summary: `${marker}_CHILD`, constraints: ['仅操作本次隔离测试目录'], expected_output: '验收结果', references: [] },
  }];
  if (marker.endsWith('CANCEL')) {
    if (!last('agent_wait')) return ['agent_wait', { task_ids: [task()], timeout_ms: 1000 }];
    if (!last('agent_cancel')) return ['agent_cancel', { task_id: task(), reason: '本次验收测试取消与资源结算' }];
    assert.equal(last('agent_cancel').resources_settled, true);
    assert.equal(last('agent_cancel').results[0].child_status, 'killed');
    return null;
  }
  if (!last('agent_send') || results.at(-1)?.tool === 'agent_wait' && last('agent_wait')?.runtime_signals?.length) return ['agent_send', { task_id: task(), message: '验收标记 PERSONAL_CONTEXT_CONFIRMED' }];
  if (!last('agent_wait') || last('agent_wait').all_terminal === false) return ['agent_wait', { task_ids: [task()], timeout_ms: 10000 }];
  assert.equal(last('agent_wait').failed_count, 0, JSON.stringify(last('agent_wait')));
  assert.equal(last('agent_wait').completed_count, 1, JSON.stringify(last('agent_wait')));
  return null;
}

export function personalMatrixStep(marker, results, messages, origin) {
  const last = tool => results.findLast(result => result.tool === tool);
  const job = () => last('shell_exec').job_id;
  const repo = args => ({ repositoryPath: 'repo', ...args });
  const goal = () => last('get_goal')?.goal ?? last('create_goal').goal;
  const plan = () => last('update_plan').plan;
  const cases = {
    PERSONAL_MATRIX_FILES: [
      ['file_mkdir', { path: 'scratch' }],
      ['file_write', { path: 'scratch/source.txt', content: 'alpha\nbeta\n' }],
      ['file_read', { path: 'scratch/source.txt' }],
      ['file_patch', { path: 'scratch/source.txt', old_string: 'alpha', new_string: 'gamma' }],
      ['apply_patch', { patch: '*** Begin Patch\n*** Add File: scratch/added.txt\n+matrix added\n*** End Patch' }],
      ['file_copy', { source: 'scratch/source.txt', destination: 'scratch/copied.txt' }],
      ['file_move', { source: 'scratch/copied.txt', destination: 'scratch/moved.txt' }],
      ['search_text', { query: 'gamma', root: 'scratch' }],
      ['diff_preview', { before_path: 'scratch/source.txt', after_path: 'scratch/added.txt' }],
      ['diagram_render', { kind: 'flow', graph: { nodes: [{ id: 'a', label: '最近对话' }, { id: 'b', label: '工具执行' }], edges: [{ source: 'a', target: 'b' }] } }],
      ['tool_catalog', { include_schema: true, include_internal: true }],
      ['web_fetch', { url: `${origin}/health` }],
      ['image_generate', { prompt: 'fixture image', output_path: 'matrix.png' }],
      ['view_image', { path: 'matrix.png' }],
      ['file_remove', { path: 'scratch/moved.txt' }],
      ['ask_user_question', { questions: [{ header: '验证', question: '是否继续本次隔离验证？', multiSelect: false,
        options: [{ label: '继续', description: '完成测试' }, { label: '停止', description: '结束测试' }] }] }],
    ],
    PERSONAL_MATRIX_PROCESS: [
      ['shell_exec', { command: 'printf ready; read line; printf "received:%s" "$line"; sleep 60', background: true, access_mode: 'explicit_write' }],
      ['shell_exec', () => ({ action: 'read', job_id: job() })],
      ['shell_exec', { action: 'list' }],
      ['process_inspect', { pid: process.pid }],
      ['shell_exec', () => ({ action: 'write', job_id: results[0].job_id, input: 'matrix-input\n' })],
      ['shell_exec', () => ({ action: 'read', job_id: results[0].job_id })],
      ['shell_exec', () => ({ action: 'kill', job_id: results[0].job_id })],
      ['shell_exec', { command: 'sleep 1; printf settled', background: true, access_mode: 'read_only' }],
      ['shell_exec', () => ({ action: 'wait', job_id: results[7].job_id, timeout_ms: 5000 })],
    ],
    PERSONAL_MATRIX_GIT: [
      ['shell_exec', { command: 'git init -b main repo && git -C repo config user.name Fixture && git -C repo config user.email fixture@example.test && git -C repo commit --allow-empty -m initial && git init --bare remote.git && git -C repo remote add origin ../remote.git', access_mode: 'explicit_write' }],
      ['git_status', repo({})],
      ['git_branch_list', repo({})],
      ['git_branch_create', repo({ branch: 'feature' })],
      ['git_branch_switch', repo({ branch: 'main' })],
      ['git_merge_preview', repo({ target: 'feature' })],
      ['git_merge', repo({ target: 'feature', confirm: true })],
      ['git_branch_delete', repo({ branch: 'feature' })],
      ['git_push', repo({ remote: 'origin', branch: 'main', setUpstream: true })],
      ['git_pull', repo({ remote: 'origin', branch: 'main' })],
      ['git_worktree_create', repo({ mode: 'read_only' })],
      ['git_worktree_list', repo({})],
      ['git_worktree_remove', () => repo({ path: last('git_worktree_create').result.data.worktree.path })],
      ['git_worktree_list', repo({})],
    ],
    PERSONAL_MATRIX_GOAL: [
      ['get_goal', {}],
      ['create_goal', { objective: '验证个人会话目标和计划工具，写入证据文件并完成目标。' }],
      ['update_plan', () => ({ planId: null, expectedRevision: 0, expectedGoalId: goal().goalId,
        expectedGoalControlRevision: goal().controlRevision, language: 'zh-CN', plan: [{ itemId: null, step: '写入验证证据', status: 'in_progress' }] })],
      ['file_write', { path: 'goal-evidence.txt', content: 'goal verified' }],
      ['update_plan', () => ({ planId: plan().planId, expectedRevision: plan().revision, expectedGoalId: goal().goalId,
        expectedGoalControlRevision: goal().controlRevision, language: 'zh-CN', plan: plan().items.map(item => ({ itemId: item.itemId, step: item.title, status: 'completed' })) })],
      ['update_goal', () => ({ goal_id: goal().goalId, expected_revision: goal().controlRevision,
        expected_plan_revision: plan().revision, status: 'complete', completion_summary: '证据文件已写入，计划已完成。',
        evidence_refs: [messages[3].tool_call_id] })],
    ],
  };
  const steps = cases[marker];
  assert.ok(steps, marker);
  results.forEach((result, index) => {
    assert.equal(result.tool, steps[index]?.[0], `回灌顺序 ${marker}/${index}`);
    assert.equal(result.status, result.tool === 'ask_user_question' ? 'answered' : 'succeeded', `${marker}/${index}: ${JSON.stringify(result)}`);
    if (result.tool === 'ask_user_question') assert.deepEqual(result.answers[0].selected, ['继续']);
  });
  const step = steps[results.length];
  return step ? [step[0], typeof step[1] === 'function' ? step[1]() : step[1]] : null;
}
