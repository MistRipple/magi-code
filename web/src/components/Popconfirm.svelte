<script lang="ts">
  import { onMount } from 'svelte';
  import { registerOverlay, topOverlay } from '../lib/overlay-stack';
  interface Props { anchor: HTMLElement; title: string; message: string; confirmLabel: string; cancelLabel: string; onConfirm: () => void; onCancel: () => void; }
  let { anchor, title, message, confirmLabel, cancelLabel, onConfirm, onCancel }: Props = $props();
  let panel: HTMLDivElement;
  const id = $props.id();
  const layer = Symbol('popconfirm');
  function keydown(event: KeyboardEvent) {
    if (topOverlay(layer) && event.key === 'Escape') { event.preventDefault(); event.stopImmediatePropagation(); onCancel(); }
  }
  onMount(() => {
    const unregister = registerOverlay(layer);
    window.addEventListener('keydown', keydown, true);
    panel.showPopover();
    const rect = anchor.getBoundingClientRect();
    const bounds = panel.getBoundingClientRect();
    panel.style.left = `${Math.max(12, Math.min(rect.left, innerWidth - bounds.width - 12))}px`;
    panel.style.top = `${Math.max(12, rect.bottom + bounds.height + 8 <= innerHeight ? rect.bottom + 8 : rect.top - bounds.height - 8)}px`;
    panel.querySelector<HTMLButtonElement>('button')?.focus();
    const outside = (event: PointerEvent) => { if (topOverlay(layer) && !panel.contains(event.target as Node) && !anchor.contains(event.target as Node)) onCancel(); };
    const reposition = () => { if (topOverlay(layer)) onCancel(); };
    document.addEventListener('pointerdown', outside, true);
    window.addEventListener('resize', reposition);
    window.addEventListener('scroll', reposition, true);
    return () => {
      const wasTop = topOverlay(layer);
      unregister();
      window.removeEventListener('keydown', keydown, true);
      document.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('resize', reposition);
      window.removeEventListener('scroll', reposition, true);
      if (wasTop && anchor.isConnected) anchor.focus({ preventScroll: true });
    };
  });
</script>

<div bind:this={panel!} popover="manual" class="popconfirm" data-overlay-kind="popconfirm" data-magi-surface="popover" role="alertdialog" aria-labelledby={id}>
  <strong id={id}>{title}</strong>
  <p>{message}</p>
  <div class="popconfirm-actions">
    <button type="button" class="btn btn--secondary btn--sm" onclick={onCancel}>{cancelLabel}</button>
    <button type="button" class="btn btn--error btn--sm" onclick={onConfirm}>{confirmLabel}</button>
  </div>
</div>

<style>
  .popconfirm { position: fixed; inset: auto; margin: 0; width: min(320px, calc(100vw - 24px)); box-sizing: border-box; padding: 14px; border: 0; border-radius: calc(var(--radius-sm) + 14px); box-shadow: var(--shadow-lg), inset 0 0 0 var(--magi-hairline) var(--border); color: var(--foreground); font-size: var(--text-sm); }
  strong { font-weight: var(--font-semibold); }
  p { margin: 8px 0 14px; color: var(--foreground-muted); white-space: pre-line; text-wrap: pretty; overflow-wrap: anywhere; }
  .popconfirm-actions { display: flex; justify-content: flex-end; gap: 8px; }
</style>
