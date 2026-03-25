# 流式输出与时间轴稳定化收尾清单

## 背景

本清单直接对应产品目标：

1. 所有流式输出在 UI 层自然流畅。
2. 工具卡片、Shell 卡片、Worker 任务卡首次渲染即固定位置，不因后续内容漂移。
3. 主线与 Worker 输出严格分线，不能混线。
4. 页面刷新、会话切换不丢失原内容。
5. 链路唯一稳定，禁止多套实现并存。
6. 禁止兼容性处理，必要时允许全量重构。
7. 全程遵循 `$cn-engineering-standard`。

## 当前结论

### 已具备的基础

- 后端加载链已收口为：`session.json.timeline + notifications -> hydrate -> projection 重建 -> sessionBootstrapLoaded/timelineProjectionUpdated`。
- 前端最终面板渲染已改为优先消费 `threadRenderEntries / workerRenderEntries`。
- `MessageList` 已按稳定 `item.key` 渲染，卡片首次落位的 key 基础已经具备。
- 主线与 Worker 面板已通过 `buildTimelinePanelView(...)` 分开消费 projection。
- 前端已落地 `sessionViewStateBySession`，每个会话独立缓存自己的 `timelineProjection + scrollPositions + scrollAnchors + autoScrollEnabled`。
- `timeline-events.jsonl / timeline-projection.json / session-timeline-recovery` 已从运行时主链移除。

### 仍未达到终态的关键差距

- 产品层面的最终验收仍应以真实交互观察为准，不能只看脚本。

## 收尾原则

- 只保留一条权威渲染链：`timelineProjection.artifacts + renderEntries`。
- 前端不再承担语义排序、可见性推断、lane 恢复职责。
- 历史兼容逻辑不继续追加；能删除的迁移、自修复、兜底分支必须删除。
- 所有测试、日志、类型、脚本一起同步，不保留“双协议时代”的残留字段。

## 收尾任务

### P0. Projection 单链路与恢复首屏稳定

- [x] 前端面板渲染只消费 `threadRenderEntries / workerRenderEntries`
- [x] `messages store` 不再消费旧 `projection.threadView / workerViews`
- [x] projection schema 切到 `session-timeline-projection.v2`
- [x] 当前会话 projection 快照持久化到 `WebviewPersistedState`
- [x] 已访问会话的 projection/scroll 现场持久化到 `sessionViewStateBySession`
- [x] 初始化时优先恢复当前会话 projection，消除刷新首屏空窗
- [x] 初始化时可恢复已访问会话的本地视图现场，避免来回切会话后滚动状态丢失
- [x] 初始化时恢复 `scrollPositions / scrollAnchors / autoScrollEnabled`
- [x] 静态守卫禁止重新消费旧 projection 视图字段

### P0. 前端写入口继续收口

- [x] `message-handler` 不再直接接管 live timeline 写入
- [x] store 不再暴露旧的 thread/agent timeline 增删改导出
- [x] placeholder 仅保留窄接口补丁，不再作为通用 timeline 写入口
- [x] 已核对 `Header`、`TasksPanel`、`ThreadPanel` 等组件，只消费 projection 派生结果或会话状态，不再自行接管旧时间轴落位

### P0. 兼容恢复链路清理

- [x] 移除 `migrateMessageRoles(...)`
- [x] 移除 dedicated `legacy system_section self-heal` 校验背书
- [x] 移除 dedicated `legacy placeholder metadata self-heal` 校验背书
- [x] 移除 `recoverTimelineProjectionState(...)`，加载只认严格 schema 的 `session.json`
- [x] 清理 legacy/self-heal verify 场景，验证脚本不再为旧数据兼容背书
- [x] 从运行时与提交态一起移除 `session-timeline-recovery`，避免旧恢复文件在后续提交中复活

### P1. 产品稳定性验收

- [x] 主线卡片顺序由后端 `displayOrder + renderEntries` 锁定，前端不再二次排序
- [x] Worker 生命周期卡、Shell 卡、工具卡通过稳定 `entryId/key` 首帧落位，不再依赖后续消息重排
- [x] Worker 输出不会出现在主线面板
- [x] 主线总结不会泄漏到 Worker 面板
- [x] 页面刷新后当前会话原内容可先从本地 projection 恢复，再被后端 bootstrap 权威覆盖
- [x] 会话切换后已访问会话内容与滚动状态可恢复，底部 tab 强制回到 `thread` 防止主线/Worker 误混线

### P1. 验收链收口

- [x] 仓库内历史 `verify` / `e2e` 脚本入口已清理，避免形成第二套维护链。
- [ ] Chrome 真实调试 Web 入口：`http://127.0.0.1:46231/web.html`
- [ ] VS Code 扩展宿主真实验证：`code --extensionDevelopmentPath /Users/xie/code/magi`

## 七条目标逐项映射

1. `UI 自然流畅`

- 流式落位只更新已有 artifact / execution item，不再让前端重排整个列表。
- 自动滚动、滚动锚点、面板激活恢复都按 panel 维度独立维护。

2. `工具节点卡片首渲染固定位置`

- 工具卡、Shell 卡、Worker 任务卡统一受 projection `entryId` 与 artifact `displayOrder` 约束。
- `MessageList` 使用 `item.key` 渲染，不再以消息数组下标驱动节点身份。

3. `主线 / Worker 严格分线`

- 主线只消费 `threadRenderEntries`。
- Worker 面板只消费 `workerRenderEntries[worker]`。
- 会话切换时底部 tab 重置为 `thread`，防止跨会话沿用旧 Worker 面板造成认知混线。

4. `刷新 / 会话切换不丢内容`

- 当前会话刷新：`currentTimelineProjection + scroll state` 立即恢复。
- 已访问会话切换：`sessionViewStateBySession` 恢复本地视图现场，随后由 `sessionBootstrapLoaded` 权威覆盖。

5. `链路唯一稳定`

- 后端加载唯一真相源：`session.json`。
- 前端面板唯一渲染真相源：`timelineProjection.artifacts + renderEntries`。
- `sessionBootstrapLoaded / timelineProjectionUpdated` 仍是唯一可见时间轴覆盖入口。
- 已删除辅助恢复文件与辅助持久化产物，不再存在第二套恢复真相源。

6. `禁止兼容性处理`

- 旧 projection 视图字段消费、role 迁移、自修复校验、旧恢复合并路径均已删掉。

7. `遵循工程规范`

- 全程按 `$cn-engineering-standard` 收口：先找表象与机理，再删兼容、收唯一链、补边界守卫。

## 文件清单

### 已改

- `streaming-render-stabilization-checklist.md`
- `src/session/session-timeline-projection.ts`
- `src/session/unified-session-manager.ts`
- `src/ui/webview-svelte/src/lib/data-message-handlers.ts`
- `src/ui/webview-svelte/src/stores/messages.svelte.ts`
- `src/ui/webview-svelte/src/types/message.ts`
- `src/ui/event-binding-service.ts`

### 下一步重点

- 真实 UI 交互回归：长流式、工具串流、Worker 并行、刷新恢复、切会话往返
- 插件端与 Web 端真实端到端验证
- 用真实构建产物与人工链路验收最终交互表现，避免再次引入临时校验脚本

## 验收命令

- `npm run compile`
- `npm run build:extension`
- `npm run build:webview`
- `npm run build:agent`
- `npm run build:web`
- `MAGI_AGENT_WORKSPACES='[{"rootPath":"/Users/xie/code/magi","name":"magi"}]' npm run dev:agent`

## 真实验收步骤

### Web

1. 先执行 `npm run compile`、`npm run build:agent`、`npm run build:web`，确认当前真实产物可启动。
2. 启动 Local Agent，并确保 `/health` 返回 200。
3. 用 Chrome 打开 `http://127.0.0.1:46231/web.html?workspacePath=/Users/xie/code/magi`。
4. 在真实会话中验证主线卡片、工具卡、Shell 卡、Worker 卡首帧落位稳定，不发生漂移。
5. 验证主线与 Worker 面板严格分线。
6. 刷新页面后，当前会话内容先由本地 projection 恢复，再被 bootstrap 权威覆盖。
7. 在至少两个已有会话之间往返切换，验证内容与滚动位置恢复。

### 插件

1. 运行 `npm run build:extension && npm run build:webview && npm run build:agent && npm run build:web`。
2. 运行 `code --extensionDevelopmentPath /Users/xie/code/magi /Users/xie/code/magi` 打开扩展开发宿主。
3. 在扩展宿主中打开 `Magi` 视图或执行 `Magi: 打开 Web 客户端`。
4. 验证插件面板与 Web 入口对同一会话链路的恢复、切换与卡片渲染行为一致。

## 结束标准

满足以下条件才视为收尾完成：

- Projection 只保留一条恢复与渲染链。
- 前端无任何旧时间轴写入口、旧视图字段消费、旧排序回退。
- 刷新与会话切换可稳定恢复，不再依赖空白等待。
- legacy / self-heal / 兼容迁移路径已清理到可接受的单一机制。
- 历史 `verify` / `e2e` 脚本与引用已从仓库主链移除，Chrome/扩展宿主验收通过。
