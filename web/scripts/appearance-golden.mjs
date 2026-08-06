import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';

const [mainWeb, runtime, desktopAppearance, desktopCapability, appearanceContract, client, settingsPanel, settingsAppearance, modal, modelConfigForm, enginePicker, knowledgePanel, runtimeStatePanel, settingsRules, settingsTools, webFolderPicker, workbenchShell, globalCss, settingsCss, bridge] = await Promise.all([
  readFile(new URL('../src/main-web.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/appearance/runtime.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/lib/desktop-appearance.ts', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/capabilities/default.json', import.meta.url), 'utf8'),
  readFile(new URL('../src/appearance/contract.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/appearance/client.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/SettingsPanel.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/SettingsAppearanceTab.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/Modal.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/ModelConfigForm.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/EnginePicker.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/KnowledgePanel.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/RuntimeStatePanel.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/SettingsRulesTab.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/SettingsToolsTab.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/web/WebFolderPicker.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/styles/global.css', import.meta.url), 'utf8'),
  readFile(new URL('../src/styles/settings.css', import.meta.url), 'utf8'),
  readFile(new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url), 'utf8'),
]);

assert.match(mainWeb, /initializeAppearanceRuntime\(\)/, 'Web 启动必须初始化 daemon 权威外观运行时');
assert.ok(
  mainWeb.indexOf('initializeAppearanceRuntime()') < mainWeb.indexOf('bootstrapApp(bridge, WebWorkbenchShell)'),
  '必须先恢复权威主题再挂载工作台，避免冷启动主题闪烁',
);
assert.doesNotMatch(runtime, /localStorage\.setItem/, '外观运行时不得把 localStorage 作为主题权威');
assert.match(runtime, /fetchAppearanceSnapshot[\s\S]*?applyAppearanceSnapshot/, '主题恢复必须从 daemon 快照进入唯一应用链路');
assert.match(runtime, /synchronizeDesktopAppearance[\s\S]*?backgroundColor: scheme\.background[\s\S]*?mode: nextMode/, '权威主题应用链路必须同步桌面壳背景与实际明暗模式');
assert.match(desktopAppearance, /isDesktopRuntime[\s\S]*?requestedSequence[\s\S]*?synchronizationQueue[\s\S]*?getCurrentWebviewWindow[\s\S]*?setTheme[\s\S]*?setBackgroundColor/, '桌面壳外观必须串行同步并保证最新主题最终生效');
for (const permission of [
  'core:window:allow-set-background-color',
  'core:window:allow-set-theme',
  'core:webview:allow-set-webview-background-color',
]) {
  assert.match(desktopCapability, new RegExp(permission), `桌面壳外观缺少最小权限：${permission}`);
}
assert.match(runtime, /--magi-surface-dialog[\s\S]*?--magi-surface-popover[\s\S]*?--magi-surface-critical[\s\S]*?--magi-window-overlay[\s\S]*?--magi-popover-overlay[\s\S]*?--magi-critical-overlay/, '主题必须提供完整的语义材质表面');
assert.match(runtime, /pruneAssetUrls[\s\S]*?URL\.revokeObjectURL/, '切换背景后必须释放未使用的 Blob URL');
assert.match(runtime, /resolveAppearanceAssetUrl[\s\S]*?referencedAppearanceAssetIds[\s\S]*?library\?\.themes/, '主题卡片与运行时必须共享资源 URL，并按主题库引用统一管理生命周期');
assert.match(client, /themes\/import[\s\S]*?themes\/\$\{encodeURIComponent\(themeId\)\}\/export/, '用户主题必须支持导入和导出');
assert.match(client, /document\.body\.append\(anchor\)[\s\S]*?setTimeout\([\s\S]*?revokeObjectURL/, '主题导出必须等待浏览器接管下载后再释放 Blob URL');
assert.match(settingsPanel, /SettingsAppearanceTab[\s\S]*?activeTab === 'appearance'/, '外观必须是设置中的一级导航');
assert.ok(
  settingsPanel.indexOf("onclick={() => store.activeTab = 'stats'}")
    < settingsPanel.indexOf("onclick={() => store.activeTab = 'appearance'}")
    && settingsPanel.indexOf("onclick={() => store.activeTab = 'appearance'}")
      < settingsPanel.indexOf("onclick={() => store.activeTab = 'project'}"),
  '外观导航必须位于统计之后、项目之前，DOM 与键盘焦点顺序保持一致',
);
assert.match(settingsAppearance, /createAppearanceTheme[\s\S]*?updateAppearanceTheme[\s\S]*?deleteAppearanceTheme/, '自定义与导入主题必须具备完整生命周期');
assert.match(settingsAppearance, /visibleThemes[\s\S]*?record\.pack\.id !== 'builtin\.system'[\s\S]*?system-appearance-row[\s\S]*?<Toggle/, '跟随系统必须作为独立策略控制，不得占用普通主题卡片');
assert.match(settingsAppearance, /toggleSystemAppearance[\s\S]*?builtin\.system[\s\S]*?builtin\.light[\s\S]*?builtin\.dark/, '关闭跟随系统时必须保持当前明暗外观');
assert.match(settingsAppearance, /resolveAppearanceAssetUrl[\s\S]*?preview-wallpaper[\s\S]*?background-position[\s\S]*?preview-wallpaper-dim/, '主题卡片必须显示壁纸缩略图并还原焦点、模糊和遮光参数');
assert.match(settingsAppearance, /previewAppearanceTheme[\s\S]*?restoreActiveAppearance/, '主题编辑必须使用可取消的实时预览事务');
assert.match(settingsAppearance, /fixedCustomPack[\s\S]*?schemePolicy: targetMode[\s\S]*?schemes: targetMode === 'light'/, '自定义主题必须固化来源主题当前实际生效的单一配色');
assert.doesNotMatch(settingsAppearance, /setSchemePolicy|appearance\.colorMode|appearance\.scheme\.|appearance\.typography|appearance\.uiFont|appearance\.codeFont/, '自定义主题不得暴露嵌套颜色模式或字体配置');
assert.doesNotMatch(appearanceContract, /typography|uiFont|codeFont/, '主题包协议不得保留无跨平台一致性的字体字段');
assert.doesNotMatch(runtime, /UI_FONT_STACKS|CODE_FONT_STACKS|pack\.typography/, '主题运行时不得修改全局字体偏好');
assert.doesNotMatch(settingsAppearance, /window\.confirm/, '主题删除必须使用产品内确认界面');
assert.match(globalCss, /\.btn-icon[\s\S]*?\.btn[\s\S]*?\.form-input[\s\S]*?\.form-textarea[\s\S]*?\.ui-segmented[\s\S]*?\.control-range[\s\S]*?\.inline-alert/, '基础控件必须由全局 primitives 统一定义');
assert.match(settingsAppearance, /btn btn--primary[\s\S]*?form-input[\s\S]*?control-color[\s\S]*?control-range/, '主题页必须复用全局按钮、表单、颜色和范围控件');
assert.match(globalCss, /\[data-magi-surface\][\s\S]*?var\(--magi-wallpaper-image\)[\s\S]*?background-attachment: scroll, fixed/, '独立窗口必须通过全局材质 primitive 复用同一视口背景');
assert.match(modal, /data-magi-surface="window"/, '通用 Modal 必须接入统一窗口材质');
assert.match(settingsPanel, /magi-settings-layout" data-magi-surface="window"/, '设置窗口必须接入统一窗口材质');
assert.match(settingsAppearance, /theme-editor" data-magi-surface="window"[\s\S]*?confirm-dialog" data-magi-surface="critical"/, '主题编辑与关键确认必须使用对应语义材质');
assert.match(modelConfigForm, /model-dropdown"[\s\S]*?data-magi-surface="popover"/, '模型选择浮层必须接入统一悬浮材质');
assert.match(modelConfigForm, /function portalToBody[\s\S]*?document\.body\.appendChild\(node\)/, '模型选择浮层必须挂载到页面根层，不能受设置窗口坐标系与裁切影响');
assert.match(modelConfigForm, /use:portalToBody[\s\S]*?class="model-dropdown"/, '模型选择列表必须通过统一根层 portal 渲染');
assert.match(modelConfigForm, /addEventListener\('scroll', handleScroll, true\)/, '模型选择浮层必须在任意滚动容器滚动时关闭，避免脱离锚点');
assert.match(enginePicker, /engine-picker-popup"[\s\S]*?data-magi-surface="popover"/, '代理模型选择浮层必须接入统一悬浮材质');
assert.match(knowledgePanel, /kp-confirm-dialog" data-magi-surface="critical"/, '知识库关键确认必须接入统一关键材质');
assert.match(runtimeStatePanel, /runtime-diagnostics__content" data-magi-surface="popover"/, '运行态展开面板必须接入统一悬浮材质');
assert.doesNotMatch(settingsAppearance, /\.(primary-button|secondary-button|icon-button|danger-button|segmented-control)\s*\{/, '主题页不得重复定义基础控件样式');
assert.match(modelConfigForm, /ui-segmented url-mode-switch/, '模型 URL 模式必须复用全局分段控件');
assert.match(settingsPanel, /ui-segmented mcp-transport-switch/, 'MCP 传输模式必须复用全局分段控件');
assert.match(settingsPanel, /class="form-input"[\s\S]*?class="form-textarea"/, '设置弹窗必须显式复用全局表单控件');
assert.match(settingsRules, /form-textarea user-rules-textarea[\s\S]*?form-input safeguard-add-input/, '规则设置必须复用全局输入和文本域');
assert.match(settingsTools, /btn btn--primary btn--sm[\s\S]*?btn-icon btn-icon--sm/, '工具设置必须复用全局按钮和图标按钮');
assert.match(webFolderPicker, /btn btn--secondary btn--sm[\s\S]*?btn btn--primary btn--sm/, '文件夹选择器必须复用全局按钮');
assert.match(workbenchShell, /btn btn--secondary[\s\S]*?btn btn--danger/, '工作区确认弹窗必须复用全局按钮');
assert.doesNotMatch(
  [settingsPanel, settingsAppearance, modelConfigForm, settingsRules, settingsTools, webFolderPicker, workbenchShell, settingsCss].join('\n'),
  /apple-action-btn|(?<!header-)settings-btn|modal-btn|llm-config-input|llm-config-select|profile-textarea|btn-icon--error/,
  '设置与弹窗不得保留旧基础控件的双实现',
);
assert.doesNotMatch(settingsCss, /\.btn-icon\s*\{|\.form-field\s+(?:input|textarea)/, '设置样式不得覆盖全局基础控件外观');
assert.doesNotMatch(settingsCss, /\.modal-(?:overlay|dialog|header|body|footer)|\.dialog-(?:overlay|content|header|body|footer)/, '设置样式不得保留重复弹窗系统');
assert.match(globalCss, /body::before[\s\S]*?background-image: var\(--magi-wallpaper-image\)/, '页面根背景必须复用权威壁纸变量');
assert.match(workbenchShell, /workbench-app-pane" data-testid="workbench-app-pane"[\s\S]*?\.workbench-app-pane \{[\s\S]*?border-radius: var\(--radius-lg\)[\s\S]*?overflow: hidden/, '主对话容器必须具备统一圆角和裁切边界');
assert.match(bridge, /eventType === 'appearance\.changed'[\s\S]*?magi:appearanceChanged/, 'daemon 外观事件必须同步到所有窗口');

await assert.rejects(
  access(new URL('../src/web/theme.ts', import.meta.url)),
  '旧 localStorage 主题运行时必须删除，不能保留双实现',
);

console.log('appearance golden checks passed');
