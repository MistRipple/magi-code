<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from './components/Icon.svelte';
  import type { IconName } from './lib/icons';

  let overlayState: MagiDesktopOverlayState | null = $state(null);
  let fieldValues: Record<string, string> = $state({});
  let dragStart = $state<{ x: number; y: number } | null>(null);
  let dragCurrent = $state<{ x: number; y: number } | null>(null);
  let actionError = $state('');
  const desktop = window.magiDesktop;

  function submit(interaction: 'select' | 'input', id: string, value: string | null = null): void {
    const current = overlayState;
    if (!desktop || !current) return;
    void desktop.submitOverlayAction({
      overlayId: current.overlayId,
      kind: current.kind,
      ownerId: current.ownerId,
      interaction,
      id,
      value,
    }).then(() => {
      actionError = '';
    }).catch((error) => {
      actionError = error instanceof Error ? error.message : String(error);
    });
  }

  function close(): void {
    void desktop?.closeOverlay();
  }

  onMount(() => {
    if (!desktop) return;
    const stop = desktop.onOverlayState((next) => {
      overlayState = next;
      fieldValues = Object.fromEntries(next.fields.map((field) => [field.id, field.value]));
      dragStart = null;
      dragCurrent = null;
      actionError = '';
      // Main 进程先把输入焦点交给 Overlay WebContents；Renderer 等 DOM
      // 完成更新后再聚焦根节点，键盘和辅助功能操作才会落到同一菜单。
      void tick().then(() => {
        window.focus();
        document.body.tabIndex = -1;
        document.body.focus({ preventScroll: true });
        document.querySelector<HTMLElement>('[data-desktop-overlay-root]')?.focus({ preventScroll: true });
      });
    });
    const stopClosed = desktop.onOverlayClosed(() => {
      overlayState = null;
      fieldValues = {};
      dragStart = null;
      dragCurrent = null;
      actionError = '';
    });
    void desktop.readyOverlay().catch((error) => {
      console.warn('[DesktopOverlayShell] 覆盖层就绪握手失败:', error);
    });
    const escape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      close();
    };
    window.addEventListener('keydown', escape);
    return () => {
      stop();
      stopClosed();
      window.removeEventListener('keydown', escape);
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
    if (overlayState?.kind !== 'annotation' || overlayState.phase !== 'select') return;
    event.preventDefault();
    const next = point(event);
    if (!next) return;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    dragStart = next;
    dragCurrent = next;
  }

  function handleAnnotationPointerMove(event: PointerEvent): void {
    if (!dragStart || overlayState?.kind !== 'annotation' || overlayState.phase !== 'select') return;
    const next = point(event);
    if (next) dragCurrent = next;
  }

  function handleAnnotationPointerUp(event: PointerEvent): void {
    if (!dragStart || overlayState?.kind !== 'annotation' || overlayState.phase !== 'select') return;
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
    if (dragStart) return;
    event.preventDefault();
    const next = point(event);
    if (!next) return;
    dragStart = next;
    dragCurrent = next;
  }

  function handleAnnotationMouseMove(event: MouseEvent): void {
    if (!dragStart) return;
    const next = point(event);
    if (next) dragCurrent = next;
  }

  function handleAnnotationMouseUp(event: MouseEvent): void {
    if (!dragStart) return;
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

{#if overlayState?.kind === 'annotation' && overlayState.phase === 'select'}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="annotation-capture"
    data-desktop-overlay-root="true"
    tabindex="-1"
    role="application"
    aria-label={overlayState.title}
    onpointerdown={handleAnnotationPointerDown}
    onpointermove={handleAnnotationPointerMove}
    onpointerup={handleAnnotationPointerUp}
    onmousedown={handleAnnotationMouseDown}
    onmousemove={handleAnnotationMouseMove}
    onmouseup={handleAnnotationMouseUp}
  >
    <div class="annotation-selection" style={selectionStyle()}></div>
    {#if actionError}<div class="overlay-error" role="alert">{actionError}</div>{/if}
  </div>
{:else if overlayState?.kind === 'annotation' && overlayState.phase === 'comment'}
  <div class="annotation-editor" data-desktop-overlay-root="true" tabindex="-1" aria-label={overlayState.title}>
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
      <button type="button" onclick={() => submit('select', 'cancel')}>取消</button>
      <button type="button" class="primary" disabled={!fieldValues.comment?.trim()} onclick={() => submit('select', 'save', fieldValues.comment ?? '')}>保存</button>
    </div>
    {#if actionError}<div class="overlay-error" role="alert">{actionError}</div>{/if}
  </div>
{:else if overlayState}
  <div class="overlay-menu" class:overlay-menu--viewport={overlayState.placement === 'browser-viewport'} data-desktop-overlay-root="true" tabindex="-1" role="menu" aria-label={overlayState.title}>
    {#if overlayState.items.length}
      <div class="overlay-menu-items">
        {#each overlayState.items as item (item.id)}
          <button
            type="button"
            class="overlay-item"
            class:selected={item.selected}
            disabled={item.disabled}
            role="menuitem"
            onclick={() => submit('select', item.id)}
          >
            {#if item.icon}
              <span class="overlay-item-icon"><Icon name={item.icon as IconName} size={14} /></span>
            {/if}
            <span class="overlay-item-label">{item.label}</span>
            {#if item.selected}<span class="overlay-item-check"><Icon name="check" size={14} /></span>{/if}
          </button>
        {/each}
      </div>
    {/if}
    {#if overlayState.fields.length}
      <div class="overlay-fields">
        {#each overlayState.fields as field (field.id)}
          <label>
            <span>{field.label}</span>
            <input
              type={field.type}
              value={field.value}
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
  .overlay-menu { box-sizing: border-box; display: flex; flex-direction: column; width: 100%; height: 100%; min-height: 0; overflow: auto; scrollbar-width: none; padding: 6px; border: 1px solid var(--border); border-radius: 9px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); color: var(--foreground); }
  .overlay-menu::-webkit-scrollbar { display: none; }
  .overlay-menu--viewport { overflow: hidden; }
  .overlay-menu-items { display: grid; gap: 2px; }
  .overlay-item { box-sizing: border-box; display: flex; align-items: center; gap: 8px; width: 100%; min-height: 36px; padding: 3px 7px 3px 4px; border: 1px solid transparent; border-radius: 7px; background: transparent; color: inherit; font: inherit; font-size: var(--text-xs); cursor: pointer; text-align: left; }
  .overlay-item:hover:not(:disabled) { background: var(--surface-hover); }
  .overlay-item.selected { border-color: color-mix(in srgb, var(--primary) 22%, var(--border)); background: color-mix(in srgb, var(--primary) 10%, var(--surface-1)); color: var(--primary); }
  .overlay-item:disabled { opacity: .45; cursor: default; }
  .overlay-item-icon { display: grid; place-items: center; width: 24px; height: 24px; flex: 0 0 24px; border-radius: 6px; background: var(--surface-2); color: var(--foreground-muted); }
  .overlay-item.selected .overlay-item-icon { background: color-mix(in srgb, var(--primary) 14%, var(--surface-1)); color: var(--primary); }
  .overlay-item-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .overlay-item-check { flex: 0 0 auto; margin-left: auto; color: var(--primary); }
  .overlay-fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 7px; margin-top: 7px; padding: 8px 3px 1px; border-top: 1px solid var(--border); }
  .overlay-fields label { display: grid; gap: 5px; min-width: 0; color: var(--foreground-muted); font-size: 10px; font-weight: 500; }
  .overlay-fields input { box-sizing: border-box; min-width: 0; width: 100%; height: 30px; padding: 0 7px; border: 1px solid var(--border); border-radius: 6px; outline: none; background: var(--surface-1); color: var(--foreground); font: inherit; font-variant-numeric: tabular-nums; }
  .overlay-fields input:focus { border-color: var(--primary); box-shadow: 0 0 0 2px color-mix(in srgb, var(--primary) 18%, transparent); }
  .overlay-error { margin: 5px; padding: 6px 8px; border: 1px solid var(--error); border-radius: 4px; background: var(--error-muted); color: var(--error); font-size: var(--text-xs); overflow-wrap: anywhere; }
  .annotation-capture > .overlay-error { position: absolute; right: 8px; bottom: 8px; max-width: min(360px, calc(100% - 16px)); }
</style>
