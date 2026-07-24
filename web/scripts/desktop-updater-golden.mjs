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
assert.match(
  statusSource,
  /header-update-action--\$\{actionPresentation\.tone\}/,
  'each updater phase must use a dedicated action color',
);
assert.match(
  statusSource,
  /<span class="header-update-action-slot">[\s\S]*<button[\s\S]*header-update-action[\s\S]*<\/button>[\s\S]*<span class="header-update-version">/,
  '更新按钮必须固定在版本号之前，避免状态变化挤压后续顶部操作',
);
assert.match(
  statusSource,
  /\.header-update-action-slot[\s\S]*?width: 70px[\s\S]*?flex: 0 0 70px/,
  '顶部更新动作槽必须预留固定宽度，状态切换时版本号不得移动',
);
assert.match(
  statusSource,
  /header-update-action--expanded[\s\S]*?width: 70px/,
  '只有具备明确下一步动作的更新状态才展开为文字按钮',
);
assert.match(
  statusSource,
  /case 'latest':[\s\S]*?'check-circle'/,
  '手动检查到最新版本时按钮必须显示明确的成功状态',
);
assert.match(
  statusSource,
  /showFeedback\('success'[\s\S]*app\.update\.latestMessage/,
  '手动检查无新版本时必须给出成功反馈',
);
assert.match(
  statusSource,
  /\$effect\(\(\) => \{[\s\S]*phase === 'available'[\s\S]*showFeedback\([\s\S]*app\.update\.availableMessage/,
  '自动或手动检查发现更新时必须只展示一次可操作反馈，并能根据安装位置调整提示',
);
assert.match(
  statusSource,
  /name=\{actionPresentation\.icon\}[\s\S]*actionPresentation\.spinning[\s\S]*header-update-action-icon--spinning/,
  '手动检查更新时必须直接旋转刷新图标，而不是只显示鼠标等待状态',
);
assert.match(
  statusSource,
  /:global\(\.header-update-action-icon--spinning\)[\s\S]*header-update-action-spin/,
  '检查更新中必须为刷新图标绑定稳定的旋转动画',
);
assert.match(
  statusSource,
  /case 'checking':[\s\S]*?icon: 'refresh'[\s\S]*?spinning: true/,
  '检查更新中必须持续使用刷新图标，避免切换成不易辨识的替代加载环',
);
assert.match(
  statusSource,
  /case 'ready':[\s\S]*?icon: 'restart'[\s\S]*?app\.update\.restart/,
  '更新包已就绪时必须显示重启图标，不能继续复用检查更新图标',
);
assert.match(
  statusSource,
  /case 'downloading':[\s\S]*?\$\{progress\.percent\}%/,
  '下载过程必须在顶部动作中展示真实百分比',
);
assert.match(
  statusSource,
  /case 'available':[\s\S]*?icon: 'download'[\s\S]*?app\.update\.update/,
  '发现更新后必须展开为明确的更新动作，不能只依赖装饰性状态点',
);
assert.match(
  statusSource,
  /case 'installing':[\s\S]*?icon: 'restart'[\s\S]*?spinning: true/,
  '进入安装重启阶段后必须持续显示旋转的重启图标',
);
assert.match(
  statusSource,
  /case 'ready':[\s\S]*?app\.update\.restart/,
  '下载完成后必须展示明确的重启文字',
);
assert.match(
  statusSource,
  /header-update-progress-ring[\s\S]*?header-update-progress-track[\s\S]*?header-update-progress-value/,
  '下载过程必须使用轨道与进度弧线组成的真正环形进度条',
);
assert.match(
  statusSource,
  /header-update-progress-value[\s\S]*?stroke-dasharray: 100[\s\S]*?stroke-dashoffset/,
  '可计算下载进度时必须通过环形弧线长度表达百分比',
);
const progressMarkup = statusSource.match(
  /\{#if actionPresentation\.progress\}([\s\S]*?)\{:else\}/,
)?.[1] ?? '';
assert.doesNotMatch(
  progressMarkup,
  /<Icon/,
  '环形进度条显示时不得同时渲染或旋转下载图标',
);
assert.match(
  statusSource,
  /header-update-progress-ring--indeterminate[\s\S]*?header-update-progress-spin/,
  '只有无法计算百分比时才允许环形进度弧线自行旋转',
);
assert.doesNotMatch(
  statusSource,
  /header-update-action-spinner-ring|header-update-action-spinner/,
  '更新按钮不得再维护独立加载环，避免图标状态与动画状态分离',
);
assert.doesNotMatch(
  statusSource,
  /cursor:\s*wait/,
  '检查更新按钮不得使用鼠标等待光标代替自身反馈',
);
assert.match(
  statusSource,
  /aria-disabled=\{actionDisabled\}/,
  '检查中必须保留可运行的图标动画，不能依赖原生 disabled 按钮状态',
);
assert.doesNotMatch(
  statusSource,
  /\sdisabled=\{actionDisabled\}/,
  '检查中不得使用可能冻结子元素动画的原生 disabled 属性',
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
  /MANUAL_UPDATE_CHECK_FEEDBACK_MS[\s\S]*source === 'manual'[\s\S]*window\.setTimeout/,
  '手动检查必须保留最短可见反馈时间，避免快速响应造成点击无反馈',
);
assert.match(
  updaterStoreSource,
  /MANUAL_LATEST_RESULT_DURATION_MS[\s\S]*desktopUpdaterState\.phase = 'latest'[\s\S]*window\.setTimeout/,
  '手动检查无更新时必须短暂保留成功状态，再恢复固定宽度的检查入口',
);
assert.match(
  updaterStoreSource,
  /source === 'automatic'[\s\S]*desktopUpdaterState\.phase = 'idle'/,
  '自动检查无更新必须静默回到空闲状态，不能持续打扰用户',
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
assert.match(
  updaterSource,
  /get_desktop_update_installability[\s\S]*?installability/,
  'desktop updater must obtain installation eligibility from the native host',
);
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
  /fn require_desktop_update_installability[\s\S]*?DiskImage/,
  'desktop host must own the rule that disk-image applications cannot install updates',
);
assert.match(
  desktopMainSource,
  /fn stage_desktop_update[\s\S]*?require_desktop_update_installability/,
  'desktop host must reject update downloads from an un-installable application location',
);
assert.match(
  desktopMainSource,
  /fn install_staged_desktop_update[\s\S]*?require_desktop_update_installability/,
  'desktop host must reject update installation from an un-installable application location',
);
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
assert.match(
  statusSource,
  /!update\.installability\.installable[\s\S]*showInstallationRequiredFeedback/,
  'running from a disk image must provide an installation instruction instead of starting download or restart',
);
assert.match(
  statusSource,
  /installationRequired[\s\S]*?app\.update\.installationRequiredMessage/,
  '发现更新时必须立即说明磁盘映像无法安装，不能误导用户可以下载并重启',
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
