import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const [workflow, securityWorkflow, releaseWorkflow, browserRuntimeReleaseWorkflow] = await Promise.all([
  readFile(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/security.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/browser-runtime-release.yml', import.meta.url), 'utf8'),
]);
const cargoManifest = await readFile(new URL('../../Cargo.toml', import.meta.url), 'utf8');
const webPackage = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
const desktopConfig = JSON.parse(await readFile(new URL('../../apps/desktop/tauri.conf.json', import.meta.url), 'utf8'));
const productVersionScript = fileURLToPath(new URL('../../scripts/product-version.mjs', import.meta.url));
const productVersion = execFileSync(process.execPath, [productVersionScript], { encoding: 'utf8' }).trim();
const cargoVersion = cargoManifest.match(
  /^\[workspace\.package\]\s*$[\s\S]*?^version\s*=\s*"([^"]+)"\s*$/m,
)?.[1];

assert.equal(productVersion, cargoVersion, 'product version helper must read Cargo workspace package version');
assert.equal(webPackage.version, undefined, 'web package must not duplicate the product version');
assert.equal(desktopConfig.version, undefined, 'Tauri config must inherit the desktop Cargo package version');

assert.match(workflow, /RUST_TOOLCHAIN:\s*"1\.97\.0"/, 'CI must pin the Rust toolchain');
assert.match(workflow, /runs-on:\s*ubuntu-22\.04/, 'Linux CI must use a pinned runner image');
assert.match(workflow, /runs-on:\s*windows-2022/, 'Windows CI must use a pinned runner image');
assert.match(workflow, /runs-on:\s*macos-15/, 'macOS CI must use a pinned runner image');
assert.match(workflow, /actions\/checkout@v7/g, 'CI must use the Node 24 checkout action');
assert.match(workflow, /actions\/setup-node@v7/, 'CI must use the Node 24 setup-node action');
assert.doesNotMatch(
  workflow,
  /actions\/(?:checkout|setup-node)@v4/,
  'CI must not depend on deprecated Node 20 action runtimes',
);
assert.match(workflow, /cancel-in-progress:\s*true/, 'stale CI runs must be cancelled');
assert.match(workflow, /cargo test --workspace --locked/, 'CI must run workspace tests');
assert.match(workflow, /cargo clippy --workspace --all-targets --locked -- -D warnings/, 'CI must lint every Rust target');
assert.match(workflow, /cargo test -p magi-desktop --all-targets --locked/, 'CI must test macOS desktop update behavior');
assert.match(
  workflow,
  /macos-desktop:[\s\S]*?npm --prefix web run build[\s\S]*?cargo test -p magi-desktop --all-targets --locked/,
  'macOS desktop tests must build the bundled frontend resources first',
);
assert.doesNotMatch(workflow, /cargo check --workspace --all-targets/, 'Clippy must own the all-target compilation gate');
assert.doesNotMatch(workflow, /(?:npm|cargo)\s+(?:--prefix web\s+)?audit/, 'dependency audits must not block ordinary CI changes');
assert.doesNotMatch(
  workflow,
  /cargo test --workspace --all-targets/,
  'CI must not re-run bench and example targets as integration tests',
);

assert.match(securityWorkflow, /schedule:[\s\S]*?cron:/, 'dependency audits must run on a schedule');
assert.match(securityWorkflow, /web\/package-lock\.json/, 'frontend lockfile changes must trigger dependency audits');
assert.match(securityWorkflow, /Cargo\.lock/, 'Rust lockfile changes must trigger dependency audits');
assert.match(
  securityWorkflow,
  /for attempt in 1 2 3; do[\s\S]*?npm --prefix web audit --omit=dev/,
  'transient npm audit transport failures must be retried without hiding persistent failures',
);
assert.match(securityWorkflow, /actions\/cache@v5[\s\S]*?~\/\.cargo\/bin\/cargo-audit/, 'cargo-audit must use a versioned binary cache');
assert.match(securityWorkflow, /cargo audit/, 'Rust dependencies must be audited');

assert.match(releaseWorkflow, /actions:\s*read/, 'the release gate must be allowed to read CI runs');
assert.match(releaseWorkflow, /verify-ci:[\s\S]*?gh run list[\s\S]*?--commit "\$GITHUB_SHA"/, 'releases must verify CI for the exact commit');
assert.match(releaseWorkflow, /build-web:[\s\S]*?needs:\s*verify-ci/, 'release builds must wait for the CI gate');
assert.match(releaseWorkflow, /npm --prefix web run build/, 'release packaging must create production web assets');
assert.doesNotMatch(releaseWorkflow, /npm --prefix web (?:run )?(?:check|test)/, 'release packaging must not repeat CI frontend validation');
assert.doesNotMatch(releaseWorkflow, /cargo test -p magi-desktop/, 'release packaging must not repeat CI desktop tests on every platform');
assert.match(releaseWorkflow, /node scripts\/product-version\.mjs/g, 'desktop releases must read the authoritative Cargo workspace version');
assert.doesNotMatch(
  releaseWorkflow,
  /(?:web\/package\.json|apps\/desktop\/tauri\.conf\.json)'?\)\.version/,
  'desktop releases must not read duplicate product versions',
);

assert.match(browserRuntimeReleaseWorkflow, /push:[\s\S]*?tags:[\s\S]*?-\s*"v\*"/, 'Browser Runtime must follow product release tags');
assert.doesNotMatch(browserRuntimeReleaseWorkflow, /runtime_version:\s*\n/, 'Browser Runtime must not expose an independent version input');
assert.doesNotMatch(browserRuntimeReleaseWorkflow, /BROWSER_RUNTIME_MANIFEST_SEQUENCE/, 'Browser Runtime must not depend on a manually maintained sequence variable');
assert.match(browserRuntimeReleaseWorkflow, /prepare-release:[\s\S]*?manifest_sequence:/, 'Browser Runtime must create one shared manifest sequence before platform packaging');
assert.match(browserRuntimeReleaseWorkflow, /needs\.prepare-release\.outputs\.manifest_sequence/, 'every platform package must use the prepared manifest sequence');
assert.match(browserRuntimeReleaseWorkflow, /concurrency:[\s\S]*?browser-runtime-stable-release[\s\S]*?cancel-in-progress:\s*false/, 'Browser Runtime publications must serialize stable-feed sequence allocation');
assert.match(browserRuntimeReleaseWorkflow, /runtime_tag="browser-runtime-v\$\{runtime_version\}"/, 'Runtime archive URLs must use the derived version');
assert.match(
  browserRuntimeReleaseWorkflow,
  /magi_version="\$\(node scripts\/product-version\.mjs\)"/,
  'Browser Runtime must derive its version from the authoritative Cargo workspace version',
);
assert.match(
  browserRuntimeReleaseWorkflow,
  /gh release download magi-desktop-stable[\s\S]*latest\.json[\s\S]*minimum_magi_version=/,
  'Browser Runtime compatibility floor must follow the last stable desktop release',
);
assert.match(
  browserRuntimeReleaseWorkflow,
  /--minimum-magi-version "\$minimum_magi_version"/,
  'Runtime manifests must use the derived compatibility floor instead of duplicating the product version',
);
assert.match(
  browserRuntimeReleaseWorkflow,
  /gh release upload "\$runtime_tag"[\s\S]*?dist\/browser-runtime\/release-\*\.json/,
  'versioned Runtime releases must stage feed manifests for desktop release promotion',
);
assert.doesNotMatch(
  browserRuntimeReleaseWorkflow,
  /gh release upload browser-runtime-stable/,
  'Runtime packaging must not promote the stable feed before the desktop release succeeds',
);
assert.match(
  releaseWorkflow,
  /等待并校验同版本 Browser Runtime[\s\S]*?browser-runtime-v\$\{version\}[\s\S]*?创建 Release 并上传产物[\s\S]*?更新桌面端稳定版 Feed[\s\S]*?提升 Browser Runtime 稳定版 Feed/,
  'desktop publication must validate the matching Runtime and promote it only after the desktop feed',
);
assert.match(
  releaseWorkflow,
  /minimumVersions = new Set\(feeds\.map\(\(feed\) => feed\.release\.manifest\.minimum_magi_version\)\)/,
  'desktop publication must require one shared Runtime compatibility floor across platforms',
);

const sequenceScript = fileURLToPath(new URL('../../scripts/browser-runtime-manifest-sequence.mjs', import.meta.url));
const sequenceFloor = execFileSync(process.execPath, [sequenceScript, '3.0.40'], { encoding: 'utf8' }).trim();
assert.equal(sequenceFloor, '3000000000040', 'product version must map above the legacy date-based manifest sequence range');

console.log('CI workflow golden replay passed');
