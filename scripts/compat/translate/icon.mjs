// 上游 package.json 的 `icon` 校验：只接受包内 png/jpg/webp 位图（SVG 可含脚本，不放行）。
// 返回要复制到 Magi 包 ui/ 下的来源路径与扩展名；上游声明了图标却不能交付时明确失败。
import { existsSync, readFileSync } from 'node:fs';
import { resolve, sep } from 'node:path';

export const ICON_MAX_BYTES = 256 * 1024;
const ICON_HEADERS = {
  png: (b) => b.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])),
  jpg: (b) => b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff,
  webp: (b) => b.length > 12 && b.subarray(0, 4).toString() === 'RIFF' && b.subarray(8, 12).toString() === 'WEBP',
};

/** @returns {{ from: string, to: string } | null} from 是上游包内的相对路径 */
export function resolveUpstreamIcon({ upstreamDir, pkg }) {
  if (pkg.icon === undefined) return null;
  if (typeof pkg.icon !== 'string' || !pkg.icon) throw new Error('上游 package.json 的 icon 必须是包内文件路径');
  const root = resolve(upstreamDir);
  const file = resolve(root, pkg.icon);
  if (file !== root && !file.startsWith(`${root}${sep}`)) throw new Error(`上游图标指向包外：${pkg.icon}`);
  if (!existsSync(file)) throw new Error(`上游声明了图标但文件不存在：${pkg.icon}`);
  const extension = file.split('.').pop().toLowerCase().replace('jpeg', 'jpg');
  if (!ICON_HEADERS[extension]) throw new Error(`上游图标格式不受支持（只接受 png、jpg、webp 位图）：${pkg.icon}`);
  const bytes = readFileSync(file);
  if (bytes.length > ICON_MAX_BYTES) throw new Error(`上游图标超过 256 KiB：${pkg.icon}（${bytes.length} 字节）`);
  if (!ICON_HEADERS[extension](bytes)) throw new Error(`上游图标内容与其扩展名不符：${pkg.icon}`);
  return { from: pkg.icon.replace(/^\.\//, ''), to: `ui/icon.${extension}` };
}
