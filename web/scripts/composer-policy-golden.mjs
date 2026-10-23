import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const policy = await server.ssrLoadModule('/src/lib/composer-policy.ts');
  const actions = await server.ssrLoadModule('/src/lib/composer-actions.ts');
  assert.equal(policy.queuedMessageEditable({ interruptTurnId: 'old-turn' }), false,
    '停止并改做的原输入不能编辑成普通投递');

  // 会话命令：草稿会话没有可压缩内容；带附件时 daemon 会拒绝。
  assert.equal(
    policy.sessionCommandDisabledReason({ sessionCommandsAvailable: true, attachmentTotal: 0 }),
    null,
  );
  assert.equal(
    policy.sessionCommandDisabledReason({ sessionCommandsAvailable: false, attachmentTotal: 0 }),
    'draft-session',
  );
  assert.equal(
    policy.sessionCommandDisabledReason({ sessionCommandsAvailable: true, attachmentTotal: 2 }),
    'has-attachments',
  );
  assert.equal(
    policy.sessionCommandDisabledReason({ sessionCommandsAvailable: false, attachmentTotal: 2 }),
    'draft-session',
    'a draft session explains the missing history first',
  );
  assert.equal(
    policy.composerAttachmentTotal({
      images: 1,
      contextReferences: 2,
      browserAnnotations: 3,
      browserNodeSelections: 4,
    }),
    10,
  );

  // 斜杠菜单：/compact 不再消失，而是带着不可用原因留在菜单里。
  const labels = {
    goal: { name: '目标模式', description: 'g' },
    compact: { name: '压缩上下文', description: 'c' },
    context: { name: '文件', description: 'f' },
  };
  const draftActions = actions.buildComposerActions([], labels, {
    sessionCommandDisabledReason: 'draft-session',
  });
  const compact = draftActions.find((action) => action.id === 'compact');
  assert.ok(compact, '/compact stays listed in draft sessions');
  assert.equal(compact.disabledReason, 'draft-session');
  assert.deepEqual(
    actions.filterSlashCommands(draftActions, '压缩').map((action) => action.id),
    ['compact'],
    'a disabled command is still discoverable by search',
  );
  const readyActions = actions.buildComposerActions([], labels);
  assert.equal(
    readyActions.find((action) => action.id === 'compact').disabledReason,
    undefined,
    'no disabled reason when the command can run',
  );

  // 图片筛选：张数、体积、非图片各有去向，且顺序稳定。
  const mb = 1024 * 1024;
  const selection = policy.selectDroppedImages(
    [
      { name: 'a.png', type: 'image/png', size: 1 * mb },
      { name: 'notes.txt', type: 'text/plain', size: 10 },
      { name: 'huge.png', type: 'image/png', size: 11 * mb },
      { name: 'b.jpg', type: 'image/jpeg', size: 2 * mb },
      { name: 'c.png', type: 'image/png', size: 1 * mb },
    ],
    3,
  );
  assert.deepEqual(selection.accepted, [0, 3], 'only the images that fit under the limit are accepted');
  assert.equal(selection.ignored, 1);
  assert.deepEqual(selection.tooLarge, [{ name: 'huge.png', size: 11 * mb }]);
  assert.equal(selection.overLimit, 1, 'the image past the limit is reported, not silently dropped');
  assert.equal(policy.MAX_COMPOSER_IMAGES, 5);

  const canonicalTurn = (status, items = []) => ({
    sessionId: 'session-resume', turnId: `turn-${status}`, turnSeq: 1,
    acceptedAt: 1, status, items,
  });
  assert.equal(policy.composerCanContinue({
    sessionId: 'session-resume', turns: [canonicalTurn('completed')], runtimeCanResume: false,
  }), false, 'completed sessions do not show continue');
  assert.equal(policy.composerCanContinue({
    sessionId: 'session-resume', turns: [canonicalTurn('failed')], runtimeCanResume: true,
  }), true, 'recoverable runtime state shows continue');
  assert.equal(policy.composerCanContinue({
    sessionId: 'session-resume', turns: [canonicalTurn('cancelled', [{
      kind: 'user_message', content: '原任务', worker: { taskId: 'task-1' },
      metadata: { interruptionSource: 'user' },
    }])], runtimeCanResume: false,
  }), true, 'a user-stopped task shows continue after reload');
  assert.equal(policy.composerCanContinue({
    sessionId: 'session-resume', turns: [canonicalTurn('cancelled', [{
      kind: 'user_message', content: '原任务', worker: { taskId: 'task-1' },
    }])], runtimeCanResume: false,
  }), false, 'an unrelated cancelled turn does not show continue');

  const failedWithTask = canonicalTurn('failed', [{ kind: 'user_message', content: '原任务', worker: { taskId: 'task-1' } }]);
  assert.equal(policy.composerCanContinue({ sessionId: 'session-resume', turns: [failedWithTask], runtimeCanResume: false }), true);
  assert.equal(policy.composerCanContinue({ sessionId: 'other', turns: [failedWithTask], runtimeCanResume: true }), false);
  for (const status of ['pending', 'running', 'blocked', 'completed', 'superseded']) {
    assert.equal(policy.composerCanContinue({ sessionId: 'session-resume', turns: [canonicalTurn(status)], runtimeCanResume: true }), false,
      `${status} cannot resume even if a stale runtime projection says it can`);
  }
  assert.equal(policy.composerCanContinue({ sessionId: 'session-resume', turns: [
    failedWithTask, { ...canonicalTurn('completed'), turnSeq: 2 },
  ], runtimeCanResume: true }), false, 'new completed mainline supersedes earlier failed work');
  assert.equal(policy.composerCanContinue({ sessionId: 'session-resume', turns: [
    failedWithTask, { ...canonicalTurn('completed'), turnSeq: 2, metadata: { agentExecution: { taskId: 'child' } } },
  ], runtimeCanResume: false }), true, 'child terminal state cannot replace the current mainline');
  assert.equal(policy.composerCanContinue({ sessionId: 'session-resume', turns: [
    canonicalTurn('interrupted', [{ metadata: { noticeKind: 'session_interrupted', recoveryState: 'ready' } }]),
  ], runtimeCanResume: false }), true, 'restart readiness is an authoritative recovery fact');
  assert.equal(policy.isLatestMainlineTurn({
    sessionId: 'session-resume', turnId: failedWithTask.turnId, turns: [
      failedWithTask,
      { ...canonicalTurn('cancelled', [{ kind: 'user_message', content: '继续', metadata: { route: 'continue' } }]), turnSeq: 2 },
    ],
  }), false, 'a hidden continue turn still makes the interrupted turn non-editable');
  assert.equal(policy.isLatestMainlineTurn({
    sessionId: 'session-resume', turnId: failedWithTask.turnId, turns: [failedWithTask],
  }), true, 'the latest stopped mainline remains editable before continuation');

  assert.equal(policy.formatImageSize(0), '');
  assert.equal(policy.formatImageSize(512), '512 B');
  assert.equal(policy.formatImageSize(2048), '2.0 KB');
  assert.equal(policy.formatImageSize(3 * mb), '3.0 MB');

  // 排队消息：附件要在卡片上可见，带标注的不能取回编辑。
  assert.deepEqual(
    policy.summarizeQueuedMessage({
      images: [{}, {}],
      contextReferences: [{}],
      browserNodeSelections: [{}],
      browserAnnotationRefs: ['a'],
    }),
    { images: 2, references: 2, annotations: 1 },
  );
  assert.deepEqual(policy.summarizeQueuedMessage({}), {
    images: 0,
    references: 0,
    annotations: 0,
  });
  assert.equal(policy.queuedMessageEditable({ images: [{}] }), true);
  assert.equal(policy.queuedMessageEditable({ browserAnnotationRefs: ['a'] }), false);

  console.log('composer policy golden tests passed');
});
