import { homedir } from "node:os";
import { join } from "node:path";

const MAGI_STATE_DIRECTORY = ".magi";

/**
 * 返回 Desktop 与独立 daemon 共用的唯一应用状态根。
 *
 * `~/.magi` 是当前产品已经使用的全局状态事实源，开发 daemon、正式
 * Electron 和历史客户端必须解析到同一个目录。workspace 自身的状态仍位于
 * 项目目录下的 `.magi` 子目录，不能把两者混成同一份 workspace 状态。
 */
export function defaultStateRoot(homeDirectory = homedir()): string {
  const base = homeDirectory.trim();
  if (!base) throw new Error("home_directory_missing");
  return join(base, MAGI_STATE_DIRECTORY);
}
