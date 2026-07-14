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

const releaseNotes = fs.readFileSync(path.resolve('../.github/releases/v3.0.1.md'), 'utf8');
assert.match(releaseNotes, /Turn ID/, '3.0.1 notes must explain the Turn ID race convergence');
assert.match(releaseNotes, /Windows|macOS|Linux/, '3.0.1 notes must explain desktop delivery scope');

const manifest = createUpdateManifest({
  version: '3.0.1',
  tag: 'v3.0.1',
  repository: 'MistRipple/magi-code',
  pubDate: '2026-07-14T00:00:00Z',
  notes: '测试更新',
  platforms: [
    {
      target: 'darwin-aarch64-app',
      filename: 'Magi_3.0.1_darwin-aarch64-app.tar.gz',
      signature: 'signed-macos',
    },
    {
      target: 'linux-x86_64-appimage',
      filename: 'Magi_3.0.1_linux-x86_64-appimage.tar.gz',
      signature: 'signed-linux',
    },
  ],
});

assert.equal(manifest.version, '3.0.1');
assert.equal(manifest.pub_date, '2026-07-14T00:00:00Z');
assert.equal(
  manifest.platforms['darwin-aarch64-app'].url,
  'https://github.com/MistRipple/magi-code/releases/download/v3.0.1/Magi_3.0.1_darwin-aarch64-app.tar.gz',
);
assert.equal(manifest.platforms['darwin-aarch64-app'].signature, 'signed-macos');
assert.equal(manifest.platforms['linux-x86_64-appimage'].signature, 'signed-linux');

assert.throws(
  () => createUpdateManifest({
    version: '3.0.1',
    tag: 'v3.0.1',
    repository: 'MistRipple/magi-code',
    platforms: [
      {
        target: 'darwin-aarch64-app',
        filename: 'Magi_3.0.1_darwin-aarch64-app.tar.gz',
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
    'Magi_3.0.1_darwin-aarch64-app.tar.gz',
    'Magi_3.0.1_linux-x86_64-appimage.AppImage',
    'Magi_3.0.1_windows-x86_64-nsis.exe',
  ];
  for (const filename of fixtureAssets) {
    fs.writeFileSync(path.join(cliFixtureDir, filename), 'artifact');
    fs.writeFileSync(path.join(cliFixtureDir, `${filename}.sig`), `signature-${filename}`);
  }
  const outputPath = path.join(cliFixtureDir, 'latest.json');
  execFileSync(process.execPath, [
    path.resolve('scripts/generate-update-manifest.mjs'),
    '--version', '3.0.1',
    '--tag', 'v3.0.1',
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
