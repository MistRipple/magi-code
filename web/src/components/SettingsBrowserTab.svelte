<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    getBrowserCapabilities,
    updateBrowserSettings,
    type BrowserCapabilitiesSnapshot,
  } from '../web/agent-api';

  type DesktopAction = 'refresh-components' | 'restart-automation' | 'clear-data' | 'check-updates';

  let capabilitySnapshot = $state<BrowserCapabilitiesSnapshot | null>(null);
  let desktopInfo = $state<MagiDesktopBrowserComponentInfo | null>(null);
  let desktopUpdate = $state<MagiDesktopUpdateSnapshot | null>(null);
  let isDesktop = $state(false);
  let capabilityLoading = $state(false);
  let desktopLoading = $state(false);
  let capabilityError = $state('');
  let desktopError = $state('');
  let actionNotice = $state('');
  let activeAction = $state<DesktopAction | ''>('');
  let savingSetting = $state<'inAppBrowserEnabled' | 'browserUseEnabled' | ''>('');
  let actionNoticeTimer: ReturnType<typeof setTimeout> | null = null;

  function clearActionFeedback(): void {
    if (actionNoticeTimer !== null) {
      clearTimeout(actionNoticeTimer);
      actionNoticeTimer = null;
    }
    actionNotice = '';
    desktopError = '';
  }

  function showActionNotice(message: string): void {
    clearActionFeedback();
    actionNotice = message;
    actionNoticeTimer = setTimeout(() => {
      actionNotice = '';
      actionNoticeTimer = null;
    }, 3500);
  }

  function errorMessage(error: unknown): string {
    if (error instanceof Error && error.message.trim()) return error.message.trim();
    return i18n.t('settings.browser.actionFailed');
  }

  async function loadCapabilities(): Promise<void> {
    if (capabilityLoading) return;
    capabilityLoading = true;
    capabilityError = '';
    try {
      capabilitySnapshot = await getBrowserCapabilities();
    } catch (error) {
      console.warn('[SettingsBrowserTab] 获取浏览器能力设置失败:', error);
      capabilityError = i18n.t('settings.browser.loadCapabilitiesFailed');
    } finally {
      capabilityLoading = false;
    }
  }

  async function loadDesktopInfo(): Promise<void> {
    const desktop = window.magiDesktop;
    if (!isDesktop || !desktop || desktopLoading) return;
    desktopLoading = true;
    desktopError = '';
    try {
      desktopInfo = await desktop.getBrowserComponentInfo();
    } catch (error) {
      console.warn('[SettingsBrowserTab] 获取桌面浏览器组件状态失败:', error);
      desktopInfo = null;
      desktopError = errorMessage(error);
    } finally {
      desktopLoading = false;
    }
  }

  async function saveCapabilitySetting(
    key: 'inAppBrowserEnabled' | 'browserUseEnabled',
    enabled: boolean,
  ): Promise<void> {
    if (!capabilitySnapshot || savingSetting) return;
    savingSetting = key;
    capabilityError = '';
    try {
      capabilitySnapshot = await updateBrowserSettings({
        inAppBrowserEnabled: key === 'inAppBrowserEnabled'
          ? enabled
          : capabilitySnapshot.inAppBrowserEnabled,
        browserUseEnabled: key === 'browserUseEnabled'
          ? enabled
          : capabilitySnapshot.browserUseEnabled,
      });
      window.dispatchEvent(new CustomEvent('magi:browserCapabilitiesChanged', {
        detail: capabilitySnapshot,
      }));
    } catch (error) {
      console.warn('[SettingsBrowserTab] 保存浏览器能力设置失败:', error);
      capabilityError = i18n.t('settings.browser.saveFailed');
    } finally {
      savingSetting = '';
    }
  }

  async function runDesktopAction(action: DesktopAction): Promise<void> {
    const desktop = window.magiDesktop;
    if (!isDesktop || !desktop || activeAction) return;
    activeAction = action;
    clearActionFeedback();
    try {
      if (action === 'refresh-components') {
        desktopInfo = await desktop.getBrowserComponentInfo();
        showActionNotice(i18n.t('settings.browser.refreshSucceeded'));
      } else if (action === 'restart-automation') {
        desktopInfo = await desktop.restartBrowserAutomation();
        showActionNotice(i18n.t('settings.browser.restartAutomationSucceeded'));
      } else if (action === 'clear-data') {
        await desktop.clearBrowserData();
        showActionNotice(i18n.t('settings.browser.clearDataSucceeded'));
      } else {
        const next = await desktop.checkForUpdates();
        desktopUpdate = next;
        if (next.status === 'failed') {
          throw new Error(next.error || i18n.t('settings.browser.actionFailed'));
        }
        if (next.status === 'unsupported') {
          showActionNotice(i18n.t('settings.browser.updateStatus.unsupported'));
          return;
        }
        showActionNotice(next.availableVersion
          ? i18n.t('settings.browser.desktopUpdateAvailable', { version: next.availableVersion })
          : i18n.t('settings.browser.desktopUpToDate'));
      }
    } catch (error) {
      console.warn(`[SettingsBrowserTab] 桌面浏览器操作失败: ${action}`, error);
      desktopError = errorMessage(error);
    } finally {
      activeAction = '';
    }
  }

  function componentStatus(
    status: 'starting' | 'ready' | 'restarting' | 'failed' | 'stopped' | 'protocol-incompatible' | undefined,
  ): string {
    if (desktopLoading) return i18n.t('settings.browser.status.loading');
    return i18n.t(`settings.browser.status.${status ?? 'unavailable'}`);
  }

  function protocolVersion(): string {
    const version = desktopInfo?.protocol_version;
    return version ? `${version.major}.${version.minor}` : '-';
  }

  function updateStatusText(): string {
    if (!desktopUpdate) return i18n.t('settings.browser.updateStatus.notChecked');
    return i18n.t(`settings.browser.updateStatus.${desktopUpdate.status}`);
  }

  function actionLabel(action: DesktopAction): string {
    if (activeAction !== action) {
      if (action === 'refresh-components') return i18n.t('settings.browser.refreshComponents');
      if (action === 'restart-automation') return i18n.t('settings.browser.restartAutomation');
      if (action === 'clear-data') return i18n.t('settings.browser.clearData');
      return i18n.t('settings.browser.checkDesktopUpdates');
    }
    if (action === 'refresh-components') return i18n.t('settings.browser.refreshingComponents');
    if (action === 'restart-automation') return i18n.t('settings.browser.restartingAutomation');
    if (action === 'clear-data') return i18n.t('settings.browser.clearingData');
    return i18n.t('settings.browser.checkingDesktopUpdates');
  }

  onMount(() => {
    const desktop = window.magiDesktop;
    isDesktop = desktop?.runtime === 'electron';
    void loadCapabilities();

    let unsubscribeUpdate: (() => void) | null = null;
    if (isDesktop && desktop) {
      void loadDesktopInfo();
      unsubscribeUpdate = desktop.onUpdate((next) => {
        desktopUpdate = next;
      });
    }

    return () => {
      unsubscribeUpdate?.();
      if (actionNoticeTimer !== null) clearTimeout(actionNoticeTimer);
    };
  });
</script>

<div class="settings-tab-inner browser-settings-tab">
  <div class="browser-settings-content">
    <section class="settings-section" aria-labelledby="browser-capabilities-title">
      <div class="section-heading">
        <div>
          <h3 id="browser-capabilities-title">{i18n.t('settings.browser.capabilitiesTitle')}</h3>
          <p>{i18n.t('settings.browser.capabilitiesDescription')}</p>
        </div>
      </div>
      <div class="capability-list">
        <div class="capability-row">
          <div>
            <strong>{i18n.t('settings.browser.inAppBrowser')}</strong>
            <span>{i18n.t('settings.browser.inAppBrowserDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.inAppBrowserEnabled ?? false}
            disabled={!capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.inAppBrowser')}
            onchange={(enabled) => void saveCapabilitySetting('inAppBrowserEnabled', enabled)}
          />
        </div>
        <div class="capability-row">
          <div>
            <strong>{i18n.t('settings.browser.browserUse')}</strong>
            <span>{i18n.t('settings.browser.browserUseDescription')}</span>
          </div>
          <Toggle
            checked={capabilitySnapshot?.browserUseEnabled ?? false}
            disabled={!capabilitySnapshot || capabilityLoading || Boolean(savingSetting)}
            ariaLabel={i18n.t('settings.browser.browserUse')}
            onchange={(enabled) => void saveCapabilitySetting('browserUseEnabled', enabled)}
          />
        </div>
      </div>
      {#if capabilityError}
        <div class="status-message status-message--error" role="status">
          <Icon name="alert-circle" size={14} />
          <span>{capabilityError}</span>
        </div>
      {/if}
    </section>

    {#if isDesktop}
      <section class="settings-section desktop-components" aria-labelledby="browser-components-title">
        <div class="section-heading">
          <div class="section-icon" aria-hidden="true"><Icon name="globe" size={18} /></div>
          <div>
            <h3 id="browser-components-title">{i18n.t('settings.browser.componentsTitle')}</h3>
            <p>{i18n.t('settings.browser.componentsDescription')}</p>
          </div>
          <button
            type="button"
            class="icon-action"
            class:icon-action--loading={activeAction === 'refresh-components' || desktopLoading}
            onclick={() => void runDesktopAction('refresh-components')}
            disabled={desktopLoading || Boolean(activeAction)}
            title={i18n.t('settings.browser.refreshComponents')}
            aria-label={i18n.t('settings.browser.refreshComponents')}
          ><Icon name="refresh" size={14} /></button>
        </div>

        <div class="component-list" aria-live="polite">
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.desktopHost')}</strong><span>{componentStatus(desktopInfo ? 'ready' : undefined)}</span></div>
            <code>{desktopInfo?.desktop_version ?? '-'}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.electron')}</strong><span>{componentStatus(desktopInfo ? 'ready' : undefined)}</span></div>
            <code>{desktopInfo?.electron_version ?? '-'}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.chromium')}</strong><span>{componentStatus(desktopInfo ? 'ready' : undefined)}</span></div>
            <code>{desktopInfo?.chromium_version ?? '-'}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.daemon')}</strong><span>{componentStatus(desktopInfo?.daemon_status)}</span></div>
            <code>{desktopInfo?.daemon_version ?? '-'}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.automationWorker')}</strong><span>{componentStatus(desktopInfo?.automation_worker_status)}</span></div>
            <code>{desktopInfo?.automation_worker_version ?? '-'}</code>
          </div>
          <div class="component-row">
            <div><strong>{i18n.t('settings.browser.component.protocol')}</strong><span>{componentStatus(desktopInfo?.protocol_compatible ? 'ready' : 'protocol-incompatible')}</span></div>
            <code>{protocolVersion()}</code>
          </div>
        </div>
      </section>

      <section class="settings-section desktop-actions" aria-labelledby="browser-actions-title">
        <div class="section-heading">
          <div>
            <h3 id="browser-actions-title">{i18n.t('settings.browser.actionsTitle')}</h3>
            <p>{i18n.t('settings.browser.actionsDescription')}</p>
          </div>
        </div>

        <div class="action-list">
          <div class="action-row">
            <div>
              <strong>{i18n.t('settings.browser.restartAutomation')}</strong>
              <span>{i18n.t('settings.browser.restartAutomationDescription')}</span>
            </div>
            <button
              type="button"
              onclick={() => void runDesktopAction('restart-automation')}
              disabled={Boolean(activeAction) || !desktopInfo}
            >{actionLabel('restart-automation')}</button>
          </div>
          <div class="action-row">
            <div>
              <strong>{i18n.t('settings.browser.clearData')}</strong>
              <span>{i18n.t('settings.browser.clearDataDescription')}</span>
            </div>
            <button
              type="button"
              class="danger"
              onclick={() => void runDesktopAction('clear-data')}
              disabled={Boolean(activeAction) || !desktopInfo}
            >{actionLabel('clear-data')}</button>
          </div>
          <div class="action-row">
            <div>
              <strong>{i18n.t('settings.browser.checkDesktopUpdates')}</strong>
              <span>{i18n.t('settings.browser.checkDesktopUpdatesDescription')}</span>
              <small>{updateStatusText()}</small>
            </div>
            <button
              type="button"
              onclick={() => void runDesktopAction('check-updates')}
              disabled={Boolean(activeAction) || !desktopInfo}
            >{actionLabel('check-updates')}</button>
          </div>
        </div>

        {#if desktopError}
          <div class="status-message status-message--error" role="status" aria-live="assertive">
            <Icon name="alert-circle" size={14} />
            <span>{i18n.t('settings.browser.actionFailed')}: <code>{desktopError}</code></span>
          </div>
        {:else if actionNotice}
          <div class="status-message status-message--success" role="status" aria-live="polite">
            <Icon name="check-circle" size={14} />
            <span>{actionNotice}</span>
          </div>
        {/if}
      </section>
    {:else}
      <section class="settings-section desktop-unavailable" aria-labelledby="browser-desktop-unavailable-title">
        <div class="section-icon" aria-hidden="true"><Icon name="globe" size={18} /></div>
        <div>
          <h3 id="browser-desktop-unavailable-title">{i18n.t('settings.browser.webUnavailableTitle')}</h3>
          <p>{i18n.t('settings.browser.webUnavailableDescription')}</p>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .browser-settings-tab {
    padding: 28px clamp(24px, 5vw, 64px) 42px;
    box-sizing: border-box;
  }

  .browser-settings-content {
    width: min(100%, 720px);
    margin: 0 auto;
  }

  .settings-section {
    padding: 0 2px 26px;
  }

  .settings-section + .settings-section {
    padding-top: 26px;
    border-top: 1px solid var(--ind-border-separator);
  }

  .section-heading,
  .capability-row,
  .component-row,
  .action-row,
  .desktop-unavailable {
    display: flex;
    align-items: center;
  }

  .section-heading {
    gap: 12px;
    margin-bottom: 12px;
  }

  .section-heading > div:not(.section-icon),
  .capability-row > div,
  .component-row > div,
  .action-row > div,
  .desktop-unavailable > div:last-child {
    min-width: 0;
    flex: 1;
  }

  h3 {
    margin: 0;
    color: var(--ind-foreground);
    font-size: 14px;
    font-weight: 650;
    letter-spacing: 0;
  }

  p {
    margin: 5px 0 0;
    color: var(--ind-foreground-secondary);
    font-size: 12px;
    line-height: 1.55;
  }

  .capability-list,
  .component-list,
  .action-list {
    border-top: 1px solid var(--ind-border-separator);
  }

  .capability-row,
  .component-row,
  .action-row {
    gap: 20px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .capability-row { min-height: 58px; }
  .component-row { min-height: 48px; }
  .action-row { min-height: 66px; }

  .capability-row strong,
  .capability-row span,
  .component-row strong,
  .component-row span,
  .action-row strong,
  .action-row span,
  .action-row small {
    display: block;
  }

  .capability-row strong,
  .component-row strong,
  .action-row strong {
    color: var(--ind-foreground);
    font-size: 12px;
    font-weight: 600;
  }

  .capability-row span,
  .component-row span,
  .action-row span,
  .action-row small {
    margin-top: 4px;
    color: var(--ind-foreground-secondary);
    font-size: 11px;
    line-height: 1.45;
  }

  .action-row small {
    color: var(--ind-foreground-muted);
  }

  .component-row code {
    max-width: 50%;
    overflow-wrap: anywhere;
    color: var(--ind-foreground);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 12px;
    text-align: right;
  }

  .section-icon {
    width: 34px;
    height: 34px;
    display: grid;
    flex: 0 0 auto;
    place-items: center;
    border: 1px solid var(--ind-border-control);
    border-radius: 8px;
    color: var(--ind-tab-accent);
    background: var(--ind-bg-control);
  }

  .icon-action,
  .action-row button {
    border: 1px solid var(--ind-border-control);
    border-radius: 6px;
    color: var(--ind-foreground);
    background: var(--ind-bg-control);
    cursor: pointer;
  }

  .icon-action {
    width: 28px;
    height: 28px;
    padding: 0;
    display: inline-grid;
    place-items: center;
  }

  .action-row button {
    min-width: 82px;
    min-height: 29px;
    padding: 0 10px;
    font-size: 12px;
  }

  .action-row button.danger:not(:disabled) {
    color: var(--danger, #c53f4f);
  }

  button:disabled {
    cursor: default;
    opacity: 0.42;
  }

  .icon-action--loading :global(svg) {
    animation: browser-components-refresh-spin 0.9s linear infinite;
  }

  @keyframes browser-components-refresh-spin {
    to { transform: rotate(360deg); }
  }

  .status-message {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
    font-size: 12px;
  }

  .status-message code {
    overflow-wrap: anywhere;
    font-size: 11px;
  }

  .status-message--error { color: var(--danger, #c53f4f); }
  .status-message--success { color: var(--success, #2f9e63); }

  .desktop-unavailable {
    gap: 12px;
  }

  @media (max-width: 560px) {
    .browser-settings-tab { padding-inline: 18px; }
    .section-heading { align-items: flex-start; }
    .component-row code { max-width: 42%; }
    .action-row { align-items: flex-start; padding: 12px 0; }
  }
</style>
