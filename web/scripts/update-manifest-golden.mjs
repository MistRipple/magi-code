import assert from 'node:assert/strict';
import { createUpdateManifest } from './generate-update-manifest.mjs';

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

console.log('update manifest golden replay passed');
