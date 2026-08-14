import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { access, cp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { arch, platform } from "node:os";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { build as esbuild } from "esbuild";

const execFileAsync = promisify(execFile);
const scriptDirectory = dirname(fileURLToPath(import.meta.url));

export const desktopRoot = resolve(scriptDirectory, "..");
export const repositoryRoot = resolve(desktopRoot, "../..");
export const desktopDist = join(desktopRoot, "dist");
export const workerDist = join(repositoryRoot, "browser-automation-worker", "dist");
export const webDist = join(repositoryRoot, "web", "dist");

const runtimePackages = ["electron", "electron-updater", "ws", "devtools-protocol"];

export async function buildDesktopJavaScript({ development = false } = {}) {
  await Promise.all([
    mkdir(join(desktopDist, "main"), { recursive: true }),
    mkdir(join(desktopDist, "preload"), { recursive: true }),
    mkdir(workerDist, { recursive: true }),
  ]);

  const common = {
    bundle: true,
    platform: "node",
    format: "cjs",
    target: "node22",
    logLevel: "info",
    minify: !development,
    sourcemap: development ? "inline" : false,
    legalComments: "none",
  };

  await Promise.all([
    esbuild({
      ...common,
      entryPoints: [join(desktopRoot, "src", "main", "index.ts")],
      outfile: join(desktopDist, "main", "index.cjs"),
      external: ["electron"],
      define: {
        "import.meta.url": "__magiImportMetaUrl",
        "process.env.NODE_ENV": JSON.stringify(development ? "development" : "production"),
      },
      banner: {
        js: "const __magiImportMetaUrl = require('node:url').pathToFileURL(__filename).href;",
      },
    }),
    esbuild({
      ...common,
      entryPoints: [join(desktopRoot, "src", "preload", "index.ts")],
      outfile: join(desktopDist, "preload", "index.cjs"),
      external: ["electron"],
      define: {
        "process.env.NODE_ENV": JSON.stringify(development ? "development" : "production"),
      },
    }),
    esbuild({
      ...common,
      entryPoints: [join(repositoryRoot, "browser-automation-worker", "src", "index.ts")],
      outfile: join(workerDist, "index.cjs"),
      define: {
        "process.env.NODE_ENV": JSON.stringify(development ? "development" : "production"),
      },
    }),
  ]);
}

export async function buildWebAssets() {
  await run(process.platform === "win32" ? "npm.cmd" : "npm", [
    "run",
    "build",
    "--workspace",
    "magi-web",
  ], repositoryRoot);
  await assertFile(join(webDist, "web.html"), "Web Renderer 构建产物");
}

export async function buildDaemon(profile) {
  const args = ["build", "--locked", "-p", "magi-daemon-app"];
  if (profile === "release") args.push("--release");
  await run("cargo", args, repositoryRoot);
  await assertFile(daemonBinary(profile), "Rust daemon sidecar");
}

export function daemonBinary(profile) {
  return join(
    repositoryRoot,
    "target",
    profile,
    process.platform === "win32" ? "magi-daemon-app.exe" : "magi-daemon-app",
  );
}

export async function prepareReleaseMetadata() {
  const resources = join(desktopDist, "resources");
  await rm(resources, { recursive: true, force: true });
  await mkdir(join(resources, "licenses"), { recursive: true });

  await cp(join(repositoryRoot, "LICENSE"), join(resources, "licenses", "Magi-LICENSE"));
  await stageRuntimeLicenses(join(resources, "licenses"));

  const [productVersion, electronVersions, git, protocolVersion] = await Promise.all([
    readProductVersion(),
    readElectronVersions(),
    readGitIdentity(),
    readDesktopProtocolVersion(),
  ]);
  const cdpPackage = await readJson(join(repositoryRoot, "node_modules", "devtools-protocol", "package.json"));
  await stageSbom(join(resources, "sbom"), productVersion, git.commit);

  const files = [];
  await appendFileHash(files, daemonBinary("release"), daemonResourcePath());
  await appendFileHash(files, join(workerDist, "index.cjs"), "browser-automation-worker/index.cjs");
  await appendTreeHashes(files, webDist, "web/dist");
  await appendTreeHashes(files, join(resources, "licenses"), "licenses");
  await appendTreeHashes(files, join(resources, "sbom"), "sbom");

  const components = {
    desktopMain: await componentHash(join(desktopDist, "main", "index.cjs"), "app.asar:dist/main/index.cjs"),
    desktopPreload: await componentHash(join(desktopDist, "preload", "index.cjs"), "app.asar:dist/preload/index.cjs"),
    automationWorker: await componentHash(
      join(workerDist, "index.cjs"),
      "browser-automation-worker/index.cjs",
    ),
    daemon: await componentHash(daemonBinary("release"), daemonResourcePath()),
  };

  const manifest = {
    schemaVersion: 1,
    productVersion,
    gitCommit: git.commit,
    gitDirty: git.dirty,
    electronVersion: electronVersions.electron,
    chromiumVersion: electronVersions.chrome,
    daemonVersion: productVersion,
    // Worker 与 daemon 一起作为 Magi Desktop 的原子发行物，使用同一个
    // 产品版本标识；CDP/Electron/Chromium 仍保留各自真实版本。
    automationWorkerVersion: productVersion,
    desktopProtocolVersion: `${protocolVersion.major}.${protocolVersion.minor}`,
    cdpCompatibilityVersion: String(cdpPackage.version),
    platform: platform(),
    arch: arch(),
    components,
    files: files.sort((left, right) => left.path.localeCompare(right.path)),
  };
  await writeFile(
    join(resources, "browser-capability-manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
    "utf8",
  );
  await writeFile(
    join(desktopDist, "build-metadata.json"),
    `${JSON.stringify({ productVersion, ...electronVersions, git, protocolVersion }, null, 2)}\n`,
    "utf8",
  );
  return manifest;
}

export async function cleanDesktopOutputs() {
  await Promise.all([
    rm(desktopDist, { recursive: true, force: true }),
    rm(workerDist, { recursive: true, force: true }),
  ]);
}

export async function readProductVersion() {
  const cargoManifest = await readFile(join(repositoryRoot, "Cargo.toml"), "utf8");
  const section = cargoManifest.match(/\[workspace\.package\]([\s\S]*?)(?=\n\[|$)/)?.[1] ?? "";
  const version = section.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error("Cargo.toml 的 [workspace.package].version 不是有效 SemVer");
  }
  return version;
}

export async function assertReleaseInputs() {
  await Promise.all([
    assertFile(join(desktopDist, "main", "index.cjs"), "Electron Main CJS"),
    assertFile(join(desktopDist, "preload", "index.cjs"), "Electron preload CJS"),
    assertFile(join(workerDist, "index.cjs"), "Browser Automation Worker"),
    assertFile(join(webDist, "web.html"), "Web Renderer"),
    assertFile(daemonBinary("release"), "Rust daemon sidecar"),
    assertFile(join(desktopDist, "resources", "browser-capability-manifest.json"), "能力清单"),
  ]);
}

export async function run(command, args, cwd = repositoryRoot, options = {}) {
  await new Promise((resolvePromise, reject) => {
    const child = execFile(command, args, {
      cwd,
      env: { ...process.env, ...options.env },
      maxBuffer: 16 * 1024 * 1024,
    });
    child.stdout?.pipe(process.stdout);
    child.stderr?.pipe(process.stderr);
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolvePromise();
      else reject(new Error(`${command} 执行失败: ${code ?? signal ?? "unknown"}`));
    });
  });
}

export async function assertFile(path, label) {
  try {
    const value = await stat(path);
    if (!value.isFile()) throw new Error("不是文件");
  } catch (cause) {
    throw new Error(`${label}不存在: ${path}`, { cause });
  }
}

async function readElectronVersions() {
  const electronPackage = await readJson(join(repositoryRoot, "node_modules", "electron", "package.json"));
  const electronExecutable = await resolveElectronExecutable();
  const probeDirectory = join(desktopDist, ".electron-version-probe");
  const probePath = join(probeDirectory, "probe.cjs");
  await mkdir(probeDirectory, { recursive: true });
  await writeFile(
    probePath,
    "console.log(JSON.stringify(process.versions)); require('electron').app.exit(0);\n",
    "utf8",
  );
  const args = [
    "--headless",
    "--disable-gpu",
    "--use-mock-keychain",
    "--no-first-run",
    ...(process.platform === "linux" ? ["--no-sandbox"] : []),
    probePath,
  ];
  const { stdout } = await execFileAsync(electronExecutable, args, {
    cwd: repositoryRoot,
    timeout: 30_000,
    maxBuffer: 1024 * 1024,
  });
  const line = stdout.trim().split(/\r?\n/).find((value) => value.trim().startsWith("{"));
  if (!line) throw new Error("无法读取 Electron/Chromium 版本");
  const versions = JSON.parse(line);
  if (versions.electron !== electronPackage.version || typeof versions.chrome !== "string") {
    throw new Error("Electron/Chromium 版本探测结果与 lockfile 不一致");
  }
  await rm(probeDirectory, { recursive: true, force: true });
  return { electron: versions.electron, chrome: versions.chrome };
}

async function resolveElectronExecutable() {
  const electronRoot = join(repositoryRoot, "node_modules", "electron");
  const relativePath = (await readFile(join(electronRoot, "path.txt"), "utf8")).trim();
  const executable = join(electronRoot, "dist", relativePath);
  await assertFile(executable, "Electron 可执行文件");
  return executable;
}

async function readGitIdentity() {
  const { stdout: commitOutput } = await execFileAsync("git", ["rev-parse", "HEAD"], {
    cwd: repositoryRoot,
  });
  const { stdout: statusOutput } = await execFileAsync("git", ["status", "--porcelain"], {
    cwd: repositoryRoot,
    maxBuffer: 16 * 1024 * 1024,
  });
  return { commit: commitOutput.trim(), dirty: statusOutput.trim().length > 0 };
}

async function readDesktopProtocolVersion() {
  const source = await readFile(
    join(repositoryRoot, "contracts", "desktop-browser", "src", "index.ts"),
    "utf8",
  );
  const match = source.match(
    /DESKTOP_BROWSER_PROTOCOL_VERSION\s*=\s*\{\s*major:\s*(\d+)\s*,\s*minor:\s*(\d+)\s*\}/,
  );
  if (!match) throw new Error("无法从共享协议源读取 Desktop Browser 协议版本");
  return { major: Number(match[1]), minor: Number(match[2]) };
}

async function stageRuntimeLicenses(destination) {
  const notices = [];
  for (const packageName of runtimePackages) {
    const packageRoot = join(repositoryRoot, "node_modules", packageName);
    const metadata = await readJson(join(packageRoot, "package.json"));
    notices.push({
      name: packageName,
      version: metadata.version,
      license: metadata.license ?? "UNKNOWN",
      repository: metadata.repository ?? null,
    });
    const license = await findLicense(packageRoot);
    if (license) {
      await cp(license, join(destination, `${packageName.replaceAll("/", "-")}-LICENSE${licenseExtension(license)}`));
    }
  }
  const chromiumLicenses = join(repositoryRoot, "node_modules", "electron", "dist", "LICENSES.chromium.html");
  await access(chromiumLicenses);
  await cp(chromiumLicenses, join(destination, "Chromium-THIRD-PARTY-LICENSES.html"));
  await writeFile(
    join(destination, "THIRD-PARTY-NOTICES.json"),
    `${JSON.stringify(notices, null, 2)}\n`,
    "utf8",
  );
}

async function stageSbom(destination, productVersion, gitCommit) {
  const { stdout } = await execFileAsync(
    "cargo",
    ["metadata", "--locked", "--format-version", "1"],
    { cwd: repositoryRoot, maxBuffer: 64 * 1024 * 1024 },
  );
  const cargoMetadata = JSON.parse(stdout);
  const cargoComponents = cargoMetadata.packages.map((value) => ({
    type: "library",
    name: value.name,
    version: value.version,
    ...(value.license ? { licenses: [{ expression: value.license }] } : {}),
    purl: `pkg:cargo/${encodeURIComponent(value.name)}@${encodeURIComponent(value.version)}`,
    properties: [
      { name: "magi:ecosystem", value: "cargo" },
      ...(value.source ? [{ name: "magi:source", value: value.source }] : []),
    ],
  }));
  const npmComponents = [];
  for (const packageName of runtimePackages) {
    const metadata = await readJson(join(repositoryRoot, "node_modules", packageName, "package.json"));
    npmComponents.push({
      type: "library",
      name: packageName,
      version: metadata.version,
      ...(metadata.license ? { licenses: [{ expression: metadata.license }] } : {}),
      purl: `pkg:npm/${encodeURIComponent(packageName)}@${encodeURIComponent(metadata.version)}`,
      properties: [{ name: "magi:ecosystem", value: "npm" }],
    });
  }
  const components = [...cargoComponents, ...npmComponents]
    .sort((left, right) => `${left.purl}`.localeCompare(`${right.purl}`));
  const sbom = {
    bomFormat: "CycloneDX",
    specVersion: "1.5",
    version: 1,
    metadata: {
      component: {
        type: "application",
        name: "Magi Desktop",
        version: productVersion,
        purl: `pkg:github/MistRipple/magi-code@${encodeURIComponent(productVersion)}`,
        properties: [{ name: "magi:gitCommit", value: gitCommit }],
      },
    },
    components,
  };
  await mkdir(destination, { recursive: true });
  await writeFile(
    join(destination, "magi-desktop.cdx.json"),
    `${JSON.stringify(sbom, null, 2)}\n`,
    "utf8",
  );
}

async function findLicense(packageRoot) {
  const entries = await readdir(packageRoot, { withFileTypes: true });
  const entry = entries.find((value) => value.isFile() && /^(license|notice)(\.|$)/i.test(value.name));
  return entry ? join(packageRoot, entry.name) : null;
}

function licenseExtension(path) {
  const name = basename(path);
  const offset = name.indexOf(".");
  return offset < 0 ? ".txt" : name.slice(offset);
}

async function appendTreeHashes(output, root, resourceRoot) {
  for (const path of await walkFiles(root)) {
    await appendFileHash(output, path, `${resourceRoot}/${toPosix(relative(root, path))}`);
  }
}

async function appendFileHash(output, source, resourcePath) {
  const bytes = await readFile(source);
  output.push({ path: resourcePath, size: bytes.byteLength, sha256: sha256(bytes) });
}

async function componentHash(source, packagedPath) {
  const bytes = await readFile(source);
  return { path: packagedPath, size: bytes.byteLength, sha256: sha256(bytes) };
}

async function walkFiles(root) {
  const output = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) output.push(...await walkFiles(path));
    else if (entry.isFile()) output.push(path);
  }
  return output.sort();
}

function daemonResourcePath() {
  return `daemon/${process.platform === "win32" ? "magi-daemon-app.exe" : "magi-daemon-app"}`;
}

function toPosix(path) {
  return sep === "/" ? path : path.split(sep).join("/");
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}
