# Magi Design System v2.0 — 标准样式模板

> 本文档是 Magi Web 端的 UI/UX 设计规范，所有前端组件、页面、弹窗均应遵循此模板。
> 设计语言定位：**Apple HIG 风格** — 克制、精致、轻量、层次分明。

---

## 1. 设计原则

| 原则 | 要求 |
|---|---|
| 克制用色 | 前景色 `#1d1d1f`（非纯黑），次要文字 `#86868b`，避免大面积饱和色 |
| 层次透明 | 背景使用 `rgba` 半透明叠加，而非纯色块，制造深度感 |
| 圆角统一 | 面板/卡片 `12px`，控件 `8px`，按钮 `4-6px`，胶囊 `9999px` |
| 阴影轻柔 | 仅用于卡片和弹窗，禁止浓重投影 |
| 控件一致 | 统一 `28px` 控件高度，标签在上、控件在下 |
| 留白充分 | 基于 4px 网格，区块间距 `24-32px`，组件内间距 `8-12px` |

---

## 2. 颜色系统

### 2.1 基础色板

```
/* 来源: global.css :root */
--background       深色 #11161d / 浅色 #ffffff
--foreground        深色 #e5e7eb / 浅色 #1f2937
--foreground-muted  深色 #98a2b3 / 浅色 #667085
--border            深色 #273142 / 浅色 #d7dce5
```

### 2.2 交互色

```
--primary           #2563eb（蓝）
--primary-hover     #1d4ed8
--primary-muted     rgba(14, 99, 156, 0.15)
--secondary         深色 #1f2937 / 浅色 #eef2f7
```

### 2.3 状态色

```
--success   #10b981   --success-muted  rgba(16, 185, 129, 0.15)
--warning   #f59e0b   --warning-muted  rgba(245, 158, 11, 0.15)
--error     #ef4444   --error-muted    rgba(239, 68, 68, 0.15)
--info      #3b82f6   --info-muted     rgba(59, 130, 246, 0.15)
```

### 2.4 表面层级（深色 → 白色透明度递增 / 浅色 → 黑色透明度递增）

```
--surface-0       transparent
--surface-1       深色 rgba(255,255,255,0.02) / 浅色 rgba(0,0,0,0.02)
--surface-2       深色 rgba(255,255,255,0.04) / 浅色 rgba(0,0,0,0.04)
--surface-3       深色 rgba(255,255,255,0.06) / 浅色 rgba(0,0,0,0.06)
--surface-hover   深色 rgba(255,255,255,0.08) / 浅色 rgba(0,0,0,0.06)
--surface-active  深色 rgba(255,255,255,0.12) / 浅色 rgba(0,0,0,0.10)
```

### 2.5 设置面板专用色（角色卡片等）

```
浅色:
  卡片背景        rgba(255, 255, 255, 0.92)
  卡片悬停        #ffffff
  卡片边框        rgba(60, 60, 67, 0.16)
  控件背景        rgba(0, 0, 0, 0.03)
  前景            #1d1d1f
  次要文字        #86868b
  柔和文字        #aeaeb2

深色:
  卡片背景        rgba(255, 255, 255, 0.04)
  卡片悬停        rgba(255, 255, 255, 0.07)
  卡片边框        rgba(255, 255, 255, 0.14)
  控件背景        rgba(255, 255, 255, 0.05)
```

---

## 3. 排版系统

### 3.1 字体栈

```
--font-family  -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif
               中文回退: 'PingFang SC'（在组件内按需声明）
--font-mono    'SFMono-Regular', 'Cascadia Code', Consolas, monospace
```

### 3.2 字号阶梯

| 变量 | 值 | 用途 |
|---|---|---|
| `--text-2xs` | 10px | 时间戳 |
| `--text-xs` | 11px | 徽章、辅助提示 |
| `--text-sm` | 12px | 描述文字、字段标签 |
| `--text-base` | 13px | 正文（默认） |
| `--text-md` | 14px | 列表标题 |
| `--text-lg` | 16px | 区块标题 |
| `--text-xl` | 18px | 面板标题 |

### 3.3 字重

```
--font-normal    400    正文
--font-medium    500    标签、按钮
--font-semibold  600    标题、强调
--font-bold      700    大标题
```

### 3.4 行高

```
--leading-tight    1.25    标题
--leading-normal   1.5     正文
--leading-relaxed  1.75    段落
```

---

## 4. 间距系统（4px 基础网格）

| 变量 | 值 | 语义别名 | 用途 |
|---|---|---|---|
| `--space-1` | 2px | — | 文字与图标间距 |
| `--space-2` | 4px | `--spacing-xs` | 紧凑间距 |
| `--space-3` | 8px | `--spacing-sm` | 组件内部间距 |
| `--space-4` | 12px | `--spacing-md` | 组件边距 |
| `--space-5` | 16px | `--spacing-lg` | 区域边距 |
| `--space-6` | 24px | `--spacing-xl` | 区块间距 |
| `--space-8` | 32px | — | 版块间距 |

---

## 5. 圆角系统

| 变量 | 值 | 用途 |
|---|---|---|
| `--radius-xs` | 2px | 微小元素 |
| `--radius-sm` | 4px | 按钮、输入框 |
| `--radius-md` | 6px | 卡片、下拉框 |
| `--radius-lg` | 8px | 大卡片、控件容器 |
| `--radius-xl` | 12px | 模态框、面板 |
| `--radius-2xl` | 16px | 特大卡片 |
| `--radius-full` | 9999px | 圆形、胶囊按钮 |

---

## 6. 阴影系统

```
--shadow-sm   0 1px 2px rgba(0, 0, 0, 0.2)        日常卡片
--shadow-md   0 4px 8px rgba(0, 0, 0, 0.3)        悬浮层
--shadow-lg   0 8px 24px rgba(0, 0, 0, 0.4)       弹窗
--shadow-xl   0 16px 48px rgba(0, 0, 0, 0.5)      模态框

/* 设置面板卡片专用（更轻柔） */
卡片阴影: 0 1px 2px rgba(0,0,0,0.04), 0 6px 18px rgba(0,0,0,0.05)
```

---

## 7. 动画系统

| 变量 | 值 | 用途 |
|---|---|---|
| `--duration-instant` | 50ms | 微反馈 |
| `--duration-fast` | 100ms | hover/active |
| `--duration-normal` | 200ms | 常规过渡 |
| `--duration-slow` | 300ms | 展开/折叠 |
| `--duration-slower` | 500ms | 进场动画 |

```
--ease-default   cubic-bezier(0.4, 0, 0.2, 1)    通用
--ease-out       cubic-bezier(0, 0, 0.2, 1)       退出
弹窗进场: 0.2s cubic-bezier(0.16, 1, 0.3, 1)     弹性
```

---

## 8. 组件尺寸

### 8.1 按钮高度

| 变量 | 值 | 用途 |
|---|---|---|
| `--btn-height-xs` | 20px | 极小操作 |
| `--btn-height-sm` | 24px | 紧凑按钮 |
| `--btn-height-md` | 28px | 默认按钮 / 输入框 |
| `--btn-height-lg` | 32px | 表单输入框 |
| `--btn-height-xl` | 40px | 主操作按钮 |

### 8.2 图标尺寸

```
--icon-xs   12px    行内图标
--icon-sm   14px    按钮内图标
--icon-md   16px    默认图标
--icon-lg   20px    列表图标
--icon-xl   24px    标题图标
--icon-2xl  32px    空状态图标
```

### 8.3 头像

```
--avatar-sm  24px    列表头像
--avatar-md  32px    卡片头像
--avatar-lg  40px    详情头像
```

---

## 9. Z-Index 层级

```
--z-base      0       基础内容
--z-dropdown  100     下拉菜单
--z-sticky    200     固定头部
--z-modal     1000    模态框
--z-popover   1100    弹出层
--z-tooltip   1200    提示
--z-toast     1300    通知
```

---

## 10. 组件标准

### 10.1 表单字段（标准结构）

```html
<div class="llm-config-field">          <!-- 或 form-group -->
  <label class="llm-config-label">标签</label>   <!-- 上方标签 -->
  <input class="llm-config-input" />              <!-- 下方控件 -->
</div>
```

规则：
- 标签在上，控件在下，**禁止横排**
- 标签 `font-size: var(--text-sm)`, `color: var(--foreground-muted)`, `white-space: nowrap`
- 控件统一 `28px` 高度
- Toggle 开关需外包 `height: 28px; display: flex; align-items: center;` 与其他控件对齐

### 10.2 表单行布局（Grid）

```css
/* 模型 Tab 示例 */
.field-row                        grid: 1fr 100px
.field-row.has-thinking           grid: 1fr 100px 100px
.field-row.has-thinking.has-level grid: 1fr 100px 100px 100px 100px
```

- 第一列（模型名）`1fr` 弹性
- 其余列统一 `100px`
- `≤768px` 时全部折叠为单列

### 10.3 分段控制（Segmented Control）

用于二选一切换（如：标准路径 / 完整路径）：
- 按钮组并排，选中项高亮
- 高度 `28px`，圆角 `var(--radius-sm)`

### 10.4 卡片

```css
.card {
  background: var(--ind-bg-card);          /* 半透明 */
  border: 1px solid var(--ind-border-card);
  border-radius: 12px;                     /* --radius-xl */
  padding: 14px 18px 18px 16px;
  box-shadow: 0 1px 2px rgba(0,0,0,0.04), 0 6px 18px rgba(0,0,0,0.05);
  /* 禁止 overflow: hidden，防止子元素被裁剪 */
  /* 禁止 min-height 固定值，高度由内容自适应 */
}
```

卡片三段式结构：
1. **头部**：图标 + 名称 + 操作（如 Toggle）
2. **主体**：描述文字
3. **底部**：状态指示 + 控件（如下拉选择）

### 10.5 卡片网格

```css
.card-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
  gap: 12px;
}
```

- 最小列宽 `340px`，确保内部控件有足够空间
- 配合 `@container` 查询（非 `@media`）在容器 ≤480px 时切单列

### 10.6 按钮

| 类型 | 类名 | 样式 |
|---|---|---|
| 主操作 | `.btn--primary` | 蓝底白字 |
| 次要 | `.btn--secondary` | 灰底 |
| 幽灵 | `.btn--ghost` | 透明底 |
| 危险 | `.btn--error` | 红色边框，hover 红底 |
| 图标 | `.btn-icon` | 方形透明，hover 灰底 |

### 10.7 模态框

```
尺寸: sm 400px / md 560px / default 480px
圆角: 12px (--radius-xl)
阴影: --shadow-xl
进场: slideUp 200ms
移动端(≤768px): 全屏，无圆角
```

### 10.8 状态指示

```css
.status-dot { width: 6px; height: 6px; border-radius: 9999px; }
/* 角色卡片内更小: 4.5px + glow 阴影 */
```

---

## 11. 设置面板布局

```
┌──────────────────────────────────────────┐
│ ┌──────────┐ ┌─────────────────────────┐ │
│ │ 侧边栏    │ │  内容区                  │ │
│ │ 140px     │ │  padding: 20px          │ │
│ │           │ │                         │ │
│ │ 统计      │ │  标题 + 描述             │ │
│ │ 模型      │ │  语言切换 + 关闭按钮      │ │
│ │ 角色      │ │                         │ │
│ │ 工具      │ │  Tab 内容               │ │
│ │ 规则      │ │                         │ │
│ └──────────┘ └─────────────────────────┘ │
└──────────────────────────────────────────┘

≤768px 时:
  侧边栏 → 64px 仅图标
  面板 → 全屏
  内容区 padding → 12px
```

---

## 12. 响应式策略

| 层级 | 机制 | 断点/条件 |
|---|---|---|
| 视口级 | `@media` | `≤768px` 移动端全屏 |
| 容器级 | `@container` | 按实际容器宽度触发（如 ≤480px 切单列） |
| 组件级 | `flex-wrap` + `min-width` | 极端挤压自动折行 |

**关键规则**：
- 嵌入式面板内部的响应式必须用 `@container`，不能用 `@media`
- 外层容器需声明 `container-type: inline-size`
- 禁止 `overflow: hidden` + 固定 `min-height` 同时存在

---

## 13. 主题适配

- 浅色/深色通过 CSS 变量自动切换
- 主题选择器：`body.vscode-light` / `body.theme-light` / `:root.theme-light`（浅色）
- 深色同理：`body.vscode-dark` / `body.theme-dark` / `:root.theme-dark`
- `color-scheme: dark | light` 与系统偏好联动
- 所有颜色必须引用变量，禁止硬编码 `#000` / `#fff`

---

## 14. 禁止事项

| 禁止 | 原因 |
|---|---|
| `overflow: hidden` + 固定 `min-height` | 内容被裁剪不可见 |
| 嵌入面板内使用 `@media` 做响应式 | 视口宽度 ≠ 容器宽度 |
| 硬编码颜色值 | 无法跟随主题切换 |
| 表单字段标签与控件横排 | 违反统一的上下结构 |
| 纯黑 `#000000` 作为文字色 | 对比度过强，不符合设计语言 |
| 浓重阴影 | 破坏轻量感 |
| 固定像素宽度的弹性内容 | 无法适配不同屏幕 |
