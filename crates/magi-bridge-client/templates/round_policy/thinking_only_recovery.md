[System] 你刚才只输出了 thinking，没有正文，也没有任何可执行的 Assignment 派发 JSON。
- 不要只在 thinking 里规划任务。
- 如果本轮需要任务编排，现在立刻输出结构化 Assignment Dispatch JSON，唯一合法形状：{ mission_title?: string, tasks: [...] }。
- 每个 tasks[*] 必须包含 task_name、ownership_hint、mode_hint、goal、acceptance、constraints、context、requires_modification。
- 禁止使用 legacy 字段 category、description，禁止把 ownership_hint/mode_hint/goal 放到顶层。
- 如果你判断当前无法形成有效 Assignment，请直接用正文说明原因。

- 示例 ✅: thinking 内规划，**同时** 正文 emit 完整 {"mission_title":"...","tasks":[...]} JSON
- 示例 ✅: 无法派发 → 正文直接说明原因（如"信息不足，请提供登录入口文件路径"）
- 示例 ❌: thinking 写了"我会派 worker-A 处理登录" 而正文为空 — 派发不会发生
- 示例 ❌: 仅在 thinking 里贴 JSON 而正文为空 — 仅识别正文中的 JSON
