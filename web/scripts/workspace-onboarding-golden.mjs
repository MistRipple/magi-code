import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const onboarding = await server.ssrLoadModule('/src/stores/workspace-onboarding.svelte.ts');
  const sessionActivity = await server.ssrLoadModule('/src/lib/session-activity-indicator.ts');
  const { workspacePathHints } = await server.ssrLoadModule('/src/lib/workspace-display.ts');
  const workspace = (workspaceId, name, rootPath) => ({ workspaceId, name, rootPath });
  assert.deepEqual(workspacePathHints([workspace('a', 'magi', '/code/magi')]), {});
  assert.deepEqual(workspacePathHints([
    workspace('a', 'workspace', '/tmp/original/workspace'),
    workspace('b', 'workspace', '/tmp/isolated/workspace'),
  ]), { a: 'original', b: 'isolated' });
  assert.deepEqual(workspacePathHints([
    workspace('a', 'workspace', '/projects/one/shared/workspace/'),
    workspace('b', 'workspace', '/projects/two/shared/workspace'),
  ]), { a: 'one/shared', b: 'two/shared' });
  assert.deepEqual(workspacePathHints([
    workspace('a', 'repo', 'C:\\dev\\repo'),
    workspace('b', 'repo', 'D:\\dev\\repo'),
  ]), { a: 'C:/dev', b: 'D:/dev' });
  assert.deepEqual(workspacePathHints([
    workspace('a', 'Project', '/code/frontend'),
    workspace('b', 'Project', '/code/backend'),
  ]), { a: 'frontend', b: 'backend' });

  assert.equal(onboarding.workspaceOnboardingState.open, false);
  assert.equal(onboarding.workspaceOnboardingState.origin, null);

  onboarding.openWorkspaceFolderPicker('composer');
  assert.equal(onboarding.workspaceOnboardingState.open, true);
  assert.equal(onboarding.workspaceOnboardingState.origin, 'composer');

  onboarding.closeWorkspaceFolderPicker();
  assert.equal(onboarding.workspaceOnboardingState.open, false);
  assert.equal(onboarding.workspaceOnboardingState.origin, null);

  onboarding.openWorkspaceFolderPicker('sidebar');
  assert.equal(onboarding.workspaceOnboardingState.origin, 'sidebar');

  assert.equal(
    sessionActivity.resolveSessionActivityIndicator({
      isRunning: true,
      hasUnreadCompletion: true,
    }),
    'running',
    '运行状态必须优先于未读完成状态',
  );
  assert.equal(
    sessionActivity.resolveSessionActivityIndicator({
      isRunning: false,
      hasUnreadCompletion: true,
    }),
    'unread',
    '后台成功完成且未查看时必须显示未读完成状态',
  );
  assert.equal(
    sessionActivity.shouldMarkSessionCompletionViewed({
      bootstrapped: true,
      sessionHydrating: false,
      isCurrentSession: true,
      isRunning: false,
      hasUnreadCompletion: true,
    }),
    true,
    '当前会话内容完成加载后必须回写已查看状态',
  );
  assert.equal(
    sessionActivity.shouldMarkSessionCompletionViewed({
      bootstrapped: true,
      sessionHydrating: true,
      isCurrentSession: true,
      isRunning: false,
      hasUnreadCompletion: true,
    }),
    false,
    '会话内容仍在加载时不能提前清除未读状态',
  );
});

const inputAreaSource = await readFile(
  new URL('../src/components/InputArea.svelte', import.meta.url),
  'utf8',
);
const shellSource = await readFile(
  new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
  'utf8',
);
const globalStyle = await readFile(
  new URL('../src/styles/global.css', import.meta.url),
  'utf8',
);
const bridgeSource = await readFile(
  new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url),
  'utf8',
);

assert.match(
  inputAreaSource,
  /openWorkspaceFolderPicker\('composer'\)/,
  '输入区工作空间菜单必须能够直接打开现有目录选择器',
);
assert.doesNotMatch(
  inputAreaSource,
  /disabled=\{workspaceOptions\.length === 0 \|\|/,
  '没有已注册项目时工作空间按钮仍必须可用',
);
assert.match(
  inputAreaSource,
  /input\.workspace\.useExistingFolder/,
  '工作空间菜单必须展示使用现有文件夹操作',
);
assert.match(
  inputAreaSource,
  /function workspaceNavigationPath\(workspace: ComposerWorkspaceOption\)[\s\S]*?workspace\.rootPath\.trim\(\)/,
  '输入区工作空间导航必须使用规范化的真实工作区路径',
);
assert.match(
  inputAreaSource,
  /function selectWorkspace\([\s\S]*?navigateSession\(\{[\s\S]*?kind: 'draft'[\s\S]*?workspaceId: workspace\.workspaceId[\s\S]*?workspacePath: workspaceNavigationPath\(workspace\)/,
  '草稿态选择已有项目后必须通过统一导航事务使用规范化工作区路径',
);
assert.match(
  shellSource,
  /workspaceOnboardingState\.open/,
  'Shell 必须以共享接入状态控制目录选择器',
);
assert.match(
  shellSource,
  /async function registerWorkspaceRoot\(rootPath: string\)[\s\S]*?workspaceId === registration\.workspaceId[\s\S]*?navigateSession\(\{[\s\S]*?kind: 'draft'[\s\S]*?workspaceId: addedWorkspace\.workspaceId[\s\S]*?workspacePath: workspaceNavigationPath\(addedWorkspace\)[\s\S]*?waitForSessionNavigation\(navigation\)/,
  '共享工作区注册流程必须以服务端返回的 workspaceId 为权威身份，并等待统一导航事务完成',
);
assert.match(
  shellSource,
  /handleFolderSelected[\s\S]*?registerWorkspaceRoot\(normalizedRootPath\)/,
  '目录选择器必须复用唯一工作区注册并绑定当前会话流程',
);
assert.match(
  shellSource,
  /async function openWorkspaceDraft\(workspace: AgentWorkspaceSummary\): Promise<void>[\s\S]*?const workspacePath = workspaceNavigationPath\(workspace\)[\s\S]*?navigateSession\(\{ kind: 'draft', scope: 'workspace', workspaceId, workspacePath \}\)/,
  '工作空间快捷新会话必须使用规范化工作区路径进入统一导航事务',
);
assert.match(
  shellSource,
  /列表同步 effect：[\s\S]*?const projections = messagesState\.workspaceSessionProjections[\s\S]*?nextSessionsByWorkspace[\s\S]*?nextSessionsByWorkspace\[normalizedWorkspaceId\] = currentSessions/,
  '侧栏会话集合必须按列表自身的工作区作用域同步，不能由当前草稿指针推断归属',
);
assert.match(
  shellSource,
  /async function openWorkspaceDraft[\s\S]*?navigateSession\(\{ kind: 'draft', scope: 'workspace', workspaceId, workspacePath \}\)[\s\S]*?if \(!sessionsAlreadyLoaded\)[\s\S]*?loadWorkspaceSessionsForSidebar\(workspace\)/,
  '工作区草稿导航与侧栏目录加载必须保持职责分离',
);
assert.doesNotMatch(
  shellSource,
  /if \(!bootstrapSessionId\) \{[\s\S]{0,900}?loadWorkspaceSessionsForSidebar/,
  '当前会话指针清空不得驱动工作区会话目录重新加载',
);
assert.match(
  shellSource,
  /class="btn-icon btn-icon--sm row-action"[\s\S]*?disabled=\{workspaceActionPending \|\| messagesState\.sessionHydrating\}[\s\S]*?event\.stopPropagation\(\)[\s\S]*?openWorkspaceDraft\(workspace\)[\s\S]*?<Icon name="plus"/,
  '工作空间行必须提供不会触发展开的加号按钮，并在工作区操作或会话加载期间禁用',
);
assert.match(
  shellSource,
  /\.workspace-row:hover \.row-action,[\s\S]*?\.row-action:focus-visible[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
  '桌面端工作空间右侧操作必须仅在行悬停时出现',
);
assert.doesNotMatch(
  shellSource,
  /\.workspace-row:focus-within \.row-action/,
  '鼠标点击后不能因工作空间行保留焦点而持续显示右侧操作',
);
assert.match(
  shellSource,
  /\.row-action:focus-visible\s*\{[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
  '键盘导航时仍必须单独显示当前获得可见焦点的操作按钮',
);
assert.match(
  shellSource,
  /@media \(hover: none\), \(pointer: coarse\)[\s\S]*?\.row-action,[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
  '触控设备必须常显工作空间操作，不能仅按窗口宽度把鼠标设备当成触屏',
);
assert.match(
  inputAreaSource,
  /async function loadPickerModels\(\)[\s\S]*?catch \(error\) \{[\s\S]*?pickerModelsConfigKey = configKey;[\s\S]*?pickerLoadedOnce = true;[\s\S]*?console\.warn\('\[InputArea\] 拉取主线模型列表失败:'/,
  '主模型列表自动加载失败后必须记录当前配置已尝试，后续仅由用户显式重试',
);
assert.match(
  inputAreaSource,
  /function orchestratorModelListConfigKey[\s\S]*?if \(!config\) return '';[\s\S]*?Array\.isArray\(config\.sources\) \? config\.sources : \[\]/,
  '主模型列表只在设置快照未就绪时不请求；用户数据源为空时仍要请求，因为插件可以单独提供数据源',
);
assert.doesNotMatch(
  inputAreaSource,
  /function orchestratorModelListConfigKey[\s\S]{0,300}sources\.length === 0\) return ''/,
  '用户数据源为空不能让 key 为空：加载成功后刷新设置快照会触发 effect，空 key 会把刚拉到的插件模型清空',
);
assert.match(
  inputAreaSource,
  /pickerPluginRevision !== pluginRevision\) pickerLoadedOnce = false;/,
  '插件注册表变化（激活、停用、升级）后必须重新拉取主模型目录',
);
assert.match(
  shellSource,
  /session\.hasUnreadCompletion !== next\.hasUnreadCompletion/,
  '会话列表同步必须比较未读完成状态',
);
assert.match(
  shellSource,
  /markAgentSessionViewed\([\s\S]*?hasUnreadCompletion: false/,
  '成功标记已查看后必须同步清除本地未读完成状态',
);
const indicatorSource = await readFile(
  new URL('../src/components/SessionActivityIndicator.svelte', import.meta.url),
  'utf8',
);
assert.equal(
  (shellSource.match(/<SessionActivityIndicator\b/g) ?? []).length,
  4,
  '项目会话、无项目会话和外部会话（运行、等待）必须共用同一个会话指示组件',
);
assert.doesNotMatch(
  shellSource,
  /session-running-dot|personal-session-indicator|external-pulse/,
  '侧栏不能保留第二套会话运行指示实现',
);
assert.match(
  indicatorSource,
  /\.session-activity-indicator\.unread::before[\s\S]*?background: var\(--success\);/,
  '未读完成状态必须使用绿色常亮灯',
);
assert.match(
  indicatorSource,
  /\.session-activity-indicator\.awaiting::before[\s\S]*?background: var\(--warning\);/,
  '等待确认状态必须使用黄色常亮灯',
);
assert.match(
  indicatorSource,
  /\.session-activity-indicator\.running :global\(svg\) \{\s*animation: magi-status-spin 1\.2s linear infinite;/,
  '运行状态只用匀速旋转表达',
);
assert.doesNotMatch(
  indicatorSource,
  /breath|pulse/,
  '运行状态不能叠加呼吸或脉冲动画',
);
assert.match(
  indicatorSource,
  /prefers-reduced-motion: reduce[\s\S]*?animation-iteration-count: infinite !important/,
  '运行状态的旋转不能因系统减少动态效果而停止',
);
assert.match(
  globalStyle,
  /prefers-reduced-motion: reduce\)[\s\S]*?:is\(\s*\.spinning[\s\S]*?\) \{\s*animation-name: magi-status-spin !important;[\s\S]*?animation-iteration-count: infinite !important;/,
  '减少动态效果时转圈等待指示必须继续旋转，不能降为呼吸',
);
assert.doesNotMatch(
  globalStyle,
  /prefers-reduced-motion: reduce[\s\S]*?:is\([^)]*\.spinning[^)]*\),[^{]*\{[^}]*magi-status-pulse/,
  '转圈指示不能再进入呼吸降级规则',
);
assert.doesNotMatch(
  globalStyle,
  /session-activity-indicator|personal-session-indicator|session-running-dot/,
  '全局降级规则不能再把会话运行指示改成呼吸',
);
assert.match(
  bridgeSource,
  /EXTERNAL_SESSION_SUMMARY_EVENTS = new Set\(\[[\s\S]*?'session\.viewed'/,
  '其他会话的已查看事件必须触发会话列表同步',
);
assert.match(
  bridgeSource,
  /shouldRefreshCurrentSessionSummary[\s\S]*?eventType === 'session\.viewed'[\s\S]*?TURN_TERMINAL_EVENTS\.has\(eventType\)[\s\S]*?eventType === 'session\.turn\.superseded'/,
  '当前会话的已查看、Turn 终止和替代事件都必须触发跨客户端状态同步',
);
assert.match(
  shellSource,
  /resolveSessionRunningState\(\{[\s\S]*?isRunning: session\.isRunning/,
  '侧栏运行灯必须以目录接口返回的显式 isRunning 为权威值，不能被残留任务数抬回运行中',
);
assert.match(
  bridgeSource,
  /function scheduleSessionSummaryRefresh[\s\S]*?(?:getWorkspaceSessions|getPersonalSessions)[\s\S]*?emitDataMessage\('sessionsUpdated'/,
  '会话状态事件必须通过对应作用域的会话目录接口同步运行态和未读完成态',
);
assert.doesNotMatch(
  bridgeSource,
  /applyWorkspaceSessionSnapshotToBootstrap|workspaceSessionSnapshot/,
  'bootstrap 不得再混入另一条会话目录快照，避免新旧读取链路组合出伪造的高游标旧列表',
);
assert.match(
  bridgeSource,
  /function refreshSessionDirectoryAfterBootstrap[\s\S]*?runtimeEpoch: snapshot\.runtimeEpoch[\s\S]*?eventStreamNextSequence: snapshot\.eventStreamNextSequence/,
  'bootstrap 后的会话运行态必须通过带运行时代际和事件游标的唯一目录快照接管',
);
assert.match(
  bridgeSource,
  /function refreshSessionDirectoryAfterBootstrap[\s\S]*?workspaceId\s*\?\s*await getWorkspaceSessions[\s\S]*?await getPersonalSessions\(\)/,
  'bootstrap 的会话记录不含运行态；个人会话与工作区会话都必须在 bootstrap 后重读目录，否则导航会抹掉其他会话的响应中状态',
);
assert.match(
  shellSource,
  /async function refreshWorkspaces\(options: \{ silent\?: boolean \} = \{\}\): Promise<void> \{[\s\S]*?workspaces = next;[\s\S]*?sessionsByWorkspace = Object\.fromEntries[\s\S]*?if \(!messagesState\.bootstrapped\) \{[\s\S]*?return;[\s\S]*?\}[\s\S]*?selectedWorkspaceId = resolveBackendWorkspaceSelection\(next\)/,
  '工作区列表先到达时必须等待无作用域 bootstrap 恢复最后会话，不能按 isActive 抢先触发工作区切换',
);
assert.match(
  shellSource,
  /工作区指针同步 effect[\s\S]*?const authoritativeWorkspaceId = currentBootstrapWorkspaceId\(\);[\s\S]*?selectedWorkspaceId = authoritativeWorkspaceId;[\s\S]*?currentSessionId = bootstrapSessionId \|\| null;[\s\S]*?refreshWorkspaceSessions\([\s\S]*?authoritativeWorkspaceId,[\s\S]*?bootstrapSessionId/,
  'bootstrap 返回后必须以恢复的工作区和会话作为侧栏加载作用域',
);
assert.match(
  shellSource,
  /个人会话没有 workspace 指针[\s\S]*?if \(!messagesState\.bootstrapped \|\| loading \|\| workspaceActionPending\)[\s\S]*?if \(currentBootstrapWorkspaceId\(\)\)[\s\S]*?selectedWorkspaceId = ''[\s\S]*?currentSessionId !== nextSessionId[\s\S]*?currentSessionId = nextSessionId;/,
  '切换到无项目对话后必须清除旧工作区侧栏选择，并同步当前个人会话的 active 指针',
);

console.log('workspace onboarding golden passed');
