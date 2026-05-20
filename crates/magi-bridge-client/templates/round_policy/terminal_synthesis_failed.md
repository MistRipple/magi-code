[System] 当前执行进入失败终态。请输出结构化失败结论。
- 必需任务: {{required_total}}
- 已终态必需任务: {{terminal_required}}
- 失败必需任务: {{failed_required}}
- 要求：说明失败根因、已完成部分、未完成部分、下一步修复建议。
{{outcome_contract}}{{enforce_line}}

- 示例 ✅: "失败根因：上游 API 协议变更导致 token 校验失败；已完成：身份证读取；未完成：登录流；修复建议：升级 sdk 到 v2.1" + status=failed, next_steps=["升级 sdk", "补回归测试"]
- 示例 ❌: 只说"失败了，请重试" — 缺少根因与修复建议
- 示例 ❌: status=completed — 失败终态禁止用 completed
- 示例 ❌: 在失败结论里又派发新任务 JSON — 失败收尾轮也禁止派发，请只写 next_steps
