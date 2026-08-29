import { lstat, mkdir, readdir, rm } from "node:fs/promises";
import { isAbsolute, join } from "node:path";

export const BROWSER_DOWNLOAD_DIRECTORY = "browser-downloads";

export function browserDownloadRoot(userDataPath: string): string {
  const normalizedPath = userDataPath.trim();
  if (!normalizedPath || !isAbsolute(normalizedPath)) {
    throw new Error("browser_download_user_data_path_invalid");
  }
  return join(normalizedPath, BROWSER_DOWNLOAD_DIRECTORY);
}

export async function clearBrowserDownloads(userDataPath: string): Promise<void> {
  const root = browserDownloadRoot(userDataPath);
  let rootStat;
  try {
    rootStat = await lstat(root);
  } catch (error) {
    if (isFileNotFound(error)) {
      await mkdir(root, { recursive: true });
      return;
    }
    throw error;
  }
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) {
    throw new Error("browser_download_root_invalid");
  }

  const entries = await readdir(root);
  await Promise.all(entries.map((entry) => rm(join(root, entry), {
    force: true,
    recursive: true,
  })));
}

function isFileNotFound(error: unknown): boolean {
  return Boolean(
    error
    && typeof error === "object"
    && "code" in error
    && error.code === "ENOENT",
  );
}
