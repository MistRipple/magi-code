import { LLMConfigLoader } from '../llm/config';
import {
  CATEGORY_DEFINITIONS,
  CATEGORY_RULES,
  WorkerAssignmentStorage,
} from '../orchestrator/profile';
import type {
  SettingsBootstrapPayload,
  SettingsWorkerStatusSnapshot,
} from '../shared/settings-bootstrap';

function buildCategoryAssignments(assignments: Record<string, string[]>): Record<string, string> {
  const assignmentMap: Record<string, string> = {};
  for (const [worker, categories] of Object.entries(assignments)) {
    for (const category of categories) {
      assignmentMap[category] = worker;
    }
  }
  return assignmentMap;
}

export async function buildProfileConfigSnapshot(): Promise<Record<string, unknown>> {
  const assignments = WorkerAssignmentStorage.ensureDefaults();
  const categoryGuidance: Record<string, unknown> = {};

  for (const [category, definition] of Object.entries(CATEGORY_DEFINITIONS)) {
    categoryGuidance[category] = {
      displayName: definition.displayName,
      description: definition.description,
      guidance: {
        focus: definition.guidance?.focus || [],
        constraints: definition.guidance?.constraints || [],
      },
      priority: definition.priority,
      riskLevel: definition.riskLevel,
    };
  }

  return {
    assignments: buildCategoryAssignments(assignments.assignments),
    categoryGuidance,
    categoryPriority: CATEGORY_RULES.categoryPriority,
    configPath: WorkerAssignmentStorage.getConfigPath(),
    userRules: LLMConfigLoader.loadUserRules().content || '',
  };
}

export async function buildSettingsBootstrapPayload(input: {
  mcpServers: unknown[];
  workerStatuses: SettingsWorkerStatusSnapshot;
}): Promise<SettingsBootstrapPayload> {
  const fullConfig = LLMConfigLoader.loadFullConfig();

  return {
    workerConfigs: fullConfig.workers as unknown as Record<string, unknown>,
    orchestratorConfig: fullConfig.orchestrator as unknown as Record<string, unknown>,
    auxiliaryConfig: fullConfig.auxiliary as unknown as Record<string, unknown>,
    profileConfig: await buildProfileConfigSnapshot(),
    skillsConfig: (LLMConfigLoader.loadSkillsConfig()
      || { customTools: [], instructionSkills: [], repositories: [] }) as Record<string, unknown>,
    safeguardConfig: LLMConfigLoader.loadSafeguardConfig(),
    repositories: LLMConfigLoader.loadRepositories(),
    mcpServers: input.mcpServers,
    workerStatuses: input.workerStatuses,
  };
}

