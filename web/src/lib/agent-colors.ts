/**
 * 动态 Agent 颜色系统
 *
 * 为每个 agentId / engineId 返回对应的 CSS 颜色值。
 * 系统 agent 与常见引擎 ID 使用预设品牌色，
 * 角色模板使用 colorToken 映射色板，
 * 未知 agent 从颜色池中按名称哈希分配稳定颜色。
 */

import type { IconName } from './icons';

export interface AgentColorPair {
  /** 主色 CSS 值（如 '#d97706' 或 'var(--color-claude)'） */
  color: string;
  /** 低透明度背景色 */
  muted: string;
}

/** 系统 agent 与常见引擎 ID 的预设颜色映射（使用 CSS 变量引用） */
const KNOWN_AGENT_COLORS: Record<string, AgentColorPair> = {
  orchestrator: { color: 'var(--color-orchestrator)', muted: 'var(--color-orchestrator-muted)' },
  auxiliary: { color: 'var(--color-auxiliary)', muted: 'var(--color-auxiliary-muted)' },
  claude: { color: 'var(--color-claude)', muted: 'var(--color-claude-muted)' },
  codex: { color: 'var(--color-codex)', muted: 'var(--color-codex-muted)' },
  gemini: { color: 'var(--color-gemini)', muted: 'var(--color-gemini-muted)' },
};

/** 角色模板 colorToken → 颜色映射（可派发代理角色） */
const ROLE_COLOR_TOKENS: Record<string, AgentColorPair> = {
  'agent-executor':    { color: '#3b82f6', muted: 'rgba(59, 130, 246, 0.15)' },   // 蓝色 · 主力执行
  'agent-explorer':    { color: '#ef4444', muted: 'rgba(239, 68, 68, 0.15)' },    // 红色 · 探索/debug
  'agent-reviewer':    { color: '#10b981', muted: 'rgba(16, 185, 129, 0.15)' },   // 绿色 · 审核
  'agent-tester':      { color: '#8b5cf6', muted: 'rgba(139, 92, 246, 0.15)' },   // 紫色 · 测试
  'agent-architect':   { color: '#6366f1', muted: 'rgba(99, 102, 241, 0.15)' },   // 靛蓝色 · 架构
};

/** 动态 agent 颜色池：用于未知 agentId */
const DYNAMIC_COLOR_POOL: AgentColorPair[] = [
  { color: '#e67e22', muted: 'rgba(230, 126, 34, 0.15)' },   // 橙色
  { color: '#2ecc71', muted: 'rgba(46, 204, 113, 0.15)' },   // 绿色
  { color: '#3498db', muted: 'rgba(52, 152, 219, 0.15)' },   // 蓝色
  { color: '#9b59b6', muted: 'rgba(155, 89, 182, 0.15)' },   // 紫色
  { color: '#e74c3c', muted: 'rgba(231, 76, 60, 0.15)' },    // 红色
  { color: '#1abc9c', muted: 'rgba(26, 188, 156, 0.15)' },   // 青色
  { color: '#f39c12', muted: 'rgba(243, 156, 18, 0.15)' },   // 金色
  { color: '#e84393', muted: 'rgba(232, 67, 147, 0.15)' },   // 粉色
];

/** 简单字符串哈希 */
function hashString(str: string): number {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = ((hash << 5) - hash + str.charCodeAt(i)) | 0;
  }
  return Math.abs(hash);
}

/**
 * 获取 agent 的颜色对
 * @param agentId - agent 标识符
 * @param colorToken - 可选 colorToken（来自 RoleTemplate.defaultUI.colorToken）
 * @returns 颜色对 { color, muted }
 */
export function getAgentColor(agentId: string, colorToken?: string): AgentColorPair {
  // 优先使用 colorToken 查找角色色板
  if (colorToken && ROLE_COLOR_TOKENS[colorToken]) {
    return ROLE_COLOR_TOKENS[colorToken];
  }
  const lower = agentId.toLowerCase();
  if (KNOWN_AGENT_COLORS[lower]) {
    return KNOWN_AGENT_COLORS[lower];
  }
  // 尝试用 agentId 匹配角色 colorToken（如 executor → agent-executor）
  const inferredToken = `agent-${lower}`;
  if (ROLE_COLOR_TOKENS[inferredToken]) {
    return ROLE_COLOR_TOKENS[inferredToken];
  }
  // 按名称哈希分配颜色池中的颜色，确保同一 agentId 始终得到同一颜色
  const index = hashString(lower) % DYNAMIC_COLOR_POOL.length;
  return DYNAMIC_COLOR_POOL[index];
}

/**
 * 获取 agent 的品牌信息（颜色 + 图标 + 标签）
 * @param agentId - agent 标识符
 * @returns { colorVar, icon, label }
 */
interface AgentBrandInfo {
  colorVar: string;
  icon: IconName;
  label: string;
}

const ROLE_TEMPLATE_LABEL_KEYS: Record<string, string> = {
  executor: 'roleTemplate.executor.displayName',
  explorer: 'roleTemplate.explorer.displayName',
  reviewer: 'roleTemplate.reviewer.displayName',
  tester: 'roleTemplate.tester.displayName',
  architect: 'roleTemplate.architect.displayName',
};

const SYSTEM_LABEL_KEYS: Record<string, string> = {
  orchestrator: 'workerBadge.role.orchestrator',
  coordinator: 'workerBadge.role.orchestrator',
  auxiliary: 'workerBadge.role.auxiliary',
};

export type AgentLabelTranslator = (key: string) => string;

function toTitleLabel(agentId: string): string {
  return agentId
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ');
}

function translateLabel(
  key: string,
  fallback: string,
  translate?: AgentLabelTranslator,
): string {
  if (!translate) {
    return fallback;
  }
  const resolved = translate(key);
  return resolved && resolved !== key ? resolved : fallback;
}

export function resolveAgentDisplayLabel(
  agentId: string,
  translate?: AgentLabelTranslator,
): string {
  const lower = agentId.toLowerCase();
  const systemKey = SYSTEM_LABEL_KEYS[lower];
  if (systemKey) {
    return translateLabel(systemKey, toTitleLabel(agentId), translate);
  }

  const roleKey = ROLE_TEMPLATE_LABEL_KEYS[lower];
  if (roleKey) {
    return translateLabel(roleKey, toTitleLabel(agentId), translate);
  }

  if (lower === 'claude') return 'Claude';
  if (lower === 'codex') return 'Codex';
  if (lower === 'gemini') return 'Gemini';

  return toTitleLabel(agentId);
}

export function getAgentBrandInfo(
  agentId: string,
  translate?: AgentLabelTranslator,
): AgentBrandInfo {
  const lower = agentId.toLowerCase();
  // 系统 agent 与常见引擎 ID 的品牌映射
  const KNOWN_BRANDS: Record<string, { colorVar: string; icon: IconName; label: string }> = {
    orchestrator: { colorVar: '--color-orchestrator', icon: 'target', label: resolveAgentDisplayLabel('orchestrator', translate) },
    auxiliary: { colorVar: '--color-auxiliary', icon: 'tool', label: resolveAgentDisplayLabel('auxiliary', translate) },
    claude: { colorVar: '--color-claude', icon: 'brain', label: 'Claude' },
    codex: { colorVar: '--color-codex', icon: 'zap', label: 'Codex' },
    gemini: { colorVar: '--color-gemini', icon: 'sparkles', label: 'Gemini' },
    coordinator: { colorVar: '', icon: 'target', label: resolveAgentDisplayLabel('coordinator', translate) },
    executor: { colorVar: '', icon: 'tool', label: resolveAgentDisplayLabel('executor', translate) },
    explorer: { colorVar: '', icon: 'search', label: resolveAgentDisplayLabel('explorer', translate) },
    reviewer: { colorVar: '', icon: 'shield', label: resolveAgentDisplayLabel('reviewer', translate) },
    tester: { colorVar: '', icon: 'check-circle', label: resolveAgentDisplayLabel('tester', translate) },
    architect: { colorVar: '', icon: 'grid', label: resolveAgentDisplayLabel('architect', translate) },
  };
  if (KNOWN_BRANDS[lower]) {
    return KNOWN_BRANDS[lower];
  }
  // 动态 agent 使用首字母大写 + 机器人图标
  return {
    colorVar: '',
    icon: 'bot',
    label: resolveAgentDisplayLabel(agentId, translate),
  };
}

/**
 * 获取 agent 的统一视觉信息（品牌色 + 图标 + 标签）
 * 角色模板与引擎品牌都走这一条，避免组件各自推断颜色。
 */
export function getAgentVisualInfo(agentId: string, colorToken?: string): {
  color: string;
  muted: string;
  icon: IconName;
  label: string;
} {
  const colorPair = getAgentColor(agentId, colorToken);
  const brand = getAgentBrandInfo(agentId);
  return {
    color: colorPair.color,
    muted: colorPair.muted,
    icon: brand.icon,
    label: brand.label,
  };
}
