[System] 你已在未建立任务轨道下连续执行 {{no_task_tool_round_streak}} 轮工具调用（重复模式 {{repeated_signature_streak}} 轮）。
- 下一轮已强制禁用工具，请直接二选一：
   1) 给出最终结论与证据；
   2) 立即输出结构化 Assignment Dispatch JSON 建立必需任务轨道后再继续。
- 不要继续重复检索。

- 示例 ✅（收尾）: "基于已有 N 次工具结果，结论是 X，关键证据见 worker-1 输出第 12 行" + OUTCOME 控制块
- 示例 ✅（派发）: 立即输出 {"mission_title":"...","tasks":[{"task_name":"...", ...}]}
- 示例 ❌: 又一次 grep/read_file/web_search — 下一轮工具已强制禁用，调用会失败
- 示例 ❌: "我需要再看一下" + 重复同样模式的探索 — 视为无效循环
