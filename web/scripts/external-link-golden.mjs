import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const [handler, bridge, projectTab, desktopMain, desktopPreload, desktopTypes, packageJson] = await Promise.all([
  readFile(new URL('../src/lib/external-link.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/SettingsProjectTab.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/src/main/index.ts', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/src/preload/index.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/types/magi-desktop.d.ts', import.meta.url), 'utf8'),
  readFile(new URL('../package.json', import.meta.url), 'utf8'),
]);

assert.match(handler, /EXTERNAL_WEB_PROTOCOLS = new Set\(\['http:', 'https:'\]\)/, 'external links must reject non-web protocols');
assert.match(handler, /window\.magiDesktop[\s\S]*?window\.magiDesktop\.openExternal\(url\)/, 'desktop links must use the Electron system browser bridge');
assert.match(handler, /window\.open\(url, '_blank', 'noopener,noreferrer'\)/, 'web links must continue to use a protected browser window');
assert.match(bridge, /case 'openLink':[\s\S]*?openExternalWebUrl\(message\.url\)/, 'all bridge links must use the shared external-link handler');
assert.match(projectTab, /type: 'openLink'/, 'project links must flow through the shared bridge');
assert.match(desktopMain, /\["http:", "https:"\]\.includes\(url\.protocol\)[\s\S]*?shell\.openExternal/, 'Electron Main must validate web protocols before opening externally');
assert.match(desktopPreload, /openExternal:[\s\S]*?magi-desktop:open-external/, 'preload must expose only the scoped external-link IPC');
assert.match(desktopTypes, /openExternal\(url: string\): Promise<void>/, 'Renderer bridge typing must expose the external-link contract');
assert.doesNotMatch(handler + desktopPreload + desktopMain, /@tauri-apps|tauri_plugin_opener/, 'external links must not retain a Tauri fallback');
assert.equal(Object.keys(JSON.parse(packageJson).dependencies).some((name) => name.startsWith('@tauri-apps/')), false, 'web dependencies must not retain Tauri packages');

console.log('external link golden replay passed');
