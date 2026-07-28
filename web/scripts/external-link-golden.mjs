import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const [handler, bridge, projectTab, desktopMain, capability, packageJson] = await Promise.all([
  readFile(new URL('../src/lib/external-link.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/SettingsProjectTab.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/src/main.rs', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/capabilities/default.json', import.meta.url), 'utf8'),
  readFile(new URL('../package.json', import.meta.url), 'utf8'),
]);

assert.match(handler, /EXTERNAL_WEB_PROTOCOLS = new Set\(\['http:', 'https:'\]\)/, 'external links must reject non-web protocols');
assert.match(handler, /isDesktopRuntime\(\)[\s\S]*?import\('@tauri-apps\/plugin-opener'\)[\s\S]*?openUrl\(url\)/, 'desktop links must use the native system browser');
assert.match(handler, /window\.open\(url, '_blank', 'noopener,noreferrer'\)/, 'web links must continue to use a protected browser window');
assert.match(bridge, /case 'openLink':[\s\S]*?openExternalWebUrl\(message\.url\)/, 'all bridge links must use the shared external-link handler');
assert.match(projectTab, /type: 'openLink'/, 'project links must flow through the shared bridge');
assert.match(desktopMain, /\.plugin\(tauri_plugin_opener::init\(\)\)/, 'desktop host must register the opener plugin');

const parsedCapability = JSON.parse(capability);
const openerPermission = parsedCapability.permissions.find(
  (permission) => typeof permission === 'object' && permission?.identifier === 'opener:allow-open-url',
);
assert.deepEqual(openerPermission?.allow, [{ url: 'http://*' }, { url: 'https://*' }], 'desktop opener must only allow web URL scopes');
assert.doesNotMatch(capability, /opener:(?:default|allow-open-path|allow-reveal-item-in-dir)/, 'desktop opener must not expose filesystem operations');
assert.equal(JSON.parse(packageJson).dependencies['@tauri-apps/plugin-opener'], '2.5.4', 'the desktop opener binding must be pinned');

console.log('external link golden replay passed');
