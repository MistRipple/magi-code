[System] 你刚才在正文里描述了内部 worker dispatch/wait，但没有输出可执行的结构化派发 JSON。
- 不要再用自然语言重复内部 worker dispatch/wait 工具名或执行细节。
- 如果你决定派发任务，现在立刻输出结构化 Assignment Dispatch JSON，唯一合法形状：{ mission_title?: string, tasks: [...] }。
- 每个 tasks[*] 必须包含 task_name、ownership_hint、mode_hint、goal、acceptance、constraints、context、requires_modification。
- 禁止使用 legacy 字段 category、description，禁止把 ownership_hint/mode_hint/goal 放到顶层。
- 如果当前不应该派发任务，请直接说明原因并停止提及工具名。

- 示例 ✅: {"mission_title":"修复登录","tasks":[{"task_name":"...","ownership_hint":"...","mode_hint":"...","goal":"...","acceptance":"...","constraints":"...","context":"...","requires_modification":true}]}
- 示例 ❌: "我会派一个 worker 去检查 src/login.tsx 文件，等它完成后再继续" — 这只是自然语言描述，派发不会发生
- 示例 ❌: {"category":"code","description":"..."} — legacy 字段会被拒绝
- 示例 ❌: 把 ownership_hint/mode_hint/goal 直接放在 JSON 顶层而不是 tasks[*] 内 — 形状不合法
