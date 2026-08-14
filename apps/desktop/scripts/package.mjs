import { Arch, Platform, build } from "electron-builder";
import { buildDesktopRelease } from "./build.mjs";
import { desktopRoot } from "./build-support.mjs";

const arguments_ = process.argv.slice(2);
const unknownArgument = arguments_.find((value) => value !== "--dir");
if (unknownArgument) throw new Error(`不支持的打包参数: ${unknownArgument}`);
const directoryOnly = arguments_.includes("--dir");
const manifest = await buildDesktopRelease();
const host = hostTarget();
const requireCodeSigning = process.env.MAGI_REQUIRE_CODE_SIGNING === "1";
const targets = host.platform.createTarget(
  directoryOnly ? ["dir"] : host.targets,
  host.arch,
);

const artifacts = await build({
  projectDir: desktopRoot,
  targets,
  publish: "never",
  config: {
    extends: null,
    extraMetadata: { version: manifest.productVersion },
    ...(requireCodeSigning ? { forceCodeSigning: true } : {}),
  },
});

process.stdout.write(`Magi Desktop 发行物已生成：\n${artifacts.map((path) => `- ${path}`).join("\n")}\n`);

function hostTarget() {
  const currentArch = process.arch === "arm64" ? Arch.arm64 : process.arch === "x64" ? Arch.x64 : null;
  if (currentArch === null) throw new Error(`不支持的桌面架构: ${process.arch}`);
  switch (process.platform) {
    case "darwin":
      return { platform: Platform.MAC, arch: currentArch, targets: ["dmg", "zip"] };
    case "win32":
      return { platform: Platform.WINDOWS, arch: currentArch, targets: ["nsis"] };
    case "linux":
      return { platform: Platform.LINUX, arch: currentArch, targets: ["AppImage", "deb"] };
    default:
      throw new Error(`不支持的桌面平台: ${process.platform}`);
  }
}
