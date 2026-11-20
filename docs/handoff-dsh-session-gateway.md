# 交接提示词：DSH 插件对齐 · 会话网关后续工作

把下面「提示词」整段交给接手的 agent。进度的权威记录在 `dsh-session-gateway-design.md` 第 2.5 节与 `model-call-record-persistence-design.md` 第 11 节，本文件只是入口，若与它们冲突以它们为准。

## 提示词

你接手 Magi 项目（/Users/xie/code/magi-rust-rewrite）里"DSH 插件对齐 · 会话网关"的后续工作。全程用中文沟通。

### 先读（权威事实在这里，不要凭记忆）

1. AGENTS.md（项目规则：禁止长期双实现/兼容层/兜底；改持久化、协议前先找唯一拥有者；如实报告验证）
2. docs/dsh-seam-alignment.md（决议 1–15；决议 10 DSH 合约是唯一对外插件接口，决议 11 不做兼容/补丁/部分服务，缺的事实补进 Magi 内核，不在宿主里合成）
3. docs/dsh-session-gateway-design.md（尤其第 2.5 节"剩余项与前置条件"、第 6 节保真表、第 11 节已确认取舍）
4. docs/plugin-host-protocol.md（host.session.* 与会话观察）
5. docs/model-call-record-persistence-design.md（流记录/系统提示/请求头持久化设计，第 10 节核对、第 11 节进度）

### 当前状态（全部未提交；工作区里还有其他 agent 的无关改动，保留，不要还原、不要清理，用户没让提交就不要提交）

已交付并有测试：

- 会话事实面与 `session` 网关：list/control/create/prompt/updateQueue/cancel/rename/page/projections/follow。page/follow 的分页与校验规则对齐锁定包 @deepseek-ai/dsh-api-session-controller 0.2.2-rc.3（已装进 crates/magi-extension-host/runtime/dsh 的依赖，源码在 node_modules/.../src）。
- 标题变更是带序号的会话级日志事件 session_title_changed（日志为权威，会话记录是投影；重启重放纠正落后的记录；已在真实 daemon 上验证）。
- 观察事件：user/message、turn/start|end、tool/call|result、assistant/message（文本、工具调用、usage、stream）、step/start|end。一个 canonical 事件可产生多条观察，共享 sourceSequence，历史分页只在事件边界切页。
- P1 流记录（仅 HTTP 提供方的成功尝试）：magi-bridge-client::AttemptStreamRecorder（attempt_stream.rs）把协议层原始块 LlmStreamChunk 归一成 DSH 紧凑流记录；挂在 http_model_client/streaming.rs::apply_provider_stream_event（在界面增量合并之前、按到达时间）；成功结束时发 ModelTransportEvent::StreamRecorded；conversation-runtime 的 model_step.rs 的 on_transport 拦截它并调 SessionStore::record_model_attempt_stream，写成会话事件 ModelAttemptStreamRecorded（上限 8 MiB，超限/无写入器不记，写失败只告警）；观察端用"待配对流记录"（PendingAttemptStreams，按 回合+步骤，在线与重放共用）把它放进 assistant/message.stream。已用 DSH 自己的 expandAssistantStream/assembleAssistantStream 交叉验证。

用户已确认（2026-11-19 回复"按建议"）：持久化流记录与系统提示；上限 8 MiB（流）/4 MiB（请求记录），超限放弃并告警而非截断；外部执行器步骤只记它回传的流，不记系统提示和请求头；单向升级（旧版本读不了新日志）；replace 语义的提供方没有增量流，记 complete=false/不记。子代理：保持 Magi 自己的 Thread/Task 模型，插件只读映射，不提供 subagents.prompt/interruptByUser，不允许插件干预内核。

### 下一步任务（按顺序）

**A. 失败/取消尝试的流记录与 assistant/attempt**（已分析，尚未动手）

- DSH 失败形状：finish 块 reason 为 `{kind:'error'|'aborted', failure:{message, code, status?}}`；未关闭的块不写 block-end，只写 finish。
- 做法：把 streaming_http_io 拆成外层包装 + 内层 streaming_http_io_inner(request, &mut StreamRecording)，recording 由外层持有（StreamDeltaProgress 改成持有 &mut StreamRecording，避免 clippy 的参数个数告警）；内层任何 return Err 之后，外层用 BridgeClientError 生成 failure 并发 StreamRecorded。failure.code 建议沿用现有约定：有 HTTP 状态用 provider_http_{status}（并带 status），取消（model_invocation_error_is_cancelled）用 aborted+CANCELLED，其余按 BridgeErrorLayer 给稳定码（如 MODEL_TRANSPORT_FAILED / MODEL_PROVIDER_REJECTED / MODEL_PROTOCOL_ERROR）。AttemptStreamRecorder 增加 fail(time, failure, aborted) 方法。
- 观察端：流事件的 finish.reason.kind 是 error/aborted 时，不进待配对，直接发 assistantAttempt{turn, step, stream} 观察；Node 端映射成 DSH 'assistant/attempt'（data:{turn, step, stream}，非 surface 事件，无 surfaceOp）。顺序：先于同一步骤之后的下一次尝试。
- 用 DSH 的 assembleAssistantStream 验证失败流（带未关闭块 + finish error/aborted）；更新 test/fixtures。

**B. P2 系统提示与请求头**（设计文档第 3–5 节）

新增事件 ModelRequestRecorded，每步首次提交写一次，正文按摘要去重；脱敏复用现有凭据脱敏规则；观察端发 request/header、system/message、request/context（EpochHeader 字段需对照 dsh-session 的 types.ts 逐字段映射）；外部执行器步骤不记。

**C. P3 实时助手流**

follow 的 assistant-stream 增量帧，需要 daemon 向宿主推送进行中的流块；先单独写设计再做，修订号必须连续，有界队列。

**D. 子代理只读映射（agents/subagents）与 commands**

先读设计文档 2d 与决议 4；commands 依赖 agents 句柄和 command/run|done 日志事件。

**E. 页面宿主迁移**

client-host 换成原包 @deepseek-ai/dsh-api-session-controller/client 与 ui-session，删除 client-host/session-facts.mjs 与 standard-kit.mjs 里手写的会话来源。该客户端只在提交命令行/向子代理发提示时才调用 commands/subagents 远端，挂载本身不需要这两个服务端。

### 真机验证（重要）

插件页面在 Magi 自己的右侧 tab 容器里运行，不是 Claude 桌面应用的内置浏览器。内置浏览器会拦截插件沙箱 iframe（ERR_BLOCKED_BY_CLIENT），那只是验证工具的限制，不是产品问题。真机验证要在 Magi 应用里做：这台机器上没有在运行的 Magi（只有备份 /Applications/Magi.app.previous-9895）。先向用户问清怎么启动带右侧 tab 的 Magi，再用 computer-use 申请只控制它的权限。

### 验证命令（改什么跑什么，别只靠静态检查）

- Rust：`cargo clippy --workspace --all-targets -- -D warnings`；`cargo test -p magi-session-store` / `magi-bridge-client` / `magi-conversation-runtime --lib` / `magi-daemon`。真实 npm 集成测试 fixed_original_dsh_plugin_mgr_http_business_flow 需要能访问 registry.npmjs.org，网络时通时断，超时不是代码问题，可用 MAGI_NPM_REGISTRY 指向镜像；跑完整套件时可 `-- --skip` 它。
- Node：`cd crates/magi-extension-host/runtime/dsh && npm test`（目前 106 通过 0 失败 1 个原有跳过）。
- 真实 daemon 验证：`MAGI_STATE_ROOT=<临时目录> MAGI_PORT=<空闲端口> MAGI_OPEN_BROWSER=0 target/debug/magi-daemon-app`；路由 /health、/bootstrap?scope=personal 在根路径，其余在 /api 下。

### 已知情况（别重复排查）

- magi-api 有 3 个测试在干净 HEAD 上就失败，与本工作无关：mcp_error_detail_keeps_cause_and_redacts_credentials、turn_harness::...task_profile_executes_a_tool_round_before_final_response、...task_profile_spawns_child_and_waits_for_child_result。
- 磁盘余量偏紧（约 13 GiB）；临时文件放 scratchpad，不要用 /tmp；不要用 pkill 之类的宽泛杀进程，按 PID 停自己启动的进程。
- 改了生成物源头要重新生成（protocol:generate 等），本工作没有改 App Server schema。
- 提示词/工具定义若有改动，同步改断言它的测试和设计文档（AGENTS.md「提示词与工具定义」）。

### 工作方式

- 每个子任务：先写/更新设计文档里的进度，再实现，再用真实路径（真实 HTTP 流、真实网关、真实 daemon）加单元测试验证，最后如实报告：已验证什么、没验证什么、哪些是已有失败。
- 做完一块就更新 docs/model-call-record-persistence-design.md 第 11 节和 docs/dsh-session-gateway-design.md 第 2.5 节，别让文档落后于代码。
- 遇到需要用户拍板的点（存储/脱敏/子代理事实模型变更），先问，不要自己定。
