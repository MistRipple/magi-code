import { access, readFile, readdir } from "node:fs/promises";
import { join, resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);
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
const workflowDirectory = join(root, ".github", "workflows");
for (const name of await readdir(workflowDirectory)) {
  if (!/\.ya?ml$/u.test(name)) continue;
  const source = await readFile(join(workflowDirectory, name), "utf8");
  if (forbidden.test(source)) throw new Error(`工作流仍引用废弃实现: .github/workflows/${name}`);
}

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
