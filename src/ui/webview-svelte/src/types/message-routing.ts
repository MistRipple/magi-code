export enum MessageCategory {
  // =========== 主对话区消息 ===========
  USER_INPUT = 'user_input',
  ORCHESTRATOR_ANALYSIS = 'orchestrator_analysis',
  ORCHESTRATOR_PLAN = 'orchestrator_plan',
  ORCHESTRATOR_SUMMARY = 'orchestrator_summary',
  ORCHESTRATOR_THINKING = 'orchestrator_thinking',

  // =========== Worker 面板消息 ===========
  WORKER_INSTRUCTION = 'worker_instruction',
  WORKER_THINKING = 'worker_thinking',
  WORKER_OUTPUT = 'worker_output',
  WORKER_TOOL_USE = 'worker_tool_use',
  WORKER_CODE = 'worker_code',

  // =========== 系统消息 ===========
  SYSTEM_NOTICE = 'system_notice',
  SYSTEM_PHASE = 'system_phase',
  SYSTEM_ERROR = 'system_error',

  // =========== 交互消息 ===========
  INTERACTION_CONFIRMATION = 'interaction_confirmation',
  INTERACTION_QUESTION = 'interaction_question',
  INTERACTION_TOOL_AUTH = 'interaction_tool_auth',

  // =========== 特殊消息 ===========
  TASK_SUMMARY_CARD = 'task_summary_card',
  PROGRESS_UPDATE = 'progress_update',
}

export type DisplayTarget =
  | { location: 'thread' }
  | { location: 'worker'; worker: 'claude' | 'codex' | 'gemini' }
  | { location: 'both'; worker: 'claude' | 'codex' | 'gemini' }
  | { location: 'task' }
  | { location: 'none' };
