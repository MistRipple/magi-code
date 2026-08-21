import { createHash } from "node:crypto";
import { access, readFile, readdir } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import { extractFile, listPackage } from "@electron/asar";

export async function afterPack(context) {
  const resources = context.electronPlatformName === "darwin"
    ? join(context.appOutDir, `${context.packager.appInfo.productFilename}.app`, "Contents", "Resources")
    : join(context.appOutDir, "resources");
  const manifest = JSON.parse(
    await readFile(join(resources, "browser-capability-manifest.json"), "utf8"),
  );
  const asarPath = join(resources, "app.asar");
  const packageMetadata = JSON.parse(extractFile(asarPath, "package.json").toString("utf8"));
  if (packageMetadata.version !== manifest.productVersion) {
    throw new Error("app.asar 版本与 Browser capability manifest 不一致");
  }
  // 目录包只用于本地验收，package.mjs 已明确将其标记为不可在线更新；
  // 正式 dmg/zip/nsis/AppImage 仍必须携带统一 Magi Desktop 更新清单。
  if (packageMetadata.magiDistribution !== "directory") {
    await verifyDesktopUpdateFeed(join(resources, "app-update.yml"));
  }

  verifyAsarComponent(asarPath, "dist/main/index.cjs", manifest.components.desktopMain);
  verifyAsarComponent(asarPath, "dist/preload/index.cjs", manifest.components.desktopPreload);
  for (const file of manifest.files) {
    const path = join(resources, ...file.path.split("/"));
    const bytes = await readFile(path);
    if (bytes.byteLength !== file.size || sha256(bytes) !== file.sha256) {
      throw new Error(`发行资源哈希不匹配: ${file.path}`);
    }
  }

  const packagedPaths = [
    ...listPackage(asarPath).map((path) => `app.asar/${path}`),
    ...await walkResources(resources),
  ];
  const unpackedNodeModule = packagedPaths.find((path) => /app\.asar\/?node_modules\//i.test(path));
  if (unpackedNodeModule) {
    throw new Error(`Desktop Main 必须是单一 bundle，不得携带运行时 node_modules: ${unpackedNodeModule}`);
  }
  const forbidden = packagedPaths.find((path) =>
    /(^|\/)(tauri|cef|playwright|native-browser|browser-runtime)(\/|\.|-|$)/i.test(path)
    || /chromium embedded framework/i.test(path)
  );
  if (forbidden) throw new Error(`发行包包含废弃 Browser/Desktop 实现: ${forbidden}`);
  process.stdout.write(`Electron 解包自检通过: ${context.appOutDir}\n`);
}

function verifyAsarComponent(asarPath, path, expected) {
  // ASAR 元数据在 Windows 使用反斜杠分隔，读取时必须沿用当前平台的路径格式。
  const archivePath = join(...path.split("/"));
  const bytes = extractFile(asarPath, archivePath);
  if (bytes.byteLength !== expected.size || sha256(bytes) !== expected.sha256) {
    throw new Error(`app.asar 组件哈希不匹配: ${path}`);
  }
}

async function walkResources(root, directory = root) {
  const output = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) output.push(...await walkResources(root, path));
    else if (entry.isFile()) output.push(toPosix(relative(root, path)));
  }
  return output;
}

async function verifyDesktopUpdateFeed(path) {
  let source;
  try {
    await access(path);
    source = await readFile(path, "utf8");
  } catch {
    throw new Error("发行包缺少 app-update.yml，无法使用统一 Magi Desktop 更新源");
  }
  const required = [
    /^\s*provider:\s*github\s*$/m,
    /^\s*owner:\s*MistRipple\s*$/m,
    /^\s*repo:\s*magi-code\s*$/m,
    /^\s*releaseType:\s*release\s*$/m,
  ];
  if (required.some((pattern) => !pattern.test(source))) {
    throw new Error("发行包 app-update.yml 必须指向统一 Magi Desktop GitHub Release");
  }
  if (/^\s*channel\s*:/m.test(source) || /browser-runtime-stable|browser-runtime-release/i.test(source)) {
    throw new Error("发行包 app-update.yml 不得包含 Browser Runtime channel");
  }
}

function toPosix(path) {
  return sep === "/" ? path : path.split(sep).join("/");
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}
