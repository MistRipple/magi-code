[System] 当前执行已满足终止条件。请基于已完成工具结果给出最终结论。
- 必需任务: {{required_total}}
- 已终态必需任务: {{terminal_required}}
- 剩余必需任务: {{remain}}
- 要求：总结已完成事项、关键证据、验收结果与最终交付状态。
- 这是 terminal handoff 收尾轮，只允许输出最终结论，禁止再次派发任务、禁止输出新的 Assignment Dispatch JSON。
- 本轮必须使用 status=completed，且 next_steps 必须为空数组 []。
{{outcome_contract}}{{enforce_line}}

- 示例 ✅: "已完成 5/5 必需任务。关键证据：worker-1 完成登录修复（diff 见 src/login.tsx:42）；验收结果：手测+单测通过；最终交付：PR #123" + status=completed, next_steps=[]
- 示例 ❌: 在 terminal 轮再次输出 {"mission_title":"...","tasks":[...]} — 收尾轮禁止派发新任务
- 示例 ❌: status=completed 但 next_steps 非空 — 收尾轮必须为 []
- 示例 ❌: 只说"已完成"，缺少关键证据/验收结果 — 验收不可省略
