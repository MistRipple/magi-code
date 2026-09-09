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
assert.match(
  inputAreaSource,
  /function isNewlineShortcut\([\s\S]*?event\.metaKey[\s\S]*?event\.ctrlKey[\s\S]*?event\.shiftKey[\s\S]*?event\.altKey/,
  '换行快捷键必须支持跨平台一致的 Shift+Enter，并保留 Option/Alt+Enter',
);
assert.match(
  inputAreaSource,
  /if \(isNewlineShortcut\(event\)\) \{[\s\S]*?event\.isComposing[\s\S]*?closeSlashMenu\(\);[\s\S]*?insertNewlineAtCursor\(\);/,
  '换行快捷键必须优先于斜杠菜单处理，并在输入法组合态下不改变输入内容',
);
assert.match(zhLocaleSource, /"input\.send": "发送 \(Enter 发送，Shift\+Enter 或 Option\/Alt\+Enter 换行\)"/, '中文发送提示必须与实际快捷键一致');
assert.match(enLocaleSource, /"input\.send": "Send \(Enter to send, Shift\+Enter or Option\/Alt\+Enter for newline\)"/, '英文发送提示必须与实际快捷键一致');
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
  /async function loadOlderHistory\(requestId: number\): Promise<boolean> \{[\s\S]*?if \(hasHiddenLocalHistory\) \{[\s\S]*?return revealPreviousRenderItems\(\);/,
  '向上滚动时必须先展开内存中的本地历史，再请求后端分页',
);
assert.match(
  messageListSource,
  /historyState\.canonicalHasMoreBefore && historyState\.canonicalBeforeCursor/,
  '历史分页能力必须只由规范轮次 canonical cursor 决定',
);
assert.match(
  messageListSource,
  /const prependedCount = Math\.max\(0, previousLastIndex\);[\s\S]*?const newlyVisibleCount = prependedCount \+ appendedCount;[\s\S]*?visibleRenderLimit = Math\.min\(count, visibleRenderLimit \+ newlyVisibleCount\);/,
  '历史 prepend 后必须扩展渲染窗口，确保新加载消息实际进入 DOM',
);
assert.doesNotMatch(
  messageListSource,
  /isLoadingBefore/,
  '历史分页不得继续使用会被 effect 自动唤醒的旧 loading 布尔状态',
);
assert.match(
  messageListSource,
  /historyLoadStatus === 'idle'/,
  '历史分页 effect 只能由 idle 状态自动触发，失败和无进展必须停止自动请求',
);
assert.match(
  messageListSource,
  /historyLoadStatus:\s*'error'/,
  '请求失败必须收敛到显式 error 状态并保留用户重试入口',
);
assert.match(
  messageListSource,
  /commitOlderSessionHistoryPage\([\s\S]*?committed\.accepted[\s\S]*?committed\.addedTurnCount/,
  '历史分页 UI 必须只接受 store 已确认实际前进的提交结果',
);
assert.match(
  messageListSource,
  /function captureScrollSnapshot\(\)[\s\S]*?interactionEpoch: scrollInteractionEpoch,[\s\S]*?anchor: captureVisibleAnchor\(\)[\s\S]*?function restoreScrollSnapshot\([\s\S]*?snapshot\.interactionEpoch !== scrollInteractionEpoch/,
  '时间线扩窗必须使用带交互代际的消息锚点快照，不能写回过期滚动位置',
);
assert.match(
  messageListSource,
  /function captureScrollSnapshot\(\)[\s\S]*?recoveryEpoch: scrollRecoveryEpoch,[\s\S]*?snapshot\.recoveryEpoch !== scrollRecoveryEpoch/,
  '滚动恢复必须校验恢复代际，真实用户输入后不能被旧布局观察回写',
);
assert.match(
  messageListSource,
  /pendingScrollState:[\s\S]*?panelKey:[\s\S]*?scopeKey:[\s\S]*?interactionEpoch:[\s\S]*?recoveryEpoch:/,
  '异步滚动状态必须绑定面板和作用域，不能跨会话或跨面板串写',
);
assert.match(
  messageListSource,
  /function requestOlderHistoryLoad\(\): Promise<boolean> \{[\s\S]*?historyLoadRequest\?\.scopeKey === scopeKey[\s\S]*?const id = \+\+nextHistoryLoadRequestId;[\s\S]*?loadOlderHistory\(id\)/,
  '历史加载必须由单飞协调器收敛，避免 scroll、wheel 和 observer 重复请求',
);
assert.doesNotMatch(messageListSource, /IntersectionObserver/, '历史加载不得保留第二条 observer 触发路径');
assert.match(
  messageListSource,
  /function installScrollIntentHandlers\(node: HTMLDivElement\)[\s\S]*?handleWheelIntent\(event, node\)/,
  '滚动意图必须由统一原生监听器接管，不得把来源归因交给事件数量预算',
);
assert.match(messageListSource, /use:installScrollIntentHandlers/, '消息列表必须在 scroll 之前接管用户滚动意图');
assert.match(messageListSource, /MessageScrollCoordinator/, '应用滚动必须使用唯一的目标位置事务协调器');
assert.doesNotMatch(messageListSource, /programmaticScrollEventBudget/, '不得使用固定 scroll 事件预算猜测事件来源');
assert.match(messageListSource, /node\.addEventListener\('pointerdown', onPointerDown\)[\s\S]*?node\.addEventListener\('pointermove', onPointerMove\)[\s\S]*?node\.addEventListener\('touchstart', onTouchStart/, '指针和触摸意图必须在真实拖动发生时取消过期应用滚动');
assert.match(messageListSource, /userScrollDirection === 'up' && scrollTop <= HISTORY_LOAD_THRESHOLD_PX/, '历史分页只能由真实向上滚动触发，不能被延迟或向下 scroll 事件误触发');
assert.match(messageListSource, /scrollbar-gutter:\s*stable/, '滚动条出现和消失不能改变消息列的可用宽度');
assert.doesNotMatch(messageListSource, /previousScrollTop \+ addedHeight/, '历史加载不得使用过期 scrollTop 加总高度回写');
assert.doesNotMatch(messageListSource, /content-visibility:\s*auto|contain-intrinsic-block-size/, '消息高度未知时不得使用固定虚拟高度估算');
assert.match(messageListSource, /overflow-anchor:\s*none/, '滚动锚点必须由应用唯一协调，不能与浏览器锚点竞争');
assert.match(
  messageListSource,
  /commitOlderSessionHistoryPage\([\s\S]*?revision:\s*requestRevision[\s\S]*?canonicalBeforeCursor:\s*requestCanonicalBeforeCursor/,
  '历史分页必须由 store 以 revision 和 cursor 快照原子提交',
);
assert.match(
  messageListSource,
  /historyState\.workspacePath[\s\S]*?workspacePath/,
  '历史分页作用域必须同时校验 workspace id 和 path',
);
assert.match(
  messageListSource,
  /const userScrollDirection = deriveMessageScrollDirection\([\s\S]*?const userScroll = !isProgrammaticScroll[\s\S]*?else if \(userScrollDirection === 'up'\) \{[\s\S]*?nextAutoScroll = false;[\s\S]*?\} else if \(isNearBottom\)/,
  '只有已确认的用户向上滚动才能退出自动跟随，普通 scroll 事件不得误判为用户操作',
);
assert.match(
  messageListSource,
  /let layoutObservationNonce = 0;[\s\S]*?observationNonce !== layoutObservationNonce/,
  '异步布局观察必须具备代际校验，不能在过期 tick 完成后回写滚动位置',
);
assert.match(
  messageListSource,
  /let autoScrollScheduleNonce = 0;[\s\S]*?scheduleNonce !== autoScrollScheduleNonce/,
  '异步布局观察和自动滚动必须具备代际校验，不能在过期 tick 完成后回写滚动位置',
);
assert.match(
  messageListSource,
  /if \(node\.scrollTop <= HISTORY_LOAD_THRESHOLD_PX\) \{\s*void requestOlderHistoryLoad\(\);/,
  '触顶滚轮必须立即进入单飞历史请求，不得额外等待固定帧数',
);
assert.doesNotMatch(
  messageListSource,
  /function scheduleOlderHistoryLoadAfterNativeScroll|remainingFrames > 1|waitForSettledScroll/,
  '历史请求不得依赖固定延迟或第二套滚轮调度器',
);
assert.match(
  messageListSource,
  /historyLoadScopeKey = nextScopeKey;[\s\S]*?layoutObservationNonce \+= 1;[\s\S]*?disconnectContentResizeObserver\(\)/,
  '会话作用域切换必须立即断开旧布局观察，不能让旧 ResizeObserver 作用于新会话',
);
assert.match(
  messageListSource,
  /function compensateLayoutAfterUpdate\([\s\S]*?observationNonce !== layoutObservationNonce[\s\S]*?observationScopeKey !== currentHistoryLoadScopeKey\(\)[\s\S]*?observationInteractionEpoch !== scrollInteractionEpoch[\s\S]*?observationRecoveryEpoch !== scrollRecoveryEpoch[\s\S]*?contentResizeFrame = requestAnimationFrame/,
  'ResizeObserver 的排队帧必须通过统一布局事务校验观察代际、作用域和滚动代际',
);
assert.match(
  messageListSource,
  /function scrollToPositionFromNavigation\(nextTop: number\): void \{[\s\S]*?scrollInteractionEpoch \+= 1;[\s\S]*?function scrollToBottom\(\) \{[\s\S]*?scrollInteractionEpoch \+= 1;/,
  '导航和回到底部必须推进滚动交互代际，使旧事务失效',
);
assert.match(
  messageListSource,
  /const messageElementSignature = \$derived\(activeRenderItems\.map\(\(item\) => item\.key\)\.join\('\|'\)\);/,
  '新增可见消息必须触发布局观察，不能只观察完整但未挂载的历史窗口',
);
assert.match(
  messageListSource,
  /const signature = `\$\{messageElementSignature\}:\$\{runtimeLayoutSignature\}`;[\s\S]*?rememberCurrentLayoutAnchor\(\);/,
  '运行态条目结构变化前必须保留阅读锚点',
);
assert.match(
  messageListSource,
  /displayContext === 'thread'[\s\S]*?historyLoadRequest\?\.scopeKey === currentHistoryLoadScopeKey\(\)[\s\S]*?historyState\.historyLoadStatus === 'loading'/,
  '只有拥有主线程历史请求的 MessageList 实例才能在销毁时收口共享 loading 状态',
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
  /\{#if\s+showRightPaneToggle\}/,
  '右栏开关不能依赖会话或文件加载后才渲染',
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
assert.match(
  workbenchShellSource,
  /\.web-workbench-shell\s*\{[\s\S]*?height:\s*100%;[\s\S]*?width:\s*100%;/,
  '工作台必须填充 App Renderer 的实际客户区，不能用 100vw/100vh 制造额外坐标层',
);
assert.doesNotMatch(
  workbenchShellSource,
  /minmax\([^;\n]*minmax\(/,
  '桌面三栏轨道不得嵌套非法 minmax，避免整条 grid 声明被浏览器丢弃',
);
assert.doesNotMatch(
  workbenchShellSource,
  /66\.6667%/,
  '桌面右栏宽度只能使用 Main 快照的单一轨道值，不能在 Renderer 再次按容器比例缩放',
);
assert.match(
  workbenchShellSource,
  /web-workbench-shell--desktop-right-pane-visible \.workbench-body[\s\S]*?minmax\(var\(--preview-min-width, 320px\), var\(--desktop-right-pane-width, 480px\)\)/,
  '桌面右栏必须直接消费权威宽度，并与中间区域共享同一 grid 坐标系',
);
assert.match(
  workbenchShellSource,
  /web-workbench-shell--desktop-preview-overlay \.workbench-body[\s\S]*?desktop-right-pane-column--overlay/,
  '窄窗口必须由同一套布局切换到右栏覆盖模式，不能继续强行三栏挤压',
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
assert.match(
  inputAreaSource,
  /type:\s*'guideQueuedMessage'[\s\S]*?queuedMessageId:/,
  '单条排队消息必须保留服务端转引导操作',
);
assert.match(
  inputAreaSource,
  /function canGuideQueuedMessage\(queued: QueuedMessage\): boolean \{\s*return queued\.canGuide === true;\s*\}/,
  '排队消息能否转引导必须只使用服务端权威能力字段',
);
assert.doesNotMatch(
  inputAreaSource,
  /function canGuideQueuedMessage\(queued: QueuedMessage\): boolean \{[^}]*isSending[^}]*\}/,
  '前端不得再用瞬时发送状态阻断服务端允许的引导操作',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-actions\s*\{[\s\S]*?opacity:\s*1;[\s\S]*?pointer-events:\s*auto;/,
  '排队消息操作区必须始终可点击，不能依赖 hover 才启用',
);
assert.doesNotMatch(
  inputAreaSource,
  /\.ia-queue-actions\s*\{[^}]*pointer-events:\s*none;/,
  '排队消息操作区不得默认禁用指针事件',
);
assert.doesNotMatch(
  `${zhLocaleSource}\n${enLocaleSource}`,
  /"input\.followUp\.(mode|queue|guide|guideTitle)"|"input\.queue\.(banner|guideInactiveUnavailable)"/,
  '删除输入区模式切换后必须同步清理废弃文案',
);

await withGoldenViteServer(async (server) => {
  const panelLayout = await server.ssrLoadModule('/src/web/panel-layout.ts');

  assert.deepEqual(
    panelLayout.resolvePanelLayout({
      viewportWidth: 1440,
      sidebarWidth: 320,
      previewPanelWidth: 320,
      sidebarVisible: true,
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
      sidebarVisible: true,
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
      sidebarVisible: true,
    }),
    {
      sidebarDrawer: false,
      previewOverlay: true,
      panelsCanCoexist: false,
    },
    'narrow tablet should use a workbench-local browser overlay without suppressing the sidebar',
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
    { sidebarVisible: true, rightPaneVisible: true },
    'right pane width must not implicitly suppress the preferred left pane',
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
    { minWidth: 320, maxWidth: 480 },
    'browser width must reserve the sidebar and conversation minimum together',
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
