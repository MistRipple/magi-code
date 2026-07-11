import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const headerSource = await readFile(
  new URL('../src/components/Header.svelte', import.meta.url),
  'utf8',
);
const topTabsSource = await readFile(
  new URL('../src/components/TopTabs.svelte', import.meta.url),
  'utf8',
);
const notificationCenterSource = await readFile(
  new URL('../src/components/NotificationCenter.svelte', import.meta.url),
  'utf8',
);
const settingsAgentsSource = await readFile(
  new URL('../src/components/SettingsAgentsTab.svelte', import.meta.url),
  'utf8',
);
const settingsToolsSource = await readFile(
  new URL('../src/components/SettingsToolsTab.svelte', import.meta.url),
  'utf8',
);
const zhLocaleSource = await readFile(
  new URL('../src/i18n/zh-CN.json', import.meta.url),
  'utf8',
);
const enLocaleSource = await readFile(
  new URL('../src/i18n/en-US.json', import.meta.url),
  'utf8',
);

assert.match(
  headerSource,
  /\.header-more-menu\s*\{[\s\S]*?background:\s*var\(--dropdown-bg\);/,
  '顶部更多菜单必须使用不透明的下拉菜单背景，不能使用透明表面层',
);
assert.match(
  headerSource,
  /@media \(max-width:\s*768px\)[\s\S]*?\.header-center\s*\{[\s\S]*?justify-content:\s*center;/,
  '手机模式下顶部导航容器必须保持居中',
);
assert.match(
  topTabsSource,
  /@media \(max-width:\s*768px\)[\s\S]*?\.tab-bar\.tab-bar--top\s*\{[\s\S]*?justify-content:\s*center;/,
  '手机模式下对话、变更、知识标签组必须保持居中',
);
assert.doesNotMatch(
  headerSource,
  /currentWorkspaceFolder|workspace-breadcrumb/,
  '顶部栏不得重复展示输入区已经提供的工作空间名称',
);
assert.doesNotMatch(
  `${zhLocaleSource}\n${enLocaleSource}`,
  /header\.workspaceBreadcrumbTitle/,
  '删除顶部工作空间展示后必须同步清理废弃文案',
);
assert.match(
  headerSource,
  /class="header-mobile-menu-item"[\s\S]*?setNotificationOpen\(true\)/,
  '手机端更多菜单必须提供通知入口',
);
assert.match(
  headerSource,
  /header-more-unread-dot/,
  '手机端通知收起后必须在更多按钮保留未读提示',
);
assert.match(
  headerSource,
  /@media \(max-width:\s*768px\)[\s\S]*?\.header-bar\s*\{[\s\S]*?display:\s*grid;[\s\S]*?grid-template-columns:\s*1fr auto 1fr;/,
  '手机顶部栏必须收敛为单行三段式布局',
);
assert.doesNotMatch(
  notificationCenterSource,
  /class="[^"]*notification-btn/,
  '通知内容组件不得继续拥有独立触发按钮，避免手机和桌面双入口双实现',
);
assert.match(
  notificationCenterSource,
  /if \(open && !wasOpen[\s\S]*?markAllNotificationsRead\(\)[\s\S]*?loadNotifications\(\)/,
  '通知面板必须在统一 open 状态首次展开时执行读取逻辑',
);
assert.doesNotMatch(
  headerSource,
  /class="header-mobile-menu-item"[\s\S]{0,240}?rightPane\.expand/,
  '手机端更多菜单不得重复承载右侧面板入口',
);
assert.doesNotMatch(
  headerSource,
  /\.header-notification-btn,\s*\.header-right-pane-btn\s*\{\s*display:\s*none;/,
  '手机端只能隐藏通知按钮，右侧面板按钮必须保持独立可见',
);
assert.match(
  settingsAgentsSource,
  /@container agents-tab \(max-width:\s*760px\)[\s\S]*?\.role-tab\s*\{[\s\S]*?flex:\s*0 0 auto;/,
  '窄屏代理 Tab 必须保持自然宽度并禁止被横向压缩',
);
assert.match(
  settingsAgentsSource,
  /@container agents-tab \(max-width:\s*760px\)[\s\S]*?\.role-tab-name\s*\{[\s\S]*?overflow:\s*visible;[\s\S]*?text-overflow:\s*clip;/,
  '窄屏代理名称必须完整展示，不能继续使用省略号',
);
assert.match(
  settingsAgentsSource,
  /@container agents-tab \(max-width:\s*560px\)[\s\S]*?\.role-tab\s*\{[\s\S]*?grid-template-columns:\s*max-content 6px;/,
  '隐藏代理头像后必须移除头像占位列',
);
assert.doesNotMatch(
  settingsToolsSource,
  /class="header-title-group"\s+style=/,
  '工具页标题布局必须由响应式样式统一管理，不能继续依赖内联布局',
);
assert.match(
  settingsToolsSource,
  /\.tools-manager\s*\{[\s\S]*?container-type:\s*inline-size;[\s\S]*?container-name:\s*tools-tab;/,
  '工具页必须基于设置内容区宽度响应，不能依赖整个窗口宽度',
);
assert.match(
  settingsToolsSource,
  /@container tools-tab \(max-width:\s*760px\)[\s\S]*?\.builtin-summary-toggle\s*\{[\s\S]*?display:\s*grid;[\s\S]*?grid-template-columns:\s*minmax\(0, 1fr\) auto;/,
  '窄屏内置工具摘要必须切换为分层网格布局',
);
assert.match(
  settingsToolsSource,
  /@container tools-tab \(max-width:\s*760px\)[\s\S]*?\.capability-dependency-strip\s*\{[\s\S]*?grid-column:\s*1 \/ -1;[\s\S]*?width:\s*100%;/,
  '窄屏依赖状态必须占据完整第二行并按自然宽度换行',
);

await withGoldenViteServer(async (server) => {
  const panelLayout = await server.ssrLoadModule('/src/web/panel-layout.ts');

  assert.deepEqual(
    panelLayout.resolvePanelLayout({
      viewportWidth: 1440,
      sidebarWidth: 320,
      previewPanelWidth: 320,
    }),
    {
      sidebarDrawer: false,
      previewOverlay: false,
      panelsCanCoexist: true,
    },
    'wide desktop should preserve both side panels without shrinking the main conversation',
  );

  assert.deepEqual(
    panelLayout.resolvePanelLayout({
      viewportWidth: 1100,
      sidebarWidth: 240,
      previewPanelWidth: 320,
    }),
    {
      sidebarDrawer: false,
      previewOverlay: false,
      panelsCanCoexist: false,
    },
    'compact desktop should keep split preview but require mutually exclusive side panels',
  );

  assert.deepEqual(
    panelLayout.resolvePanelLayout({
      viewportWidth: 930,
      sidebarWidth: 240,
      previewPanelWidth: 320,
    }),
    {
      sidebarDrawer: false,
      previewOverlay: true,
      panelsCanCoexist: false,
    },
    'narrow tablet should use an overlay preview before the mobile drawer breakpoint',
  );

  assert.deepEqual(
    panelLayout.resolvePanelLayout({
      viewportWidth: 390,
      sidebarWidth: 320,
      previewPanelWidth: 320,
    }),
    {
      sidebarDrawer: true,
      previewOverlay: true,
      panelsCanCoexist: false,
    },
    'mobile should render both side surfaces as mutually exclusive overlays',
  );

  assert.deepEqual(
    panelLayout.resolvePanelVisibility({
      sidebarDrawer: false,
      panelsCanCoexist: false,
      sidebarPreferredOpen: true,
      sidebarDrawerOpen: false,
      rightPaneOpen: true,
    }),
    { sidebarVisible: false, rightPaneVisible: true },
    'compact mode should temporarily suppress the preferred left pane while the right pane is open',
  );

  assert.deepEqual(
    panelLayout.resolvePanelVisibility({
      sidebarDrawer: false,
      panelsCanCoexist: false,
      sidebarPreferredOpen: true,
      sidebarDrawerOpen: false,
      rightPaneOpen: false,
    }),
    { sidebarVisible: true, rightPaneVisible: false },
    'closing the compact right pane should restore the preferred left pane',
  );

  assert.deepEqual(
    panelLayout.resolvePanelVisibility({
      sidebarDrawer: false,
      panelsCanCoexist: true,
      sidebarPreferredOpen: true,
      sidebarDrawerOpen: false,
      rightPaneOpen: true,
    }),
    { sidebarVisible: true, rightPaneVisible: true },
    'wide mode should allow both preferred side panels to remain visible',
  );

  assert.deepEqual(
    panelLayout.resolvePanelVisibility({
      sidebarDrawer: false,
      panelsCanCoexist: true,
      sidebarPreferredOpen: false,
      sidebarDrawerOpen: false,
      rightPaneOpen: false,
    }),
    { sidebarVisible: false, rightPaneVisible: false },
    'an explicitly collapsed sidebar must remain collapsed after the right pane closes',
  );

  assert.deepEqual(
    panelLayout.resolvePanelVisibility({
      sidebarDrawer: true,
      panelsCanCoexist: false,
      sidebarPreferredOpen: true,
      sidebarDrawerOpen: true,
      rightPaneOpen: true,
    }),
    { sidebarVisible: false, rightPaneVisible: true },
    'mobile overlays must never expose both side surfaces at once',
  );

  console.log('panel layout golden passed');
});
