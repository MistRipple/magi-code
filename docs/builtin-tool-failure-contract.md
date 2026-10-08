# 内置工具的失败合同

内置工具的返回值是模型决定下一步的唯一依据。本文约定「工具失败时必须告诉模型什么」，
以及各层谁负责什么，避免同一个故障被多层处理、被模型误读或被静默吞掉。

## 原则

1. **失败要被看见**：请求成功但内容无用（例如搜索结果与关键词无关）、部分内容没处理（例如有目录读不了）
   都不能报告成「成功且完整」。要么返回失败，要么在成功结果里写明未处理的部分。
2. **暂态故障的重试只在工具内部发生一次**：超时、连接中断、5xx、限流由工具自己带预算地重试；
   重试之后仍失败，就把「已经重试过、用相同参数再调用不会改变结果」明确告诉模型。
   运行时对「相同工具 + 相同参数 + 相同错误码」的连续失败会止损整轮任务，模型被引导去重复调用就等于把任务送进止损。
3. **失败结果是结构化的**：`error_code`（稳定、按失败类别区分）、`error`（面向模型的类别说明）、
   `instruction`（下一步该怎么做）；失败依赖当前状态时附带该状态，模型据此重新提交，而不是凭过期记忆反复试。
4. **不暴露内部细节**：失败文本只描述类别（超时、域名无法解析、HTTP 503……），
   不带底层错误文本、内部地址或原始响应；完整的错误链只写日志。
5. **一个负责方**：同一类问题只有一层处理。新增兜底前先确认现有负责层，替换旧实现时同次删除旧代码、常量和测试。
6. **参数只认 schema 声明的一种写法**：不做别名、大小写或类型的宽松兼容（`timeoutMs`、`"true"`、`goto_definition`、
   裸 patch 文本都不再接受）。参数形状由调用前的 schema 校验负责，工具内不再各写一份兜底；
   内部调用方构造工具输入时同样使用 schema 里的键名。
7. **状态只认规范标签**：内置工具结果的 `status` 必须是 `succeeded` / `failed` / `rejected` / `needs_approval` /
   `cancelled` / `indeterminate`（`ExecutionResultStatus::from_wire_label` 是唯一映射）；缺失或不认识按失败处理并记录，
   不会默认成成功。

## 实现入口

- 失败载荷的唯一出口是 `magi_core::ToolFailure`（`crates/magi-core/src/tool_failure.rs`）：
  `error_code`（`{tool}_{类别}`）、`error`、可选 `instruction` 与附加字段。内置工具运行时、`update_plan`、
  目标工具（`get_goal` / `create_goal` / `update_goal`）都从它构造失败载荷，不各写一份。
  `crates/magi-tool-runtime/src/builtin/failure.rs` 在其上提供文件工具的分类：参数问题用 `invalid_input`，
  文件系统问题用 `filesystem_failure`，路径解析问题用 `path_resolution_failure`。
- 文件系统失败按 `io::ErrorKind` 归类，各工具共用同一组类别：`not_found`、`permission_denied`、`already_exists`、
  `not_a_directory`、`is_a_directory`、`directory_not_empty`、`storage_full`、`read_only_filesystem`、`not_utf8_text`、
  `io_failed`；每类都有对应的 `instruction`。失败载荷不带解析后的绝对路径，也不带系统错误文本。
- 工具实现按职责分在 `builtin/` 下：`files`（读、写、局部修改、目录、差异预览）、`file_transfer`（复制、移动、删除）、
  `search`、`shell`、`process`、`web`、`knowledge`、`diagram`；`fs_support` 放路径包含关系与临时路径；原子写入只有一份，
  在 `magi_core::fs_atomic::write_atomic_preserving_target`（保留符号链接目标与权限位）。

## 文件工具

- `file_read`：内容必须是 UTF-8 文本。含 NUL 或不是 UTF-8 的内容返回 `file_read_not_utf8_text`（图片提示用 `view_image`），
  不再用替换字符伪装成文本；预览被截断时只丢弃末尾被切开的字符。
- `file_write` / `file_patch` / `apply_patch`：写入走「同目录临时文件 + rename」，中途失败不会留下半截文件，
  符号链接写到真实文件，已有文件的权限位保持不变。
- `file_patch`：整批 patch 要么全部匹配并写入，要么一个都不写；失败结果区分 `no_match` / `ambiguous_match` / `not_applicable`。
- `apply_patch`：输入必须是带 `patch` 字符串字段的 JSON 对象。`Add File` 目标已存在时返回 `file_exists`
  （替换已有文件用 `Delete File` + `Add File`）；先整体在内存中匹配，写盘中途失败会把已写入的文件恢复成原样，
  结果里的 `rolled_back` / `unrestored_paths` 说明恢复情况。patch 里不带前缀的非 ASCII 行是 patch 格式错误，不会 panic。
- `file_copy`：先检查再动手——源不存在（`source_not_found`）、目标已存在且未覆盖（`already_exists`）、类型不匹配
  （`type_mismatch`）、目标等于源或落在源目录内部（`same_path` / `destination_inside_source`）都不会改动任何东西。
  目录复制到已存在目录是合并；符号链接按链接复制，不跟随。
- `file_move`：覆盖不先删目标。文件覆盖靠 rename 原子替换；目标是已存在的目录时，必须同时设置
  `overwrite=true` 和 `confirm_replace_directory=true`，否则返回 `directory_overwrite_requires_confirmation`；
  确认后旧目录先移到备份位置，新内容就位才删除，失败则恢复。跨磁盘移动是复制后删除源，
  源删除失败时明确报告「目标已写入、源仍在」。
- `file_remove`：不跟随符号链接（悬空链接、指向目录的链接按链接本身删除）；工作区根、主目录、文件系统根返回
  `protected_path`（`rejected`）；非空目录且未设置 `recursive` 返回 `directory_not_empty`。

## shell_exec 与后台进程

- 前台命令未成功时必有 `error_code` 与 `instruction`：`shell_exec_timeout`（超时，建议后台启动或调大 `timeout_ms`）、
  `shell_exec_command_not_found`、`shell_exec_nonzero_exit`；用户取消是 `cancelled`，不是失败。
  参数 `action` 只认 `run/read/write/kill/list`，不再从 `terminal_id` 推断动作。
- 前台与后台共用同一份工作目录、Shell 与缺命令预检（`resolve_shell_invocation` / `missing_executables_failure`）。
- 后台启动会观察约 0.3 秒：立刻退出的命令（命令不存在、端口占用等）返回 `shell_exec_exited_early` 与输出，
  `startup_status` 为 `failed`；仍在运行为 `confirmed`。
- 后台输出用滚动缓冲，读取结果带 `stdout_start_offset` / `stdout_next_offset` / `stdout_omitted_bytes` / `stdout_has_more`
  （stderr 同理），传 `stdout_offset` / `stderr_offset` 只读新增输出；被淘汰的输出明确报告，不静默丢弃。
- 后台进程的错误分类：`not_found`、`not_owned`、`stdin_closed`、`timeout`（写入超时）、`io_failed`、`terminate_failed`
  （没停掉的进程仍留在进程表里，不变成孤儿）。失败指引里不再出现「请稍后重试」。

## 索引与知识类工具

- `code_symbols` 的 `action` 只有 `definition` / `file_symbols`；`knowledge_query` 的 `kind` 只认 schema 枚举，`tags` 只认数组。
- 索引仍在构建（`*_index_building`）、不可用或失败时，指引模型改用 `search_text` / `file_read`，不要立即重复调用。

## Git 工具

- 错误码只有一份映射：`magi_git::GitError::code()`，REST 的 `error.kind` 与结构化 Git 工具的 `error_code` 共用，统一 `git_` 前缀。
- 命令确实执行并失败（`CommandFailed` / `Io`）是 `failed`，前置条件不满足、被规则拒绝是 `rejected`。
  stderr 里有确定特征时细分为 `git_network_unreachable`、`git_authentication_failed`、`git_push_rejected`、
  `git_remote_unavailable`；其余保持 `git_command_failed`（界面据此决定是否用 force 重试）。每类附 `instruction`。

## web_search

- 实现位于 `crates/magi-tool-runtime/src/builtin/web.rs`，分三层：HTTP 访问层（共享客户端、总预算内重试、失败分类）、
  搜索来源（`SearchSource`：只负责「关键词 → 请求地址」和「页面 → 结果」）、工具入口（调度、相关性校验、输出合同）。
- 来源按顺序尝试（Bing → Brave）：前一个失败（网络、人机验证、页面无法识别、结果不相关）才换下一个。
  请求不指定语言和地区，由来源按访问者所在区域选择市场——写死 `cc=us&setlang=en-us` 会让中文关键词返回无关页面。
- **相关性校验**：每条结果按命中关键词的占比打分（英文按词、CJK 按相邻二字组），达到三分之一算相关，
  相关结果不到五分之一就不认这次搜索，换下一个来源；全部不相关则返回 `web_search_irrelevant_results`，不是成功。
- 成功结果带 `source`；摘要里的日期前缀拆成 `published`，HTML 实体由 HTML 解析器解码。
- 失败码：`web_search_unavailable`（来源都不可用，`sources[]` 逐个说明类别和请求次数）、
  `web_search_irrelevant_results`。

## web_fetch

- 只支持 http / https；失败码形如 `web_fetch_<类别>`：`timeout`、`dns_failed`、`connect_failed`、`tls_failed`、
  `network_error`、`http_error`（带 `http_status`）、`unsupported_content`（PDF、图片等二进制）、`invalid_url`、
  `too_many_redirects`。
- 暂态 5xx / 408 / 429 / 超时 / 连接类故障最多请求 3 次；404、401、403 等不重试，`instruction` 给出对应建议。

## search_text

- 只有**搜索根本身**读不了才失败：`search_text_not_found`、`search_text_permission_denied`、`search_text_failed`。
- 子目录或文件读不了、超过 2MB、不是 UTF-8 文本，都只跳过并计入结果里的 `skipped`
  （`unreadable` / `too_large` / `non_text`），摘要里点明有多少没搜，避免把「没搜到」读成「没有」。

## update_plan

- 失败带稳定的 `error_code`（`plan_revision_conflict`、`plan_id_mismatch`、`plan_invalid_transition` ……）和 `instruction`。
- 失败取决于当前计划（版本冲突、缺 planId / itemId、非法状态转换、移除进行中步骤……）时附带 `current_plan`，
  版本冲突的指引直接写明应当使用的 `expected_revision`。

## 目标工具（get_goal / create_goal / update_goal）

- 目标状态的权威是会话目标存储；工具只做参数校验并转述存储的拒绝原因。存储以 `DomainError::GoalRejected { reason, .. }`
  （`magi_core::GoalRejection`）给出类别，工具层把它映射成 `{tool}_{reason}`：
  `already_unfinished`、`not_active`、`terminal`、`not_owned_by_turn`、`revision_conflict`、`plan_missing`、
  `plan_revision_required`、`plan_revision_conflict`、`plan_unfinished`、`plan_tasks_active`、`evidence_required`、
  `illegal_transition`、`concurrent_modification`、`no_orchestrator_thread`；参数问题是 `{tool}_invalid_input`，
  目标不存在是 `{tool}_not_found`，其余存储故障是 `{tool}_failed`。
- 每个失败都带 `instruction`，如 `control_revision` 冲突时指引先 `get_goal` 取得最新的 `goal_id`、`control_revision`、
  `plan.revision` 再提交；失败取决于当前目标或计划时附带 `goal` 与 `plan`，模型据此重新提交而不是凭记忆重试。
- 成功的 `status` 一律是规范标签 `succeeded`；创建 / 更新 / 记录阻塞这类区别放在 `outcome`
  （`created` / `updated` / `blocker_observed`），不再复用 `status`。

## 项目状态目录

- 项目记忆目录 `projects/<slug>/memory/` 在第一次写入时才创建；只打开项目不会在状态目录里留下空目录。
