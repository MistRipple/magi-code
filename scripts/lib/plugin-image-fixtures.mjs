// Electron GUI 与模型桥共用真实编码样本，避免用改 MIME 的 PNG 冒充格式覆盖。
export async function browserImageFixtures(sharp) {
  const png = await sharp({ create: { width: 2, height: 2, channels: 4, background: '#e53935' } }).png().toBuffer();
  const bmp = Buffer.alloc(58);
  bmp.write('BM'); bmp.writeUInt32LE(58, 2); bmp.writeUInt32LE(54, 10);
  bmp.writeUInt32LE(40, 14); bmp.writeInt32LE(1, 18); bmp.writeInt32LE(1, 22);
  bmp.writeUInt16LE(1, 26); bmp.writeUInt16LE(24, 28); bmp.writeUInt32LE(4, 34); bmp[56] = 255;
  const icon = (data, width, height) => {
    const ico = Buffer.alloc(22 + data.length);
    ico.writeUInt16LE(1, 2); ico.writeUInt16LE(1, 4); ico[6] = width; ico[7] = height;
    ico.writeUInt16LE(1, 10); ico.writeUInt16LE(32, 12); ico.writeUInt32LE(data.length, 14); ico.writeUInt32LE(22, 18); data.copy(ico, 22);
    return ico;
  };
  // 独立 DIB 编码的 ICO；不是 PNG 容器的第二个名字。
  const dib = Buffer.alloc(48);
  dib.writeUInt32LE(40, 0); dib.writeInt32LE(1, 4); dib.writeInt32LE(2, 8);
  dib.writeUInt16LE(1, 12); dib.writeUInt16LE(32, 14); dib[42] = 255; dib[43] = 255;
  return [
    ['png', 'image/png', png, 2, 2],
    ['jpg', 'image/jpeg', await sharp(png).jpeg().toBuffer(), 2, 2],
    ['gif', 'image/gif', await sharp(png).gif().toBuffer(), 2, 2],
    ['webp', 'image/webp', await sharp(png).webp().toBuffer(), 2, 2],
    ['avif', 'image/avif', await sharp(png).avif().toBuffer(), 2, 2],
    ['bmp', 'image/bmp', bmp, 1, 1],
    ['ico', 'image/x-icon', icon(png, 2, 2), 2, 2],
    ['dib.ico', 'image/x-icon', icon(dib, 1, 1), 1, 1],
    ['svg', 'image/svg+xml', Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="red"/></svg>'), 2, 2],
  ];
}

export function bmpEncodingFixtures() {
  return [1, 4, 8, 16, 32, 'rle4', 'rle8'].map((encoding) => {
    const depth = encoding === 'rle4' ? 4 : encoding === 'rle8' ? 8 : encoding;
    const compressed = typeof encoding === 'string';
    const colors = depth <= 8 ? 2 : 0;
    const offset = 54 + colors * 4;
    const pixels = compressed ? Buffer.from([2, depth === 4 ? 0x11 : 1, 0, 1]) : Buffer.alloc(depth === 32 ? 8 : 4);
    if (!compressed) {
      if (depth === 1) pixels[0] = 0xc0;
      else if (depth === 4) pixels[0] = 0x11;
      else if (depth === 8) { pixels[0] = 1; pixels[1] = 1; }
      else if (depth === 16) { pixels.writeUInt16LE(0x7c00, 0); pixels.writeUInt16LE(0x7c00, 2); }
      else { pixels[2] = 255; pixels[6] = 255; }
    }
    const data = Buffer.alloc(offset + pixels.length);
    data.write('BM'); data.writeUInt32LE(data.length, 2); data.writeUInt32LE(offset, 10);
    data.writeUInt32LE(40, 14); data.writeInt32LE(2, 18); data.writeInt32LE(1, 22);
    data.writeUInt16LE(1, 26); data.writeUInt16LE(depth, 28); data.writeUInt32LE(encoding === 'rle4' ? 2 : encoding === 'rle8' ? 1 : 0, 30);
    data.writeUInt32LE(pixels.length, 34); data.writeUInt32LE(colors, 46);
    if (colors) data[60] = 255;
    pixels.copy(data, offset);
    return [String(encoding), data];
  });
}
