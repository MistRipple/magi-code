<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { normalizeExternalWebUrl, openExternalWebUrl } from '../../lib/external-link';
  import {
    browserScreenshotUrl,
    getBrowserSession,
    navigateBrowserTab,
    type BrowserAnnotationSnapshot,
    type BrowserDeviceType,
    type BrowserSessionSnapshot,
    type BrowserTabSnapshot,
    type BrowserViewportMode,
    BROWSER_AUTHORITY_CHANGED_EVENT,
  } from '../../web/agent-api';

  interface Props {
    browserSessionId: string;
    tabId: string;
    onTitleChange?: (label: string) => void;
  }

  interface DesktopBrowserEvent {
    type?: string;
    binding?: { tab_id?: string };
    page?: { url?: string; title?: string };
    loading?: boolean;
  }

  const VIEWPORT_DEVICE_MODES = [
    { id: 'wide', width: 1280, height: 800, deviceType: 'desktop' },
    { id: 'narrow', width: 390, height: 844, deviceType: 'mobile' },
  ] as const;
  const CUSTOM_VIEWPORT_DEBOUNCE_MILLIS = 180;

  let { browserSessionId, tabId, onTitleChange }: Props = $props();
  const desktopRuntime = typeof window !== 'undefined' && window.magiDesktop?.runtime === 'electron';
  let snapshot = $state<BrowserSessionSnapshot | null>(null);
  let address = $state('');
  let lastObservedUrl = '';
  let loading = $state(true);
  let browserLoading = $state(false);
  let sessionError = $state('');
  let actionError = $state('');
  let busy = $state(false);
  let viewportMenuElement = $state<HTMLDivElement | undefined>();
  let annotationMenuElement = $state<HTMLDivElement | undefined>();
  let viewportMenuOpen = $state(false);
  let annotationMenuOpen = $state(false);
  let localViewportMode = $state<BrowserViewportMode>('auto');
  let localViewport = $state({ width: 1280, height: 800, deviceType: 'desktop' as BrowserDeviceType });
  let customViewportWidth = $state(390);
  let customViewportHeight = $state(844);
  let customViewportTimer: number | null = null;
  let pendingViewport: { width: number; height: number; deviceType: BrowserDeviceType } | null = null;
  let refreshGeneration = 0;

  const activeTab = $derived.by<BrowserTabSnapshot | null>(() => (
    snapshot?.tabs.find((tab) => tab.tabId === tabId && tab.lifecycle !== 'closed') ?? null
  ));
  const savedAnnotations = $derived((activeTab?.annotations ?? []).filter((annotation) => annotation.status !== 'deleted'));
  const externalUrl = $derived(normalizeExternalWebUrl(activeTab?.url || address));
  const browserReady = $derived(desktopRuntime && activeTab?.lifecycle === 'ready');
  const error = $derived(actionError || sessionError);
  const connectionState = $derived.by<'ready' | 'connecting' | 'error'>(() => {
    if (error || !desktopRuntime) return 'error';
    if (loading || !browserReady) return 'connecting';
    return 'ready';
  });
  const connectionStatusText = $derived.by(() => {
    if (error) return error;
    if (!desktopRuntime) {
      return i18n.t('browser.error.internalUnavailable');
    }
    if (connectionState === 'ready') {
      return i18n.t(browserLoading ? 'browser.status.connecting' : 'browser.status.connected');
    }
    return i18n.t('browser.status.connecting');
  });

  $effect(() => {
    const tab = activeTab;
    if (!tab) return;
    onTitleChange?.(tab.title.trim() || (tab.url === 'about:blank' ? i18n.t('browser.tab.new') : tab.url));
  });

  function errorMessage(value: unknown): string {
    return value instanceof Error ? value.message : String(value);
  }

  async function refreshSession(initialLoad = false): Promise<void> {
    const expectedSessionId = browserSessionId;
    const expectedTabId = tabId;
    const generation = ++refreshGeneration;
    if (initialLoad) loading = true;
    try {
      const next = await getBrowserSession(expectedSessionId);
      if (generation !== refreshGeneration || expectedSessionId !== browserSessionId || expectedTabId !== tabId) return;
      snapshot = next;
      const nextTab = next.tabs.find((candidate) => candidate.tabId === expectedTabId && candidate.lifecycle !== 'closed');
      const nextUrl = nextTab?.url ?? '';
      if (initialLoad || address === lastObservedUrl) address = nextUrl;
      lastObservedUrl = nextUrl;
      sessionError = '';
    } catch (cause) {
      if (generation === refreshGeneration) sessionError = errorMessage(cause);
    } finally {
      if (generation === refreshGeneration) loading = false;
    }
  }

  function fixedPresetSelected(mode: (typeof VIEWPORT_DEVICE_MODES)[number]): boolean {
    return localViewportMode === 'fixed'
      && localViewport.width === mode.width
      && localViewport.height === mode.height;
  }

  async function updateLogicalViewport(
    mode: BrowserViewportMode,
    viewport?: { width: number; height: number; deviceType: BrowserDeviceType },
  ): Promise<void> {
    const tab = activeTab;
    const desktop = window.magiDesktop;
    if (!tab || !desktop) return;
    const next = await desktop.setBrowserViewport({
      tabId: tab.tabId,
      viewport: mode === 'auto'
        ? { mode: 'auto' }
        : {
            mode: 'fixed',
            width: viewport!.width,
            height: viewport!.height,
            deviceScaleFactorMillis: 1_000,
            deviceType: viewport!.deviceType,
          },
    });
    applyDesktopViewport(next);
    actionError = '';
  }

  function applyDesktopViewport(next: MagiDesktopWindowSnapshot): void {
    if (next.layout.activeTabId !== tabId || !next.activeBrowserViewport) return;
    const viewport = next.activeBrowserViewport;
    localViewportMode = viewport.mode;
    if (viewport.mode === 'auto') return;
    localViewport = {
      width: viewport.width,
      height: viewport.height,
      deviceType: viewport.device_type,
    };
    customViewportWidth = viewport.width;
    customViewportHeight = viewport.height;
  }

  function useAutomaticViewport(): void {
    void run(async () => updateLogicalViewport('auto'));
  }

  function useFixedViewport(width: number, height: number): void {
    const viewport = { width, height, deviceType: width <= 600 ? 'mobile' as const : 'desktop' as const };
    customViewportWidth = width;
    customViewportHeight = height;
    void run(async () => updateLogicalViewport('fixed', viewport));
  }

  function scheduleCustomViewportUpdate(): void {
    const width = Math.round(Number(customViewportWidth));
    const height = Math.round(Number(customViewportHeight));
    if (width < 320 || width > 7680 || height < 240 || height > 4320) return;
    pendingViewport = { width, height, deviceType: width <= 600 ? 'mobile' : 'desktop' };
    if (customViewportTimer !== null) window.clearTimeout(customViewportTimer);
    customViewportTimer = window.setTimeout(() => {
      customViewportTimer = null;
      const next = pendingViewport;
      pendingViewport = null;
      if (next) void run(async () => updateLogicalViewport('fixed', next));
    }, CUSTOM_VIEWPORT_DEBOUNCE_MILLIS);
  }

  function toggleViewportMenu(): void {
    viewportMenuOpen = !viewportMenuOpen;
    annotationMenuOpen = false;
    if (!desktopRuntime || !window.magiDesktop) return;
    if (!viewportMenuOpen) {
      void window.magiDesktop.closeOverlay();
      return;
    }
    void window.magiDesktop.openOverlay({
      kind: 'menu',
      ownerId: tabId,
      placement: 'browser-viewport',
      title: i18n.t('browser.viewport.control'),
      items: [
        {
          id: 'auto',
          label: i18n.t('browser.viewport.auto'),
          icon: null,
          selected: localViewportMode === 'auto',
          disabled: false,
        },
        {
          id: 'wide',
          label: i18n.t('browser.viewport.mode.wide'),
          icon: 'monitor',
          selected: fixedPresetSelected(VIEWPORT_DEVICE_MODES[0]),
          disabled: false,
        },
        {
          id: 'narrow',
          label: i18n.t('browser.viewport.mode.narrow'),
          icon: null,
          selected: fixedPresetSelected(VIEWPORT_DEVICE_MODES[1]),
          disabled: false,
        },
      ],
      fields: [
        {
          id: 'width',
          label: i18n.t('browser.viewport.width'),
          type: 'number',
          value: String(customViewportWidth),
          min: 320,
          max: 7680,
        },
        {
          id: 'height',
          label: i18n.t('browser.viewport.height'),
          type: 'number',
          value: String(customViewportHeight),
          min: 240,
          max: 4320,
        },
      ],
    }).catch((cause) => {
      viewportMenuOpen = false;
      actionError = errorMessage(cause);
    });
  }

  function toggleAnnotationMenu(): void {
    annotationMenuOpen = !annotationMenuOpen;
    viewportMenuOpen = false;
    if (!desktopRuntime || !window.magiDesktop) return;
    if (!annotationMenuOpen) {
      void window.magiDesktop.closeOverlay();
      return;
    }
    void window.magiDesktop.openOverlay({
      kind: 'menu',
      ownerId: tabId,
      placement: 'browser-annotations',
      title: i18n.t('browser.annotation.history'),
      items: savedAnnotations.map((annotation) => ({
        id: `annotation:${annotation.annotationId}`,
        label: `${annotation.sequence}. ${annotation.comment}`,
        icon: 'target',
        selected: false,
        disabled: false,
      })),
      fields: [],
    }).catch((cause) => {
      annotationMenuOpen = false;
      actionError = errorMessage(cause);
    });
  }

  async function run(action: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    actionError = '';
    try {
      await action();
    } catch (cause) {
      actionError = errorMessage(cause);
    } finally {
      busy = false;
    }
  }

  function navigate(action: 'url' | 'back' | 'forward' | 'reload'): void {
    const tab = activeTab;
    if (!tab || !browserReady) return;
    void run(async () => {
      const updated = await navigateBrowserTab(tab.tabId, action, action === 'url' ? address : undefined);
      address = updated.url;
      lastObservedUrl = updated.url;
      await refreshSession();
    });
  }

  function openCurrentPageExternally(): void {
    if (!externalUrl) return;
    void run(async () => {
      if (desktopRuntime && window.magiDesktop) {
        await window.magiDesktop.openExternal(externalUrl);
        return;
      }
      await openExternalWebUrl(externalUrl);
    });
  }

  function blobDataUrl(blob: Blob): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => typeof reader.result === 'string'
        ? resolve(reader.result)
        : reject(new Error(i18n.t('browser.error.screenshot')));
      reader.onerror = () => reject(reader.error ?? new Error(i18n.t('browser.error.screenshot')));
      reader.readAsDataURL(blob);
    });
  }

  function captureScreenshotForMessage(): void {
    const tab = activeTab;
    if (!tab || !browserReady) return;
    void run(async () => {
      const response = await fetch(browserScreenshotUrl(tab.tabId), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ fullPage: false }),
      });
      if (!response.ok) throw new Error(i18n.t('browser.error.screenshot').replace('{status}', String(response.status)));
      const blob = await response.blob();
      window.dispatchEvent(new CustomEvent('magi:browserScreenshotCaptured', {
        detail: {
          name: `magi-browser-${Date.now()}.png`,
          dataUrl: await blobDataUrl(blob),
          size: blob.size,
          type: blob.type || 'image/png',
        },
      }));
    });
  }

  function selectSavedAnnotation(annotation: BrowserAnnotationSnapshot): void {
    annotationMenuOpen = false;
    window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: annotation }));
  }

  function handleDesktopOverlayAction(value: unknown): void {
    if (!value || typeof value !== 'object') return;
    const action = value as {
      kind?: string;
      ownerId?: string;
      interaction?: string;
      id?: string;
      value?: string | null;
    };
    if (action.kind !== 'menu' || action.ownerId !== tabId || typeof action.id !== 'string') return;
    if (action.interaction === 'input') {
      const numericValue = Number(action.value ?? '');
      if (!Number.isFinite(numericValue)) return;
      if (action.id === 'width') customViewportWidth = numericValue;
      if (action.id === 'height') customViewportHeight = numericValue;
      scheduleCustomViewportUpdate();
      return;
    }
    if (action.interaction !== 'select') return;
    if (action.id === 'auto') {
      viewportMenuOpen = false;
      useAutomaticViewport();
      return;
    }
    const preset = VIEWPORT_DEVICE_MODES.find((mode) => mode.id === action.id);
    if (preset) {
      viewportMenuOpen = false;
      useFixedViewport(preset.width, preset.height);
      return;
    }
    if (action.id.startsWith('annotation:')) {
      const annotationId = action.id.slice('annotation:'.length);
      const annotation = savedAnnotations.find((candidate) => candidate.annotationId === annotationId);
      if (annotation) selectSavedAnnotation(annotation);
    }
  }

  function handleDesktopBrowserEvent(value: unknown): void {
    if (!value || typeof value !== 'object') return;
    const event = value as DesktopBrowserEvent;
    if (event.binding?.tab_id !== tabId) return;
    if (event.type === 'loading_changed') {
      browserLoading = event.loading === true;
      return;
    }
    if (event.type !== 'page_updated' || !event.page) return;
    if (event.page.url?.trim()) {
      address = event.page.url;
      lastObservedUrl = event.page.url;
    }
    if (event.page.title?.trim()) onTitleChange?.(event.page.title);
  }

  $effect(() => {
    const expectedSessionId = browserSessionId.trim();
    const expectedTabId = tabId.trim();
    untrack(() => {
      snapshot = null;
      address = '';
      lastObservedUrl = '';
      sessionError = '';
      actionError = '';
      browserLoading = false;
      loading = Boolean(expectedSessionId && expectedTabId);
      if (!expectedSessionId || !expectedTabId) return;
      void refreshSession(true);
    });
  });

  onMount(() => {
    const desktop = window.magiDesktop;
    if (desktop) void desktop.getSnapshot().then(applyDesktopViewport).catch(() => undefined);
    const pointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (viewportMenuElement && target instanceof Node && !viewportMenuElement.contains(target)) viewportMenuOpen = false;
      if (annotationMenuElement && target instanceof Node && !annotationMenuElement.contains(target)) annotationMenuOpen = false;
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      viewportMenuOpen = false;
      annotationMenuOpen = false;
    };
    const browserAuthorityChanged = (event: Event) => {
      const detail = (event as CustomEvent<{
        eventType?: string;
        payload?: Record<string, unknown>;
      }>).detail;
      const changedTabId = detail?.payload?.tab_id;
      if (typeof changedTabId === 'string' && changedTabId !== tabId) return;
      void refreshSession();
    };
    const unsubscribeDesktopSnapshot = desktop?.onSnapshot(applyDesktopViewport);
    const unsubscribeBrowserEvent = window.magiDesktop?.onBrowserEvent(handleDesktopBrowserEvent);
    const unsubscribeOverlayAction = desktop?.onOverlayAction(handleDesktopOverlayAction);
    const unsubscribeOverlayClosed = desktop?.onOverlayClosed(() => {
      viewportMenuOpen = false;
      annotationMenuOpen = false;
    });
    window.addEventListener('pointerdown', pointerDown);
    window.addEventListener('keydown', keyboard);
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
    return () => {
      unsubscribeBrowserEvent?.();
      unsubscribeDesktopSnapshot?.();
      unsubscribeOverlayAction?.();
      unsubscribeOverlayClosed?.();
      window.removeEventListener('pointerdown', pointerDown);
      window.removeEventListener('keydown', keyboard);
      window.removeEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
      if (customViewportTimer !== null) window.clearTimeout(customViewportTimer);
      customViewportTimer = null;
      pendingViewport = null;
    };
  });
</script>

<section class="browser-pane" aria-label={i18n.t('browser.pane.label')}>
  <div class="browser-toolbar">
    <button type="button" class="icon-button flip" onclick={() => navigate('back')} disabled={!browserReady || busy} title={i18n.t('browser.navigation.back')} aria-label={i18n.t('browser.navigation.back')}><Icon name="chevron-right" size={13} /></button>
    <button type="button" class="icon-button" onclick={() => navigate('forward')} disabled={!browserReady || busy} title={i18n.t('browser.navigation.forward')} aria-label={i18n.t('browser.navigation.forward')}><Icon name="chevron-right" size={13} /></button>
    <button type="button" class="icon-button" onclick={() => navigate('reload')} disabled={!browserReady || busy} title={i18n.t('browser.navigation.reload')} aria-label={i18n.t('browser.navigation.reload')}><Icon name="refresh" size={13} /></button>
    <form class="address-form" onsubmit={(event) => { event.preventDefault(); navigate('url'); }}>
      <input bind:value={address} aria-label={i18n.t('browser.navigation.address')} spellcheck="false" disabled={!browserReady || busy} onkeydown={(event) => { if (event.key !== 'Enter' || event.isComposing) return; event.preventDefault(); navigate('url'); }} />
      <button type="submit" class="address-submit" disabled={!browserReady || busy} title={i18n.t('browser.navigation.go')} aria-label={i18n.t('browser.navigation.go')}><Icon name="chevron-right" size={12} /></button>
    </form>
    <div class="menu-wrap" bind:this={viewportMenuElement}>
      <button type="button" class="icon-button" class:active={localViewportMode === 'fixed'} onclick={toggleViewportMenu} disabled={!browserReady || busy} title={i18n.t('browser.viewport.control')} aria-label={i18n.t('browser.viewport.control')}><Icon name="monitor" size={13} /></button>
      {#if !desktopRuntime && viewportMenuOpen && activeTab}
        <div class="viewport-menu" role="menu" aria-label={i18n.t('browser.viewport.control')}>
          <button type="button" class:selected={localViewportMode === 'auto'} onclick={useAutomaticViewport} role="menuitem"><span>{i18n.t('browser.viewport.auto')}</span></button>
          <div class="menu-divider"></div>
          <div class="viewport-device-modes">
            {#each VIEWPORT_DEVICE_MODES as mode (mode.id)}
              <button type="button" class:selected={fixedPresetSelected(mode)} onclick={() => useFixedViewport(mode.width, mode.height)}>{i18n.t(`browser.viewport.mode.${mode.id}`)}</button>
            {/each}
          </div>
          <div class="menu-divider"></div>
          <div class="viewport-custom">
            <label><span>{i18n.t('browser.viewport.width')}</span><input type="number" min="320" max="7680" bind:value={customViewportWidth} oninput={scheduleCustomViewportUpdate} /></label>
            <label><span>{i18n.t('browser.viewport.height')}</span><input type="number" min="240" max="4320" bind:value={customViewportHeight} oninput={scheduleCustomViewportUpdate} /></label>
          </div>
        </div>
      {/if}
    </div>
    <button type="button" class="icon-button" onclick={openCurrentPageExternally} disabled={!externalUrl || busy} title={i18n.t('browser.action.openExternal')} aria-label={i18n.t('browser.action.openExternal')}><Icon name="external-link" size={13} /></button>
    <button type="button" class="icon-button" onclick={captureScreenshotForMessage} disabled={!browserReady || busy} title={i18n.t('browser.action.screenshot')} aria-label={i18n.t('browser.action.screenshot')}><Icon name="file-plus" size={13} /></button>
    <div class="menu-wrap" bind:this={annotationMenuElement}>
      <button type="button" class="icon-button annotation-history-button" class:active={annotationMenuOpen} onclick={toggleAnnotationMenu} disabled={savedAnnotations.length === 0} title={i18n.t('browser.annotation.history')} aria-label={i18n.t('browser.annotation.history')}><Icon name="target" size={13} />{#if savedAnnotations.length}<span class="annotation-count">{savedAnnotations.length}</span>{/if}</button>
      {#if !desktopRuntime && annotationMenuOpen}
        <div class="annotation-menu" role="menu">
          {#each savedAnnotations as annotation (annotation.annotationId)}
            <button type="button" onclick={() => selectSavedAnnotation(annotation)} title={annotation.comment}><span class="annotation-menu-number">{annotation.sequence}</span><span>{annotation.comment}</span></button>
          {/each}
        </div>
      {/if}
    </div>
    <span class="status-light" class:ready={connectionState === 'ready' && !browserLoading} class:loading={connectionState === 'ready' && browserLoading} class:error={connectionState === 'error'} title={connectionStatusText} role="status"></span>
  </div>

  <div class="browser-surface-slot" aria-label={i18n.t('browser.viewport.label')}>
    {#if !desktopRuntime}
      <div class="browser-placeholder error">{i18n.t('browser.error.internalUnavailable')}</div>
    {:else if connectionState !== 'ready'}
      <div class="browser-placeholder" class:error={connectionState === 'error'}>{connectionStatusText}</div>
    {/if}
  </div>
</section>

<style>
  .browser-pane { position: relative; display: flex; flex-direction: column; min-height: 0; height: 100%; background: var(--background); }
  .browser-toolbar { z-index: 2; box-sizing: border-box; display: flex; align-items: center; height: 36px; min-height: 36px; gap: 3px; padding: 4px 6px; border-bottom: 1px solid var(--border); flex-shrink: 0; }
  .icon-button { width: 27px; height: 27px; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; padding: 0; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; }
  .icon-button:hover:not(:disabled) { background: var(--surface-2); color: var(--foreground); }
  .icon-button.active { color: var(--primary); background: var(--surface-2); }
  .icon-button:disabled { opacity: .45; cursor: default; }
  .flip { transform: scaleX(-1); }
  .address-form { display: flex; flex: 1; min-width: 80px; }
  .address-form input { box-sizing: border-box; width: 100%; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-right: 0; border-radius: var(--radius-sm) 0 0 var(--radius-sm); background: var(--surface-1); color: var(--foreground); font: inherit; }
  .address-submit { display: grid; place-items: center; width: 27px; height: 27px; flex: 0 0 27px; padding: 0; border: 1px solid var(--border); border-radius: 0 var(--radius-sm) var(--radius-sm) 0; background: var(--surface-1); color: var(--foreground-muted); cursor: pointer; }
  .menu-wrap { position: relative; display: flex; flex: 0 0 auto; }
  .viewport-menu, .annotation-menu { position: absolute; z-index: 10; top: calc(100% + 5px); right: 0; width: 242px; max-height: 240px; overflow: auto; padding: 5px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
  .viewport-menu > button, .annotation-menu > button { box-sizing: border-box; display: flex; align-items: center; gap: 7px; width: 100%; min-height: 30px; padding: 0 8px; border: 0; border-radius: 4px; background: transparent; color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; text-align: left; }
  .viewport-menu > button:hover, .viewport-menu > button.selected, .annotation-menu > button:hover { background: var(--surface-hover); }
  .viewport-menu > button.selected { color: var(--primary); }
  .menu-divider { height: 1px; margin: 4px 3px; background: var(--border); }
  .viewport-device-modes { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 3px; padding: 3px; }
  .viewport-device-modes button { height: 28px; min-width: 0; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground-muted); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .viewport-device-modes button.selected { border-color: var(--primary); color: var(--primary); }
  .viewport-custom { display: grid; grid-template-columns: 1fr 1fr; gap: 5px; padding: 3px; }
  .viewport-custom label { display: grid; gap: 3px; min-width: 0; color: var(--foreground-muted); font-size: 10px; }
  .viewport-custom input { box-sizing: border-box; min-width: 0; width: 100%; height: 27px; padding: 0 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; }
  .annotation-history-button { position: relative; }
  .annotation-count { position: absolute; top: 1px; right: 1px; min-width: 12px; height: 12px; padding: 0 2px; border-radius: 6px; background: var(--info); color: white; font-size: 8px; font-weight: 700; line-height: 12px; text-align: center; }
  .annotation-menu { width: min(300px, calc(100vw - 24px)); }
  .annotation-menu-number { display: grid; place-items: center; flex: 0 0 19px; width: 19px; height: 19px; border-radius: 50%; background: var(--info); color: white; font-size: 10px; font-weight: 700; }
  .status-light { width: 7px; height: 7px; flex: 0 0 auto; border-radius: 50%; background: var(--foreground-subtle); }
  .status-light.ready { background: var(--success); }
  .status-light.loading { background: var(--warning); }
  .status-light.error { background: var(--error); }
  .browser-surface-slot { position: relative; display: flex; flex: 1; min-height: 0; overflow: hidden; background: var(--surface-1); }
  .browser-placeholder { display: grid; flex: 1; place-items: center; padding: 20px; color: var(--foreground-muted); font-size: var(--text-sm); text-align: center; }
  .browser-placeholder.error { color: var(--error); }
</style>
