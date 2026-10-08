import { execFileSync, spawnSync } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const rustToolchain = process.env.MAGI_RELEASE_RUST_TOOLCHAIN ?? "1.97.0";
const options = parseArgs(process.argv.slice(2));

if (options.help) {
  printHelp();
  process.exit(0);
}

validateVersion(options.tag);

const commonSteps = [
  ["crate 架构文档一致性", npm, ["run", "docs:architecture:check"]],
  ["TypeScript 与 Svelte 检查", npm, ["run", "check"]],
  ["Desktop、Worker 与 Web 测试", npm, ["run", "test"]],
  ["Browser 核心边界验证", npm, ["run", "test:browser-core"]],
  ["Web 生产构建", npm, ["run", "build", "--workspace", "magi-web"]],
  ["Browser Automation Worker 生产构建", npm, ["run", "build", "--workspace", "@magi/browser-automation-worker"]],
  ["单一 Electron 发行边界", npm, ["run", "release:guard"]],
  ["本地 daemon 与 bridge loopback 可执行文件构建", "cargo", [rustPrefix(), "build", "-p", "magi-daemon-app", "-p", "magi-bridge-client", "--bins", "--locked"]],
  ["本地 bridge loopback 实际 preflight", npm, ["run", "bridge:preflight", "--workspace", "@magi/desktop", "--", "--profile", "debug"]],
  ["Rust Clippy", "cargo", [rustPrefix(), "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]],
  ["Rust 全量测试", "cargo", [rustPrefix(), "test", "--workspace", "--locked"]],
];

for (const [label, command, args] of commonSteps) run(label, command, args);

if (options.package) {
  run("当前平台 Electron Desktop 发行打包", npm, ["run", "desktop:package"]);
}

if (options.audit) {
  run("前端生产依赖安全审计", npm, ["audit", "--omit=dev", "--workspaces"]);
  run("Rust 依赖安全审计", "cargo", [rustPrefix(), "audit"]);
}

console.log("\n发布前置校验全部通过。可以提交并推送到 main，随后再创建版本 Tag。\n");

function run(label, command, args) {
  console.log(`\n[release-preflight] ${label}`);
  console.log(`$ ${command} ${args.filter((arg) => arg !== "").join(" ")}`);
  const result = spawnSync(command, args, {
    cwd: root,
    env: { ...process.env, CARGO_TERM_COLOR: "always" },
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${label}失败，退出码：${result.status ?? "unknown"}`);
  }
}

function validateVersion(tag) {
  const version = execFileSync(process.execPath, [resolve(root, "scripts/product-version.mjs")], {
    cwd: root,
    encoding: "utf8",
  }).trim();
  if (!tag) {
    console.log(`[release-preflight] 产品版本：${version}`);
    return;
  }
  const normalizedTag = tag.startsWith("v") ? tag.slice(1) : tag;
  if (normalizedTag !== version) {
    throw new Error(`Tag ${tag} 与产品版本 ${version} 不一致`);
  }
  const notes = resolve(root, ".github", "releases", `v${version}.md`);
  try {
    accessSync(notes, constants.R_OK);
  } catch {
    throw new Error(`缺少发布说明：${notes}`);
  }
  console.log(`[release-preflight] 产品版本与发布说明：v${version}`);
}

function rustPrefix() {
  return `+${rustToolchain}`;
}

function parseArgs(values) {
  const output = { audit: false, help: false, package: false, tag: null };
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === "--help" || value === "-h") {
      output.help = true;
    } else if (value === "--audit") {
      output.audit = true;
    } else if (value === "--package") {
      output.package = true;
    } else if (value === "--tag") {
      output.tag = values[++index];
      if (!output.tag) throw new Error("--tag 需要一个版本 Tag");
    } else {
      throw new Error(`不支持的参数：${value}`);
    }
  }
  return output;
}

function printHelp() {
  console.log(`用法：npm run release:preflight -- [选项]

选项：
  --tag vX.Y.Z   校验 Tag、产品版本和发布说明一致
  --package      在当前平台额外执行 Electron Desktop 打包
  --audit        额外执行 npm audit 和 cargo audit
  --help         显示帮助

推荐：
  npm run release:preflight -- --tag vX.Y.Z --package --audit
`);
}
