import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

/**
 * 读取一个已按领域拆分的模块的完整源码：入口文件加上同名目录下的全部子模块。
 * 断言只关心“这个公开模块里是否接线了某个接口”，不应绑定在具体拆到了哪个文件。
 */
export function readSplitSource(entryFile) {
  const dir = entryFile.replace(/\.ts$/, '');
  const parts = [readFileSync(entryFile, 'utf8')];
  for (const name of readdirSync(dir).sort()) {
    if (name.endsWith('.ts')) parts.push(readFileSync(join(dir, name), 'utf8'));
  }
  return parts.join('\n');
}
