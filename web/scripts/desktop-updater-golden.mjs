import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { withGoldenViteServer } from './golden-vite.mjs';

const appSource = fs.readFileSync(path.resolve('src/App.svelte'), 'utf8');
const statusSource = fs.readFileSync(path.resolve('src/components/DesktopUpdateStatus.svelte'), 'utf8');
const settingsSource = fs.readFileSync(path.resolve('src/components/SettingsPanel.svelte'), 'utf8');
const headerSource = fs.readFileSync(path.resolve('src/components/Header.svelte'), 'utf8');
const updaterSource = fs.readFileSync(path.resolve('src/lib/desktop-updater.ts'), 'utf8');
const updaterStoreSource = fs.readFileSync(path.resolve('src/stores/desktop-updater.svelte.ts'), 'utf8');
const desktopMainSource = fs.readFileSync(path.resolve('../apps/desktop/src/main.rs'), 'utf8');

assert.doesNotMatch(appSource, /DesktopUpdatePrompt/, 'desktop updater must not mount a separate popup card');
assert.match(statusSource, /desktopUpdaterState/, 'desktop header update status must use the shared updater state machine');
assert.doesNotMatch(
  settingsSource,
  /desktopUpdaterState|checkForDesktopUpdate|downloadDesktopUpdate|restartWithDesktopUpdate/,
  'settings must not own a second update control now that the header is the single update entry',
);
assert.match(statusSource, /currentVersion/, 'desktop header must show the current application version');
assert.match(statusSource, /startDesktopUpdater/, 'desktop header must own the single updater lifecycle');
assert.match(statusSource, /downloadDesktopUpdate/, 'desktop header must expose download action');
assert.match(statusSource, /restartWithDesktopUpdate/, 'desktop header must expose restart action');
assert.match(headerSource, /DesktopUpdateStatus/, 'desktop header must mount the update status control');
assert.doesNotMatch(statusSource, /header-update-popover|toggleOpen|role="dialog"/, 'desktop header update actions must not open a popup');
assert.match(statusSource, /header-update-action--\$\{actionTone\}/, 'each updater phase must use a dedicated action color');

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
  updaterSource,
  /DESKTOP_UPDATE_CHECK_RETRY_DELAYS_MS[\s\S]*await check\(\)/,
  'native update checks must retry transient release or network failures before surfacing an error',
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

assert.match(updaterSource, /stage_desktop_update/, 'update download must be a separate step');
assert.match(updaterSource, /install_staged_desktop_update/, 'restart action must install the persisted update');
assert.doesNotMatch(
  updaterSource,
  /downloadAndInstall/,
  'downloading an update must never install or restart automatically',
);
assert.match(
  updaterSource,
  /await invoke\('prepare_update_restart'\)/,
  'installation must ask the desktop host to force-stop the daemon',
);
assert.match(updaterSource, /await relaunch\(\)/, 'installed updates must relaunch the desktop host');
assert.match(
  desktopMainSource,
  /fn prepare_update_restart[\s\S]*force_shutdown_desktop_runtime/,
  'desktop host must expose a forceful update restart boundary',
);
assert.match(
  desktopMainSource,
  /fn get_staged_desktop_update[\s\S]*read_staged_update/,
  'desktop host must restore a staged update after an application restart',
);
assert.match(
  desktopMainSource,
  /fn stage_desktop_update[\s\S]*write_staged_update/,
  'desktop host must persist the verified update package',
);
assert.match(
  desktopMainSource,
  /fn install_staged_desktop_update[\s\S]*update[\s\S]*\.install\(bytes\)/,
  'desktop host must install the persisted update package',
);
assert.match(
  desktopMainSource,
  /invoke_handler\(tauri::generate_handler!\[[\s\S]*get_staged_desktop_update[\s\S]*stage_desktop_update[\s\S]*install_staged_desktop_update/,
  'desktop host must register the complete staged update command set',
);

assert.match(
  statusSource,
  /phase === 'ready'[\s\S]*restartWithDesktopUpdate\(\)/,
  'clicking the ready update action in the header must install and restart immediately',
);
const downloadFunctionSource = updaterStoreSource.match(
  /export async function downloadDesktopUpdate[\s\S]*?(?=\nexport async function restartWithDesktopUpdate)/,
)?.[0] ?? '';
assert.match(
  downloadFunctionSource,
  /await update\.download\([\s\S]*desktopUpdaterState\.phase = 'ready'/,
  'download completion must settle into ready before any restart action',
);
assert.doesNotMatch(
  downloadFunctionSource,
  /installAndRestart|relaunch|update\.install/,
  'download completion must never install or restart automatically',
);
assert.doesNotMatch(settingsSource, /desktop-update-progress/, 'settings must not render a download progress bar');
assert.match(statusSource, /progress\?\.percent/, 'header update control must show the trusted download percentage');

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
