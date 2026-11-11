import type { RoleTemplate } from './types/role-templates';

/** `agent_role_save` 的输入草稿：对话里只展示“要保存什么”，不展示原始 JSON。 */
export interface AgentRoleDraft {
  roleId: string;
  displayName: string;
  description: string;
  capabilities: string[];
  focus: string[];
  constraints: string[];
  outputPreferences: string[];
  ownerships: string[];
  parallelismLimit: number | null;
  systemPrompt: string;
  /** 修改已有角色时的版本号；新建为 null。 */
  expectedRevision: number | null;
}

export interface AgentRoleSaveResult {
  mode: 'created' | 'updated' | 'unchanged';
  revision: number | null;
}

function record(value: unknown): Record<string, unknown> | null {
  let parsed = value;
  if (typeof parsed === 'string') {
    try {
      parsed = JSON.parse(parsed);
    } catch {
      return null;
    }
  }
  return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
    ? parsed as Record<string, unknown>
    : null;
}

function text(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function list(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string').map((item) => item.trim()).filter(Boolean)
    : [];
}

function positiveInteger(value: unknown): number | null {
  return typeof value === 'number' && Number.isInteger(value) && value > 0 ? value : null;
}

export function parseAgentRoleDraft(input: unknown): AgentRoleDraft | null {
  const value = record(input);
  if (!value) return null;
  const roleId = text(value.role_id);
  if (!roleId) return null;
  return {
    roleId,
    displayName: text(value.display_name),
    description: text(value.description),
    capabilities: list(value.capabilities),
    focus: list(value.focus),
    constraints: list(value.constraints),
    outputPreferences: list(value.output_preferences),
    ownerships: list(value.ownerships),
    parallelismLimit: positiveInteger(value.parallelism_limit),
    systemPrompt: text(value.system_prompt),
    expectedRevision: positiveInteger(value.expected_role_revision),
  };
}

/** 保存成功的结果；失败、拒绝和等待授权返回 null，仍由通用错误与授权区域展示。 */
export function parseAgentRoleSaveResult(output: unknown): AgentRoleSaveResult | null {
  const value = record(output);
  if (!value || value.status !== 'ok') return null;
  const mode = value.mode;
  if (mode !== 'created' && mode !== 'updated' && mode !== 'unchanged') return null;
  return { mode, revision: positiveInteger(value.role_revision) };
}

/** 专业能力 id → 显示名：取自已加载的全部角色模板，缺失时调用方回退到 id。 */
export function agentRoleCapabilityLabels(templates: readonly RoleTemplate[] | undefined): Map<string, string> {
  const labels = new Map<string, string>();
  for (const template of templates ?? []) {
    for (const capability of template.capabilities ?? []) {
      if (capability.id && capability.displayName && !labels.has(capability.id)) {
        labels.set(capability.id, capability.displayName);
      }
    }
  }
  return labels;
}
