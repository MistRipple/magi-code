<script lang="ts">
  import { tick } from 'svelte';
  import {
    calculateTurnNavigationMagnet,
    calculateTurnNavigationScrollTarget,
    isTurnNavigationNeighbor,
    type TurnNavigationItem,
  } from '../lib/turn-navigation';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    items: TurnNavigationItem[];
    container: HTMLDivElement | null;
  }

  let { items, container }: Props = $props();

  let railRef: HTMLDivElement | null = $state(null);
  let markerListRef: HTMLDivElement | null = $state(null);
  let previewRef: HTMLDivElement | null = $state(null);
  let markerPositions = $state<Record<string, number>>({});
  let markerStrengths = $state<Record<string, number>>({});
  let activeTurnId = $state('');
  let focusedTurnId = $state('');
  let selectedTurnId = $state('');
  let menuOpen = $state(false);
  let positionFrame = 0;

  const activeItem = $derived(items.find((item) => item.turnId === activeTurnId) || items[items.length - 1]);
  const focusedItem = $derived(items.find((item) => item.turnId === focusedTurnId) || null);
  const selectedIndex = $derived(items.findIndex((item) => item.turnId === selectedTurnId));

  function statusLabel(status: TurnNavigationItem['status']): string {
    return i18n.t(`messageList.turnNavigation.status.${status}`);
  }

  function findMessageElement(item: TurnNavigationItem): HTMLElement | null {
    if (!container) return null;
    const messageIds = new Set(item.messageIds);
    const elements = container.querySelectorAll<HTMLElement>('[data-message-id]');
    let fallback: HTMLElement | null = null;
    for (const element of elements) {
      const messageId = element.dataset.messageId || '';
      if (!messageIds.has(messageId)) continue;
      if (messageId === item.anchorMessageId) return element;
      fallback ||= element;
    }
    return fallback;
  }

  function resolveMessageDocumentTop(item: TurnNavigationItem): number | null {
    if (!container) return null;
    const element = findMessageElement(item);
    if (!element) return null;
    const containerRect = container.getBoundingClientRect();
    const elementRect = element.getBoundingClientRect();
    return container.scrollTop + elementRect.top - containerRect.top;
  }

  function updateMarkerPositions(): void {
    if (!railRef || !markerListRef || items.length === 0) return;
    const railRect = railRef.getBoundingClientRect();
    const nextPositions: Record<string, number> = {};
    for (const element of markerListRef.querySelectorAll<HTMLElement>('[data-turn-id]')) {
      const turnId = element.dataset.turnId;
      if (!turnId) continue;
      const markerRect = element.getBoundingClientRect();
      nextPositions[turnId] = markerRect.top + markerRect.height / 2 - railRect.top;
    }
    markerPositions = nextPositions;
  }

  function scheduleMarkerPositionUpdate(): void {
    if (positionFrame) cancelAnimationFrame(positionFrame);
    positionFrame = requestAnimationFrame(() => {
      positionFrame = 0;
      updateMarkerPositions();
    });
  }

  function updateActiveTurn(): void {
    if (!container || items.length === 0) return;
    if (container.scrollTop + container.clientHeight >= container.scrollHeight - 8) {
      activeTurnId = items[items.length - 1]?.turnId || '';
      return;
    }
    const probe = container.scrollTop + Math.min(180, container.clientHeight * 0.32);
    let nextTurnId = items[0]?.turnId || '';
    for (const item of items) {
      const documentTop = resolveMessageDocumentTop(item);
      if (documentTop !== null && documentTop <= probe) {
        nextTurnId = item.turnId;
      }
    }
    activeTurnId = nextTurnId;
  }

  function resetMagnet(): void {
    focusedTurnId = '';
    markerStrengths = {};
  }

  function handleRailMouseMove(event: MouseEvent): void {
    if (!railRef || items.length === 0) return;
    const eventTarget = event.target;
    if (eventTarget instanceof Element && eventTarget.closest('.turn-navigation-preview')) return;
    const railRect = railRef.getBoundingClientRect();
    const markerPositionsInRail = items.map((item) => markerPositions[item.turnId] ?? 8);
    const magnet = calculateTurnNavigationMagnet(
      markerPositionsInRail,
      event.clientY - railRect.top,
      80,
    );
    focusedTurnId = items[magnet.focusIndex]?.turnId || '';
    markerStrengths = Object.fromEntries(
      items.map((item, index) => [item.turnId, magnet.strengths[index] || 0]),
    );
  }

  function isPointInside(rect: DOMRect, event: MouseEvent): boolean {
    return event.clientX >= rect.left
      && event.clientX <= rect.right
      && event.clientY >= rect.top
      && event.clientY <= rect.bottom;
  }

  function handleDocumentMouseMove(event: MouseEvent): void {
    if (!focusedTurnId || !railRef) return;
    const insideRail = isPointInside(railRef.getBoundingClientRect(), event);
    const insidePreview = previewRef
      && isPointInside(previewRef.getBoundingClientRect(), event);
    if (!insideRail && !insidePreview) resetMagnet();
  }

  function focusTurn(item: TurnNavigationItem): void {
    if (!container) return;
    const element = findMessageElement(item);
    if (!element) return;
    const containerRect = container.getBoundingClientRect();
    const elementRect = element.getBoundingClientRect();
    const targetTop = container.scrollTop + elementRect.top - containerRect.top - 24;
    container.scrollTo({ top: Math.max(0, targetTop), behavior: 'smooth' });
    activeTurnId = item.turnId;
    selectedTurnId = item.turnId;
    menuOpen = false;
    resetMagnet();
    const turnElements = container.querySelectorAll<HTMLElement>('[data-turn-id]');
    for (const turnElement of turnElements) {
      if (turnElement.dataset.turnId !== item.turnId) continue;
      try {
        turnElement.animate(
          [
            { boxShadow: '0 0 0 0 color-mix(in srgb, var(--primary) 0%, transparent)' },
            { boxShadow: '0 0 0 2px color-mix(in srgb, var(--primary) 45%, transparent)' },
            { boxShadow: '0 0 0 0 color-mix(in srgb, var(--primary) 0%, transparent)' },
          ],
          { duration: 900, easing: 'ease-out' },
        );
      } catch {}
    }
  }

  $effect(() => {
    const scrollContainer = container;
    const navigationItems = items;
    if (!scrollContainer || navigationItems.length === 0) return;
    const handleScroll = () => updateActiveTurn();
    scrollContainer.addEventListener('scroll', handleScroll, { passive: true });
    let disposed = false;
    let resizeObserver: ResizeObserver | null = null;
    let removeMarkerListScroll = () => {};
    void tick().then(() => {
      if (disposed) return;
      updateActiveTurn();
      updateMarkerPositions();
      if (typeof ResizeObserver !== 'undefined') {
        resizeObserver = new ResizeObserver(() => scheduleMarkerPositionUpdate());
        resizeObserver.observe(scrollContainer);
        if (markerListRef) resizeObserver.observe(markerListRef);
        for (const element of scrollContainer.querySelectorAll<HTMLElement>('[data-message-id]')) {
          resizeObserver.observe(element);
        }
      }
      const markerList = markerListRef;
      const handleMarkerListScroll = () => scheduleMarkerPositionUpdate();
      markerList?.addEventListener('scroll', handleMarkerListScroll, { passive: true });
      removeMarkerListScroll = () => markerList?.removeEventListener('scroll', handleMarkerListScroll);
      if (markerList) updateMarkerPositions();
    });
    return () => {
      disposed = true;
      scrollContainer.removeEventListener('scroll', handleScroll);
      resizeObserver?.disconnect();
      removeMarkerListScroll();
      if (positionFrame) cancelAnimationFrame(positionFrame);
    };
  });

  $effect(() => {
    document.addEventListener('mousemove', handleDocumentMouseMove);
    return () => document.removeEventListener('mousemove', handleDocumentMouseMove);
  });

  $effect(() => {
    if (!activeTurnId && items.length > 0) activeTurnId = items[items.length - 1]?.turnId || '';
    if (activeTurnId && !items.some((item) => item.turnId === activeTurnId)) {
      activeTurnId = items[items.length - 1]?.turnId || '';
    }
    if (selectedTurnId && !items.some((item) => item.turnId === selectedTurnId)) {
      selectedTurnId = '';
    }
  });

  function findMarkerElement(turnId: string): HTMLElement | null {
    if (!markerListRef) return null;
    for (const element of markerListRef.querySelectorAll<HTMLElement>('[data-turn-id]')) {
      if (element.dataset.turnId === turnId) return element;
    }
    return null;
  }

  function ensureActiveMarkerVisible(turnId: string): void {
    const markerList = markerListRef;
    const marker = findMarkerElement(turnId);
    if (!markerList || !marker) return;
    const targetTop = calculateTurnNavigationScrollTarget(
      marker.offsetTop,
      marker.offsetHeight,
      markerList.clientHeight,
      markerList.scrollTop,
      markerList.scrollHeight,
    );
    if (Math.abs(targetTop - markerList.scrollTop) < 1) return;
    markerList.scrollTo({ top: targetTop, behavior: 'smooth' });
  }

  $effect(() => {
    const currentTurnId = activeTurnId;
    if (!currentTurnId || !markerListRef) return;
    void tick().then(() => {
      if (currentTurnId !== activeTurnId) return;
      ensureActiveMarkerVisible(currentTurnId);
      updateMarkerPositions();
    });
  });
</script>

{#if items.length > 1}
  <div class="turn-navigation-layer">
    <div
      class="turn-navigation-rail"
      class:magnetic={focusedTurnId !== ''}
      class:has-selection={selectedTurnId !== ''}
      bind:this={railRef}
      data-testid="turn-navigation-rail"
      role="navigation"
      aria-label={i18n.t('messageList.turnNavigation.label')}
      onmousemove={handleRailMouseMove}
      onmouseleave={resetMagnet}
    >
      <div class="turn-navigation-marker-list" bind:this={markerListRef}>
        {#each items as item (item.turnId)}
          <button
            type="button"
            class="turn-navigation-marker"
            class:active={item.turnId === activeTurnId}
            class:running={item.status === 'running' || item.status === 'pending'}
            class:magnetic-focus={item.turnId === focusedTurnId}
            class:selected={item.turnId === selectedTurnId}
            class:selected-neighbor={isTurnNavigationNeighbor(items.indexOf(item), selectedIndex)}
            style:--turn-wave-strength={markerStrengths[item.turnId] ?? 0}
            data-turn-id={item.turnId}
            aria-label={i18n.t('messageList.turnNavigation.jump', { index: item.index, summary: item.summary })}
            onclick={() => focusTurn(item)}
          ></button>
        {/each}
      </div>
      {#if focusedItem}
        <div
          class="turn-navigation-preview"
          class:show={focusedTurnId !== ''}
          bind:this={previewRef}
          style={`top: ${markerPositions[focusedItem.turnId] ?? 8}px;`}
        >
          <div class="turn-navigation-preview-kicker">
            {i18n.t('messageList.turnNavigation.round', { index: focusedItem.index })} · {statusLabel(focusedItem.status)}
          </div>
          <div class="turn-navigation-preview-summary">{focusedItem.summary}</div>
        </div>
      {/if}
    </div>

    <div class="turn-navigation-capsule" class:open={menuOpen}>
      {#if menuOpen}
        <div class="turn-navigation-menu" role="menu">
          <div class="turn-navigation-menu-title">{i18n.t('messageList.turnNavigation.menuTitle')}</div>
          {#each items as item (item.turnId)}
            <button
              type="button"
              class="turn-navigation-menu-item"
              class:active={item.turnId === activeTurnId}
              role="menuitem"
              onclick={() => focusTurn(item)}
            >
              <span class="turn-navigation-menu-index">{String(item.index).padStart(2, '0')}</span>
              <span class="turn-navigation-menu-copy">
                <strong>{item.summary}</strong>
                <span>{statusLabel(item.status)}</span>
              </span>
            </button>
          {/each}
        </div>
      {/if}
      <button
        type="button"
        class="turn-navigation-capsule-button floating-overlay-control"
        data-testid="turn-navigation-capsule"
        aria-expanded={menuOpen}
        aria-label={i18n.t('messageList.turnNavigation.menuTitle')}
        onclick={() => { menuOpen = !menuOpen; }}
      >
        <span class="turn-navigation-capsule-count">{activeItem?.index || 0}/{items.length}</span>
        <span class="turn-navigation-status-dot" class:complete={activeItem?.status === 'completed'}></span>
      </button>
    </div>
  </div>
{/if}

<style>
  .turn-navigation-layer {
    position: absolute;
    inset: 0;
    z-index: 12;
    pointer-events: none;
  }

  .turn-navigation-rail {
    position: absolute;
    top: 50%;
    left: 2px;
    width: 14px;
    height: min(60vh, 560px);
    max-height: calc(100% - 32px);
    min-height: 120px;
    transform: translateY(-50%);
    opacity: 0.82;
    pointer-events: auto;
    transition: opacity 180ms ease, width 220ms cubic-bezier(.16, 1, .3, 1), transform 180ms ease;
  }

  .turn-navigation-rail:hover,
  .turn-navigation-rail:focus-within {
    width: 36px;
    opacity: 1;
  }

  .turn-navigation-marker-list {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: safe center;
    gap: 11px;
    overflow-y: auto;
    padding: 12px 0;
    scrollbar-width: none;
    overscroll-behavior: contain;
  }

  .turn-navigation-marker-list::-webkit-scrollbar {
    display: none;
  }

  .turn-navigation-marker {
    flex: 0 0 2px;
    position: relative;
    left: 0;
    width: 6px;
    height: 2px;
    padding: 0;
    border: 0;
    border-radius: 2px;
    background: var(--border);
    cursor: pointer;
    transition: width 180ms cubic-bezier(.16, 1, .3, 1), background 180ms ease, opacity 180ms ease, box-shadow 180ms ease;
  }

  .turn-navigation-marker.active {
    background: var(--foreground);
  }

  .turn-navigation-marker.running {
    background: var(--primary);
    animation: turnNavigationPulse 1.5s ease-in-out infinite;
  }

  .turn-navigation-marker:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 3px;
  }

  .turn-navigation-rail.has-selection .turn-navigation-marker.active:not(.selected):not(.selected-neighbor) {
    background: var(--border);
  }

  .turn-navigation-rail.magnetic .turn-navigation-marker {
    width: calc(6px + var(--turn-wave-strength, 0) * 14px);
    opacity: calc(0.38 + var(--turn-wave-strength, 0) * 0.62);
  }

  .turn-navigation-rail.magnetic .turn-navigation-marker.magnetic-focus {
    opacity: 1;
    background: var(--foreground);
    animation: none;
  }

  .turn-navigation-marker.selected-neighbor {
    opacity: 0.78;
    background: color-mix(in srgb, var(--foreground) 62%, var(--border));
    animation: none;
  }

  .turn-navigation-marker.selected {
    width: 20px;
    opacity: 1;
    background: var(--foreground);
    animation: none;
  }

  @keyframes turnNavigationPulse {
    50% { opacity: 0.35; }
  }

  .turn-navigation-preview {
    position: absolute;
    left: 40px;
    width: min(320px, calc(100vw - 100px));
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    background: var(--dropdown-bg, var(--surface-2));
    box-shadow: var(--shadow-lg);
    opacity: 0;
    pointer-events: none;
    transform: translateY(-50%) translateX(-7px) scale(0.98);
    transition: opacity 150ms ease, transform 180ms cubic-bezier(.16, 1, .3, 1);
  }

  .turn-navigation-preview.show {
    opacity: 1;
    pointer-events: auto;
    transform: translateY(-50%) translateX(0) scale(1);
  }

  .turn-navigation-preview-kicker {
    margin-bottom: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .turn-navigation-preview-summary {
    color: var(--foreground);
    font-size: var(--text-sm);
    line-height: var(--leading-relaxed);
    display: -webkit-box;
    line-clamp: 3;
    -webkit-line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .turn-navigation-capsule {
    position: absolute;
    right: 20px;
    bottom: 64px;
    pointer-events: auto;
  }

  .turn-navigation-capsule-button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 5px;
    width: 54px;
    height: 36px;
    padding: 0 9px;
    border-radius: var(--radius-full);
    color: var(--foreground);
  }

  .turn-navigation-capsule-count {
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .turn-navigation-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--primary);
    animation: turnNavigationPulse 1.5s ease-in-out infinite;
  }

  .turn-navigation-status-dot.complete {
    background: var(--success);
    animation: none;
  }

  .turn-navigation-menu {
    position: absolute;
    right: 0;
    bottom: 44px;
    width: min(340px, calc(100vw - 30px));
    max-height: 410px;
    overflow: auto;
    padding: var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    background: var(--dropdown-bg, var(--surface-2));
    box-shadow: var(--shadow-lg);
  }

  .turn-navigation-menu-title {
    padding: var(--space-2) var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .turn-navigation-menu-item {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr);
    gap: var(--space-2);
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground);
    text-align: left;
    cursor: pointer;
  }

  .turn-navigation-menu-item:hover,
  .turn-navigation-menu-item.active {
    background: var(--surface-hover);
  }

  .turn-navigation-menu-index {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .turn-navigation-menu-copy strong,
  .turn-navigation-menu-copy span {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .turn-navigation-menu-copy strong {
    font-size: var(--text-sm);
  }

  .turn-navigation-menu-copy span {
    margin-top: 2px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  @container message-list (min-width: 640px) {
    .turn-navigation-capsule {
      display: none;
    }
  }

  @container message-list (max-width: 639px) {
    .turn-navigation-rail {
      display: none;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .turn-navigation-marker,
    .turn-navigation-rail,
    .turn-navigation-preview {
      transition: none;
    }

    .turn-navigation-marker.running,
    .turn-navigation-status-dot {
      animation: none;
    }
  }
</style>
