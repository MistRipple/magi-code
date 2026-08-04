<script lang="ts">
  import QRCode from 'qrcode';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { agentUrl } from '../web/agent-api';
  import { getTransport } from '../shared/transport';

  interface Props { visible: boolean; onClose: () => void; }
  let { visible, onClose }: Props = $props();

  let lanUrl = $state('');
  let copied = $state(false);
  let loading = $state(false);
  let lanInfoRequestSeq = 0;
  let qrSvg = $state('');
  let qrError = $state('');
  let wasVisible = false;

  // 隧道状态
  let tunnelStatus = $state('stopped');
  let tunnelAccessUrl = $state('');
  let tunnelError = $state('');
  let tunnelQrSvg = $state('');
  let tunnelCopied = $state(false);
  let tunnelBusy = $state(false);
  let tunnelOperationSeq = 0;

  function currentBindingValue(name: string): string {
    return new URLSearchParams(window.location.search).get(name) || '';
  }

  function currentAccessBinding(): { workspaceId: string; workspacePath: string; sessionId: string } {
    return {
      workspaceId: currentBindingValue('workspaceId'),
      workspacePath: currentBindingValue('workspacePath'),
      sessionId: currentBindingValue('sessionId'),
    };
  }

  function currentAccessBindingQuery(): string {
    const binding = currentAccessBinding();
    const query = new URLSearchParams();
    if (binding.workspaceId) query.set('workspaceId', binding.workspaceId);
    if (binding.workspacePath) query.set('workspacePath', binding.workspacePath);
    if (binding.sessionId) query.set('sessionId', binding.sessionId);
    return query.toString();
  }

  async function renderQR(url: string): Promise<string> {
    if (!url) return '';
    try {
      return await QRCode.toString(url, { type: 'svg', margin: 1, color: { dark: '#111827', light: '#FFFFFF' } });
    } catch { return ''; }
  }

  async function requestLanInfo() {
    const requestSeq = ++lanInfoRequestSeq;
    loading = true; lanUrl = ''; qrSvg = ''; qrError = '';
    try {
      const query = currentAccessBindingQuery();
      const res = await getTransport().request(agentUrl('/api/lan-access', query));
      const data = await res.json();
      const nextLanUrl = typeof data?.url === 'string' ? data.url : '';
      const nextQrSvg = nextLanUrl ? await renderQR(nextLanUrl) : '';
      if (requestSeq !== lanInfoRequestSeq) return;
      lanUrl = nextLanUrl;
      qrSvg = nextQrSvg;
      qrError = nextLanUrl && nextQrSvg ? '' : 'fail';
    } catch {
      if (requestSeq !== lanInfoRequestSeq) return;
      qrError = 'fail';
    } finally {
      if (requestSeq === lanInfoRequestSeq) {
        loading = false;
      }
    }
  }

  function tunnelOperationIsCurrent(operationSeq: number): boolean {
    return visible && operationSeq === tunnelOperationSeq;
  }

  async function applyTunnelState(data: Record<string, unknown>, operationSeq: number): Promise<string | null> {
    const nextStatus = typeof data?.status === 'string' ? data.status : 'stopped';
    const nextAccessUrl = typeof data?.accessUrl === 'string' ? data.accessUrl : '';
    const rawError = typeof data?.error === 'string' ? data.error.trim() : '';
    const nextQrSvg = nextAccessUrl ? await renderQR(nextAccessUrl) : '';
    if (!tunnelOperationIsCurrent(operationSeq)) return null;
    if (rawError) {
      console.warn('[LanAccessPanel] tunnel state error:', rawError);
    }
    tunnelStatus = nextStatus;
    tunnelAccessUrl = nextAccessUrl;
    tunnelError = rawError ? i18n.t('lanAccess.tunnelUnavailable') : '';
    tunnelQrSvg = nextQrSvg;
    return nextStatus;
  }

  async function fetchTunnelStatus(operationSeq = ++tunnelOperationSeq): Promise<string | null> {
    try {
      const res = await getTransport().request(agentUrl('/api/tunnel/status'));
      if (!res.ok) {
        if (!tunnelOperationIsCurrent(operationSeq)) return null;
        console.warn('[LanAccessPanel] tunnel status request failed:', res.status);
        tunnelStatus = 'error';
        tunnelError = i18n.t('lanAccess.tunnelStatusFailed');
        tunnelAccessUrl = '';
        tunnelQrSvg = '';
        return tunnelStatus;
      }
      const data = await res.json();
      return await applyTunnelState(data, operationSeq);
    } catch (error) {
      if (!tunnelOperationIsCurrent(operationSeq)) return null;
      console.warn('[LanAccessPanel] tunnel status request failed:', error);
      tunnelStatus = 'error';
      tunnelError = i18n.t('lanAccess.tunnelStatusFailed');
      tunnelAccessUrl = '';
      tunnelQrSvg = '';
      return tunnelStatus;
    }
  }

  async function toggleTunnel() {
    const operationSeq = ++tunnelOperationSeq;
    tunnelBusy = true;
    const action = tunnelStatus === 'running' ? 'stop' : 'start';
    try {
      const binding = currentAccessBinding();
      const init: RequestInit = action === 'start'
        ? {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(binding),
          }
        : { method: 'POST' };
      const res = await getTransport().request(agentUrl(`/api/tunnel/${action}`), init);
      if (!res.ok) {
        if (!tunnelOperationIsCurrent(operationSeq)) return;
        console.warn('[LanAccessPanel] tunnel action failed:', action, res.status);
        tunnelStatus = 'error';
        tunnelError = i18n.t(action === 'start' ? 'lanAccess.tunnelStartFailed' : 'lanAccess.tunnelStopFailed');
        tunnelAccessUrl = '';
        tunnelQrSvg = '';
        return;
      }
      const data = await res.json();
      if (await applyTunnelState(data, operationSeq) === null) return;
      // 隧道启动是异步的，轮询直到状态稳定
      await pollTunnelUntilStable(operationSeq);
    } catch (error) {
      if (!tunnelOperationIsCurrent(operationSeq)) return;
      console.warn('[LanAccessPanel] tunnel action failed:', action, error);
      tunnelStatus = 'error';
      tunnelError = i18n.t(action === 'start' ? 'lanAccess.tunnelStartFailed' : 'lanAccess.tunnelStopFailed');
      tunnelAccessUrl = '';
      tunnelQrSvg = '';
    }
    finally {
      if (tunnelOperationIsCurrent(operationSeq)) {
        tunnelBusy = false;
      }
    }
  }

  async function pollTunnelUntilStable(operationSeq: number) {
    const maxAttempts = 30; // 最多轮询 30 次，每次 1 秒
    for (let i = 0; i < maxAttempts; i++) {
      const status = await fetchTunnelStatus(operationSeq);
      if (status === null || status === 'running' || status === 'stopped' || status === 'error') {
        return;
      }
      await new Promise(r => setTimeout(r, 1000));
      if (!tunnelOperationIsCurrent(operationSeq)) return;
    }
  }

  async function copyTunnelUrl() {
    if (!tunnelAccessUrl) return;
    try { await navigator.clipboard.writeText(tunnelAccessUrl); tunnelCopied = true; setTimeout(() => { tunnelCopied = false; }, 2000); }
    catch { /* silent */ }
  }

  $effect(() => {
    if (visible === wasVisible) return;
    wasVisible = visible;
    if (!visible) {
      lanInfoRequestSeq += 1;
      tunnelOperationSeq += 1;
      loading = false;
      tunnelBusy = false;
      return;
    }
    void requestLanInfo();
    void fetchTunnelStatus();
  });

  async function copyUrl() {
    if (!lanUrl) return;
    try { await navigator.clipboard.writeText(lanUrl); copied = true; setTimeout(() => { copied = false; }, 2000); }
    catch { /* silent */ }
  }

</script>

{#if visible}
<div class="lan-access-panel" data-magi-surface="popover">
  <div class="panel-header">
    <span class="panel-title">{i18n.t('lanAccess.title')}</span>
    <button
      class="panel-close"
      onclick={onClose}
      title={i18n.t('common.close')}
      aria-label={i18n.t('common.close')}
    >
      <Icon name="close" size={14} />
    </button>
  </div>
  <div class="section">
    <div class="section-label">{i18n.t('lanAccess.sectionLan')}</div>
    {#if loading}
      <div class="qr-placeholder">
        <span class="loading-dot"></span><span class="loading-dot"></span><span class="loading-dot"></span>
      </div>
    {:else if qrSvg}
      <div class="qr-container">{@html qrSvg}</div>
    {:else if qrError}
      <div class="qr-error">{i18n.t('lanAccess.qrError')}</div>
    {/if}
    <div class="panel-body">
      <p class="panel-desc">{i18n.t('lanAccess.description')}</p>
      {#if lanUrl}
        <div class="url-row">
          <input class="url-input" type="text" value={lanUrl} readonly />
          <button class="copy-btn" onclick={copyUrl} title={copied ? i18n.t('lanAccess.copied') : i18n.t('lanAccess.copy')}>
            {#if copied}<Icon name="check" size={14} />{:else}<Icon name="copy" size={14} />{/if}
          </button>
        </div>
      {/if}
    </div>
  </div>
  <div class="section tunnel-section">
    <div class="section-label">
      {i18n.t('lanAccess.sectionTunnel')}
      <span class="tunnel-status" class:running={tunnelStatus === 'running'} class:error={tunnelStatus === 'error'}>
        {i18n.t(`lanAccess.status.${tunnelStatus}`)}
      </span>
    </div>
    {#if tunnelStatus === 'running' && tunnelQrSvg}
      <div class="qr-container">{@html tunnelQrSvg}</div>
    {/if}
    {#if tunnelError}<p class="tunnel-error">{tunnelError}</p>{/if}
    {#if tunnelAccessUrl}
      <div class="panel-body">
        <div class="url-row">
          <input class="url-input" type="text" value={tunnelAccessUrl} readonly />
          <button class="copy-btn" onclick={copyTunnelUrl} title={tunnelCopied ? i18n.t('lanAccess.copied') : i18n.t('lanAccess.copy')}>
            {#if tunnelCopied}<Icon name="check" size={14} />{:else}<Icon name="copy" size={14} />{/if}
          </button>
        </div>
      </div>
    {/if}
    <div class="panel-body">
      <button class="tunnel-toggle-btn" onclick={toggleTunnel} disabled={tunnelBusy || tunnelStatus === 'starting' || tunnelStatus === 'stopping'}>
        {#if tunnelBusy || tunnelStatus === 'starting' || tunnelStatus === 'stopping'}
          <Icon name="loader" size={14} />
        {:else if tunnelStatus === 'running'}
          {i18n.t('lanAccess.tunnelClose')}
        {:else}
          {i18n.t('lanAccess.tunnelOpen')}
        {/if}
      </button>
      <p class="tunnel-hint">{i18n.t('lanAccess.tunnelHint')}</p>
    </div>
  </div>
</div>
{/if}


<style>
  .lan-access-panel { position: absolute; top: calc(100% + 8px); right: 0; width: 300px; max-height: 80vh; overflow-y: auto; background: var(--glass-bg); backdrop-filter: blur(20px); -webkit-backdrop-filter: blur(20px); border: 1px solid var(--border); border-radius: var(--radius-xl); box-shadow: var(--shadow-xl); z-index: var(--z-popover); animation: panelSlideIn 0.15s ease-out; }
  @keyframes panelSlideIn { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: translateY(0); } }
  .panel-header { display: flex; align-items: center; justify-content: space-between; padding: 10px 14px; border-bottom: 1px solid var(--border); }
  .panel-title { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); }
  .panel-close { display: flex; align-items: center; justify-content: center; width: 24px; height: 24px; background: transparent; border: none; border-radius: var(--radius-md); color: var(--foreground-muted); cursor: pointer; transition: all 0.15s; }
  .panel-close:hover { background: var(--surface-hover); color: var(--foreground); }
  .section { padding: 0; }
  .section-label { display: flex; align-items: center; gap: 8px; padding: 10px 14px 6px; font-size: var(--text-xs); font-weight: var(--font-semibold); color: var(--foreground-muted); text-transform: uppercase; letter-spacing: 0.5px; }
  .panel-body { padding: 6px 14px 12px; }
  .panel-desc { font-size: var(--text-xs); color: var(--foreground-muted); margin: 0 0 8px 0; line-height: 1.5; text-align: center; }
  .qr-container { display: flex; align-items: center; justify-content: center; aspect-ratio: 1; margin: 4px 14px; padding: 16px; background: #ffffff; border-radius: var(--radius-lg); }
  .qr-container :global(svg) { width: 100%; height: 100%; }
  .qr-placeholder { display: flex; align-items: center; justify-content: center; gap: 6px; aspect-ratio: 1; margin: 4px 14px; background: var(--surface-2); border-radius: var(--radius-lg); }
  .qr-error { display: flex; align-items: center; justify-content: center; aspect-ratio: 1; margin: 4px 14px; padding: 12px; border-radius: var(--radius-lg); border: 1px dashed var(--border); background: var(--surface-2); color: var(--foreground-muted); font-size: var(--text-xs); line-height: 1.6; text-align: center; }
  .loading-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--foreground-muted); animation: dotPulse 1.2s ease-in-out infinite; }
  .loading-dot:nth-child(2) { animation-delay: 0.15s; }
  .loading-dot:nth-child(3) { animation-delay: 0.3s; }
  @keyframes dotPulse { 0%, 80%, 100% { opacity: 0.3; transform: scale(0.8); } 40% { opacity: 1; transform: scale(1); } }
  .url-row { display: flex; gap: 6px; }
  .url-input { flex: 1; min-width: 0; padding: 6px 8px; font-size: var(--text-xs); font-family: var(--font-mono); color: var(--foreground); background: var(--vscode-input-background, var(--surface-2)); border: 1px solid var(--border); border-radius: var(--radius-sm); outline: none; }
  .url-input:focus { border-color: var(--primary); }
  .copy-btn { display: flex; align-items: center; justify-content: center; width: 32px; height: 32px; flex-shrink: 0; background: var(--primary); border: none; border-radius: var(--radius-md); color: var(--primary-foreground); cursor: pointer; transition: opacity 0.15s; }
  .copy-btn:hover { opacity: 0.85; }
  .tunnel-section { border-top: 1px solid var(--border); }
  .tunnel-status { font-weight: 400; text-transform: none; letter-spacing: 0; font-size: var(--text-xs); color: var(--foreground-muted); }
  .tunnel-status.running { color: var(--success); }
  .tunnel-status.error { color: var(--error); }
  .tunnel-error { padding: 4px 14px; margin: 0; font-size: var(--text-xs); color: var(--error); line-height: 1.4; }
  .tunnel-toggle-btn { display: flex; align-items: center; justify-content: center; gap: 6px; width: 100%; padding: 6px 12px; font-size: var(--text-xs); font-weight: var(--font-medium); color: var(--primary-foreground); background: var(--primary); border: none; border-radius: var(--radius-md); cursor: pointer; transition: opacity 0.15s; }
  .tunnel-toggle-btn:hover:not(:disabled) { opacity: 0.85; }
  .tunnel-toggle-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .tunnel-hint { font-size: 10px; color: var(--foreground-muted); margin: 6px 0 0 0; text-align: center; line-height: 1.4; }
</style>
