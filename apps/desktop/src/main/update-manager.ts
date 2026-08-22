import { app } from "electron";
import { createHash } from "node:crypto";
import { accessSync, constants, createReadStream, existsSync, readFileSync } from "node:fs";
import { mkdir, open, rm } from "node:fs/promises";
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
    if (!this.#updateSupported) throw new Error("desktop_update_unsupported_for_directory_build");
    if (!this.#snapshot.availableVersion) throw new Error("desktop_update_not_available");
    if (process.platform === "darwin") {
      if (!this.#snapshot.installable) throw new Error("desktop_update_installation_required");
      if (!this.#macUpdateFile) throw new Error("desktop_update_metadata_missing");
    }
    this.patch({ status: "downloading", error: null, downloadedBytes: 0, totalBytes: null, percent: null });
    this.#downloadPromise = (async () => {
      try {
        if (process.platform === "darwin") {
          await downloadMacUpdate(this.#macUpdateFile!, (downloadedBytes, totalBytes) => {
            this.patch({
              status: "downloading",
              downloadedBytes,
              totalBytes,
              percent: totalBytes ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100)) : null,
            });
          });
          this.patch({
            status: "downloaded",
            downloadedBytes: this.#macUpdateFile!.size ?? this.#snapshot.downloadedBytes,
            totalBytes: this.#macUpdateFile!.size,
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

  install(): never {
    if (this.#snapshot.status !== "downloaded") throw new Error("desktop_update_not_downloaded");
    if (process.platform === "darwin") {
      installUnsignedMacUpdate(this.#snapshot.availableVersion!);
      throw new Error("desktop_update_install_did_not_exit");
    }
    autoUpdater.quitAndInstall(false, true);
    throw new Error("desktop_update_install_did_not_exit");
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
  if (!metadata || typeof metadata.version !== "string" || !Array.isArray(metadata.files)) {
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
  const response = await fetch(updateFile.url, {
    signal: AbortSignal.timeout(10 * 60_000),
  });
  if (!response.ok || !response.body) throw new Error(`桌面安装包下载失败: HTTP ${response.status}`);

  const totalBytes = Number(response.headers.get("content-length")) || updateFile.size;
  let downloadedBytes = 0;
  const file = await open(zipPath, "w");
  try {
    const reader = response.body.getReader();
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      await file.write(next.value);
      downloadedBytes += next.value.byteLength;
      onProgress(downloadedBytes, totalBytes);
    }
  } finally {
    await file.close();
  }

  const digest = await createHashForFile(zipPath);
  if (digest !== updateFile.sha512) {
    await rm(zipPath, { force: true });
    throw new Error("桌面安装包校验失败，文件可能已损坏或被篡改");
  }

  const stagingRoot = join(updateRoot, `staged-${updateFile.version}-${process.pid}`);
  await rm(stagingRoot, { recursive: true, force: true });
  await mkdir(stagingRoot, { recursive: true });
  await execFileAsync("/usr/bin/ditto", ["-x", "-k", zipPath, stagingRoot]);
  await rm(zipPath, { force: true });
  if (!existsSync(join(stagingRoot, "Magi.app"))) {
    await rm(stagingRoot, { recursive: true, force: true });
    throw new Error("桌面安装包缺少 Magi.app");
  }
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

function installUnsignedMacUpdate(version: string): void {
  const bundlePath = macAppBundlePath();
  const stagingRoot = join(app.getPath("userData"), "updates", `staged-${version}-${process.pid}`);
  const stagedBundlePath = join(stagingRoot, "Magi.app");
  if (!existsSync(stagedBundlePath)) throw new Error("desktop_update_staging_missing");
  if (!canInstallUnsignedMacUpdate()) throw new Error("desktop_update_installation_required");

  const backupPath = `${bundlePath}.previous-${process.pid}`;
  const script = `
set -eu
pid="$1"
current="$2"
staged="$3"
backup="$4"
while kill -0 "$pid" 2>/dev/null; do sleep 0.25; done
if ! mv "$current" "$backup"; then exit 1; fi
if ! mv "$staged" "$current"; then mv "$backup" "$current"; exit 1; fi
rm -rf "$backup"
/usr/bin/open "$current"
`;
  const child = spawn("/bin/sh", ["-c", script, "magi-unsigned-update", String(process.pid), bundlePath, stagedBundlePath, backupPath], {
    detached: true,
    stdio: "ignore",
  });
  child.unref();
  app.quit();
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
