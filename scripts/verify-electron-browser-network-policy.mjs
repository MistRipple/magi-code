import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createRequire } from "node:module";
import { build } from "esbuild";

/**
 * 浏览器网络边界的真实 Chromium 验收：用开发依赖里的 Electron 启动一个不显示窗口的最小宿主，
 * 加载 Main 实际使用的求值器，检查 iframe / fetch / 重定向都被 session 层拦截。
 * 不依赖打包产物，也不会触碰用户的 Magi 状态目录。
 */

const root = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const electronPath = createRequire(import.meta.url)("electron");
const workdir = await mkdtemp(join(tmpdir(), "magi-browser-network-policy-"));

try {
  const bundle = join(workdir, "policy.cjs");
  await build({
    entryPoints: [join(root, "apps/desktop/src/main/browser-network-policy.ts")],
    bundle: true,
    platform: "node",
    format: "cjs",
    outfile: bundle,
    logLevel: "silent",
  });
  const child = spawn(electronPath, [join(root, "scripts/electron-browser-network-policy-main.cjs")], {
    env: { ...process.env, MAGI_NETWORK_POLICY_BUNDLE: bundle },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  child.stdout.on("data", (chunk) => { output += chunk; });
  child.stderr.on("data", () => {});
  const timer = setTimeout(() => child.kill("SIGKILL"), 60_000);
  const code = await new Promise((resolve) => child.on("exit", resolve));
  clearTimeout(timer);
  const lines = output.split("\n").filter((line) => /^(PASS|FAIL|ALL PASSED|\d+ FAILED)/u.test(line));
  process.stdout.write(`${lines.join("\n")}\n`);
  if (code !== 0) {
    process.stderr.write(`浏览器网络边界验收失败（退出码 ${code}）\n`);
    process.exitCode = 1;
  }
} finally {
  await rm(workdir, { recursive: true, force: true });
}
