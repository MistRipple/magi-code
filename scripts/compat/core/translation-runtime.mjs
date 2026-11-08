// 发行构建生成一次身份，daemon 和安装准备读取同一事实；不在 UI 推断运行库版本。
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';

export function writeTranslationRuntime(root) {
  const files = ['cli.mjs', 'index.mjs', 'package.json', 'package-lock.json'];
  const collect = directory => {
    for (const entry of readdirSync(join(root, directory), { withFileTypes: true })) {
      const path = `${directory}/${entry.name}`;
      if (entry.name === '.gitkeep' || path === 'dist/translation-runtime.json') continue;
      if (entry.isDirectory()) collect(path);
      else if (entry.isFile()) files.push(path);
      else throw new Error(`运行库含链接或特殊文件：${path}`);
    }
  };
  for (const directory of ['core', 'dsh', 'pi', 'runtime', 'install', 'dist']) collect(directory);
  const hash = createHash('sha256');
  for (const path of files.sort()) {
    hash.update(path); hash.update('\0'); hash.update(createHash('sha256').update(readFileSync(join(root, path))).digest());
  }
  const manifest = { format: 1, id: hash.digest('hex') };
  writeFileSync(join(root, 'dist/translation-runtime.json'), JSON.stringify(manifest) + '\n');
  return manifest;
}

export function translationRuntimeId(root) {
  const manifest = JSON.parse(readFileSync(join(root, 'dist/translation-runtime.json'), 'utf8'));
  if (manifest.format !== 1 || !/^[a-f0-9]{64}$/.test(manifest.id ?? '')) throw new Error('插件运行库身份无效，请更新 Magi 后重试');
  return manifest.id;
}
