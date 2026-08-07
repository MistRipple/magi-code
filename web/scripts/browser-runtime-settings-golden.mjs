import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const source = fs.readFileSync(
  path.resolve('src/components/SettingsBrowserTab.svelte'),
  'utf8',
);

assert.match(
  source,
  /async function pollRuntimeStatus\(generation: number\)/,
  'browser runtime settings must observe authoritative status during an operation',
);
assert.match(
  source,
  /await fetchSnapshot\(\);[\s\S]*?setTimeout\(resolve, 350\)/,
  'browser runtime status polling must refresh continuously instead of waiting for the action request',
);
assert.match(
  source,
  /next\.revision >= snapshot\.revision/,
  'status polling must reject stale responses',
);
assert.match(
  source,
  /await statusPolling;/,
  'runtime action completion must wait for the observer to stop cleanly',
);
assert.match(source, /settings\.browser\.checkingUpdates/);
assert.match(source, /settings\.browser\.installing/);
assert.match(source, /settings\.browser\.uninstalling/);
assert.match(
  source,
  /settings\.browser\.installSucceeded/,
  'successful installation must report activation completion to the user',
);

console.log('browser runtime settings golden tests passed');
