import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const policy = await server.ssrLoadModule('/src/lib/notification-policy.ts');

  assert.deepEqual(policy.resolveFeedbackPolicy('success'), {
    category: 'feedback',
    persistToCenter: false,
    countUnread: false,
    actionRequired: false,
    displayMode: 'silent',
  });

  assert.equal(policy.resolveFeedbackPolicy('info').displayMode, 'silent');
  assert.equal(policy.resolveFeedbackPolicy('warning').displayMode, 'toast');
  assert.equal(policy.resolveFeedbackPolicy('error').displayMode, 'toast');
  assert.equal(policy.shouldDisplayToast('success'), false);
  assert.equal(policy.shouldDisplayToast('info'), false);
  assert.equal(policy.shouldDisplayToast('warning'), true);
  assert.equal(policy.shouldDisplayToast('error'), true);

  assert.deepEqual(policy.resolveIncidentPolicy({ scope: 'workspace' }), {
    category: 'incident',
    persistToCenter: true,
    countUnread: true,
    actionRequired: true,
    displayMode: 'silent',
    scope: 'workspace',
  });

  assert.deepEqual(
    policy.buildIncidentRequest(
      {
        scope: 'app',
        level: 'error',
        message: '前端运行时异常',
        source: 'message-handler',
      },
      {
        workspaceId: 'workspace-a',
        workspacePath: '/tmp/workspace-a',
      },
    ),
    {
      scope: 'app',
      level: 'error',
      message: '前端运行时异常',
      source: 'message-handler',
      workspaceId: 'workspace-a',
      workspacePath: '/tmp/workspace-a',
    },
  );

  assert.equal(
    policy.buildIncidentRequest(
      { scope: 'workspace', level: 'error', message: '工作区索引失败' },
      { workspaceId: 'workspace-a', sessionId: 'session-a' },
    ).sessionId,
    'session-a',
    'workspace incident should keep the current session as response context without changing record scope',
  );

  assert.deepEqual(
    policy.buildIncidentRequest(
      {
        scope: 'session',
        level: 'warning',
        message: '任务执行失败',
        fingerprint: 'task-failed',
      },
      {
        workspaceId: 'workspace-a',
        workspacePath: '/tmp/workspace-a',
        sessionId: 'session-a',
      },
    ),
    {
      scope: 'session',
      level: 'warning',
      message: '任务执行失败',
      fingerprint: 'task-failed',
      workspaceId: 'workspace-a',
      workspacePath: '/tmp/workspace-a',
      sessionId: 'session-a',
    },
  );

  assert.throws(
    () => policy.buildIncidentRequest(
      { scope: 'session', level: 'error', message: '缺少会话' },
      { workspaceId: 'workspace-a' },
    ),
    /session incident requires sessionId/,
  );

  const records = policy.normalizeIncidentRecords([
    {
      notificationId: 'incident-1',
      kind: 'incident',
      level: 'error',
      message: '连接失败',
      scope: 'workspace',
      workspaceId: 'workspace-a',
      createdAt: 10,
      read: false,
      countUnread: true,
      occurrenceCount: 2,
    },
    {
      notificationId: 'audit-1',
      kind: 'audit',
      level: 'success',
      message: '配置已保存',
      createdAt: 20,
    },
    {
      notificationId: 'invalid-scope',
      kind: 'incident',
      scope: 'project',
      level: 'error',
      message: '非法作用域不应进入通知中心',
      createdAt: 30,
    },
  ]);

  assert.equal(records.length, 1);
  assert.deepEqual(records[0], {
    id: 'incident-1',
    type: 'error',
    message: '连接失败',
    scope: 'workspace',
    workspaceId: 'workspace-a',
    sessionId: undefined,
    source: undefined,
    actionRequired: true,
    countUnread: true,
    occurrenceCount: 2,
    timestamp: 10,
    read: false,
    resolved: false,
    title: undefined,
  });

  console.log('notification policy golden passed');
});
