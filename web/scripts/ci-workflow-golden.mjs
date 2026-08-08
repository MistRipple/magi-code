import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const [workflow, securityWorkflow, releaseWorkflow, browserRuntimeReleaseWorkflow] = await Promise.all([
  readFile(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/security.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/release.yml', import.meta.url), 'utf8'),
  readFile(new URL('../../.github/workflows/browser-runtime-release.yml', import.meta.url), 'utf8'),
]);

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

assert.match(browserRuntimeReleaseWorkflow, /push:[\s\S]*?tags:[\s\S]*?-\s*"v\*"/, 'Browser Runtime must follow product release tags');
assert.doesNotMatch(browserRuntimeReleaseWorkflow, /runtime_version:\s*\n/, 'Browser Runtime must not expose an independent version input');
assert.match(browserRuntimeReleaseWorkflow, /runtime_version="\$magi_version"/, 'Browser Runtime version must derive from the product version');
assert.match(browserRuntimeReleaseWorkflow, /test "\$tag_version" = "\$magi_version"/, 'Browser Runtime must reject mismatched product tags');
assert.match(browserRuntimeReleaseWorkflow, /runtime_tag="browser-runtime-v\$\{runtime_version\}"/, 'Runtime archive URLs must use the derived version');

console.log('CI workflow golden replay passed');
