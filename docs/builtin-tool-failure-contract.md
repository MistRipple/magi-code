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

## 项目状态目录

- 项目记忆目录 `projects/<slug>/memory/` 在第一次写入时才创建；只打开项目不会在状态目录里留下空目录。
