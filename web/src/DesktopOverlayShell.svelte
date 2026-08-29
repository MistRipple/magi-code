<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from './components/Icon.svelte';
  import type { IconName } from './lib/icons';
  import { i18n } from './stores/i18n.svelte';

  let overlayState: MagiDesktopOverlayState | null = $state(null);
  let fieldValues: Record<string, string> = $state({});
  let dragStart = $state<{ x: number; y: number } | null>(null);
  let dragCurrent = $state<{ x: number; y: number } | null>(null);
  let actionError = $state('');
  let closePending = $state(false);
  let overlayRevision = 0;
  let actionRevision = 0;
  let closingIdentity = $state<{ overlayId: string; ownerId: string } | null>(null);
  let closeAttemptRevision = 0;
  let activeCloseAttempt = 0;
  const desktop = window.magiDesktop;

  function submit(interaction: 'select' | 'input', id: string, value: string | null = null): void {
    const current = overlayState;
    if (!desktop || !current || closePending) return;
    const revision = overlayRevision;
    const requestRevision = ++actionRevision;
    const identity = { overlayId: current.overlayId, ownerId: current.ownerId };
    void desktop.submitOverlayAction({
      ...identity,
      kind: current.kind,
      interaction,
      id,
      value,
    }).then(() => {
      if (
        !overlayState
        || overlayState.overlayId !== identity.overlayId
        || overlayState.ownerId !== identity.ownerId
        || overlayRevision !== revision
        || actionRevision !== requestRevision
        || closePending
      ) return;
      actionError = '';
    }).catch((error) => {
      if (
        !overlayState
        || overlayState.overlayId !== identity.overlayId
        || overlayState.ownerId !== identity.ownerId
        || overlayRevision !== revision
        || actionRevision !== requestRevision
        || closePending
      ) return;
      actionError = error instanceof Error ? error.message : String(error);
    });
  }

  function isCurrentIdentity(identity: { overlayId: string; ownerId: string }): boolean {
    return overlayState?.overlayId === identity.overlayId && overlayState.ownerId === identity.ownerId;
  }

  function closeFailure(
    identity: { overlayId: string; ownerId: string },
    attempt: number,
    error: unknown = null,
  ): void {
    if (
      activeCloseAttempt !== attempt
      || !closingIdentity
      || closingIdentity.overlayId !== identity.overlayId
      || closingIdentity.ownerId !== identity.ownerId
      || !isCurrentIdentity(identity)
    ) return;
    const detail = error instanceof Error ? error.message : typeof error === 'string' ? error : '';
    const message = i18n.t('bridge.toast.actionFailed', { action: i18n.t('common.close') });
    actionError = detail ? `${message}: ${detail}` : message;
    closePending = false;
    closingIdentity = null;
    activeCloseAttempt = 0;
  }

  function confirmClosed(event: MagiDesktopOverlayClosedEvent): void {
    if (!isCurrentIdentity(event)) return;
    overlayState = null;
    fieldValues = {};
    dragStart = null;
    dragCurrent = null;
    actionError = '';
    closePending = false;
    closingIdentity = null;
    activeCloseAttempt = 0;
  }

  function close(): void {
    const current = overlayState;
    if (!desktop || !current || closePending) return;
    const identity = { overlayId: current.overlayId, ownerId: current.ownerId };
    const attempt = ++closeAttemptRevision;
    activeCloseAttempt = attempt;
    actionRevision += 1;
    closingIdentity = identity;
    closePending = true;
    actionError = '';
    void desktop.closeOverlay(identity).then((event) => {
      if (
        activeCloseAttempt !== attempt
        || !closingIdentity
        || !isCurrentIdentity(closingIdentity)
      ) return;
      if (event === null) {
        closeFailure(identity, attempt);
        return;
      }
      // 关闭事件才是权威确认。当前 IPC 处理器可能无返回值，因此必须等
      // onOverlayClosed 到达后再保留或清理 UI，不能把空返回当成成功并乐观
      // 地提前清除状态。
      if (event) confirmClosed(event);
    }).catch((error) => {
      closeFailure(identity, attempt, error);
    });
  }

  onMount(() => {
    if (!desktop) return;
    const stop = desktop.onOverlayState((next) => {
      const nextIdentity = { overlayId: next.overlayId, ownerId: next.ownerId };
      if (
        closePending
        && closingIdentity
        && nextIdentity.overlayId === closingIdentity.overlayId
        && nextIdentity.ownerId === closingIdentity.ownerId
      ) return;
      overlayRevision += 1;
      overlayState = next;
      fieldValues = Object.fromEntries(next.fields.map((field) => [field.id, field.value]));
      dragStart = null;
      dragCurrent = null;
      actionError = '';
      closePending = false;
      closingIdentity = null;
      activeCloseAttempt = 0;
      // 只有标记备注输入框需要键盘焦点。菜单和标记选择层不能在收到布局
      // 或状态更新时调用 window/body.focus，否则会把对话输入焦点抢走。
      void tick().then(() => {
        if (
          !isCurrentIdentity(nextIdentity)
          || overlayState?.kind !== 'annotation'
          || overlayState.phase !== 'comment'
        ) return;
        document.querySelector<HTMLTextAreaElement>('.annotation-editor textarea')?.focus({ preventScroll: true });
      });
    });
    const stopClosed = desktop.onOverlayClosed((event) => {
      confirmClosed(event);
    });
    void desktop.readyOverlay().catch((error) => {
      console.warn('[DesktopOverlayShell] 覆盖层就绪握手失败:', error);
    });
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || !overlayState) return;
      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation();
      close();
    };
    window.addEventListener('keydown', escape, { capture: true });
    return () => {
      stop();
      stopClosed();
      window.removeEventListener('keydown', escape, { capture: true });
    };
  });

  function point(event: MouseEvent | PointerEvent): { x: number; y: number } | null {
    const target = event.currentTarget;
    if (!(target instanceof HTMLElement)) return null;
    const rect = target.getBoundingClientRect();
    if (!rect.width || !rect.height) return null;
    return {
      x: Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width)),
      y: Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height)),
    };
  }

  function handleAnnotationPointerDown(event: PointerEvent): void {
    if (closePending || overlayState?.kind !== 'annotation' || overlayState.phase !== 'select') return;
    event.preventDefault();
    const next = point(event);
    if (!next) return;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    dragStart = next;
    dragCurrent = next;
  }

  function handleAnnotationPointerMove(event: PointerEvent): void {
    if (closePending || !dragStart || overlayState?.kind !== 'annotation' || overlayState.phase !== 'select') return;
    const next = point(event);
    if (next) dragCurrent = next;
  }

  function handleAnnotationPointerUp(event: PointerEvent): void {
    if (closePending || !dragStart || overlayState?.kind !== 'annotation' || overlayState.phase !== 'select') return;
    event.preventDefault();
    const start = dragStart;
    const end = point(event) ?? dragCurrent ?? start;
    dragStart = null;
    dragCurrent = null;
    const width = Math.abs(end.x - start.x);
    const height = Math.abs(end.y - start.y);
    const selection = width < 0.012 && height < 0.012
      ? { kind: 'element', x: end.x, y: end.y }
      : {
          kind: 'region',
          rect: {
            x: Math.min(start.x, end.x),
            y: Math.min(start.y, end.y),
            width,
            height,
          },
        };
    submit('select', 'selection', JSON.stringify(selection));
  }

  // 某些桌面辅助输入路径只注入传统 mouse 事件，不会产生完整的
  // PointerEvent 序列。标记层同时接收两种事件，但共享同一个拖拽状态，
  // 避免同一次鼠标操作被提交两次。
  function handleAnnotationMouseDown(event: MouseEvent): void {
    if (closePending || dragStart) return;
    event.preventDefault();
    const next = point(event);
    if (!next) return;
    dragStart = next;
    dragCurrent = next;
  }

  function handleAnnotationMouseMove(event: MouseEvent): void {
    if (closePending || !dragStart) return;
    const next = point(event);
    if (next) dragCurrent = next;
  }

  function handleAnnotationMouseUp(event: MouseEvent): void {
    if (closePending || !dragStart) return;
    event.preventDefault();
    const start = dragStart;
    const end = point(event) ?? dragCurrent ?? start;
    dragStart = null;
    dragCurrent = null;
    const width = Math.abs(end.x - start.x);
    const height = Math.abs(end.y - start.y);
    const selection = width < 0.012 && height < 0.012
      ? { kind: 'element', x: end.x, y: end.y }
      : {
          kind: 'region',
          rect: {
            x: Math.min(start.x, end.x),
            y: Math.min(start.y, end.y),
            width,
            height,
          },
        };
    submit('select', 'selection', JSON.stringify(selection));
  }

  function selectionStyle(): string {
    if (!dragStart || !dragCurrent) return '';
    return `left:${Math.min(dragStart.x, dragCurrent.x) * 100}%;top:${Math.min(dragStart.y, dragCurrent.y) * 100}%;width:${Math.abs(dragCurrent.x - dragStart.x) * 100}%;height:${Math.abs(dragCurrent.y - dragStart.y) * 100}%;`;
  }
</script>

{#if overlayState?.kind === 'menu'}
  <div
    class="desktop-overlay-menu-root"
    data-desktop-overlay-root="true"
    tabindex="-1"
    role="presentation"
    aria-busy={closePending}
    onpointerdown={(event) => {
      if (event.target instanceof Element && event.target.closest('.desktop-overlay-menu')) return;
      close();
    }}
  >
    <div
      class="desktop-overlay-menu"
      class:desktop-overlay-menu--viewport={overlayState.placement === 'browser-viewport'}
      role="menu"
      aria-label={overlayState.title}
    >
      {#if overlayState.items.length}
        <div class="desktop-overlay-menu-items">
          {#each overlayState.items as item (item.id)}
            <button
              type="button"
              class:selected={item.selected}
              disabled={item.disabled}
              role="menuitem"
              onclick={() => submit('select', item.id)}
            >
              {#if item.icon}
                <span class="desktop-overlay-menu-icon"><Icon name={item.icon as IconName} size={14} /></span>
              {/if}
              <span class="desktop-overlay-menu-label">{item.label}</span>
              {#if item.selected}<span class="desktop-overlay-menu-check"><Icon name="check" size={14} /></span>{/if}
            </button>
          {/each}
        </div>
      {/if}
      {#if overlayState.fields.length}
        <div class="desktop-overlay-menu-fields">
          {#each overlayState.fields as field (field.id)}
            <label>
              <span>{field.label}</span>
              <input
                type={field.type}
                value={fieldValues[field.id] ?? field.value}
                min={field.min ?? undefined}
                max={field.max ?? undefined}
                oninput={(event) => {
                  const value = (event.currentTarget as HTMLInputElement).value;
                  fieldValues = { ...fieldValues, [field.id]: value };
                  submit('input', field.id, value);
                }}
              />
            </label>
          {/each}
        </div>
      {/if}
    </div>
    {#if closePending}<div class="overlay-status desktop-overlay-menu-status" role="status" aria-live="polite">{i18n.t('common.close')}...</div>{/if}
    {#if actionError}<div class="overlay-error desktop-overlay-menu-error" role="alert">{actionError}</div>{/if}
  </div>
{:else if overlayState?.kind === 'annotation' && overlayState.phase === 'select'}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="annotation-capture"
    data-desktop-overlay-root="true"
    tabindex="-1"
    role="application"
    aria-label={overlayState.title}
    aria-busy={closePending}
    onpointerdown={handleAnnotationPointerDown}
    onpointermove={handleAnnotationPointerMove}
    onpointerup={handleAnnotationPointerUp}
    onmousedown={handleAnnotationMouseDown}
    onmousemove={handleAnnotationMouseMove}
    onmouseup={handleAnnotationMouseUp}
  >
    <div class="annotation-selection" style={selectionStyle()}></div>
    {#if closePending}<div class="overlay-status" role="status" aria-live="polite">{i18n.t('common.close')}...</div>{/if}
    {#if actionError}<div class="overlay-error" role="alert">{actionError}</div>{/if}
  </div>
{:else if overlayState?.kind === 'annotation' && overlayState.phase === 'comment'}
  <div class="annotation-editor" data-desktop-overlay-root="true" tabindex="-1" aria-label={overlayState.title} aria-busy={closePending}>
    <textarea
      value={fieldValues.comment ?? ''}
      placeholder={overlayState.title}
      oninput={(event) => {
        const value = (event.currentTarget as HTMLTextAreaElement).value;
        fieldValues = { ...fieldValues, comment: value };
        submit('input', 'comment', value);
      }}
    ></textarea>
    <div class="annotation-editor-actions">
      <button type="button" disabled={closePending} onclick={() => submit('select', 'cancel')}>取消</button>
      <button type="button" class="primary" disabled={closePending || !fieldValues.comment?.trim()} onclick={() => submit('select', 'save', fieldValues.comment ?? '')}>保存</button>
    </div>
    {#if closePending}<div class="overlay-status" role="status" aria-live="polite">{i18n.t('common.close')}...</div>{/if}
    {#if actionError}<div class="overlay-error" role="alert">{actionError}</div>{/if}
  </div>
{/if}

<style>
  :global(html), :global(body), :global(#app) { width: 100%; height: 100%; margin: 0; overflow: hidden; background: transparent; }
  :global(body) { outline: none; }
  /* 透明 Overlay 仍必须是一个明确的命中面。Electron 的原生 View
     覆盖在 Browser Surface 上时，若只依赖全透明背景，部分平台的合成层
     会把鼠标命中继续交给下方页面，导致标记拖拽变成网页自己的选择菜单。 */
  .annotation-capture {
    position: relative;
    width: 100%;
    height: 100%;
    cursor: crosshair !important;
    pointer-events: auto;
    touch-action: none;
    user-select: none;
    background: rgba(0, 0, 0, 0.001);
  }
  .annotation-selection { position: absolute; border: 1px solid var(--primary); background: color-mix(in srgb, var(--primary) 18%, transparent); pointer-events: none; }
  .annotation-editor { position: absolute; right: 12px; bottom: 12px; width: min(360px, calc(100% - 24px)); padding: 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
  .annotation-editor textarea { box-sizing: border-box; width: 100%; min-height: 74px; resize: vertical; padding: 7px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); }
  .annotation-editor-actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 8px; }
  .annotation-editor-actions button { min-width: 58px; height: 28px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); cursor: pointer; }
  .annotation-editor-actions button.primary { border-color: var(--primary); background: var(--primary); color: var(--primary-foreground); }
  .annotation-editor-actions button:disabled { opacity: .5; cursor: default; }
  .overlay-status { margin: 5px; padding: 5px 7px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); color: var(--foreground-muted); font-size: var(--text-xs); }
  .annotation-capture > .overlay-status { position: absolute; right: 8px; bottom: 8px; max-width: min(220px, calc(100% - 16px)); }
  .overlay-error { margin: 5px; padding: 6px 8px; border: 1px solid var(--error); border-radius: 4px; background: var(--error-muted); color: var(--error); font-size: var(--text-xs); overflow-wrap: anywhere; }
  .annotation-capture > .overlay-error { position: absolute; right: 8px; bottom: 8px; max-width: min(360px, calc(100% - 16px)); }
  .desktop-overlay-menu-root { position: absolute; inset: 0; background: transparent; }
  .desktop-overlay-menu {
    position: relative;
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    max-height: none;
    overflow: auto;
    padding: 5px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--dropdown-bg);
    box-shadow: var(--shadow-lg);
    pointer-events: auto;
  }
  .desktop-overlay-menu::-webkit-scrollbar { display: none; }
  .desktop-overlay-menu-items { display: grid; gap: 1px; }
  .desktop-overlay-menu-items > button {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    min-height: 30px;
    padding: 0 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--foreground);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    text-align: left;
  }
  .desktop-overlay-menu-items > button:hover:not(:disabled) { background: var(--surface-hover); }
  .desktop-overlay-menu-items > button.selected { color: var(--primary); }
  .desktop-overlay-menu-items > button:disabled { opacity: .45; cursor: default; }
  .desktop-overlay-menu-icon { display: grid; place-items: center; color: var(--foreground-muted); }
  .desktop-overlay-menu-items > button.selected .desktop-overlay-menu-icon { color: var(--primary); }
  .desktop-overlay-menu-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .desktop-overlay-menu-check { margin-left: auto; color: var(--primary); }
  .desktop-overlay-menu-fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 7px; margin-top: 6px; padding-top: 6px; border-top: 1px solid var(--border); }
  .desktop-overlay-menu-fields label { display: grid; gap: 4px; min-width: 0; color: var(--foreground-muted); font-size: 10px; font-weight: 500; }
  .desktop-overlay-menu-fields input { box-sizing: border-box; min-width: 0; width: 100%; height: 27px; padding: 0 7px; border: 1px solid var(--border); border-radius: 4px; outline: none; background: var(--surface-1); color: var(--foreground); font: inherit; font-variant-numeric: tabular-nums; }
  .desktop-overlay-menu-fields input:focus { border-color: var(--primary); }
  .desktop-overlay-menu-status { position: absolute; right: 8px; bottom: 8px; }
  .desktop-overlay-menu-error { position: absolute; right: 8px; bottom: 8px; max-width: min(300px, calc(100% - 16px)); }
</style>
