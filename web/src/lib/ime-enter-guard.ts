/**
 * 选字结束后补发的重复回车只会紧跟 compositionend 到达；窗口只需覆盖这一次事件，
 * 远小于人手再按一次回车的间隔，用户确认选字后的正常回车不会感到延迟。
 */
export const POST_COMPOSITION_ENTER_GUARD_MS = 50;

/** Protect every text-entry shortcut, including rename, settings and browser URL fields. */
export function imeEnterGuard(node: HTMLElement) {
  let disposed = false;
  const composing = new WeakSet<EventTarget>();
  const endedAt = new WeakMap<EventTarget, number>();
  const fieldFor = (target: EventTarget | null): HTMLElement | null => {
    if (!(target instanceof HTMLElement)) return null;
    const field = target.closest('input, textarea, [contenteditable="true"]');
    return field instanceof HTMLElement ? field : null;
  };
  const start = (event: CompositionEvent) => {
    if (disposed) return;
    const field = fieldFor(event.target);
    if (field) composing.add(field);
  };
  const end = (event: CompositionEvent) => {
    if (disposed) return;
    const field = fieldFor(event.target);
    if (!field) return;
    composing.delete(field);
    endedAt.set(field, performance.now());
  };
  const keydown = (event: KeyboardEvent) => {
    if (disposed) return;
    const field = fieldFor(event.target);
    if (!field) return;
    if (event.isComposing || event.keyCode === 229 || composing.has(field)) {
      // Leave native candidate selection intact; don't let application shortcuts see it.
      event.stopImmediatePropagation();
      return;
    }
    if ((event.key === 'Enter' || event.code === 'NumpadEnter')
      && !event.shiftKey && !event.altKey
      && performance.now() - (endedAt.get(field) ?? -Infinity) < POST_COMPOSITION_ENTER_GUARD_MS) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  };
  node.addEventListener('compositionstart', start, true);
  node.addEventListener('compositionend', end, true);
  node.addEventListener('keydown', keydown, true);
  return { destroy() {
    if (disposed) return;
    disposed = true;
    node.removeEventListener('compositionstart', start, true);
    node.removeEventListener('compositionend', end, true);
    node.removeEventListener('keydown', keydown, true);
  } };
}
