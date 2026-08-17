import { app } from "electron";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { autoUpdater, type UpdateInfo } from "electron-updater";

const DESKTOP_UPDATE_FEED = {
  provider: "github",
  owner: "MistRipple",
  repo: "magi-code",
  releaseType: "release",
} as const;

export interface DesktopUpdateSnapshot {
  status: "idle" | "checking" | "available" | "downloading" | "downloaded" | "failed" | "unsupported";
  currentVersion: string;
  availableVersion: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
  error: string | null;
}

export class UpdateManager {
  readonly #currentVersion: string;
  readonly #publish: (snapshot: DesktopUpdateSnapshot) => void;
  readonly #updateSupported: boolean;
  #snapshot: DesktopUpdateSnapshot;
  #checkPromise: Promise<DesktopUpdateSnapshot> | null = null;
  #downloadPromise: Promise<DesktopUpdateSnapshot> | null = null;

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
        const result = await autoUpdater.checkForUpdates();
        this.applyInfo(result?.updateInfo ?? null);
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
    this.patch({ status: "downloading", error: null, downloadedBytes: 0, totalBytes: null, percent: null });
    this.#downloadPromise = (async () => {
      try {
        await autoUpdater.downloadUpdate();
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
    autoUpdater.quitAndInstall(false, true);
    throw new Error("desktop_update_install_did_not_exit");
  }

  private applyInfo(info: UpdateInfo | null): void {
    if (!info || info.version === this.#currentVersion) {
      this.patch({ status: "idle", availableVersion: null, error: null });
      return;
    }
    this.patch({ status: "available", availableVersion: info.version, error: null });
  }

  private patch(patch: Partial<DesktopUpdateSnapshot>): void {
    this.#snapshot = { ...this.#snapshot, ...patch };
    this.#publish(this.snapshot());
  }
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
