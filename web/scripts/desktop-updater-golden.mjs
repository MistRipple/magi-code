import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { withGoldenViteServer } from './golden-vite.mjs';

const appSource = fs.readFileSync(path.resolve('src/App.svelte'), 'utf8');
const promptSource = fs.readFileSync(path.resolve('src/components/DesktopUpdatePrompt.svelte'), 'utf8');
const settingsSource = fs.readFileSync(path.resolve('src/components/SettingsPanel.svelte'), 'utf8');
const updaterSource = fs.readFileSync(path.resolve('src/lib/desktop-updater.ts'), 'utf8');
const updaterStoreSource = fs.readFileSync(path.resolve('src/stores/desktop-updater.svelte.ts'), 'utf8');
const inputSource = fs.readFileSync(path.resolve('src/components/InputArea.svelte'), 'utf8');
const messagesStoreSource = fs.readFileSync(path.resolve('src/stores/messages.svelte.ts'), 'utf8');
const desktopMainSource = fs.readFileSync(path.resolve('../apps/desktop/src/main.rs'), 'utf8');

assert.match(appSource, /DesktopUpdatePrompt/, 'desktop startup must mount the update prompt');
assert.match(
  settingsSource,
  /desktopUpdaterState/,
  'desktop settings and the global prompt must share one updater state machine',
);
assert.doesNotMatch(
  settingsSource,
  /checkDesktopUpdate/,
  'settings must not create a second native updater resource',
);
const currentVersionPosition = settingsSource.indexOf('current-version-label');
const updateButtonPosition = settingsSource.indexOf('update-check-btn');
assert.ok(currentVersionPosition >= 0, 'desktop settings must render the current version label');
assert.ok(
  currentVersionPosition < updateButtonPosition,
  'the current version label must appear before the update button',
);

assert.match(
  updaterStoreSource,
  /DESKTOP_UPDATE_INITIAL_CHECK_DELAY_MS/,
  'desktop startup must schedule a non-blocking initial update check',
);
assert.match(
  updaterStoreSource,
  /window\.setInterval\([\s\S]*DESKTOP_UPDATE_RETRY_INTERVAL_MS/,
  'desktop runtime must continue checking for updates while the app stays open',
);
assert.match(
  updaterStoreSource,
  /lastCheckAttemptAt[\s\S]*DESKTOP_UPDATE_RETRY_INTERVAL_MS/,
  'failed automatic checks must use a bounded retry interval',
);
assert.match(
  updaterStoreSource,
  /visibilitychange/,
  'returning to the visible desktop window must re-evaluate a stale update check',
);
assert.match(
  updaterStoreSource,
  /window\.addEventListener\('online'/,
  'network recovery must re-evaluate a stale update check',
);
const checkFunctionSource = updaterStoreSource.match(
  /export async function checkForDesktopUpdate[\s\S]*?(?=\nexport async function downloadDesktopUpdate)/,
)?.[0] ?? '';
assert.match(
  checkFunctionSource,
  /source === 'automatic'[\s\S]*desktopUpdaterState\.error = ''/,
  'automatic discovery failures must stay silent',
);

assert.match(updaterSource, /await update\.download\(/, 'update download must be a separate step');
assert.match(updaterSource, /await update\.install\(\)/, 'restart action must install the downloaded update');
assert.doesNotMatch(
  updaterSource,
  /downloadAndInstall/,
  'downloading an update must never install or restart automatically',
);
assert.match(
  updaterSource,
  /await invoke\('prepare_update_restart'\)/,
  'installation must ask the desktop host to stop the daemon gracefully',
);
assert.match(updaterSource, /await relaunch\(\)/, 'installed updates must relaunch the desktop host');
assert.match(
  desktopMainSource,
  /async fn prepare_update_restart[\s\S]*shutdown_desktop_runtime/,
  'desktop host must expose a graceful update restart boundary',
);
assert.match(
  desktopMainSource,
  /invoke_handler\(tauri::generate_handler!\[prepare_update_restart\]\)/,
  'desktop host must register the update restart command',
);

assert.match(promptSource, /phase === 'ready'/, 'download completion must enter a visible ready state');
assert.match(promptSource, /app\.update\.restartNow/, 'ready prompt must offer immediate restart');
assert.match(promptSource, /app\.update\.restartLater/, 'ready prompt must offer deferred restart');
assert.match(
  promptSource,
  /hasProtectedWork[\s\S]*restartConfirmationOpen = true/,
  'active sessions and unsent drafts must be protected by a restart confirmation',
);
assert.match(
  promptSource,
  /hasQueuedMessagesAcrossSessions\(\)/,
  'restart protection must include queued messages outside the visible session',
);
assert.match(
  messagesStoreSource,
  /export function hasQueuedMessagesAcrossSessions[\s\S]*sessionQueuedMessagesByScope/,
  'queued-message protection must inspect every retained session scope',
);
assert.match(inputSource, /setComposerHasUnsavedInput/, 'composer drafts must participate in restart protection');
assert.match(
  inputSource,
  /setComposerHasUnsavedInput\([\s\S]*pendingImageReadCount > 0[\s\S]*selectedGoalMode[\s\S]*selectedSkill !== null/,
  'restart protection must include pending images and unsent composer modes',
);
assert.match(settingsSource, /desktop-update-progress/, 'settings must render an update progress bar');
assert.match(promptSource, /desktop-update-progress/, 'global prompt must render an update progress bar');
assert.match(settingsSource, /aria-valuenow/, 'settings progress must expose accessible numeric progress');
assert.match(promptSource, /desktop-update-progress__fill--indeterminate/, 'update progress must support unknown content length');

await withGoldenViteServer(async (server) => {
  const updater = await server.ssrLoadModule('/src/lib/desktop-updater.ts');

  assert.equal(
    await updater.getDesktopAppVersion(),
    null,
    'browser runtime must not report a desktop application version',
  );

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
  assert.equal(
    updater.isDesktopUpdateCheckDue(0, 1_000, 500),
    true,
    'never-checked desktop sessions must check for updates',
  );
  assert.equal(
    updater.isDesktopUpdateCheckDue(800, 1_000, 500),
    false,
    'fresh update checks must not be repeated',
  );
  assert.equal(
    updater.isDesktopUpdateCheckDue(400, 1_000, 500),
    true,
    'stale update checks must run again',
  );

  console.log('desktop updater golden replay passed');
});
