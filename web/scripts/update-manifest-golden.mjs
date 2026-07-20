import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createUpdateManifest } from './generate-update-manifest.mjs';

const releaseWorkflow = fs.readFileSync(path.resolve('../.github/workflows/release.yml'), 'utf8');

assert.doesNotMatch(
  releaseWorkflow,
  /dist\/release-assets\/normalized/,
  'release workflow must keep one authoritative release-assets directory',
);
assert.match(
  releaseWorkflow,
  /--output dist\/release-assets\/latest\.json/,
  'release workflow must generate latest.json beside the release assets',
);
assert.match(
  releaseWorkflow,
  /files: dist\/release-assets\/\*/,
  'GitHub Release must upload installers, updater archives, signatures, and latest.json',
);
assert.match(
  releaseWorkflow,
  /notes_file="\.github\/releases\/\$\{tag\}\.md"/,
  'release workflow must read the versioned product release notes',
);
assert.doesNotMatch(
  releaseWorkflow,
  /git log|release-changes\.md|previous_tag/,
  'release notes must not expose raw commit history',
);
assert.doesNotMatch(
  releaseWorkflow,
  /## 构建信息|Rust 工具链|Tag：/,
  'release notes must not append implementation-oriented build metadata',
);
assert.match(
  releaseWorkflow,
  /- name: 构建桌面安装包\n\s+shell: bash\n\s+env:/,
  'cross-platform desktop packaging must use bash for its shell script',
);
assert.doesNotMatch(
  releaseWorkflow,
  /AppImage\.tar\.gz|nsis\.zip/,
  'Tauri v2 self-contained Linux and Windows updater artifacts must not use legacy v1 archive names',
);
assert.match(releaseWorkflow, /\.AppImage\.sig/, 'Linux updater validation must use the signed AppImage');
assert.match(releaseWorkflow, /\.exe\.sig/, 'Windows updater validation must use the signed NSIS installer');
assert.match(releaseWorkflow, /runner: macos-15/, 'release builds must pin the macOS runner image');
assert.match(releaseWorkflow, /platform: macos-intel[\s\S]*runner: macos-15-intel/, 'release builds must include the Intel macOS runner');
assert.match(releaseWorkflow, /macos_assets\[@\]\}.*-eq 2/, 'release validation must require both macOS installers');
assert.match(releaseWorkflow, /update_assets\[@\]\}.*-eq 4/, 'release validation must require both macOS updater archives');
assert.match(releaseWorkflow, /macOS Apple Silicon[\s\S]*macOS Intel/, 'release notes must distinguish both macOS architectures');
assert.doesNotMatch(releaseWorkflow, /macos-latest/, 'release builds must not drift with macos-latest');
assert.match(releaseWorkflow, /actions\/checkout@v7/g, 'release workflow must use the Node 24 checkout action');
assert.match(releaseWorkflow, /actions\/setup-node@v7/g, 'release workflow must use the Node 24 setup-node action');
assert.match(releaseWorkflow, /actions\/upload-artifact@v7/g, 'release workflow must use the current artifact uploader');
assert.match(releaseWorkflow, /actions\/download-artifact@v8/g, 'release workflow must use the current artifact downloader');
assert.doesNotMatch(
  releaseWorkflow,
  /actions\/(?:checkout|setup-node|upload-artifact|download-artifact)@v4/,
  'release workflow must not depend on deprecated Node 20 action runtimes',
);
assert.match(
  releaseWorkflow,
  /host_triple="\$\(rustc -vV \| sed -n 's\/\^host: \/\/p'\)"/,
  'release workflow must read the complete Rust host triple before extracting its architecture',
);
assert.match(
  releaseWorkflow,
  /host_arch="\$\{host_triple%%-\*\}"/,
  'release workflow must extract the leading architecture component from the Rust host triple',
);
assert.doesNotMatch(
  releaseWorkflow,
  /host: \.\*\\-\\\(x86_64\\\|aarch64/,
  'release workflow must not require a separator before the leading host architecture',
);

const releaseNotes = fs.readFileSync(path.resolve('../.github/releases/v3.0.5.md'), 'utf8');
assert.match(releaseNotes, /轮次导航/, '3.0.5 notes must explain turn navigation');
assert.match(releaseNotes, /Shell/, '3.0.5 notes must explain shell reliability');
assert.match(releaseNotes, /排队/, '3.0.5 notes must explain queued-message interaction');
assert.match(releaseNotes, /设置/, '3.0.5 notes must explain settings restoration');
assert.doesNotMatch(releaseNotes, /[0-9a-f]{7,40}/i, '3.0.5 notes must not expose commit hashes');

const manifest = createUpdateManifest({
  version: '3.0.5',
  tag: 'v3.0.5',
  repository: 'MistRipple/magi-code',
  pubDate: '2026-07-17T00:00:00Z',
  notes: '测试更新',
  platforms: [
    {
      target: 'darwin-aarch64-app',
      filename: 'Magi_3.0.5_darwin-aarch64-app.tar.gz',
      signature: 'signed-macos',
    },
    {
      target: 'darwin-x86_64-app',
      filename: 'Magi_3.0.5_darwin-x86_64-app.tar.gz',
      signature: 'signed-macos-intel',
    },
    {
      target: 'linux-x86_64-appimage',
      filename: 'Magi_3.0.5_linux-x86_64-appimage.tar.gz',
      signature: 'signed-linux',
    },
  ],
});

assert.equal(manifest.version, '3.0.5');
assert.equal(manifest.pub_date, '2026-07-17T00:00:00Z');
assert.equal(
  manifest.platforms['darwin-aarch64-app'].url,
  'https://github.com/MistRipple/magi-code/releases/download/v3.0.5/Magi_3.0.5_darwin-aarch64-app.tar.gz',
);
assert.equal(manifest.platforms['darwin-aarch64-app'].signature, 'signed-macos');
assert.equal(manifest.platforms['darwin-x86_64-app'].signature, 'signed-macos-intel');
assert.equal(manifest.platforms['linux-x86_64-appimage'].signature, 'signed-linux');

assert.throws(
  () => createUpdateManifest({
    version: '3.0.5',
    tag: 'v3.0.5',
    repository: 'MistRipple/magi-code',
    platforms: [
      {
        target: 'darwin-aarch64-app',
        filename: 'Magi_3.0.5_darwin-aarch64-app.tar.gz',
        signature: '',
      },
    ],
  }),
  /signature is required/,
  'manifest generation must reject unsigned update targets',
);

const cliFixtureDir = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-update-manifest-'));
try {
  const fixtureAssets = [
    'Magi_3.0.5_darwin-aarch64-app.tar.gz',
    'Magi_3.0.5_darwin-x86_64-app.tar.gz',
    'Magi_3.0.5_linux-x86_64-appimage.AppImage',
    'Magi_3.0.5_windows-x86_64-nsis.exe',
  ];
  for (const filename of fixtureAssets) {
    fs.writeFileSync(path.join(cliFixtureDir, filename), 'artifact');
    fs.writeFileSync(path.join(cliFixtureDir, `${filename}.sig`), `signature-${filename}`);
  }
  const outputPath = path.join(cliFixtureDir, 'latest.json');
  execFileSync(process.execPath, [
    path.resolve('scripts/generate-update-manifest.mjs'),
    '--version', '3.0.5',
    '--tag', 'v3.0.5',
    '--repository', 'MistRipple/magi-code',
    '--assets-dir', cliFixtureDir,
    '--output', outputPath,
  ]);
  const cliManifest = JSON.parse(fs.readFileSync(outputPath, 'utf8'));
  assert.match(cliManifest.platforms['linux-x86_64'].url, /\.AppImage$/);
  assert.match(cliManifest.platforms['windows-x86_64'].url, /\.exe$/);
} finally {
  fs.rmSync(cliFixtureDir, { recursive: true, force: true });
}

console.log('update manifest golden replay passed');
