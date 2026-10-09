import { execFile } from "node:child_process";
import { access, stat } from "node:fs/promises";
import { constants } from "node:fs";
import { join } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
export const pluginWorkerFileName = `magi-plugin-worker${process.platform === "win32" ? ".exe" : ""}`;

export async function assertPluginWorker(root) {
  const path = join(root, pluginWorkerFileName);
  const metadata = await stat(path);
  if (!metadata.isFile()) throw new Error(`插件 Worker 不是文件: ${path}`);
  await access(path, constants.X_OK);
  if (process.platform !== "win32" && (metadata.mode & 0o111) === 0) {
    throw new Error(`插件 Worker 缺少执行权限: ${path}`);
  }
  return path;
}

export async function runPluginWorkerPreflight(root, productVersion) {
  const path = await assertPluginWorker(root);
  // 引擎与依赖必须来自产品制品；清空环境，防止本地 PATH 或开发工具掩盖漏打包。
  const { stdout } = await execFileAsync(path, ["--preflight"], {
    env: {}, windowsHide: true, timeout: 15_000, maxBuffer: 4096,
  });
  const result = JSON.parse(stdout);
  if (result.ok !== true || result.workerVersion !== productVersion) {
    throw new Error(`插件 Worker 版本或实际执行校验失败: ${path}`);
  }
}
