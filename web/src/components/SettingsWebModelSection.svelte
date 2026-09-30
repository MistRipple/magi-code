<script lang="ts">
  /**
   * 「设置 → 浏览器 → GPT Web 模型」分区（设计基线 §5.5、§5.11；实现计划 2.4）。
   *
   * 这里是发现通道的唯一入口：探测只返回候选，用户确认后才写入模型清单
   * （`engines` 是模型清单的唯一注册表）。
   *
   * 分区只展示 daemon 的探测事实，不自行推断 Web 模型是否可用：未登录 /
   * 探测失败时服务端返回 `engines = []`，前端不得用上一次结果占位。
   */
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { addToast } from '../stores/messages.svelte';
  import {
    configureWebModelTunnel,
    confirmWebModelConsent,
    createAppBrowserSession,
    discoverWebModels,
    getWebModelDiagnostics,
    getWebModelTunnel,
    listAgentRegistryEngines,
    resetWebModels,
    upsertAgentRegistryEngine,
    type WebModelDiscoveryResponse,
    type WebModelTunnelStatus,
  } from '../web/agent-api';
  import type { ModelEngine } from '../shared/types/registry-types';
  import {
    consumeWebModelSettingsRequest,
    dispatchWebModelAction,
    WEB_MODEL_SETTINGS_READY_EVENT,
    type WebModelActionKind,
    type WebModelSettingsRequest,
  } from '../web/web-model-actions';
  import {
    projectWebModelAppSession,
    refreshWebModelRuntime,
  } from '../web/web-model-session-projection';
  import { activateWebModelTab } from '../stores/right-pane.svelte';
  import { closeSettings } from '../stores/shell-ui.svelte';
  import {
    applyWebModelRuntime,
    webModelRuntimeState as readWebModelRuntimeState,
  } from '../stores/web-model-runtime.svelte';

  interface Props {
    isDesktop: boolean;
  }

  let { isDesktop }: Props = $props();

  let discovery = $state<WebModelDiscoveryResponse | null>(null);
  let discovering = $state(false);
  let saving = $state(false);
  let errorMessage = $state('');
  let notice = $state('');
  /** 已经写进模型清单的 Web 引擎（开关写回同一份注册表，不建第二处存储）。 */
  let engines = $state<ModelEngine[]>([]);
  let savingEngineId = $state('');
  let tunnel = $state<WebModelTunnelStatus | null>(null);
  let tunnelBusy = $state(false);
  let tunnelId = $state('');
  let credentialFile = $state('');
  let clearing = $state(false);
  let diagnosticsLoading = $state(false);
  let diagnosticsError = $state('');
  let diagnosticsCheckedAt = $state(0);
  const runtimeProjection = readWebModelRuntimeState();
  const DEFAULT_TOOL_ROUND_LIMIT = 20;
  let desktopDataLoaded = false;

  function webEngineEntries(all: ModelEngine[]): ModelEngine[] {
    return all.filter((engine) => engine.apiProtocol === 'chatgpt_web');
  }

  async function loadEngines(): Promise<void> {
    try {
      engines = webEngineEntries(await listAgentRegistryEngines());
    } catch (error) {
      console.warn('[SettingsWebModel] 读取 Web 引擎失败:', error);
    }
  }

  async function loadTunnel(): Promise<void> {
    if (!isDesktop) return;
    try {
      tunnel = await getWebModelTunnel();
      tunnelId = tunnel?.tunnelId || '';
      credentialFile = tunnel?.credentialFile || '';
    } catch (error) {
      console.warn('[SettingsWebModel] 读取 T3 通道状态失败:', error);
      tunnel = null;
    }
  }

  async function loadRuntime(): Promise<void> {
    if (!isDesktop) return;
    await refreshWebModelRuntime();
  }

  onMount(() => {
    void loadEngines();
    const handleSettingsReady = async (event: Event) => {
      const request = (event as CustomEvent<WebModelSettingsRequest>).detail;
      if (!request) return;
      if (request.focus === 'roundLimit' && engines.length === 0) {
        await loadEngines();
      }
      if (request.focus === 'roundLimit') {
        const target = request.engineId
          ? engines.find((engine) => engine.id === request.engineId)
          : engines.length === 1 ? engines[0] : null;
        if (target) {
          await updateEngineRoundLimit(
            target,
            Math.max(DEFAULT_TOOL_ROUND_LIMIT, (target.toolRoundLimit ?? DEFAULT_TOOL_ROUND_LIMIT) + 5),
          );
        }
      }
      window.setTimeout(() => {
        const targetEngineId = request.engineId
          || (engines.length === 1 ? engines[0]?.id : '');
        const target = request.focus === 'tunnel'
          ? document.querySelector<HTMLElement>('[data-web-model-tunnel]')
            : request.focus === 'roundLimit'
            ? Array.from(document.querySelectorAll<HTMLElement>('[data-web-model-round-limit]'))
              .find((element) => element.dataset.webModelRoundLimit === targetEngineId)
              ?? document.querySelector<HTMLElement>('[data-web-model-engines]')
            : document.querySelector<HTMLElement>('[data-web-model-diagnostics]');
        target?.scrollIntoView({ block: 'center', behavior: 'smooth' });
        if (request.focus === 'roundLimit' && target instanceof HTMLInputElement) target.focus();
      }, 0);
    };
    window.addEventListener(WEB_MODEL_SETTINGS_READY_EVENT, handleSettingsReady);
    const pending = consumeWebModelSettingsRequest();
    if (pending) window.dispatchEvent(new CustomEvent(WEB_MODEL_SETTINGS_READY_EVENT, { detail: pending }));
    const runtimeTimer = window.setInterval(() => {
      if (isDesktop) void loadRuntime();
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
    void loadRuntime();
  });

  /** 引擎级开关（工单 3.10）：写回既有 `engines` 条目，不新增设置段。 */
  async function toggleEngineSwitch(
    engine: ModelEngine,
    key: 'toolsEnabled' | 'newChatPerTurn',
    value: boolean,
  ): Promise<void> {
    if (savingEngineId) return;
    savingEngineId = engine.id;
    errorMessage = '';
    try {
      const saved = await upsertAgentRegistryEngine({ ...engine, [key]: value } as ModelEngine);
      engines = webEngineEntries(saved);
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      savingEngineId = '';
    }
  }

  async function updateEngineRoundLimit(engine: ModelEngine, value: number): Promise<void> {
    if (savingEngineId) return;
    const candidate = Number.isFinite(value)
      ? Math.floor(value)
      : (engine.toolRoundLimit ?? DEFAULT_TOOL_ROUND_LIMIT);
    const next = Math.max(1, Math.min(100, candidate));
    savingEngineId = engine.id;
    errorMessage = '';
    try {
      const saved = await upsertAgentRegistryEngine({
        ...engine,
        toolRoundLimit: next,
      } as ModelEngine);
      engines = webEngineEntries(saved);
      notice = i18n.t('webModel.engine.roundLimitSaved', { limit: next });
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      savingEngineId = '';
    }
  }

  async function saveTunnel(): Promise<void> {
    if (tunnelBusy) return;
    tunnelBusy = true;
    errorMessage = '';
    try {
      tunnel = await configureWebModelTunnel({ tunnelId, credentialFile });
      if (tunnel.ready) {
        addToast('success', i18n.t('webModel.tunnel.status', { status: tunnel.code }));
      } else if (tunnel.code) {
        addToast('warning', i18n.t(`webModel.tunnel.${tunnel.code === 'client_missing' ? 'clientMissing' : 'credentialMissing'}`));
      }
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      tunnelBusy = false;
    }
  }

  async function clearTunnel(): Promise<void> {
    if (tunnelBusy) return;
    tunnelBusy = true;
    errorMessage = '';
    try {
      tunnel = await configureWebModelTunnel({ clear: true });
      tunnelId = '';
      credentialFile = '';
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      tunnelBusy = false;
    }
  }

  /**
   * 「清除数据」：先让 daemon 撤销在飞回复并隐藏 Web 模型，再按应用级分区粒度清
   * 登录态（§5.13、工单 3.11）。两者顺序不能反过来。
   */
  async function clearWebData(): Promise<void> {
    if (clearing) return;
    const desktop = window.magiDesktop;
    if (!isDesktop || !desktop?.clearWebModelData) {
      addToast('warning', i18n.t('settings.browser.webModel.needDesktop'));
      return;
    }
    if (!window.confirm(i18n.t('webModel.clearData.confirmBody'))) return;
    clearing = true;
    errorMessage = '';
    try {
      await resetWebModels();
      await desktop.clearWebModelData();
      applyWebModelRuntime(null);
      discovery = null;
      tunnel = null;
      tunnelId = '';
      credentialFile = '';
      notice = i18n.t('webModel.clearData.done');
      await loadEngines();
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      clearing = false;
    }
  }

  /** 首次说明确认（A9）：只在「点击连接且尚未确认」这一步出现。 */
  const needsConsent = $derived(discovery?.status === 'consent_required');

  function errorText(error: unknown): string {
    if (error instanceof Error && error.message.trim()) return error.message.trim();
    return i18n.t('settings.browser.webModel.loadFailed');
  }

  /** 应用级会话要先存在，宿主内容槽才会挂载主页 guest，探测才可能读到页面。 */
  async function ensureAppSession(): Promise<void> {
    await createAppBrowserSession();
    // 仅创建 daemon 侧的应用级逻辑会话还不够：GPT Web 的真实 guest 由
    // RightPane 的 App 内容槽挂载。设置页也可能是用户的第一个入口，因此
    // 必须在探测前主动把同一份 Authority 会话投影到 appTabs；否则 daemon
    // 的 ensure_surface 只能等待一个永远不存在的 webview（R49）。
    await projectWebModelAppSession();
    // 让 RightPane / WebModelTabContent 完成一次 DOM 提交；Electron 的
    // webview 仍会在随后异步 attach，daemon 的后台 ensure_surface 会继续
    // 等待真实注册，不在这里制造第二条宿主控制路径。
    await tick();
  }

  async function refresh(): Promise<void> {
    if (!isDesktop || discovering) return;
    discovering = true;
    errorMessage = '';
    notice = '';
    try {
      await ensureAppSession();
      discovery = await discoverWebModels();
      await loadEngines();
      await loadTunnel();
      await loadRuntime();
    } catch (error) {
      console.warn('[SettingsWebModel] 探测失败:', error);
      errorMessage = errorText(error);
      discovery = null;
    } finally {
      discovering = false;
    }
  }

  async function runDiagnostics(): Promise<void> {
    if (!isDesktop || diagnosticsLoading) return;
    diagnosticsLoading = true;
    diagnosticsError = '';
    try {
      await ensureAppSession();
      const snapshot = await getWebModelDiagnostics();
      discovery = snapshot.discovery;
      tunnel = snapshot.tunnel;
      diagnosticsCheckedAt = snapshot.checkedAt;
      applyWebModelRuntime(snapshot.runtime);
      const errors = Object.entries(snapshot.errors)
        .map(([key, value]) => `${key}: ${value}`)
        .join(' · ');
      diagnosticsError = errors;
    } catch (error) {
      diagnosticsError = errorText(error);
    } finally {
      diagnosticsLoading = false;
    }
  }

  async function confirmConsent(): Promise<void> {
    if (saving) return;
    saving = true;
    errorMessage = '';
    try {
      await confirmWebModelConsent();
      await refresh();
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      saving = false;
    }
  }

  /** 把探测候选写入模型清单：Web 引擎不写 `llm`，来源与窗口写在顶层（A22）。 */
  async function writeEngines(): Promise<void> {
    const engines = discovery?.engines ?? [];
    if (saving || engines.length === 0) return;
    saving = true;
    errorMessage = '';
    try {
      for (const engine of engines) {
        await upsertAgentRegistryEngine({
          id: engine.id,
          displayName: engine.displayName,
          apiProtocol: engine.apiProtocol,
          contextWindowTokens: engine.contextWindowTokens,
          efforts: engine.efforts,
          toolsEnabled: engine.toolsEnabled,
          newChatPerTurn: engine.newChatPerTurn,
          toolRoundLimit: engine.toolRoundLimit ?? DEFAULT_TOOL_ROUND_LIMIT,
          origin: engine.origin,
        } as unknown as ModelEngine);
      }
      notice = i18n.t('webModel.discovery.saved');
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      saving = false;
    }
  }

  /** 诊断投影：全部来自 daemon 事实，前端不做任何推断（工单 5.2）。 */
  const diagnostics = $derived.by(() => {
    const entries: Array<{ label: string; value: string }> = [];
    if (discovery?.siteRevision) {
      entries.push({
        label: i18n.t('webModel.diagnostics.site', { revision: discovery.siteRevision }),
        value: discovery.siteRevision,
      });
    }
    if (discovery?.limitsRevision) {
      entries.push({
        label: i18n.t('webModel.diagnostics.limits', { revision: discovery.limitsRevision }),
        value: discovery.limitsRevision,
      });
    }
    for (const engine of engines) {
      entries.push({
        label: `${engine.displayName || engine.id} · ${engine.id}`,
        value: `${engine.efforts?.join(' / ') || '-'} · ${engine.toolsEnabled === false ? 'T0' : 'T2/T3'} · ${
          engine.newChatPerTurn ? 'per-turn' : 'reuse'
        } · ${i18n.t('webModel.engine.roundLimitValue', {
          limit: engine.toolRoundLimit ?? DEFAULT_TOOL_ROUND_LIMIT,
        })}`,
      });
    }
    if (tunnel) {
      entries.push({
        label: i18n.t('webModel.diagnostics.tunnelState', {
          state: tunnel.ready ? tunnel.channel : `${tunnel.channel} · ${tunnel.code}`,
        }),
        value: tunnel.detail || '',
      });
    }
    return entries;
  });

  function statusLabel(status: string): string {
    switch (status) {
      case 'login_required':
        return i18n.t('webModel.status.loginRequired');
      case 'consent_required':
        return i18n.t('webModel.status.consentRequired');
      case 'desktop_unavailable':
        return i18n.t('webModel.status.desktopUnavailable');
      case 'refresh_required':
        return i18n.t('webModel.status.refreshRequired');
      case 'site_blocked':
        return i18n.t('webModel.status.siteBlocked');
      case 'quota_exhausted':
        return i18n.t('webModel.status.quotaExhausted');
      case 'tool_degraded':
        return i18n.t('webModel.status.toolDegraded');
      case 'failed':
        return i18n.t('webModel.status.failed');
      default:
        return i18n.t('webModel.status.available');
    }
  }

  type DiscoveryAction =
    | { kind: 'refresh'; labelKey: string }
    | { kind: WebModelActionKind; labelKey: string };

  function discoveryAction(status: string): DiscoveryAction | null {
    switch (status) {
      case 'login_required':
      case 'site_blocked':
        return { kind: 'openView', labelKey: 'webModel.action.openHome' };
      case 'refresh_required':
        return { kind: 'refresh', labelKey: 'webModel.action.refresh' };
      case 'quota_exhausted':
        return { kind: 'switchModel', labelKey: 'webModel.action.switchModel' };
      case 'tool_degraded':
        return { kind: 'openTunnelSettings', labelKey: 'webModel.action.configureTunnel' };
      case 'desktop_unavailable':
        return { kind: 'openSettings', labelKey: 'webModel.action.openSettings' };
      case 'failed':
        return { kind: 'runDiagnostics', labelKey: 'webModel.action.runDiagnostics' };
      default:
        return null;
    }
  }

  function runDiscoveryAction(action: DiscoveryAction): void {
    if (action.kind === 'refresh') {
      void refresh();
      return;
    }
    if (action.kind === 'runDiagnostics') {
      void runDiagnostics();
      return;
    }
    if (action.kind === 'openView') {
      void openWebModelHome();
      return;
    }
    dispatchWebModelAction({ kind: action.kind });
  }

  /**
   * 设置页本身可能是当前唯一挂载的顶层视图，RightPane 也可能因尚未有
   * appTabs 而尚未挂载；此时不能只派发给 RightPane 的事件总线。直接投影
   * 同一份应用级会话、激活 app Tab，再返回工作台，保证「打开主页」在
   * 未登录、页面风控和刷新失败三种状态下都能真正到达 GPT Web guest。
   */
  async function openWebModelHome(): Promise<void> {
    if (!isDesktop || discovering) return;
    discovering = true;
    errorMessage = '';
    try {
      await ensureAppSession();
      activateWebModelTab();
      closeSettings();
    } catch (error) {
      errorMessage = errorText(error);
    } finally {
      discovering = false;
    }
  }

  function runtimeStageLabel(stage: string, queuePosition: number | null): string {
    if (typeof queuePosition === 'number' && queuePosition > 0) {
      return i18n.t('webModel.turnStage.queued', { position: queuePosition });
    }
    if (stage === 'waiting_engine') return i18n.t('webModel.turnStage.waitingEngine');
    if (stage === 'generating') return i18n.t('webModel.turnStage.generating');
    if (stage === 'finished') return i18n.t('webModel.runtime.finished');
    return stage;
  }
</script>

<section class="web-model-section">
  <header class="section-heading">
    <div>
      <h3>{i18n.t('settings.browser.webModel.title')}</h3>
      <p>{i18n.t('settings.browser.webModel.desc')}</p>
    </div>
    <button
      type="button"
      class="primary-action"
      disabled={!isDesktop || discovering}
      onclick={() => void refresh()}
    >
      {discovering
        ? i18n.t('settings.browser.webModel.connecting')
        : i18n.t('settings.browser.webModel.connect')}
    </button>
  </header>

  {#if !isDesktop}
    <p class="status-message">{i18n.t('settings.browser.webModel.needDesktop')}</p>
  {/if}

  {#if needsConsent}
    <div class="card">
      <h4>{i18n.t('webModel.consent.title')}</h4>
      <p class="consent-body">{i18n.t('webModel.consent.body')}</p>
      <div class="action-row">
        <button type="button" class="primary-action" disabled={saving} onclick={() => void confirmConsent()}>
          {i18n.t('webModel.consent.confirm')}
        </button>
        <button type="button" disabled={saving} onclick={() => { discovery = null; }}>
          {i18n.t('webModel.consent.cancel')}
        </button>
      </div>
    </div>
  {:else if discovery}
    {#if discovery.status === 'ok'}
      <div class="card">
        <h4>{i18n.t('webModel.discovery.previewTitle')}</h4>
        <p>{i18n.t('settings.browser.webModel.previewHint')}</p>
        <p class="meta">{i18n.t('settings.browser.webModel.accountHint', { hint: discovery.accountHint })}</p>
        {#if discovery.limitsRevision}
          <p class="meta">{i18n.t('settings.browser.webModel.limitsRevision', { revision: discovery.limitsRevision })}</p>
        {/if}
        <ul class="engine-list">
          {#each discovery.engines as engine (engine.id)}
            <li>
              <div class="engine-main">
                <span class="engine-name">{engine.displayName}</span>
                <span class="badge">{i18n.t('webModel.badge.fromWeb')}</span>
              </div>
              <div class="engine-meta">
                <code>{engine.id}</code>
                <span>{engine.contextWindowTokens} tokens</span>
                {#if engine.efforts.length > 0}
                  <span>{engine.efforts.join(' / ')}</span>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
        <div class="action-row">
          <button type="button" class="primary-action" disabled={saving} onclick={() => void writeEngines()}>
            {i18n.t('webModel.discovery.confirm')}
          </button>
          <button type="button" disabled={saving} onclick={() => { discovery = null; }}>
            {i18n.t('webModel.discovery.cancel')}
          </button>
        </div>
      </div>
    {:else}
      {@const primaryDiscoveryAction = discoveryAction(discovery.status)}
      <div class="card status-card">
        <div class="status-title">
          <Icon name="globe" size={14} />
          <span>{statusLabel(discovery.status)}</span>
        </div>
        {#if discovery.reason === 'selectors_drift'}
          <p class="meta">{i18n.t('webModel.reason.selectorsDrift')}</p>
        {:else if discovery.reason === 'risk_page'}
          <p class="meta">{i18n.t('webModel.reason.riskPage')}</p>
        {:else if discovery.reason}
          <p class="meta">{i18n.t('webModel.reason.detail', { reason: discovery.reason })}</p>
        {/if}
        <div class="action-row">
          {#if primaryDiscoveryAction}
            <button
              type="button"
              class="primary-action"
              disabled={discovering}
              onclick={() => runDiscoveryAction(primaryDiscoveryAction)}
            >
              {i18n.t(primaryDiscoveryAction.labelKey)}
            </button>
          {/if}
          <button type="button" disabled={discovering} onclick={() => void refresh()}>
            {i18n.t('webModel.action.retry')}
          </button>
        </div>
      </div>
    {/if}
  {/if}

  <div class="card" data-web-model-engines="1">
    <h4>{i18n.t('webModel.engine.section')}</h4>
    {#if engines.length === 0}
      <p class="meta">{i18n.t('webModel.engine.empty')}</p>
    {:else}
      <ul class="engine-list">
        {#each engines as engine (engine.id)}
          <li>
            <div class="engine-main">
              <span class="engine-name">{engine.displayName || engine.id}</span>
              <span class="badge">{i18n.t('webModel.badge.fromWeb')}</span>
            </div>
            <div class="switch-row">
              <label>
                <input
                  type="checkbox"
                  data-web-model-tools={engine.id}
                  checked={engine.toolsEnabled !== false}
                  disabled={savingEngineId !== ''}
                  onchange={(event) => void toggleEngineSwitch(
                    engine,
                    'toolsEnabled',
                    (event.currentTarget as HTMLInputElement).checked,
                  )}
                />
                <span>{i18n.t('webModel.engine.tools')}</span>
              </label>
              <label>
                <input
                  type="checkbox"
                  data-web-model-new-chat={engine.id}
                  checked={engine.newChatPerTurn === true}
                  disabled={savingEngineId !== ''}
                  onchange={(event) => void toggleEngineSwitch(
                    engine,
                    'newChatPerTurn',
                    (event.currentTarget as HTMLInputElement).checked,
                  )}
                />
                <span>{i18n.t('webModel.engine.newChatPerTurn')}</span>
              </label>
            </div>
            <label class="field round-limit-field">
              <span>{i18n.t('webModel.engine.roundLimit')}</span>
              <input
                type="number"
                min="1"
                max="100"
                step="1"
                data-web-model-round-limit={engine.id}
                aria-label={i18n.t('webModel.engine.roundLimit')}
                value={engine.toolRoundLimit ?? DEFAULT_TOOL_ROUND_LIMIT}
                disabled={savingEngineId !== ''}
                onchange={(event) => void updateEngineRoundLimit(
                  engine,
                  Number.parseInt((event.currentTarget as HTMLInputElement).value, 10),
                )}
              />
            </label>
            <p class="meta">{i18n.t('webModel.engine.toolsHint')}</p>
            <p class="meta">{i18n.t('webModel.engine.roundLimitHint')}</p>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="card" data-web-model-tunnel="1">
    <h4>{i18n.t('webModel.tunnel.channel', { channel: tunnel?.channel || 'none' })}</h4>
    <p class="meta">{i18n.t('webModel.tunnel.setupGuide')}</p>
    {#if tunnel}
      <p class="meta">{i18n.t('webModel.tunnel.status', { status: tunnel.ready ? tunnel.code : `${tunnel.code} · ${tunnel.detail}` })}</p>
    {/if}
    <label class="field">
      <span>Tunnel id</span>
      <input bind:value={tunnelId} data-web-model-tunnel-id="1" placeholder="tunnel_..." />
    </label>
    <label class="field">
      <span>{i18n.t('webModel.tunnel.credentialMissing')}</span>
      <input bind:value={credentialFile} data-web-model-tunnel-credential="1" placeholder="/path/to/api-key" />
    </label>
    <div class="action-row">
      <button type="button" class="primary-action" disabled={!isDesktop || tunnelBusy} onclick={() => void saveTunnel()}>
        {i18n.t('webModel.action.configureTunnel')}
      </button>
      <button type="button" disabled={!isDesktop || tunnelBusy} onclick={() => void clearTunnel()}>
        {i18n.t('webModel.action.logout')}
      </button>
    </div>
  </div>

  <div class="card" data-web-model-diagnostics="1">
    <h4>{i18n.t('webModel.diagnostics.section')}</h4>
    {#if diagnostics.length === 0}
      <p class="meta">{i18n.t('webModel.diagnostics.clean')}</p>
    {:else}
      <ul class="engine-list">
        {#each diagnostics as entry (entry.label)}
          <li>
            <span class="engine-name">{entry.label}</span>
            <span class="engine-meta">{entry.value}</span>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="runtime-summary" data-web-model-runtime="1">
      <strong>{i18n.t('webModel.runtime.section')}</strong>
      <p class="meta">{i18n.t('webModel.quota.notice')}</p>
      {#if runtimeProjection.quota}
        <p class="meta">
          {i18n.t('webModel.quota.sentCount', { count: runtimeProjection.quota.sentMessages })}
        </p>
      {:else}
        <p class="meta">{i18n.t('webModel.quota.unavailable')}</p>
      {/if}
      {#if runtimeProjection.entries.length === 0}
        <p class="meta">{i18n.t('webModel.runtime.idle')}</p>
      {:else}
        <ul class="engine-list">
          {#each runtimeProjection.entries as entry (`${entry.sessionId}:${entry.threadId}:${entry.engineId}:${entry.epoch}`)}
            <li>
              <div class="engine-main">
                <span class="engine-name">{entry.engineId}</span>
                <span class="badge">{runtimeStageLabel(entry.stage, entry.queuePosition)}</span>
              </div>
              <div class="engine-meta">
                <code>{entry.threadId}</code>
                <span>{entry.active ? i18n.t('webModel.runtime.active') : i18n.t('webModel.runtime.finished')}</span>
                <span>{i18n.t('webModel.runtime.sentMessages', { count: entry.sentMessages })}</span>
                {#if entry.queuePosition !== null}
                  <span>{i18n.t('webModel.runtime.queuePosition', { position: entry.queuePosition })}</span>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
    {#if diagnosticsCheckedAt > 0}
      <p class="meta">{i18n.t('webModel.diagnostics.checkedAt', {
        time: new Date(diagnosticsCheckedAt).toLocaleTimeString(),
      })}</p>
    {/if}
    {#if diagnosticsError}
      <p class="status-message error" role="alert">{diagnosticsError}</p>
    {/if}
    <div class="action-row">
      <button
        type="button"
        disabled={!isDesktop || diagnosticsLoading}
        aria-busy={diagnosticsLoading}
        onclick={() => void runDiagnostics()}
      >
        {diagnosticsLoading ? i18n.t('webModel.action.diagnosing') : i18n.t('webModel.action.runDiagnostics')}
      </button>
      <button
        type="button"
        data-web-model-clear-data="1"
        disabled={!isDesktop || clearing}
        onclick={() => void clearWebData()}
      >{i18n.t('webModel.action.clearData')}</button>
    </div>
    <p class="meta">{i18n.t('settings.browser.clearDataWebNotice')}</p>
  </div>

  {#if notice}
    <p class="status-message success">{notice}</p>
  {/if}
  {#if errorMessage}
    <p class="status-message error">{errorMessage}</p>
  {/if}
</section>

<style>
  .web-model-section {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 0;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
  }

  .section-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  .section-heading h3 {
    margin: 0 0 4px;
    font-size: 14px;
    font-weight: 600;
  }

  .section-heading p {
    margin: 0;
    max-width: 560px;
    font-size: 12px;
    line-height: 1.5;
    opacity: 0.72;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.18));
    border-radius: 10px;
  }

  .card h4 {
    margin: 0;
    font-size: 13px;
  }

  .consent-body {
    margin: 0;
    font-size: 12px;
    line-height: 1.6;
    opacity: 0.85;
  }

  .meta {
    margin: 0;
    font-size: 11px;
    opacity: 0.66;
  }

  .switch-row {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    font-size: 11px;
  }

  .switch-row label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
  }

  .round-limit-field {
    align-items: flex-start;
    max-width: 160px;
  }

  .round-limit-field input {
    width: 72px;
    padding: 4px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 12px;
    background: transparent;
    color: inherit;
  }

  .runtime-summary {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-top: 8px;
    border-top: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.14));
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
  }

  .field input {
    padding: 4px 8px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 6px;
    font-size: 12px;
    background: transparent;
    color: inherit;
  }

  .engine-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
  }

  .engine-list li {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--surface-muted, rgba(127, 127, 127, 0.08));
  }

  .engine-main {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .engine-name {
    font-size: 12px;
    font-weight: 600;
  }

  .badge {
    padding: 1px 6px;
    border-radius: 999px;
    font-size: 10px;
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    color: var(--accent-strong, #3b6fd4);
  }

  .engine-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 11px;
    opacity: 0.7;
  }

  .action-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 4px;
  }

  button {
    padding: 5px 12px;
    border: 1px solid var(--border-subtle, rgba(127, 127, 127, 0.24));
    border-radius: 8px;
    font-size: 12px;
    background: transparent;
    cursor: pointer;
  }

  button.primary-action {
    background: var(--accent-soft, rgba(80, 140, 255, 0.16));
    border-color: transparent;
    color: var(--accent-strong, #3b6fd4);
  }

  button:disabled {
    cursor: default;
    opacity: 0.45;
  }

  .status-title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    font-weight: 600;
  }

  .status-message {
    margin: 0;
    font-size: 12px;
  }

  .status-message.error { color: var(--danger, #c53f4f); }
  .status-message.success { color: var(--success, #2f9e63); }
</style>
