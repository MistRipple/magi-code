import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import {
  buildDaemon,
  buildDesktopJavaScript,
  cleanDesktopOutputs,
  repositoryRoot,
} from "./build-support.mjs";

await cleanDesktopOutputs();
await Promise.all([
  buildDaemon("debug"),
  buildDesktopJavaScript({ development: true }),
]);

const electronRoot = join(repositoryRoot, "node_modules", "electron");
const executable = join(
  electronRoot,
  "dist",
  (await readFile(join(electronRoot, "path.txt"), "utf8")).trim(),
);
const child = spawn(executable, [join(repositoryRoot, "apps", "desktop")], {
  cwd: repositoryRoot,
  env: {
    ...process.env,
    NODE_ENV: "development",
    // 开发 daemon 由 scripts/dev-daemon.sh 托管；Electron 只连接它，
    // 避免桌面宿主再启动第二个实例争抢端口。MAGI_PORT 是 dev-daemon.sh
    // 的入口配置；若调用方没有单独覆盖 Desktop 端口，必须把同一端口传给 Main。
    MAGI_DESKTOP_REUSE_DAEMON: "1",
    MAGI_DESKTOP_DAEMON_PORT:
      process.env.MAGI_DESKTOP_DAEMON_PORT?.trim()
      || process.env.MAGI_PORT?.trim()
      || "38123",
  },
  stdio: "inherit",
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => child.kill(signal));
}
child.once("error", (error) => {
  throw error;
});
child.once("exit", (code, signal) => {
  process.exitCode = code ?? (signal ? 1 : 0);
});
