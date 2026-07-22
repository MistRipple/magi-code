import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

await withGoldenViteServer(async (server) => {
  const posted = [];
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  bridgeRuntime.setClientBridge({
    kind: 'web',
    postMessage(message) { posted.push(message); },
    onMessage() { return () => {}; },
    getState() { return undefined; },
    setState() {},
    getInitialSessionId() { return ''; },
    getInitialLocale() { return 'zh-CN'; },
    notifyReady() {},
  });

  const store = await server.ssrLoadModule('/src/stores/messages.svelte.ts');
  const notifications = await server.ssrLoadModule('/src/lib/notifications.ts');

  store.messagesState.currentWorkspaceId = 'workspace-notification-runtime';
  store.messagesState.currentWorkspacePath = '/tmp/workspace-notification-runtime';
  store.messagesState.currentSessionId = 'session-notification-runtime';

  notifications.showFeedback('success', '配置已保存', { source: 'settings-panel' });
  assert.equal(posted.length, 0, 'normal feedback must never persist to the notification center');
  assert.equal(store.getToasts().length, 0, 'routine success feedback must stay silent');

  notifications.showFeedback('info', '消息已发送', { source: 'bridge-runtime' });
  assert.equal(store.getToasts().length, 0, 'routine conversation feedback must stay silent');

  notifications.showFeedback('warning', '上下文即将达到上限', { source: 'model-runtime' });
  assert.equal(store.getToasts().length, 1, 'warnings must remain visible');

  assert.equal(notifications.reportIncident('模型请求失败', {
    scope: 'workspace',
    source: 'model-runtime',
    fingerprint: 'model-request-failed',
  }), true);
  assert.equal(posted.length, 1);
  assert.equal(posted[0].type, 'reportIncident');
  assert.equal(posted[0].incident.scope, 'workspace');
  assert.equal(posted[0].incident.workspaceId, 'workspace-notification-runtime');
  assert.equal(posted[0].incident.sessionId, 'session-notification-runtime');
  assert.equal(
    store.getToasts().length,
    1,
    '持久化到通知中心的 incident 不得额外触发右下角 toast',
  );

  store.applyNotificationsSnapshot('session-notification-runtime', {
    records: Array.from({ length: 4 }, (_, index) => ({
      notificationId: `incident-runtime-${index + 1}`,
      kind: 'incident',
      scope: 'workspace',
      level: index === 0 ? 'error' : 'warning',
      message: index === 0 ? '模型请求失败' : `运行时异常 ${index + 1}`,
      workspaceId: 'workspace-notification-runtime',
      createdAt: 10 + index,
      read: false,
      handled: false,
      resolved: false,
      actionRequired: true,
      countUnread: true,
      occurrenceCount: index === 0 ? 3 : 1,
    })),
  }, 'workspace-notification-runtime');
  assert.equal(store.getNotifications().length, 4);
  assert.equal(store.getUnreadNotificationCount(), 4);
  assert.equal(store.getNotifications()[0].occurrenceCount, 3);
  assert.deepEqual(
    store.getNotifications().map((item) => item.id),
    ['incident-runtime-1', 'incident-runtime-2', 'incident-runtime-3', 'incident-runtime-4'],
    'notification center must render the complete authoritative collection',
  );

  store.resolveNotification('incident-runtime-1');
  assert.equal(posted[1].type, 'resolveNotification');
  assert.equal(posted[1].notificationId, 'incident-runtime-1');

  console.log('notification runtime golden passed');
});
