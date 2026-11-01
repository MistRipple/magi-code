# Prompt 缓存分析与优化记录

更新：2026-11-01。范围：HTTP 主对话、worker、会话辅助调用及主助手承接的 GPT Web 配置任务。

## 1. 结论与历史基线

缓存低不是所有角色的共同现象。读取本机当前使用的分段用量账本、按事件身份去重并沿执行统计的重置边界计算，得到以下历史数据。读取过程不修改账本，不输出凭据或对话正文。

样本时间为 UTC 2026-10-05 17:09:02 至 2026-10-31 01:44:03，最后一次统计重置水位为 0。该账本包含本机日常使用和历史验收请求，不能解释成纯生产流量，也不能作为本次优化后的效果。

| 来源 | 调用数 | 返回缓存读取数据的调用数 | 有缓存命中的调用数 | 输入 Token 命中率 | 请求命中率 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 主对话 orchestrator | 2,284 | 2,234 | 1,866 | 58.02% | 83.53% |
| worker | 386 | 385 | 382 | 84.81% | 99.22% |
| auxiliary | 308 | 308 | 2 | 5.07% | 0.65% |
| 生图 | 3 | 0 | 0 | 未观测 | 未观测 |
| 合计 | 2,981 | 2,927 | 2,250 | 68.61% | 76.87% |

辅助请求的完整输入平均约 459 Token，中位数 200.5 Token，280 / 308 次少于 1,024 Token。短输入、共享前缀短和一次性内容能解释其复用机会少；不同模型的缓存门槛不同，不能据此认定每个 miss 的原因。禁止为了增加命中率填充无用提示词。

历史 worker 样本以 explorer 为主（356 次），reviewer 和 executor 各 15 次；不能将总体结果直接推广到所有子代理角色。账本没有独立 web-setup 绑定记录，不能给它虚构独立命中率。当前配置任务已复用主线，新增调用归主对话统计。

## 2. 开源参考与采用边界

下列内容来自本轮实际读取的上游源码，体现机制，不代表这些项目的实测命中率：

| 参考项目 | 已核对机制 | Magi 的选择 |
| --- | --- | --- |
| [Codex CLI client.rs](https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/client.rs) | 会话派生键、显式覆盖/省略策略；增量请求核对模型、工具、推理设置与完整输入前缀 | 使用会话键，不使用 Turn、Step 或重试 ID；暂不引入 WebSocket 续写和上游专用路由头 |
| [pi OpenAI key helper](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/openai-prompt-cache.ts)、[Anthropic adapter](https://github.com/earendil-works/pi/blob/main/packages/ai/src/api/anthropic-messages.ts) | OpenAI 键限制为 64 字符；Anthropic 为 system、工具表末项、会话末项放置缓存断点 | Magi 会话键使用带版本域的 SHA-256，固定 64 个十六进制字符；补工具表和历史尾部断点 |
| [OpenCode model-request.ts](https://github.com/anomalyco/opencode/blob/bf544cd419c5f17535c0d5af929f7ecd41188320/packages/core/src/session/model-request.ts)、[shared.ts](https://github.com/anomalyco/opencode/blob/bf544cd419c5f17535c0d5af929f7ecd41188320/packages/ai/src/protocols/shared.ts)、[openai-chat.ts](https://github.com/anomalyco/opencode/blob/bf544cd419c5f17535c0d5af929f7ecd41188320/packages/ai/src/protocols/openai-chat.ts) | 会话 affinity；协议兼容性控制缓存字段；Chat 路由不向所有兼容服务盲发扩展 | Responses 使用标准缓存键字段；Chat 自动发送仅限直接 OpenAI 域名；Anthropic 使用其缓存断点 |

pi 当前仓库为 `earendil-works/pi`，旧 `badlogic/pi-mono` 地址发生迁移；以上 pi 链接在 2026-10-31 读取，为当时 main，非固定提交。

[OpenAI 官方缓存文档](https://developers.openai.com/api/docs/guides/prompt-caching)明确要求渲染后的完整前缀匹配；工具名称、描述、schema、顺序及相关推理设置都会影响复用。文档也区分模型代际：GPT-5.6 之前的模型可用稳定 key 改善路由；GPT-5.6 及之后 key 是可选的分组/核算机制，不能把“增加 key”当成这些模型命中率提升的保证。本轮没有强制长保留期、显式写入或计费预热。

## 3. 本轮落地

### 3.1 会话键与请求稳定性

- `PromptLineage` 在 daemon 运行时拥有缓存身份。主线、恢复和同一分支的输出上限调整沿同一 lineage；worker 按 task 分支、auxiliary 按 purpose 分支，不同会话和不同 epoch 分开。不把会话 ID 原文、凭据、当前时间、模型输出或工具回执编码进键。
- `StepModelClient` 和 `invoke_auxiliary_model_with_usage` 都从可信 session/branch lineage 派生 64 字符摘要键；草稿态没有会话时不虚构身份。请求发送前已经生成 Prompt IR 摘要，并把稳定规则、工具面、模型/权限绑定计算为 `cache_epoch`；追加历史与易变尾部保留为诊断字段，不会单独轮换 lineage key。
- `ModelStep` 冻结 `promptFrame` 与 `cacheEpoch`，重启和重试都沿同一份 Step 事实恢复；旧历史缺失这些字段时保持兼容并按未知处理。
- 缓存摘要统一从 `ModelBridgeClient::cache_prompt_ir` 取得与真实发送共用的 Prompt IR；HTTP 连接在该入口复用能力降级和工具 wire name 编码，避免 canonical 工具名摘要与 Provider 实际前缀分叉。
- Provider 私有上下文遵循所在消息的生命周期：稳定前缀变化进入 `cacheEpoch` 并记录 `provider_context_changed`；历史内正常追加保持 epoch，改写旧历史由 `history_rewritten` 记录；易变尾部只参与尾部诊断。第 12 节修正了此前对全部私有上下文统一断代的错误。
- Prompt IR 保留私有 thinking/signature/Responses 段诊断；版本 4 的 `providerContextHash` 仅摘要稳定前缀中的私有上下文。历史链和易变尾部分别拥有各自的私有上下文，旧 Step 缺少字段或使用旧版本仍可读取。
- 在 HTTP 请求转为 Provider 工具名之前，按规范工具名稳定排序，并递归规范化嵌套 JSON schema 的对象键。相同目录和等价 schema 的装配顺序不再改变 wire schema；工具内容变化仍真实发送，不保留已撤销能力。
- `ProviderCacheContract` 统一描述 route key、Anthropic 断点和 continuation 能力；未知 Chat 兼容代理保持保守策略，不猜测支持缓存扩展。
- Responses 和直接 OpenAI Chat 的流式/非流式路径共用装配。自定义 Chat 代理没有明确能力合同，仍保留原请求字段；因此本轮并未为这类代理开启显式缓存键。
- 请求指纹诊断额外包含实际发送的缓存键，可检查恢复或重试是否改变分组。

### 3.2 Anthropic 断点

保留已有静态 system 前缀断点，再给最后一个工具定义、最后一条 user 消息的末个受支持内容块加 `ephemeral`；工具结果仍保持 `tool_result` 结构。普通文本尾消息转为等价 text block。当前最多三个断点，不对 assistant thinking/signature 加字段，不改变授权或省略当前运行时规则。

这增加了可缓存边界。为避免新断点白写缓存，请求的易变尾部（任务上下文、引用资料、计划、邮箱、执行步骤快照）在模型视图里标为 `ChatMessageOrigin::RoundContext`：

- 滚动断点放在尾部之前的最后一条 user 消息上，而不是最后一条消息上；尾部每轮重建，标在它上面只会每步付一次写入费用却不会被读到。
- Anthropic 适配器只把开头那一段 system/developer 消息并进 system 字段。执行步骤快照等对话开始之后出现的 developer 消息，按 user 角色留在原位置，信任标记写在片段正文里。此前它们被并进 system，每轮变化都会使整段历史缓存失效。这是对该类消息发送位置的明确取舍：Anthropic 没有对话中途的 system 角色，留在 system 就必须接受每轮失效。
- `prompt_cache_key` 属于传输细节，不参与请求意图的序列化。工作流的待决动作冻结的是不含该键的请求，发送时才由 `StepModelClient` 按会话附加。

回归测试覆盖：尾部不进 system、断点不落在尾部、相邻两轮请求去掉标记后系统、工具表和旧历史逐字节相同。以上只验证请求体，未对真实 Anthropic 账号发送过请求。

### 3.3 统一用量统计和展示

Provider 响应 → 原有 usage 解析 → 原有 `model.usage.recorded` 账本 → `magi-usage-authority` reducer → 执行统计 API → 统计页。没有增加第二套业务存储。

`UsageTotals` 新增可从原账本重建的三个聚合值：已观测调用数、命中调用数、已观测完整输入 Token。旧快照缺少字段时按零读取；实际统计仍从原始账本重建。只有 `cache_read_tokens` 为 Some（包括 0）的调用进入观测分母；缺失字段的失败、GPT Web 或其他执行器调用不冒充 miss。只有缓存读写量的响应也保留其用量。

执行统计的总量、模型项和角色项都增加 `cache` 对象：

| 字段 | 定义 |
| --- | --- |
| `observedCallCount` / `hitCallCount` | 已报告缓存读取的调用数 / 读取量大于零的调用数 |
| `inputTokens` | 已观测调用的完整输入；OpenAI 已包含读取量时不重复加，Anthropic 分离的读取和写入量合入 |
| `readTokens` / `writeTokens` | Provider 已返回的缓存读取/写入 token 合计，未报告的写入量不推算 |
| `tokenHitRate` | 缓存读取 token / 已观测完整输入 token，分母为零时为 null |
| `requestHitRate` | 命中调用数 / 已观测调用数，无观测时为 null |

统计页按原有模型/角色切片展示两种命中率及“已观测 X / 总调用 Y”；跨连接先加总分子分母再计算比例，不平均百分比。没有缓存 usage 显示“未观测”，明确的零读取显示 0%。沿用共享设置行及中英文翻译。

## 4. 验证与未完成范围

- `cargo check -p magi-daemon`。
- `cargo test -p magi-bridge-client -p magi-usage-authority --lib`：273 + 33 项，覆盖协议字段、工具顺序、Anthropic 断点、缓存 usage 归一化、缺失/零/命中、幂等和聚合。
- `cargo test -p magi-conversation-runtime --lib model_step::tests::`：13 项，包含跨 Step/恢复/worker 的同会话键、不同会话隔离，以及压缩检查点代际断代。
- `cargo test -p magi-conversation-runtime --lib usage_recording::tests::`：16 项，包含 cache-only usage。
- `cargo test -p magi-api --lib execution_stats_`：8 项，覆盖账本权威、模型/角色/总量缓存投影与重置。
- `npm --prefix web run check`、`test:settings-store`、`test:i18n`：类型、加权聚合和本地化验证。
- `cargo check -p magi-bridge-client -p magi-web-model -p magi-plugin-system -p magi-daemon --tests`：涉及新请求字段的附属测试目标均可编译。
- 通过 daemon 的 `http://127.0.0.1:38123/web.html` 验收统计页：1280px 浅色布局与 375px 窄屏可读，窄屏 body 宽度为 375px，缓存统计行位于视口内。该次浏览器验收只覆盖旧 daemon 返回缺失 cache 字段时的“未观测”展示；正命中、零命中与加权计算由上述 API/聚合回归覆盖。未做深色、全部译文和真实新版 daemon 的完整视觉验收。

以上为首轮离线/旧版 UI 验证；后续独立 daemon 与真实模型验收见第 5 节。用户正在使用的 daemon 未重启，仍为不返回新增 cache 投影的旧构建，因此该进程上的热更新前端可显示“未观测”；这不是新的真实 miss。

缓存身份、Prompt IR、恢复控制、逐 attempt 用量归属和重启投影已完成本地架构验收；仍不能把它解释成所有 Provider 的真实命中率已解决。各模型成本、Anthropic 路径上尾部与断点的真实命中表现、自定义 Chat 代理缓存能力、Skill/权限切换与压缩边界仍需独立验收。不将短辅助请求或无数据执行器纳入统一百分比目标。

## 5. 独立 daemon 与真实模型对照

### 5.1 验收入口与数据来源

`scripts/verify-prompt-cache.mjs` 通过真实 daemon/App Server/Turn/工具执行/用量账本完成验收。每个构建使用单独的状态根和临时工作区，权限均为 `read_only`；不替换已有 daemon、用户设置或历史账本。样本固定为三轮短对话、依次读取两个合成文件、一个 explorer 读取任务、两次提示词强化，并计入自然发生的标题/知识提取辅助调用。

默认使用本地夹具，明确把返回 usage 标为合成数据。受控旧/新版各 21 次 HTTP 请求通过，核对每个角色的工具 schema 在稳定区间保持一致；优雅关闭 daemon 待账本批量刷盘后，按事件身份去重，分别核对总量、角色和模型缓存统计。旧版缺少 cache DTO 时从原账本核对，不将缺失字段当作零。超限回归用 `--max-calls 1` 确认只允许一次上游请求，拒绝后退出失败并保存阶段、统计和拒绝原因，不误报整轮通过。

真实模式显式使用 `--live`，从已有设置在内存读取连接凭据；本次主线和 worker 使用模型别名 `gemini-3.8-flash`，辅助调用使用 `gpt-6-luna`。两版均采用相同协议、推理和权限设置；实际 wire 控制参数哈希相同，主线与 worker 的工具 schema 在两版各自的全部场景中均稳定。新版同一 branch 的缓存键保持一致，不同 worker task、auxiliary purpose 和 session 区分；上游为现有 Responses 代理，其返回的模型身份和缓存策略未独立验证，结果只代表这条连接。

复现命令（需先构建候选 `magi-daemon-app`，基线二进制应提前另存）：

```bash
node scripts/verify-prompt-cache.mjs --baseline /absolute/path/baseline/magi-daemon-app --max-calls 56
node scripts/verify-prompt-cache.mjs --live --baseline /absolute/path/baseline/magi-daemon-app --max-calls 56 --output /absolute/path/results
```

请求数上限约束整轮两版合计，包含自然辅助调用和传输重试。产物保留二进制 SHA-256、请求/工具/控制参数哈希、原始请求字节摘要、缓存键、首字节/首文本/总耗时、usage、账本核对和失败阶段，不保存完整模型请求或上游凭据。传输次数与账本逻辑调用次数分开，不要求发生重试时两者必然相同。

### 5.2 有效真实样本

最终有效对照共 43 次上游请求：旧版 22 次，新版 21 次，均返回 HTTP 200，全部进入原始用量账本。两版所有样本均报告缓存读取值（包括明确的 0）。下表按账本计算，保留失败调用已经消耗的 usage；新版数值还与执行统计的总量、模型及角色投影逐项一致。

| 角色 | 旧版调用 / 命中 | 新版调用 / 命中 | 旧版完整输入 / 缓存读取 Token | 新版完整输入 / 缓存读取 Token | Token 命中率：旧 → 新 | 请求命中率：旧 → 新 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| orchestrator | 10 / 9 | 9 / 8 | 191,424 / 146,307 | 171,390 / 130,055 | 76.43% → 75.88% | 90.00% → 88.89% |
| worker | 2 / 0 | 2 / 1 | 23,758 / 0 | 23,680 / 8,101 | 0% → 34.21% | 0% → 50.00% |
| auxiliary | 10 / 0 | 10 / 0 | 1,860 / 0 | 1,858 / 0 | 0% → 0% | 0% → 0% |
| 合计 | 22 / 9 | 21 / 9 | 217,042 / 146,307 | 196,928 / 138,156 | 67.41% → 70.16% | 40.91% → 42.86% |

两版连续对话的主线缓存读取均为 `0 → 16,256 → 16,246` Token，工具循环的两次读取及最终回答均保持命中。新版 worker 为 `0 → 8,101`，但每版只有两个 worker 请求，不能外推稳定收益。辅助输入平均约 186 Token，仍为短请求零命中，未为抬高命中率填充提示词。旧版 worker 场景中的主线发生一次 `tool_arguments_invalid` 后自我修正，新版没有该错误，双方请求数因此不同。

端到端耗时包括 daemon 调度、工具和等待；首文本耗时只记录实际可见文本，工具调用请求没有文本时保持 null：

| 场景 | 旧版耗时（秒） | 新版耗时（秒） |
| --- | --- | --- |
| 三轮连续对话，逐轮 | 4.65 / 6.38 / 6.09 | 5.14 / 5.19 / 9.28 |
| 连续对话首文本，逐轮 | 4.13 / 5.67 / 5.35 | 4.48 / 4.73 / 8.45 |
| 两次文件读取的完整 Turn | 17.91 | 21.70 |
| worker 派发至主线完成 | 65.92 | 59.90 |
| 两次提示词强化，逐次 | 12.10 / 2.25 | 12.38 / 2.78 |

当前证据证明真实缓存读取与统计链路可用，**没有证明主线命中率、TTFT 或总耗时有稳定提升**。合计比例变化受到角色构成、旧版参数错误、模型非确定输出、上游排队与缓存预热影响；固定先旧后新的顺序也不能排除交叉预热。首请求仅代表该会话的新请求，未清空 Provider 缓存，不能称为严格冷缓存。缓存键并不替代完整前缀匹配。

上游没有返回 `usage.cost`，且模型为代理别名，本次费用保持未知，未套用公开零售价格；缓存写入未报告，不将聚合的 0 解释为已证明不存在写入。

### 5.3 失败、修正与产物

前置失败/干扰样本共 70 次真实上游请求，全部排除在上表之外：第一次 48 次全局上限被耗尽，候选阶段最后的辅助请求被本地夹具拒绝；随后独立候选运行 22 次。检查发现代理把 HTTP Buffer 逐块转字符串，中文 schema 在 UTF-8 分块边界处被替换字符破坏，影响工具前缀，不能拿这两轮计算优化收益。现改为先收齐 Buffer 再解码，并核对 UTF-8 往返字节一致；受控回归增加角色工具 schema 稳定断言。此问题位于新增验收脚本，daemon 原始指纹中的工具 schema 保持一致。

账本最初在优雅关闭前读取，因批量刷盘而读到空目录；现先完成独立 daemon 关闭，再与此前 API 统计核对。异步标题/知识提取请求用有界排空代替固定 300ms 等待，失败阶段也写 `result.json`，避免只保存成功轮次。

最终真实产物为 `/tmp/magi-cache-final.NNonUQ/benchmark.json`，SHA-256 为 `d696b00819f5577369954b5a40dbb91778b8bc7ce35a366f0582b9728f8af9a7`。旧版二进制 SHA-256 为 `526b27f0fc49c312ef5dab6dd1e8409a6dd716c2ba86a37dabd00c16b8806c65`；新版为 `d436dd4fd90f6ad18d8b9b2248dd87eef360a9f1a0b043910666aeb7e0686c54`。这些临时产物不是仓库内持久附件，长期结论以本文口径和可重复入口为准。验收前后用户 daemon 的 `runtimeEpoch`、`startupNonce` 相同，未被重启。

下一步聚焦尚未覆盖的边界：在同模型/权限/工具面下扩充并交错旧新版采样，再单独验证 Skill/权限变化和压缩后的前缀差异；Anthropic 的真实命中表现与自定义 Chat 代理能力按各自协议处理。GPT Web 配置任务的真实账号工具往返仍按其独立验收文档推进，本次 HTTP 缓存结果不能替代该验收。

## 6. 模型矩阵尝试

为避免只用当前默认模型下结论，`scripts/verify-prompt-cache-models.mjs --live` 先从同一 Responses 连接读取模型目录，再逐个发送一次最小文本探测；探测通过后运行三轮短对话 smoke（主线三次请求，辅助调用使用同一模型，独立 daemon/状态根）。图片模型不进入文本/工具缓存任务，标记为跳过；探测失败、超时和 smoke 失败分开记录。`--full-model <id>` 可在 smoke 通过后追加完整的主线、worker、辅助调用对照。

本次目录共 22 项：17 个文本模型均通过探测，5 个 `gpt-image-*` 按文本任务边界跳过。GLM 首次 smoke 因后台请求收尾窗口不足失败，延长收尾时间后单独复核通过；这次复核不与初次失败样本混合。以下是各文本模型三次主线请求收到的缓存读取 Token 和按完整输入计算的 Token 命中率：

| 模型 | 三次缓存读取 Token | Token 命中率 |
| --- | --- | ---: |
| `codex-auto-review` | 0 / 16,512 / 16,512 | 63.87% |
| `deepseek-v4.1-flash-1017` | 0 / 16,384 / 16,384 | 58.16% |
| `gemini-3.8-flash` | 0 / 16,257 / 16,247 | 57.64% |
| `gemini-4-argon` | 0 / 16,281 / 16,504 | 59.06% |
| `glm-5.3-flash` | 0 / 0 / 17,920 | 32.50% |
| `gpt-5.6-luna` | 0 / 0 / 16,640 | 32.15% |
| `gpt-5.6-sol` | 16,896 / 16,640 / 16,640 | 96.93% |
| `gpt-5.6-terra` | 12,928 / 16,640 / 16,640 | 89.27% |
| `gpt-6-astra` | 0 / 16,640 / 16,640 | 64.29% |
| `gpt-6-luna` | 12,928 / 16,640 / 16,640 | 89.28% |
| `gpt-6-sol` | 12,928 / 16,640 / 16,640 | 89.29% |
| `gpt-6.1-sol` | 0 / 16,640 / 16,640 | 64.32% |
| `gpt-6.1-terra` | 12,928 / 16,512 / 0 | 56.91% |
| `gpt-reserve` | 12,928 / 0 / 16,512 | 56.94% |
| `mimo-v2.6-flash` | 0 / 14,336 / 16,384 | 52.86% |
| `qwen3.8-27b` | 0 / 14,976 / 14,976 | 53.57% |
| `sensenova-6.8-flash-lite` | 0 / 16,384 / 18,432 | 61.21% |

smoke 只用于发现“首请求建立、后续请求是否出现读取”以及协议失败，不用于模型排名。不同模型的缓存粒度、最小缓存前缀、代理路由和输出策略可能不同；单次三轮不能代表长期 P50/P95，也不能把某模型的零读取解释为模型本身没有缓存。当前完整对照结果如下：

| 模型 | 旧版主线 / worker / auxiliary Token 命中率 | 新版主线 / worker / auxiliary Token 命中率 |
| --- | --- | --- |
| `deepseek-v4.1-flash-1017` | 82.87% / 65.46% / 31.70% | 89.75% / 49.00% / 46.04% |
| `gemini-3.8-flash` | 75.29% / 0% / 0% | 81.97% / 0% / 0% |
| `gpt-6-luna` | 41.80% / 0% / 0% | 50.39% / 48.81% / 0% |

完整样本分别为 DeepSeek 44 次、Gemini 47 次、GPT 44 次上游请求；各自都完成账本与统计核对。Gemini 新版出现 26 次传输、25 次账本调用，GPT 新版为 23 / 22，传输尝试与逻辑调用不能视为同一个分母；当前核对证明账本到统计一致，尚未逐条证明所有传输尝试的计费用量均被覆盖。GPT 新版还出现一次失败调用，其 provider usage 已保留并按账本口径纳入。费用均未知，未对代理别名套价格表。

本次真实请求合计 282 次，包含 18 次探测、初轮 smoke 的 122 次尝试、三组完整对照的 135 次，以及 GLM 复核 smoke 的 7 次。首次 GLM 的失败阶段原样保留：原 15 秒收尾窗口不足，现改为 125 秒有界排空，保持与单次传输超时相符；主线内容和权限未因此改变。三轮 smoke、完整夹具回归，以及含可用、404 不可用和图片跳过分支的本地矩阵集成验收均通过，不为新增脚本重复运行全仓套件。

矩阵产物为 `/var/folders/zq/nly47zbs19g601r8k_lqh2n40000gn/T/magi-cache-model-matrix-qkxar2ia/matrix.json`，SHA-256 为 `b0c025d0e861df025cfe9b6f3fc268f149c8b7984781b64e1a638b8af3771c81`；GLM 复核产物位于同目录 `glm-settle-recheck/matrix.json`。这些是临时验收产物，不作为运行时数据源。

### 6.1 Prompt Lineage v2 复验

第一阶段接入 branch lineage、provider/model scope 和请求 epoch 后，重新对当前 Gemini Responses 连接执行 smoke + 完整候选验收。结果为 `outcome=passed`，候选完整场景 28 次传输、27 次账本逻辑调用；主线 15 次中 14 次命中，Token 命中率 77.69%，worker 2 次均为 provider 报告的 0 读取，auxiliary 10 次为 0 读取。完整结果仍与执行统计和原始账本核对一致；传输重试与逻辑调用继续分开统计。

产物为 `/tmp/magi-cache-lineage-live.G2x9UR/matrix.json`，SHA-256 为 `fcae4e21f6680bfafd7049999159c082193c1d5164e15a751f8cd5ad263dee46`。该结果证明 lineage/epoch 接入没有破坏主线真实命中；worker 和 auxiliary 的单轮零读取仍是 provider/样本行为，不能据此判断 branch key 设计失败。

## 7. 第二轮：回合边界、请求尾部与账号路由

前几节的数据只覆盖短对话，看不到“多回合、带工具”的真实会话形态。本节新增 `--tool-turns` 场景（`verify-prompt-cache.mjs` / `verify-prompt-cache-models.mjs` 的开关）：同一会话内先一轮纯对话，再一轮读取两个约 25 KB 文件的工具回合，再两轮纯对话。用请求指纹逐项比较相邻请求，找出不是“上一次请求加末尾追加”的位置。

### 7.1 找到并修复的 Magi 自身问题

| 问题 | 现象 | 处理 |
| --- | --- | --- |
| 历史边界说明夹在历史与当前输入之间 | 它不写进 thread 历史，下一回合重建时消失，上一回合从当前输入起的全部内容（含工具结果）整段错位，回合首个请求读不到这部分缓存 | 主线把它放进易变尾部（`RoundContext`） |
| 大文件读取结果的实时形态与重建形态不同 | 超过单条上限的结果，裁剪外壳丢掉 `fact_state`，且 `original_bytes` 的计算时点不同，下一回合同一条结果字节不同 | 统一经 `live_model_visible_tool_result`：先补事实标注再裁剪，外壳保留 `fact_state` |
| 引用上下文重复装配线程历史 | 近期回合（含“会话已创建/重命名”这类界面事件）和文件读取摘要每轮放在尾部，夹具里单次约 17 KB（约 5k token），永远命不中 | 主线不再装配这两类引用；子代理仍按任务装配 |
| 工具结果总预算每轮改写全部旧结果 | 超出预算后，旧结果的摘要长度随条数变化、保留边界随总量逐条移动，每轮都改写旧历史 | 降级边界只落在从头累计的块边界（约预算一半）；摘要形态只由结果自身决定；前移时整块一次 |
| Anthropic 适配器把对话中途的 developer 并进 system；滚动断点落在易变尾部 | 见第 3.2 节 | 见第 3.2 节 |
| `prompt_cache_key` 序列化进被冻结的请求意图 | 工作流待决动作与实际发送的请求不一致 | 键不参与序列化，发送时由 `StepModelClient` 附加 |
| 多账号网关看不到会话身份 | 请求被轮询到不同上游账号，各账号的缓存互不共享 | 所有 HTTP 协议的请求带 `X-Session-Id`（该分支稳定的 `lineage_id`），见 7.4 |

请求指纹新增 `prefix` 字段（`append_only`、`history_rewritten` 及第一个不同位置、`tools_changed`、`instructions_changed`、`parameters_changed`、`first_request`），不再需要手工对哈希。

### 7.2 受控夹具：相邻请求里没有被共享的字符数

`--smoke --tool-turns`，基线为已提交的 HEAD（`e1768770` 之后的构建），候选为工作区。单位是上一次请求中没有出现在下一次请求前缀里的字符数（越小越好）；差值就是该回合首个请求要重新计费、读不到缓存的部分。

| 相邻请求 | HEAD | 当前 |
| --- | ---: | ---: |
| 工具回合末 → 下一回合首个请求 | 15,987 | 817 |
| 再下一回合首个请求 | 19,612 | 2,015 |
| 再下一回合首个请求 | 19,839 | 2,422 |

剩余的 0.8–2.4 KB 是本回合的易变尾部（任务事实、步骤快照、历史边界说明），是设计上就每次重发的内容。

### 7.3 真实连接：三个模型，旧构建与新构建对照

同一场景、同一连接、先旧后新（候选可能吃到旧版留下的头部缓存），每格只有一次采样，不能当作稳定收益。数值为主线请求的 Token 命中率。

| 模型 | HEAD | 候选（含 7.1 的前五项） |
| --- | ---: | ---: |
| `deepseek-v4.1-flash-1017` | 80.7% | 89.7% |
| `gemini-3.8-flash` | 70.6% | 76.3% |
| `gpt-6-luna` | 61.9% | 71.2% |

其中 gpt 的提升含随机成分：零命中的请求从 3 次变成 2 次，剩下的零命中不是 Magi 的前缀问题（见 7.4）。

### 7.4 账号路由：GPT 类模型的随机零命中（网关轮转假设未被验证）

候选构建在目录里全部 17 个文本模型上跑同一场景（图片模型跳过，全部探测与 smoke 通过）。每个模型 9 个主线请求，含三次会话冷启动，所以比例低于稳态：

| 模型 | Token 命中率 | 模型 | Token 命中率 |
| --- | ---: | --- | ---: |
| `deepseek-v4.1-flash-1017` | 87.8% | `gemini-3.8-flash` | 76.1% |
| `mimo-v2.6-flash` | 84.6% | `gpt-5.6-sol` | 71.0% |
| `gpt-6.1-sol` / `gpt-6.1-terra` / `gpt-6-astra` / `gpt-reserve` / `gpt-6-luna` | 79.1–79.4% | `gpt-5.6-luna` | 70.9% |
| `qwen3.8-27b` | 78.5% | `gemini-4-argon` | 69.9% |
| `gpt-5.6-terra` | 77.1% | `codex-auto-review` | 68.6% |
| `gpt-6-sol` | 66.4% | `sensenova-6.8-flash-lite` | 64.8% |
| `glm-5.3-flash` | 60.4% | 合计 | 74.8% |

GPT 类模型的序列里反复出现 `16k 输入 / 0 命中` 的中间请求，前缀字节并没有变。本机网关是 CLIProxyAPI，当前配置为 `routing.strategy: round-robin`、`session-affinity: false`，请求在多个上游账号间轮转，各账号的缓存互不共享。我用同一前缀、同键、异键、无键反复探测，命中与键无关、随机出现，与这个解释一致。DeepSeek、MiMo 的命中稳定，看起来不受账号轮转影响（这是从结果推断，没有逐个核实上游）。Gemini 路由只在输入跨过约 4k 的整块时才增加缓存读取，与前缀是否稳定无关。

网关的会话亲和读取 `X-Session-ID`（其次 `prompt_cache_key` 等）。Magi 现在对所有 HTTP 请求发送 `X-Session-Id`；网关侧的 `routing.session-affinity` 是网关配置，不在本仓库内。

2026-11-01 对照实验（当前构建，`gpt-6.1-terra`，`--smoke --tool-turns`，亲和关/开交替各 5 轮，每轮主线 9 个请求，网关热重载配置，跑完恢复原值）：

| 亲和 | 各轮 Token 命中率 | 平均 | 零命中请求 |
| --- | --- | ---: | ---: |
| 开 | 78.9 / 89.4 / 78.9 / 73.3 / 89.8 | 82.1% | 4 / 45 |
| 关 | 87.1 / 87.1 / 79.3 / 81.6 / 81.6 | 83.3% | 3 / 45 |

两组没有可分辨的差别，零命中请求数也相同。因此"网关账号轮转导致随机零命中"这个解释**没有被证实**，打开亲和不是提升命中率的有效手段，上面的探测现象只能说明零命中与缓存键无关。真正原因未定位，可能是上游缓存本身的随机性或写入延迟。

对照数据里的 7 次主线零命中请求，有 6 次是一段新对话的第一个请求（它要靠上一段对话留下的静态头部），其余约 96% 的后续请求读到的正是“上一请求的历史末端”（约 16.5k token，剩下的是本轮新增内容和易变尾部）。绕开 Magi 直接探测网关得到的观察（`gpt-6.1-terra`，纯文本探针，不含 Magi 请求）：纯追加的请求序列每次都命中上一请求的整段输入（向下取整到 128 token）；同一份约 22k 头部加不同后缀的两次请求，在单条消息探针里 36 次全部零命中（同键、异键、无键结果一样），在多条消息探针里 3 组只有 1 组命中了共同头部。也就是说，对不是“上一请求整体延长”的请求，这个上游的复用并不稳定，原因未定位（路由、淘汰或粒度都有可能），也没有被证明是“只复用完整请求”。结论：新对话首个请求读不到静态头部更像上游行为而不是 Magi 的前缀问题（Magi 侧头部字节稳定，见第 14 节的核算），但原因没有定位。靠改键或头部布局无法改善；预热需要成本门禁，当前不做。

### 7.5 仍未解决或未验证

- 压缩摘要请求是一个独立的新提示，不与主对话共享前缀，压缩那一刻整段历史按未命中计费。要复用前缀需要把压缩指令追加在主对话请求之后，涉及摘要模型与质量门禁，本轮没有改。
- 模型、权限、Skill、推理强度切换仍使缓存失效一次，是明确边界，指纹只能报告“前缀变了”，不能告诉原因。
- 7.3 的采样次数少；工具结果总预算的块边界方案只有单元与夹具验证，没有在真实的超长工具循环里对照。
- 网关亲和对 `gpt-6.1-terra` 没有可测效果（见 7.4）；零命中的真正原因未定位。Anthropic 路径的真实命中未验证。


## 8. 缓存身份、断代与持久化收敛

本轮从执行边界修正了以下架构问题：

- 去掉全局 settings revision 和 binding revision 对 epoch 的影响；它们继续记录在执行快照中，无关设置保存不再轮换缓存身份。
- `ModelBridgeClient::cache_identity` 由实际发送的客户端声明协议、规范化端点、模型和推理参数，HTTP 路径与真实请求共用 renderer。API Key、端点凭据和未发送的 capability 元数据不参与身份。
- 工具目录的哈希与 transport 共用 `canonical_tool_surface`；排序和递归 schema 规范化发生在两条路径共同的入口。
- 辅助调用按明确用途区分标题、Prompt 强化、建议、知识提取和记忆提取，不因同属 Integration 阶段而共享 branch。草稿没有会话时不生成虚构的缓存/亲和身份。
- `X-Session-Id` 跟随稳定 branch lineage；Provider `prompt_cache_key` 跟随 provider/branch/epoch，避免 epoch 变化同时触发网关账号重新分配。
- `ModelStep.promptFrame.cacheBinding` 随原 canonical 事务冻结身份和断代原因，沿原 `session.turn.step` / `model_step_updated` 事件、session/read 与日志重放交付。没有第二份缓存身份存储和单独的失效写入路径。
- 每个 Step 保存固定大小的历史哈希链末端与条数，在本次请求中验证前驱前缀，区分正常追加和旧历史删改/重排/私有上下文改写。历史被截回上次改写边界也创建新 epoch；随后的追加保持该 epoch。独立辅助请求不承担追加历史合同。

单元与跨层回归验证实际连接/推理字段、无关 revision、工具等价排序、volatile 尾部、模型/端点/推理/权限/Skill/工具/renderer/压缩变化、历史改写后的追加与截回、重启重建、schema DTO 读取和 daemon 日志重放。

最终独立 daemon 夹具使用 `node scripts/verify-prompt-cache.mjs --tool-turns --max-calls 48`，核对持久化的 lineage/cache key 与实际 wire 请求及稳定分支亲和头。该夹具 usage 为合成数据，不用于宣称真实命中率提升。本轮通过 32 次 HTTP 请求与 32 次账本调用核对；17 个 canonical ModelStep 覆盖 5 条分支，5 次 initial 和 12 次 continued，没有意外断代。所有 Step 的 Provider 缓存键与实际 wire 一致，稳定分支摘要与 X-Session-Id 一致。产物为 `/tmp/magi-cache-architecture-final-20261031/candidate/result.json`，SHA-256 为 `582ff0155daf865ce643c96b766ff120e824c690d4f0e00ac1d258231b079c0f`。该目录为临时验收产物，不是运行时数据源。

本轮 58 项定向回归通过：Bridge 缓存相关 12 项、PromptLineage 7 项、ModelStep 14 项、usage 17 项、SessionStore 模型 Step 6 项、API 引导/请求冻结 1 项、daemon 日志重放 1 项。`cargo check -p magi-daemon --tests`、`npm run protocol:check`、`npm --prefix web run check`、格式与 diff 检查通过。没有运行全仓测试套件。

第 8 阶段完成缓存身份与失效边界；第 9 节完成共同正文 IR。逐 transport attempt 的实际状态/usage 归属与本地重启投影恢复见第 10 节；真实 Anthropic 和网关启用亲和后的效果未在本轮新增实测。

## 9. 共同正文 IR 与全部原生分支的尾部合同

2026-11-01 继续收敛正文装配：`magi-bridge-client::prompt_ir::compile_prompt_ir` 负责历史工具调用 ID、参数 JSON、工具 schema 和模型可见多模态结果的规范化；三种 Provider adapter 直接消费 `PromptIr`，缓存摘要通过 `PromptFrame::from_ir` 消费同一套结构。旧 `LlmMessageParams` 和字符串缓存 boundary 路径均已删除，没有为旧调用方保留另一个 renderer。`PromptIr` 不序列化为第二份持久请求正文，原始输入和 transcript 仍归 canonical history。

静态段由可信来源标记声明，正文不能生成缓存控制指令。Anthropic 分开渲染历史与尾部，再在历史中选断点；首个 RoundContext 后追加的恢复说明不会被误选为缓存尾部。静态-only 系统段同样可用原生断点。主线的当前任务引用跨 Turn 变化，放在尾部；worker 的冻结交接引用在其 task lineage 内稳定，保留为可缓存前缀。计划、邮箱和非持久历史说明对两者统一放在易变尾部，移除了只为保留 worker 旧顺序而存在的整块条件装配。IR 的独立 system_prompt 字符串入口同样删除，系统规则只来自有序消息列表。

等价工具调用 ID 的空白、参数 JSON 的空白/对象键顺序，以及 None/空工具目录，统一生成同一份正文 IR 和缓存摘要。私有 thinking/signature/Responses 上下文独立计入摘要，不改写其原始内容。生命周期影响 wire 断点，纳入稳定前缀身份；本次 renderer 版本升级为 3，旧 Step 继续读取，新请求明确断代。

本轮通过 376 项定向回归：Bridge 277、对话执行 60、ModelStep 14、lineage 9、提示词来源 15、API 引导/请求冻结 1；daemon 测试目标检查、协议生成一致性、格式与 diff 检查通过。没有运行全仓测试套件。

验收夹具还修正了两个偏差：当前请求已登记后再统计 prior，避免首个合成请求被误算为已有缓存；纯对话种子轮不再因场景标签而提前读文件。现按 canonical completed tool item 验证双文件轮确实完成两次 file_read，纯对话轮没有执行工具，产物保留逐轮 executedTools。第 7.2 节早期夹具的工具回合位置与此严格场景不同，其历史数值不作为新夹具的回归阈值；真实模式不使用这个合成模型响应器。

最终旧/新版受控对照各 32 次 HTTP 请求，均完成账本/统计核对；新版 17 个 Step、5 条分支记录 5 次 initial 和 12 次 continued，wire 缓存键与稳定亲和头逐项匹配。合成 usage 同时覆盖明确的零读取和正读取，不作为真实收益证据。产物为 `/tmp/magi-prompt-ir-qualified-final-fixture-20261101/benchmark.json`，SHA-256 为 `5c1a2062d31073035c73ea5765a01544513a01f3093377472b4743e424cd8a27`。

最终构建 SHA-256 为 `f034e723f3c11a02cecf3f669a57286fc9d69b0004b5e21fc4ffe2fa7e4519ce`。通过当前 Gemini Responses 连接运行完整真实场景，辅助模型为 gpt-6-luna：37 次传输、37 次账本调用均返回成功，22 个 Step、5 条分支为 5 次 initial / 17 次 continued。纯对话、第二轮双文件工具调用、继续对话、worker 和辅助请求均完成 canonical 工具回执与账本核对。

| 角色 | 调用 / 有缓存读取 | 完整输入 Token | 缓存读取 Token | 输入 Token 命中率 |
| --- | ---: | ---: | ---: | ---: |
| orchestrator | 20 / 19 | 407,110 | 321,027 | 78.86% |
| worker | 2 / 0 | 23,523 | 0 | 0% |
| auxiliary | 15 / 0 | 3,092 | 0 | 0% |

所有 37 次调用都由当前连接明确报告缓存读取值；上游没有返回费用，不对代理别名套公开价格。辅助输入平均约 206 Token；两个 worker 样本和短辅助请求的零读取不能证明架构失效，也不能被隐藏为“全部解决”。本次没有真实旧/新版交错对照，且模型主动增加了执行步骤工具和模型回合，不能从这一轮推断稳定收益。产物为 `/tmp/magi-prompt-ir-qualified-final-live-20261101/benchmark.json`，SHA-256 为 `54775daecb491001da73731280c48ef9a26bbf8397ff69d0325e181a9116c20c`。

本阶段另有 63 次前置真实调用：中间构建 32 次和最终构建早期验证 31 次，分别保存于 `/tmp/magi-prompt-ir-live-20261101` 与 `/tmp/magi-prompt-ir-accepted-live-20261101`；未混入上表。第 9 节最终口径只使用强化工具场景断言后的 37 次。

2026-11-01 重构后，本地受控入口通过 `npm run test:prompt-cache -- --max-calls 48`（21 次 HTTP）及 `npm run test:prompt-cache -- --tool-turns --max-calls 48`（32 次 HTTP）；两轮均完成账本、统计、canonical attempt、lineage/cache key/header 和 wire 摘要核对。两轮 usage 都是合成数据，不替代真实 Provider usage。

第 10 节已收敛请求级恢复提示生命周期，并完成逐 transport attempt 的请求摘要、状态、usage 归属、canonical/auxiliary 重启投影恢复。仍待独立验收的范围是真实 Anthropic 命中表现和网关启用亲和后的多账号效果。


## 10. 请求控制生命周期与逐次传输用量归属

2026-11-01 继续推进第 5 阶段。主线、worker 与 conversation 执行器的空响应、中断和完成补证提示改由 `RequestControls` 持有；重复恢复替换当前原因，得到可用输出后清除。它们只在发送视图末尾声明为 `RoundContext`，不再伪装成持久化用户历史。已提交的可见片段、工具证据和真实 steer 继续沿原 canonical 历史追加。探索停滞提醒由当前状态逐请求生成；运行期间新到达的 Task mailbox 也归回 Task 生命周期的尾部视图，避免后续追加的真实历史被夹在合成邮箱片段后。压缩和上下文估算继续计入这些尾部信息，不把它们混进压缩后的历史。

`ModelBridgeClient::invoke_observed` 统一非流式、流式与输出限制调用的观测合同。HTTP 发送前冻结实际序列化请求 body 的 SHA-256；每次重试/强制 tool_choice 降级都有原 Step 内的独立 attempt。`ModelAttempt.transport` 随原 canonical Step 事务保存 HTTP 状态、已报告 usage 和 transport outcome。SSE 按协议累计 usage 快照，空流与缺少 terminal 的失败仍保留已收到的用量；发送前记录失败会阻止 HTTP 请求。旧记录及非 HTTP 执行器保持未知。独立阻塞 HTTP/retry 实现已删除，普通调用同样进入共享可取消路径；daemon 的设置包装器转发该观测合同。

统计投影使用 attempt ID 作为 call/event 去重身份，原 Step 或辅助调用作为 `parentCallId`，并保存请求摘要与 HTTP 状态。主线/worker/压缩调用从已冻结的 canonical Step 投影；辅助请求使用相同 observer/reducer 的请求内回执。流式失败与同轮非流式恢复不再复用同一账本身份，取消/迟到返回也先投影已有用量再停止交付。缺少缓存 usage 的尝试不进入命中率样本。Goal 由原 Step 事务累计各 attempt 已报告的总消耗，重启和重复结算不重复计数；活动上下文继续使用最后一次成功请求的输入，不能使用多个尝试的消耗总和。

通过 459 项定向回归：Bridge 277、任务对话 60、conversation 执行 32、ModelStep 18、usage 18、请求控制 2、lineage 9、SessionStore 模型 Step 7、Goal 用量 3、Usage Authority 33。覆盖实际 HTTP 500 重试、空 SSE 后恢复、部分流失败用量、发送前记录拒绝、回执不可改写、恢复提示去重、压缩重建和账本重放去重。`cargo check -p magi-daemon --tests`、daemon 构建、协议生成一致性、Web 类型检查、格式与 diff 检查通过。没有运行全仓测试套件。

独立 daemon 最终受控夹具执行 `node scripts/verify-prompt-cache.mjs --tool-turns --max-calls 48`。32 次 HTTP 调用与 32 次账本调用逐项核对真实 wire SHA-256、HTTP 状态和输入/输出/缓存 usage，并核对 canonical attempt 的请求摘要与传输状态；同时保留 lineage/cache key/header 的交叉检查。产物为 `/tmp/magi-cache-recovery-final-20261101/benchmark.json`，SHA-256 为 `6d1b85e67f8bf3c91679a0b07b6844b1eeb28ab7e326daf4d6db39ff2d4f09ca`。夹具 usage 仍是合成数据，不是本阶段真实命中率收益证据。

本轮额外覆盖 4 个恢复层次：canonical Step 已结算、收到 transport usage 但逻辑结果未知、账本只落盘部分 attempt，以及无 Turn 的 auxiliary 请求。恢复使用冻结的 workspace/model/provider/account 脱敏上下文，不从当前设置重解析；`usage.stats.reset` 和账本保留期在事件追加锁内裁决；辅助恢复只追加 usage projection，不创建 canonical Turn。新增真实 daemon restart 回归和辅助 journal 写入失败阻断回归均通过。

第 5 阶段的投影恢复已完成：canonical Step 和独立 auxiliary 传输事实均使用冻结的脱敏模型绑定、确定性 attempt 身份和账本重置/保留期门禁恢复；恢复不创建 Turn、不重放 Provider。非流式响应头已到达但读取响应体失败时，HTTP 状态先写入 attempt。仍保留 canonical 业务提交与 usage 追加之间不是单一物理文件事务的边界。真实 Anthropic 与网关启用亲和后的效果也未在本阶段新增实测，不把这些边界标为已交付。

## 11. 2026-11-01 收口验证与清理

- `ModelBridgeClient::cache_prompt_ir` 与 HTTP renderer 现在共用工具 wire name 编码和能力降级；非 HTTP 执行器仍使用保守的通用 IR，不猜测其 wire 规则。
- Responses 流式 transport receipt 在中间 usage snapshot 后以 `response.completed` 的最终 usage 覆盖，避免隐藏推理或中间统计造成 ledger 与最终 response 分叉。
- 验收脚本对相同 wire 请求的并发重复调用改用 usage 多重集合匹配，避免仅按 SHA/HTTP 状态取第一条造成误报；仍逐项核对每个物理 attempt 的输入、输出和缓存 usage。
- 模型矩阵在 `--tool-turns --full-model` 下将完整场景上限提升到 56 次，覆盖 worker 和双文件工具回合，不因脚本上限提前拒绝有效请求。

本轮验证结果：`gpt-6-luna` 通过 probe、smoke、完整场景及带 `--tool-turns` 的完整场景；后者覆盖纯对话、双文件读取、worker、辅助调用和 canonical usage 核对。临时产物分别位于 `/var/folders/zq/nly47zbs19g601r8k_lqh2n40000gn/T/magi-cache-models-HYLmPH` 与 `/var/folders/zq/nly47zbs19g601r8k_lqh2n40000gn/T/magi-cache-models-VqHMef`，不作为运行时数据源。

当前仍未宣称真实 Anthropic 命中表现、网关 session-affinity 打开后的多账号效果或所有目录模型的完整真实矩阵已经完成；这些属于外部 Provider/网关条件，继续按各自合同单独验收。

## 12. 私有上下文与历史边界的生命周期收口

继续审计发现，第 8–11 阶段仍有一个实质性缺陷：`providerContextHash` 汇总所有私有上下文并进入基础 epoch。正常追加 reasoning/signature 也会轮换键，还会被误报为 `provider_context_changed`。此前夹具只要求“报告失效时键发生变化”，没有拒绝固定配置下的意外失效，不能证明该边界已正确实现。

本轮将私有上下文归还所在消息的生命周期，并统一消费 `PromptIr::history_end`：

- 稳定前缀中的私有上下文由 `providerContextHash` 拥有，变化明确断代。
- 历史中的私有上下文由原历史哈希链拥有；正常追加保持 epoch，改写、截断或重排旧历史才生成历史断代。
- 首个 RoundContext 及其后全部消息和私有上下文归易变尾部；后续消息即使仍标记为 Conversation，也不进入历史链。误放位置继续由 `postBoundaryHash` 诊断。
- renderer/摘要版本升为 4，旧 Step 保持原版本可读，新请求明确断代；未增加第二份持久事实或改写既有记录。

同时删除 `SettingsBackedBusinessModelBridgeClient` 转发层。它只重建环境 client，却遗漏 `cache_prompt_ir` 转发，使环境回落路径重新走通用摘要。daemon 现在直接持有 bootstrap 解析的 HTTP client，其 IR、连接身份和实际传输来自同一实现；会话/设置模型仍沿原角色解析入口。`PromptFrame::from_request` 收为测试入口，生产只能从实际 Bridge 获取 IR。缺少工具调用 ID 时，确定性派生先规范化 JSON 参数，等价空白/对象键顺序不再改变 ID 和历史前缀。

本轮 329 项相关回归通过：Bridge 280、PromptLineage 10、ModelStep 18、usage 19、daemon 环境回落/未配置入口 2。覆盖带私有上下文的正常追加、重启续写、历史改写与截回、稳定私有上下文变化、尾部恢复说明，以及环境 HTTP client 的真实工具编码和未知 Chat 缓存合同。`cargo check -p magi-daemon --tests`、daemon 构建、脚本语法、格式及 diff 检查通过；没有运行全仓测试套件。

受控夹具每次返回带 encrypted_content 的合成 reasoning item，并检查后续 wire 请求确实回放；固定配置下只允许 initial/continued，其他失效直接失败。`node scripts/verify-prompt-cache.mjs --tool-turns --max-calls 48` 通过：32 次 HTTP/账本调用，17 个 canonical Step、5 条分支，5 次 initial / 12 次 continued；主线 11 次、worker 1 次后续请求携带 reasoning 回放。wire 摘要、传输状态、usage、缓存键和分支亲和头均完成核对。产物为 `/tmp/magi-cache-lifecycle-v4-20261101/benchmark.json`，SHA-256 为 `24c771f43cd885a95375640bb9ebb668344f5ba058405a9378d39fbdeb202d24`；构建 SHA-256 为 `3ea3d28567ff2fb66e10fca716379e50be310e0fdab39c8e1b7ba6af1b2501d5`。合成 usage 只用于链路验证，不作为命中收益证据。

同一构建通过现有 Responses 连接执行 `--live --model gpt-6-luna --aux-model gpt-6-luna --smoke --tool-turns --max-calls 28`：18 次真实 HTTP/账本调用全部成功，主线 9 次、辅助 9 次；9 个 Step、2 条分支为 2 次 initial / 7 次 continued，主线有 3 次请求实际回放 reasoning。主线输入 156,292 Token、缓存读取 115,584 Token，Token 命中率 73.95%，请求命中 7/9；短辅助请求输入合计 1,858 Token、缓存读取均为 0。此轮覆盖连续对话与双文件工具回合，worker 由上述完整受控场景验证；没有新增真实 worker 或 Anthropic 验收。产物为 `/tmp/magi-cache-lifecycle-v4-gpt-live-20261101/benchmark.json`，SHA-256 为 `e566e7b5ca5957829ec3e77c3c0fcf31bb1e601cf7f1d35c42e467422af75743`。没有旧新版交错采样或已知费用，不能据此声称稳定收益、成本下降或网关亲和效果；用户原有 daemon 和设置未替换。

## 13. 最新源码真实统计（2026-11-01）

本节使用当前工作区源码重新构建的 `magi-daemon-app`，不是历史构建。二进制 SHA-256 为 `b64c9211ea0fe535df3a186cc8ad3c86c8bfd7c15fa43f68c08d3317d5a0583b`，代码源集合摘要为 `a68c489efe6ffe0e0253611b16e8fe5d3182b0adfa5cc16e96b15d97eee5c3fb`。每轮都使用独立 daemon、状态根和只读工作区，完整覆盖纯对话、双文件工具回合、worker、标题/Prompt 强化等辅助调用；每次 Provider 返回的 usage 都与原始账本、统计投影和实际 wire SHA-256 逐条核对。验收代理同时转发 daemon 生成的 `X-Session-Id`，避免测试代理改变网关路由条件。

两种可用模型各执行两轮完整场景，共 130 次真实请求，均为 HTTP 200、账本成功且缓存 usage 已报告：

| 模型/角色 | 调用 | 缓存命中调用 | 完整输入 Token | 缓存读取 Token | Token 命中率 | 请求命中率 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `gpt-6-luna` / orchestrator | 31 | 21 | 550,504 | 343,168 | 62.34% | 67.74% |
| `gpt-6-luna` / worker | 4 | 1 | 43,814 | 7,424 | 16.94% | 25.00% |
| `gpt-6-luna` / auxiliary | 60 | 0 | 13,148 | 0 | 0% | 0% |
| `deepseek-v4.1-flash-1017` / orchestrator | 30 | 30 | 602,000 | 537,728 | 89.32% | 100% |
| `deepseek-v4.1-flash-1017` / worker | 5 | 0 | 63,224 | 0 | 0% | 0% |
| 合计 | 130 | 52 | 1,272,690 | 888,320 | 69.80% | 40.00% |

角色加权结果为：orchestrator 61 次、Token 命中率 76.43%、请求命中率 83.61%；worker 9 次、Token 命中率 6.94%、请求命中率 11.11%；auxiliary 60 次、Token 命中率 0%、请求命中率 0%。auxiliary 输入平均约 219 Token，属于独立短请求，不能通过填充无意义提示词人为抬高命中率。

DeepSeek 在本次两轮中的主线结果高于 GPT；模型别名背后的 Provider/网关缓存策略未独立验证，不能据此断言 Magi 对某模型有固定收益。worker 仍只有少量样本，低值原因尚未定位，保留为待查问题，不能宣称已经解决。GPT 的两轮主线 Token 命中率分别为 56.46% 和 67.72%，DeepSeek 分别为 90.45% 和 87.82%，说明单轮波动仍然存在。

Gemini `gemini-3.8-flash` 本次未纳入完整场景统计：当前连接返回 HTTP 400，错误为 `User location is not supported for the API use`，没有缓存 usage，不能计为 miss；该失败轮单独保存在 `/tmp/magi-cache-latest-20261101/benchmark.json`。该轮额外产生 2 次传输：辅助 GPT 返回 200、输入 111 Token、读取 0，Gemini 主线返回 400、usage 未知；失败轮的调用和已有用量保留，不混入四轮完整成功场景。四轮成功结果和加权汇总保存在 `/tmp/magi-cache-latest-20261101-6pq188hd/summary.json`，SHA-256 为 `624c529c21f4a069c231589a7047fec5c544a6f56f28202fe90f2a29b3504255`。

本次结果可以证明最新代码的真实请求、缓存 usage、lineage、分支亲和和账本统计是一致的，并给出当前模型/角色的实际命中率；仍不能证明所有 Provider、所有账号路由或冷缓存条件下的长期稳定提升。GPT/DeepSeek 连接没有返回费用，未估算成本；Anthropic 和网关开启 session-affinity 后的独立效果仍需单独验收。


## 14. 历史投影、声明式改写与缓存核算（2026-11-01）

本节记录架构收敛后的实现与验证；设计见 [Prompt Lineage 与缓存根架构](prompt-cache-lineage-architecture.md) 的“历史投影唯一权威”等小节。

**发现并修复的 Magi 自身缺陷。** 实时循环每轮把工具结果总预算的输出写回 `messages`，下一轮在已降级的内容上重新计算块边界；回合边界重建则从原文计算。两者在第 12 次追加（首次超出预算）起就逐字节不同——这是旧版本里“实时与重建分叉”的同一类问题，之前只靠单测覆盖纯函数，没有覆盖这个原地叠加模式。新增一致性测试逐次追加 150 条结果（含超过单条上限的大结果），比较实时视图与重建视图，修复前该测试在第 12 次追加失败。`ContextAuthority::prepare` 同时改为返回校验后的原始序列，预算只用于压缩判定，避免把已降级的历史交给实时循环再叠加一次。

**验证。** 受控夹具 `--tool-turns` 通过（32 次 HTTP、17 个 Step、5 条分支，5 次 initial / 12 次 continued），新增两条结构不变量：只追加的 Step 其前缀字节数不收缩；易变尾部不超过请求的 35%（实测最大约 2.6–3.0%）。真实 DeepSeek（`deepseek-v4.1-flash-1017`，一次采样）：主线 14/14 请求命中，Token 命中率 90.2%；explorer 3/3，80.3%；辅助请求因输入过短（数百 token）命中率 22.4%，是结构性的。核算日志（`magi::prompt_cache`）在其中 3 个有预期的 Step 上给出：输入 53,983，实际读取 46,848（86.8%），按 Magi 保持稳定的前缀本应可复用 49,596（91.9%），易变尾部 1,497（2.8%），provider 侧缺口 2,956（5.5%）。样本很小，不能当作稳定指标。

**未做：压缩作为主线分叉。** 评估后决定不做（已实现过一版并通过单测，随后撤回），原因是它改变摘要的生成语义（历史由“不可信数据文本”变为原生角色消息，注入防护靠指令而不是形态）、需要与分块/辅助模型两条路径并存，且在本网关上无法证明收益：对不是整体延长的请求，上游复用不稳定（见 7.4）。没有可测的收益目标，不为它改变压缩这条关键路径。条件和接口约束见架构文档。

**定期检查。** `npm run report:cache-usage -- --since <改动落地日期>` 读取真实用量账本，按模型/角色与日期汇总命中率。落地前的基线（账本全量，2026-10-29 至 10-31，已按 attempt 去重）：`gpt-6-luna` orchestrator 约 25%（10-29/30 日整体 24–27%，10-31 日 68% 混有新旧构建），DeepSeek orchestrator/worker 约 85–87%，`gemini-3.8-flash` 约 75%。之后以新构建的数据为准重新比较；GPT 类在受控场景里约 80% 以上，真实使用能否达到、仍有多少差距来自上游，要靠这份报告积累的数据判断。
