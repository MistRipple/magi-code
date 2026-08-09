import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

export async function readProductVersion() {
  const manifest = await readFile(new URL('../Cargo.toml', import.meta.url), 'utf8');
  const sectionHeader = '[workspace.package]';
  const sectionStart = manifest.indexOf(sectionHeader);
  if (sectionStart < 0) {
    throw new Error('Cargo.toml 缺少 [workspace.package]');
  }

  const sectionBodyStart = sectionStart + sectionHeader.length;
  const nextSectionOffset = manifest.slice(sectionBodyStart).search(/^\[[^\r\n]+\]\s*$/m);
  const sectionEnd = nextSectionOffset < 0
    ? manifest.length
    : sectionBodyStart + nextSectionOffset;
  const workspacePackage = manifest.slice(sectionBodyStart, sectionEnd);
  const version = workspacePackage.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];

  if (!version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error('Cargo.toml 的 [workspace.package].version 不是有效 SemVer');
  }

  return version;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.stdout.write(`${await readProductVersion()}\n`);
}
