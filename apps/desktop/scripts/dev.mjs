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
    // 避免桌面宿主再启动第二个实例争抢 38123 端口。
    MAGI_DESKTOP_REUSE_DAEMON: "1",
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
