export interface BrowserSurfaceRegistryRecord {
  surfaceId: string;
  windowId: string;
  tabId: string;
  primary: boolean;
  closed: boolean;
}

export interface PrimaryPromotion<T> {
  previous: T | null;
  current: T;
}

/**
 * 维护 Browser WebContentsView 的 Surface 索引和逻辑 Tab 的单一 Primary。
 *
 * 一个逻辑 Tab 可以在多个桌面窗口各有一个 Surface，因此“窗口 + Tab”
 * 只能用于查找 Surface，不能用于判定 Primary。Primary 必须按逻辑 Tab
 * 全局唯一，否则 Authority 会收到来自 Secondary Surface 的页面事实。
 */
export class BrowserSurfaceRegistry<T extends BrowserSurfaceRegistryRecord> {
  readonly #records = new Map<string, T>();
  readonly #surfaceByWindowTab = new Map<string, string>();
  readonly #primaryByTab = new Map<string, string>();
  readonly #revisionByTab = new Map<string, number>();

  add(record: T): void {
    if (this.#records.has(record.surfaceId)) {
      throw new Error(`browser_surface_duplicate:${record.surfaceId}`);
    }
    const key = surfaceKey(record.windowId, record.tabId);
    const existing = this.#surfaceByWindowTab.get(key);
    if (existing) throw new Error(`browser_surface_window_tab_duplicate:${key}`);
    this.#records.set(record.surfaceId, record);
    this.#surfaceByWindowTab.set(key, record.surfaceId);
  }

  get(surfaceId: string): T | undefined {
    return this.#records.get(surfaceId);
  }

  forWindowTab(windowId: string, tabId: string): T | undefined {
    const surfaceId = this.#surfaceByWindowTab.get(surfaceKey(windowId, tabId));
    return surfaceId ? this.#records.get(surfaceId) : undefined;
  }

  primaryForTab(tabId: string): T | undefined {
    const surfaceId = this.#primaryByTab.get(tabId);
    return surfaceId ? this.#records.get(surfaceId) : undefined;
  }

  isPrimary(record: T): boolean {
    return !record.closed && this.#primaryByTab.get(record.tabId) === record.surfaceId;
  }

  nextRevision(tabId: string): number {
    const next = (this.#revisionByTab.get(tabId) ?? 0) + 1;
    this.#revisionByTab.set(tabId, next);
    return next;
  }

  promote(surfaceId: string): PrimaryPromotion<T> {
    const current = this.#records.get(surfaceId);
    if (!current || current.closed) throw new Error(`browser_surface_not_found:${surfaceId}`);
    const previous = this.primaryForTab(current.tabId) ?? null;
    if (previous?.surfaceId === surfaceId) {
      current.primary = true;
      return { previous, current };
    }
    if (previous) previous.primary = false;
    current.primary = true;
    this.#primaryByTab.set(current.tabId, surfaceId);
    return { previous, current };
  }

  promoteFallback(tabId: string): T | null {
    if (this.#primaryByTab.has(tabId)) return null;
    const fallback = [...this.#records.values()].find((record) => (
      !record.closed && record.tabId === tabId
    ));
    if (!fallback) return null;
    this.promote(fallback.surfaceId);
    return fallback;
  }

  remove(record: T): void {
    if (this.#records.get(record.surfaceId) !== record) return;
    this.#records.delete(record.surfaceId);
    const key = surfaceKey(record.windowId, record.tabId);
    if (this.#surfaceByWindowTab.get(key) === record.surfaceId) {
      this.#surfaceByWindowTab.delete(key);
    }
    if (this.#primaryByTab.get(record.tabId) === record.surfaceId) {
      this.#primaryByTab.delete(record.tabId);
    }
  }

  values(): IterableIterator<T> {
    return this.#records.values();
  }

  clear(): void {
    this.#records.clear();
    this.#surfaceByWindowTab.clear();
    this.#primaryByTab.clear();
    this.#revisionByTab.clear();
  }
}

function surfaceKey(windowId: string, tabId: string): string {
  return `${windowId}\u0000${tabId}`;
}
