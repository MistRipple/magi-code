import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const repositoryRoot = path.resolve(new URL('../..', import.meta.url).pathname);

const source = fs.readFileSync(
  path.join(repositoryRoot, 'web/src/components/SettingsBrowserTab.svelte'),
  'utf8',
);
const desktopMainSource = fs.readFileSync(
  path.join(repositoryRoot, 'apps/desktop/src/main/index.ts'),
  'utf8',
);
const desktopPreloadSource = fs.readFileSync(
  path.join(repositoryRoot, 'apps/desktop/src/preload/index.ts'),
  'utf8',
);
const desktopTypesSource = fs.readFileSync(
  path.join(repositoryRoot, 'web/src/types/magi-desktop.d.ts'),
  'utf8',
);
const agentApiSource = fs.readFileSync(
  path.join(repositoryRoot, 'web/src/web/agent-api.ts'),
  'utf8',
);
const zhCN = JSON.parse(fs.readFileSync(path.join(repositoryRoot, 'web/src/i18n/zh-CN.json'), 'utf8'));
const enUS = JSON.parse(fs.readFileSync(path.join(repositoryRoot, 'web/src/i18n/en-US.json'), 'utf8'));

assert.doesNotMatch(source, /runBrowserRuntimeAction|AgentApiError|@tauri-apps|pollRuntimeStatus/);
assert.doesNotMatch(source, /runtimeStatus|runtimeMode|playwrightVersion|hostVersion|restartRequired/);

assert.match(source, /getBrowserCapabilities\(\)/);
assert.match(source, /updateBrowserSettings\(/);
assert.match(agentApiSource, /browserClientPlatform\(\)/);
assert.match(agentApiSource, /platformCapabilities/);
assert.match(agentApiSource, /clientPlatform/);
assert.match(agentApiSource, /'desktop' \| 'web' \| 'mobile-web'/);
assert.match(source, /inAppBrowserEnabled/);
assert.match(source, /browserUseEnabled/);

assert.match(source, /const desktop = window\.magiDesktop;[\s\S]*?desktop\?\.runtime === 'electron'/);
assert.match(source, /desktop\.getBrowserComponentInfo\(\)/);
assert.match(source, /desktop\.restartBrowserAutomation\(\)/);
assert.match(source, /desktop\.clearBrowserData\(\)/);
assert.match(source, /checkForDesktopUpdate\('manual'\)/);
assert.doesNotMatch(source, /desktop\.checkForUpdates\(\)/);
assert.match(source, /desktop\.onBrowserComponent\(/);
assert.match(source, /MagiDesktopBrowserComponentSnapshot/);
assert.match(desktopMainSource, /function browserComponentSnapshot\(/);
assert.match(desktopMainSource, /broadcastAll\("magi-desktop:browser-component"/);
assert.doesNotMatch(desktopMainSource, /app\.getVersion\(\)/);
assert.match(desktopPreloadSource, /onBrowserComponent/);
assert.match(desktopTypesSource, /interface MagiDesktopBrowserComponentSnapshot/);
assert.match(desktopTypesSource, /onBrowserComponent\(/);

assert.match(source, /activeAction === 'refresh-components'/);
assert.match(source, /activeAction !== action/);
assert.match(source, /restartAutomationSucceeded/);
assert.match(source, /clearDataSucceeded/);
assert.match(source, /desktopUpdateAvailable/);
assert.match(source, /desktopUpToDate/);
assert.match(source, /webUnavailableTitle/);

for (const locale of [zhCN, enUS]) {
  for (const key of [
    'settings.browser.install',
    'settings.browser.uninstall',
    'settings.browser.playwrightVersion',
    'settings.browser.hostVersion',
    'settings.browser.runtimeMode',
    'settings.browser.managementTitle',
  ]) {
    assert.equal(locale[key], undefined, `legacy browser component key must be removed: ${key}`);
  }
}

const localizedBrowserText = Object.entries({ ...zhCN, ...enUS })
  .filter(([key]) => key.startsWith('settings.browser.'))
  .map(([, value]) => String(value))
  .join('\n');
assert.doesNotMatch(localizedBrowserText, /Browser Runtime|Browser Host|Playwright|CEF/);

console.log('desktop browser settings golden tests passed');
