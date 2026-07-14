import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const workflow = await readFile(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');

assert.match(workflow, /RUST_TOOLCHAIN:\s*"1\.97\.0"/, 'CI must pin the Rust toolchain');
assert.match(workflow, /runs-on:\s*ubuntu-22\.04/, 'Linux CI must use a pinned runner image');
assert.match(workflow, /runs-on:\s*windows-2022/, 'Windows CI must use a pinned runner image');
assert.match(workflow, /cancel-in-progress:\s*true/, 'stale CI runs must be cancelled');
assert.match(workflow, /cargo test --workspace --locked/, 'CI must run workspace tests');
assert.match(
  workflow,
  /for attempt in 1 2 3; do[\s\S]*?npm --prefix web audit --omit=dev/,
  'transient npm audit transport failures must be retried without hiding persistent failures',
);
assert.doesNotMatch(
  workflow,
  /cargo test --workspace --all-targets/,
  'CI must not re-run bench and example targets as integration tests',
);

console.log('CI workflow golden replay passed');
