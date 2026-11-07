// 保留上游图片格式与原始字节，包括 SVG；最终内容校验由 Magi 包准入统一执行。
import { existsSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { resolve, sep } from 'node:path';

export const ICON_MAX_BYTES = 256 * 1024;
const ICON_HEADERS = {
  png: (b) => b.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])),
  jpg: (b) => b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff,
  webp: (b) => b.length > 12 && b.subarray(0, 4).toString() === 'RIFF' && b.subarray(8, 12).toString() === 'WEBP',
  gif: (b) => /^GIF8[79]a/.test(b.subarray(0, 6).toString()),
  avif: (b) => b.length >= 12 && b.subarray(4, 8).toString() === 'ftyp' && /avif|avis/.test(b.subarray(8, 32).toString()),
  bmp: (b) => b.subarray(0, 2).toString() === 'BM',
  ico: (b) => b.subarray(0, 4).equals(Buffer.from([0, 0, 1, 0])),
};

export async function resolveUpstreamIcon({ upstreamDir, pkg }) {
  if (pkg.icon === undefined) return null;
  if (typeof pkg.icon !== 'string' || !pkg.icon) throw new Error('上游 package.json 的 icon 必须是包内文件路径');
  const root = realpathSync(resolve(upstreamDir));
  const file = resolve(root, pkg.icon);
  if (file !== root && !file.startsWith(`${root}${sep}`)) throw new Error(`上游图标指向包外：${pkg.icon}`);
  if (!existsSync(file)) throw new Error(`上游声明了图标但文件不存在：${pkg.icon}`);
  if (!realpathSync(file).startsWith(`${root}${sep}`) || !statSync(file).isFile()) throw new Error('上游图标必须是包内普通文件');
  const extension = file.split('.').pop().toLowerCase().replace('jpeg', 'jpg');
  if (!ICON_HEADERS[extension] && extension !== 'svg') throw new Error(`上游图标格式不受支持：${pkg.icon}`);
  if (statSync(file).size > ICON_MAX_BYTES) throw new Error(`上游图标超过 256 KiB：${pkg.icon}`);
  const bytes = readFileSync(file);
  if (bytes.length > ICON_MAX_BYTES) throw new Error(`上游图标超过 256 KiB：${pkg.icon}（${bytes.length} 字节）`);
  if (extension !== 'svg' && !ICON_HEADERS[extension](bytes)) throw new Error(`上游图标内容与其扩展名不符：${pkg.icon}`);
  return { to: `ui/icon.${extension}`, bytes };
}
