// Conversion only: approved artwork is kept in icons/source.
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
if (process.platform !== 'darwin') throw new Error('需要 macOS 的 sips 和 iconutil');
const root = fileURLToPath(new URL('../', import.meta.url));
const icons = join(root, 'icons');
const tray = join(root, 'resources', 'tray');
const sizes = [16, 24, 32, 48, 64, 96, 128, 256, 512, 1024];
const temporary = await mkdtemp(join(tmpdir(), 'magi-icons-'));
function resize(source, target, size, dpi = 72) {
  execFileSync('sips', ['-z', String(size), String(size), '-s', 'dpiWidth', String(dpi), '-s', 'dpiHeight', String(dpi), source, '--out', target], { stdio: 'pipe' });
}
try {
  await mkdir(tray, { recursive: true });
  const source = join(icons, 'source', 'app.png');
  for (const size of sizes) resize(source, join(icons, size + 'x' + size + '.png'), size);
  resize(source, join(icons, '128x128@2x.png'), 256, 144);
  const iconset = join(temporary, 'Magi.iconset');
  await mkdir(iconset);
  for (const size of [16, 32, 128, 256, 512]) {
    for (const scale of [1, 2]) resize(source, join(iconset, 'icon_' + size + 'x' + size + (scale === 2 ? '@2x' : '') + '.png'), size * scale, 72 * scale);
  }
  execFileSync('iconutil', ['-c', 'icns', iconset, '-o', join(icons, 'icon.icns')]);
  // ICO directory with individual PNG payloads for Windows DPI sizes.
  const icoSizes = sizes.filter(size => size <= 256);
  const images = await Promise.all(icoSizes.map(size => readFile(join(icons, size + 'x' + size + '.png'))));
  const header = Buffer.alloc(6 + images.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  for (let index = 0; index < images.length; index++) {
    const entry = 6 + index * 16;
    header[entry] = header[entry + 1] = icoSizes[index] === 256 ? 0 : icoSizes[index];
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(images[index].length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += images[index].length;
  }
  await writeFile(join(icons, 'icon.ico'), Buffer.concat([header, ...images]));
  resize(join(icons, 'source', 'tray.png'), join(tray, 'magiTemplate.png'), 18);
  resize(join(icons, 'source', 'tray.png'), join(tray, 'magiTemplate@2x.png'), 36, 144);
  console.log('桌面 PNG / ICNS / ICO 与 macOS 18pt / Retina 模板图标已导出');
} finally {
  await rm(temporary, { recursive: true, force: true });
}
