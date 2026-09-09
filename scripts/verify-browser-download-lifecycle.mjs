import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const read = (path) => readFile(join(root, path), "utf8");
const [indexSource, managerSource, storageSource] = await Promise.all([
  read("apps/desktop/src/main/index.ts"),
  read("apps/desktop/src/main/browser-surface-manager.ts"),
  read("apps/desktop/src/main/browser-download-storage.ts"),
]);

function ordered(source, markers, label) {
  let cursor = -1;
  for (const marker of markers) {
    const next = source.indexOf(marker, cursor + 1);
    assert.notEqual(next, -1, `${label} 缺少或顺序错误: ${marker}`);
    cursor = next;
  }
}

assert.match(storageSource, /export const BROWSER_DOWNLOAD_DIRECTORY = "browser-downloads"/u);
assert.match(storageSource, /lstat\(root\)/u);
assert.match(storageSource, /rootStat\.isSymbolicLink\(\)/u);
assert.match(storageSource, /rm\(join\(root, entry\)/u);
assert.doesNotMatch(storageSource, /browser-uploads/u);

ordered(
  indexSource,
  [
    "const surfaces = new BrowserSurfaceManager({",
    "await surfaces.clearDownloads();",
    "const browserUploadRoot = join(",
  ],
  "启动下载清理",
);
const shutdownStart = indexSource.indexOf("async function shutdown(): Promise<void>");
assert.notEqual(shutdownStart, -1, "退出清理缺少 shutdown");
const shutdownSource = indexSource.slice(shutdownStart);
ordered(
  shutdownSource,
  ["surfaceManager?.closeAll();", "automationWorker?.stop();", "await surfaceManager?.clearDownloads();"],
  "退出下载清理",
);

assert.match(managerSource, /async clearBrowsingData\(\): Promise<void> \{\s*await this\.clearDownloads\(\)/u);
assert.match(managerSource, /#downloadCleanupInProgress/u);
assert.match(managerSource, /this\.cancelActiveDownloads\(\)/u);
assert.match(managerSource, /item\.cancel\(\)/u);
assert.match(managerSource, /browser_download_cleanup_in_progress/u);

console.log("Browser 下载生命周期静态验证通过。");
