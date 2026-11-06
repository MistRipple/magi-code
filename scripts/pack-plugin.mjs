#!/usr/bin/env node
// 把插件目录打成 Magi 发布包（ZIP）。实现见 compat/translate/pack.mjs（翻译器与本脚本共用，保证同一目录得到同一摘要）。
//
// 用法：node scripts/pack-plugin.mjs <插件目录> [输出文件]
// 目录须包含 manifest.json；声明 extension 的包还须包含 extension/index.mjs，视图资源放在 ui/ 下。
import { packPlugin } from './compat/translate/pack.mjs';

const [directory, output] = process.argv.slice(2);
if (!directory) {
  console.error('用法: node scripts/pack-plugin.mjs <插件目录> [输出文件]');
  process.exit(2);
}
try {
  const { target, size, files } = packPlugin(directory, output);
  console.log(`${target} (${size} 字节, ${files} 个文件)`);
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
