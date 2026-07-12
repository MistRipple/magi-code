import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const onboarding = await server.ssrLoadModule('/src/stores/workspace-onboarding.svelte.ts');

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
});

const inputAreaSource = await readFile(
  new URL('../src/components/InputArea.svelte', import.meta.url),
  'utf8',
);
const shellSource = await readFile(
  new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
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
  /onboardingOrigin === 'composer'[\s\S]*?selectComposerDraftWorkspace\([\s\S]*?loadWorkspaceSessionsForSidebar\(/,
  '从输入区注册目录后必须自动选中新的草稿工作空间',
);
assert.match(
  shellSource,
  /onboardingOrigin === 'composer'[\s\S]*?requestWorkspaceBindingSync\(addedWorkspace, null\)[\s\S]*?loadWorkspaceSessionsForSidebar\(/,
  '从输入区注册目录后必须切换权威工作空间绑定并继续保留新会话',
);
assert.match(
  shellSource,
  /onboardingOrigin !== 'composer' && selectedWorkspaceId[\s\S]*?refreshWorkspaceSessions\(/,
  '输入区添加目录不能自动切入该项目的历史会话',
);

console.log('workspace onboarding golden passed');
