// 仅模型请求内的附件投影。会话附件仍归 Magi；这里不写 DSH 的持久存储。
import { AsyncLocalStorage } from 'node:async_hooks';
import { createHash } from 'node:crypto';
import sharp from 'sharp';
import { modelImage } from './browser-image.mjs';

const requests = new AsyncLocalStorage();
const MAX_BYTES = 24 * 1024 * 1024;
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');
const abort = (signal) => signal?.throwIfAborted();
const map = () => {
  const current = requests.getStore();
  if (!current) throw new Error('附件仅在当前 Magi 模型请求内可读，不提供第二套持久存储');
  return current;
};

export const withRequestImages = (run) => requests.run(new Map(), run);
// 活动模型请求独占其临时引用；无请求上下文时才使用插件持久附件服务。
export const scopedRequestImage = (ref, signal) => requests.getStore() ? requestAttachments.readImage(ref, signal) : undefined;

export async function saveRequestImage(url) {
  const match = typeof url === 'string' && /^data:(image\/(?:png|jpeg|webp|gif|avif|bmp|x-icon|svg\+xml));base64,([A-Za-z0-9+/]*={0,2})$/.exec(url);
  if (!match) throw new Error('模型桥只接受 Magi 已上传的 base64 图片，不代取远程 URL');
  const input = Buffer.from(match[2], 'base64');
  if (input.toString('base64') !== match[2] || !input.length || input.length > MAX_BYTES) throw new Error('图片编码无效或超过 24 MiB');
  if ([...map().values()].reduce((total, entry) => total + entry.ref.bytes, 0) + input.length > 32 * 1024 * 1024) throw new Error('单次模型请求图片超过 32 MiB');
  if (match[1] === 'image/svg+xml') {
    const svg = input.toString('utf8');
    if (!/^\s*(?:<\?xml[^>]*>\s*)?<svg\b/i.test(svg)
      || /<\s*script\b|\bon[a-z]+\s*=|(?:href|src)\s*=\s*["']\s*(?:https?:|data:(?!image\/|font\/)|javascript:)/i.test(svg)) {
      throw new Error('SVG 图片包含无效根节点或活动/外部内容');
    }
  }
  const image = await modelImage(input, match[1]);
  // 完整解码并规范化，变体始终是 8 位 sRGB，不伪造深度或尺寸。
  const { data, info } = await image.rotate().toColourspace('srgb').png().toBuffer({ resolveWithObject: true });
  if (data.length > MAX_BYTES) throw new Error('规范化图片超过 24 MiB');
  const ref = Object.freeze({ attachmentId: digest(data), mediaType: 'image/png', bytes: data.length, width: info.width, height: info.height });
  map().set(ref.attachmentId, { ref, data });
  return ref;
}

export const requestAttachments = {
  async readImage(ref, signal) {
    abort(signal);
    const item = map().get(ref?.attachmentId);
    if (!item || ['mediaType', 'bytes', 'width', 'height'].some((key) => item.ref[key] !== ref[key])) throw new Error('图片引用不属于当前请求或元数据不匹配');
    return { ref: item.ref, data: Buffer.from(item.data) };
  },
  async readImageRequest(ref, target, signal) {
    const { data } = await requestAttachments.readImage(ref, signal);
    return prepareRequestImage(ref, data, target, signal);
  },
  imageHostPath() { return undefined; },
  fileHostPath() { return undefined; },
};

export async function prepareRequestImage(ref, data, target, signal) {
    if (![target?.width, target?.height, target?.maxBytes].every((n) => Number.isFinite(n) && n > 0)) throw new Error('图片请求目标无效');
    let result;
    for (const quality of [90, 75, 60, 40]) {
      result = await (await modelImage(data, ref.mediaType)).resize({ width: Math.min(ref.width, Math.floor(target.width)), height: Math.min(ref.height, Math.floor(target.height)), fit: 'inside', withoutEnlargement: true }).flatten({ background: '#ffffff' }).toColourspace('srgb').jpeg({ quality }).toBuffer({ resolveWithObject: true });
      abort(signal);
      if (result.data.length <= target.maxBytes) break;
    }
    return { variantId: digest(result.data), attachment: ref, data: result.data, mediaType: 'image/jpeg', bytes: result.data.length, width: result.info.width, height: result.info.height, depth: 'uchar', space: 'srgb', hasAlpha: false };
}
