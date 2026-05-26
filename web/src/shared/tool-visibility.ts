/**
 * 工具可见性分级 — 统一判定工具调用在主线 / 代理详情中的可见性
 *
 * 设计原则（架构方案）：
 * - runtime_internal：编排协议工具，不应出现在任何 UI 面板
 * - agent_spawn：代理创建调用本身是主线可见的 ToolCall 卡片，不能归入内部工具
 * - agent_sidechain：代理自身调用的工具（shell/edit/search 等），
 *   仅在代理详情展示
 * - thread_visible：用户可见的工具调用（如搜索结果、文件变更摘要等），
 *   展示在主线
 *
 * 此模块替代 BaseNormalizer.USER_HIDDEN_TOOL_NAMES 静态黑名单。
 */

export type ToolVisibility = 'thread_visible' | 'runtime_internal' | 'agent_sidechain';

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
  // Task System v2 协调 / 长任务工具：这些是模型侧内部协议，不进入用户可见时间线。
  // agent_spawn 除外：它是父代理主线上的代理创建 ToolCall 卡片。
  'agent',
  'spawn_agent',
  'agent_wait',
  'wait_agent',
  'todo_write',
  'todowrite',
  'todo',
  'memory_write',
  'memorywrite',
  'memory',
  'mission_charter_write',
  'missioncharterwrite',
  'mission_charter',
  'plan_write',
  'planwrite',
  'plan',
  'kg_write',
  'kgwrite',
  'knowledge_write',
  'knowledge_graph_write',
  'validation_record',
  'validationrecord',
  'validation_write',
  'validation',
  'checkpoint_create',
  'checkpoint',
  'snapshot',
  'human_checkpoint_request',
  'human_checkpoint',
  'human_review',
  // 后端 builtin shell 内部能力（模型不可调用，仅在 shell_exec 内部走子进程协议）
  'process_launch',
  'process_read',
  'process_write',
  'process_kill',
  'process_list',
]);

/**
 * 解析工具调用的可见性级别。
 *
* @param toolName 工具名称
 * @param callerContext 调用者上下文（orchestrator = 编排器自身调用，worker = 代理调用）
 */
export function resolveToolVisibility(
  toolName: string,
  callerContext: 'orchestrator' | 'worker',
): ToolVisibility {
  const normalizedName = toolName.trim().toLowerCase();

  // 编排器内部协议工具，无论谁调用都不可见
  if (RUNTIME_INTERNAL_TOOLS.has(normalizedName)) {
    return 'runtime_internal';
  }

  // 代理调用的工具 → 详情页可见
  if (callerContext === 'worker') {
    return 'agent_sidechain';
  }

  // 编排器调用的非内部工具 → 主线可见
  return 'thread_visible';
}

/**
 * 批量判断工具是否为运行时内部工具。
 * 用于替代旧编排内部工具黑名单中的硬编码判断。
 */
export function isRuntimeInternalTool(toolName: string): boolean {
  return RUNTIME_INTERNAL_TOOLS.has(toolName.trim().toLowerCase());
}
