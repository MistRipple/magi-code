/**
 * Settings Bootstrap 类型定义
 *
 * 前端自包含版本 — 仅保留 web-client-bridge.ts 所需的类型。
 */

import type { RoleTemplate } from './types/role-templates';
import type { AgentBinding, ModelEngine } from './types/registry-types';

export type SettingsWorkerStatusSnapshot = Record<string, {
  status: string;
  model?: string;
  error?: string;
}>;

export type SettingsWorkerStatusMap = SettingsWorkerStatusSnapshot;

export interface SettingsBuiltinTool {
  name: string;
  riskLevel?: string;
  approvalRequirement?: string;
  accessMode?: 'read_only' | 'maybe_write' | 'explicit_write' | string;
  runtimeStatus?: string;
  runtimeWarnings?: string[];
  schemaStatus?: string;
  schemaWarnings?: string[];
  enabled?: boolean;
}

export interface SettingsCapabilityDependency {
  name: string;
  status: string;
  requiredBy: string[];
  workspaceId?: string | null;
  sessionId?: string | null;
  fileCount?: number | null;
  lastIndexed?: number | null;
  roleCount?: number | null;
  spawnableRoleCount?: number | null;
  snapshotActive?: boolean | null;
  configuredCount?: number | null;
  enabledCount?: number | null;
  readyCount?: number | null;
  toolCount?: number | null;
}

export interface SettingsRuntimeSnapshot {
  locale: string;
}

export interface SettingsBootstrapPayload {
  workspaceId?: string | null;
  workspacePath?: string | null;
  workerConfigs: Record<string, unknown>;
  orchestratorConfig: Record<string, unknown>;
  auxiliaryConfig: Record<string, unknown>;
  userRulesConfig: Record<string, unknown>;
  skillsConfig: Record<string, unknown>;
  safeguardConfig: Record<string, unknown>;
  repositories: unknown[];
  mcpServers: unknown[];
  builtinTools?: SettingsBuiltinTool[];
  capabilityDependencies?: SettingsCapabilityDependency[];
  workerStatuses: SettingsWorkerStatusSnapshot;
  runtimeSettings: SettingsRuntimeSnapshot;
  roleTemplates?: RoleTemplate[];
  registryEngines?: ModelEngine[];
  registryAgents?: AgentBinding[];
  bootstrapScope?: 'core' | 'full';
  mcpServersHydrated?: boolean;
}

export type SettingsBootstrapSnapshot = SettingsBootstrapPayload;
