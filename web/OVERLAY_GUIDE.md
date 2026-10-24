# Magi 浮层分类与内容页交互规范

按用户最终约束：Magi 的长表单和长详情采用「代理 Tab → 新增代理」的内容区切换方式，保留左侧导航；不用侧边抽屉。以下六类是沟通分类，不要求每类都引入一个组件。

## 清单与选型

| 类型 | 用途 | 当前项目入口 / 规则 |
| --- | --- | --- |
| Tooltip | 悬停/聚焦辅助说明 | 侧栏与浏览器工具提示、`title`；不能放唯一可见的关键事实 |
| Toast | 短时反馈 | `ToastContainer` / `addToast`；默认 5 秒自熄，可带撤销；复制对勾 2.5 秒清除 |
| Alert | 常驻重要提醒 | `.settings-notice[role=alert]`、浏览器错误条、恢复和隔离提示；由真实状态解除 |
| Popconfirm | 按钮旁轻确认 | `Popconfirm` / `confirmMessage(..., { anchor })`；知识条目、图谱关联、单条审计/异常删除 |
| Modal | 居中强确认 | `Modal` / `ConfirmDialog`；批量清理、会话删除、Git/文件还原、令牌失效和网络授权 |
| Drawer | 通用术语：侧边长内容 | **Magi 设置不用此方案**。长表单、长详情改为 `SettingsDetailPage` 内容区切换 |
| 内容页 | 长表单与连续管理 | MCP 新增/编辑、技能仓库、Skill 安装列表、插件来源/指南、外观编辑；代理保持原有页内编辑 |
| Popover / Menu | 非阻塞详情和选择菜单 | 模型、引擎、Git 分支、通知中心、上下文用量、运行态、错误详情、远程访问、浏览器工具菜单；不叫 Modal |
| Lightbox / Picker | 图片预览、文件选择 | 只读预览可点背景关闭；文件/目录选择保留既有选择器 |

`Modal` 标准宽度为 400/520/720px（xl 为 800px），小屏受 90vw 约束。需要彻底更换主流程时进入新页面；本次无支付、结算等流程。

## 全项目入口清点

| 源码 / 功能 | 最终选型和处理 |
| --- | --- |
| `SettingsPanel` MCP、仓库、Skill 库 | 在工具内容区替换列表；列表保活以恢复触发按钮焦点；原 API 和 store 继续拥有保存逻辑 |
| `SettingsAgentsTab` / `SettingsAppearanceTab` | 长表单在内容区切换；自动草稿；主题退出时恢复 daemon 的活动主题 |
| `SettingsPluginsTab` / `PluginGuidePage` | 来源和指南在插件内容区切换；安装候选和权限对照继续页内展示；指南加载到正文不重建页面，返回恢复入口焦点 |
| `PluginSettingsForm` | 原地编辑，按插件和 scope 保存内存草稿；daemon version 变化使旧草稿失效 |
| `KnowledgePanel` / `KnowledgeGraphPanel` | 原地编辑有草稿；单项删除 Popconfirm，清空知识 Modal；没有恢复 API，不伪造撤销 |
| `InputArea` | 编辑既有批注使用短表单 Modal，禁止蒙层关闭，草稿按 annotationId 保留；附件引用移除 Toast + 撤销 |
| `BrowserTabContent` | 空间批注编辑器保持原来的 Top Layer Popover，重新选择同一导航/同一位置时恢复评论草稿；不保存物理选择身份 |
| `ConfirmDialog` | 所有调用方指定动作化确认文字；按有无 anchor 选择 Popconfirm / Modal |
| `SettingsMcpServerSection` / audit / safeguard | 网络暴露、降级保护、令牌吊销、清空等保留明确安全确认；动作按钮说明真实影响 |
| `NotificationCenter` / `SafeguardAuditPanel` | 单条永久删除 Popconfirm，批量永久清理 Modal；沿用 daemon 删除入口 |
| `IsolationExitDialog` / `IsolationMergeDialog` | 丢弃/合并事务保留 Modal；合并选择属于表单，禁用蒙层关闭；不自动提交 |
| `WebWorkbenchShell` | 添加工作区为 Picker，删除会话/移除工作区为 Modal；移除工作区会影响 MCP 注册，保留确认 |
| `ActiveAgentCenter` | 代理详情为原有面板，重启确认 Modal；不改变运行状态所有权 |
| `MessageItem` / `ImageAttachmentTray` | 图片只读 Lightbox，允许关闭；无待保存表单 |
| Electron Main | 启动失败用原生错误提示 + “重试/打开日志位置/退出”；保存文件使用 OS Picker；guest 的 JS dialog 保留受控 bridge，不改网页语义 |
| `examples/plugins/board/ui/index.html` | 示例隔离 iframe，无浮层需求；继续经宿主消息桥读写 |

`GoalRunDrawers` 实际是输入框上方的目标/计划折叠卡，不是侧边 Drawer；暂保留内部代码名避免无关重命名。移动端导航侧栏仍按原布局收放，不改为编辑抽屉。

## 交互规则

1. 长表单/长详情优先内容区切换。短表单如必须使用 Modal，应设置 `form`，组件会硬禁用蒙层点击。按钮和 Esc 返回时保留草稿；提交成功后清理，失败继续编辑。
2. `view-drafts.ts` 只保存未提交 UI 草稿，不能自动写 daemon。知识、角色、外观和既有批注草稿放在当前标签页的 sessionStorage，关闭标签页后清除；源内容或 revision 改变后丢弃旧基线草稿。MCP、插件来源/设置、空间批注仅在 Renderer 内存保留，刷新/退出清除，避免额外存储凭据。仓库 URL 由保活的 settings store 保留。
3. Popconfirm 的取消/点外部/Esc 都不执行操作，滚动或尺寸变化时取消；关闭返回触发按钮。真正不可恢复、有外部副作用或扩大权限的操作继续使用 Modal。不能用“重新创建一个相似对象”冒充撤销。
4. 可恢复附件移除直接执行 + Toast 撤销。恢复前检查同一工作区/会话、输入框未提交清空和容量限制；不恢复到别的会话。
5. 所有短确认必须提供明确动词，不使用“是/否”或通用“确定”。`confirmMessage` 的类型要求 `confirmLabel`。
6. `overlay-stack` 把 Esc / Tab 交给最上层 Modal / Popconfirm；内容页返回键在浮层关闭后才生效，避免一次 Esc 连关两层。保存期间仍消费 Esc，但不关闭。Modal 使用独立标题 ID，焦点陷阱排除隐藏/禁用控件，卸载恢复焦点。
7. 数字、正文、图标、焦点和减少动态效果继续遵循 DESIGN_SYSTEM；本轮不改变会话、权限、API 或持久化协议。

## 提示词模板

```text
检查 Magi 的「{页面/操作}」：
- 长表单/详情：按新增代理模式在现有内容区切换，保留左侧导航，使用 SettingsDetailPage；不用 Drawer。
- 短不可恢复确认或权限影响：Modal，标题和按钮写明动作与对象，正文说明范围与后果。
- 高频单项删除：按钮旁 Popconfirm；能可靠恢复的操作直接执行 + Toast 撤销。
- 表单如必须使用 Modal，设置 form 禁用蒙层关闭；返回保留草稿，成功提交后清理。
- 草稿按身份与服务端基线隔离；凭据只保留内存草稿，不额外写入浏览器持久存储。
输出组件清单、关闭/返回路径、焦点恢复点、i18n 改动、回归证据与未验证范围。
```

## 三个可直接修改的实际页面示例

1. **MCP 新增/编辑**：`SettingsPanel.svelte` 中工具分类的 `SettingsDetailPage`。`showMCPDialogState` 只控制内容视图；`openMCPDialog` 恢复草稿，`saveMCPServer` 沿原 API 保存。返回按钮和 Esc 调用 `closeMCPDialog`，保留草稿。无需新增路由或业务 store。

```svelte
<SettingsDetailPage title={i18n.t('settings.mcp.addTitle')}
  busy={store.saveStatus.mcp === 'saving'} onClose={store.closeMCPDialog}>
  <!-- 复用现有 MCP 表单 / JSON 编辑器 -->
  {#snippet footer()}
    <button class="btn btn--primary btn--sm" onclick={store.saveMCPServer}>
      {i18n.t('settings.mcp.save')}
    </button>
  {/snippet}
</SettingsDetailPage>
```

2. **知识条目删除**：`KnowledgePanel.svelte` 中 `deleteKnowledgeEntry`，先记录当前 workspace，再 await 按钮旁确认，答复后核对 scope，最后调用原删除 API。

```ts
const confirmed = await confirmMessage(i18n.t('overlay.deleteKnowledgeBody'), {
  title: i18n.t('overlay.deleteKnowledge'),
  confirmLabel: i18n.t('overlay.deleteKnowledge'), tone: 'danger', anchor,
});
if (!confirmed || scope !== currentWorkspaceKey) return;
// 继续既有 deleteAgentKnowledgeItem 入口。
```

3. **可恢复附件移除**：`InputArea.svelte` 的 `removeImage` / `removeContextReference` / `removeAnnotationReference`。Toast 的 `action` 只存在于 Renderer，点击后只运行一次；其恢复函数核对 scope / epoch / 容量，不改变已提交消息。

```ts
addToast('info', i18n.t('overlay.imageRemoved'), undefined, {
  forceVisible: true,
  action: { label: i18n.t('overlay.undo'), run: restoreImageIfSameDraft },
});
```
