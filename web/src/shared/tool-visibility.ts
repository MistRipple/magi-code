/**
 * 工具可见性分级 — 统一判定工具调用在 Thread/Worker Tab 中的可见性
 *
 * 设计原则（架构方案）：
 * - runtime_internal：编排器内部协议工具（worker_dispatch/wait/send_message/task 等），
 *   不应出现在任何 UI 面板
 * - worker_sidechain：Worker 自身调用的工具（shell/edit/search 等），
 *   仅在 Worker Tab（侧链）展示
 * - thread_visible：用户可见的工具调用（如搜索结果、文件变更摘要等），
 *   展示在主线
 *
 * 此模块替代 BaseNormalizer.USER_HIDDEN_TOOL_NAMES 静态黑名单。
 */

export type ToolVisibility = 'thread_visible' | 'runtime_internal' | 'worker_sidechain';

/**
 * 编排器内部协议工具名列表。
 * 这些工具调用产生的输出不应出现在任何用户可见的面板中。
 *
 * 该列表必须与后端 `BuiltinToolName::is_public_tool_surface()` 保持一致：
 *   - process_launch / process_read / process_write / process_kill / process_list
 *     在后端被标记为 shell_exec 的内部执行能力，不是模型可见工具；前端同样不展示。
 *   - 其余条目是编排协议工具（worker_dispatch / worker_wait / send_worker_message 等），
 *     由编排器内部使用，不面向用户。
 */
const RUNTIME_INTERNAL_TOOLS = new Set<string>([
  // 编排协议（worker lane / task graph）
  'assignment_dispatch',
  'worker_dispatch',
  'worker_wait',
  'send_worker_message',
  'sendworkermessage',
  'wait_for_workers',
  'waitforworkers',
  'worker_poll',
  'dispatch_task',
  'dispatchtask',
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
 * @param callerContext 调用者上下文（orchestrator = 编排器自身调用，worker = Worker 调用）
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

  // Worker 调用的工具 → 侧链可见
  if (callerContext === 'worker') {
    return 'worker_sidechain';
  }

  // 编排器调用的非内部工具 → 主线可见
  return 'thread_visible';
}

/**
 * 批量判断工具是否为运行时内部工具。
 * 用于替代 isInternalWorkerOrchestrationToolBlock 中的硬编码判断。
 */
export function isRuntimeInternalTool(toolName: string): boolean {
  return RUNTIME_INTERNAL_TOOLS.has(toolName.trim().toLowerCase());
}
