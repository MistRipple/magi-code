import assert from "node:assert/strict";
import test from "node:test";
import { BrowserSurfaceRegistry, type BrowserSurfaceRegistryRecord } from "./browser-surface-registry.js";

function record(surfaceId: string, windowId: string, tabId: string): BrowserSurfaceRegistryRecord {
  return { surfaceId, windowId, tabId, primary: false, closed: false };
}

test("同一逻辑 Tab 的多窗口 Surface 共享唯一 Primary，但各自独立查找", () => {
  const registry = new BrowserSurfaceRegistry<BrowserSurfaceRegistryRecord>();
  const first = record("surface-1", "window-1", "tab-1");
  const second = record("surface-2", "window-2", "tab-1");
  registry.add(first);
  registry.add(second);

  assert.equal(registry.forWindowTab("window-1", "tab-1"), first);
  assert.equal(registry.forWindowTab("window-2", "tab-1"), second);
  assert.equal(registry.primaryForTab("tab-1"), undefined);

  registry.promote(first.surfaceId);
  assert.equal(registry.isPrimary(first), true);
  assert.equal(registry.isPrimary(second), false);

  const promotion = registry.promote(second.surfaceId);
  assert.equal(promotion.previous, first);
  assert.equal(registry.primaryForTab("tab-1"), second);
  assert.equal(registry.isPrimary(first), false);
  assert.equal(registry.isPrimary(second), true);
});

test("Primary Surface 关闭后只提升同一逻辑 Tab 的剩余 Surface", () => {
  const registry = new BrowserSurfaceRegistry<BrowserSurfaceRegistryRecord>();
  const first = record("surface-1", "window-1", "tab-1");
  const second = record("surface-2", "window-2", "tab-1");
  const otherTab = record("surface-3", "window-2", "tab-2");
  registry.add(first);
  registry.add(second);
  registry.add(otherTab);
  registry.promote(first.surfaceId);

  registry.remove(first);
  const fallback = registry.promoteFallback("tab-1");

  assert.equal(fallback, second);
  assert.equal(registry.primaryForTab("tab-1"), second);
  assert.equal(registry.primaryForTab("tab-2"), undefined);
});

test("关闭窗口中的 Surface 不会误删另一窗口的同一 Tab", () => {
  const registry = new BrowserSurfaceRegistry<BrowserSurfaceRegistryRecord>();
  const first = record("surface-1", "window-1", "tab-1");
  const second = record("surface-2", "window-2", "tab-1");
  registry.add(first);
  registry.add(second);
  registry.promote(second.surfaceId);

  registry.remove(first);

  assert.equal(registry.forWindowTab("window-1", "tab-1"), undefined);
  assert.equal(registry.forWindowTab("window-2", "tab-1"), second);
  assert.equal(registry.primaryForTab("tab-1"), second);
});

test("同一逻辑 Tab 的 Surface revision 在多窗口切换时全局单调", () => {
  const registry = new BrowserSurfaceRegistry<BrowserSurfaceRegistryRecord>();

  assert.equal(registry.nextRevision("tab-1"), 1);
  assert.equal(registry.nextRevision("tab-2"), 1);
  assert.equal(registry.nextRevision("tab-1"), 2);
  assert.equal(registry.nextRevision("tab-1"), 3);

  registry.clear();
  assert.equal(registry.nextRevision("tab-1"), 1);
});
