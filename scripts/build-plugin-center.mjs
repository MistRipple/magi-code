#!/usr/bin/env node
// 生成 Magi 官方插件中心（plugin-center/）：把 plugin-center/plugins.json 列出的插件目录打成发布包，
// 并生成目录文件 catalog.json。官方插件中心就是仓库里的这个文件夹，经 GitHub 提供，
// Magi 默认从 raw.githubusercontent.com 读取；上架一个插件 = 向仓库提交 PR，由代码审查把关。
//
// 用法：
//   node scripts/build-plugin-center.mjs          重新生成 plugin-center/packages 与 catalog.json
//   node scripts/build-plugin-center.mjs --check  只检查已提交的内容与源码一致（CI 使用）
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const root = resolve(new URL('..', import.meta.url).pathname);
const center = join(root, 'plugin-center');
const check = process.argv.includes('--check');
const plugins = JSON.parse(readFileSync(join(center, 'plugins.json'), 'utf8'));

const workspace = check ? mkdtempSync(join(tmpdir(), 'magi-plugin-center-')) : center;
try {
  const packages = join(workspace, 'packages');
  rmSync(packages, { recursive: true, force: true });
  mkdirSync(packages, { recursive: true });
  const archives = plugins.map(({ dir }) => {
    const manifest = JSON.parse(readFileSync(join(root, dir, 'manifest.json'), 'utf8'));
    const archive = join(packages, `${manifest.id}-${manifest.version}.zip`);
    execFileSync('node', [join(root, 'scripts/pack-plugin.mjs'), join(root, dir), archive], { stdio: 'inherit' });
    return archive;
  });
  execFileSync('node', [
    join(root, 'scripts/plugin-center.mjs'), 'publish',
    '--out', join(workspace, 'catalog.json'), '--prefix', 'packages/', '--reproducible', 'true', ...archives,
  ], { stdio: 'inherit' });

  if (check) {
    const mismatches = [];
    const compare = (relative) => {
      const committed = join(center, relative);
      const rebuilt = join(workspace, relative);
      if (!existsSync(committed) || !readFileSync(committed).equals(readFileSync(rebuilt))) mismatches.push(relative);
    };
    compare('catalog.json');
    for (const file of readdirSync(packages)) compare(join('packages', file));
    const committedPackages = existsSync(join(center, 'packages')) ? readdirSync(join(center, 'packages')) : [];
    for (const file of committedPackages) {
      if (!readdirSync(packages).includes(file)) mismatches.push(`${join('packages', file)}（源码已不再生成它）`);
    }
    if (mismatches.length > 0) {
      console.error(`plugin-center 与源码不一致，请运行 node scripts/build-plugin-center.mjs 后提交：\n  ${mismatches.join('\n  ')}`);
      process.exit(1);
    }
    console.log('plugin-center 与源码一致');
  }
} finally {
  if (check) rmSync(workspace, { recursive: true, force: true });
}
