<script lang="ts">
  import { untrack } from 'svelte';
  import type { WebModelSetupStatus } from '../shared/app-server-protocol.generated';
  import { getWebModelSetup, saveWebModelSetupCredentials, setupIsActive, setupNeedsConnectorConsent, setupCanRetryTunnel, setupCanEditCredentials, setupHintKey, setupCredentialInput, webModelSetupAvailable, type SetupAction } from '../web/web-model-setup';
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
  let editingCredentials = $state(false);
  let saving = $state(false);
  let error = $state('');
  let dismissed = $state(false);
  let channelReady = $state(false);
  let channelCode = $state('');
  let channelDetail = $state('');
  // 用代次隔离切换会话/停用后的迟到响应；凭据草稿只在组件内存中存在。
  let generation = 0;

  $effect(() => {
    const current = sessionId;
    const epoch = ++generation;
    status = null;
    apiKey = '';
    tunnelId = '';
    savedTunnelId = '';
    hasSavedKey = false;
    editingCredentials = false;
    saving = false;
    dismissed = false;
    error = '';
    channelReady = false;
    channelCode = '';
    channelDetail = '';
    if (!pluginActive || !current || window.magiDesktop?.runtime !== 'electron') return;
    let disposed = false;
    let inFlight = false;
    const refresh = async () => {
      if (inFlight || saving) return;
      inFlight = true;
      try {
        const next = await getWebModelSetup(current);
        if (disposed || epoch !== generation || saving) return;
        // 快速重配可能在两次轮询间已完成；新服务端记录同样需要重新显示，不能沿用旧卡片的收起状态。
        if (setupIsActive(next) || (next && next.updatedAt !== status?.updatedAt)) dismissed = false;
        status = next;
        if (!setupCanEditCredentials(next)) { apiKey = ''; editingCredentials = false; }
        if (setupCanEditCredentials(next)) {
          const tunnel = await getWebModelTunnel();
          if (disposed || epoch !== generation || saving) return;
          // 只回填 ID 与“是否保存密钥”的事实，不读取密钥内容或遮罩预览。
          if (tunnelId === savedTunnelId) tunnelId = tunnel.tunnelId;
          savedTunnelId = tunnel.tunnelId;
          hasSavedKey = tunnel.hasApiKey;
          channelReady = tunnel.ready;
          channelCode = tunnel.code;
          channelDetail = tunnel.detail;
          if (next?.stage === 'connecting' && tunnel.ready && !busy && !saving && !editingCredentials) {
            await onaction('continue');
          }
        }
      } catch {
        // 下一次成功响应恢复服务端配置事实。
      } finally { inFlight = false; }
    };
    untrack(() => void refresh());
    const timer = window.setInterval(() => void refresh(), 1800);
    return () => { disposed = true; window.clearInterval(timer); apiKey = ''; };
  });

  async function saveCredentials(): Promise<void> {
    if (saving || busy) return;
    const current = sessionId;
    const epoch = generation;
    saving = true;
    error = '';
    const secret = apiKey;
    let credentialsSaved = false;
    apiKey = '';
    try {
      await saveWebModelSetupCredentials(current, tunnelId, secret);
      credentialsSaved = true;
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
      editingCredentials = false;
      // 只接续控制命令；ID 与密钥不进入 Turn、模型或插件命令参数。
      await onaction('continue');
    } catch {
      // 不透传网络/平台错误正文，避免任何凭据原文进入诊断或对话。
      if (epoch === generation && current === sessionId) error = i18n.t(credentialsSaved ? 'webModel.setup.continueFailed' : 'webModel.setup.credentialsFailed');
    } finally { if (epoch === generation) saving = false; }
  }
  const step = $derived(status?.stage || 'login');
  const terminal = $derived(status && !setupIsActive(status) && step !== 'busy');
  const consent = $derived(setupNeedsConnectorConsent(status));
  const retryConnector = $derived(consent && status?.reason !== 'confirmation_required');
  const canRetryTunnel = $derived(setupCanRetryTunnel(status));
  const canRetryChannel = $derived(step === 'connecting' && !channelReady && !['', 'client_installing', 'runtime_not_ready'].includes(channelCode));
  const showCredentials = $derived(step === 'credentials' || editingCredentials);
  const showTunnelGuide = $derived(step === 'tunnel' && status?.reason === 'workspace_required');
  const keyNeedsPreparation = $derived(step === 'credentials' && ['project_required', 'key_permissions_required', 'runtime_key_required', 'login_required'].includes(status?.reason || ''));
  const credentialInput = $derived(setupCredentialInput(step, tunnelId, savedTunnelId, hasSavedKey));
  const keyRequired = $derived(credentialInput.required);
</script>

{#if pluginActive && status && !dismissed}
  <section class="setup-panel" aria-label={i18n.t('webModel.setup.title')} data-web-model-setup={step} aria-busy={busy || saving}>
    <div class="setup-heading">
      <strong><Icon name={step === 'complete' ? 'check' : 'globe'} size={14} />{i18n.t('webModel.setup.title')}</strong>
      <span>{i18n.t(busy ? 'webModel.setup.working' : `webModel.setup.stage.${step}`)}</span>
    </div>
    <p aria-live="polite">{i18n.t(setupHintKey(status, busy, canRetryChannel))}</p>
    {#if showTunnelGuide}
      <dl class="tunnel-guide" aria-label={i18n.t('webModel.setup.tunnelGuide.title')}>
        {#each ['name', 'description', 'organization', 'workspace'] as field}
          <div><dt>{i18n.t(`webModel.setup.tunnelGuide.${field}`)}</dt><dd>{i18n.t(`webModel.setup.tunnelGuide.${field}Value`)}</dd></div>
        {/each}
      </dl>
    {/if}
    {#if !busy && canRetryChannel && channelDetail}<p class="reason" role="status">{channelDetail}</p>{/if}
    {#if error && !showCredentials}<p class="credentials-error" role="alert">{error}</p>{/if}
    {#if keyNeedsPreparation}<button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => void onaction('continue')}>{i18n.t('webModel.setup.prepareKey')}</button>{/if}
    {#if showCredentials}
      <form class="credentials-card" data-web-model-setup-credentials onsubmit={(event) => { event.preventDefault(); void saveCredentials(); }}>
        <div class="credentials-heading"><Icon name="shield" size={14} /><strong>{i18n.t('webModel.setup.secureTitle')}</strong></div>
        <label>{i18n.t('webModel.setup.tunnelId')}<input class="form-input" name="tunnelId" bind:value={tunnelId} data-web-model-tunnel-credential="1" autocomplete="off" spellcheck="false" placeholder="tunnel_…" required disabled={busy || saving} /></label>
        <label>{i18n.t('webModel.setup.apiKey')}<input class="form-input" name="apiKey" type="password" bind:value={apiKey} data-web-model-tunnel-credential="1" autocomplete="new-password" spellcheck="false" placeholder={i18n.t(credentialInput.placeholderKey)} required={keyRequired} disabled={busy || saving} /></label>
        <span class="secure-hint">{i18n.t('webModel.setup.secretHint')}</span>
        {#if error}<p class="credentials-error" role="alert">{error}</p>{/if}
        <div class="credential-actions">
          <button class="btn btn--sm btn--primary" type="submit" disabled={busy || saving || !tunnelId.trim() || (keyRequired && !apiKey.trim())}>{i18n.t(saving ? 'webModel.setup.saving' : 'webModel.setup.saveContinue')}</button>
          {#if step !== 'credentials'}<button class="btn btn--sm btn--secondary" type="button" disabled={saving} onclick={() => { editingCredentials = false; apiKey = ''; error = ''; tunnelId = savedTunnelId; }}>{i18n.t('webModel.setup.cancelEdit')}</button>{/if}
        </div>
      </form>
    {/if}
    <div class="setup-actions">
      {#if !terminal}
        <button class="btn btn--sm btn--secondary" disabled={saving} onclick={() => dispatchWebModelAction({ kind: 'openView' })}>{i18n.t('webModel.setup.showPage')}</button>
        {#if setupCanEditCredentials(status) && !showCredentials}
          <button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => { editingCredentials = true; error = ''; }}>{i18n.t(step === 'tunnel' ? 'webModel.setup.manualTunnel' : 'webModel.setup.editCredentials')}</button>
        {/if}
        {#if !showCredentials}
          <button class="btn btn--sm btn--primary" disabled={busy || saving || (step === 'connecting' && !channelReady && !canRetryChannel)} onclick={() => void onaction(consent ? 'confirm_connector' : canRetryTunnel ? 'retry_tunnel' : 'continue')}>
            {busy ? i18n.t('webModel.setup.working') : i18n.t(retryConnector ? 'webModel.setup.retryConnector' : consent ? 'webModel.setup.allowConnector' : canRetryTunnel ? 'webModel.setup.retryTunnel' : canRetryChannel ? 'webModel.setup.retryConnection' : 'webModel.setup.continue')}
          </button>
        {/if}
        <button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => { apiKey = ''; void onaction('cancel'); }}>{i18n.t('webModel.setup.cancel')}</button>
      {:else}
        <button class="btn btn--sm btn--secondary" onclick={() => { apiKey = ''; dismissed = true; }}>{i18n.t('webModel.setup.dismiss')}</button>
      {/if}
    </div>
  </section>
{/if}

<style>
  .setup-panel { border: var(--magi-hairline) solid var(--border); border-radius: 10px; padding: 14px 16px; margin: 0 0 8px; min-width: 0; max-height: min(60vh, 520px); overflow-y: auto; font-size: 12px; }
  .setup-heading, .setup-heading strong, .setup-actions, .credentials-heading, .credential-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .setup-heading { justify-content: space-between; gap: 6px 12px; }
  .setup-heading > span { color: var(--foreground-muted); }
  p { margin: 8px 0; line-height: 1.6; overflow-wrap: anywhere; }
  .reason, .secure-hint { color: var(--foreground-muted); }
  .tunnel-guide { margin: 10px 0; display: grid; gap: 8px; }
  .tunnel-guide > div { display: grid; grid-template-columns: minmax(84px, 1fr) minmax(0, 3fr); gap: 12px; }
  .tunnel-guide dt { color: var(--foreground-muted); }
  .tunnel-guide dd { margin: 0; overflow-wrap: anywhere; line-height: 1.5; }
  .credentials-card { display: grid; gap: 12px; border: var(--magi-hairline) solid var(--border); border-radius: 8px; padding: 12px; margin: 12px 0; min-width: 0; }
  label { display: grid; gap: 6px; min-width: 0; }
  input { min-width: 0; width: 100%; box-sizing: border-box; }
  .secure-hint { line-height: 1.5; overflow-wrap: anywhere; }
  .credentials-error { margin: 0; color: var(--error); }
  .setup-actions { margin-top: 12px; }
  .setup-actions .btn, .credential-actions .btn { white-space: normal; }
  @media (max-width: 640px) { .setup-panel { padding: 12px; } }
</style>
