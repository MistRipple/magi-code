# Magi Issues 处理进度追踪

> 来源：https://github.com/MistRipple/magi-docs/issues
> 更新时间：2026-02-24

## 一、可代码修复的 Bug

| Issue | 标题 | 状态 | 根因分析 | 修复文件 |
|-------|------|------|----------|----------|
| #2 | mcp 不支持 http mcp | ✅ 已修复 | 前端 MCPServer 接口未与后端同步 | `SettingsPanel.svelte` (4处) |
| #15 | 粘贴模型端点时会重复输入 | ✅ 已修复（Issue 已关闭） | VS Code 宿主和浏览器引擎双重触发 paste | `main.ts` (1处) |
| #19 | 切换窗口后输入内容丢失 | ✅ 已修复 | 顶部 Tab 使用 `{#if}` 条件渲染导致组件销毁重建 | `App.svelte` (2处) |
| #16 | Worker EISDIR 错误 | ✅ 已修复 | `readFileSync`/`readFile` 未检查目录路径直接读取 | `worker-pipeline.ts` (2处) + `autonomous-worker.ts` (1处) |
| #4 | 任务无法中断 | ✅ 已修复 | `interrupt()` 清除 abortController 引用导致循环中断检测失效 + 中断步骤顺序竞态 | `worker-adapter.ts` + `orchestrator-adapter.ts` + `webview-provider.ts` |
| #6 | 界面超时/按钮失效 | ✅ 已修复 | 与 #4 + #20 同源（中断失效 + UI 高频渲染） | 同 #4 + #20 |
| #20 | 运行中点击任务卡死 | ✅ 已修复 | `$effect` 每次 stateUpdate 都创建新 Set 触发无效渲染 | `TasksPanel.svelte` |
| #1 | 找不到工具，无限循环+停止没反应 | ✅ 已修复 | 中断失效(#4修复) + Orchestrator 缺少止损机制 | `orchestrator-adapter.ts` (三层止损) |
| #11 | 编排模式循环调用+无法暂停 | ✅ 已修复 | 中断失效(#4修复) + Orchestrator 缺少止损机制 | `orchestrator-adapter.ts` (三层止损) |
| #22 | GPT编排模型循环调用view | ✅ 已修复 | Orchestrator 无总轮次上限 + 无空转检测 + 无重复工具检测 | `orchestrator-adapter.ts` (三层止损) |
| #21 | codex用chat接口/推理强度不生效 | ✅ 已修复 | `reasoning_effort` 被 `enableThinking` 门控，用户未设 `enableThinking: true` 时参数静默不传递 | `universal-client.ts` (2处) |
| #7 | 未设codex但仍运行codex | ✅ 已修复 | `dispatch_task` 工具定义的 worker enum 未过滤 enabled=false 的 Worker，与系统提示词信息矛盾 | `dispatch-manager.ts` (1处) |
| #9 | ACE问题 | ✅ 已修复 | 日志 Error 对象序列化丢失 + `is400ToolSchemaError` 正则过于宽泛误触发工具排除 | `unified-logger.ts` (1处) + `index-persistence.ts` (1处) + `universal-client.ts` (1处) |

## 二、已间接修复（由其他 Bug 修复覆盖）

> 所有间接修复的 Issues 已升级到"一、可代码修复的 Bug"中（#1、#11 由 #4 中断修复 + Orchestrator 三层止损覆盖）

## 三、模型行为 / 使用配置类问题（非代码 Bug）

| Issue | 标题 | 状态 | 详细分析 |
|-------|------|------|----------|
| #8 | 第二次运行出错后突然好了 | ⬜ 已被现有修复覆盖 | 截图分析：AbortController 状态残留（#4 已修复）+ 瞬态 API 错误（内置重试覆盖）+ 三层止损（#1/#11/#22）+ 日志改进（#9） |
| #9 | ACE问题 | ✅ 已修复 | 日志中 Error 对象序列化丢失，LLM 400 为用户配置问题，ACE 超时降级机制正常 |
| #10 | 开始编排失败 | 📋 已分析 | 仅标题，无任何描述或截图，信息严重不足，无法定位 |
| #17 | 模型配置是否支持chat/gemini接口 | ⬜ 无需解决 | 使用疑问。Gemini 通过 OpenAI 兼容端点接入（provider=openai, baseUrl 指向 Gemini 端点），无需单独支持 gemini provider — 违反统一架构原则且 Gemini 特有功能对 Magi 无业务价值 |
| #18 | codex worker总报错 | 📋 已分析 | 使用问题，无详细错误日志。需用户提供具体报错信息才能分析 |

## 四、功能优化需求

| Issue | 标题 | 状态 | 备注 |
|-------|------|------|------|
| #12 | 设置界面新增模型列表 | 📝 功能需求 | UI 功能增强 |
| #13 | 无法配置隐藏思考过程 | ✅ 已完成（Issue 已关闭） | 配置功能增强已交付 |

---

## 详细修复记录

### Issue #2 - MCP 不支持 HTTP MCP ✅

- **表象**：HTTP 类型 MCP 服务器无法在前端正确编辑和显示
- **根因**：前端 `MCPServer` 接口仅适配 stdio，未同步后端 `MCPServerConfig` 的 HTTP 字段
- **修复**：`SettingsPanel.svelte` 4 处变更（接口扩展 + 数据加载 + 编辑序列化 + 列表展示）
- **验证**：类型检查 ✅ | 数据流审计 ✅ | 构建 ✅ | 边界场景 ✅

### Issue #15 - 粘贴模型端点时会重复输入 ✅

- **表象**：粘贴 "AAAAA" 会变成 "AAAAAAAAAA"（翻倍），API Key 等字段同样
- **根因（5 Whys）**：
  1. 粘贴内容被执行两次 → Why?
  2. VS Code 的 clipboardPaste 命令 + 浏览器原生 paste 同时触发 → Why?
  3. 粘贴链路没有在 paste 事件层做去重，宿主/浏览器双触发会被直接写入两次 → Why?
  4. 早期实现只关注快捷键处理，缺少 paste 事件级别的统一去重机制 → **根因**
- **修复**：`main.ts` 在 capture 阶段对 `paste` 做 100ms 时间窗去重，重复事件 `preventDefault + stopImmediatePropagation`，从事件源头避免双写入
- **验证**：类型检查 ✅ | 前端构建 ✅ | 后端构建 ✅ | 边界场景(8项) ✅

### Issue #19 - 切换窗口后输入内容丢失 ✅

- **表象**：在对话输入框输入内容后，切换到任务/变更/知识 Tab 再切回，输入内容消失
- **根因（5 Whys）**：
  1. 输入内容丢失 → Why?
  2. InputArea 组件被销毁重建，`$state` 重新初始化 → Why?
  3. ThreadPanel（包含 InputArea）在 Tab 切换时被销毁 → Why?
  4. App.svelte 顶部 Tab 使用 `{#if}` 条件渲染，条件不满足时组件被完全销毁 → **根因**
- **对比**：ThreadPanel 内部底部 Tab（thread/claude/codex/gemini）已使用 CSS 显隐方式，不存在此问题
- **修复**：`App.svelte` 2 处变更 — 模板从 `{#if}` 改为 CSS class 控制显隐 + 添加 `.top-tab-pane` CSS 样式
- **验证**：类型检查 ✅ | 前端构建 ✅ | 后端构建 ✅ | 边界场景(7项) ✅

### Issue #16 - Worker EISDIR 错误 ✅

- **表象**：三个 Worker 全部因 `EISDIR: illegal operation on a directory, read` 错误失败
- **根因（5 Whys）**：
  1. Worker 执行失败并抛出 EISDIR → Why?
  2. `fs.readFileSync` / `fs.readFile` 被调用在目录路径上 → Why?
  3. `captureTargetContents()` / `hasContentChanges()` 只检查 `existsSync`（目录返回 true）→ Why?
  4. `collectTargetFiles()` 从 `assignment.scope.targetPaths` 收集路径时不区分文件和目录 → Why?
  5. 设计时假设 `targetPaths` 只包含文件路径，未考虑用户/编排者可能传入目录路径 → **根因**
- **修复**：2 个文件 3 处变更
  - `worker-pipeline.ts` `captureTargetContents()` — 添加 `fs.statSync()` + `isDirectory()` 检查，目录跳过
  - `worker-pipeline.ts` `hasContentChanges()` — 添加 `fs.statSync()` + `isDirectory()` 检查，目录跳过
  - `autonomous-worker.ts` `readFileWithCache()` — 添加 `fs.stat()` + `isDirectory()` 检查，目录抛出明确错误（上层 `buildTargetFileContext` catch 捕获）
- **验证**：后端构建(tsc) ✅ | 前端构建(vite) ✅

### Issues #4/#6/#20 - 任务无法中断 / 界面超时按钮失效 / 运行中点击任务卡死 ✅

- **表象**：
  - #4：点击结束按钮任务中断无效，新开窗口继续输出，死循环
  - #6：界面显示超时，后台持续运行，无法中断，按钮全部失效，最后无法响应
  - #20：运行过程中点击任务面板必定卡死，切换窗口后恢复
- **根因（5 Whys）**：
  - **根因 1 — `interrupt()` 清除 abortController 引用（#4/#6）**
    1. 点击中断后任务仍继续 → Why?
    2. adapter 的 `while(true)` 循环未正确检测到 abort 状态 → Why?
    3. L306 `this.abortController.signal.aborted` 抛出 TypeError（abortController 为 undefined）→ Why?
    4. L422 `this.abortController?.signal.aborted` 返回 false（可选链对 undefined 返回 undefined → falsy）→ Why?
    5. `interrupt()` 调用 `abort()` 后立即 `this.abortController = undefined` → **根因**
  - **根因 2 — `interruptCurrentTask()` 步骤顺序竞态（#4）**
    1. Worker 在 abort 后恢复并开始新 Todo → Why?
    2. `cancellationToken.isCancelled` 仍为 false → Why?
    3. `adapterFactory.interruptAll()` 先于 `orchestratorEngine.cancel()` 执行 → **根因**
  - **根因 3 — `$effect` 高频无效渲染（#20）**
    1. 运行中点击任务面板卡死 → Why?
    2. UI 渲染被大量同步更新阻塞 → Why?
    3. `$effect` 每次 `tasks` 变化都创建新 Set 写入 `expandedTasks` → Why?
    4. 后端 `sendStateUpdate()` 高频触发（20+ 调用点），每次重建 `store.tasks` 数组 → **根因**
    5. "切换窗口可恢复"：VS Code webview 不可见时暂停渲染，再次可见时批量处理积压更新
- **修复**：4 个文件 4 处变更
  - `worker-adapter.ts` `interrupt()` — 移除 `this.abortController = undefined`，保留引用让循环检查点正确检测 abort 状态
  - `orchestrator-adapter.ts` `interrupt()` — 同上
  - `webview-provider.ts` `interruptCurrentTask()` — 调换步骤顺序：先 `orchestratorEngine.interrupt()`（设置 cancellationToken + 中断 Worker）再 `adapterFactory.interruptAll()`（兜底）
  - `TasksPanel.svelte` `$effect` — 增加 `needsExpand` 判断，仅在有新增 running task 需要展开时才写入 `expandedTasks`
- **验证**：类型检查 ✅ | 数据流链路审计(3场景) ✅ | 后端构建(tsc) ✅ | 前端构建(vite) ✅ | 边界场景 ✅


---

## 剩余 Issues 详细分析记录

### Issues #1/#11 — 循环调用 + 无法暂停 🔶

- **表象**：
  - #1：「找不到工具，无限在循环」+「停止点击后没反应」
  - #11：「codex 一直在重复查看同一个文件，并且无法暂停」
- **分析**：
  - 核心痛点「停止/暂停无响应」已被 #4/#6/#20 修复解决（interrupt() 不再清除 abortController 引用 + 中断步骤顺序修正）
  - 循环调用本身是 LLM 模型行为问题 — Worker adapter 已有完整安全网：
    - `maxTotalRounds`：claude=40, codex=25, gemini=40（硬上限，到达后强制总结并终止）
    - 空转检测：stallScore 渐进式三级警告 → stallAbortThreshold 终止
    - 连续失败检测：consecutiveFailThreshold=5 → totalFailLimit=25
    - 无实质输出检测：noOutputWarn/Force/Abort (5/8/12)
  - 这些安全网最终会终止循环，但用户感知到的「无法停止」是因为 #4 的 bug
- **结论**：核心痛点已修复。循环行为是模型能力限制，安全网机制已覆盖

### Issue #22 — GPT 编排模型循环调用 ✅（止损已加，待回归观察）

- **表象**：使用 GPT 作为编排模型，一直来回调用 view 工具，明明已读出内容仍反复读取
- **分析**：
  - 循环表象来自两部分叠加：模型收敛性波动 + 编排层缺少统一止损
  - 代码侧已补三层止损：总轮次上限、连续同工具重复检测、编排空转检测（见 `orchestrator-adapter.ts`）
- **结论**：代码层止损已落地，能够在循环场景主动收敛；Issue 仍 Open 主要是等待用户回归确认

### Issue #7 — 未设 codex 但仍运行 codex ✅

- **表象**：用户未配置 codex（enabled=false），画像也全改为 claude，但编排模型仍然尝试向 codex 分派任务
- **根因分析（5 Whys）**：
  1. 编排模型仍选择 codex 进行 dispatch → Why?
  2. `dispatch_task` 工具的 `worker` 参数 enum 包含 codex → Why?
  3. `setupOrchestrationToolHandlers()` 注入 Worker 列表时未过滤 `enabled` 状态 → Why?
  4. 代码直接使用 `ProfileLoader.getAllProfiles()`（始终返回 3 个画像），未结合 LLM 配置的 `enabled` 字段过滤 → Why?
  5. "可用 Worker"查询逻辑分散在多个消费者各自实现，缺少统一收口的 Single Source of Truth → **根因**
- **信息一致性问题**：编排 LLM 通过两个通道获取 Worker 信息 — 系统提示词（已正确过滤 enabled）和工具 schema enum（未过滤 enabled），两者矛盾导致 LLM 倾向相信结构化约束（enum）而忽略自然语言约束（提示词）
- **修复**（4 个文件）：
  - `profile-loader.ts`：新增 `getEnabledProfiles()` 方法，组合画像系统（角色定义）和 LLM 配置（可用性），作为"可用 Worker"查询的 Single Source of Truth
  - `dispatch-manager.ts`：改用 `getEnabledProfiles()` 替代 `getAllProfiles()` + 手动过滤
  - `mission-driven-engine.ts`：改用 `getEnabledProfiles()` 替代 `getAllProfiles()` + 手动过滤，移除多余的 `LLMConfigLoader` import
  - `profile-aware-reviewer.ts`：改用 `getEnabledProfiles()`，修复互检评审者可能选到 disabled Worker 的潜在隐患
- **冗余清理**：
  - `dispatch-manager.ts`：移除运行时 enabled 校验（死代码，上游 `orchestration-executor.ts` 的 `validWorkers.includes()` 已拦截）+ 移除多余的 `LLMConfigLoader` import
  - `mission-driven-engine.ts`：移除多余的 `LLMConfigLoader` import
- **多代理交叉验证**：
  - tsc 编译 ✅ | vite 前端构建 ✅
  - 数据流链路：3 个消费者统一使用 `getEnabledProfiles()`，`getAllProfiles()` 已无外部消费者 ✅
  - 防线层级：`orchestration-executor` 双工具（dispatch_task / send_worker_message）均通过 `getWorkerEnum()` + `validWorkers.includes()` 校验 ✅
  - 边界场景：全禁用（安全降级 L1/L2）、单 Worker（一致正确）、评审者不足（正确抛错）✅

### Issue #8 — 第二次运行出错后突然好了 ⬜ 已被现有修复覆盖

- **表象**：第一次运行正常 → 第二次运行出错 → 最后突然恢复
- **截图分析**：VS Code 输出面板完整运行日志（1209x2969px），包含正常运行 → 错误 → 恢复流程
- **代码审查**：
  - AbortController 生命周期：每次 `sendMessage`/`runOrchestrationLoop` 创建新实例（worker-adapter L301, orchestrator-adapter L134/L452），不存在状态残留
  - 编排器执行队列：`enqueueOrchestratorExecution` 串行化保护，等待 `running=false` 后才执行下一个
  - LLM 重试：`withRetry` 3 次 + 指数退避（500ms base），覆盖 408/429/5xx/网络错误
- **已覆盖的修复**：
  - Issue #4：`interrupt()` 不再清除 abortController 引用 → 消除状态残留
  - Issues #1/#11/#22：三层止损 → 防止无限循环耗尽资源
  - Issue #9：Error 序列化修复 → 增强可诊断性
- **结论**：无需额外代码修复

### Issue #10 — 开始编排失败 📋

- 仅标题，无任何描述或截图，信息严重不足
- **结论**：需用户补充详细错误信息和日志

### Issue #9 — ACE 问题 ✅

- **表象**：日志中出现三个问题
  1. `[session] 索引持久化.保存失败 {error: {…}}` — Error 对象被序列化为空对象
  2. `[llm] LLM error 400: Request contains an invalid argument.` — provider=openai, model=claude-opus-4-6-thinking
  3. `[tools] AceExecutor.ACE搜索超时，降级到本地搜索` — timeout: 8000
- **问题 C — 索引持久化.保存失败（代码 bug，已修复）**：
  - **根因（5 Whys）**：
    1. 日志中错误信息完全丢失，显示为 `{error: {…}}` → Why?
    2. `JSON.stringify(Error实例)` 序列化为 `{}`（Error 的 message/stack 属性不可枚举）→ Why?
    3. `logger.warn('...', { error }, ...)` 把 Error 包在 `{ error }` 对象中作为 `data` 传入 → Why?
    4. `warn()` 方法不像 `error()` 方法那样对 Error 实例做特殊处理（提取 message/stack 到 `record.error`）→ Why?
    5. `writeToFile()` 的 `JSON.stringify` 无 Error replacer，无法序列化 data 中嵌套的 Error 对象 → **根因**
  - **影响范围**：全项目 23 处 `logger.warn('...', { error }, ...)` 模式全部存在错误信息丢失
  - **修复**（2 个文件）：
    - `unified-logger.ts` L652：`JSON.stringify` 添加 replacer 函数，将 Error 实例转换为 `{ name, message, stack }` — 单点修复全部 23 处
    - `index-persistence.ts` L134：错误日志增加 `path: this.cacheFilePath`，便于路径诊断
  - **验证**：tsc 编译 ✅
- **问题 A — LLM 400 错误 + `is400ToolSchemaError` 正则过于宽泛（代码 bug，已修复）**：
  - `Request contains an invalid argument.` 是 Google API 标准 400 错误格式
  - 用户将 Gemini 端点配给 provider=openai，但模型名填了 `claude-opus-4-6-thinking` — 端点不认识该模型名
  - **代码侧缺陷**：`is400ToolSchemaError()` 的正则 `/invalid.argument/` 过于宽泛，误匹配所有包含 "invalid argument" 的 Google API 400 错误
  - **危害**：通用 400 误判为工具 schema 不兼容 → 触发 `retryWithToolElimination` 二分法递归 → 对 N 个工具产生 ~2N 次无效 API 调用 + ~N 条 warn 日志 → 输出面板"一次性输出多条" + 前端错误展示混乱
  - **修复**：`universal-client.ts` L252 — 正则移除 `invalid.argument`，保留 `invalid.*schema|invalid.*tool|invalid.*function`（明确指向工具/schema 问题）
  - **验证**：tsc 编译 ✅
- **问题 B — ACE 搜索超时（正常降级行为，非代码 bug）**：
  - ACE 远程搜索超时 8s → 降级到本地三级搜索 → 本地搜索成功（resultLength: 4389）
  - 降级机制设计正确，按预期工作

### Issue #17 — 模型配置是否支持 chat/gemini 接口 ⬜ 无需解决

- **表象**：用户询问设置界面只有 openai 和 anthropic 两种 provider，没有 gemini 类型
- **分析**：Gemini 通过 Google 官方提供的 OpenAI 兼容端点接入（provider=`openai`，baseUrl=`https://generativelanguage.googleapis.com/v1beta/openai/`）。代码中已有 3 处 Gemini 专项兼容（stream_options 降级、工具 schema 净化、default 属性过滤）。单独支持 gemini provider 违反统一架构原则（§5.3 单一主实现路径），且 Gemini 特有功能（Grounding、Code Execution）对 Magi 无业务价值
- **结论**：无需解决。使用疑问，非代码问题

### Issue #18 — codex worker 总报错 ✅

- **表象**：截图错误信息 `LLM 执行失败:LLM 响应为空:流式传输完成但未收到有效内容`
- **错误链路**：
  - `worker-adapter.ts` L617-618：`finalText` 为空 → 抛出 `'LLM 响应为空：流式传输完成但未收到有效内容'`
  - `autonomous-worker.ts` L1024-1026：catch 包装为 `'LLM 执行失败: ${errorMessage}'`
- **完整数据流转链路审查**（5 环节逐一排查系统是否截断内容）：
  1. OpenAI SSE chunk 解析 → `chunk.choices[0]?.delta` 安全访问 → ❌ 无截断
  2. content 累加 → `delta?.content` falsy 检查合理，truthy 值完整累加 → ❌ 无截断
  3. LLMResponse 返回 → `fullContent` 完整传递 → ❌ 无截断
  4. worker-adapter 接收 → `accumulatedText` + `response.content` 双重保障 → ❌ 无截断
  5. thinking 路径 → 独立通道，不影响 `delta.content` → ❌ 无截断
- **根因**：**LLM API 真的没返回任何 content 也没有 tool_calls**（系统侧无截断逻辑）。codex 默认 `enabled: true`，未正确配置时仍可能被 dispatch → Issue #7 `getEnabledProfiles()` 已覆盖此场景
- **改进**：错误消息增加诊断上下文 `[worker/model/provider]`，便于用户快速定位问题 endpoint
- **修改文件**：`src/llm/adapters/worker-adapter.ts` L618

### Issue #21 — codex 用 chat 接口 / reasoning_effort 有效性 ✅

- **表象**：三个子问题
  1. Codex 使用 Chat Completion API 而非 Response API
  2. `reasoning_effort`（low/medium/high）是否生效
  3. 编排模型不按预定逻辑调用 Worker
- **分析结论**：
  1. **Chat API 是正确选择**：Magi 有自己完整的工具系统（不需要 OpenAI 内置工具），统一架构原则禁止双实现路径，第三方代理渠道仅支持 Chat API
  2. **reasoning_effort 存在代码 bug（已修复）**：原代码将 `reasoning_effort` 的传递耦合在 `enableThinking === true` 条件下，但 OpenAI Chat API 的 `reasoning_effort` 是独立顶层参数，无需前置条件。用户配置了 `reasoningEffort` 但未设 `enableThinking: true` 时参数静默不生效
  3. Worker 分配是编排模型（LLM）自主决策行为，非代码可控
- **修复**（文件：`universal-client.ts`）：
  - L874-879（非流式）、L935-940（流式）：移除 `this.shouldEnableThinking()` 前置条件
  - 修改前：`if (this.shouldEnableThinking() && this.config.reasoningEffort)`
  - 修改后：`if (this.config.reasoningEffort)`
  - `enableThinking` 仍保留其 UI 层职责（控制推理内容是否在前端展示），不影响 API 参数传递

### Issue #12 — 功能需求 📝

- #12：设置界面新增模型列表 — UI 功能增强需求
- **结论**：功能需求，非 bug

### Issue #13 — 隐藏思考过程 ✅

- GitHub Issue 已关闭（completed）
- 状态从“需求”更新为“已完成”

### Issues #1/#11/#22 — Orchestrator 三层止损机制 ✅

- **表象**：编排模型（GPT 等）循环调用 view 等只读工具，无限循环不收敛；停止按钮无响应
- **根因分析**：
  1. 「停止按钮无响应」→ 已被 #4 修复（interrupt() 清除 abortController 引用）
  2. 「无限循环不收敛」→ 修复前 Orchestrator adapter 仅有连续失败检测（工具成功返回但无意义时不触发），缺少总轮次/空转/重复工具止损
- **修复方案**（文件：`orchestrator-adapter.ts`）：
  - **层 1：总轮次安全网**（MAX_TOTAL_ROUNDS=80）
    - round=70 → 注入"即将达到上限"提前警告
    - round=80 → 撤掉工具权限（forceNoToolsNextRound=true），给 LLM 一轮纯文本总结机会
    - round>80 → 强制终止
  - **层 2：连续同工具重复检测**（SAME_TOOL_WARN=6, SAME_TOOL_FORCE=10）
    - 跟踪主工具名，连续 6 轮同一工具 → 警告提示
    - 连续 10 轮同一工具 → 撤掉工具权限强制总结
    - dispatch_task/send_worker_message/wait_for_workers/写入操作不参与检测（这些重复调用是正常编排行为）
    - 阈值高于 Worker adapter，因为 L1 场景下 Orchestrator 连续调用 file_view 读取不同文件是正常的分析行为
  - **层 3：编排者空转检测**（STALL_WARN=10, STALL_FORCE=15）
    - "编排动作" = dispatch_task / send_worker_message / wait_for_workers / file_edit / file_create / file_insert / file_remove
    - 连续 10 轮无编排动作 → 警告提示
    - 连续 15 轮无编排动作 → 撤掉工具权限强制总结
    - 任何编排动作 → 重置所有空转计数
- **验证**：tsc 编译通过 + vite 构建通过 + 多代理交叉验证（逻辑正确性 + 数据流 + 7 个边界场景）
