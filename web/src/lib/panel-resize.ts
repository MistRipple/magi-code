/** One pointer transaction for shell dividers, including guests/iframes. */
export function beginPanelResize(event: PointerEvent, callbacks: {
  move: (event: PointerEvent) => void;
  finish: (cancelled: boolean) => void;
}): () => void {
  if (event.button !== 0 || event.isPrimary === false) return () => {};
  event.preventDefault();
  const handle = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
  const pointerId = event.pointerId;
  const cursor = document.body.style.cursor;
  const userSelect = document.body.style.userSelect;
  const shield = document.createElement('div');
  shield.className = 'panel-resize-shield';
  shield.setAttribute('aria-hidden', 'true');
  // Top Layer also shields Electron guests and any open preview popovers.
  shield.setAttribute('popover', 'manual');
  document.body.appendChild(shield);
  shield.showPopover?.();
  document.body.style.cursor = 'col-resize';
  document.body.style.userSelect = 'none';
  let stopped = false;
  let moved = false;
  try { handle?.setPointerCapture(pointerId); } catch { /* Window listeners also cover capture-less hosts. */ }

  const swallowClick = (next: MouseEvent) => {
    next.preventDefault();
    next.stopImmediatePropagation();
  };
  const finish = (cancelled: boolean) => {
    if (stopped) return;
    stopped = true;
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', up);
    window.removeEventListener('pointercancel', cancel);
    window.removeEventListener('keydown', key, true);
    window.removeEventListener('blur', blur);
    handle?.removeEventListener('lostpointercapture', blur);
    shield.remove();
    document.body.style.cursor = cursor;
    document.body.style.userSelect = userSelect;
    try {
      if (handle?.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
    } catch { /* Capture may already be released. */ }
    if (moved) {
      window.addEventListener('click', swallowClick, true);
      setTimeout(() => window.removeEventListener('click', swallowClick, true), 0);
    }
    callbacks.finish(cancelled);
  };
  const move = (next: PointerEvent) => {
    if (next.pointerId !== pointerId) return;
    if (Math.hypot(next.clientX - event.clientX, next.clientY - event.clientY) <= 3 && !moved) return;
    moved = true;
    callbacks.move(next);
  };
  const up = (next: PointerEvent) => { if (next.pointerId === pointerId) finish(false); };
  const cancel = (next: PointerEvent) => { if (next.pointerId === pointerId) finish(true); };
  const blur = () => finish(true);
  const key = (next: KeyboardEvent) => {
    if (next.key !== 'Escape') return;
    next.preventDefault();
    next.stopImmediatePropagation();
    finish(true);
  };
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', up);
  window.addEventListener('pointercancel', cancel);
  window.addEventListener('keydown', key, true);
  window.addEventListener('blur', blur);
  handle?.addEventListener('lostpointercapture', blur);
  return () => finish(true);
}
