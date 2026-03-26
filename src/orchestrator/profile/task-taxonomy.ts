/**
 * Task Taxonomy — ownership × mode 双轴类型系统
 *
 * 设计原则：
 * - ownership 决定"谁来执行"（Worker 路由的唯一依据）
 * - mode 决定"怎样执行"（执行行为约束，不影响 Worker 选择）
 * - 两轴正交，不存在交叉语义
 *
 * 旧 category 字段身兼二职（ownership 路由 + 执行方式标记），
 * 当用户说"给前端补个测试"时 category 无法同时表达 frontend 和 test。
 * 双轴模型下：ownership=frontend, mode=test → 路由到 frontend worker，以测试模式执行。
 */

// ============================================================================
// Ownership 轴：决定 Worker 路由
// ============================================================================

/**
 * 任务归属域——决定由哪个 Worker 执行。
 *
 * - frontend: 前端 UI/组件/样式/交互
 * - backend: 后端 API/数据库/服务端逻辑
 * - integration: 跨端联调/集成测试/端到端
 * - data_analysis: 数据分析/统计/可视化
 * - general: 无法归类到特定域的通用任务
 */
export const TASK_OWNERSHIPS = [
  'frontend',
  'backend',
  'integration',
  'data_analysis',
  'general',
] as const;

export type TaskOwnership = typeof TASK_OWNERSHIPS[number];

export const DEFAULT_OWNERSHIP: TaskOwnership = 'general';

/** 需要跨域拆分的边界域（frontend + backend 同时出现时必须拆为多个任务） */
export const OWNERSHIP_SPLIT_BOUNDARY: ReadonlySet<TaskOwnership> = new Set<TaskOwnership>([
  'frontend',
  'backend',
  'integration',
]);

// ============================================================================
// Mode 轴：决定执行行为
// ============================================================================

/**
 * 任务执行模式——约束 Worker 的执行行为，不影响路由。
 *
 * - implement: 功能实现（默认）
 * - test: 编写/修复测试
 * - document: 编写/更新文档
 * - review: 代码审查
 * - debug: 调试/排查问题
 * - refactor: 重构
 * - architecture: 架构设计/分析
 */
export const TASK_MODES = [
  'implement',
  'test',
  'document',
  'review',
  'debug',
  'refactor',
  'architecture',
] as const;

export type TaskMode = typeof TASK_MODES[number];

export const DEFAULT_MODE: TaskMode = 'implement';

// ============================================================================
// Mode 行为约束定义
// ============================================================================

/**
 * Mode 约束配置——定义每种模式对 Worker 行为的限制
 */
export interface ModeConstraints {
  /** 模式描述 */
  description: string;
  /** 文件修改约束（正则表达式，匹配允许修改的文件路径） */
  allowedFilePatterns?: RegExp[];
  /** 禁止修改的文件模式 */
  forbiddenFilePatterns?: RegExp[];
  /** 行为约束（注入到 Worker prompt） */
  behavioralConstraints: string[];
  /** 是否只读模式 */
  readOnly?: boolean;
  /** 工具使用约束（允许的工具列表，空表示全部允许） */
  allowedTools?: string[];
  /** 禁止使用的工具 */
  forbiddenTools?: string[];
}

export const MODE_CONSTRAINTS: Record<TaskMode, ModeConstraints> = {
  implement: {
    description: '功能实现模式',
    behavioralConstraints: [
      '专注于功能实现',
      '遵循项目代码规范',
      '确保代码可测试',
    ],
  },
  test: {
    description: '测试编写模式',
    allowedFilePatterns: [
      /\.test\.ts$/,
      /\.spec\.ts$/,
      /__tests__\//,
      /test\//,
      /tests\//,
    ],
    behavioralConstraints: [
      '只修改测试文件',
      '不修改生产代码（除非修复测试中发现的 bug）',
      '确保测试覆盖率',
      '使用项目约定的测试框架',
    ],
  },
  document: {
    description: '文档编写模式',
    allowedFilePatterns: [
      /\.md$/,
      /README/,
      /CHANGELOG/,
      /docs\//,
      /documentation\//,
    ],
    behavioralConstraints: [
      '只修改文档文件',
      '使用清晰简洁的语言',
      '保持文档与代码同步',
    ],
  },
  review: {
    description: '代码审查模式',
    readOnly: true,
    behavioralConstraints: [
      '只读模式，不修改任何文件',
      '关注代码质量、安全性、性能',
      '提供具体的改进建议',
      '不改变代码逻辑',
    ],
  },
  debug: {
    description: '调试排查模式',
    behavioralConstraints: [
      '专注于问题定位和修复',
      '添加必要的调试日志',
      '修复后清理调试代码',
      '记录问题原因和解决方案',
    ],
  },
  refactor: {
    description: '重构优化模式',
    behavioralConstraints: [
      '保持功能不变',
      '改善代码结构和可读性',
      '不改变外部接口',
      '确保现有测试通过',
    ],
  },
  architecture: {
    description: '架构设计模式',
    behavioralConstraints: [
      '关注系统整体架构',
      '考虑可扩展性和可维护性',
      '提供架构决策文档',
      '不直接修改实现代码',
    ],
  },
};

/** 获取指定模式的约束 */
export function getModeConstraints(mode: TaskMode): ModeConstraints {
  return MODE_CONSTRAINTS[mode] || MODE_CONSTRAINTS.implement;
}

// ============================================================================
// 双轴组合
// ============================================================================

/** 任务分类（编译结果） */
export interface TaskClassification {
  ownership: TaskOwnership;
  mode: TaskMode;
}

// ============================================================================
// 类型守卫
// ============================================================================

const OWNERSHIP_SET: ReadonlySet<string> = new Set(TASK_OWNERSHIPS);
const MODE_SET: ReadonlySet<string> = new Set(TASK_MODES);

export function isValidOwnership(value: string): value is TaskOwnership {
  return OWNERSHIP_SET.has(value);
}

export function isValidMode(value: string): value is TaskMode {
  return MODE_SET.has(value);
}

