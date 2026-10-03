# Magi MCP 服务 · 设计基线

> 状态：**本机模式（回环 HTTP + stdio）与网络模式（仅 Cloudflare Quick Tunnel，地址每次开启都会变）已实现，GPT Web 侧接入接口就绪**；进度与偏差见《[开发计划](./magi-mcp-server-development-plan.md)》§2.1.1。本文定义 Magi 对外提供的**标准 MCP 服务端**：任何支持 MCP 的客户端（Claude Desktop、Cursor、Cline、ChatGPT 连接器等）都可以连接它，在 Magi 已注册的工作区里使用 Magi 的工具（文件创建 / 编辑 / 删除、搜索、git、变更账本等），审批、审计与变更记录仍由 Magi 负责。
> 更新日期：2026-09-30。
> 相关文档：[GPT Web 开发文档](./web-model-browser-development.md)（其工具能力是本服务的一个客户端）、[工程约束与运行入口](./README.md)、[Magi Connect 与移动端方案](./magi-connect-mobile-plan.md)、[内置浏览器完整设计](./browser-runtime-design.md)。

---

## 0. 定位

Magi 目前只是 MCP **客户端**（Magi 去调用别人的工具）。本文新增 MCP **服务端**（别人来调用 Magi 的工具）。

- **通用扩展，独立于 GPT Web。** GPT Web 是内置功能：在 Magi 发消息、同步到网页、把回复读回 Magi。本服务是并列的另一件事：把 Magi 的工具能力以标准协议开放出去。两者互不依赖；GPT Web 想用 Magi 工具时，只是在 ChatGPT 侧把本服务配置成连接器，和 Claude Desktop 配置它没有本质区别。
- **价值在治理，不在“又一个文件工具”。** 单纯的文件读写别的 MCP 服务也有。Magi 的差异化是：人工审批、变更账本（待处理变更的批准 / 回退）、快照与恢复、审计、知识与语义检索，以及与 Magi 会话事实的一致性。产品定位与文案围绕这些。
- **默认只本机。** 网络模式（含 Cloudflare 隧道）是显式开启的二期能力，而且是全案风险最高的部分（§10、§15）。

---

## 1. 固定规则

以下规则是唯一结论源，改动必须先经用户确认并更新本节。

| 编号 | 规则 | 理由与边界 |
| --- | --- | --- |
| M1 | **只有一个 Magi MCP 服务端**，按标准 MCP 协议实现 `initialize`、`tools/list`、`tools/call`（并按需 `notifications/tools/list_changed`）。工具由服务端直接暴露，不做“通用 inventory / call 二次包装”，不做文本协议。 | 客户端能看到真实的参数 schema、说明与只读 / 破坏性标注。 |
| M2 | **每个客户端一份可吊销的令牌**，绑定**一个工作区**、一个权限档和一个归属模式。令牌哈希存储，可设有效期，可随时吊销。 | 授权粒度是“这个客户端在这个工作区里能做什么”。 |
| M3 | **所有调用都先过：认证 → 令牌范围 → 权限档 → 路径校验 → 审批 → 执行 → 审计。** 没有旁路，本机与网络客户端走同一条链。 | 网络模式与本机模式只在传输和默认权限档上不同，不允许出现两套执行路径。 |
| M4 | **读工具自动放行；写、删除、执行默认需要 Magi 界面里的人工确认。** 网络客户端默认不得自动放行；用户可对**某个客户端在其工作区内**显式授予“允许编辑”，并可随时撤销。执行类（shell）默认关闭。 | 网络暴露的最坏后果是对本机的远程代码执行，默认必须最保守。 |
| M5 | **归属模式是令牌属性：** `external`（默认）与 `follow_web_slot`（仅 GPT Web 连接器使用）。服务端只有一套实现。 | 见 §5、§12。 |
| M6 | **外部客户端的调用记录在“外部工具会话”里。** 每个令牌对应一个由 Magi 管理的会话（类型为外部工具），复用既有的审批、变更账本、审计与 canonical。不为外部调用另建第二套账。 | Magi 的变更批准 / 回退、快照恢复都以会话为单位。 |
| M7 | **令牌只对绑定的已注册工作区有效。** 路径必须规范化并限制在工作区根内，拒绝符号链接逃逸；永远不可访问 Magi 状态目录、个人会话目录、凭据与系统敏感路径。 | 防止越权读写。 |
| M8 | **不暴露管理面：** 设置、模型与密钥、令牌管理、隧道与连接器配置、GPT Web 宿主、子代理，均不作为工具开放；下游 MCP 工具与 Skill handler 作为网关目录的一部分开放，但按 `Destructive` 每次确认。 | 最小暴露面；管理面永不开放，业务能力经审批开放。 |
| M9 | **网络模式必须显式开启，且默认关闭。** 开启前展示风险说明；令牌泄露等价于对该工作区的读写（如已授予写入与执行则等价于远程执行）。提供一键“撤销全部令牌并停止隧道”。 | 明确后果、可立即止损。 |
| M10 | **MCP 服务的监听端口与隧道入口独立于 Magi 主应用。** 不挂到主 app、不共享其鉴权中间件、不暴露任何 `/api/*` 路由；不复用 Magi 现有“远程访问”隧道的用户会话路由与访问令牌。 | 现有远程访问是给 Magi Web / 手机用的，认证与路由边界不同。 |
| M11 | **不做兼容。** 不为旧版本保留分支；令牌与外部会话属于新数据，读不出来就视为无令牌，绝不让 daemon 加载失败。 | 沿用既有基线。 |
| M12 | **所有拒绝与失败都返回明确的 MCP 工具错误**，不得伪造成功，也不得在挂起超时后继续执行。 | 客户端与用户都需要可依据的结果。 |

---

## 2. 目标与非目标

**目标**
1. 本机：任何 MCP 客户端通过 stdio 或回环 HTTP 使用 Magi 工作区的工具，读操作即用，写操作经 Magi 审批。
2. 网络：显式开启后，通过 Cloudflare 隧道让远端 MCP 客户端使用同一套工具，安全边界不弱于本机（令牌、审批、范围限制、审计、撤销）。
3. 每个客户端的调用可见、可审计、可回退（变更账本）。
4. GPT Web 的连接器可以直接使用它，并让调用内联显示在 Web 会话里（§12）。

**非目标**
- 不做 MCP 客户端的托管与发现（Magi 已有的客户端能力不变）。
- 不开放管理面（M8）与子代理；下游 MCP 与 Skill 以网关目录项开放，每次调用确认。
- 不为网络客户端提供“默认写入 / 默认执行”。
- 不支持匿名访问：没有令牌一律拒绝，包括本机 HTTP。
- 不承诺兼容所有 MCP 客户端的所有可选特性；核心只依赖标准 `tools/list` / `tools/call`。

---

## 3. 总体架构

```text
MCP 客户端（Claude Desktop / Cursor / Cline / ChatGPT 连接器 / …）
   │  stdio（本机子进程）        │ Streamable HTTP（回环 127.0.0.1）      │ Streamable HTTP（网络：Cloudflare 隧道）
   ▼                             ▼                                        ▼
┌────────────────────────── Magi MCP 服务（daemon 内，独立监听） ──────────────────────────┐
│ 认证（令牌 / 可选 OAuth）→ 令牌范围（工作区、权限档、归属模式）→ 限流与体积限制            │
│        │                                                                                │
│        ▼                                                                                │
│ 工具目录（按令牌生成，命名空间化）→ 路径与范围校验 → 审批策略 →                          │
│        │                                                                                │
│        ▼                                                                                │
│ 归属：external → 外部工具会话     |   follow_web_slot → GPT Web 槽位拥有者进行中的 turn    │
└────────┬───────────────────────────────────────────────────────────────────────────────┘
         ▼
既有执行 owner（tool runtime、权限与安全闸、snapshot 变更账本、audit / canonical）
```

- 服务端是**薄的协议与策略层**，不复制工具实现：调用最终交给现有的工具运行时与 loop 执行，审批走现有审批链，变更进现有变更账本。
- 传输实现位于 `crates/magi-mcp-server`：Streamable HTTP、stdio 与本地 socket 中继（原 GPT Web 专用 harness 已上移并删除，§13）。

---

## 4. 核心概念

| 概念 | 含义 |
| --- | --- |
| 客户端（client） | 一个被用户创建并命名的 MCP 使用者，例如“Cursor · 笔记本”“ChatGPT 连接器”。 |
| 令牌（token） | 客户端的凭据。创建时只展示一次；服务端只存哈希、前缀与元数据；绑定工作区、权限档、归属模式、有效期。 |
| 权限档（profile） | 决定该令牌能看到哪些工具、哪些默认放行：`read_only`（只读）、`edit`（允许在工作区内编辑，写入仍逐次确认）、`edit_trusted`（在该工作区内预授权编辑，无需逐次确认，仅可显式授予）、`exec`（额外开放 shell，逐次确认）。默认 `read_only`；网络客户端不得默认高于 `edit`。 |
| 外部工具会话 | Magi 为每个 `external` 令牌管理的一个会话，承载该客户端的所有调用、审批与变更。 |
| 归属模式 | `external`：调用归到该令牌自己的外部工具会话。`follow_web_slot`：调用归到 GPT Web 槽位拥有者**进行中的 turn**，作为普通工具项内联显示在 Web 会话里。 |

---

## 5. 认证与令牌

### 5.1 令牌
- 创建：设置页选择工作区、权限档、有效期与归属模式，生成后**只展示一次**。
- 存储：只存哈希（不可逆）、前缀（用于列表识别）、名称、工作区、权限档、归属模式、创建与最近使用时间、过期时间。原文不进日志、不进诊断、不进 canonical。
- 校验：恒定时间比较；过期、吊销、工作区被移除即失效；失败统一返回认证错误，不区分原因。
- 吊销：即时生效，同时断开该令牌的所有连接并使挂起调用以错误收口。

### 5.2 协议层认证
- **本机 stdio：** 客户端启动 `magi-mcp --stdio --token-file <path>`（或环境变量）；stdio 进程只是轻量中继，经仅当前 OS 用户可访问的本地 socket 连到 daemon，令牌校验在 daemon 侧完成。
- **HTTP（回环与网络）：** 标准 `Authorization: Bearer <token>`。同时校验 `Origin` 与 `Host`（防止 DNS 重绑定），拒绝无令牌请求。
- **OAuth 2.1（可选，二期）：** 面向要求 OAuth 的客户端（授权码 + PKCE，含动态客户端注册），由 Magi 作为授权方，授权页面在 Magi 内完成并落到同一套令牌与权限档模型。第一版不做。
- 网络模式在隧道外再叠加 Cloudflare Access 是**推荐但可选**的一层：只有当客户端能带自定义头（服务令牌）时才可用，不能替代 Magi 自己的令牌。

---

## 6. 工具面与命名

工具名命名空间化，避免与下游冲突；每个工具带 MCP 注解（只读 / 破坏性 / 幂等），让客户端可据此决定是否再确认。

| 命名空间 | 对应 Magi 现有能力 | 最低权限档 | 默认放行 |
| --- | --- | --- | --- |
| `magi.fs.read`、`magi.fs.list` | `file_read`、目录读取 | `read_only` | 是 |
| `magi.fs.write`、`magi.fs.patch`、`magi.fs.mkdir`、`magi.fs.move`、`magi.fs.copy` | `file_write`、`file_patch` / `apply_patch`、`file_mkdir`、`file_move`、`file_copy` | `edit` | 否（逐次确认；`edit_trusted` 除外） |
| `magi.fs.remove` | `file_remove` | `edit` | 否（始终逐次确认，`edit_trusted` 也不豁免） |
| `magi.search.text`、`magi.search.semantic` | `search_text`、`search_semantic` | `read_only` | 是 |
| `magi.git.status`、`magi.git.diff`、`magi.git.log` | git 只读能力 | `read_only` | 是 |
| `magi.git.*`（分支切换 / 创建 / 合并 / 推送等变更类） | `git_branch_switch`、`git_push`、`git_merge` 等 | `edit` | 否（逐次确认；`git_push` 始终确认） |
| `magi.changes.list`、`magi.changes.diff` | snapshot 变更账本的待处理变更 | `read_only` | 是 |
| `magi.changes.approve`、`magi.changes.revert` | 变更账本的批准 / 回退 | `edit` | 否（逐次确认） |
| `magi.shell.exec` | `shell_exec` | `exec` | 否（逐次确认，并受现有安全闸约束） |

> **第一批实现范围（已在 `crates/magi-mcp-server` 落地）：** 只包含已在工具运行时核对过存在的工具——`magi.fs.read`、`magi.fs.write`、`magi.fs.patch`、`magi.fs.apply_patch`、`magi.fs.mkdir`、`magi.fs.move`、`magi.fs.copy`、`magi.fs.remove`、`magi.search.text`、`magi.search.semantic`、`magi.shell.exec`。上表中的 `magi.git.*` 与 `magi.changes.*` 需要先核对现有 git 与变更账本能力的对外形态，核对后再加入目录；在此之前不对外暴露。

> `magi.changes.*` 是**新增的对外封装**（对应现有变更账本能力），需要在实现阶段核对其在会话之外的可调用性；其余工具映射到现有工具运行时。

**目录组成（网关）：** 除上面的静态精选名外，目录还包含**项目允许的**动态内置工具 `magi.<name>`、下游 MCP 工具 `mcp.<model_tool_name>` 与 Skill handler `skill.<name>`。下游 MCP 中非只读的工具与 Skill 一律按 `Destructive` 处理（每次都需用户确认）；GPT Web 槽位端点与外部客户端共用同一个目录、按各自权限档过滤（槽位没有 `exec`）。

**永不开放：** 依赖会话上下文的工具（agent / goal / plan / memory / context / browser / image）、子代理、任何管理面（M8）；GPT Web 宿主永远不可作为目标。

`tools/list` 按令牌生成：只包含该令牌权限档允许的工具，目录在令牌生命周期内固定；权限档或工作区变化时使旧连接失效并要求重连（或发送 `tools/list_changed`），客户端不支持可靠刷新时以重连为准。

---

## 7. 审批与权限

- **审批发生在 Magi 界面**（主窗口对话区 / 系统通知），复用现有审批链与安全闸；MCP 客户端只收到最终结果。
- **调用挂起：** `tools/call` 在审批与执行期间保持挂起。默认挂起时限 90 秒（必须早于客户端与隧道侧的超时，按需配置）。超时或用户拒绝，在**同一个 `tools/call`** 中返回明确工具错误，不执行、不在稍后补执行。
- **预授权：** `edit_trusted` 只能由用户在设置页对“某个客户端 + 某个工作区”显式授予，界面显示醒目标记与撤销入口；`magi.fs.remove`、`magi.git.push`、`magi.shell.exec` 不受预授权豁免。
- **无人值守：** Magi 界面未打开或无人响应时，需要审批的调用只会超时失败；不提供“无人时自动通过”。
- **限流与并发：** 每令牌限制并发调用数与调用频率，请求体与响应体设上限；超限返回明确错误。
- **按会话记录：** 每次审批、执行、拒绝都写入该令牌的外部工具会话与审计。

---

## 8. 路径与范围限制

- 令牌只对绑定工作区有效；所有路径参数先规范化再校验：必须落在工作区根内，拒绝 `..` 逃逸与符号链接逃逸，写入目标的父目录也要校验。
- 永远拒绝：Magi 状态目录（全局与工作区内的 `.magi/` 运行态）、个人会话目录、凭据与密钥目录（如 `~/.ssh`）、系统敏感路径；具体黑名单集中在一处维护，不散落。
- 工作区不再注册或令牌被吊销后，已有连接的后续调用一律拒绝。
- 结果大小设上限；超限返回带提示的截断，不做静默截断。

---

## 9. 会话、变更账本与审计

- **外部工具会话（M6）：** 每个 `external` 令牌一个，类型为外部工具，标题体现客户端名称与工作区；归属工作区固定。它承载调用记录、审批请求与变更，因此现有的“待处理变更 → 批准 / 回退”“快照与恢复”“审计”自然适用。
- **变更可回退：** 外部客户端写入的文件经同一个变更账本记录；用户可在 Magi 里查看差异并批准或回退，与 Magi 自己 agent 的改动一致。
- **审计：** 每次调用记录令牌前缀、客户端名称、工具名、路径 / 参数摘要、审批结果、执行结果与时间；不记录令牌原文，不记录文件正文。
- **展示：** 外部工具会话在会话列表中以独立类型呈现，避免与用户对话混淆；用户可以查看，但不能在其中发起模型对话。

> 实现前置核对：现有工具执行入口是否可以脱离“模型 turn”独立发起（例如以一次外部调用生成一个轻量的“外部工具 turn”承载审批与事实）。这是阶段 0 的第一项验证（§14）。

---

## 10. 传输与网络模式

### 10.1 三种传输

| 传输 | 用途 | 监听 / 入口 | 认证 |
| --- | --- | --- | --- |
| stdio | 本机客户端由配置直接拉起 | 无监听；经本地 socket 连 daemon | 令牌文件 / 环境变量 |
| 回环 HTTP | 本机支持 HTTP 的客户端 | 仅 `127.0.0.1` 随机高端口，写入 `state_root` 端口文件 | Bearer 令牌 + Origin / Host 校验 |
| 网络 HTTP | 远端客户端（含 ChatGPT 连接器） | 回环端口经隧道对外 | Bearer 令牌（可选 OAuth、可选 Cloudflare Access） |

### 10.2 Cloudflare 隧道方案（网络模式）

| 方案 | 特点 | 定位 |
| --- | --- | --- |
| Quick Tunnel | 无需账号；地址每次启动都变；无法做稳定连接器配置 | 仅开发态 spike，不向用户交付 |
| **命名隧道** | 用户自己的 Cloudflare 账号与域名；稳定地址；令牌（隧道 token）按文件引用，不进命令行、日志、settings | **网络模式的正式形态** |
| Magi Connect 托管地址 | 由 Connect 提供稳定 MCP 地址与设备凭据，用户无需自建 | Connect 就绪后优先采用 |

- 复用现有 `cloudflared` 托管能力（sidecar 检测、启动、监控），但**入口规则独立**：隧道只转发到 MCP 服务的独立回环端口，不转发到 Magi 主应用端口（M10）。
- 隧道凭据的存储与吊销规则与 GPT Web 文档 §8.4 对 OpenAI Tunnel 的要求一致。
- 隧道状态、公网地址、启动 / 停止、复制地址都在设置页；停止隧道或“撤销全部令牌”不影响本机模式。

### 10.3 网络模式的额外要求
- 默认关闭，开启前展示风险说明（M9）。
- 仅接受 HTTPS（由隧道终结 TLS）；服务端仍校验 `Host` 与 `Origin`。
- 每令牌限流、请求体上限、并发上限；连续认证失败按来源退避。
- 网络令牌默认权限档不高于 `edit`，且不得预授权 `edit_trusted`，除非用户在该令牌上单独确认。
- 交付前必须完成一次专门的安全评审（§14 阶段门槛）。

---

## 11. 设置与界面

设置 → “MCP 服务”（或“对外工具”）：
1. **总开关**与状态：本机服务是否运行、端口、活动连接数。
2. **客户端与令牌列表**：名称、工作区、权限档、归属模式、有效期、最近使用；创建、吊销、调整权限档。创建后生成**客户端配置片段**（Claude Desktop、Cursor 的 stdio 与远端 JSON）供复制。
3. **网络模式**：开关、风险说明、隧道方案与状态、公网地址、启动 / 停止；凭据只显示引用与状态。
4. **一键撤销**：撤销全部令牌并停止隧道。
5. **活动与审计**：最近调用、待审批数、被拒绝数。
6. 审批请求出现在主窗口对话区与系统通知，并标明来源客户端。

---

## 12. 与 GPT Web 的关系

两者并列：GPT Web 的收发同步不依赖本服务；本服务不依赖 GPT Web。GPT Web 想使用 Magi 工具时，经 OpenAI Tunnel 把本服务的**槽位端点**配置成 ChatGPT 连接器（Magi 可在托管浏览器里按用户的显式动作自动配置并回读，见 GPT Web 文档 §8）。本服务的权限档、审批、路径限制、审计对该连接器同样生效。

**已知取舍**：`follow_web_slot` 下归属由“单槽位 + 进行中的 turn”确定，不由消息里的令牌绑定；同一账号在别处（如手机上的 ChatGPT）恰好在此期间调用会被归入该 turn。审批不可绕过，写入类默认需要确认。

### 12.1 槽位端点（无令牌）

GPT Web 的项目工具只有一条路：ChatGPT 连接器 → OpenAI Tunnel → `magi-daemon-app mcp-relay --stdio --slot`（daemon 自己的子命令）→ daemon 的**槽位端点**（本地 socket，仅当前 OS 用户可访问）。槽位端点**不签发、不接受令牌**：调用方身份与归属每个请求都从唯一的槽位表（`magi_web_model::WebSlotTable`）派生。

| | 外部客户端（令牌） | GPT Web（槽位端点） |
| --- | --- | --- |
| 认证 | Bearer 令牌（设置页创建 / 吊销） | 无令牌；端点 = 当前 OS 用户的本地 socket |
| 工作区 | 令牌绑定的工作区 | **槽位拥有者会话的项目**（W17） |
| 调用归属 | 令牌自己的外部工具会话 | 拥有者会话里**唯一进行中的 turn**（`follow_web_slot`，W9） |
| 审批出现在 | 全局“外部客户端请求确认”托盘 | 拥有者会话的普通授权托盘（审批挂在该会话上） |
| 工具写入 | 外部工具会话的账本 | 拥有者会话 canonical 时间线里的**正式工具条目**（`ExternalToolItemWriter`） |
| 没有进行中的 turn / 槽位已释放 | — | 所有调用一律拒绝；已建立的连接下一条请求即失败 |
| 权限档 | 用户创建令牌时选择（可含 `exec`） | 设置里选的连接器权限档：`read_only` / `edit` / `edit_trusted`，**没有 `exec`** |

槽位在 turn 结束、被释放（停止 / 退出 / 切换到本地 / 清除数据 / 会话删除）时通过 `WebSlotTable::set_end_hook` 取消该槽位遗留的待审批，等待中的调用以“已取消”收口，不会补执行。

### 12.2 工具目录

槽位端点向模型暴露**项目允许的全部可用工具**，不是一个手写白名单：

- 静态精选名：`magi.fs.*`、`magi.search.*`、`magi.git.*`（只读）、`magi.changes.list/revert`、`magi.shell.exec`（仅非槽位客户端的 `exec` 档）；
- 动态内置工具：`magi.<name>`，来自当前项目的内置工具注册表；
- 下游 MCP：`mcp.<model_tool_name>`；
- Skill：`skill.<name>` handler。

排除：依赖会话上下文的工具（agent / goal / plan / memory / context / browser / image）永远不暴露。下游 MCP 中非只读工具与 Skill 一律按 `Destructive` 处理，**每次都需要用户确认**。同一个目录对所有客户端按权限档过滤，Web 槽位只是没有 `exec`。

### 12.3 连接器与通道

- 通道配置在设置 → 浏览器 → GPT Web：Tunnel id、运行时 API 密钥（粘贴后由 Magi 存进 state root 下的私有文件，之后不再显示）、工具权限档。`tunnel-client` 由 Magi 自动下载（固定版本，发布包 SHA-256 固定在代码里，校验通过才安装并记录解压后摘要供每次启动复核），失败 fail-closed 并报告原因。详见 GPT Web 文档 §8.3。
- GPT Web 通道的 stdio 中继是 daemon 可执行文件的 `mcp-relay` 子命令，不依赖单独分发的 `magi-mcp`；外部 MCP 客户端使用的 `magi-mcp` 开发期用 `cargo build -p magi-mcp-server --bin magi-mcp` 构建（尚未纳入桌面安装包）。
- 通道不可用（`web_tunnel_unavailable`）**不是 turn 失败**：GPT Web 仍可纯对话，会话只是没有项目工具，选择器入口显示“无工具”，设置页说明缺什么。
- Magi 公网隧道（Cloudflare Quick Tunnel 的 MCP 网络模式）是**外部 agent 的入口**，不用于 GPT Web 槽位：GPT Web 只走 OpenAI Tunnel。

### 12.4 代码地图

| 职责 | 位置 |
| --- | --- |
| 槽位表、占用 / 释放 / 结束钩子 | `crates/magi-web-model/src/binding.rs` |
| 槽位身份与归属解析 | `crates/magi-api/src/web_slot_mcp.rs` |
| 网关目录（静态 + 动态 + 下游 MCP + Skill） | `crates/magi-mcp-server/src/catalog.rs`、`crates/magi-api/src/mcp_service.rs` |
| 外部工具会话、审批、规范工具条目 | `crates/magi-conversation-runtime/src/{external_tool,external_approval,session_writeback}.rs` |
| OpenAI Tunnel 托管与通道状态 | `crates/magi-web-model/src/tunnel.rs`、`crates/magi-api/src/web_model_channel.rs` |
| 已保存对话 / 停止 / 释放 | `crates/magi-api/src/web_model_ops.rs` |

### 12.5 验证

```bash
CARGO_INCREMENTAL=0 cargo test -p magi-mcp-server -p magi-api web_slot
node scripts/verify-mcp-server.mjs --base <daemon> --mcp-bin target/debug/magi-mcp   # 外部客户端端到端
```

仍需真实环境验证（不能被自动化替代）：ChatGPT 连接器对 Tunnel stdio 的实际握手、单次 `tools/call` 的等待上限（审批默认 90 秒超时，若连接器更短则需做成可配置）、连接器自动配置的页面选择器。

---

## 13. 与现有代码的对照

| 现有部分 | 处理 |
| --- | --- |
| `crates/magi-web-model/src/harness/`（http、stdio、mcp、tunnel）与 `crates/magi-api/src/web_model_harness.rs` | **已完成**：上移为独立 crate `magi-mcp-server`，旧 harness 与 `web_model_harness.rs` 已删除（状态字段现名 `ApiState::web_model`）；GPT Web 只作为其槽位端点客户端 |
| `crates/magi-api/src/tunnel.rs`（`TunnelManager`，cloudflared 托管） | **复用托管能力**，新增独立入口规则与命名隧道的令牌引用；不复用其用户会话路由与访问令牌 |
| 工具运行时、权限、安全闸、审批链（`magi-tool-runtime`、`magi-permissions`、`magi-safety-gate`、`magi-governance`） | **复用**，作为唯一执行 owner；需核对其在“无模型 turn”下的可调用性 |
| `magi-snapshot` 变更账本、审计账本、canonical | **复用**；新增 `magi.changes.*` 对外封装 |
| 设置存储 | 新增令牌元数据（哈希、前缀、工作区、权限档、归属模式、有效期）；读不出来视为无令牌（M11） |
| 会话模型 | 新增“外部工具会话”类型，列表中独立呈现 |
| 前端设置页 | 新增“MCP 服务”分区（§11） |

---

## 14. 分期与验收

### 阶段 0：前置验证（先于一切）
1. **执行入口可独立发起**：验证现有工具运行时 + 审批链能否由外部调用直接驱动（可能需要新增一个入口或一种轻量的外部工具 turn）。
2. 变更账本对外部写入的记录、批准与回退可用。
3. 用 MCP 官方调试客户端验证 `tools/list` / `tools/call`、挂起与超时、取消。
任一不成立回到本文重新决策。

**阶段 0 代码核对结果（2026-09-30，读代码，未跑真实客户端）**

| 验证项 | 结论 | 依据与影响 |
| --- | --- | --- |
| 工具执行能否脱离模型 turn | **可以** | `magi-tool-runtime::ToolRegistry::execute_with_policy(input, context, policy)` 只需要 `ToolExecutionContext`（会话、工作区、工作目录、访问档）与 `ToolExecutionPolicy`（允许 / 拒绝路径、工具名、命令模式），不依赖 Task 或 turn。MCP 服务直接以它为执行入口。 |
| 审批能否复用 | **注册表可以复用，等待逻辑要新写** | `ToolApprovalRegistry::request_with_arguments` / `resolve` 把 `task_id`、`turn_id` 当不透明字符串使用，可以用 `external:<token_ref>` 形式的合成标识；但现有等待循环 `await_task_tool_approval`（`tool_batch.rs`）是私有函数，并强依赖“会话当前活动 turn + Task 仍在运行”。MCP 服务需要自己的等待循环，存活判据改为“令牌仍有效且连接未断”。外部审批只支持 `allow_once` / `deny`，不使用 `allow_for_turn`。 |
| 审批在界面里怎么出现 | **缺口，需要新增全局入口** | 现有审批查询与解析接口按会话作用域（`GET /session/tool-approvals?sessionId=…`）；外部工具会话不是用户当前打开的会话，审批会看不见。需要一个跨会话的“待审批”入口（通知中心 / 全局待办），并把外部审批标明来源客户端。这是阶段 2 的必做项。 |
| 变更账本对外部写入 | **待验证** | 变更账本以会话为单位；外部写入需要在外部工具会话下建立对应的快照会话，阶段 1–2 中用真实写入验证。 |

> **进度（2026-10-01，分支 `feature/magi-mcp`）：** 本机模式（阶段 1–3）已实现：宿主适配、外部工具会话与账本、审批、令牌持久化（`state_root/mcp-server.json`，**不经 settings**）、固定回环端口、stdio 中继（`magi-mcp`）、设置页、跨会话待审批托盘、审计、只读 git 与 `magi.changes.{list,revert}`；已用真实 daemon 与真实中继二进制端到端验证。网络模式（Quick Tunnel、按需激活、仅网络令牌、退避与限流）已在真实隧道上验证；GPT Web 槽位端点已接线（见 §12）。**未做：** 命名隧道 / Tailscale / 托管发放（决策：只用 Quick Tunnel）、OAuth、第三方客户端兼容验证、外部会话的用户查看入口。

### 阶段 1：本机 stdio + 令牌（只读优先）
令牌创建 / 存储 / 吊销、stdio 中继、`read_only` 工具集、路径校验、审计、外部工具会话。
**验收**：Claude Desktop 或 Cursor 通过 stdio 连接，能列出并调用只读工具；越权路径、符号链接逃逸、无令牌、过期令牌都被拒绝；审计可查。

### 阶段 2：写入与审批 + 回环 HTTP
`edit` / `exec` 档、逐次审批（含超时与拒绝）、变更账本对外写入、回环 HTTP 与 Origin / Host 校验、限流、设置页令牌管理与配置片段。
**验收**：外部客户端写文件需要 Magi 里确认；确认后变更出现在待处理变更中并可回退；拒绝 / 超时返回明确错误且不执行；`remove` / `push` / `exec` 始终逐次确认。

### 阶段 3：网络模式（Cloudflare）
命名隧道托管与令牌引用、独立入口规则、网络默认权限档、风险说明、一键撤销、限流退避；可选 OAuth 与 Cloudflare Access。
**门槛**：完成专门的安全评审（威胁模型、令牌泄露演练、越权与逃逸测试、拒绝服务测试）后才允许对用户开放。
**验收**：远端客户端经隧道可用同一套工具；撤销令牌与停止隧道立即生效；未开启网络模式时公网不可达。

### 阶段 4：接入 GPT Web
`follow_web_slot` 槽位端点、连接器自动配置、槽位释放使身份消失。真实通道 spike：ChatGPT 连接器对 `tools/list` 与挂起 `tools/call` 的实际行为。
**验收**：见 GPT Web 文档 §13 阶段 3。

### 检查命令
- Rust：`cargo check --workspace --tests` 及新增 crate、`magi-api`、工具运行时相关 crate 的测试。
- 涉及设置页与前端：`npm --prefix web run check` 及对应 `test:*`。
- 安全相关改动须运行 `security-review`。

---

## 15. 风险与待定项

| 风险 / 待定 | 影响 | 处理 |
| --- | --- | --- |
| **令牌泄露 = 对工作区的读写，授予执行时等价远程执行** | 网络模式下最严重的风险 | 默认本机；网络显式开启；网络默认不高于 `edit` 且逐次审批；限流、审计、一键撤销；交付前专项安全评审 |
| 工具执行入口是否能脱离模型 turn | 决定外部会话与审批的实现代价 | 阶段 0 第一项验证；必要时新增轻量外部工具 turn |
| 客户端超时短于审批耗时 | 写操作经常超时失败 | 挂起时限可配置；提示用户预授权受限范围的编辑（`edit_trusted`），高风险工具不豁免 |
| MCP 客户端差异（OAuth、Header、重连、`list_changed`） | 部分客户端接入受阻 | 核心只依赖标准 `tools/list` / `tools/call`；以重连兜底目录刷新；OAuth 二期 |
| Cloudflare 命名隧道需要用户自己的账号与域名 | 网络模式门槛高 | 文档与设置页引导；Connect 就绪后优先采用 |
| 外部会话在会话列表里造成噪音 | 用户困惑 | 独立类型呈现，可折叠 / 归档 |
| 归属 `follow_web_slot` 的误归属 | 别处同账号调用被归入 Web turn | 审批不可绕过；写入类默认确认；记录为已知取舍 |
| 范围蔓延 | 与 GPT Web 工作并行导致失控 | 分期严格按门槛推进；先本机后网络；管理面与代理类工具第一版不开放 |
