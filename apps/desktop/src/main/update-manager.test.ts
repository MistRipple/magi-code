import assert from "node:assert/strict";
import { EventEmitter, once } from "node:events";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, chmodSync, existsSync, rmSync } from "node:fs";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { runInNewContext } from "node:vm";
import { promisify } from "node:util";
import { transformSync } from "esbuild";
import test from "node:test";
import type { UpdateManager } from "./update-manager.js";

const require = createRequire(import.meta.url);
const source = readFileSync(new URL("./update-manager.ts", import.meta.url), "utf8");
const compiled = transformSync(source, { loader: "ts", format: "cjs", target: "node24" }).code;
const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;

function fixture(spawnHelper: typeof spawn = spawn, options: {
  fetch?: typeof fetch;
  execFile?: typeof execFile;
  platform?: string;
  setTimeout?: typeof setTimeout;
} = {}) {
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
  const fakeProcess = { ...process, platform: options.platform ?? "darwin", resourcesPath: resources, pid: 99999999 };
  const payload = Buffer.from("verified update archive");
  const metadata = JSON.stringify({ version: "3.0.56", files: [{
    url: `Magi-3.0.56-mac-${process.arch === "arm64" ? "arm64" : "x64"}.zip`,
    sha512: createHash("sha512").update(payload).digest("base64"), size: payload.length,
  }] });
  const staged = join(updates, `staged-3.0.56-${fakeProcess.pid}`, "Magi.app");
  const fakeExecFile = ((command: string, args: string[], callback: (...args: unknown[]) => void) => {
    queueMicrotask(() => {
      if (command === "/usr/bin/ditto") {
        appBundle(staged, join(root, "reopened"), "new");
        callback(null, "", "");
      } else if (command === "/usr/libexec/PlistBuddy") {
        callback(null, args[1]?.includes("CFBundleExecutable") ? "probe" : "3.0.56", "");
      } else callback(new Error(`unexpected command ${command}`));
    });
  }) as unknown as typeof execFile;
  Object.assign(fakeExecFile, { [promisify.custom]: (command: string, args: string[]) => new Promise((resolve, reject) => {
    fakeExecFile(command, args, (error, stdout, stderr) => error ? reject(error) : resolve({ stdout, stderr }));
  }) });
  runInNewContext(`(function(require, module, exports, process) { ${compiled}\n})`, {
    fetch: options.fetch ?? (async (url: string) => new Response(url.endsWith("latest-mac.yml") ? metadata : payload)),
    AbortSignal, AbortController, setTimeout: options.setTimeout ?? setTimeout, clearTimeout,
  })(
    (name: string) => {
      if (name === "electron") return { app: {
        isPackaged: true, getAppPath: () => root, getPath: () => root,
        exit: () => { exited = true; events.emit("exit"); },
      } };
      if (name === "electron-updater") return { autoUpdater: updater };
      if (name === "node:child_process") return { ...require(name), spawn: spawnHelper, execFile: options.execFile ?? fakeExecFile };
      return require(name);
    }, module, module.exports, fakeProcess,
  );
  const manager = new module.exports.UpdateManager("3.0.55", () => undefined);
  const ready = async () => { await manager.check(); await manager.download(); };
  return { root, current, staged, updates, payload, metadata, updater, manager, events, ready, exited: () => exited, cleanup: () => rmSync(root, { recursive: true, force: true }) };
}

test("更新助手启动失败保留应用并显示失败事实", async () => {
  const context = fixture((() => {
    const child = new EventEmitter() as ChildProcess;
    queueMicrotask(() => child.emit("error", new Error("helper spawn failed")));
    return child;
  }) as typeof spawn);
  try {
    await context.ready();
    let prepared = false;
    await assert.rejects(context.manager.install(async () => { prepared = true; }), /helper spawn failed/);
    assert.equal(prepared, false, "助手启动失败不应提前关闭应用资源");
    assert.equal(context.exited(), false);
    assert.equal(context.manager.snapshot().status, "downloaded");
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
<key>CFBundleShortVersionString</key><string>3.0.56</string><key>CFBundleExecutable</key><string>probe</string><key>CFBundleIdentifier</key><string>test.magi.update.${Date.now()}</string>
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
    await context.ready();
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

test("macOS 不接受 electron-updater 的提前就绪事件", async () => {
  const context = fixture();
  try {
    context.updater.emit("update-downloaded", { version: "3.0.56" });
    assert.equal(context.manager.snapshot().status, "idle");
    await assert.rejects(context.manager.install(async () => undefined), /not_downloaded/);
    await context.ready();
    assert.equal(context.manager.snapshot().status, "downloaded");
  } finally { context.cleanup(); }
});

test("下载中断自动续传；重试只从已落盘位置追加，校验后才就绪", async () => {
  const offsets: number[] = [];
  let calls = 0;
  const context = fixture(spawn, { fetch: (async (url: string, init?: RequestInit) => {
    if (url.endsWith("latest-mac.yml")) return new Response(context.metadata);
    calls++;
    const offset = Number(new Headers(init?.headers).get("Range")?.match(/bytes=(\d+)-/)?.[1] ?? 0);
    offsets.push(offset);
    if (calls === 1) return new Response(new ReadableStream({
      start(controller) { controller.enqueue(context.payload.subarray(0, 7)); },
      pull(controller) { controller.error(new Error("connection reset")); },
    }));
    return new Response(context.payload.subarray(offset), {
      status: 206, headers: { "content-range": `bytes ${offset}-${context.payload.length - 1}/${context.payload.length}` },
    });
  }) as typeof fetch });
  try {
    await context.ready();
    assert.deepEqual(offsets, [0, 7]);
    assert.equal(context.manager.snapshot().status, "downloaded");
    assert.equal(existsSync(join(context.updates, `Magi-3.0.56-${process.arch}.zip`)), false);
  } finally { context.cleanup(); }
});

test("旧版残留半包遇到不支持 Range 的服务器会覆盖重下", async () => {
  let requestedRange: string | null = null;
  const context = fixture(spawn, { fetch: (async (url: string, init?: RequestInit) => {
    if (url.endsWith("latest-mac.yml")) return new Response(context.metadata);
    requestedRange = new Headers(init?.headers).get("Range");
    return new Response(context.payload);
  }) as typeof fetch });
  try {
    writeFileSync(join(context.updates, `Magi-3.0.56-${process.arch}.zip`), context.payload.subarray(0, 4));
    await context.ready();
    assert.equal(requestedRange, "bytes=4-");
    assert.equal(context.manager.snapshot().status, "downloaded");
  } finally { context.cleanup(); }
});

test("断流重试失败保留半包，手动重试继续下载", async () => {
  let disconnected = true;
  const offsets: number[] = [];
  const context = fixture(spawn, { fetch: (async (url: string, init?: RequestInit) => {
    if (url.endsWith("latest-mac.yml")) return new Response(context.metadata);
    const offset = Number(new Headers(init?.headers).get("Range")?.match(/bytes=(\d+)-/)?.[1] ?? 0);
    offsets.push(offset);
    if (disconnected) {
      if (offset) throw new Error("network unavailable");
      return new Response(context.payload.subarray(0, 5));
    }
    return new Response(context.payload.subarray(offset), {
      status: 206, headers: { "content-range": `bytes ${offset}-${context.payload.length - 1}/${context.payload.length}` },
    });
  }) as typeof fetch });
  try {
    await context.manager.check();
    await assert.rejects(context.manager.download(), /network unavailable/);
    assert.equal(context.manager.snapshot().status, "failed");
    assert.equal(context.exited(), false);
    assert.equal(existsSync(context.staged), false);
    disconnected = false;
    await context.manager.download();
    assert.deepEqual(offsets, [0, 5, 5, 5]);
    assert.equal(context.manager.snapshot().status, "downloaded");
  } finally { context.cleanup(); }
});

test("完整但哈希不匹配的包不解包也不允许安装", async () => {
  const context = fixture(spawn, { fetch: (async (url: string) => new Response(
    url.endsWith("latest-mac.yml") ? context.metadata : Buffer.alloc(context.payload.length),
  )) as typeof fetch });
  try {
    await context.manager.check();
    await assert.rejects(context.manager.download(), /校验失败/);
    assert.equal(context.manager.snapshot().status, "failed");
    assert.equal(existsSync(context.staged), false);
    assert.equal(context.exited(), false);
    await assert.rejects(context.manager.install(async () => undefined), /not_downloaded/);
  } finally { context.cleanup(); }
});

test("解包失败清理半成品并保留已校验 ZIP 供重试", async () => {
  let failExtraction = true;
  const context = fixture(spawn, { execFile: Object.assign(
    (() => undefined) as unknown as typeof execFile,
    { [promisify.custom]: async (command: string, args: string[]) => {
      if (command === "/usr/bin/ditto") {
        mkdirSync(context.staged, { recursive: true });
        if (failExtraction) throw new Error("ditto failed");
        appBundle(context.staged, join(context.root, "reopened"), "new");
        return { stdout: "" };
      }
      return { stdout: args[1]?.includes("CFBundleExecutable") ? "probe" : "3.0.56" };
    } },
  ) });
  try {
    await context.manager.check();
    await assert.rejects(context.manager.download(), /ditto failed/);
    assert.equal(existsSync(context.staged), false);
    assert.equal(existsSync(join(context.updates, `Magi-3.0.56-${process.arch}.zip`)), true);
    failExtraction = false;
    await context.manager.download();
    assert.equal(context.manager.snapshot().status, "downloaded");
  } finally { context.cleanup(); }
});

test("安装助手启动失败后可重试，重复点击不启动多个助手", async () => {
  let attempts = 0;
  const context = fixture((() => {
    attempts++;
    const child = new EventEmitter() as ChildProcess;
    queueMicrotask(() => child.emit("error", new Error("helper spawn failed")));
    return child;
  }) as typeof spawn);
  try {
    await context.ready();
    const first = context.manager.install(async () => undefined);
    const second = context.manager.install(async () => undefined);
    await Promise.all([assert.rejects(first, /helper spawn failed/), assert.rejects(second, /helper spawn failed/)]);
    assert.equal(attempts, 1);
    await assert.rejects(context.manager.install(async () => undefined), /helper spawn failed/);
    assert.equal(attempts, 2);
    assert.equal(context.exited(), false);
  } finally { context.cleanup(); }
});

test("慢网持续有数据时按空闲时间续期；真正停滞才取消并有限重试", async () => {
  const delays: number[] = [];
  const context = fixture(spawn, { setTimeout: ((callback: () => void, ms: number) => {
    delays.push(ms);
    return setTimeout(callback, ms);
  }) as typeof setTimeout });
  try {
    await context.ready();
    assert.ok(delays.length >= 2, "收到数据后必须重新计时");
    assert.ok(delays.every((ms) => ms === 60_000), "只设置空闲期限，不再设置整个下载的 10 分钟期限");
  } finally { context.cleanup(); }

  let requests = 0;
  const stalled = fixture(spawn, {
    setTimeout: ((callback: () => void) => setTimeout(callback, 10)) as typeof setTimeout,
    fetch: (async (url: string, init?: RequestInit) => {
      if (url.endsWith("latest-mac.yml")) return new Response(stalled.metadata);
      requests++;
      return new Promise((_resolve, reject) => init?.signal?.addEventListener("abort", () => reject(init.signal?.reason), { once: true }));
    }) as typeof fetch,
  });
  try {
    await stalled.manager.check();
    await assert.rejects(stalled.manager.download(), /60 秒无响应/);
    assert.equal(requests, 3);
    assert.equal(stalled.manager.snapshot().status, "failed");
    assert.equal(stalled.exited(), false);
  } finally { stalled.cleanup(); }
});

test("macOS 真实 ZIP 经 ditto 解包、版本校验、替换与 LaunchServices 重开", { skip: process.platform !== "darwin" }, async () => {
  const context = fixture(spawn, { execFile, fetch: (async (url: string) => {
    if (url.endsWith("latest-mac.yml")) {
      const metadata = JSON.parse(context.metadata);
      const bytes = readFileSync(join(context.root, "release.zip"));
      metadata.files[0].sha512 = createHash("sha512").update(bytes).digest("base64");
      metadata.files[0].size = bytes.length;
      return new Response(JSON.stringify(metadata));
    }
    return new Response(readFileSync(join(context.root, "release.zip")));
  }) as typeof fetch });
  try {
    const marker = join(context.root, "reopened");
    const bundle = join(context.root, "release", "Magi.app");
    appBundle(bundle, marker, "real zip reopened");
    await promisify(execFile)("/usr/bin/ditto", ["-c", "-k", "--keepParent", bundle, join(context.root, "release.zip")]);
    await context.ready();
    assert.equal(context.manager.snapshot().status, "downloaded");
    const exit = once(context.events, "exit");
    const installing = context.manager.install(async () => undefined);
    await Promise.race([exit, installing]);
    await waitUntil(() => existsSync(marker));
    assert.equal(readFileSync(marker, "utf8"), "real zip reopened");
    await waitUntil(() => !existsSync(`${context.current}.previous-99999999`));
  } finally { context.cleanup(); }
});

test("暂存包丢失后重试会重新准备，不会永远卡在安装失败", async () => {
  const context = fixture();
  try {
    await context.ready();
    rmSync(context.staged, { recursive: true, force: true });
    await assert.rejects(context.manager.install(async () => undefined), /staging_missing/);
    assert.equal(context.exited(), false);
    await context.manager.download();
    assert.equal(existsSync(context.staged), true);
    assert.equal(context.manager.snapshot().status, "downloaded");
  } finally { context.cleanup(); }
});

test("Windows 安装错误通过原 IPC 调用返回并允许重试", async () => {
  const context = fixture(spawn, { platform: "win32" });
  try {
    let attempts = 0;
    Object.assign(context.updater, { quitAndInstall: () => {
      attempts++;
      if (attempts === 1) context.updater.emit("error", new Error("installer failed"));
      else queueMicrotask(() => context.updater.emit("error", new Error("async installer failed")));
    } });
    context.updater.emit("update-downloaded", { version: "3.0.56" });
    await assert.rejects(context.manager.install(async () => undefined), /installer failed/);
    await assert.rejects(context.manager.install(async () => undefined), /async installer failed/);
    assert.equal(attempts, 2);
    assert.equal(context.exited(), false);
  } finally { context.cleanup(); }
});

test("系统拒绝打开新包时恢复旧包并保留新包供排查", { skip: process.platform !== "darwin" }, async () => {
  let helper: ChildProcess | undefined;
  const context = fixture(((command, args, options) => {
    const simulated = [...(args ?? [])];
    simulated[1] = simulated[1]!.replace('if ! /usr/bin/open -n "$current"; then', 'if ! /usr/bin/false; then');
    helper = spawn(command, simulated, { ...options });
    return helper;
  }) as typeof spawn);
  try {
    await context.ready();
    const marker = join(context.root, "rollback-opened");
    appBundle(context.current, marker, "old restored");
    const exit = once(context.events, "exit");
    await Promise.race([exit, context.manager.install(async () => undefined)]);
    await waitUntil(() => existsSync(marker));
    assert.equal(readFileSync(marker, "utf8"), "old restored");
    assert.equal(existsSync(context.staged), true);
    assert.equal(existsSync(`${context.current}.previous-99999999`), false);
    if (helper?.exitCode === null) await once(helper, "exit");
    assert.equal(helper?.exitCode, 1);
  } finally {
    if (helper?.exitCode === null) helper.kill();
    context.cleanup();
  }
});
