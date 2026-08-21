import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { access, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const [ci, security, release, cargo, rootPackage, webPackage, desktopPackage, lock] = await Promise.all([
  readFile(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/security.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../Cargo.toml', import.meta.url), 'utf8'),
  readFile(new URL('../../package.json', import.meta.url), 'utf8').then(JSON.parse),
  readFile(new URL('../package.json', import.meta.url), 'utf8').then(JSON.parse),
  readFile(new URL('../../apps/desktop/package.json', import.meta.url), 'utf8').then(JSON.parse),
  readFile(new URL('../../package-lock.json', import.meta.url), 'utf8'),
]);
const productVersion = execFileSync(process.execPath, [
  fileURLToPath(new URL('../../scripts/product-version.mjs', import.meta.url)),
], { encoding: 'utf8' }).trim();
const cargoVersion = cargo.match(/^\[workspace\.package\]\s*$[\s\S]*?^version\s*=\s*"([^"]+)"\s*$/m)?.[1];

assert.equal(productVersion, cargoVersion, '产品版本必须只来自 Cargo workspace');
assert.equal(rootPackage.version, '0.0.0', 'npm workspace 只能使用非产品占位版本');
assert.equal(desktopPackage.version, '0.0.0', 'Electron package 版本必须在构建时由 Cargo 注入');
assert.equal(webPackage.version, undefined, 'Web 不得复制产品版本');

assert.match(ci, /npm ci[\s\S]*npm run check[\s\S]*npm run test/, 'CI 必须从根 lockfile 验证全部 workspace');
assert.match(ci, /npm run release:guard/, 'CI 必须执行废码门禁');
assert.match(ci, /cargo clippy --workspace --all-targets --locked -- -D warnings/, 'CI 必须执行 Rust 全量 Clippy');
assert.match(ci, /cargo test --workspace --locked/, 'CI 必须执行 Rust workspace 测试');
assert.match(security, /package-lock\.json[\s\S]*npm audit --omit=dev --workspaces/, '安全检查必须使用根 lockfile 审计生产依赖');

assert.match(release, /verify-ci:[\s\S]*gh run list[\s\S]*--commit "\$GITHUB_SHA"/, '发布必须校验同一提交的 CI');
assert.match(release, /node scripts\/product-version\.mjs/, '发布必须读取 Cargo 产品版本');
assert.match(release, /npm run desktop:package/, '发布必须通过 Electron workspace 构建单一桌面包');
assert.match(release, /browser-capability-manifest\.json[\s\S]*magi-desktop\.cdx\.json/, '发布包必须包含能力清单和 SBOM');
assert.match(release, /CSC_IDENTITY_AUTO_DISCOVERY:\s*"false"/, '未签名发行必须关闭 macOS 证书自动发现');
assert.doesNotMatch(
  release,
  /MACOS_CSC_LINK|APPLE_API_KEY|WINDOWS_CSC_LINK|Get-AuthenticodeSignature|RELEASE_GPG_PRIVATE_KEY|--detach-sign|notarytool|stapler validate/,
  '未签名发行流程不得读取证书、执行公证、验证 Authenticode 或生成 GPG 签名',
);
assert.match(release, /actions\/attest-build-provenance@v3/, '最终发行文件必须生成 GitHub 构建来源证明');
assert.match(release, /latest\.yml[\s\S]*latest-linux\.yml[\s\S]*latest-mac\.yml/, 'Electron 更新元数据必须覆盖三个桌面平台');
assert.match(release, /make_latest:\s*true/, 'Desktop Release 必须显式成为 GitHub latest Release');
assert.match(release, /GitHub Desktop 更新源[\s\S]*releases\/latest[\s\S]*latest-mac\.yml[\s\S]*latest-linux\.yml/, '发布流程必须校验统一 Desktop 更新源');
assert.match(release, /真实启动解包 Desktop[\s\S]*\/health[\s\S]*\/web\.html/, '发布链必须真实启动解包 Desktop');
assert.doesNotMatch(
  `${ci}\n${security}\n${release}`,
  /@tauri-apps|\btauri\b|chromium embedded framework|\bcef\b|browser-runtime-release|browser-host|native-browser-runtime|playwright/i,
  '工作流不得残留旧桌面宿主、独立 Runtime 或生产 Playwright',
);
assert.doesNotMatch(lock, /@tauri-apps|playwright(?:-core)?|browser-host/i, '根 npm lockfile 不得包含旧生产依赖');

for (const relativePath of [
  '../../.github/workflows/browser-runtime-release.yml',
  '../../browser-host',
  '../../config/native-browser-runtime.json',
  '../../config/browser-runtime-release.json',
]) {
  await assert.rejects(access(new URL(relativePath, import.meta.url)), `废弃资产必须删除: ${relativePath}`);
}

console.log('CI workflow golden replay passed');
