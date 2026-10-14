# Magi Web 视觉与组件规范

本文说明现行视觉 token 的来源和组件使用方式。区域规则见 [web/AGENTS.md](AGENTS.md)；设置页结构和样式以 [settings.css](src/styles/settings.css) 为准，不再维护另一套设置页模板。

## 视觉与主题来源

- [global.css](src/styles/global.css) 定义颜色、字体、间距、圆角、控件尺寸、动画与层级 token，以及共享按钮和输入框。
- [appearance/runtime.ts](src/appearance/runtime.ts) 根据外观配置投影主题变量；组件消费变量，不自行持久化或推导另一套主题。
- [settings.css](src/styles/settings.css) 定义所有设置分类共享的框架、分区、列表、卡片和二级标签。
- 文案使用现有 i18n；重复交互优先复用 Icon、Toggle 和对话框等现有组件。

颜色、材质和尺寸的精确值以这些源码为准。`--vscode-*` 是仍被使用的主题适配变量，不能因为名字来自旧宿主就删除。

## 通用 token

| 用途 | 使用方式 |
| --- | --- |
| 前景、背景与边框 | `--foreground`、`--foreground-muted`、`--background`、`--border` |
| 交互与状态 | `--primary`、`--primary-muted`、`--success`、`--warning`、`--error` |
| 表面层级 | `--surface-*`、`--magi-surface-*`；由主题投影选择材质 |
| 字体与排版 | `--font-family`、`--font-mono`、`--text-*`、`--font-*`、`--leading-*` |
| 间距与圆角 | `--space-*`、`--spacing-*`、`--radius-*`；区域共享类有明确值时沿用该类 |
| 控件尺寸 | `--btn-height-*`、`--icon-*`、`--avatar-*`，使用现有组件的尺寸参数 |
| 动画与层级 | `--duration-*`、`--ease-*`、`--transition-*`、`--z-*` |

不要把某次浅色/深色主题的色值复制为组件默认值。设置内容面板不添加背景、渐变或阴影；浮层按其已有组件的层级和材质实现。

## 设置页结构

设置分类共用同一套页面框架与控件，新增分类按下列结构组合：

| 层级 | 现行共享结构 |
| --- | --- |
| 页面 | `.apple-scroller-proxy` 或 `.settings-frame`，居中内容列、最大宽度 880px |
| 分区 | `.settings-section`、`.settings-section-header`、标题、描述与操作区；分区之间一条细分隔线，每区最多一个主按钮 |
| 内容 | `.settings-card`，或 `.settings-rows` / `.settings-row`；只使用 1px 边框与 10px 圆角 |
| 行文案 | `.settings-row-copy`、`.settings-row-title`、`.settings-row-desc` |
| 辅助状态 | `.settings-chip`、`.settings-dot`、`.settings-notice`、`.settings-empty` |
| 二级标签 | `.settings-subtabs` / `.settings-subtab`，下划线样式 |
| 按钮 | `.btn` + `.btn--sm`；主操作 `.btn--primary`，次操作 `.btn--secondary`，破坏性操作 `.btn--error` |
| 输入与开关 | `.form-input` 与 Toggle；沿现行表单或行布局，不统一强制标签和控件只能上下排列 |

插件分类使用左右主从面板：左侧按状态分组、可独立滚动的列表，右侧在同页展示所选项详情与操作，默认选择第一项；安装/更新等首要操作使用 `.btn--lg`。不通过额外弹窗展示插件详情。

以下是分区结构示例，具体内容仍由现有组件负责：

```html
<section class="settings-section">
  <header class="settings-section-header">
    <div>
      <h2 class="settings-section-title">分区标题</h2>
      <p class="settings-section-desc">分区说明</p>
    </div>
    <div class="settings-section-actions">操作按钮</div>
  </header>
  <div class="settings-card">内容</div>
</section>
```

## 布局、响应式与交互

- 工作台壳、右栏和设置页分别由自己的既有布局 owner 管理，不在内容组件复制外层几何或留白。
- 嵌入面板按容器宽度适配，优先沿用已有 `@container`；工作台外层可按视口使用 `@media`。
- 弹性内容设置必要的 `min-width: 0` / `min-height: 0`；避免固定高度与裁剪共同隐藏用户内容。
- 列表、表单与卡片按现行模式折行；不能把历史示例里的固定列宽、控件高度或断点当作所有页面的合同。
- 浅色/深色均使用主题变量，焦点、禁用、错误和加载状态保持可辨认；背景只在适用的控件和选中项上使用。
- 使用现有模态框、提示和 Top Layer 组件，避免在内容组件发明另一套 z-index 或全屏覆盖机制。

## 验证

修改视觉组件后运行 `npm --prefix web run check`；交互变化运行对应 `test:*`。实际布局、主题和响应式需要在 daemon 的 `http://127.0.0.1:38123/web.html` 或相应 Desktop 入口验证，源码检查不能代替视觉验收。
