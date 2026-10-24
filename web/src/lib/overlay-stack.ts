/** 共享键盘所有权：嵌套确认只让最上层处理 Esc / Tab。 */
const overlays: symbol[] = [];
export function topOverlay(id: symbol): boolean { return overlays.at(-1) === id; }
export function registerOverlay(id: symbol): () => void {
  overlays.push(id);
  return () => { const index = overlays.indexOf(id); if (index >= 0) overlays.splice(index, 1); };
}

export function hasOverlay(): boolean { return overlays.length > 0; }
