<script lang="ts">
  import { untrack } from 'svelte';
  import type { WebModelSetupStatus } from '../shared/app-server-protocol.generated';
  import { getWebModelSetup, getWebModelSetupReceipt, saveWebModelSetupCredentials, setupIsActive, setupCanEditCredentials, setupHintKey, setupCredentialInput, webModelSetupAvailable, WEB_MODEL_SETUP_UPDATED, type SetupAction } from '../web/web-model-setup';
  import { dispatchWebModelAction } from '../web/web-model-actions';
  import { i18n } from '../stores/i18n.svelte';
  import { getWebModelTunnel } from '../web/agent-api';
  import Icon from './Icon.svelte';
  import { APPLICATION_PLUGIN_SCOPE, pluginProjection, trackPluginProjection } from '../stores/plugin-registry.svelte';

  $effect(() => { trackPluginProjection(APPLICATION_PLUGIN_SCOPE); });
  const pluginActive = $derived(webModelSetupAvailable(pluginProjection(APPLICATION_PLUGIN_SCOPE)));
  let { sessionId, busy, status = $bindable(null), onaction }: {
    sessionId: string;
    busy: boolean;
    status?: WebModelSetupStatus | null;
    onaction: (action: SetupAction) => Promise<void>;
  } = $props();
  let apiKey = $state('');
  let tunnelId = $state('');
  let savedTunnelId = $state('');
  let hasSavedKey = $state(false);
  let saving = $state(false);
  let error = $state('');
  let dismissed = $state(false);
  let collapsed = $state(false);
  let syncError = $state(false);
  let channelReady = $state(false);
  let channelCode = $state('');
  let channelDetail = $state('');
  // 用代次隔离切换会话/停用后的迟到响应；凭据草稿只在组件内存中存在。
  let generation = 0;
  const localDesktop = typeof window !== 'undefined' && window.magiDesktop?.runtime === 'electron';
  const setupRunning = $derived(status?.runState === 'running');

  $effect(() => {
    const current = sessionId;
    const epoch = ++generation;
    status = null;
    apiKey = '';
    tunnelId = '';
    savedTunnelId = '';
    hasSavedKey = false;

    saving = false;
    dismissed = false;
    error = '';
    channelReady = false;
    channelCode = '';
    channelDetail = '';
    if (!pluginActive || !current) return;
    let disposed = false;
    let firstSync = true;
    let inFlight = false;
    const refresh = async () => {
      if (inFlight || saving) return;
      inFlight = true;
      try {
        const next = await getWebModelSetup(current);
        if (disposed || epoch !== generation || saving) return;
        if (next && status && next.sessionId===status.sessionId && next.updatedAt<status.updatedAt) return;
        // 快速重配可能在两次轮询间已完成；新服务端记录同样需要重新显示，不能沿用旧卡片的收起状态。
        if (next?.setupRunId !== status?.setupRunId || next?.handoffId !== status?.handoffId || next?.revision !== status?.revision) {
          apiKey = '';
          dismissed = false;
          if (next?.handoffId !== status?.handoffId || next?.setupRunId !== status?.setupRunId) collapsed = next?.runState !== 'waiting_user';
        }
        if (next && !setupIsActive(next)) {
          collapsed = true;
          if (firstSync) dismissed = true;
        }
        firstSync = false;
        syncError = false;
        status = next;
        if (!setupCanEditCredentials(next)) { apiKey = '';  }
        if (next && localDesktop) {
          const tunnel = await getWebModelTunnel();
          if (disposed || epoch !== generation || saving) return;
          // 只回填 ID 与“是否保存密钥”的事实，不读取密钥内容或遮罩预览。
          if (tunnelId === savedTunnelId) tunnelId = tunnel.tunnelId;
          savedTunnelId = tunnel.tunnelId;
          hasSavedKey = tunnel.hasApiKey;
          channelReady = tunnel.ready;
          channelCode = tunnel.code;
          channelDetail = tunnel.detail;
        }
      } catch {
        if (!disposed && epoch === generation) syncError = true;
      } finally { inFlight = false; }
    };
    untrack(() => void refresh());
    // SSE 只触发读取权威快照。订阅先于初次读取完成，轮询补偿断线和在途更新。
    const onUpdated = (event: Event) => {
      if ((event as CustomEvent<{ sessionId?: string }>).detail?.sessionId === current) void refresh();
    };
    window.addEventListener(WEB_MODEL_SETUP_UPDATED, onUpdated);
    const timer = window.setInterval(() => void refresh(), 1800);
    return () => { disposed = true; window.clearInterval(timer); window.removeEventListener(WEB_MODEL_SETUP_UPDATED, onUpdated); apiKey = ''; };
  });

  async function saveCredentials(): Promise<void> {
    if (saving || busy) return;
    const current = sessionId;
    const epoch = generation;
    saving = true;
    error = '';
    const secret = apiKey;
    const requestId = crypto.randomUUID();
    let credentialsSaved = false;
    apiKey = '';
    try {
      if (!status?.setupRunId || status.revision === undefined || !status.handoffId) throw new Error('配置交接已过期，请刷新后重试');
      const receipt = await saveWebModelSetupCredentials({
        sessionId: current, setupRunId: status.setupRunId, expectedRevision: status.revision,
        handoffId: status.handoffId, requestId, tunnelId: tunnelId.trim(), apiKey: secret,
        continueAfterSave: true,
      });
      credentialsSaved = receipt.saved;
      if (!receipt.saved) throw new Error('save_unconfirmed');
      if (epoch !== generation || current !== sessionId) return;
      const tunnel = await getWebModelTunnel();
      if (epoch !== generation || current !== sessionId) return;
      tunnelId = tunnel.tunnelId;
      savedTunnelId = tunnel.tunnelId;
      hasSavedKey = tunnel.hasApiKey;
      channelReady = tunnel.ready;
      channelCode = tunnel.code;
      channelDetail = tunnel.detail;
      const next = await getWebModelSetup(current);
      if (epoch !== generation || current !== sessionId) return;
      status = next;

      // daemon 已接纳一次继续意图；UI 不再另发第二次继续。
    } catch {
      // 响应可能在保存后丢失：按 requestId 查询脱敏回执，不再次提交密钥。
      if (!credentialsSaved) {
        try {
          const receipt = await getWebModelSetupReceipt(current, requestId);
          credentialsSaved = receipt.saved;
          if (epoch === generation && current === sessionId && (!status || receipt.configuration.updatedAt >= status.updatedAt)) status = receipt.configuration;
        } catch { /* 回执未知，保留明确的待核对提示。 */ }
      }
      if (epoch === generation && current === sessionId) error = i18n.t(credentialsSaved ? 'webModel.setup.continueFailed' : 'webModel.setup.credentialsFailed');
    } finally { if (epoch === generation) saving = false; }
  }
  const step = $derived(status?.stage || 'login');
  const terminal = $derived(status && !setupIsActive(status) && step !== 'busy');
  const canRetryChannel = $derived(step === 'connecting' && !channelReady && !['', 'client_installing', 'runtime_not_ready'].includes(channelCode));
  // 只有密钥表单已准备（或用户明确要手动填写）时才展示安全输入框；
  // Agent 交还用户处理表单时还没有密钥可粘贴，不能提示“等待安全输入”。
  const showCredentials = $derived(localDesktop && setupCanEditCredentials(status) && !collapsed);
  const keyNeedsPreparation = $derived(step === 'credentials' && status?.reason === 'runtime_key_required');
  const credentialInput = $derived(setupCredentialInput(step, tunnelId, savedTunnelId, hasSavedKey));
  const keyRequired = $derived(credentialInput.required);
</script>

{#if pluginActive && status && !dismissed}
  <section class="setup-panel" aria-label={i18n.t('webModel.setup.title')} data-web-model-setup={step} aria-busy={setupRunning || saving}>
    <div class="setup-heading">
      <button type="button" class="btn btn--sm btn--secondary" aria-expanded={!collapsed}
        onclick={() => { collapsed = !collapsed; if (collapsed) { apiKey = '';  } }}>
        <Icon name={step === 'complete' ? 'check' : 'globe'} size={14} />{i18n.t('webModel.setup.title')}
        <span>{i18n.t(collapsed ? 'webModel.setup.expand' : 'webModel.setup.collapse')}</span>
        {#if !collapsed && showCredentials}<span class="secure-hint">{i18n.t('webModel.setup.collapseClearsKey')}</span>{/if}
      </button>
      <span>{i18n.t(`webModel.setup.state.${status.runState ?? 'paused'}`)}</span>
    </div>
    {#if syncError}<p class="credentials-error" role="status">{i18n.t('webModel.setup.syncFailed')}</p>{/if}
    {#if status.continueState === 'waiting_channel'}<p role="status">{i18n.t('webModel.setup.waitingChannel')}</p>{/if}
    {#if status.continueState === 'saved_not_continued'}<p role="status">{i18n.t('webModel.setup.continueFailed')}</p>{/if}
    {#if !collapsed}
    <p aria-live="polite">{!setupRunning && status.detail ? status.detail : i18n.t(setupHintKey(status, setupRunning, canRetryChannel))}</p>
    {#if !localDesktop}<p role="status">{i18n.t('settings.browser.webModel.needDesktop')}</p>{/if}
    {#if status.organization || status.project}<p class="reason">{status.organization}{status.organization && status.project ? ' / ' : ''}{status.project}</p>{/if}
    {#if !setupRunning && canRetryChannel && channelDetail}<p class="reason" role="status">{channelDetail}</p>{/if}
    {#if error && !showCredentials}<p class="credentials-error" role="alert">{error}</p>{/if}
    {#if localDesktop && keyNeedsPreparation}<button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => void onaction('continue')}>{i18n.t('webModel.setup.prepareKey')}</button>{/if}
    {#if terminal && localDesktop && channelCode && !channelReady}<p class="credentials-error" role="status">{channelDetail}</p>{/if}
    {#if showCredentials}
      <form class="credentials-card" data-web-model-setup-credentials onsubmit={(event) => { event.preventDefault(); void saveCredentials(); }}>
        <div class="credentials-heading"><Icon name="shield" size={14} /><strong>{i18n.t('webModel.setup.secureTitle')}</strong></div>
        <label>{i18n.t('webModel.setup.tunnelId')}<input class="form-input" name="tunnelId" bind:value={tunnelId} data-web-model-tunnel-credential="1" autocomplete="off" spellcheck="false" placeholder="tunnel_…" required disabled={busy || saving} /></label>
        <label>{i18n.t('webModel.setup.apiKey')}<input class="form-input" name="apiKey" type="password" bind:value={apiKey} data-web-model-tunnel-credential="1" autocomplete="new-password" spellcheck="false" placeholder={i18n.t(credentialInput.placeholderKey)} required={keyRequired} disabled={busy || saving} /></label>
        <span class="secure-hint">{i18n.t('webModel.setup.secretHint')}</span>
        {#if error}<p class="credentials-error" role="alert">{error}</p>{/if}
        <div class="credential-actions">
          <button class="btn btn--sm btn--primary" type="submit" disabled={busy || saving || !tunnelId.trim() || (keyRequired && !apiKey.trim())}>{i18n.t(saving ? 'webModel.setup.saving' : 'webModel.setup.saveContinue')}</button>
          {#if step === 'credentials' && status?.reason === 'key_generation_required'}<button class="btn btn--sm btn--secondary" type="button" disabled={busy || saving} onclick={() => void onaction('continue')}>{i18n.t('webModel.setup.prepareKey')}</button>{/if}
          {#if step !== 'credentials'}<button class="btn btn--sm btn--secondary" type="button" disabled={saving} onclick={() => {  apiKey = ''; error = ''; tunnelId = savedTunnelId; }}>{i18n.t('webModel.setup.cancelEdit')}</button>{/if}
        </div>
      </form>
    {/if}
    <div class="setup-actions">
      {#if localDesktop && !terminal}
        <button class="btn btn--sm btn--secondary" disabled={saving} onclick={() => dispatchWebModelAction({ kind: 'openView' })}>{i18n.t('webModel.setup.showPage')}</button>

        {#if status.allowedActions?.includes('pause')}
          <button class="btn btn--sm btn--secondary" disabled={saving} onclick={() => void onaction('pause')}>{i18n.t('webModel.setup.pause')}</button>
        {/if}
        {#if !showCredentials && status.allowedActions?.includes('continue')}
          <button class="btn btn--sm btn--primary" disabled={busy || saving || (step === 'connecting' && !channelReady && !canRetryChannel)} onclick={() => void onaction('continue')}>
            {busy ? i18n.t('webModel.setup.working') : i18n.t(canRetryChannel ? 'webModel.setup.retryConnection' : 'webModel.setup.continue')}
          </button>
        {/if}
        <button class="btn btn--sm btn--secondary" onclick={() => { apiKey = ''; void onaction('cancel'); }}>{i18n.t('webModel.setup.cancel')}</button>
      {:else}
        {#if localDesktop && status.allowedActions?.includes('start')}<button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => void onaction('start')}>{i18n.t('webModel.setup.restart')}</button>{/if}
        <button class="btn btn--sm btn--secondary" onclick={() => { apiKey = ''; dismissed = true; }}>{i18n.t('webModel.setup.dismiss')}</button>
      {/if}
    </div>
    {/if}
  </section>
{/if}

<style>
  .setup-panel { border: var(--magi-hairline) solid var(--border); border-radius: 10px; padding: 14px 16px; margin: 0 0 8px; min-width: 0; max-height: min(60vh, 520px); overflow-y: auto; font-size: 12px; }
  .setup-heading, .setup-actions, .credentials-heading, .credential-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .setup-heading { justify-content: space-between; gap: 6px 12px; }
  .setup-heading > span { color: var(--foreground-muted); }
  p { margin: 8px 0; line-height: 1.6; overflow-wrap: anywhere; }
  .reason, .secure-hint { color: var(--foreground-muted); }
  .credentials-card { display: grid; gap: 12px; border: var(--magi-hairline) solid var(--border); border-radius: 8px; padding: 12px; margin: 12px 0; min-width: 0; }
  label { display: grid; gap: 6px; min-width: 0; }
  input { min-width: 0; width: 100%; box-sizing: border-box; }
  .secure-hint { line-height: 1.5; overflow-wrap: anywhere; }
  .credentials-error { margin: 0; color: var(--error); }
  .setup-actions { margin-top: 12px; }
  .setup-actions .btn, .credential-actions .btn { white-space: normal; }
  @media (max-width: 640px) { .setup-panel { padding: 12px; } }
</style>
