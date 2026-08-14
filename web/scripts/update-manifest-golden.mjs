import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { load } from 'js-yaml';
import { mergeUpdateMetadata, verifyUpdateMetadata } from '../../apps/desktop/scripts/merge-update-metadata.mjs';

const [releaseWorkflow, builderConfig] = await Promise.all([
  readFile(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/electron-builder.yml', import.meta.url), 'utf8'),
]);

assert.match(builderConfig, /publish:[\s\S]*provider: github[\s\S]*owner: MistRipple[\s\S]*repo: magi-code/, 'Electron updater 必须使用产品 GitHub Release');
assert.match(builderConfig, /notarize: true[\s\S]*dmg:[\s\S]*sign: true/, 'macOS 包必须签名并公证');
assert.match(releaseWorkflow, /latest\.yml[\s\S]*latest-linux\.yml[\s\S]*latest-mac\.yml/, 'Release 必须发布 Electron 原生更新元数据');
assert.doesNotMatch(releaseWorkflow, /latest\.json|magi-desktop-stable|\.AppImage\.sig|\.exe\.sig/, '不得保留旧更新清单和更新 feed');

const directory = await mkdtemp(join(tmpdir(), 'magi-electron-update-'));
try {
  const armName = 'Magi-3.0.43-mac-arm64.zip';
  const x64Name = 'Magi-3.0.43-mac-x64.zip';
  const armBytes = Buffer.from('arm64');
  const x64Bytes = Buffer.from('x64');
  await writeFile(join(directory, armName), armBytes);
  await writeFile(join(directory, x64Name), x64Bytes);
  const armHash = createHash('sha512').update(armBytes).digest('base64');
  const x64Hash = createHash('sha512').update(x64Bytes).digest('base64');
  const armMetadata = join(directory, 'arm.yml');
  const x64Metadata = join(directory, 'x64.yml');
  await writeFile(armMetadata, `version: 3.0.43\nfiles:\n  - url: ${armName}\n    sha512: ${armHash}\npath: ${armName}\nsha512: ${armHash}\n`);
  await writeFile(x64Metadata, `version: 3.0.43\nfiles:\n  - url: ${x64Name}\n    sha512: ${x64Hash}\npath: ${x64Name}\nsha512: ${x64Hash}\n`);
  const output = join(directory, 'latest-mac.yml');
  await mergeUpdateMetadata([armMetadata, x64Metadata], output);
  await verifyUpdateMetadata(output, directory);
  const merged = load(await readFile(output, 'utf8'));
  assert.deepEqual(merged.files.map((value) => value.url), [armName, x64Name]);
  await writeFile(join(directory, armName), 'corrupted');
  await assert.rejects(verifyUpdateMetadata(output, directory), /哈希不匹配/, '更新元数据必须拒绝被替换的发行文件');
} finally {
  await rm(directory, { recursive: true, force: true });
}

console.log('Electron update metadata golden replay passed');
