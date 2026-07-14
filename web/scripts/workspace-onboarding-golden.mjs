import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const onboarding = await server.ssrLoadModule('/src/stores/workspace-onboarding.svelte.ts');
  const sessionActivity = await server.ssrLoadModule('/src/lib/session-activity-indicator.ts');

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
  /function selectWorkspace\([\s\S]*?type: 'workspaceBindingChanged'[\s\S]*?workspaceId: workspace\.workspaceId[\s\S]*?workspacePath: workspace\.rootPath[\s\S]*?sessionId: ''/,
  '草稿态选择已有项目后必须切换权威工作空间绑定并继续保留新会话',
);
assert.match(
  shellSource,
  /workspaceOnboardingState\.open/,
  'Shell 必须以共享接入状态控制目录选择器',
);
assert.match(
  shellSource,
  /async function registerWorkspaceRoot\(rootPath: string, openDraft: boolean\)[\s\S]*?if \(openDraft\)[\s\S]*?selectComposerDraftWorkspace\(addedWorkspace\.workspaceId\)[\s\S]*?loadWorkspaceSessionsForSidebar\(addedWorkspace\)/,
  '共享工作区注册流程必须能够自动选中新的草稿工作空间并只加载侧栏历史',
);
assert.match(
  shellSource,
  /if \(openDraft\)[\s\S]*?setCurrentSessionId\(null\)[\s\S]*?requestWorkspaceBindingSync\(addedWorkspace, null\)[\s\S]*?loadWorkspaceSessionsForSidebar\(addedWorkspace\)/,
  '从输入区注册目录后必须切换权威工作空间绑定并继续保留新会话',
);
assert.match(
  shellSource,
  /handleFolderSelected[\s\S]*?registerWorkspaceRoot\(normalizedRootPath, onboardingOrigin === 'composer'\)/,
  '目录选择器必须通过显式草稿参数复用唯一工作区注册流程',
);
assert.match(
  shellSource,
  /async function openWorkspaceDraft\(workspace: AgentWorkspaceSummary\): Promise<void>[\s\S]*?selectComposerDraftWorkspace\(workspaceId\)[\s\S]*?type: 'newSession'[\s\S]*?workspaceId,[\s\S]*?workspacePath,[\s\S]*?loadWorkspaceSessionsForSidebar\(workspace\)/,
  '工作空间快捷新会话必须复用单一草稿绑定并由 bridge 进入新会话',
);
assert.match(
  shellSource,
  /class="workspace-new-session-btn"[\s\S]*?disabled=\{workspaceActionPending \|\| messagesState\.sessionHydrating \|\| Boolean\(pendingSessionSwitchId\)\}[\s\S]*?event\.stopPropagation\(\)[\s\S]*?openWorkspaceDraft\(workspace\)[\s\S]*?<Icon name="plus"/,
  '工作空间行必须提供不会触发展开的加号按钮，并在状态切换期间禁用',
);
assert.match(
  shellSource,
  /\.workspace-row:hover \.workspace-new-session-btn,[\s\S]*?\.workspace-row:hover \.workspace-remove-btn[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
  '桌面端工作空间右侧操作必须仅在行悬停时出现',
);
assert.doesNotMatch(
  shellSource,
  /\.workspace-row:focus-within \.workspace-(?:new-session|remove)-btn/,
  '鼠标点击后不能因工作空间行保留焦点而持续显示右侧操作',
);
assert.match(
  shellSource,
  /\.workspace-new-session-btn:focus-visible,[\s\S]*?\.workspace-remove-btn:focus-visible[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
  '键盘导航时仍必须单独显示当前获得可见焦点的操作按钮',
);
assert.match(
  shellSource,
  /@media \(max-width: 900px\)[\s\S]*?\.workspace-new-session-btn[\s\S]*?opacity: 1;[\s\S]*?pointer-events: auto;/,
  '抽屉和窄屏布局必须常显工作空间新会话按钮',
);
assert.match(
  inputAreaSource,
  /async function loadPickerModels\(\)[\s\S]*?catch \(error\) \{[\s\S]*?pickerModelsConfigKey = configKey;[\s\S]*?pickerLoadedOnce = true;[\s\S]*?console\.warn\('\[InputArea\] 拉取主线模型列表失败:'/,
  '主模型列表自动加载失败后必须记录当前配置已尝试，后续仅由用户显式重试',
);
assert.match(
  inputAreaSource,
  /import \{ canFetchModelList \} from '\.\.\/shared\/model-governance';[\s\S]*?function orchestratorModelListConfigKey[\s\S]*?if \(!config \|\| !canFetchModelList\(config\)\) return '';/,
  '主模型列表自动加载必须复用统一连接配置校验，未配置连接时不能发起请求',
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
assert.match(
  shellSource,
  /class:running=\{sessionIndicator === 'running'\}[\s\S]*?class:unread=\{sessionIndicator === 'unread'\}/,
  '会话指示灯必须分别渲染运行和未读完成状态',
);
assert.match(
  shellSource,
  /\.session-running-dot\.unread::before[\s\S]*?background: var\(--success\);/,
  '未读完成状态必须使用绿色常亮灯',
);
assert.match(
  bridgeSource,
  /EXTERNAL_SESSION_SUMMARY_EVENTS = new Set\(\[[\s\S]*?'session\.viewed'/,
  '其他会话的已查看事件必须触发会话列表同步',
);
assert.match(
  bridgeSource,
  /shouldRefreshCurrentWorkspaceSessionSummary[\s\S]*?eventType === 'session\.viewed'/,
  '当前会话的已查看事件也必须触发跨客户端状态同步',
);

console.log('workspace onboarding golden passed');
