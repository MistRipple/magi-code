/** Shared lifetime for data-tooltip hints; each surface retains its own placement. */
export function delegatedTooltip(node: HTMLElement, present: (target: HTMLElement | null) => void) {
  const doc = node.ownerDocument;
  const view = doc.defaultView!;
  const listeners = new AbortController();
  let active: HTMLElement | null = null;
  let text = '';

  const targetFor = (target: EventTarget | null): HTMLElement | null => {
    const element = target instanceof Element ? target.closest<HTMLElement>('[data-tooltip]') : null;
    return element && node.contains(element) ? element : null;
  };
  const available = (target: HTMLElement): boolean => target.isConnected
    && node.contains(target)
    && Boolean(target.dataset.tooltip?.trim())
    && !target.matches(':disabled, [aria-disabled="true"], [aria-expanded="true"]')
    && !target.closest('[hidden], [inert], [aria-hidden="true"]')
    && target.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true });

  const dismiss = () => {
    observer.disconnect();
    if (!active) return;
    active = null;
    text = '';
    present(null);
  };
  // DOM removal/disable/hiding need not emit pointerout or focusout.
  const observer = new MutationObserver(() => {
    if (active && (!available(active) || active.dataset.tooltip?.trim() !== text)) dismiss();
  });
  const show = (target: HTMLElement | null) => {
    if (!target || !available(target)) {
      dismiss();
      return;
    }
    if (active === target) return;
    dismiss();
    active = target;
    text = target.dataset.tooltip!.trim();
    present(target);
    const attributes = ['data-tooltip', 'disabled', 'aria-disabled', 'aria-expanded', 'aria-hidden', 'hidden', 'inert', 'class', 'style'];
    observer.observe(node, { subtree: true, childList: true, attributes: true, attributeFilter: attributes });
    for (let parent = node.parentElement; parent; parent = parent.parentElement) {
      observer.observe(parent, { attributes: true, attributeFilter: attributes });
    }
  };
  const pointerOver = (event: PointerEvent) => {
    if (event.pointerType === 'touch') return;
    const target = targetFor(event.target);
    if (target && event.relatedTarget instanceof Node && target.contains(event.relatedTarget)) return;
    show(target);
  };
  const pointerOut = (event: PointerEvent) => {
    const target = targetFor(event.target);
    if (target && event.relatedTarget instanceof Node && target.contains(event.relatedTarget)) return;
    dismiss();
  };
  const focusIn = (event: FocusEvent) => {
    const target = targetFor(event.target);
    // Mouse clicks leave focus behind; only keyboard focus should open a hint.
    show(target?.matches(':focus-visible') ? target : null);
  };
  const keyDown = (event: KeyboardEvent) => {
    if (event.key === 'Escape' && active) {
      event.preventDefault();
      dismiss();
    }
  };
  const options = { signal: listeners.signal };
  node.addEventListener('pointerover', pointerOver, options);
  node.addEventListener('pointerout', pointerOut, options);
  node.addEventListener('pointerleave', dismiss, options);
  node.addEventListener('pointercancel', dismiss, options);
  node.addEventListener('focusin', focusIn, options);
  node.addEventListener('focusout', dismiss, options);
  view.addEventListener('pointerdown', dismiss, { ...options, capture: true });
  view.addEventListener('scroll', dismiss, { ...options, capture: true, passive: true });
  view.addEventListener('resize', dismiss, options);
  view.addEventListener('blur', dismiss, options);
  view.addEventListener('keydown', keyDown, { ...options, capture: true });
  doc.addEventListener('visibilitychange', dismiss, options);
  return { destroy() {
    listeners.abort();
    dismiss();
  } };
}
