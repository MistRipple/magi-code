import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => value;

await withGoldenViteServer(async (server) => {
  const actions = await server.ssrLoadModule('/src/web/web-model-actions.ts');
  const setup = await server.ssrLoadModule('/src/web/web-model-setup.ts');
  for (const [label, projection, available] of [
    ['未安装或未激活', { manifests: [], commands: [] }, false],
    ['旧版未贡献配置命令', { manifests: [{ id: 'openai.chatgpt-web' }], commands: [] }, false],
    ['残留命令无激活清单', { manifests: [], commands: [{ id: setup.WEB_MODEL_SETUP_COMMAND }] }, false],
    ['其他插件的同名命令', { manifests: [{ id: 'openai.chatgpt-web' }], commands: [{ id: 'plugin/other/setup' }] }, false],
    ['激活且贡献配置命令', { manifests: [{ id: 'openai.chatgpt-web' }], commands: [{ id: setup.WEB_MODEL_SETUP_COMMAND }] }, true],
  ]) {
    assert.equal(setup.webModelSetupAvailable(projection), available, label);
    assert.equal(setup.isWebModelSetupRequest('帮我配置 GPT Web', setup.webModelSetupAvailable(projection)), available, label);
  }
  for (const text of ['帮我配置 GPT Web', '请帮我配置一下gpt web插件', '配置 GPT Web', 'Please help me configure GPT Web']) {
    assert.equal(setup.isWebModelSetupRequest(text, true), true, text);
    assert.equal(setup.isWebModelSetupRequest(text, false), false, '未激活插件不拦截普通消息：' + text);
  }
  for (const text of ['不要配置 GPT Web', 'GPT Web 怎么配置？', '解释配置 GPT Web 的代码', 'configure GPT Web and delete files', 'sk-private-configure-gpt-web']) {
    assert.equal(setup.isWebModelSetupRequest(text, true), false, text);
  }
  for (const stage of ['tunnel', 'credentials', 'connecting']) assert.equal(setup.setupCanEditCredentials({ stage }), true);
  for (const stage of ['login', 'complete', 'cancelled', 'busy', 'failed']) assert.equal(setup.setupCanEditCredentials({ stage }), false);
  assert.equal(setup.setupCanEditCredentials({ stage: 'connector', reason: 'agent_handoff' }), false);
  const { buildStoreItems, storeGroup } = await server.ssrLoadModule('/src/lib/plugin-store.ts');
  const oldPlugin = { manifest: { id: 'openai.chatgpt-web', name: 'GPT Web', version: '1.1.0' }, activeScopes: ['application'], draining: [] };
  const bundled = { ...oldPlugin.manifest, version: '1.2.0' };
  const [upgrade] = buildStoreItems([oldPlugin], [bundled], [], (manifest, field) => manifest[field] || '');
  assert.equal(upgrade.updatable, true, '内置新版必须向旧版用户提供升级入口');
  assert.equal(storeGroup(upgrade), 'updatable');
  const [same] = buildStoreItems([oldPlugin], [oldPlugin.manifest], [], (manifest, field) => manifest[field] || '');
  assert.equal(same.updatable, false);
  for (const stage of ['complete', 'cancelled', 'busy']) assert.equal(setup.setupIsActive({ stage }), false);
  for (const stage of ['login', 'tunnel', 'credentials', 'connecting', 'connector', 'failed']) assert.equal(setup.setupIsActive({ stage }), true);
  assert.equal(setup.setupReasonKey('untrusted page text'), null);
  assert.equal(setup.setupReasonKey('key_generation_required'), 'webModel.setup.reason.keyReady');
  assert.equal(setup.setupReasonKey('user_takeover'), 'webModel.setup.reason.userTakeover');
  for (const [stage, reason, expected] of [
    ['credentials', 'key_generation_required', 'reason.keyReady'],
    ['credentials', 'runtime_key_required', 'reason.keyPreparation'],
    ['tunnel', 'user_takeover', 'reason.userTakeover'],
    ['connector', 'user_takeover', 'reason.userTakeover'],
    ['tunnel', 'agent_handoff', 'hint.tunnel'],
    ['connector', 'agent_handoff', 'hint.connector'],
    ['login', 'chatgpt_login_required', 'hint.login'],
    ['complete', '', 'hint.complete'],
    ['cancelled', '', 'hint.cancelled'],
    ['busy', 'web_session_busy', 'hint.busy'],
    ['failed', 'agent_failed', 'hint.failed'],
  ]) {
    assert.equal(setup.setupHintKey({ stage, reason }, false), `webModel.setup.${expected}`);
    assert.equal(setup.setupHintKey({ stage, reason }, true), 'webModel.setup.workingHint');
  }
  assert.equal(setup.setupHintKey({ stage: 'connecting', reason: 'runtime_not_ready' }, false), 'webModel.setup.hint.connecting');
  assert.equal(setup.setupHintKey({ stage: 'connecting', reason: 'stop_failed' }, false, true), 'webModel.setup.reason.connectionFailed');
  for (const stage of ['tunnel', 'credentials', 'connecting', 'connector']) {
    const fresh = setup.setupCredentialInput(stage, 'tunnel_new', '', false);
    assert.equal(fresh.required, stage !== 'tunnel');
    assert.equal(fresh.placeholderKey, stage === 'tunnel' ? 'webModel.setup.optionalKeyPlaceholder' : 'webModel.setup.keyPlaceholder');
    assert.deepEqual(setup.setupCredentialInput(stage, 'tunnel_saved', 'tunnel_saved', true), {
      required: false, placeholderKey: 'webModel.setup.savedKeyPlaceholder',
    });
    assert.deepEqual(setup.setupCredentialInput(stage, 'tunnel_new', 'tunnel_saved', true), {
      required: true, placeholderKey: 'webModel.setup.keyPlaceholder',
    }, '更换 Tunnel 时不能提示沿用旧密钥');
  }
  const commands = await server.ssrLoadModule('/src/lib/user-message-command.ts');
  assert.equal(commands.isWebModelSetupCommand({ pluginCommandId: setup.WEB_MODEL_SETUP_COMMAND }), true);
  assert.equal(commands.isWebModelSetupCommand({ 'magi.pluginCommandId': setup.WEB_MODEL_SETUP_COMMAND }), true);
  assert.equal(commands.isWebModelSetupCommand({ pluginCommandId: 'plugin/other/setup' }), false);

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
    web_send_unconfirmed: 'openView',
    web_write_not_confirmed: 'retry',
    web_attach_failed: 'retry',
    web_turn_timeout: 'retry',
    web_quota_exhausted: 'switchModel',
    web_tunnel_unavailable: 'openTunnelSettings',
    web_plugin_inactive: 'openPluginSettings',
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
  assert.equal(pick({ slotOwnerSessionId: 's1', toolsAvailable: true }), null,
    '个人 GPT Web 会话不因无项目而显示工具不可用');
});

console.log('web model golden replay passed');
