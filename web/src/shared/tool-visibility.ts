/**
 * 工具可见性分级 —— 统一判定工具调用是否属于运行时内部协议
 *
 * 设计原则（架构方案）：
 * - agent_spawn：代理创建调用本身是主线可见的 ToolCall 卡片，不能归入内部工具
 *
 * 此模块替代 BaseNormalizer.USER_HIDDEN_TOOL_NAMES 静态黑名单。
 */

/**
 * 编排器内部协议工具名列表。
 * 这些工具调用产生的输出不应出现在任何用户可见的面板中。
 *
 * 该列表是前端展示层约束：有些工具需要暴露给模型，但不应成为用户时间线卡片。
 * 后端 `BuiltinToolName::is_public_tool_surface()` 只决定模型可见工具 schema：
 *   - process_launch / process_read / process_write / process_kill / process_list
 *     在后端被标记为 shell_exec 的内部执行能力，不是模型可见工具；前端同样不展示。
 */
const RUNTIME_INTERNAL_TOOLS = new Set<string>([
  // 旧编排控制与状态类工具已经退出模型可见面；这里仅保留仍存在的内部控制项。
  'task_status',
  'context_window_status',
  'instruction_skill',
  'report_progress',
  'task_complete',
  'task_failed',
  'escalate',
  'request_clarification',
  'submit_review',
  'read_instructions',
  'governance_handshake',
  // 现役任务状态工具不进入用户可见时间线。
  'agent_wait',
  'update_plan',
  'memory_write',
  // 后端 builtin shell 内部能力（模型不可调用，仅在 shell_exec 内部走子进程协议）
  'process_launch',
  'process_read',
  'process_write',
  'process_kill',
  'process_list',
]);

/**
 * 批量判断工具是否为运行时内部工具。
 * 用于替代旧编排内部工具黑名单中的硬编码判断。
 */
export function isRuntimeInternalTool(toolName: string): boolean {
  return RUNTIME_INTERNAL_TOOLS.has(toolName.trim().toLowerCase());
}
