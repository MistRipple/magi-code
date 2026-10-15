import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import { buildSync } from "esbuild";
import test from "node:test";
import type { WindowManager, DesktopAppearance } from "./window-manager.js";

const require = createRequire(import.meta.url);
const compiled = buildSync({
  entryPoints: [fileURLToPath(new URL("./window-manager.ts", import.meta.url))],
  bundle: true, platform: "node", format: "cjs", target: "node22",
  external: ["electron"], write: false,
}).outputFiles[0]!.text;

for (const platform of ["darwin", "win32", "linux"]) {
test(`${platform} 冷启动、主题恢复和新窗口使用同一主题标题栏`, async () => {
  class NativeWindow extends EventEmitter {
    background: string;
    accent: unknown;
    overlay: { color?: string; symbolColor?: string; height?: number } = {};
    visible = false;
    webContents = Object.assign(new EventEmitter(), {
      isDestroyed: () => false, setWindowOpenHandler() {}, send() {},
      loadURL: async () => {},
    });
    constructor(readonly options: {
      backgroundColor: string; accentColor?: unknown; show: boolean;
      titleBarStyle: string; titleBarOverlay: boolean | { color?: string; symbolColor?: string; height?: number };
    }) {
      super();
      this.background = options.backgroundColor;
      this.accent = options.accentColor;
      this.visible = options.show;
      if (typeof options.titleBarOverlay === "object") this.overlay = options.titleBarOverlay;
    }
    isDestroyed() { return false; }
    getContentBounds() { return { x: 0, y: 0, width: 1000, height: 700 }; }
    setBackgroundColor(color: string) { this.background = color; }
    setTitleBarOverlay(overlay: typeof this.overlay) { this.overlay = { ...this.overlay, ...overlay }; }
    setAccentColor(color: unknown) { this.accent = color; }
    setBackgroundMaterial() {}
    isVisible() { return this.visible; }
    setContentSize() {}
    show() { this.visible = true; }
  }
  const display = { workAreaSize: { width: 1440, height: 900 }, scaleFactor: 1 };
  const electron = {
    BrowserWindow: NativeWindow,
    screen: Object.assign(new EventEmitter(), { getPrimaryDisplay: () => display }),
  };
  const module = { exports: {} as { WindowManager: typeof WindowManager } };
  runInNewContext(`(function(require, module, exports, process) { ${compiled}\n})`, { URL })(
    (name: string) => name === "electron" ? electron : require(name),
    module, module.exports, { platform },
  );
  const windows = new Map();
  const manager = new module.exports.WindowManager({
    desktopEpoch: "test", preloadPath: "/preload", agentOrigin: "http://127.0.0.1:38123",
    windows,
    surfaceManager: {
      attachWindow() {}, viewportForSurface: () => null, displayMetricsForSurface: () => null,
      navigationRevisionForSurface: () => null, browserDownloadSnapshotsForWindow: () => [],
      browserSurfaceSnapshotsForWindow: () => [],
    } as unknown as ConstructorParameters<typeof WindowManager>[0]["surfaceManager"],
    onSnapshot() {},
  });
  const firstId = manager.createWindow();
  const first = windows.get(firstId) as NativeWindow;
  assert.equal(first.visible, false);
  assert.equal(first.options.titleBarStyle, "hidden", "主题绘制整窗标题栏，系统只提供窗口按钮");
  if (platform === "win32") assert.equal(first.options.accentColor, first.options.backgroundColor);
  if (platform === "darwin") assert.equal(first.options.titleBarOverlay, true);
  else assert.equal(first.overlay.color, first.background);
  const appearance: DesktopAppearance = {
    backgroundColor: "#ffffff", accentColor: "#2563eb", mode: "light", themeSource: "light", material: "clear",
  };
  manager.setAppearance(firstId, appearance);
  manager.handleRightPaneReady(firstId);
  assert.equal(first.visible, true);
  if (platform === "win32") assert.equal(first.accent, "#ffffff");
  const secondId = manager.createWindow();
  const second = windows.get(secondId) as NativeWindow;
  assert.equal(second.options.backgroundColor, "#ffffff", "新窗口直接继承当前背景");
  for (const [backgroundColor, mode, themeSource] of [
    ["#151820", "dark", "dark"], ["#f6f0e6", "light", "light"], ["#fafafa", "light", "system"],
  ] as const) {
    manager.setAppearance(firstId, { ...appearance, backgroundColor, mode, themeSource });
    for (const window of [first, second]) {
      assert.equal(window.background, backgroundColor);
      if (platform === "win32") assert.equal(window.accent, backgroundColor);
      if (platform !== "darwin") {
        assert.equal(window.overlay.color, backgroundColor, "窗口按钮底色不能留下旧主题颜色");
        assert.equal(window.overlay.symbolColor, mode === "dark" ? "#ffffff" : "#111827");
        assert.equal(window.overlay.height, 32);
      }
    }
  }
  await Promise.resolve();
});
}
