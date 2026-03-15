# Magi Web 多宿主 UI 架构方案

> 定稿目标：
> - 保持 Magi 的产品定位不变
> - 支持“一套 UI，同时服务 VS Code 插件与 Web”
> - 不把 Web 版本降级成纯聊天页
> - 不复制两套前端

---

## 1. 结论

Magi 可以做 Web 形态，而且**应该优先走“一套 UI，多宿主”路线**，而不是：

- 把 VS Code Webview 原样搬到浏览器
- 或者重新做一套独立 Web 前端

最合理的目标架构是：

> **One UI, Multiple Hosts**

即：

1. 一套共享的 Svelte UI、状态模型、消息协议
2. 一个 VS Code 宿主适配层
3. 一个 Web 宿主适配层
4. 后端执行侧采用“Core Runtime + Host Capabilities”模式

---

## 2. 为什么不能直接把现在的插件 UI 当成 Web UI

当前项目的前端主体已经有较高复用价值，主要集中在：

- `src/ui/webview-svelte/src/components/*`
- `src/ui/webview-svelte/src/stores/*`
- `src/ui/webview-svelte/src/types/*`
- `src/ui/webview-svelte/src/lib/*` 中的纯业务逻辑

但它还**没有完全脱离 VS Code 宿主假设**。

### 2.1 当前直接耦合点

#### 启动入口默认是 Webview 宿主

- [main.ts](/Users/xie/code/magi/src/ui/webview-svelte/src/main.ts)

当前启动流程会在挂载后直接执行：

- `vscode.postMessage({ type: 'webviewReady' })`

这说明前端默认自己运行在 VS Code Webview 中。

#### 通信桥接层仍然是 VS Code 专用命名与职责

- [vscode-bridge.ts](/Users/xie/code/magi/src/ui/webview-svelte/src/lib/vscode-bridge.ts)

它当前承载了：

- `postMessage`
- `onMessage`
- `getState`
- `setState`
- 初始 session / locale 注入

这些能力本身是可复用的，但实现和命名都默认绑定 VS Code。

#### 宿主总控制器仍是 VS Code WebviewProvider

- [webview-provider.ts](/Users/xie/code/magi/src/ui/webview-provider.ts)

它现在仍然是产品运行期的重要控制枢纽，而不是单纯的宿主壳。

---

## 3. 产品判断：应该共用一套 UI，但不能共用宿主层

正确边界不是：

- 插件 UI 一套
- Web UI 一套

而是：

- **共享 UI 层一套**
- **宿主接入层两套**

### 3.1 应共享的内容

这些应当保持单一事实源：

- 组件树
- 页面布局
- 状态模型
- 消息协议
- Worker / Session / Todo / Task / Runtime 语义
- 大部分交互行为

### 3.2 不应强行共享的内容

这些必须按宿主拆开：

- 宿主消息桥接
- 初始状态注入
- 文件选择/下载/外链打开
- 宿主生命周期
- 本地工程能力入口

---

## 4. 目标架构

建议将前端和运行时统一抽象成下面四层。

### 4.1 Shared UI Layer

职责：

- 纯 UI 组件
- store
- types
- 视图状态计算
- 业务交互编排

建议保留在现有目录下：

- `src/ui/webview-svelte/src/components`
- `src/ui/webview-svelte/src/stores`
- `src/ui/webview-svelte/src/types`
- `src/ui/webview-svelte/src/lib` 中的纯业务部分

这层必须做到：

- 不直接引用 `vscode`
- 不感知宿主是插件还是浏览器

### 4.2 Client Bridge Layer

职责：

- 抹平 VS Code Webview 与浏览器环境差异
- 提供统一的消息通道与宿主能力接口

建议定义统一接口，例如：

```ts /Users/xie/code/magi/src/ui/shared/client-bridge.ts
export interface ClientBridge {
  postMessage(message: Record<string, unknown>): void;
  onMessage(listener: (message: Record<string, unknown>) => void): () => void;
  getState<T>(): T | undefined;
  setState<T>(state: T): void;
  getInitialSessionId(): string;
  getInitialLocale(): string;
  notifyReady(): void;
  openExternal?(url: string): void;
  downloadFile?(name: string, data: string | Blob): void;
}
```

然后提供两套实现：

- `vscode-client-bridge`
- `web-client-bridge`

### 4.3 Boot Layer

职责：

- 挂载 App
- 注入 bridge
- 做宿主相关初始化

建议拆成：

- `main-vscode.ts`
- `main-web.ts`

但它们都挂同一个 `App.svelte`。

### 4.4 Runtime Host Layer

这是比 UI 更关键的一层。

Web 之所以不能只做前端，是因为 Magi 不是纯聊天产品，它需要：

- 文件系统
- shell / terminal
- Git / worktree
- LSP / diagnostics
- workspace session

因此后端应进一步区分宿主能力：

- `vscode-host`
- `local-agent-host`
- 后续可扩展 `cloud-workspace-host`

---

## 5. 推荐产品路线

## 5.1 第一阶段：Web UI + Local Agent

这是当前最合理的路线。

形态：

1. 用户打开浏览器中的 Magi Web
2. 本地运行 `magi-agent`
3. Web UI 连接本地 agent
4. agent 负责：
   - workspace
   - shell
   - git
   - worktree
   - LSP
   - session/runtime

这样做的好处：

- 不要求用户必须打开 VS Code
- 不丢失 Magi 的“可执行开发”属性
- 最大化复用现有编排核心
- 不需要立刻做云工作区平台

## 5.2 第二阶段：Core Runtime 抽离

目标不是重写，而是把“VS Code 专属调用”从核心逻辑中拿出去。

需要抽出的能力包括：

- 文件系统能力
- LSP 能力
- diagnostics 能力
- shell/terminal 能力
- UI 消息通道

## 5.3 第三阶段：Cloud Workspace

这是长期路线，不建议先做。

只有当本地 agent 方案稳定后，再考虑：

- 远端容器
- 多人协作
- 企业托管
- 浏览器即工作区

---

## 6. 当前代码上的拆分建议

## 6.1 前端改造

### A. 抽掉 `vscode-bridge` 的宿主专有命名

当前：

- [vscode-bridge.ts](/Users/xie/code/magi/src/ui/webview-svelte/src/lib/vscode-bridge.ts)

建议改为：

- `src/ui/shared/bridges/client-bridge.ts`
- `src/ui/shared/bridges/vscode-client-bridge.ts`
- `src/ui/shared/bridges/web-client-bridge.ts`

目标：

- 共享 UI 只依赖 `ClientBridge`
- 不直接依赖 `acquireVsCodeApi`

### B. 拆启动入口

当前：

- [main.ts](/Users/xie/code/magi/src/ui/webview-svelte/src/main.ts)

建议拆为：

- `main-vscode.ts`
- `main-web.ts`

其中共享逻辑抽成：

- `bootstrap-app.ts`

### C. 让 `message-handler` 依赖 bridge，而不是依赖 VS Code

当前：

- [message-handler.ts](/Users/xie/code/magi/src/ui/webview-svelte/src/lib/message-handler.ts)

建议改造方向：

- 初始化时注入 `ClientBridge`
- 所有消息收发通过 bridge 完成
- 保持协议不变，不在 UI 层重新发明通信格式

---

## 6.2 宿主层改造

### A. WebviewProvider 降级为 VS Code 壳

当前：

- [webview-provider.ts](/Users/xie/code/magi/src/ui/webview-provider.ts)

建议目标：

- `WebviewProvider` 只负责 VS Code 生命周期
- 真正业务控制器改为可复用的 runtime gateway

### B. 抽出 HostCapabilities

建议定义：

```ts /Users/xie/code/magi/src/host/types.ts
export interface HostCapabilities {
  workspace: WorkspaceHost;
  fs: FileSystemHost;
  terminal: TerminalHost;
  git: GitHost;
  lsp?: LspHost;
  diagnostics?: DiagnosticsHost;
}
```

然后由：

- `vscode-host`
- `local-agent-host`

分别实现。

### C. 优先处理强耦合模块

优先级最高的 VS Code 直接依赖点：

1. [file-executor.ts](/Users/xie/code/magi/src/tools/file-executor.ts)
2. [lsp-executor.ts](/Users/xie/code/magi/src/tools/lsp-executor.ts)
3. [verification-runner.ts](/Users/xie/code/magi/src/orchestrator/verification-runner.ts)
4. [extension.ts](/Users/xie/code/magi/src/extension.ts)
5. [webview-provider.ts](/Users/xie/code/magi/src/ui/webview-provider.ts)

---

## 7. 第一阶段最小可行版本（MVP）

建议不要追求“一次做完整 Web IDE”，而是先做可交付的最小闭环。

### MVP 目标

支持用户在不打开 VS Code 的情况下：

- 打开 Web UI
- 连接本地 agent
- 选择/绑定工作区
- 发起会话
- 查看主线 / Worker / 任务 / 变更 / 知识
- 运行编排
- 执行文件编辑、shell、Git worktree、基础 LSP 查询

### MVP 不做

- VS Code 专属装饰能力
- 编辑器内联交互
- 复杂 panel/command 集成
- 高级云端工作区托管

---

## 8. 分阶段实施计划

## Phase 0：边界冻结

目标：

- 明确“一套 UI，多宿主”作为正式方向
- 禁止新代码继续把共享 UI 绑死在 `vscode-bridge`

交付物：

- 本文档
- 宿主抽象原则

## Phase 1：前端桥接抽象

目标：

- 引入 `ClientBridge`
- UI 不再直接依赖 `vscode-bridge`
- 拆 `main-vscode.ts` / `main-web.ts`

验收标准：

- 共享组件中不再出现 VS Code 宿主专有调用
- Web 入口可在浏览器独立启动

## Phase 2：本地 Agent 宿主

目标：

- 将 runtime 核心运行在本地 agent
- Web 前端通过桥接协议连接 agent

验收标准：

- 非 VS Code 环境可完成完整任务编排
- 文件/Git/shell/worktree 能力可用

## Phase 3：VS Code 宿主收壳

目标：

- 插件仅作为宿主入口之一
- 业务核心不再被 `WebviewProvider` 持有

验收标准：

- VS Code 与 Web 共用同一套 UI 状态模型
- 运行时核心不分叉

---

## 9. 风险与控制

## 9.1 最大风险

不是“Web 做不出来”，而是：

> 在没有抽宿主边界的情况下，硬做第二套入口，最终导致插件和 Web 双线分叉。

### 风险表现

- 两套 UI 逻辑
- 两套状态模型
- 两套消息协议
- 两套行为语义

这是必须避免的。

## 9.2 控制原则

1. UI 共享优先，不复制页面
2. 协议共享优先，不新造第二套前后端消息结构
3. 核心 runtime 单一事实源，不在 Web 侧再造一套编排逻辑
4. 宿主能力通过 adapter 注入，不允许组件层写宿主分支

---

## 10. 最终建议

从当前项目实际情况出发，推荐结论如下：

1. **可以做一套 UI 同时服务插件和 Web，而且应当这样做**
2. **不能继续让共享 UI 直接依赖 VS Code Webview 宿主**
3. **Web 版本不应做成纯浏览器聊天页，而应接本地 agent 宿主**
4. **短期最佳路线是：Shared UI + ClientBridge + Local Agent**
5. **长期再考虑 Cloud Workspace，而不是现在直接上云端形态**

一句话收口：

> **Magi 的 Web 化，正确方向不是“把插件搬上网页”，而是“把 Magi 升级成一个多宿主产品”。**

