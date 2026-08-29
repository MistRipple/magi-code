import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { browserDownloadRoot, clearBrowserDownloads } from "./browser-download-storage.js";

test("清理 browser-downloads 内容但保留 userData 下的其它文件", async () => {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "magi-browser-downloads-"));
  try {
    const userDataPath = join(temporaryRoot, "user-data");
    const downloadRoot = browserDownloadRoot(userDataPath);
    await mkdir(join(downloadRoot, "session-a", "nested"), { recursive: true });
    await writeFile(join(downloadRoot, "session-a", "download.bin"), "download");
    await writeFile(join(downloadRoot, "session-a", "nested", "partial.tmp"), "partial");
    await mkdir(userDataPath, { recursive: true });
    await writeFile(join(userDataPath, "browser-partitions.json"), "preserve");

    await clearBrowserDownloads(userDataPath);

    assert.deepEqual(await readdir(downloadRoot), []);
    assert.equal(await readFile(join(userDataPath, "browser-partitions.json"), "utf8"), "preserve");
  } finally {
    await rm(temporaryRoot, { force: true, recursive: true });
  }
});

test("首次启动没有 browser-downloads 时创建空私有目录", async () => {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "magi-browser-downloads-"));
  try {
    const userDataPath = join(temporaryRoot, "user-data");
    await clearBrowserDownloads(userDataPath);
    assert.deepEqual(await readdir(browserDownloadRoot(userDataPath)), []);
  } finally {
    await rm(temporaryRoot, { force: true, recursive: true });
  }
});

test("browser-downloads 根路径不是目录时拒绝清理，不触碰该路径", async () => {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "magi-browser-downloads-"));
  try {
    const userDataPath = join(temporaryRoot, "user-data");
    const downloadRoot = browserDownloadRoot(userDataPath);
    await mkdir(userDataPath, { recursive: true });
    await writeFile(downloadRoot, "user-data-file");

    await assert.rejects(
      clearBrowserDownloads(userDataPath),
      /browser_download_root_invalid/u,
    );
    assert.equal(await readFile(downloadRoot, "utf8"), "user-data-file");
  } finally {
    await rm(temporaryRoot, { force: true, recursive: true });
  }
});
