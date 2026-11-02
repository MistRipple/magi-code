/** Keep the existing upward popover inside its conversation column and visual viewport. */
export function anchorStatsPanel(node: HTMLElement, trigger: HTMLButtonElement) {
  const column = trigger.closest('.ia-container')?.parentElement ?? document.documentElement;
  let frame = 0;
  const measure = () => {
    const anchor = trigger.getBoundingClientRect();
    const parent = column.getBoundingClientRect();
    const viewport = window.visualViewport;
    const left = Math.max(parent.left + 12, (viewport?.offsetLeft ?? 0) + 12);
    const right = Math.min(parent.right - 12, (viewport?.offsetLeft ?? 0) + (viewport?.width ?? window.innerWidth) - 12);
    const top = Math.max(parent.top + 8, (viewport?.offsetTop ?? 0) + 10);
    const width = Math.max(0, Math.min(356, right - left));
    node.style.width = `${width}px`;
    node.style.maxHeight = `${Math.max(0, Math.min(560, anchor.top - top - 10))}px`;
    node.style.left = `${Math.max(left, Math.min(anchor.right - width, right - width))}px`;
    node.style.top = `${Math.max(top, anchor.top - node.getBoundingClientRect().height - 10)}px`;
  };
  const schedule = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(measure); };
  const observer = new ResizeObserver(schedule);
  observer.observe(node);
  for (let parent: HTMLElement | null = trigger; parent; parent = parent.parentElement) observer.observe(parent);
  window.addEventListener('resize', schedule);
  window.addEventListener('scroll', schedule, true);
  window.visualViewport?.addEventListener('resize', schedule);
  window.visualViewport?.addEventListener('scroll', schedule);
  // Electron's native popover implementation can reject a show request while
  // the renderer is still attaching the anchored node. Keep the panel usable
  // in that frame instead of aborting the Svelte branch and leaving the trigger
  // permanently closed.
  let nativePopover = false;
  try {
    node.showPopover();
    nativePopover = true;
  } catch {
    // Keep the panel usable if Chromium rejects the native toggle while the
    // renderer is attaching the anchored node.
    node.style.display = 'flex';
  }
  measure();
  return { destroy() {
    observer.disconnect(); cancelAnimationFrame(frame);
    window.removeEventListener('resize', schedule);
    window.removeEventListener('scroll', schedule, true);
    window.visualViewport?.removeEventListener('resize', schedule);
    window.visualViewport?.removeEventListener('scroll', schedule);
    if (!nativePopover) node.style.removeProperty('display');
  } };
}
