import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { withGoldenViteServer } from './golden-vite.mjs';

const appSource = fs.readFileSync(path.resolve('src/App.svelte'), 'utf8');
const promptSource = fs.readFileSync(path.resolve('src/components/DesktopUpdatePrompt.svelte'), 'utf8');
assert.match(appSource, /DesktopUpdatePrompt/, 'desktop startup must mount the update prompt');
assert.match(
  promptSource,
  /setTimeout\(\(\) => void checkForUpdate\(\), 1200\)/,
  'desktop startup must perform a non-blocking update check',
);
const checkFunctionSource = promptSource.match(
  /async function checkForUpdate[\s\S]*?(?=\n  async function installUpdate)/,
)?.[0] ?? '';
assert.match(
  checkFunctionSource,
  /catch(?: \([^)]*\))? \{[\s\S]*?promptState = 'idle'[\s\S]*?error = ''/,
  'update discovery failures must remain silent instead of showing a startup error prompt',
);
assert.doesNotMatch(
  checkFunctionSource,
  /promptState = 'error'/,
  'only update installation failures may use the global desktop update error prompt',
);
assert.match(
  promptSource,
  /promptState === 'error'[\s\S]*?onclick=\{\(\) => void installUpdate\(\)\}/,
  'installation failures must retry installation instead of repeating update discovery',
);

await withGoldenViteServer(async (server) => {
  const updater = await server.ssrLoadModule('/src/lib/desktop-updater.ts');

  assert.deepEqual(
    updater.formatUpdateProgress(512, 1024),
    { downloadedBytes: 512, contentLength: 1024, percent: 50 },
    'update progress should expose a bounded percentage when content length is known',
  );

  assert.deepEqual(
    updater.formatUpdateProgress(2048, 1024),
    { downloadedBytes: 2048, contentLength: 1024, percent: 100 },
    'update progress must clamp percentages at 100',
  );

  assert.deepEqual(
    updater.formatUpdateProgress(512),
    { downloadedBytes: 512, contentLength: undefined, percent: undefined },
    'update progress must remain indeterminate when the server omits content length',
  );

  console.log('desktop updater golden replay passed');
});
