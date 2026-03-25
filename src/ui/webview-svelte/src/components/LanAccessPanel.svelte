<script lang="ts">
  import QRCode from 'qrcode';
  import type { StandardMessage } from '../../../../protocol/message-protocol';
  import { MessageCategory } from '../../../../protocol/message-protocol';
  import Icon from './Icon.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { i18n } from '../stores/i18n.svelte';

  interface TunnelState {
    status: 'stopped' | 'starting' | 'running' | 'stopping' | 'installing' | 'error';
    publicUrl: string | null;
    accessUrl: string | null;
    token: string | null;
    error: string | null;
  }

  interface Props { visible: boolean; onClose: () => void; }
  let { visible, onClose }: Props = $props();

  let lanUrl = $state('');
  let copied = $state(false);
  let loading = $state(false);
  let qrSvg = $state('');
  let qrError = $state('');

  const defaultTS: TunnelState = { status: 'stopped', publicUrl: null, accessUrl: null, token: null, error: null };
  let tunnelState = $state<TunnelState>({ ...defaultTS });
  let tunnelQrSvg = $state('');
  let tunnelCopied = $state(false);
  let tunnelBusy = $state(false);

  async function renderQR(url: string): Promise<string> {
    if (!url) return '';
    try {
      return await QRCode.toString(url, { type: 'svg', margin: 1, color: { dark: '#111827', light: '#FFFFFF' } });
    } catch {
      return '';
    }
  }

  function requestLanInfo() {
    loading = true; lanUrl = ''; qrSvg = ''; qrError = '';
    vscode.postMessage({ type: 'getLanAccessInfo' });
  }

  $effect(() => {
    const unsub = vscode.onMessage((msg) => {
      if (msg.type !== 'unifiedMessage') return;
      const std = msg.message as StandardMessage;
      if (!std || std.category !== MessageCategory.DATA || !std.data) return;
      const { dataType, payload } = std.data;
      if (dataType === 'lanAccessInfo') {
        const d = payload as { url?: string } | undefined;
        if (d?.url) {
          lanUrl = d.url;
          renderQR(d.url).then(s => { qrSvg = s; qrError = s ? '' : 'fail'; });
        } else { qrSvg = ''; }
        loading = false;
      }
      if (dataType === 'tunnelState') {
        const d = payload as TunnelState | undefined;
        if (d) {
          tunnelState = d; tunnelBusy = false;
          if (d.accessUrl) { renderQR(d.accessUrl).then(s => { tunnelQrSvg = s; }); }
          else { tunnelQrSvg = ''; }
        }
      }
    });
    return unsub;
  });

  $effect(() => {
    if (visible && !lanUrl) requestLanInfo();
    if (visible) vscode.postMessage({ type: 'getTunnelStatus' });
  });

  async function copyUrl() {
    if (!lanUrl) return;
    try { await navigator.clipboard.writeText(lanUrl); copied = true; setTimeout(() => { copied = false; }, 2000); }
    catch { /* silent */ }
  }

  function toggleTunnel() {
    tunnelBusy = true;
    if (tunnelState.status === 'running') { vscode.postMessage({ type: 'stopTunnel' }); }
    else { vscode.postMessage({ type: 'startTunnel' }); }
  }

  async function copyTunnelUrl() {
    if (!tunnelState.accessUrl) return;
    try { await navigator.clipboard.writeText(tunnelState.accessUrl); tunnelCopied = true; setTimeout(() => { tunnelCopied = false; }, 2000); }
    catch { /* silent */ }
  }

  function handleClickOutside(e: MouseEvent) {
    const t = e.target as HTMLElement;
    if (!t.closest('.lan-access-panel') && !t.closest('.lan-access-trigger')) onClose();
  }

  $effect(() => {
    if (visible) {
      const timer = setTimeout(() => document.addEventListener('click', handleClickOutside), 0);
      return () => { clearTimeout(timer); document.removeEventListener('click', handleClickOutside); };
    }
  });

  function statusLabel(s: string): string {
    switch (s) {
      case 'stopped': return i18n.t('lanAccess.status.stopped');
      case 'starting': return i18n.t('lanAccess.status.starting');
      case 'running': return i18n.t('lanAccess.status.running');
      case 'stopping': return i18n.t('lanAccess.status.stopping');
      case 'installing': return i18n.t('lanAccess.status.installing');
      case 'error': return i18n.t('lanAccess.status.error');
      default: return s;
    }
  }

  const transitioning = $derived(
    tunnelState.status === 'starting' || tunnelState.status === 'stopping' || tunnelState.status === 'installing'
  );
</script>

{#if visible}
<div class="lan-access-panel">
  <div class="panel-header">
    <span class="panel-title">{i18n.t('lanAccess.title')}</span>
    <button class="panel-close" onclick={onClose}><Icon name="close" size={14} /></button>
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
      <span class="tunnel-status" class:running={tunnelState.status==='running'} class:error={tunnelState.status==='error'}>
        {statusLabel(tunnelState.status)}
      </span>
    </div>
    {#if tunnelState.status === 'running' && tunnelQrSvg}
      <div class="qr-container">{@html tunnelQrSvg}</div>
    {/if}
    {#if tunnelState.error}<p class="tunnel-error">{tunnelState.error}</p>{/if}
    {#if tunnelState.accessUrl}
      <div class="panel-body">
        <div class="url-row">
          <input class="url-input" type="text" value={tunnelState.accessUrl} readonly />
          <button class="copy-btn" onclick={copyTunnelUrl} title={tunnelCopied ? i18n.t('lanAccess.copied') : i18n.t('lanAccess.copy')}>
            {#if tunnelCopied}<Icon name="check" size={14} />{:else}<Icon name="copy" size={14} />{/if}
          </button>
        </div>
      </div>
    {/if}
    <div class="panel-body">
      <button class="tunnel-toggle-btn" onclick={toggleTunnel} disabled={tunnelBusy || transitioning}>
        {#if tunnelBusy || transitioning}<Icon name="loader" size={14} />
        {:else if tunnelState.status === 'running'}{i18n.t('lanAccess.tunnelClose')}
        {:else}{i18n.t('lanAccess.tunnelOpen')}{/if}
      </button>
      <p class="tunnel-hint">{i18n.t('lanAccess.tunnelHint')}</p>
    </div>
  </div>
</div>
{/if}

<style>
  .lan-access-panel { position: absolute; top: calc(100% + 4px); right: 0; width: 300px; max-height: 80vh; overflow-y: auto; background: var(--vscode-editor-background, #1e1e1e); border: 1px solid var(--vscode-panel-border, #454545); border-radius: var(--radius-lg, 8px); box-shadow: 0 12px 28px rgba(0, 0, 0, 0.45); z-index: var(--z-popover, 100); animation: panelSlideIn 0.15s ease-out; }
  @keyframes panelSlideIn { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: translateY(0); } }
  .panel-header { display: flex; align-items: center; justify-content: space-between; padding: 10px 12px; border-bottom: 1px solid var(--border, #333); }
  .panel-title { font-size: var(--text-sm, 13px); font-weight: 600; color: var(--foreground, #ccc); }
  .panel-close { display: flex; align-items: center; justify-content: center; width: 22px; height: 22px; background: transparent; border: none; border-radius: var(--radius-sm, 4px); color: var(--foreground-muted, #888); cursor: pointer; transition: all 0.15s; }
  .panel-close:hover { background: var(--surface-hover, rgba(255,255,255,0.08)); color: var(--foreground, #ccc); }
  .section { padding: 0; }
  .section-label { display: flex; align-items: center; gap: 8px; padding: 8px 12px 4px; font-size: var(--text-xs, 11px); font-weight: 600; color: var(--foreground-muted, #888); text-transform: uppercase; letter-spacing: 0.5px; }
  .tunnel-section { border-top: 1px solid var(--border, #333); }
  .tunnel-status { font-weight: 400; text-transform: none; letter-spacing: 0; font-size: var(--text-xs, 11px); color: var(--foreground-muted, #888); }
  .tunnel-status.running { color: var(--vscode-terminal-ansiGreen, #4ec9b0); }
  .tunnel-status.error { color: var(--vscode-errorForeground, #f44747); }
  .panel-body { padding: 6px 12px 10px; }
  .panel-desc { font-size: var(--text-xs, 11px); color: var(--foreground-muted, #888); margin: 0 0 8px 0; line-height: 1.5; text-align: center; }
  .qr-container { display: flex; align-items: center; justify-content: center; aspect-ratio: 1; margin: 4px 12px; padding: 16px; background: #ffffff; border-radius: var(--radius-lg, 8px); }
  .qr-container :global(svg) { width: 100%; height: 100%; }
  .qr-placeholder { display: flex; align-items: center; justify-content: center; gap: 6px; aspect-ratio: 1; margin: 4px 12px; background: var(--surface-2, rgba(255,255,255,0.04)); border-radius: var(--radius-lg, 8px); }
  .qr-error { display: flex; align-items: center; justify-content: center; aspect-ratio: 1; margin: 4px 12px; padding: 12px; border-radius: var(--radius-lg, 8px); border: 1px dashed var(--vscode-panel-border, #454545); background: var(--vscode-editorWidget-background, #252526); color: var(--foreground-muted, #888); font-size: var(--text-xs, 11px); line-height: 1.6; text-align: center; }
  .loading-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--foreground-muted, #888); animation: dotPulse 1.2s ease-in-out infinite; }
  .loading-dot:nth-child(2) { animation-delay: 0.15s; }
  .loading-dot:nth-child(3) { animation-delay: 0.3s; }
  @keyframes dotPulse { 0%, 80%, 100% { opacity: 0.3; transform: scale(0.8); } 40% { opacity: 1; transform: scale(1); } }
  .url-row { display: flex; gap: 6px; }
  .url-input { flex: 1; min-width: 0; padding: 6px 8px; font-size: var(--text-xs, 11px); font-family: var(--font-mono, monospace); color: var(--foreground, #ccc); background: var(--surface-2, rgba(255,255,255,0.04)); border: 1px solid var(--border, #333); border-radius: var(--radius-sm, 4px); outline: none; }
  .url-input:focus { border-color: var(--accent, var(--vscode-focusBorder, #007fd4)); }
  .copy-btn { display: flex; align-items: center; justify-content: center; width: 32px; height: 32px; flex-shrink: 0; background: var(--accent, var(--vscode-button-background, #0078d4)); border: none; border-radius: var(--radius-sm, 4px); color: var(--accent-fg, var(--vscode-button-foreground, #fff)); cursor: pointer; transition: opacity 0.15s; }
  .copy-btn:hover { opacity: 0.85; }
  .tunnel-toggle-btn { display: flex; align-items: center; justify-content: center; gap: 6px; width: 100%; padding: 6px 12px; font-size: var(--text-xs, 12px); font-weight: 500; color: var(--accent-fg, var(--vscode-button-foreground, #fff)); background: var(--accent, var(--vscode-button-background, #0078d4)); border: none; border-radius: var(--radius-sm, 4px); cursor: pointer; transition: opacity 0.15s; }
  .tunnel-toggle-btn:hover:not(:disabled) { opacity: 0.85; }
  .tunnel-toggle-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .tunnel-hint { font-size: 10px; color: var(--foreground-muted, #666); margin: 6px 0 0 0; text-align: center; line-height: 1.4; }
  .tunnel-error { padding: 4px 12px; margin: 0; font-size: var(--text-xs, 11px); color: var(--vscode-errorForeground, #f44747); line-height: 1.4; }
</style>
