import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { build as esbuild } from "esbuild";
import { desktopRoot, readProductVersion, repositoryRoot } from "./build-support.mjs";

const config = await readFile(join(desktopRoot, "electron-builder.yml"), "utf8");
const requiredConfig = [
  "appId: com.mistripple.magi",
  "asar: true",
  "browser-automation-worker",
  "magi-daemon-app",
  "browser-capability-manifest.json",
  "magi-desktop.cdx.json",
];
for (const requirement of requiredConfig) {
  if (!config.includes(requirement)) throw new Error(`Electron Builder 配置缺少: ${requirement}`);
}
if (/tauri|cef|playwright|native-browser|browser-runtime/i.test(config)) {
  throw new Error("Electron Builder 配置不得包含 Tauri、CEF、Playwright 或独立 Browser Runtime");
}
await readProductVersion();

const shared = {
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node22",
  external: ["electron"],
  logLevel: "silent",
  write: false,
};
await Promise.all([
  esbuild({
    ...shared,
    entryPoints: [join(desktopRoot, "src", "main", "index.ts")],
    define: { "import.meta.url": "__magiImportMetaUrl" },
    banner: { js: "const __magiImportMetaUrl = require('node:url').pathToFileURL(__filename).href;" },
  }),
  esbuild({ ...shared, entryPoints: [join(desktopRoot, "src", "preload", "index.ts")] }),
  esbuild({
    ...shared,
    external: [],
    entryPoints: [join(repositoryRoot, "browser-automation-worker", "src", "index.ts")],
  }),
]);

process.stdout.write("Electron Desktop 构建链检查通过。\n");
