<script lang="ts">
  import type { WebModelSetupStatus } from '../shared/app-server-protocol.generated';
  import { getWebModelSetup, saveWebModelSetupCredentials, setupIsActive, setupNeedsConnectorConsent, setupReasonKey, type SetupAction } from '../web/web-model-setup';
  import { dispatchWebModelAction } from '../web/web-model-actions';
  import { i18n } from '../stores/i18n.svelte';
  import { getWebModelTunnel } from '../web/agent-api';
  import Icon from './Icon.svelte';

  let { sessionId, busy, status = $bindable(null), onaction }: {
    sessionId: string;
    busy: boolean;
    status?: WebModelSetupStatus | null;
    onaction: (action: SetupAction) => Promise<void>;
  } = $props();
  let apiKey = $state('');
  let tunnelId = $state('');
  let saving = $state(false);
  let error = $state('');
  let dismissed = $state(false);
  let channelReady = $state(false);
  let channelCode = $state('');
  let channelDetail = $state('');

  $effect(() => {
    const current = sessionId;
    status = null;
    apiKey = '';
    tunnelId = '';
    dismissed = false;
    error = '';
    channelReady = false;
    channelCode = '';
    channelDetail = '';
    if (!current || window.magiDesktop?.runtime !== 'electron') return;
    let disposed = false;
    let inFlight = false;
    let advancedAt: number | null = null;
    const refresh = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await getWebModelSetup(current);
        if (disposed) return;
        status = next;
        if (next?.stage === 'connecting') {
          const tunnel = await getWebModelTunnel();
          if (disposed) return;
          channelReady = tunnel.ready;
          channelCode = tunnel.code;
          channelDetail = tunnel.detail;
          // 等待组件是 Magi 的工作。就绪后自动推进到连接器确认，用户只处理真正的阻断。
          if (tunnel.ready && !busy && advancedAt !== next.updatedAt) {
            advancedAt = next.updatedAt;
            await onaction('continue');
          }
        }
      } catch {
        // 未加载时不凭旧值推断进度。下一次成功响应会恢复同一配置。
      } finally { inFlight = false; }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 1800);
    return () => { disposed = true; window.clearInterval(timer); apiKey = ''; };
  });

  async function saveCredentials(): Promise<void> {
    if (saving || busy) return;
    const current = sessionId;
    saving = true;
    error = '';
    const secret = apiKey;
    apiKey = '';
    try {
      await saveWebModelSetupCredentials(current, tunnelId, secret);
      if (current !== sessionId) return;
      const next = await getWebModelSetup(current);
      if (current !== sessionId) return;
      status = next;
      await onaction('continue');
    } catch (failure) {
      if (current === sessionId) error = failure instanceof Error ? failure.message : i18n.t('webModel.setup.failed');
    } finally { saving = false; }
  }
  const step = $derived(status?.stage || 'login');
  const terminal = $derived(status && !setupIsActive(status) && step !== 'busy');
  const consent = $derived(setupNeedsConnectorConsent(status));
  const reasonKey = $derived(setupReasonKey(status?.reason || ''));
  const canRetryChannel = $derived(step === 'connecting' && !channelReady && !['', 'client_installing', 'runtime_not_ready'].includes(channelCode));
</script>

{#snippet credentialsForm(manual: boolean)}
  <form onsubmit={(event) => { event.preventDefault(); void saveCredentials(); }}>
    {#if manual}
      <label>{i18n.t('webModel.setup.tunnelId')}<input class="form-input" bind:value={tunnelId} autocomplete="off" spellcheck="false" placeholder="tunnel_…" required /></label>
    {/if}
    <label>{i18n.t('webModel.setup.apiKey')}<input class="form-input" type="password" bind:value={apiKey} autocomplete="new-password" spellcheck="false" placeholder={i18n.t('webModel.setup.keyPlaceholder')} /></label>
    <span class="secure-hint">{i18n.t('webModel.setup.secretHint')}</span>
    <button class="btn btn--sm btn--primary" type="submit" disabled={busy || saving || (manual && !tunnelId.trim()) || (!manual && !apiKey.trim())}>{i18n.t('webModel.setup.saveContinue')}</button>
  </form>
{/snippet}

{#if status && !dismissed}
  <section class="setup-panel" aria-label={i18n.t('webModel.setup.title')} data-web-model-setup={step}>
    <div class="setup-heading">
      <strong><Icon name={step === 'complete' ? 'check' : 'globe'} size={14} />{i18n.t('webModel.setup.title')}</strong>
      <span>{i18n.t(`webModel.setup.stage.${step}`)}</span>
    </div>
    <p aria-live="polite">{i18n.t(`webModel.setup.hint.${step}`)}</p>
    {#if reasonKey}
      <p class="reason">{i18n.t(reasonKey)}</p>
    {/if}
    {#if canRetryChannel && channelDetail}<p class="reason" role="status">{channelDetail}</p>{/if}
    {#if step === 'credentials'}
      {@render credentialsForm(false)}
    {:else if step === 'tunnel'}
      <details><summary>{i18n.t('webModel.setup.manualTunnel')}</summary>{@render credentialsForm(true)}</details>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
    <div class="setup-actions">
      {#if !terminal}
        <button class="btn btn--sm btn--secondary" disabled={saving} onclick={() => dispatchWebModelAction({ kind: 'openView' })}>{i18n.t('webModel.setup.showPage')}</button>
        {#if canRetryChannel}
          <button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => dispatchWebModelAction({ kind: 'openTunnelSettings' })}>{i18n.t('webModel.action.configureTunnel')}</button>
        {/if}
        {#if step !== 'credentials'}
          <button class="btn btn--sm btn--primary" disabled={busy || saving || (step === 'connecting' && !channelReady && !canRetryChannel)} onclick={() => void onaction(consent ? 'confirm_connector' : 'continue')}>
            {busy ? i18n.t('webModel.setup.working') : i18n.t(consent ? 'webModel.setup.allowConnector' : canRetryChannel ? 'webModel.setup.retryConnection' : 'webModel.setup.continue')}
          </button>
        {/if}
        <button class="btn btn--sm btn--secondary" disabled={busy || saving} onclick={() => void onaction('cancel')}>{i18n.t('webModel.setup.cancel')}</button>
      {:else}
        <button class="btn btn--sm btn--secondary" onclick={() => { dismissed = true; }}>{i18n.t('webModel.setup.dismiss')}</button>
      {/if}
    </div>
  </section>
{/if}

<style>
  .setup-panel { border: 1px solid var(--border); border-radius: 10px; padding: 12px; margin: 0 0 8px; font-size: 12px; }
  .setup-heading, .setup-heading strong, .setup-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .setup-heading { justify-content: space-between; }
  p { margin: 8px 0; line-height: 1.6; overflow-wrap: anywhere; }
  .reason, .secure-hint { color: var(--foreground-muted); }
  form { display: grid; gap: 8px; margin-bottom: 10px; }
  label { display: grid; gap: 4px; }
  input { min-width: 0; width: 100%; box-sizing: border-box; }
  form button { justify-self: start; }
  details { margin: 8px 0; }
  summary { cursor: pointer; margin-bottom: 8px; }
</style>
