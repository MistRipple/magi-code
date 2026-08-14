import { realpath, stat } from "node:fs/promises";
import { isAbsolute, relative, sep } from "node:path";
import { shell } from "electron";

const UNIX_PATH_REF_PREFIX = "mhp1:u:";
const WINDOWS_PATH_REF_PREFIX = "mhp1:w:";

export async function openWorkspaceFolder(workspaceRootPathRef: string): Promise<void> {
  const workspaceRoot = await canonicalPath(workspaceRootPathRef, "directory");
  const error = await shell.openPath(workspaceRoot);
  if (error) throw new Error(`workspace_folder_open_failed:${error}`);
}

export async function revealWorkspaceFile(input: {
  targetPathRef: string;
  workspaceRootPathRef: string;
}): Promise<void> {
  const [workspaceRoot, targetPath] = await Promise.all([
    canonicalPath(input.workspaceRootPathRef, "directory"),
    canonicalPath(input.targetPathRef, "file"),
  ]);
  const relativePath = relative(workspaceRoot, targetPath);
  if (
    !relativePath
    || relativePath === ".."
    || relativePath.startsWith(`..${sep}`)
    || isAbsolute(relativePath)
  ) {
    throw new Error("workspace_file_outside_root");
  }
  shell.showItemInFolder(targetPath);
}

async function canonicalPath(pathRef: string, expected: "file" | "directory"): Promise<string> {
  const decoded = decodeHostPathRef(pathRef);
  const canonical = await realpath(decoded);
  const metadata = await stat(canonical);
  if (expected === "file" ? !metadata.isFile() : !metadata.isDirectory()) {
    throw new Error(`host_path_not_${expected}`);
  }
  return canonical;
}

function decodeHostPathRef(value: string): string {
  const pathRef = value.trim();
  const expectedPrefix = process.platform === "win32"
    ? WINDOWS_PATH_REF_PREFIX
    : UNIX_PATH_REF_PREFIX;
  if (!pathRef.startsWith(expectedPrefix)) throw new Error("host_path_ref_invalid");

  const payload = pathRef.slice(expectedPrefix.length);
  if (!payload) throw new Error("host_path_ref_invalid");
  let bytes: Buffer;
  try {
    bytes = Buffer.from(payload, "base64url");
  } catch {
    throw new Error("host_path_ref_invalid");
  }
  if (bytes.length === 0) throw new Error("host_path_ref_invalid");

  const decoded = process.platform === "win32"
    ? decodeWindowsPath(bytes)
    : new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  if (!decoded || decoded.includes("\0") || !isAbsolute(decoded)) {
    throw new Error("host_path_ref_invalid");
  }
  return decoded;
}

function decodeWindowsPath(bytes: Buffer): string {
  if (bytes.length % 2 !== 0) throw new Error("host_path_ref_invalid");
  return bytes.toString("utf16le");
}
