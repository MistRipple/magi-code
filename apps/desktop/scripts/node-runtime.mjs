// 受信扩展宿主随包携带的固定版本 Node。
//
// 版本与每个平台归档的 SHA-256 固定在 apps/desktop/node-runtime.json（随仓库审阅）。
// 下载内容必须与固定摘要一致；只提取 node 可执行文件和许可证，其余（npm、头文件等）不进入发行物。
// 扩展宿主由 daemon 直接启动该可执行文件，不依赖用户 PATH，也不使用 Electron 自带的 Node。

import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { access, cp, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { constants, createWriteStream } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { extract as extractZip } from "@electron-internal/extract-zip";

const execFileAsync = promisify(execFile);
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const desktopRoot = resolve(scriptDirectory, "..");
const repositoryRoot = resolve(desktopRoot, "../..");

export const nodeRuntimeFileName = process.platform === "win32" ? "node.exe" : "node";
export const nodeRuntimeCache = join(repositoryRoot, "target", "node-runtime-cache");

export async function readNodeRuntimePin() {
  const pin = JSON.parse(await readFile(join(desktopRoot, "node-runtime.json"), "utf8"));
  const key = `${process.platform}-${process.arch}`;
  const archive = pin.archives?.[key];
  if (!archive) throw new Error(`没有为 ${key} 固定的随包 Node`);
  return { version: pin.version, source: pin.source, archive, key };
}

/**
 * 准备随包 Node：下载（或复用缓存）并校验归档摘要，再提取可执行文件与许可证。
 * 返回 `{ version, executable, license }`。
 */
export async function stageNodeRuntime(destination) {
  const pin = await readNodeRuntimePin();
  const archivePath = await fetchVerifiedArchive(pin);
  await rm(destination, { recursive: true, force: true });
  await mkdir(destination, { recursive: true });
  const extractionRoot = await mkdtemp(join(nodeRuntimeCache, "extract-"));
  try {
    const directory = pin.archive.file.replace(/\.(tar\.gz|tar\.xz|zip)$/u, "");
    let executable;
    let license;
    if (pin.archive.file.endsWith(".zip")) {
      await extractZip(archivePath, { dir: extractionRoot });
      executable = join(extractionRoot, directory, "node.exe");
      license = join(extractionRoot, directory, "LICENSE");
    } else {
      await execFileAsync("tar", [
        process.platform === "linux" ? "-xJf" : "-xzf",
        archivePath,
        "-C", extractionRoot,
        `${directory}/bin/node`,
        `${directory}/LICENSE`,
      ], { maxBuffer: 1024 * 1024 });
      executable = join(extractionRoot, directory, "bin", "node");
      license = join(extractionRoot, directory, "LICENSE");
    }
    await assertFile(executable, "Node 可执行文件");
    await assertFile(license, "Node 许可证");
    const staged = join(destination, nodeRuntimeFileName);
    await cp(executable, staged);
    await cp(license, join(destination, "LICENSE"));
    if (process.platform !== "win32") await (await import("node:fs/promises")).chmod(staged, 0o755);
    return { version: pin.version, executable: staged, license: join(destination, "LICENSE") };
  } finally {
    await rm(extractionRoot, { recursive: true, force: true });
  }
}

async function fetchVerifiedArchive(pin) {
  await mkdir(nodeRuntimeCache, { recursive: true });
  const archivePath = join(nodeRuntimeCache, pin.archive.file);
  if (await fileDigest(archivePath) === pin.archive.sha256) return archivePath;
  const url = `${pin.source}/${pin.archive.file}`;
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok || !response.body) throw new Error(`下载随包 Node 失败: ${url} (${response.status})`);
  const partial = `${archivePath}.partial-${process.pid}`;
  try {
    await pipeline(Readable.fromWeb(response.body), createWriteStream(partial));
    const actual = await fileDigest(partial);
    if (actual !== pin.archive.sha256) {
      throw new Error(`随包 Node 归档摘要不符，拒绝使用: ${pin.archive.file}\n  期望 ${pin.archive.sha256}\n  实际 ${actual}`);
    }
    await rename(partial, archivePath);
  } finally {
    await rm(partial, { force: true });
  }
  return archivePath;
}

async function fileDigest(path) {
  try {
    return createHash("sha256").update(await readFile(path)).digest("hex");
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

/**
 * 最终制品上的实际执行校验：清空环境（防止本地 PATH 或开发工具掩盖漏打包），
 * 确认版本、平台架构与 ESM/网络模块可用。
 */
export async function runNodeRuntimePreflight(directory) {
  const pin = await readNodeRuntimePin();
  const executable = join(directory, nodeRuntimeFileName);
  const metadata = await stat(executable);
  if (!metadata.isFile()) throw new Error(`随包 Node 不是文件: ${executable}`);
  await access(executable, constants.X_OK);
  const { stdout } = await execFileAsync(executable, [
    "--input-type=module",
    "-e",
    "const net = await import('node:net');"
      + "console.log(JSON.stringify({ version: process.versions.node, platform: process.platform, arch: process.arch, net: typeof net.createConnection }));",
  ], { env: {}, windowsHide: true, timeout: 20_000, maxBuffer: 4096 });
  const report = JSON.parse(stdout);
  if (report.version !== pin.version || `${report.platform}-${report.arch}` !== pin.key || report.net !== "function") {
    throw new Error(`随包 Node 执行校验失败: ${executable} ${stdout.trim()}`);
  }
  return report;
}

async function assertFile(path, label) {
  const value = await stat(path).catch(() => null);
  if (!value?.isFile()) throw new Error(`${label}不存在: ${path}`);
}

// 开发入口：node scripts/node-runtime.mjs stage <目录>，把固定版本 Node 准备到指定目录。
if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href && process.argv[2] === "stage") {
  const destination = resolve(process.argv[3] ?? join(repositoryRoot, "target", "extension-node"));
  const staged = await stageNodeRuntime(destination);
  await runNodeRuntimePreflight(destination);
  process.stdout.write(`${staged.executable}\n`);
}
