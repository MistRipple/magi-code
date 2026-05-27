export interface AgentRoleSource {
  templateId: string;
  displayName: string;
  displayNameKey?: string;
  engineId?: string;
  colorToken?: string;
}

export interface AgentRoleRegistrySnapshot {
  roleTemplates?: Array<{
    templateId: string;
    displayName: string;
    i18n?: {
      displayNameKey?: string;
    };
    defaultUI?: {
      colorToken?: string;
    };
  }>;
  registryAgents?: Array<{
    templateId: string;
    /** engineId 空串 = 继承编排模型，非空 = 显式绑定到 engine */
    engineId: string;
    order: number;
  }>;
}

export type AgentRoleTranslator = (key: string) => string;

const BUILTIN_ROLE_TEMPLATE_IDS = [
  'executor',
  'explorer',
  'reviewer',
  'tester',
  'architect',
];

function normalizeAgentId(value: unknown): string {
  if (typeof value !== 'string') return '';
  return value.trim();
}

function resolveRoleTemplateDisplayNameKey(templateId: string): string {
  const normalizedTemplateId = normalizeAgentId(templateId);
  return normalizedTemplateId ? `roleTemplate.${normalizedTemplateId}.displayName` : '';
}

function resolveTemplateDisplayMeta(
  agentId: string,
  registrySnapshot?: AgentRoleRegistrySnapshot | null,
): { displayName?: string; displayNameKey?: string; colorToken?: string } {
  const roleTemplates = Array.isArray(registrySnapshot?.roleTemplates)
    ? registrySnapshot.roleTemplates
    : [];
  const matchedTemplate = roleTemplates.find((template) => normalizeAgentId(template.templateId) === agentId);
  const templateId = normalizeAgentId(matchedTemplate?.templateId) || agentId;
  return {
    displayName: normalizeAgentId(matchedTemplate?.displayName) || undefined,
    displayNameKey: normalizeAgentId(matchedTemplate?.i18n?.displayNameKey) || resolveRoleTemplateDisplayNameKey(templateId) || undefined,
    colorToken: normalizeAgentId(matchedTemplate?.defaultUI?.colorToken) || undefined,
  };
}

function collectKnownRoleTemplateIds(registrySnapshot?: AgentRoleRegistrySnapshot | null): string[] {
  const roleTemplates = Array.isArray(registrySnapshot?.roleTemplates)
    ? registrySnapshot.roleTemplates
    : [];
  return Array.from(new Set([
    ...roleTemplates.map((template) => normalizeAgentId(template.templateId)).filter(Boolean),
    ...BUILTIN_ROLE_TEMPLATE_IDS,
  ])).sort((left, right) => right.length - left.length);
}

function resolveRuntimeAgentTemplateId(
  agentId: string,
  registrySnapshot?: AgentRoleRegistrySnapshot | null,
): string {
  const normalizedAgentId = normalizeAgentId(agentId);
  if (!normalizedAgentId) {
    return '';
  }
  for (const templateId of collectKnownRoleTemplateIds(registrySnapshot)) {
    if (normalizedAgentId === templateId || normalizedAgentId === `task-worker-${templateId}`) {
      return templateId;
    }
  }
  return '';
}

function resolveAgentRoleSource(
  agentId: string,
  enabledAgents: ReadonlyArray<AgentRoleSource>,
  registrySnapshot?: AgentRoleRegistrySnapshot | null,
): AgentRoleSource | null {
  const normalizedAgentId = normalizeAgentId(agentId);
  if (!normalizedAgentId) {
    return null;
  }
  const normalizedRoleId = resolveRuntimeAgentTemplateId(normalizedAgentId, registrySnapshot);
  const lookupAgentId = normalizedRoleId || normalizedAgentId;

  const matchedEnabledAgent = enabledAgents.find((agent) => {
    const templateId = normalizeAgentId(agent.templateId);
    const engineId = normalizeAgentId(agent.engineId);
    return templateId === lookupAgentId || engineId === lookupAgentId;
  });
  if (matchedEnabledAgent) {
    const templateMeta = resolveTemplateDisplayMeta(
      normalizeAgentId(matchedEnabledAgent.templateId) || normalizedAgentId,
      registrySnapshot,
    );
    return {
      ...matchedEnabledAgent,
      ...(templateMeta.displayNameKey ? { displayNameKey: templateMeta.displayNameKey } : {}),
      ...(templateMeta.displayName ? { displayName: templateMeta.displayName } : {}),
      ...(templateMeta.colorToken ? { colorToken: templateMeta.colorToken } : {}),
    };
  }

  const registryAgents = Array.isArray(registrySnapshot?.registryAgents)
    ? registrySnapshot.registryAgents
    : [];
  const matchedBinding = registryAgents.find((binding) => {
    const templateId = normalizeAgentId(binding.templateId);
    const engineId = normalizeAgentId(binding.engineId);
    return templateId === lookupAgentId || engineId === lookupAgentId;
  });
  if (!matchedBinding) {
    const templateMeta = resolveTemplateDisplayMeta(lookupAgentId, registrySnapshot);
    if (normalizedRoleId || templateMeta.displayName || templateMeta.colorToken) {
      return {
        templateId: lookupAgentId,
        displayName: templateMeta.displayName || lookupAgentId,
        displayNameKey: templateMeta.displayNameKey,
        colorToken: templateMeta.colorToken,
      };
    }
    return null;
  }

  const templateMeta = resolveTemplateDisplayMeta(normalizeAgentId(matchedBinding.templateId), registrySnapshot);
  return {
    templateId: normalizeAgentId(matchedBinding.templateId),
    displayName: templateMeta.displayName || normalizeAgentId(matchedBinding.templateId),
    displayNameKey: templateMeta.displayNameKey,
    engineId: normalizeAgentId(matchedBinding.engineId) || undefined,
    colorToken: templateMeta.colorToken,
  };
}

export function resolveAgentDisplayName(
  agentId: string,
  enabledAgents: ReadonlyArray<AgentRoleSource>,
  registrySnapshot?: AgentRoleRegistrySnapshot | null,
  translate?: AgentRoleTranslator,
): string {
  const normalizedAgentId = normalizeAgentId(agentId);
  if (!normalizedAgentId) {
    return '';
  }

  const matchedAgent = resolveAgentRoleSource(normalizedAgentId, enabledAgents, registrySnapshot);
  const displayNameKey = normalizeAgentId(matchedAgent?.displayNameKey)
    || resolveRoleTemplateDisplayNameKey(normalizeAgentId(matchedAgent?.templateId));
  if (displayNameKey && translate) {
    const translated = translate(displayNameKey);
    if (translated && translated !== displayNameKey) {
      return translated;
    }
  }
  const displayName = normalizeAgentId(matchedAgent?.displayName);
  if (displayName) {
    return displayName;
  }

  return normalizedAgentId.charAt(0).toUpperCase() + normalizedAgentId.slice(1);
}
