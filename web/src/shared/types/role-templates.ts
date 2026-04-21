/**
 * 角色模板前端类型定义
 *
 * 从 magi 原始 src/orchestrator/profile/builtin/role-templates.ts 提取前端所需子集。
 * 前端自包含版本 — 仅保留 RoleTemplate 接口定义，不包含内置模板数据。
 */

/**
 * 角色模板
 */
export interface RoleTemplate {
  templateId: string;
  displayName: string;
  description: string;
  i18n?: {
    displayNameKey: string;
    descriptionKey: string;
  };
  defaultUI: {
    colorToken: string;
    icon?: string;
  };
  profile: {
    role: string;
    focus: string[];
    constraints: string[];
    outputPreferences?: string[];
  };
  ownerships: string[];
  insightPreferences: ('decision' | 'contract' | 'risk' | 'constraint')[];
}
