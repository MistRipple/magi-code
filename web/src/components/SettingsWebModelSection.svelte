<script lang="ts">
  /**
   * 「设置 → GPT Web」独立分类。
   *
   * 只有四件事：登录与可用性、停止（释放槽位）、工具通道（OpenAI Tunnel）、清除数据。
   * 分区只展示 daemon 的事实，不自行推断可用性；GPT Web 的模型、窗口与强度都在网页里，
   * 这里没有任何模型清单或限额配置。
   */
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import { WEB_MODEL_SETUP_REQUEST } from '../web/web-model-setup';
  import { i18n } from '../stores/i18n.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    configureWebConnector,
    configureWebModelTunnel,
    createAppBrowserSession,
    getWebConnectorStatus,
    getWebModelTunnel,
    navigateWebModelPage,
    revealWebModelTunnelApiKey,
    setWebModelToolPolicy,
    type WebModelApprovalMode,
    resetWebModels,
    stopWebModel,
    type WebModelProbeResponse,
    type WebModelToolProfile,
    type WebModelTunnelStatus,
  } from '../web/agent-api';
  import {
    consumeWebModelSettingsRequest,
    WEB_MODEL_SETTINGS_READY_EVENT,
    type WebModelSettingsRequest,
  } from '../web/web-model-actions';
  import {
    projectWebModelAppSession,
    refreshWebModelRuntime,
    runWebModelProbe,
  } from '../web/web-model-session-projection';
  import { activateWebModelTab, appWebModelTab } from '../stores/right-pane.svelte';
  import { closeSettings } from '../stores/shell-ui.svelte';
  import { goToSlotOwnerSession } from '../web/web-model-session-projection';
  import {
    applyWebModelRuntime,
    markWebModelStoppedByUser,
    webModelRuntimeState as readWebModelRuntimeState,
    webModelSlotOwner,
  } from '../stores/web-model-runtime.svelte';

  interface Props {
    isDesktop: boolean;
  }

  let { isDesktop }: Props = $props();

  let probe = $state<WebModelProbeResponse | null>(null);
  let probing = $state(false);
  let stopping = $state(false);
  let clearing = $state(false);
  let errorMessage = $state('');
  let notice = $state('');
  let tunnel = $state<WebModelTunnelStatus | null>(null);
  let tunnelBusy = $state(false);
  let tunnelId = $state('');
  let apiKey = $state('');
  /** 已保存密钥：默认只显示遮罩预览；点「显示」才取回完整密钥，点「更换」才进入输入。 */
  let keyEditing = $state(false);
  let keyRevealed = $state(false);
  let revealedKey = $state('');
  let keyInputVisible = $state(false);
  let toolProfile = $state<WebModelToolProfile>('edit');
  let approvalMode = $state<WebModelApprovalMode>('ask');
  const runtimeProjection = readWebModelRuntimeState();
  const slotOwner = $derived(webModelSlotOwner());
  let desktopDataLoaded = false;

  async function loadTunnel(refreshInputs = true): Promise<void> {
    if (!isDesktop) return;
    try {
      tunnel = await getWebModelTunnel();
      if (!refreshInputs) return;
      tunnelId = tunnel?.tunnelId || '';
      if (tunnel?.toolProfile === 'read_only' || tunnel?.toolProfile === 'edit' || tunnel?.toolProfile === 'edit_trusted') {
        toolProfile = tunnel.toolProfile;
      }
      if (tunnel?.approvalMode === 'ask' || tunnel?.approvalMode === 'always' || tunnel?.approvalMode === 'deny') {
        approvalMode = tunnel.approvalMode;
      }
    } catch (error) {
      console.warn('[SettingsWebModel] 读取工具通道状态失败:', error);
      tunnel = null;
    }
  }

  async function toggleKeyReveal(): Promise<void> {
    if (keyRevealed) {
      keyRevealed = false;
      revealedKey = '';
      return;
    }
    try {
      revealedKey = await revealWebModelTunnelApiKey();
      keyRevealed = true;
    } catch (error) {
      errorMessage = errorText(error);
    }
  }

  function startEditingKey(): void {
    keyEditing = true;
    keyRevealed = false;
    revealedKey = '';
    apiKey = '';
  }

  function cancelEditingKey(): void {
    keyEditing = false;
    keyInputVisible = false;
    apiKey = '';
  }

  /** 权限档 / 授权方式改动立即生效（下一次工具调用起），不需要再点保存，也不重启通道。 */
  async function applyToolPolicy(change: { toolProfile?: WebModelToolProfile; approvalMode?: WebModelApprovalMode }): Promise<void> {
    errorMessage = '';
    try {
      tunnel = await setWebModelToolPolicy(change);
    } catch (error) {
      errorMessage = errorText(error);
      await loadTunnel();
    }
  }

  // 创建 Tunnel 与运行时 API 密钥都在 OpenAI 平台完成：直接在 GPT Web 标签里切过去（登录态共用），
  // 之后可用标签顶部的快捷地址在对话页 / Tunnels / API keys 之间切换。
  async function openPlatformPage(target: 'tunnels' | 'api_keys'): Promise<void> {
    errorMessage = '';
    try {
      await navigateWebModelPage(target);
      activateWebModelTab();
      closeSettings();
    } catch (error) {
      errorMessage = errorText(error);
    }
  }

  // 通道组件在后台下载 / 启动：状态还在「准备中」时轮询，完成后自动停下。
  const PREPARING_CODES = new Set(['client_installing', 'runtime_not_ready']);
  $effect(() => {
    if (!isDesktop || !tunnel || !PREPARING_CODES.has(tunnel.code)) return;
    const timer = setInterval(() => void loadTunnel(false), 1500);
    return () => clearInterval(timer);
  });

  onMount(() => {
    const handleSettingsReady = (event: Event) => {
      const request = (event as CustomEvent<WebModelSettingsRequest>).detail;
      if (!request) return;
      window.setTimeout(() => {
        const target = request.focus === 'tunnel'
          ? document.querySelector<HTMLElement>('[data-web-model-tunnel]')
          : document.querySelector<HTMLElement>('[data-web-model-status]');
        target?.scrollIntoView({ block: 'center', behavior: 'smooth' });
      }, 0);
    };
    window.addEventListener(WEB_MODEL_SETTINGS_READY_EVENT, handleSettingsReady);
    const pending = consumeWebModelSettingsRequest();
    if (pending) window.dispatchEvent(new CustomEvent(WEB_MODEL_SETTINGS_READY_EVENT, { detail: pending }));
    const runtimeTimer = window.setInterval(() => {
      if (isDesktop) void refreshWebModelRuntime();
    }, 2000);
    return () => {
      window.clearInterval(runtimeTimer);
      window.removeEventListener(WEB_MODEL_SETTINGS_READY_EVENT, handleSettingsReady);
    };
  });

  $effect(() => {
    if (!isDesktop || desktopDataLoaded) return;
    desktopDataLoaded = true;
    void loadTunnel();
    void refreshWebModelRuntime();
    // 打开设置就显示当前状态：只有应用级会话已经存在（用户用过 GPT Web）才自动探测；
    // 从没用过的用户不会因为看一眼设置就在后台多出一个 ChatGPT 页面。
    if (appWebModelTab()) void autoCheck();
  });

  async function autoCheck(): Promise<void> {
    try {
      probe = await runWebModelProbe();
    } catch {
      probe = null;
    }
  }

  /** 当前错误是不是「GPT Web 被某个会话占用」：这类错误给出可点的处理动作，而不是原始错误码。 */
  let errorIsSlotBusy = $state(false);

  function errorText(error: unknown): string {
    const raw = error instanceof Error ? error.message.trim() : '';
    errorIsSlotBusy = raw.includes('web_session_busy');
    if (errorIsSlotBusy) {
      const title = slotOwner?.sessionTitle?.trim();
      return i18n.t('webModel.error.sessionBusy', { session: title ? `「${title}」` : '' });
    }
    if (raw) return raw;
    return i18n.t('settings.browser.webModel.loadFailed');
  }

  const TUNNEL_CODES = new Set([
    'running', 'stopped', 'client_installing', 'client_install_failed', 'tunnel_not_configured',
    'tunnel_id_invalid', 'credential_missing', 'client_missing', 'client_checksum_mismatch',
    'client_checksum_missing', 'process_exited', 'runtime_not_ready', 'start_failed', 'mcp_unavailable',
  ]);
  /** 通道状态用人话显示；已知状态码给本地化名称，未知的原样带上服务端说明。 */
  function tunnelStatusText(status: WebModelTunnelStatus): string {
    if (TUNNEL_CODES.has(status.code)) {
      const label = i18n.t(`webModel.tunnel.code.${status.code}`);
      return status.ready || !status.detail ? label : `${label} · ${status.detail}`;
    }
    return status.detail ? `${status.code} · ${status.detail}` : status.code;
  }

  /** 应用级会话要先存在，宿主内容槽才会挂载主页 guest，探测才可能读到页面。 */
  async function ensureAppSession(): Promise<void> {
    await createAppBrowserSession();
    // 设置页也可能是用户的第一个入口：必须在探测前把同一份 Authority 会话投影到
    // appTabs，否则 daemon 的 ensure_surface 只能等待一个永远不存在的 webview。
    await projectWebModelAppSession();
    await tick();
  }

  async function checkAvailability(): Promise<void> {
    if (!isDesktop || probing) return;
    probing = true;
    errorMessage = '';
    notice = '';
    try {
      await ensureAppSession();
      probe = await runWebModelProbe();
      await loadTunnel();
      await refreshWebModelRuntime();
    } catch (error) {
      console.warn('[SettingsWebModel] 探测失败:', error);
      errorMessage = errorText(error);
      probe = null;
    } finally {
      probing = false;
    }
  }

  /** 打开 ChatGPT 页面（登录页 / 主页）：投影应用级会话、激活 Tab，再返回工作台。 */
  async function openWebModelHome(): Promise<void> {
    if (!isDesktop || probing) return;
    probing = true;
    errorMessage = '';
    try {
      await ensureAppSession();
      activateWebModelTab();
      closeSettings();
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      probing = false;
    }
  }

  /** 「停止」：取消在飞推理、销毁页面、释放槽位；登录态保留。 */
  async function stopWeb(): Promise<void> {
    if (stopping) return;
    stopping = true;
    errorMessage = '';
    try {
      await refreshWebModelRuntime();
      if (slotOwner?.active && !(await confirmMessage(i18n.t('webModel.stop.confirmBody'), { tone: 'danger' }))) return;
      await stopWebModel();
      markWebModelStoppedByUser(true);
      await refreshWebModelRuntime();
      notice = i18n.t('webModel.stop.done');
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      stopping = false;
    }
  }

  async function saveTunnel(): Promise<void> {
    if (tunnelBusy) return;
    tunnelBusy = true;
    errorMessage = '';
    try {
      tunnel = await configureWebModelTunnel({ tunnelId, apiKey, toolProfile, approvalMode });
      apiKey = '';
      keyEditing = false;
      keyInputVisible = false;
      keyRevealed = false;
      revealedKey = '';
      if (tunnel.ready) {
        addToast('success', i18n.t('webModel.tunnel.status', { status: tunnelStatusText(tunnel) }));
      }
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      tunnelBusy = false;
    }
  }

  let connectorBusy = $state(false);
  let connectorMessage = $state('');

  function connectorText(status: { supported: boolean; exists: boolean; enabled: boolean; toolCount?: number | null; toolsStale?: boolean | null; reason?: string | null }): string {
    if (!status.supported) return i18n.t('webModel.connector.unsupported');
    if (!status.exists) return i18n.t('webModel.connector.missing');
    if (!status.enabled) return i18n.t('webModel.connector.disabled');
    // This count comes from Magi's live MCP tools/list catalog, not ChatGPT's
    // installed-app row, which does not reliably expose the catalog size.
    const base = status.toolCount == null
      ? i18n.t('webModel.connector.enabledCountUnavailable')
      : i18n.t('webModel.connector.enabled', { count: status.toolCount });
    // ChatGPT 只在创建 / 刷新连接器时读取工具列表，Magi 目录变化后它手里的是旧的。
    return status.toolsStale ? `${base}；${i18n.t('webModel.connector.stale')}` : base;
  }

  async function checkConnector(): Promise<void> {
    if (connectorBusy) return;
    connectorBusy = true;
    errorMessage = '';
    try {
      await ensureAppSession();
      connectorMessage = connectorText(await getWebConnectorStatus());
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      connectorBusy = false;
    }
  }

  /** 让 ChatGPT 重新读取 Magi 当前的工具列表（只刷新缓存，不改连接器的其他设置）。 */
  async function refreshConnectorTools(): Promise<void> {
    if (connectorBusy) return;
    connectorBusy = true;
    errorMessage = '';
    try {
      await ensureAppSession();
      const result = await configureWebConnector();
      connectorMessage = result.confirmedEnabled && result.reason === 'tools_refreshed'
        ? i18n.t('webModel.connector.refreshed')
        : i18n.t('webModel.connector.refreshFailed', { reason: result.reason || '-' });
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      connectorBusy = false;
    }
  }

  /** 会改动用户 ChatGPT 账号里的连接器设置：必须经用户确认。 */
  async function configureConnector(): Promise<void> {
    if (connectorBusy) return;
    if (!(await confirmMessage(i18n.t('webModel.connector.confirmBody')))) return;
    connectorBusy = true;
    errorMessage = '';
    try {
      await ensureAppSession();
      const result = await configureWebConnector();
      connectorMessage = result.confirmedEnabled
        ? i18n.t(result.reason === 'tools_refreshed' ? 'webModel.connector.refreshed' : 'webModel.connector.configured')
        : i18n.t('webModel.connector.configFailed', { reason: result.reason || '-' });
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      connectorBusy = false;
    }
  }

  async function clearTunnel(): Promise<void> {
    if (tunnelBusy) return;
    tunnelBusy = true;
    errorMessage = '';
    try {
      tunnel = await configureWebModelTunnel({ clear: true });
      tunnelId = '';
      apiKey = '';
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      tunnelBusy = false;
    }
  }

  /**
   * 「清除数据」：先让 daemon 撤销在飞回复、释放槽位并隐藏 GPT Web，再按应用级
   * 分区粒度清登录态。两者顺序不能反过来。
   */
  async function clearWebData(): Promise<void> {
    if (clearing) return;
    const desktop = window.magiDesktop;
    if (!isDesktop || !desktop?.clearWebModelData) {
      addToast('warning', i18n.t('settings.browser.webModel.needDesktop'));
      return;
    }
    if (!(await confirmMessage(i18n.t('webModel.clearData.confirmBody'), { tone: 'danger' }))) return;
    clearing = true;
    errorMessage = '';
    try {
      await resetWebModels();
      await desktop.clearWebModelData();
      applyWebModelRuntime(null);
      probe = null;
      notice = i18n.t('webModel.clearData.done');
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      clearing = false;
    }
  }

  function statusLabel(status: string): string {
    switch (status) {
      case 'login_required':
        return i18n.t('webModel.status.loginRequired');
      case 'desktop_unavailable':
        return i18n.t('webModel.status.desktopUnavailable');
      case 'site_blocked':
        return i18n.t('webModel.status.siteBlocked');
      case 'selectors_drift':
        return i18n.t('webModel.status.selectorsDrift');
      case 'failed':
        return i18n.t('webModel.status.failed');
      default:
        return i18n.t('webModel.status.available');
    }
  }

  // 只展示识别到的套餐；未识别（unknown）不输出原始字符串，也不占一行。
  const accountPlanLabel = $derived.by(() => {
    const plan = probe?.accountHint;
    return plan === 'free' || plan === 'plus' || plan === 'pro'
      ? i18n.t(`webModel.accountPlan.${plan}`)
      : '';
  });
  const needsOpenHome = $derived(
    probe?.status === 'login_required'
      || probe?.status === 'site_blocked'
      || probe?.status === 'selectors_drift',
  );
</script>

<div class="apple-manager">
<div class="apple-scroller-proxy">
<div class="settings-section web-model-section">
  <div class="settings-card" data-web-model-setup-entry>
    <div class="settings-row">
      <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('webModel.setup.title')}</span><span class="settings-row-desc">{i18n.t('webModel.setup.entryHint')}</span></div>
      <button class="btn btn--sm btn--primary" disabled={!isDesktop} onclick={() => window.dispatchEvent(new CustomEvent(WEB_MODEL_SETUP_REQUEST))}>{i18n.t('webModel.setup.start')}</button>
    </div>
  </div>

  <div class="settings-section-header">
    <div class="settings-section-title" id="web-model-title">{i18n.t('settings.browser.webModel.title')}</div>
    <div class="settings-section-actions">
      <button
        type="button"
        class="btn btn--sm btn--secondary"
        data-web-model-connect="1"
        disabled={!isDesktop || probing}
        onclick={() => void checkAvailability()}
      >
        {probing
          ? i18n.t('settings.browser.webModel.connecting')
          : i18n.t('settings.browser.webModel.connect')}
      </button>
    </div>
  </div>
  <div class="settings-section-desc">{i18n.t('settings.browser.webModel.desc')}</div>

  {#if !isDesktop}
    <div class="settings-notice settings-notice--warn">{i18n.t('settings.browser.webModel.needDesktop')}</div>
  {/if}

  <div class="settings-rows">
    <div class="settings-row" data-web-model-status="1">
      <div class="settings-row-copy">
        <span class="settings-row-title wm-status-title">
          <span class="settings-dot" class:settings-dot--ok={probe?.status === 'ok'} class:settings-dot--warn={Boolean(probe) && probe?.status !== 'ok'} aria-hidden="true"></span>
          {probe ? statusLabel(probe.status) : i18n.t('webModel.status.notStarted')}
        </span>
        {#if probe?.reason}
          <span class="settings-row-desc">{i18n.t('webModel.reason.detail', { reason: probe.reason })}</span>
        {/if}
        {#if probe?.status === 'ok'}
          {#if accountPlanLabel}
            <span class="settings-row-desc">{i18n.t('settings.browser.webModel.accountHint', { hint: accountPlanLabel })}</span>
          {/if}
          <span class="settings-row-desc">{i18n.t('webModel.status.fixedEntry')}</span>
        {/if}
      </div>
      <button
        type="button"
        class={`btn btn--sm ${needsOpenHome || !probe ? 'btn--primary' : 'btn--secondary'}`}
        disabled={!isDesktop || probing}
        data-web-model-open="1"
        onclick={() => void openWebModelHome()}
      >
        {needsOpenHome || !probe ? i18n.t('webModel.action.openLogin') : i18n.t('webModel.action.openHome')}
      </button>
    </div>

    <div class="settings-row" data-web-model-slot="1">
      <div class="settings-row-copy">
        <span class="settings-row-title">{i18n.t('webModel.slot.title')}</span>
        {#if slotOwner}
          <span class="settings-row-desc">
            {slotOwner.active ? i18n.t('webModel.slot.ownedActive') : i18n.t('webModel.slot.owned')}
            · {slotOwner.sessionTitle?.trim() || i18n.t('webModel.slot.untitled')}
          </span>
        {:else}
          <span class="settings-row-desc">{i18n.t('webModel.slot.idle')}</span>
        {/if}
        {#if runtimeProjection.quota}
          <span class="settings-row-desc">{i18n.t('webModel.quota.notice')}</span>
          <span class="settings-row-desc">{i18n.t('webModel.quota.sentCount', { count: runtimeProjection.quota.sentMessages })}</span>
        {/if}
        <span class="settings-row-desc wm-fine">{i18n.t('webModel.slot.stopHint')}</span>
      </div>
      <button
        type="button"
        class="btn btn--sm btn--secondary"
        data-web-model-stop="1"
        disabled={!isDesktop || stopping || !slotOwner}
        onclick={() => void stopWeb()}
      >{i18n.t('webModel.action.stop')}</button>
    </div>
  </div>

  <details class="tools-group" data-web-model-tunnel="1">
    <summary class="settings-row wm-summary">
      <div class="settings-row-copy">
        <span class="settings-row-title">{i18n.t('webModel.group.tools')}</span>
        <span class="settings-row-desc">{tunnel?.channel === 'openai_tunnel' ? 'OpenAI Tunnel' : i18n.t('webModel.tunnel.channelNone')}</span>
      </div>
      <span class="group-summary-end">
        <span class="state" class:state--ok={tunnel?.ready}>
          {tunnel?.ready ? i18n.t('webModel.group.toolsReady') : i18n.t('webModel.group.toolsNotReady')}
        </span>
        <span class="group-chevron" aria-hidden="true"><Icon name="chevron-down" size={14} /></span>
      </span>
    </summary>

    <div class="settings-rows wm-nested">
      <div class="settings-row settings-row--stacked">
        <div class="settings-row-copy">
          <span class="settings-row-desc">{i18n.t('webModel.tunnel.setupGuide')}</span>
        </div>
        <div class="settings-row-actions">
          <button type="button" class="btn btn--sm btn--secondary" onclick={() => openPlatformPage('tunnels')}>
            {i18n.t('webModel.tunnel.openPlatformTunnels')}
          </button>
          <button type="button" class="btn btn--sm btn--secondary" onclick={() => openPlatformPage('api_keys')}>
            {i18n.t('webModel.tunnel.openPlatformKeys')}
          </button>
        </div>
      </div>

      {#if tunnel}
        <div class="settings-row">
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('webModel.tunnel.statusLabel')}</span>
            <span class="settings-row-desc">{tunnelStatusText(tunnel)}</span>
          </div>
        </div>
      {/if}

      <div class="settings-row">
        <div class="settings-row-copy"><span class="settings-row-title">Tunnel id</span></div>
        <div class="wm-control">
          <input class="form-input" bind:value={tunnelId} data-web-model-tunnel-id="1" placeholder="tunnel_..." />
        </div>
      </div>

      <div class="settings-row">
        <div class="settings-row-copy">
          <span class="settings-row-title">{i18n.t('webModel.tunnel.apiKey')}</span>
        </div>
        <div class="wm-control wm-control--inline">
          {#if tunnel?.hasApiKey && !keyEditing}
            <input
              class="form-input"
              type="text"
              readonly
              autocomplete="off"
              value={keyRevealed ? revealedKey : (tunnel.apiKeyPreview ?? '')}
              data-web-model-tunnel-credential="1"
            />
            <button type="button" class="btn btn--sm btn--secondary" onclick={() => void toggleKeyReveal()}>
              {keyRevealed ? i18n.t('webModel.tunnel.keyHide') : i18n.t('webModel.tunnel.keyShow')}
            </button>
            <button type="button" class="btn btn--sm btn--secondary" onclick={startEditingKey}>{i18n.t('webModel.tunnel.keyReplace')}</button>
          {:else}
            <input
              class="form-input"
              type={keyInputVisible ? 'text' : 'password'}
              autocomplete="off"
              bind:value={apiKey}
              data-web-model-tunnel-credential="1"
              placeholder="sk-..."
            />
            <button type="button" class="btn btn--sm btn--secondary" onclick={() => (keyInputVisible = !keyInputVisible)}>
              {keyInputVisible ? i18n.t('webModel.tunnel.keyHide') : i18n.t('webModel.tunnel.keyShow')}
            </button>
            {#if tunnel?.hasApiKey}
              <button type="button" class="btn btn--sm btn--secondary" onclick={cancelEditingKey}>{i18n.t('webModel.tunnel.keyCancel')}</button>
            {/if}
          {/if}
        </div>
      </div>

      <div class="settings-row">
        <div class="settings-row-copy"><span class="settings-row-title">{i18n.t('webModel.tunnel.toolProfile')}</span></div>
        <div class="wm-control">
          <select
            class="form-input"
            bind:value={toolProfile}
            data-web-model-tool-profile="1"
            onchange={() => void applyToolPolicy({ toolProfile })}
          >
            <option value="read_only">{i18n.t('webModel.tunnel.profile.readOnly')}</option>
            <option value="edit">{i18n.t('webModel.tunnel.profile.edit')}</option>
            <option value="edit_trusted">{i18n.t('webModel.tunnel.profile.editTrusted')}</option>
          </select>
        </div>
      </div>

      <div class="settings-row">
        <div class="settings-row-copy">
          <span class="settings-row-title">{i18n.t('webModel.tunnel.approvalMode')}</span>
          <span class="settings-row-desc">{i18n.t(`webModel.tunnel.approval.${approvalMode}Hint`)}</span>
        </div>
        <div class="wm-control">
          <select
            class="form-input"
            bind:value={approvalMode}
            data-web-model-approval-mode="1"
            onchange={() => void applyToolPolicy({ approvalMode })}
          >
            <option value="ask">{i18n.t('webModel.tunnel.approval.ask')}</option>
            <option value="always">{i18n.t('webModel.tunnel.approval.always')}</option>
            <option value="deny">{i18n.t('webModel.tunnel.approval.deny')}</option>
          </select>
        </div>
      </div>

      <div class="settings-row">
        <div class="settings-row-actions">
          <button type="button" class="btn btn--sm btn--primary" disabled={!isDesktop || tunnelBusy} onclick={() => void saveTunnel()}>
            {i18n.t('webModel.action.configureTunnel')}
          </button>
          <button type="button" class="btn btn--sm btn--secondary" disabled={!isDesktop || tunnelBusy} onclick={() => void clearTunnel()}>
            {i18n.t('webModel.action.clearTunnel')}
          </button>
        </div>
      </div>

      <div class="settings-row" data-web-model-connector="1">
        <div class="settings-row-copy">
          <span class="settings-row-title">{i18n.t('webModel.connector.title')}</span>
          {#if connectorMessage}
            <span class="settings-row-desc">{connectorMessage}</span>
          {/if}
        </div>
        <div class="settings-row-actions">
          <button type="button" class="btn btn--sm btn--secondary" disabled={!isDesktop || connectorBusy} onclick={() => void checkConnector()}>
            {i18n.t('webModel.connector.check')}
          </button>
          <button type="button" class="btn btn--sm btn--secondary" data-web-model-refresh-tools="1" disabled={!isDesktop || connectorBusy || !tunnelId.trim()} onclick={() => void refreshConnectorTools()}>
            {i18n.t('webModel.connector.refresh')}
          </button>
          <button type="button" class="btn btn--sm btn--secondary" disabled={!isDesktop || connectorBusy || !tunnelId.trim()} onclick={() => void configureConnector()}>
            {i18n.t('webModel.connector.configure')}
          </button>
        </div>
      </div>
    </div>
  </details>

  <div class="settings-rows">
    <div class="settings-row">
      <div class="settings-row-copy">
        <span class="settings-row-title">{i18n.t('webModel.action.clearData')}</span>
        <span class="settings-row-desc">{i18n.t('settings.browser.clearDataWebNotice')}</span>
      </div>
      <button
        type="button"
        class="btn btn--sm btn--secondary"
        data-web-model-clear-data="1"
        disabled={!isDesktop || clearing}
        onclick={() => void clearWebData()}
      >{i18n.t('webModel.action.clearData')}</button>
    </div>
  </div>

  {#if notice}
    <div class="settings-notice settings-notice--success">{notice}</div>
  {/if}
  {#if errorMessage}
    <div class="settings-notice settings-notice--error">{errorMessage}</div>
    {#if errorIsSlotBusy && slotOwner}
      <div class="settings-row-actions wm-actions-start">
        <button type="button" class="btn btn--sm btn--secondary" onclick={() => goToSlotOwnerSession(slotOwner)}>
          {i18n.t('webModel.slot.goToOwner')}
        </button>
        <button type="button" class="btn btn--sm btn--secondary" disabled={stopping} onclick={() => void stopWeb()}>
          {i18n.t('webModel.action.stop')}
        </button>
      </div>
    {/if}
  {/if}
</div>
</div>
</div>

<style>
  /* 视觉来自 settings.css 的共享类与全局 .btn / .form-input，这里只放本页特有的布局。 */
  .wm-control { flex: 0 1 340px; min-width: 0; }
  .wm-control--inline { display: flex; align-items: center; gap: 6px; }
  .wm-control--inline .form-input { flex: 1; }
  .wm-status-title { display: inline-flex; align-items: center; gap: 8px; }
  .wm-fine { color: var(--foreground-muted); opacity: 0.85; }
  .wm-actions-start { margin-top: 10px; }
  .wm-nested { border-top: 0; }

  /* 折叠分组：标题行本身就是一行，右侧是状态文字和展开箭头。 */
  .tools-group { border-top: 1px solid var(--border-subtle); margin-top: var(--space-3); }
  .tools-group > summary { cursor: pointer; list-style: none; }
  .tools-group > summary::-webkit-details-marker { display: none; }
  .tools-group > summary:hover .settings-row-title,
  .tools-group > summary:hover .group-chevron { color: var(--primary); }
  .tools-group > summary:focus-visible { outline: 2px solid var(--primary); outline-offset: -2px; border-radius: 6px; }
  .group-summary-end { display: inline-flex; align-items: center; gap: 8px; flex: none; }
  .group-summary-end .state { color: var(--foreground-muted); font-size: var(--text-xs); }
  .group-summary-end .state--ok { color: var(--success); }
  .group-chevron { display: inline-flex; color: var(--foreground-muted); transition: transform 0.15s ease; }
  .tools-group[open] > summary .group-chevron { transform: rotate(180deg); }

  @media (max-width: 640px) {
    .wm-control { flex: 1 1 100%; }
  }
</style>
