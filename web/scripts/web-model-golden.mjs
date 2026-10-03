import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const actions = await server.ssrLoadModule('/src/web/web-model-actions.ts');

  // 设计基线 §12：每个错误码只映射到一个主行动；没有用户动作的码不出按钮。
  const expected = {
    web_session_busy: 'releaseSession',
    web_context_lost: 'switchModel',
    web_saved_conversation_unavailable: 'switchModel',
    web_saved_conversation_conflict: 'openView',
    web_login_expired: 'openView',
    web_site_blocked: 'openView',
    web_selectors_drift: 'probe',
    web_desktop_unavailable: 'openSettings',
    web_send_rejected: 'retry',
    web_write_not_confirmed: 'retry',
    web_turn_timeout: 'retry',
    web_quota_exhausted: 'switchModel',
    web_tunnel_unavailable: 'openTunnelSettings',
  };
  for (const [code, kind] of Object.entries(expected)) {
    assert.equal(actions.webModelFailureAction(code)?.kind, kind, code);
  }
  for (const code of ['magi_tool_timeout', 'magi_tool_denied', 'magi_tool_unavailable', 'unknown']) {
    assert.equal(actions.webModelFailureAction(code), null, code);
  }
  // 旧 T2 / 排队 / 轮数上限错误码不再存在。
  for (const code of ['web_tool_protocol_invalid', 'web_tool_round_limit', 'web_queue_full', 'web_queue_timeout']) {
    assert.equal(actions.webModelFailureAction(code), null, code);
  }

  const banner = await server.ssrLoadModule('/src/web/web-model-session-banner.ts');
  const base = {
    usesWeb: true, projection: { mode: 'temporary', syncState: 'active', hasRemoteConversation: false },
    slotOwnerSessionId: null, sessionId: 's1', turnActive: false, runtimeFresh: true,
    toolsAvailable: true,
  };
  const pick = (patch) => banner.resolveWebSessionBanner({ ...base, ...patch });
  assert.equal(pick({ usesWeb: false }), null);
  assert.equal(pick({ slotOwnerSessionId: 's1' }), null, '持有槽位的临时会话正常');
  assert.equal(pick({}).id, 'lost', '有历史却没持有槽位 = 网页对话已失效');
  assert.equal(pick({}).blocksSend, true);
  assert.equal(pick({ runtimeFresh: false }), null, '占用信息未刷新时不下失效结论');
  assert.equal(pick({ turnActive: true }), null, '推理进行中不判失效');
  const neverAccepted = { projection: { mode: 'temporary', syncState: 'unbound', hasRemoteConversation: false } };
  assert.equal(pick(neverAccepted), null, '第一条消息就失败的会话网页里没有上下文，不判失效，可直接重试');
  assert.equal(pick({ ...neverAccepted, slotOwnerSessionId: 's2' }).id, 'busy');
  assert.equal(
    pick({ ...neverAccepted, slotOwnerSessionId: 's2' }).action.labelKey,
    'webModel.action.releaseAndUse',
    '被占用时的主行动是「释放并使用」，后果在提示文字里说明',
  );
  assert.equal(pick({ slotOwnerSessionId: 's2' }).id, 'lost', '失效优先于占用提示');
  const saved = (syncState, extra = {}) => pick({
    projection: { mode: 'saved', syncState, hasRemoteConversation: true, remoteTitle: '标题', ...extra },
  });
  assert.equal(saved('conflict').id, 'conflict');
  assert.equal(saved('stale').id, 'unavailable');
  assert.equal(saved('active'), null, '已保存对话不因没持有槽位而失效（可按 conversation_id 重绑）');
  assert.equal(saved('active', { remoteTitle: null }).id, 'unsynced');
  assert.equal(saved('active', { remoteTitle: null }).blocksSend, false);
  const noTools = pick({ slotOwnerSessionId: 's1', toolsAvailable: false, toolsDetail: '缺凭据' });
  assert.equal(noTools.id, 'noTools');
  assert.equal(noTools.blocksSend, false, '没有项目工具不阻止纯对话');
});

console.log('web model golden replay passed');
