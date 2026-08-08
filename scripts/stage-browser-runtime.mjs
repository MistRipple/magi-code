#!/usr/bin/env node

import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { chmod, copyFile, cp, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';

const args = parseArgs(process.argv.slice(2));
const root = path.resolve(required(args, 'root'));
const metadataPath = path.resolve(required(args, 'metadata'));
const browserHostRoot = path.resolve(args['browser-host-root'] ?? 'browser-host');
const hostEntry = path.join(browserHostRoot, 'dist', 'index.cjs');
const requireFromHost = createRequire(path.join(browserHostRoot, 'package.json'));
const playwrightRoot = path.dirname(requireFromHost.resolve('playwright-core/package.json'));
const { chromium } = requireFromHost('playwright-core');
const chromiumExecutable = path.resolve(
  args['chromium-executable'] ?? chromium.executablePath(),
);
const chromiumRoot = findChromiumRoot(chromiumExecutable);
const chromiumExecutableRelative = path.relative(chromiumRoot, chromiumExecutable);
const nodeName = process.platform === 'win32' ? 'node.exe' : 'node';
const nodeRoot = process.platform === 'win32'
  ? path.dirname(process.execPath)
  : path.dirname(path.dirname(process.execPath));
const stagedNodeRoot = path.join(root, 'node');
const stagedNode = process.platform === 'win32'
  ? path.join(stagedNodeRoot, nodeName)
  : path.join(stagedNodeRoot, 'bin', nodeName);
const stagedHost = path.join(root, 'host', 'index.cjs');
const stagedPlaywright = path.join(root, 'host', 'node_modules', 'playwright-core');
const stagedChromiumRoot = path.join(root, 'chromium');

await rm(root, { recursive: true, force: true });
await mkdir(path.dirname(stagedHost), { recursive: true });
await mkdir(path.dirname(stagedPlaywright), { recursive: true });
await mkdir(path.join(root, 'licenses'), { recursive: true });
await mkdir(path.dirname(stagedNode), { recursive: true });
await copyFile(process.execPath, stagedNode);
if (process.platform !== 'win32') {
  await chmod(stagedNode, 0o755);
  const nodeLibraryRoot = path.join(nodeRoot, 'lib');
  await cp(nodeLibraryRoot, path.join(stagedNodeRoot, 'lib'), {
    recursive: true,
    force: true,
    verbatimSymlinks: true,
  }).catch(() => undefined);
}
await copyFile(hostEntry, stagedHost);
await copyDirectory(playwrightRoot, stagedPlaywright);
await stageChromiumDirectory(chromiumRoot, stagedChromiumRoot);

await copyFile(
  path.join(playwrightRoot, 'LICENSE'),
  path.join(root, 'licenses', 'playwright-core.txt'),
);
const nodeLicense = await findNodeLicense(process.execPath);
if (nodeLicense) {
  await copyFile(nodeLicense, path.join(root, 'licenses', 'node.txt'));
}
await writeFile(
  path.join(root, 'licenses', 'chromium.txt'),
  [
    'Chromium is distributed under the Chromium project licenses.',
    'See chrome://credits in the packaged runtime and https://www.chromium.org/Home/chromium-security/ for upstream notices.',
    '',
  ].join('\n'),
  'utf8',
);

const stagedChromiumExecutable = path.join(stagedChromiumRoot, chromiumExecutableRelative);
const chromiumRelative = toManifestPath(path.join('chromium', chromiumExecutableRelative));
const chromiumVersionOutput = execFileSync(stagedChromiumExecutable, ['--version'], {
  encoding: 'utf8',
}).trim();
const chromiumVersion = chromiumVersionOutput.match(/\d+(?:\.\d+){3}/)?.[0];
if (!chromiumVersion) {
  throw new Error(`无法识别 Chromium 版本：${chromiumVersionOutput}`);
}
const playwrightPackage = JSON.parse(
  await readFile(path.join(playwrightRoot, 'package.json'), 'utf8'),
);
const protocolSource = await readFile(path.join(browserHostRoot, 'src', 'protocol.ts'), 'utf8');
const protocolMatch = protocolSource.match(
  /PROTOCOL_VERSION\s*=\s*\{\s*major:\s*(\d+),\s*minor:\s*(\d+)\s*\}/,
);
if (!protocolMatch) throw new Error('无法读取 Browser Host 协议版本');
const metadata = {
  root,
  nodeVersion: process.versions.node,
  playwrightVersion: playwrightPackage.version,
  chromiumVersion,
  nodeExecutablePath: toManifestPath(path.relative(root, stagedNode)),
  hostEntryPath: 'host/index.cjs',
  chromiumExecutablePath: chromiumRelative,
  protocolMajor: Number(protocolMatch[1]),
  protocolMinor: Number(protocolMatch[2]),
};
await mkdir(path.dirname(metadataPath), { recursive: true });
await writeFile(metadataPath, `${JSON.stringify(metadata, null, 2)}\n`, 'utf8');
process.stdout.write(`${JSON.stringify(metadata)}\n`);

function parseArgs(values) {
  const parsed = {};
  for (let index = 0; index < values.length; index += 2) {
    const key = values[index];
    const value = values[index + 1];
    if (!key?.startsWith('--') || value === undefined) {
      throw new Error(`无效参数：${key ?? ''}`);
    }
    parsed[key.slice(2)] = value;
  }
  return parsed;
}

function required(values, key) {
  const value = values[key]?.trim();
  if (!value) throw new Error(`缺少参数 --${key}`);
  return value;
}

function findChromiumRoot(executable) {
  let current = path.dirname(executable);
  while (current !== path.dirname(current)) {
    if (/^chromium-\d+$/i.test(path.basename(current))) return current;
    current = path.dirname(current);
  }
  throw new Error(`Chromium 可执行文件不在 Playwright 运行目录中：${executable}`);
}

async function copyDirectory(source, destination) {
  if (process.platform !== 'win32') {
    await cp(source, destination, {
      recursive: true,
      force: true,
      verbatimSymlinks: true,
    });
    return;
  }

  // Windows 的 Chromium 目录包含大量二进制文件，Node fs.cp 在 runner 上可能长时间卡在递归复制。
  // 使用系统 robocopy，既能处理长路径，也不会把符号链接展开成递归目录。
  const result = spawnSync(
    'robocopy',
    [
      source,
      destination,
      '/E',
      '/SL',
      '/XJ',
      '/COPY:DAT',
      '/DCOPY:DAT',
      '/R:2',
      '/W:1',
      '/MT:32',
      '/J',
      '/NFL',
      '/NDL',
      '/NJH',
      '/NJS',
    ],
    { stdio: 'inherit', windowsHide: true },
  );
  if (result.error) throw result.error;
  if (result.status === null || result.status > 7) {
    throw new Error(`robocopy 复制运行组件失败，退出码：${result.status ?? 'unknown'}`);
  }
}

async function stageChromiumDirectory(source, destination) {
  if (process.platform === 'win32') {
    // CI 将 Playwright 浏览器缓存放在工作区内，直接移动目录，避免 Windows runner
    // 对数十万 Chromium 文件做跨盘递归复制。
    await rename(source, destination);
    return;
  }
  await copyDirectory(source, destination);
}

async function findNodeLicense(executable) {
  let current = path.dirname(executable);
  for (let depth = 0; depth < 5; depth += 1) {
    for (const name of ['LICENSE', 'LICENSE.txt']) {
      const candidate = path.join(current, name);
      try {
        await readFile(candidate);
        return candidate;
      } catch {
        // 继续向 Node 安装目录上层查找。
      }
    }
    current = path.dirname(current);
  }
  return null;
}

function toManifestPath(value) {
  return value.split(path.sep).join('/');
}
