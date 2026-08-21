import { execFileSync } from "node:child_process";
import { cp, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";

const args = parseArgs(process.argv.slice(2));
const assetsDir = resolve(required(args, "assets-dir"));
const version = required(args, "version");
const tag = required(args, "tag");
const notesFile = resolve(required(args, "notes-file"));
const repository = required(args, "repository");
const privateKey = process.env.TAURI_SIGNING_PRIVATE_KEY;
const privateKeyPassword = process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? "";

if (!privateKey) {
  throw new Error("缺少 GitHub Secret: TAURI_SIGNING_PRIVATE_KEY");
}

const packages = [
  {
    source: `Magi-${version}-mac-arm64.zip`,
    output: `Magi_${version}_darwin-aarch64-electron.app.tar.gz`,
    platforms: ["darwin-aarch64-app", "darwin-aarch64"],
    kind: "mac",
  },
  {
    source: `Magi-${version}-mac-x64.zip`,
    output: `Magi_${version}_darwin-x86_64-electron.app.tar.gz`,
    platforms: ["darwin-x86_64-app", "darwin-x86_64"],
    kind: "mac",
  },
  {
    source: `Magi-${version}-linux-x86_64.AppImage`,
    output: `Magi_${version}_linux-x86_64-electron.AppImage`,
    platforms: ["linux-x86_64-appimage", "linux-x86_64"],
    kind: "copy",
  },
  {
    source: `Magi-${version}-win-x64.exe`,
    output: `Magi_${version}_windows-x86_64-electron.exe`,
    platforms: ["windows-x86_64-nsis", "windows-x86_64"],
    kind: "copy",
  },
];

const temporary = await mkdtemp(join(tmpdir(), "magi-legacy-desktop-"));
try {
  const platformEntries = {};
  for (const packageInfo of packages) {
    const sourcePath = join(assetsDir, packageInfo.source);
    await assertFile(sourcePath);
    const outputPath = join(assetsDir, packageInfo.output);

    if (packageInfo.kind === "mac") {
      await makeMacArchive(sourcePath, outputPath, temporary, packageInfo.output);
    } else {
      await cp(sourcePath, outputPath);
    }

    await sign(outputPath, privateKey, privateKeyPassword);
    const signature = (await readFile(`${outputPath}.sig`, "utf8")).trim();
    if (!signature) throw new Error(`迁移包签名为空：${basename(outputPath)}`);

    const url = `https://github.com/${repository}/releases/download/${tag}/${basename(outputPath)}`;
    for (const platform of packageInfo.platforms) {
      platformEntries[platform] = { url, signature };
    }
  }

  const notes = await readFile(notesFile, "utf8");
  const manifest = {
    version,
    notes,
    pub_date: new Date().toISOString(),
    platforms: platformEntries,
  };
  const manifestPath = join(assetsDir, "latest.json");
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  await assertManifest(manifestPath, packages);
  process.stdout.write(`旧版桌面更新桥已生成：${manifestPath}\n`);
} finally {
  await rm(temporary, { recursive: true, force: true });
}

async function makeMacArchive(sourcePath, outputPath, temporaryRoot, outputName) {
  const extraction = join(temporaryRoot, outputName);
  execFileSync("unzip", ["-q", sourcePath, "-d", extraction], { stdio: "inherit" });
  const entries = await readdir(extraction, { withFileTypes: true });
  const app = entries.find((entry) => entry.isDirectory() && entry.name === "Magi.app");
  if (!app) throw new Error(`macOS ZIP 缺少 Magi.app：${basename(sourcePath)}`);
  execFileSync("tar", ["-czf", outputPath, "-C", extraction, "Magi.app"], { stdio: "inherit" });
}

async function sign(file, privateKey, password) {
  execFileSync(
    "cargo",
    ["tauri", "signer", "sign", "--private-key", privateKey, "--password", password, file],
    { stdio: "inherit" },
  );
}

async function assertManifest(manifestPath, packageInfos) {
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  if (Object.keys(manifest.platforms ?? {}).length !== packageInfos.length * 2) {
    throw new Error("旧版桌面更新清单的平台数量不完整");
  }
  for (const packageInfo of packageInfos) {
    await assertFile(join(assetsDir, packageInfo.output));
    await assertFile(join(assetsDir, `${packageInfo.output}.sig`));
    for (const platform of packageInfo.platforms) {
      if (!manifest.platforms[platform]?.url || !manifest.platforms[platform]?.signature) {
        throw new Error(`旧版桌面更新清单缺少平台：${platform}`);
      }
    }
  }
}

async function assertFile(path) {
  const file = await stat(path);
  if (!file.isFile() || file.size === 0) throw new Error(`缺少发行文件：${path}`);
}

function parseArgs(values) {
  const output = {};
  for (let index = 0; index < values.length; index += 2) {
    const key = values[index]?.replace(/^--/u, "");
    const value = values[index + 1];
    if (!key || value === undefined || !values[index].startsWith("--")) {
      throw new Error(`参数格式错误：${values[index] ?? ""}`);
    }
    output[key] = value;
  }
  return output;
}

function required(values, key) {
  if (!values[key]) throw new Error(`缺少参数：--${key}`);
  return values[key];
}
