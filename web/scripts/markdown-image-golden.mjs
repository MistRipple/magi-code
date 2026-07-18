import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const image = await server.ssrLoadModule('/src/lib/markdown-image.ts');
  const markdownUrl = await server.ssrLoadModule('/src/lib/markdown-url.ts');

  assert.equal(
    image.resolveMarkdownImageFilePath('./images/logo.png', '/repo/docs/README.md'),
    '/repo/docs/images/logo.png',
    'Markdown 相对图片应以当前文档目录为基准解析',
  );
  assert.equal(
    image.resolveMarkdownImageFilePath('..\\assets\\logo.png', 'C:\\repo\\docs\\README.md'),
    'C:/repo/assets/logo.png',
    'Windows Markdown 图片应保留盘符并规范路径分隔符',
  );
  assert.equal(
    image.resolveMarkdownImageFilePath('images/logo.png'),
    'images/logo.png',
    '没有文档基准时应保留工作区相对图片路径',
  );
  assert.equal(
    image.resolveMarkdownImageFilePath('https://example.com/logo.png', '/repo/README.md'),
    null,
    '远程图片不应被转成工作区文件路径',
  );
  assert.equal(
    markdownUrl.sanitizeMarkdownUrl('./images/logo.png', { type: 'image', tag: 'img' }),
    './images/logo.png',
    '本地 Markdown 图片引用必须通过 URL 清洗',
  );
  assert.match(
    markdownUrl.sanitizeMarkdownUrl('data:image/png;base64,AAAA', { type: 'image', tag: 'img' }),
    /^data:image\/png;/u,
    '安全的栅格 data 图片应允许直接展示',
  );
  assert.equal(
    markdownUrl.sanitizeMarkdownUrl('javascript:alert(1)', { type: 'image', tag: 'img' }),
    '',
    '危险图片协议必须继续被拦截',
  );

  console.log('markdown image golden passed');
}, { configFile: 'vite.web.config.ts' });
