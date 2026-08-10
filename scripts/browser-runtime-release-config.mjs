import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const CONFIG_PATH = fileURLToPath(new URL('../config/browser-runtime-release.json', import.meta.url));
const config = JSON.parse(await readFile(CONFIG_PATH, 'utf8'));

if (!/^[^/]+\/[^/]+$/.test(config.repository)) {
  throw new Error('Browser Runtime 发布配置的 repository 无效');
}
if (!/^[A-Za-z0-9][A-Za-z0-9.-]*$/.test(config.stableReleaseTag)) {
  throw new Error('Browser Runtime 发布配置的 stableReleaseTag 无效');
}
if (!/^[0-9a-f]{64}$/.test(config.releasePublicKeyHex)) {
  throw new Error('Browser Runtime 发布配置的 releasePublicKeyHex 必须是 32 字节十六进制值');
}

const command = process.argv[2] ?? 'validate';
switch (command) {
  case 'validate':
    process.stdout.write(`${JSON.stringify(config)}\n`);
    break;
  case 'public-key':
    process.stdout.write(`${config.releasePublicKeyHex}\n`);
    break;
  case 'feed-url': {
    const os = process.argv[3];
    const arch = process.argv[4];
    if (!os || !arch) throw new Error('feed-url 需要 os 和 arch 参数');
    process.stdout.write(
      `https://github.com/${config.repository}/releases/download/${config.stableReleaseTag}/release-${os}-${arch}.json\n`,
    );
    break;
  }
  default:
    throw new Error(`未知命令：${command}`);
}
