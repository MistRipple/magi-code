import { app } from "electron";
import { createHash } from "node:crypto";
import { accessSync, closeSync, constants, createReadStream, existsSync, openSync, readFileSync } from "node:fs";
import { once } from "node:events";
import { mkdir, open, rm, stat } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { autoUpdater, type UpdateInfo } from "electron-updater";
import { load as parseYaml } from "js-yaml";

const DESKTOP_UPDATE_FEED = {
  provider: "github",
  owner: "MistRipple",
  repo: "magi-code",
  releaseType: "release",
} as const;

const MAC_UPDATE_METADATA_URL = "https://github.com/MistRipple/magi-code/releases/latest/download/latest-mac.yml";
const MAC_UPDATE_DOWNLOAD_BASE_URL = "https://github.com/MistRipple/magi-code/releases/download";
const execFileAsync = promisify(execFile);

export interface DesktopUpdateSnapshot {
  status: "idle" | "checking" | "available" | "downloading" | "downloaded" | "failed" | "unsupported";
  currentVersion: string;
  availableVersion: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
  error: string | null;
  installable: boolean;
}

interface MacUpdateFile {
  version: string;
  url: string;
  sha512: string;
  size: number | null;
}

export class UpdateManager {
  readonly #currentVersion: string;
  readonly #publish: (snapshot: DesktopUpdateSnapshot) => void;
  readonly #updateSupported: boolean;
  #snapshot: DesktopUpdateSnapshot;
  #checkPromise: Promise<DesktopUpdateSnapshot> | null = null;
  #downloadPromise: Promise<DesktopUpdateSnapshot> | null = null;
  #installPromise: Promise<never> | null = null;
  #macUpdateFile: MacUpdateFile | null = null;

  constructor(currentVersion: string, publish: (snapshot: DesktopUpdateSnapshot) => void) {
    this.#currentVersion = currentVersion;
    this.#publish = publish;
    // electron-builder 的目录测试包也会生成 app-update.yml，不能只依赖
    // 该文件判断发行包，否则本地验收会误访问线上更新通道。
    this.#updateSupported = app.isPackaged
      && readDistributionKind() !== "directory"
      && existsSync(join(process.resourcesPath, "app-update.yml"));
    this.#snapshot = {
      status: "idle",
      currentVersion,
      availableVersion: null,
      downloadedBytes: 0,
      totalBytes: null,
      percent: null,
      error: null,
      installable: process.platform !== "darwin" || canInstallUnsignedMacUpdate(),
    };
    autoUpdater.autoDownload = false;
    autoUpdater.autoInstallOnAppQuit = false;
    if (this.#updateSupported) {
      // 以产品发行源覆盖包内旧配置，防止历史 Browser Runtime channel 污染桌面更新链。
      autoUpdater.setFeedURL(DESKTOP_UPDATE_FEED);
    }
    // macOS 由下面的下载、校验与解包链拥有状态，不能被另一套 updater 事件提前置为就绪。
    if (process.platform === "darwin") return;
    autoUpdater.on("update-available", (info) => this.patch({
      status: "available",
      availableVersion: info.version,
      error: null,
    }));
    autoUpdater.on("update-not-available", () => this.patch({
      status: "idle",
      availableVersion: null,
      error: null,
    }));
    autoUpdater.on("download-progress", (progress) => this.patch({
      status: "downloading",
      downloadedBytes: progress.transferred,
      totalBytes: progress.total || null,
      percent: Number.isFinite(progress.percent) ? progress.percent : null,
      error: null,
    }));
    autoUpdater.on("update-downloaded", (info) => this.patch({
      status: "downloaded",
      availableVersion: info.version,
      error: null,
    }));
    autoUpdater.on("error", (error) => this.patch({
      status: "failed",
      error: error.message,
    }));
  }

  snapshot(): DesktopUpdateSnapshot {
    return { ...this.#snapshot };
  }

  async check(): Promise<DesktopUpdateSnapshot> {
    if (this.#checkPromise) return this.#checkPromise;
    if (this.#snapshot.status === "downloading" || this.#snapshot.status === "downloaded") {
      return this.snapshot();
    }
    if (!this.#updateSupported) {
      this.patch({ status: "unsupported", error: "当前为目录测试包，在线更新仅支持正式安装包" });
      return this.snapshot();
    }
    this.patch({
      status: "checking",
      availableVersion: null,
      downloadedBytes: 0,
      totalBytes: null,
      percent: null,
      error: null,
    });
    this.#checkPromise = (async () => {
      try {
        if (process.platform === "darwin") {
          const updateFile = await fetchMacUpdateFile();
          this.#macUpdateFile = updateFile;
          this.applyVersion(updateFile?.version ?? null);
        } else {
          const result = await autoUpdater.checkForUpdates();
          this.applyInfo(result?.updateInfo ?? null);
        }
        return this.snapshot();
      } catch (cause) {
        this.patch({ status: "failed", error: errorMessage(cause) });
        throw cause;
      }
    })().finally(() => {
      this.#checkPromise = null;
    });
    return this.#checkPromise;
  }

  async download(): Promise<DesktopUpdateSnapshot> {
    if (this.#downloadPromise) return this.#downloadPromise;
    if (this.#snapshot.status === "downloaded") {
      if (process.platform !== "darwin") return this.snapshot();
      try {
        await validateMacBundle(macStagedBundlePath(this.#snapshot.availableVersion!), this.#snapshot.availableVersion!);
        return this.snapshot();
      } catch {
        // 暂存文件被系统清理或损坏时，重试应重新准备更新包。
      }
      if (this.#downloadPromise) return this.#downloadPromise;
    }
    if (!this.#updateSupported) throw new Error("desktop_update_unsupported_for_directory_build");
    if (!this.#snapshot.availableVersion) throw new Error("desktop_update_not_available");
    if (process.platform === "darwin") {
      if (!this.#snapshot.installable) throw new Error("desktop_update_installation_required");
      if (!this.#macUpdateFile) throw new Error("desktop_update_metadata_missing");
    }
    const macUpdateFile = this.#macUpdateFile;
    this.patch({ status: "downloading", error: null, downloadedBytes: 0, totalBytes: null, percent: null });
    this.#downloadPromise = (async () => {
      try {
        if (process.platform === "darwin") {
          await downloadMacUpdate(macUpdateFile!, (downloadedBytes, totalBytes) => {
            this.patch({
              status: "downloading",
              downloadedBytes,
              totalBytes,
              percent: totalBytes ? Math.min(99, Math.round((downloadedBytes / totalBytes) * 100)) : null,
            });
          });
          this.patch({
            status: "downloaded",
            downloadedBytes: macUpdateFile!.size ?? this.#snapshot.downloadedBytes,
            totalBytes: macUpdateFile!.size,
            percent: 100,
            error: null,
          });
        } else {
          await autoUpdater.downloadUpdate();
        }
        return this.snapshot();
      } catch (cause) {
        this.patch({ status: "failed", error: errorMessage(cause) });
        throw cause;
      }
    })().finally(() => {
      this.#downloadPromise = null;
    });
    return this.#downloadPromise;
  }

  async install(prepareToQuit: () => Promise<void>): Promise<never> {
    if (this.#installPromise) return this.#installPromise;
    if (this.#snapshot.status !== "downloaded") throw new Error("desktop_update_not_downloaded");
    this.#installPromise = this.installDownloadedUpdate(prepareToQuit).catch((cause) => {
      this.#installPromise = null;
      // 安装助手尚未接管时保留已就绪包，允许重试；失败通过 IPC 返回给原调用方。
      this.patch({
        status: process.platform === "darwin" && this.#snapshot.status === "failed" ? "failed" : "downloaded",
        error: errorMessage(cause),
      });
      throw cause;
    });
    return this.#installPromise;
  }

  private async installDownloadedUpdate(prepareToQuit: () => Promise<void>): Promise<never> {
    if (process.platform === "darwin") {
      try {
        if (!existsSync(macStagedBundlePath(this.#snapshot.availableVersion!))) throw new Error("desktop_update_staging_missing");
        await validateMacBundle(macStagedBundlePath(this.#snapshot.availableVersion!), this.#snapshot.availableVersion!);
      } catch (cause) {
        this.patch({ status: "failed", error: errorMessage(cause) });
        throw cause;
      }
      await installUnsignedMacUpdate(this.#snapshot.availableVersion!, prepareToQuit);
      return new Promise<never>(() => undefined);
    }
    autoUpdater.quitAndInstall(false, true);
    if (this.#snapshot.status === "failed") throw new Error(this.#snapshot.error ?? "desktop_update_install_failed");
    return new Promise<never>((_resolve, reject) => autoUpdater.once("error", reject));
  }

  private applyInfo(info: UpdateInfo | null): void {
    this.applyVersion(info?.version ?? null);
  }

  private applyVersion(version: string | null): void {
    if (!version || version === this.#currentVersion) {
      this.#macUpdateFile = null;
      this.patch({ status: "idle", availableVersion: null, error: null });
      return;
    }
    this.patch({ status: "available", availableVersion: version, error: null });
  }

  private patch(patch: Partial<DesktopUpdateSnapshot>): void {
    this.#snapshot = { ...this.#snapshot, ...patch };
    this.#publish(this.snapshot());
  }
}

async function fetchMacUpdateFile(): Promise<MacUpdateFile | null> {
  const response = await fetch(MAC_UPDATE_METADATA_URL, {
    headers: { accept: "text/yaml" },
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error(`桌面更新清单请求失败: HTTP ${response.status}`);
  const metadata = parseYaml(await response.text()) as MacUpdateMetadata;
  if (!metadata || typeof metadata.version !== "string" || !/^\d+\.\d+\.\d+$/.test(metadata.version) || !Array.isArray(metadata.files)) {
    throw new Error("桌面更新清单格式无效");
  }
  const suffix = process.arch === "arm64" ? "-mac-arm64.zip" : "-mac-x64.zip";
  const file = metadata.files.find((candidate) => (
    typeof candidate.url === "string" && candidate.url.endsWith(suffix)
  ));
  if (!file || typeof file.url !== "string" || typeof file.sha512 !== "string") {
    throw new Error(`桌面更新清单缺少当前架构安装包: ${suffix}`);
  }
  return {
    version: metadata.version,
    url: `${MAC_UPDATE_DOWNLOAD_BASE_URL}/v${encodeURIComponent(metadata.version)}/${encodeURIComponent(file.url)}`,
    sha512: file.sha512,
    size: typeof file.size === "number" ? file.size : null,
  };
}

interface MacUpdateMetadata {
  version?: unknown;
  files?: Array<{ url?: unknown; sha512?: unknown; size?: unknown }>;
}

async function downloadMacUpdate(
  updateFile: MacUpdateFile,
  onProgress: (downloadedBytes: number, totalBytes: number | null) => void,
): Promise<void> {
  const updateRoot = join(app.getPath("userData"), "updates");
  await mkdir(updateRoot, { recursive: true });
  const zipPath = join(updateRoot, `Magi-${updateFile.version}-${process.arch}.zip`);
  // 下载以“连续无数据”的时间判超时；慢网持续传输不受总时长限制。
  // 保留部分 ZIP，重试通过 Range 续传；只有完整哈希通过后才解包。
  for (let attempt = 0; ; attempt++) {
    try {
      await transferMacUpdate(updateFile, zipPath, onProgress);
      break;
    } catch (cause) {
      if (attempt >= 2) throw cause;
    }
  }
  const digest = await createHashForFile(zipPath);
  if (digest !== updateFile.sha512) {
    await rm(zipPath, { force: true });
    throw new Error("桌面安装包校验失败，请重试下载");
  }

  const stagingRoot = join(updateRoot, `staged-${updateFile.version}-${process.pid}`);
  await rm(stagingRoot, { recursive: true, force: true });
  await mkdir(stagingRoot, { recursive: true });
  try {
    await execFileAsync("/usr/bin/ditto", ["-x", "-k", zipPath, stagingRoot]);
    await validateMacBundle(join(stagingRoot, "Magi.app"), updateFile.version);
    await rm(zipPath, { force: true });
  } catch (cause) {
    await rm(stagingRoot, { recursive: true, force: true });
    throw cause;
  }
}

async function transferMacUpdate(
  updateFile: MacUpdateFile,
  zipPath: string,
  onProgress: (downloadedBytes: number, totalBytes: number | null) => void,
): Promise<void> {
  let offset = await stat(zipPath).then((file) => file.size, (cause: NodeJS.ErrnoException) => {
    if (cause.code === "ENOENT") return 0;
    throw cause;
  });
  if (updateFile.size !== null && offset === updateFile.size) return;
  if (updateFile.size !== null && offset > updateFile.size) {
    await rm(zipPath, { force: true });
    offset = 0;
  }
  const controller = new AbortController();
  const idleError = new Error("更新下载连续 60 秒无响应，已保留下载进度，请重试");
  let idleTimer: ReturnType<typeof setTimeout>;
  const resetIdleTimeout = () => {
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => controller.abort(idleError), 60_000);
  };
  resetIdleTimeout();
  try {
    const response = await fetch(updateFile.url, {
      headers: offset > 0 ? { Range: `bytes=${offset}-` } : {},
      signal: controller.signal,
    });
    if (!response.ok || !response.body) throw new Error(`桌面安装包下载失败: HTTP ${response.status}`);
    let totalBytes = updateFile.size;
    if (response.status === 206) {
      const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get("content-range") ?? "");
      if (!range || Number(range[1]) !== offset
        || (totalBytes !== null && Number(range[3]) !== totalBytes)) {
        throw new Error("更新服务器返回的续传范围无效，请重试");
      }
      totalBytes ??= Number(range[3]);
    } else {
      // 不支持 Range 的服务器会返回整个文件，必须覆盖，不能追加成损坏 ZIP。
      offset = 0;
      totalBytes ??= Number(response.headers.get("content-length")) || null;
    }
    const file = await open(zipPath, offset > 0 ? "a" : "w");
    try {
      let downloadedBytes = offset;
      onProgress(downloadedBytes, totalBytes);
      const reader = response.body.getReader();
      while (true) {
        const next = await reader.read();
        if (next.done) break;
        if (totalBytes !== null && downloadedBytes + next.value.byteLength > totalBytes) {
          await file.truncate(0);
          throw new Error("桌面安装包大小超过更新清单，请重试");
        }
        await file.writeFile(next.value);
        downloadedBytes += next.value.byteLength;
        resetIdleTimeout();
        onProgress(downloadedBytes, totalBytes);
      }
      if (totalBytes !== null && downloadedBytes !== totalBytes) {
        throw new Error(`桌面安装包下载不完整（${downloadedBytes}/${totalBytes} 字节），已保留进度，请重试`);
      }
      await file.sync();
    } finally {
      await file.close();
    }
  } finally {
    clearTimeout(idleTimer!);
    controller.abort();
  }
}

async function validateMacBundle(bundle: string, version: string): Promise<void> {
  const plist = join(bundle, "Contents", "Info.plist");
  const { stdout: actualVersion } = await execFileAsync("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleShortVersionString", plist]);
  const { stdout: executable } = await execFileAsync("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleExecutable", plist]);
  const executableName = executable.trim();
  if (actualVersion.trim() !== version || !executableName || /[/\\]/.test(executableName)) {
    throw new Error("桌面安装包版本或入口无效，请重新下载");
  }
  accessSync(join(bundle, "Contents", "MacOS", executableName), constants.X_OK);
}

async function createHashForFile(path: string): Promise<string> {
  const digest = createHash("sha512");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest("base64");
}

function canInstallUnsignedMacUpdate(): boolean {
  if (process.platform !== "darwin" || !app.isPackaged) return true;
  const bundlePath = macAppBundlePath();
  if (bundlePath.startsWith("/Volumes/")) return false;
  try {
    accessSync(dirname(bundlePath), constants.W_OK);
    return true;
  } catch {
    return false;
  }
}

function macAppBundlePath(): string {
  return resolve(process.resourcesPath, "..", "..");
}

async function installUnsignedMacUpdate(version: string, prepareToQuit: () => Promise<void>): Promise<void> {
  const bundlePath = macAppBundlePath();
  const stagedBundlePath = macStagedBundlePath(version);
  if (!existsSync(stagedBundlePath)) throw new Error("desktop_update_staging_missing");
  if (!canInstallUnsignedMacUpdate()) throw new Error("desktop_update_installation_required");

  const backupPath = `${bundlePath}.previous-${process.pid}`;
  const script = `
set -eu
pid="$1"
current="$2"
staged="$3"
backup="$4"
for _ in $(seq 1 600); do
  state="$(ps -p "$pid" -o state= 2>/dev/null | tr -d ' ')"
  if [ -z "$state" ] || [ "$state" = "Z" ]; then break; fi
  sleep 0.1
done
state="$(ps -p "$pid" -o state= 2>/dev/null | tr -d ' ')"
[ -z "$state" ] || [ "$state" = "Z" ] || exit 1
if ! mv "$current" "$backup"; then exit 1; fi
if ! mv "$staged" "$current"; then mv "$backup" "$current"; exit 1; fi
# 旧包内可能有只读目录；先重新打开新版本，清理失败不能阻断重启。
if ! /usr/bin/open -n "$current"; then
  mv "$current" "$staged"
  mv "$backup" "$current"
  /usr/bin/open -n "$current" || true
  exit 1
fi
rm -rf "$backup" || true
`;
  const log = openSync(join(app.getPath("userData"), "updates", "install.log"), "a", 0o600);
  try {
    const child = spawn("/bin/sh", ["-c", script, "magi-unsigned-update", String(process.pid), bundlePath, stagedBundlePath, backupPath], {
      detached: true,
      stdio: ["ignore", log, log],
    });
    // 助手成功启动后才退出，启动失败仍可在当前界面显示错误。
    await once(child, "spawn");
    child.unref();
  } finally {
    closeSync(log);
  }
  // 更新助手已接管替换；先复用 Main 的关闭事务结算 daemon/Worker，
  // 再退出主进程，避免新实例与遗留 daemon 争用端口和状态锁。
  try {
    await prepareToQuit();
  } finally {
    app.exit(0);
  }
}

function macStagedBundlePath(version: string): string {
  return join(app.getPath("userData"), "updates", `staged-${version}-${process.pid}`, "Magi.app");
}

function readDistributionKind(): "directory" | "release" | null {
  try {
    const packageJson = JSON.parse(
      readFileSync(join(app.getAppPath(), "package.json"), "utf8"),
    ) as { magiDistribution?: unknown };
    return packageJson.magiDistribution === "directory"
      ? "directory"
      : packageJson.magiDistribution === "release"
        ? "release"
        : null;
  } catch {
    return null;
  }
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}
