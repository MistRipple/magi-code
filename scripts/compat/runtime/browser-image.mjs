// 只在模型传输边界生成派生位图；GUI 与 canonical 消息仍保留原始格式。
import sharp from 'sharp';
import { decode as decodeBmp } from 'bmp-ts';
import bmp from 'bmp-js';
import decodeIco from 'decode-ico';

const MAX_PIXELS = 64_000_000;
const dimensions = (width, height) => {
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height) || width < 1 || height < 1 || width * height > MAX_PIXELS) throw new Error('图片尺寸无效或超过上限');
};

function bmpImage(input) {
  if (input.length < 54 || input.toString('ascii', 0, 2) !== 'BM') throw new Error('BMP 图片内容无效');
  const headerSize = input.readUInt32LE(14), offset = input.readUInt32LE(10);
  if (![40, 52, 56, 108, 124].includes(headerSize) || 14 + headerSize > input.length || offset < 14 + headerSize || offset >= input.length) throw new Error('BMP 图片头无效');
  const width = input.readInt32LE(18), height = Math.abs(input.readInt32LE(22));
  dimensions(width, height);
  const depth = input.readUInt16LE(28), colors = input.readUInt32LE(46);
  if (![1, 4, 8, 16, 24, 32].includes(depth) || colors > 256 || (depth <= 8 && colors > 2 ** depth)) throw new Error('BMP 调色板或色深无效');
  const compression = input.readUInt32LE(30);
  if (![0, 1, 2, 3, 6].includes(compression) || compression === 1 && depth !== 8 || compression === 2 && depth !== 4) throw new Error('BMP 压缩与色深不匹配');
  // bmp-ts 的 toRGBA 在调色板/RLE 分支不一致；使用其文档约定的 ABGR，
  // RLE 使用独立的成熟解码器，避免依赖其有缺陷的 RLE 像素写入实现。
  const decoded = compression === 1 || compression === 2 ? bmp.decode(input) : decodeBmp(input);
  const rgba = Buffer.alloc(decoded.data.length);
  // BMP 的保留字节不是透明度；没有显式 alpha mask 的位图必须按不透明图片发送。
  for (let at = 0; at < rgba.length; at += 4) {
    rgba[at] = decoded.data[at + 3]; rgba[at + 1] = decoded.data[at + 2]; rgba[at + 2] = decoded.data[at + 1];
    rgba[at + 3] = decoded.maskAlpha ? decoded.data[at] : 255;
  }
  return sharp(rgba, { raw: { width, height, channels: 4 } });
}

function icoImage(input) {
  if (input.length < 22 || input.readUInt32LE(0) !== 0x00010000) throw new Error('ICO 图片内容无效');
  const count = input.readUInt16LE(4);
  if (!count || count > 256 || 6 + count * 16 > input.length) throw new Error('ICO 图片目录无效');
  const entries = Array.from({ length: count }, (_, index) => {
    const at = 6 + index * 16;
    return { at, width: input[at] || 256, height: input[at + 1] || 256, bytes: input.readUInt32LE(at + 8), offset: input.readUInt32LE(at + 12), depth: input.readUInt16LE(at + 6) };
  }).sort((a, b) => b.width * b.height - a.width * a.height || b.depth - a.depth);
  const entry = entries[0];
  if (entry.bytes < 40 || entry.offset < 6 + count * 16 || entry.offset + entry.bytes > input.length) throw new Error('ICO 图片数据越界');
  const data = input.subarray(entry.offset, entry.offset + entry.bytes);
  if (data.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) {
    if (data.readUInt32BE(16) !== entry.width || data.readUInt32BE(20) !== entry.height) throw new Error('ICO PNG 尺寸与目录不一致');
  } else {
    const header = data.readUInt32LE(0), width = data.readInt32LE(4), height = data.readInt32LE(8) / 2;
    if (header < 40 || header > data.length || width !== entry.width || height !== entry.height || data.readUInt32LE(32) > 256) throw new Error('ICO 位图尺寸或调色板无效');
  }
  // 只解码最高分辨率一帧，避免解码库按整个图标目录重复分配像素。
  const single = Buffer.alloc(22 + data.length);
  input.copy(single, 0, 0, 6); single.writeUInt16LE(1, 4);
  input.copy(single, 6, entry.at, entry.at + 16); single.writeUInt32LE(22, 18); data.copy(single, 22);
  const decoded = decodeIco(single)[0];
  if (decoded.type === 'png') return sharp(decoded.data, { limitInputPixels: MAX_PIXELS });
  return sharp(decoded.data, { raw: { width: decoded.width, height: decoded.height, channels: 4 } });
}

export async function modelImage(input, mime) {
  if (mime === 'image/bmp') return bmpImage(input);
  if (mime === 'image/x-icon') return icoImage(input);
  const image = sharp(input, { limitInputPixels: MAX_PIXELS });
  const meta = await image.metadata();
  const detected = meta.mediaType ?? (meta.format === 'svg' ? 'image/svg+xml' : meta.format === 'heif' && meta.compression === 'av1' ? 'image/avif' : `image/${meta.format}`);
  if (detected !== mime) throw new Error('图片格式与声明不一致');
  dimensions(meta.width, meta.height);
  return image;
}
