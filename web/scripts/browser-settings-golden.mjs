import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const source = fs.readFileSync(
  path.resolve('src/components/SettingsBrowserTab.svelte'),
  'utf8',
);
const zhCN = JSON.parse(fs.readFileSync(path.resolve('src/i18n/zh-CN.json'), 'utf8'));
const enUS = JSON.parse(fs.readFileSync(path.resolve('src/i18n/en-US.json'), 'utf8'));

assert.doesNotMatch(source, /runBrowserRuntimeAction|AgentApiError|@tauri-apps|pollRuntimeStatus/);
assert.doesNotMatch(source, /runtimeStatus|runtimeMode|playwrightVersion|hostVersion|restartRequired/);

assert.match(source, /getBrowserCapabilities\(\)/);
assert.match(source, /updateBrowserSettings\(/);
assert.match(source, /inAppBrowserEnabled/);
assert.match(source, /browserUseEnabled/);

assert.match(source, /const desktop = window\.magiDesktop;[\s\S]*?desktop\?\.runtime === 'electron'/);
assert.match(source, /desktop\.getBrowserComponentInfo\(\)/);
assert.match(source, /desktop\.restartBrowserAutomation\(\)/);
assert.match(source, /desktop\.clearBrowserData\(\)/);
assert.match(source, /desktop\.checkForUpdates\(\)/);
assert.match(source, /desktop\.onUpdate\(/);

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
