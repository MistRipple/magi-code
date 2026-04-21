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

export interface SettingsRuntimeSnapshot {
  locale: string;
  deepTask: boolean;
}

export interface SettingsBootstrapPayload {
  workerConfigs: Record<string, unknown>;
  orchestratorConfig: Record<string, unknown>;
  auxiliaryConfig: Record<string, unknown>;
  userRulesConfig: Record<string, unknown>;
  skillsConfig: Record<string, unknown>;
  safeguardConfig: Record<string, unknown>;
  repositories: unknown[];
  mcpServers: unknown[];
  workerStatuses: SettingsWorkerStatusSnapshot;
  runtimeSettings: SettingsRuntimeSnapshot;
  roleTemplates?: RoleTemplate[];
  registryEngines?: ModelEngine[];
  registryAgents?: AgentBinding[];
  bootstrapScope?: 'core' | 'full';
  mcpServersHydrated?: boolean;
}

export type SettingsBootstrapSnapshot = SettingsBootstrapPayload;
