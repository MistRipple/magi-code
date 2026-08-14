import {
  assertReleaseInputs,
  buildDaemon,
  buildDesktopJavaScript,
  buildWebAssets,
  cleanDesktopOutputs,
  prepareReleaseMetadata,
} from "./build-support.mjs";
import { pathToFileURL } from "node:url";

export async function buildDesktopRelease() {
  await cleanDesktopOutputs();
  await Promise.all([
    buildWebAssets(),
    buildDaemon("release"),
    buildDesktopJavaScript(),
  ]);
  const manifest = await prepareReleaseMetadata();
  await assertReleaseInputs();
  process.stdout.write(
    `Magi Desktop ${manifest.productVersion} 构建完成（Electron ${manifest.electronVersion} / Chromium ${manifest.chromiumVersion}）。\n`,
  );
  return manifest;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await buildDesktopRelease();
}
