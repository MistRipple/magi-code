import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const headerSource = await readFile(
  new URL('../src/components/Header.svelte', import.meta.url),
  'utf8',
);
const appSource = await readFile(
  new URL('../src/App.svelte', import.meta.url),
  'utf8',
);
const workbenchShellSource = await readFile(
  new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
  'utf8',
);
const rightPaneSource = await readFile(
  new URL('../src/web/RightPane.svelte', import.meta.url),
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
const inputAreaSource = await readFile(
  new URL('../src/components/InputArea.svelte', import.meta.url),
  'utf8',
);
const messageListSource = await readFile(
  new URL('../src/components/MessageList.svelte', import.meta.url),
  'utf8',
);
const gitContextControlSource = await readFile(
  new URL('../src/components/GitContextControl.svelte', import.meta.url),
  'utf8',
);
const gitRepositoryPanelSource = await readFile(
  new URL('../src/components/GitRepositoryPanel.svelte', import.meta.url),
  'utf8',
);
const gitContextStoreSource = await readFile(
  new URL('../src/stores/git-context.svelte.ts', import.meta.url),
  'utf8',
);
const editsPanelSource = await readFile(
  new URL('../src/components/EditsPanel.svelte', import.meta.url),
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

assert.doesNotMatch(
  appSource,
  /import\s+(?:EditsPanel|KnowledgePanel|SettingsPanel|DesktopRuntimeRecovery)\s+from/,
  '非首屏功能不得进入 App 静态依赖图',
);
assert.match(
  appSource,
  /import\('\.\/components\/EditsPanel\.svelte'\)[\s\S]*?import\('\.\/components\/KnowledgePanel\.svelte'\)[\s\S]*?import\('\.\/components\/SettingsPanel\.svelte'\)/,
  '变更、知识和设置面板必须按可见状态动态加载',
);
assert.doesNotMatch(
  workbenchShellSource,
  /import\s+(?:RightPane|ProjectFileTree|WebFolderPicker)\s+from/,
  '工作台非首屏表面不得进入静态依赖图',
);
assert.match(
  workbenchShellSource,
  /import\('\.\/ProjectFileTree\.svelte'\)[\s\S]*?import\('\.\/RightPane\.svelte'\)[\s\S]*?import\('\.\/WebFolderPicker\.svelte'\)/,
  '文件树、右侧面板和工作区选择器必须按实际可见状态动态加载',
);

assert.match(
  inputAreaSource,
  /<GitContextControl[\s\S]*?workspace=\{composerWorkspace\}[\s\S]*?sessionId=\{persistedSessionId\}/,
  '输入区必须使用统一 Git 上下文入口',
);
assert.doesNotMatch(
  inputAreaSource,
  /previewWorkspaceMerge|mergeWorkspaceBranch|deleteWorkspaceBranch|fetchWorkspaceWorktrees|createWorkspaceWorktree|removeWorkspaceWorktree/,
  '输入区不得继续承担合并、删除和 worktree 管理职责',
);
assert.doesNotMatch(
  inputAreaSource,
  /import\s+WebFolderPicker\s+from/,
  '输入区目录选择器不得进入主对话静态依赖图',
);
assert.match(
  inputAreaSource,
  /import\('\.\.\/web\/WebFolderPicker\.svelte'\)/,
  '输入区目录选择器必须在用户打开时动态加载',
);
assert.match(
  messageListSource,
  /const INITIAL_RENDER_WINDOW = 72;[\s\S]*?const RENDER_WINDOW_CHUNK = 48;/,
  '长会话首屏必须使用有界时间线窗口，并分批加载本地历史',
);
assert.match(
  messageListSource,
  /\$effect\.pre\(\(\) => \{[\s\S]*?visibleRenderLimit = Math\.min\(count, INITIAL_RENDER_WINDOW\);[\s\S]*?const activeRenderItems = \$derived\(safeRenderItems\.slice\(visibleStartIndex\)\);/,
  '时间线窗口必须在 DOM 更新前收敛，避免先挂载完整历史再裁剪',
);
assert.match(
  messageListSource,
  /async function loadOlderHistory\(\): Promise<void> \{[\s\S]*?if \(hasHiddenLocalHistory\) \{[\s\S]*?await revealPreviousRenderItems\(\);[\s\S]*?return;/,
  '向上滚动时必须先展开内存中的本地历史，再请求后端分页',
);
assert.match(
  messageListSource,
  /\(historyState\.hasMoreBefore && historyState\.beforeCursor\)[\s\S]*?\|\| \(historyState\.canonicalHasMoreBefore && historyState\.canonicalBeforeCursor\)/,
  '旧时间线与规范轮次必须独立判断分页能力',
);
assert.match(
  messageListSource,
  /function setContainerScrollPosition\(nextTop: number\)[\s\S]*?async function revealPreviousRenderItems\(\)[\s\S]*?setContainerScrollPosition\(previousScrollTop \+ addedHeight\);/,
  '时间线扩窗必须复用统一的程序化滚动入口并保持可见锚点',
);
assert.match(
  messageListSource,
  /async function revealRenderMessage\(messageId: string\)[\s\S]*?const requiredLimit = safeRenderItems\.length - targetIndex;[\s\S]*?visibleRenderLimit = requiredLimit;/,
  '消息定位与滚动恢复必须按目标位置扩展同一个时间线窗口',
);
assert.match(
  gitContextControlSource,
  /width:\s*min\(280px,\s*calc\(100vw - 24px\)\)/,
  'Git 快速入口必须保持紧凑宽度',
);
assert.match(
  gitContextControlSource,
  /\.git-context-trigger\s*\{[\s\S]*?border:\s*1px solid var\(--border-subtle\);[\s\S]*?border-radius:\s*var\(--radius-full\);/,
  'Git 快速入口必须保持与输入区一致的胶囊视觉',
);
assert.doesNotMatch(
  gitContextControlSource,
  /disabled=\{[^}]*hasUncommitted/,
  '未提交变更不得直接禁用分支切换或新建，Git 应按原生安全规则执行并返回冲突',
);
assert.match(
  gitContextControlSource,
  /text-overflow:\s*ellipsis;[\s\S]*?white-space:\s*nowrap;/,
  '长分支名必须单行省略',
);
assert.match(
  editsPanelSource,
  /<GitRepositoryPanel\s*\/>[\s\S]*?edits\.section\.pendingChanges/,
  '变更中心必须区分 Git 仓库上下文和 Magi 待确认文件变更',
);
assert.match(
  gitRepositoryPanelSource,
  /mergeWorkspaceBranch[\s\S]*?deleteWorkspaceBranch[\s\S]*?createWorkspaceWorktree[\s\S]*?removeWorkspaceWorktree/,
  '高级仓库管理必须完整保留合并、删除和 worktree 能力',
);
assert.match(
  gitRepositoryPanelSource,
  /advancedOpen[\s\S]*?git-advanced-content/,
  '高级仓库能力必须默认折叠并按需展示',
);
assert.match(
  gitContextStoreSource,
  /if \(workspaceId\) return `id:\$\{workspaceId\}\\u0000\$\{sessionId\}`;/,
  '统一 Git 状态必须优先使用工作区 ID 作为主键，避免路径引用格式造成双状态',
);
assert.match(
  gitRepositoryPanelSource,
  /const visible = \$derived\(stateMatches && gitContextState\.loaded && gitContextState\.isRepo\)/,
  '非 Git 工作区不得渲染仓库管理区域，但仍必须保留变更中心本身',
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
  /class="header-menu-item header-mobile-menu-item"[\s\S]*?setNotificationOpen\(true\)/,
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
  /\{#if\s+showRightPaneToggle\s+&&\s+currentRightPane\.collapsed\}/,
  '右栏开关不能只在折叠态出现，展开后仍应留在 Header 右侧图标组内',
);
assert.match(
  headerSource,
  /class="btn-icon header-action-btn header-right-pane-btn"[\s\S]*?onclick=\{toggleRightPane\}/,
  '右栏开关必须作为 Header 右侧图标组的一员常驻渲染',
);
assert.doesNotMatch(
  headerSource,
  /class:active=\{!currentRightPane\.collapsed\}/,
  '右栏开关只负责展开和折叠，不得因面板已展开而持续显示选中态',
);
assert.doesNotMatch(
  workbenchShellSource,
  /right-pane-edge-toggle/,
  '工作台外壳不得保留脱离 Header 图标组的绝对定位右栏开关',
);
assert.doesNotMatch(
  rightPaneSource,
  /right-pane-collapse-btn/,
  '右栏内部不得保留第二套折叠按钮',
);
assert.match(
  workbenchShellSource,
  /import MagiWordmark[\s\S]*?<MagiWordmark\s*\/>/,
  '产品标识应保留在左侧面板顶部，不能因清理应用 Header 而一并删除',
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
assert.doesNotMatch(
  inputAreaSource,
  /type FollowUpMode = 'queue' \| 'guide'|data-testid="input-followup-mode-button"|ia-followup-mode/,
  '运行中输入区必须默认排队，不得保留排队与引导模式切换入口',
);
assert.doesNotMatch(
  `${zhLocaleSource}\n${enLocaleSource}`,
  /"input\.followUp\.(mode|queue|guide|guideTitle)"|"input\.queue\.banner"/,
  '删除输入区模式切换后必须同步清理废弃文案',
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
      panelsCanCoexist: true,
    },
    'compact desktop should keep the sidebar when the reduced conversation minimum still fits',
  );

  assert.deepEqual(
    panelLayout.resolvePanelLayout({
      viewportWidth: 930,
      sidebarWidth: 240,
      previewPanelWidth: 320,
    }),
    {
      sidebarDrawer: false,
      previewOverlay: false,
      panelsCanCoexist: false,
    },
    'narrow tablet should keep the browser split while temporarily suppressing the sidebar',
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

  assert.deepEqual(
    panelLayout.resolvePreviewPanelWidthBounds({
      viewportWidth: 1280,
      sidebarWidth: 320,
      sidebarVisible: true,
      rightPaneOpen: true,
      previewOverlay: false,
    }),
    { minWidth: 320, maxWidth: 808 },
    'browser focus width should use the full workbench after the sidebar yields space',
  );

  assert.deepEqual(
    panelLayout.resolvePreviewPanelWidthBounds({
      viewportWidth: 1440,
      sidebarWidth: 320,
      sidebarVisible: false,
      rightPaneOpen: true,
      previewOverlay: false,
    }),
    { minWidth: 320, maxWidth: 960 },
    'browser focus width should reach two thirds of a standard desktop window',
  );

  assert.deepEqual(
    panelLayout.resolvePreviewPanelWidthBounds({
      viewportWidth: 1280,
      sidebarWidth: 320,
      sidebarVisible: false,
      rightPaneOpen: true,
      previewOverlay: false,
    }),
    { minWidth: 320, maxWidth: 808 },
    'conversation minimum should cap the browser before it distorts a compact window',
  );

  console.log('panel layout golden passed');
});
