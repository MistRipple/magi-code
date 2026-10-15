import assert from "node:assert/strict";
import { EventEmitter, once } from "node:events";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, chmodSync, existsSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { runInNewContext } from "node:vm";
import { transformSync } from "esbuild";
import test from "node:test";
import type { UpdateManager } from "./update-manager.js";

const require = createRequire(import.meta.url);
const source = readFileSync(new URL("./update-manager.ts", import.meta.url), "utf8");
const compiled = transformSync(source, { loader: "ts", format: "cjs", target: "node22" }).code;
const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;

function fixture(spawnHelper: typeof spawn = spawn) {
  const root = mkdtempSync(join(tmpdir(), "magi-update-"));
  const current = join(root, "Magi's installed copy.app");
  const resources = join(current, "Contents", "Resources");
  const updates = join(root, "updates");
  mkdirSync(resources, { recursive: true });
  mkdirSync(updates);
  writeFileSync(join(resources, "app-update.yml"), "provider: github\n");
  writeFileSync(join(root, "package.json"), '{"magiDistribution":"release"}');
  const updater = new EventEmitter();
  Object.assign(updater, { setFeedURL() {} });
  const events = new EventEmitter();
  let exited = false;
  const module = { exports: {} as { UpdateManager: typeof UpdateManager } };
  const fakeProcess = { ...process, platform: "darwin", resourcesPath: resources, pid: 99999999 };
  runInNewContext(`(function(require, module, exports, process) { ${compiled}\n})`, {})(
    (name: string) => {
      if (name === "electron") return { app: {
        isPackaged: true, getAppPath: () => root, getPath: () => root,
        exit: () => { exited = true; events.emit("exit"); },
      } };
      if (name === "electron-updater") return { autoUpdater: updater };
      if (name === "node:child_process") return { ...require(name), spawn: spawnHelper };
      return require(name);
    }, module, module.exports, fakeProcess,
  );
  const manager = new module.exports.UpdateManager("3.0.55", () => undefined);
  updater.emit("update-downloaded", { version: "3.0.56" });
  const staged = join(updates, `staged-3.0.56-${fakeProcess.pid}`, "Magi.app");
  mkdirSync(staged, { recursive: true });
  return { root, current, staged, manager, events, exited: () => exited, cleanup: () => rmSync(root, { recursive: true, force: true }) };
}

test("更新助手启动失败保留应用并显示失败事实", async () => {
  const context = fixture((() => {
    const child = new EventEmitter() as ChildProcess;
    queueMicrotask(() => child.emit("error", new Error("helper spawn failed")));
    return child;
  }) as typeof spawn);
  try {
    let prepared = false;
    await assert.rejects(context.manager.install(async () => { prepared = true; }), /helper spawn failed/);
    assert.equal(prepared, false, "助手启动失败不应提前关闭应用资源");
    assert.equal(context.exited(), false);
    assert.equal(context.manager.snapshot().status, "failed");
    assert.match(context.manager.snapshot().error ?? "", /helper spawn failed/);
    assert.ok(existsSync(context.current));
  } finally {
    context.cleanup();
  }
});

function appBundle(path: string, marker: string, content: string): void {
  const executable = join(path, "Contents", "MacOS", "probe");
  mkdirSync(join(path, "Contents", "MacOS"), { recursive: true });
  writeFileSync(join(path, "Contents", "Info.plist"), `<?xml version="1.0"?><plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>probe</string><key>CFBundleIdentifier</key><string>test.magi.update.${Date.now()}</string>
<key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/></dict></plist>`);
  writeFileSync(executable, `#!/bin/sh\nprintf '%s' ${quote(content)} > ${quote(marker)}\n`);
  chmodSync(executable, 0o755);
}

async function waitUntil(probe: () => boolean): Promise<void> {
  const deadline = Date.now() + 15_000;
  while (!probe()) {
    assert.ok(Date.now() < deadline, "更新助手应完成替换与重新打开");
    await sleep(100);
  }
}

test("macOS 旧包清理失败仍替换并通过 LaunchServices 重新打开新应用", { skip: process.platform !== "darwin" }, async () => {
  let helper: ChildProcess | undefined;
  const context = fixture(((command, args, options) => {
    helper = spawn(command, args ?? [], { ...options, env: { ...process.env, PATH: `${join(context.root, "bin")}:${process.env.PATH}` } });
    return helper;
  }) as typeof spawn);
  try {
    const marker = join(context.root, "reopened");
    appBundle(context.current, marker, "old");
    appBundle(context.staged, marker, "new");
    // 重现旧包清理报错；不改变系统命令或测试用户实际安装的 Magi。
    mkdirSync(join(context.root, "bin"));
    const rm = join(context.root, "bin", "rm");
    writeFileSync(rm, "#!/bin/sh\necho cleanup-denied >&2\nexit 1\n");
    chmodSync(rm, 0o755);
    const exit = once(context.events, "exit");
    let prepared = false;
    context.events.once("exit", () => assert.equal(prepared, true, "必须先结算宿主资源再退出"));
    const installing = context.manager.install(async () => { await sleep(20); prepared = true; });
    await Promise.race([exit, installing]);
    assert.equal(context.exited(), true);
    await waitUntil(() => existsSync(marker));
    assert.equal(readFileSync(marker, "utf8"), "new");
    if (helper?.exitCode === null) await once(helper, "exit");
    assert.equal(helper?.exitCode, 0, readFileSync(join(context.root, "updates", "install.log"), "utf8"));
    assert.ok(existsSync(`${context.current}.previous-99999999`));
    assert.match(readFileSync(join(context.root, "updates", "install.log"), "utf8"), /cleanup-denied/);
  } finally {
    if (helper?.exitCode === null) helper.kill();
    context.cleanup();
  }
});
