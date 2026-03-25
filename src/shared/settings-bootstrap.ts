import type { LocaleCode } from '../i18n/types';

export type SettingsWorkerStatusSnapshot = Record<string, {
  status: string;
  model?: string;
  error?: string;
}>;

export type SettingsWorkerStatusMap = SettingsWorkerStatusSnapshot;

export interface SettingsBootstrapPayload {
  workerConfigs: Record<string, unknown>;
  orchestratorConfig: Record<string, unknown>;
  auxiliaryConfig: Record<string, unknown>;
  profileConfig: Record<string, unknown>;
  skillsConfig: Record<string, unknown>;
  repositories: unknown[];
  mcpServers: unknown[];
  workerStatuses: SettingsWorkerStatusSnapshot;
}

export interface SettingsRuntimeSnapshot {
  locale: LocaleCode;
  deepTask: boolean;
}

export interface SettingsBootstrapSnapshot extends SettingsBootstrapPayload {
  runtimeSettings: SettingsRuntimeSnapshot;
}

export function buildSettingsBootstrapSnapshot(
  payload: SettingsBootstrapPayload,
  runtimeSettings: SettingsRuntimeSnapshot,
): SettingsBootstrapSnapshot {
  return {
    ...payload,
    runtimeSettings,
  };
}

export function cloneSettingsWorkerStatusMap(
  statuses: SettingsWorkerStatusMap,
): SettingsWorkerStatusMap {
  return Object.fromEntries(
    Object.entries(statuses).map(([worker, status]) => [
      worker,
      { ...status },
    ]),
  );
}
