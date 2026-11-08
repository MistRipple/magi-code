import { readFileSync, copyFileSync } from 'node:fs';
import { join } from 'node:path';
import { BROWSER_LIBRARIES } from '../runtime/client/modules.mjs';

// 公共页面 SDK 是宿主的固定合同。原包可在交互后经注入函数间接取用组件，
// 不以源码里直接 require 的出现与否裁剪运行库，避免页面加载成功却缺失操作。
export function copyBrowserAssets(sharedRoot, destination) {
  const manifest = JSON.parse(readFileSync(join(sharedRoot, 'dist/browser-modules.json'), 'utf8'));
  const files = BROWSER_LIBRARIES.flatMap((library) => {
    if (!manifest[library.name]) throw new Error(`发行包缺少浏览器模块 ${library.name}`);
    return manifest[library.name];
  });
  for (const file of files) {
    if (!/^[a-z0-9-]+\.(js|css)$/.test(file)) throw new Error('浏览器运行库清单路径无效');
    copyFileSync(join(sharedRoot, 'dist', file), join(destination, file));
  }
  return { scripts: files.filter((file) => file.endsWith('.js')), styles: files.filter((file) => file.endsWith('.css')) };
}
