<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './components/Icon.svelte';
  import type { IconName } from './lib/icons';

  let overlayState: MagiDesktopOverlayState | null = $state(null);
  let fieldValues: Record<string, string> = $state({});
  let dragStart = $state<{ x: number; y: number } | null>(null);
  let dragCurrent = $state<{ x: number; y: number } | null>(null);
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
    });
    void desktop.readyOverlay().catch((error) => {
      console.warn('[DesktopOverlayShell] 覆盖层就绪握手失败:', error);
    });
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close();
    };
    window.addEventListener('keydown', escape);
    return () => {
      stop();
      window.removeEventListener('keydown', escape);
    };
  });

  function point(event: PointerEvent): { x: number; y: number } | null {
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

  function selectionStyle(): string {
    if (!dragStart || !dragCurrent) return '';
    return `left:${Math.min(dragStart.x, dragCurrent.x) * 100}%;top:${Math.min(dragStart.y, dragCurrent.y) * 100}%;width:${Math.abs(dragCurrent.x - dragStart.x) * 100}%;height:${Math.abs(dragCurrent.y - dragStart.y) * 100}%;`;
  }
</script>

{#if overlayState?.kind === 'annotation' && overlayState.phase === 'select'}
  <div
    class="annotation-capture"
    role="application"
    aria-label={overlayState.title}
    onpointerdown={handleAnnotationPointerDown}
    onpointermove={handleAnnotationPointerMove}
    onpointerup={handleAnnotationPointerUp}
  >
    <div class="annotation-selection" style={selectionStyle()}></div>
  </div>
{:else if overlayState?.kind === 'annotation' && overlayState.phase === 'comment'}
  <div class="annotation-editor" aria-label={overlayState.title}>
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
  </div>
{:else if overlayState}
  <div class="overlay-menu" role="menu" aria-label={overlayState.title}>
    {#each overlayState.items as item (item.id)}
      <button
        type="button"
        class="overlay-item"
        class:selected={item.selected}
        disabled={item.disabled}
        role="menuitem"
        onclick={() => submit('select', item.id)}
      >
        {#if item.icon}<Icon name={item.icon as IconName} size={14} />{/if}
        <span>{item.label}</span>
      </button>
    {/each}
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
  </div>
{/if}

<style>
  :global(html), :global(body), :global(#app) { width: 100%; height: 100%; margin: 0; overflow: hidden; background: transparent; }
  .annotation-capture { position: relative; width: 100%; height: 100%; cursor: crosshair; background: transparent; }
  .annotation-selection { position: absolute; border: 1px solid var(--primary); background: color-mix(in srgb, var(--primary) 18%, transparent); pointer-events: none; }
  .annotation-editor { position: absolute; right: 12px; bottom: 12px; width: min(360px, calc(100% - 24px)); padding: 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
  .annotation-editor textarea { box-sizing: border-box; width: 100%; min-height: 74px; resize: vertical; padding: 7px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); }
  .annotation-editor-actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 8px; }
  .annotation-editor-actions button { min-width: 58px; height: 28px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); cursor: pointer; }
  .annotation-editor-actions button.primary { border-color: var(--primary); background: var(--primary); color: #fff; }
  .annotation-editor-actions button:disabled { opacity: .5; cursor: default; }
  .overlay-menu { box-sizing: border-box; width: 100%; height: 100%; overflow: auto; padding: 5px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); color: var(--foreground); }
  .overlay-item { box-sizing: border-box; display: flex; align-items: center; gap: 8px; width: 100%; min-height: 32px; padding: 0 8px; border: 0; border-radius: 4px; background: transparent; color: inherit; font: inherit; font-size: var(--text-xs); cursor: pointer; text-align: left; }
  .overlay-item:hover:not(:disabled), .overlay-item.selected { background: var(--surface-hover); }
  .overlay-item.selected { color: var(--primary); }
  .overlay-item:disabled { opacity: .45; cursor: default; }
  .overlay-fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 5px; padding: 5px 3px 2px; border-top: 1px solid var(--border); margin-top: 4px; }
  .overlay-fields label { display: grid; gap: 3px; min-width: 0; color: var(--foreground-muted); font-size: 10px; }
  .overlay-fields input { box-sizing: border-box; min-width: 0; width: 100%; height: 27px; padding: 0 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; }
</style>
