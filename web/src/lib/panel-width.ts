/** Keep anchored panels inside the viewport without changing their opening direction. */
export function constrainPanelWidth(node: HTMLElement, initial: { align: 'start' | 'end'; active?: boolean }) {
  let cleanup = () => {};
  function update(options: typeof initial) {
    cleanup();
    if (options.active === false) return;
    const measure = () => {
      const owner = node.offsetParent;
      if (!(owner instanceof HTMLElement)) return;
      const rect = owner.getBoundingClientRect();
      const available = options.align === 'end' ? rect.right - 12 : window.innerWidth - rect.left - 12;
      node.style.setProperty('--panel-available-width', `${Math.max(0, available)}px`);
    };
    // Pane/toolbar resizing can move an anchor without resizing the window.
    const observer = new ResizeObserver(measure);
    for (let parent = node.parentElement; parent; parent = parent.parentElement) observer.observe(parent);
    window.addEventListener('resize', measure);
    window.addEventListener('scroll', measure, true);
    measure();
    cleanup = () => {
      observer.disconnect();
      window.removeEventListener('resize', measure);
      window.removeEventListener('scroll', measure, true);
    };
  }
  update(initial);
  return { update, destroy: () => cleanup() };
}
