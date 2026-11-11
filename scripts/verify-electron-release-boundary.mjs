import { access, readFile, readdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { isRetiredRuntimePath } from "../apps/desktop/scripts/after-pack.mjs";

assert.equal(isRetiredRuntimePath("daemon/compat/node_modules/simple-icons/icons/tauri.svg"), false);
for (const path of ["tauri/runtime.js", "daemon/tauri/icons/logo.svg", "cef.dll", "playwright/index.js", "Chromium Embedded Framework.framework/main", "daemon/compat/translate/settings-page.mjs", "daemon/model_bridge_loopback", "daemon/mcp_bridge_loopback.exe", "daemon/mcp_manager_server"]) {
  assert.equal(isRetiredRuntimePath(path), true, path);
}

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const forbiddenPaths = [
  ".github/workflows/browser-runtime-release.yml",
  "browser-host",
  "config/native-browser-runtime.json",
  "config/browser-runtime-release.json",
  "scripts/browser-runtime-manifest-sequence.mjs",
  "scripts/browser-runtime-release-config.mjs",
  "scripts/fetch-cef-runtime.mjs",
  "scripts/self-test-browser-runtime.mjs",
  "scripts/stage-browser-runtime.mjs",
  "scripts/stage-native-browser-bundle.mjs",
];

for (const relativePath of forbiddenPaths) {
  try {
    await access(join(root, relativePath));
    throw new Error(`废弃发行资产仍然存在: ${relativePath}`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

const forbidden = /(?:@tauri-apps|\btauri\b|chromium embedded framework|\bcef\b|browser-runtime-release|browser-host|native-browser-runtime|playwright)/iu;
function assertWorkflowRuntimeBoundary(source, name) {
  // 旧更新器签名是正式升级合同；只允许发布工作流安装固定版本的签名 CLI。
  // 其他位置或命令仍禁止旧宿主，不能靠拆分字符串绕过同一个门禁。
  const runtimeSource = name === "release.yml"
    ? source.replace(/^\s*cargo install tauri-cli --version "\$TAURI_CLI_VERSION" --locked\s*$/gmu, "")
    : source;
  if (forbidden.test(runtimeSource)) throw new Error(`工作流仍引用废弃实现: .github/workflows/${name}`);
}

const migrationSignerInstall = 'cargo install tauri-cli --version "$TAURI_CLI_VERSION" --locked';
assert.doesNotThrow(() => assertWorkflowRuntimeBoundary(migrationSignerInstall, "release.yml"));
for (const source of ["cargo tauri build", `${migrationSignerInstall}\ncargo tauri build`, "cargo install tauri-cli --locked"]) {
  assert.throws(() => assertWorkflowRuntimeBoundary(source, "release.yml"));
}
assert.throws(() => assertWorkflowRuntimeBoundary(migrationSignerInstall, "ci.yml"));
const workflowDirectory = join(root, ".github", "workflows");
for (const name of await readdir(workflowDirectory)) {
  if (!/\.ya?ml$/u.test(name)) continue;
  const source = await readFile(join(workflowDirectory, name), "utf8");
  assertWorkflowRuntimeBoundary(source, name);
}
assertWorkflowRuntimeBoundary(await readFile(join(root, "scripts/release-preflight.mjs"), "utf8"), "release-preflight.mjs");

const lock = JSON.parse(await readFile(join(root, "package-lock.json"), "utf8"));
for (const [path, metadata] of Object.entries(lock.packages ?? {})) {
  const identity = `${path} ${metadata?.name ?? ""}`;
  if (forbidden.test(identity)) throw new Error(`npm lockfile 仍包含废弃生产组件: ${identity}`);
}

for (const relativePath of ["package.json", "apps/desktop/package.json", "browser-automation-worker/package.json", "web/package.json"]) {
  const manifest = JSON.parse(await readFile(join(root, relativePath), "utf8"));
  for (const section of ["dependencies", "optionalDependencies"]) {
    for (const dependency of Object.keys(manifest[section] ?? {})) {
      if (forbidden.test(dependency)) throw new Error(`${relativePath} 的 ${section} 仍包含废弃依赖: ${dependency}`);
    }
  }
}

process.stdout.write("Electron 单一发行边界校验通过。\n");
