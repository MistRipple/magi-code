# 桌面更新进度与主线模型刷新 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在桌面更新下载期间显示真实确定/不确定进度条，并为主线模型选择器增加可重复点击的最新模型列表刷新入口。

**Architecture:** 复用现有 `DesktopUpdateProgress` 和 Tauri updater 下载回调，不增加第二套更新状态机；两个更新入口只增加统一进度展示。主线模型选择器在现有 `loadPickerModels` 基础上增加显式强制刷新参数，仍通过既有 `/api/settings/models/fetch` 请求和当前全局主模型配置取数。

**Tech Stack:** Svelte 5、TypeScript、Vite、Tauri updater、现有 Node Golden 脚本、daemon 托管真实浏览器入口。

## Global Constraints

- 保持 `DesktopUpdateProgress` 为唯一下载进度契约。
- 已知 `contentLength` 才显示百分比；未知总大小显示不确定动画，不伪造数值。
- 模型刷新期间保留旧列表，不清空当前模型，不允许重复请求。
- 不新增 API、缓存层、第二套下载逻辑或回退实现。
- 所有新增文案同时维护 `zh-CN` 与 `en-US`。

---

### Task 1: 扩展更新与模型刷新 Golden 测试

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/web/scripts/desktop-updater-golden.mjs`

**Interfaces:**
- Consumes: `DesktopUpdateProgress`、`SettingsPanel.svelte`、`DesktopUpdatePrompt.svelte`、`InputArea.svelte` 源码。
- Produces: 能证明确定/不确定进度条和主线模型刷新入口存在的失败测试。

- [ ] **Step 1: 写失败测试**

在现有 `desktop-updater-golden.mjs` 中增加源码断言：

```js
assert.match(settingsSource, /desktop-update-progress/, 'settings must render an update progress bar');
assert.match(promptSource, /desktop-update-progress/, 'startup update prompt must render an update progress bar');
assert.match(settingsSource, /aria-valuenow/, 'settings progress must expose accessible numeric progress');
assert.match(promptSource, /ia-update-progress__indeterminate/, 'startup prompt must support unknown content length');

const inputSource = fs.readFileSync(path.resolve('src/components/InputArea.svelte'), 'utf8');
assert.match(inputSource, /refreshPickerModels/, 'main model picker must expose an explicit refresh action');
assert.match(inputSource, /input\.mainModelPicker\.refresh/, 'refresh action must have a localized accessible label');
```

- [ ] **Step 2: 运行失败测试**

运行：`npm --prefix web run test:desktop-updater`

预期：失败，提示设置面板、启动提示或主线模型刷新入口尚未实现。

- [ ] **Step 3: 确认失败原因**

只接受因目标源码缺少上述进度条和刷新入口导致的断言失败；若出现脚本加载错误，先修正测试路径或断言后重新运行。

### Task 2: 实现统一更新进度条展示

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/components/SettingsPanel.svelte`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/components/DesktopUpdatePrompt.svelte`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/styles/global.css`

**Interfaces:**
- Consumes: `updateProgress: DesktopUpdateProgress | null`、`progress: DesktopUpdateProgress | null`。
- Produces: `.desktop-update-progress`、`.desktop-update-progress__fill`、`.desktop-update-progress__fill--indeterminate` 三个统一样式类，以及可访问的进度条 DOM。

- [ ] **Step 1: 增加设置面板进度条**

在 `SettingsPanel.svelte` 的 `updateState === 'installing'` 分支中，保留按钮文案，并在更新按钮相邻位置渲染：

```svelte
{#if updateState === 'installing'}
  <div
    class="desktop-update-progress"
    role="progressbar"
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow={updateProgress?.percent}
    aria-label={i18n.t('settings.update.installing')}
  >
    <span
      class:desktop-update-progress__fill--indeterminate={updateProgress?.percent === undefined}
      class="desktop-update-progress__fill"
      style:width={updateProgress?.percent !== undefined ? `${updateProgress.percent}%` : undefined}
    ></span>
  </div>
{/if}
```

- [ ] **Step 2: 增加启动更新提示进度条**

在 `DesktopUpdatePrompt.svelte` 的安装中内容区中使用同一组类和无障碍属性，宽度由提示卡片内容区域决定，不改变按钮排列。

- [ ] **Step 3: 添加统一样式**

在 `web/src/styles/global.css` 增加紧凑轨道、确定填充和不确定动画；移动端限制最大宽度为内容区域，避免更新提示溢出。

- [ ] **Step 4: 运行更新 Golden 测试**

运行：`npm --prefix web run test:desktop-updater`

预期：通过，且 `formatUpdateProgress(512)` 仍返回无百分比的不确定状态。

### Task 3: 实现主线模型列表强制刷新

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/components/InputArea.svelte`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/i18n/zh-CN.json`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/i18n/en-US.json`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/scripts/desktop-updater-golden.mjs`

**Interfaces:**
- Consumes: `loadPickerModels()`、`fetchAgentModelList()`、`pickerLoading`、`pickerModels`。
- Produces: `refreshPickerModels()`，通过 `loadPickerModels(true)` 强制重新请求当前主模型配置。

- [ ] **Step 1: 为刷新行为补充断言**

在 Golden 脚本中断言模型列表标题区域包含 `refreshPickerModels` 调用、`refresh` 图标和本地化标题。

- [ ] **Step 2: 增加最小刷新实现**

保留现有 `async function loadPickerModels()` 签名，新增刷新函数先清除本次加载标记再复用同一加载路径；新增：

```ts
async function refreshPickerModels() {
  if (pickerLoading) return;
  pickerLoadedOnce = false;
  await loadPickerModels();
}
```

请求失败时不清空原有 `pickerModels`，继续沿用当前错误提示和重试行为。

- [ ] **Step 3: 增加刷新图标按钮**

在 `ia-section-header-row` 标题右侧加入无文字按钮：

```svelte
<button
  type="button"
  class="ia-picker-refresh"
  onclick={() => void refreshPickerModels()}
  disabled={pickerLoading || pickerSavingModel !== null || pickerSavingReasoning !== null}
  title={i18n.t('input.mainModelPicker.refresh')}
  aria-label={i18n.t('input.mainModelPicker.refresh')}
>
  <Icon name="refresh" size={13} class:spinning={pickerLoading} />
</button>
```

- [ ] **Step 4: 增加中英文文案并补样式**

新增：

```json
"input.mainModelPicker.refresh": "刷新模型列表"
```

英文对应为 `Refresh model list`；按钮使用无边框、圆形悬浮样式，保持选择器现有视觉重量。

- [ ] **Step 5: 运行前端检查和相关测试**

运行：`npm --prefix web run check && npm --prefix web run test:desktop-updater`

预期：类型检查无错误/警告，更新与模型刷新 Golden 测试通过。

### Task 4: 真实浏览器验收与回归验证

**Files:**
- Modify: 无
- Test: `/Users/xie/code/magi-rust-rewrite/web/src/components/SettingsPanel.svelte`
- Test: `/Users/xie/code/magi-rust-rewrite/web/src/components/DesktopUpdatePrompt.svelte`
- Test: `/Users/xie/code/magi-rust-rewrite/web/src/components/InputArea.svelte`

**Interfaces:**
- Consumes: daemon 托管开发入口 `http://127.0.0.1:38123/web.html`。
- Produces: 浏览器场景验收记录和最终构建结果。

- [ ] **Step 1: 启动 daemon 托管开发入口**

运行：`MAGI_WEB_DEV=1 ./scripts/dev-daemon.sh`

浏览器只访问：`http://127.0.0.1:38123/web.html`。

- [ ] **Step 2: 验证设置更新进度呈现**

打开设置面板，检查更新按钮在检查、可用、安装中和失败状态下的布局；安装中确认确定进度条会随事件填充，未知总大小时显示不确定动画。

- [ ] **Step 3: 验证启动更新提示呈现**

关闭设置面板并保持启动更新提示可见，确认下载期间进度条、文字和重试按钮不互相遮挡。

- [ ] **Step 4: 验证主线模型刷新**

打开主线模型选择器，点击模型标题右侧刷新图标一次，确认只发起一次模型列表请求、旧列表不会闪空、返回的新模型可选择且当前选中模型仍保持选中。

- [ ] **Step 5: 运行完整验证**

运行：`npm --prefix web run test && npm --prefix web run build && cargo check -p magi-daemon`

预期：全部通过，生产构建产物可由 daemon 正常加载。

- [ ] **Step 6: 检查改动范围**

运行：`git diff --check` 和 `git status --short`；只保留本次功能文件和既有未跟踪目录，不提交无关改动。
