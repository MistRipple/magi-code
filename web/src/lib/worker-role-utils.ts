import { normalizeWorkerSlot } from './message-classifier';

export interface WorkerRoleSource {
  templateId: string;
  displayName: string;
  displayNameKey?: string;
  modelSource?: 'orchestrator' | 'engine';
  engineId?: string;
  colorToken?: string;
  enabled?: boolean;
}

export interface WorkerRoleRegistrySnapshot {
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
    modelSource: 'orchestrator' | 'engine';
    engineId: string;
    enabled: boolean;
    order: number;
  }>;
}

const BUILTIN_ROLE_TEMPLATE_IDS = [
  'coordinator',
  'executor',
  'explorer',
  'reviewer',
  'tester',
  'architect',
];

function normalizeWorkerId(value: unknown): string {
  if (typeof value !== 'string') return '';
  return value.trim();
}

export type WorkerRoleTranslator = (key: string) => string;

function resolveRoleTemplateDisplayNameKey(templateId: string): string {
  const normalizedTemplateId = normalizeWorkerId(templateId);
  return normalizedTemplateId ? `roleTemplate.${normalizedTemplateId}.displayName` : '';
}

function resolveTemplateDisplayMeta(
  workerId: string,
  registrySnapshot?: WorkerRoleRegistrySnapshot | null,
): { displayName?: string; displayNameKey?: string; colorToken?: string } {
  const roleTemplates = Array.isArray(registrySnapshot?.roleTemplates)
    ? registrySnapshot.roleTemplates
    : [];
  const matchedTemplate = roleTemplates.find((template) => normalizeWorkerId(template.templateId) === workerId);
  const templateId = normalizeWorkerId(matchedTemplate?.templateId) || workerId;
  return {
    displayName: normalizeWorkerId(matchedTemplate?.displayName) || undefined,
    displayNameKey: normalizeWorkerId(matchedTemplate?.i18n?.displayNameKey) || resolveRoleTemplateDisplayNameKey(templateId) || undefined,
    colorToken: normalizeWorkerId(matchedTemplate?.defaultUI?.colorToken) || undefined,
  };
}

function collectKnownRoleTemplateIds(registrySnapshot?: WorkerRoleRegistrySnapshot | null): string[] {
  const roleTemplates = Array.isArray(registrySnapshot?.roleTemplates)
    ? registrySnapshot.roleTemplates
    : [];
  return Array.from(new Set([
    ...roleTemplates.map((template) => normalizeWorkerId(template.templateId)).filter(Boolean),
    ...BUILTIN_ROLE_TEMPLATE_IDS,
  ])).sort((left, right) => right.length - left.length);
}

function resolveRuntimeWorkerTemplateId(
  workerId: string,
  registrySnapshot?: WorkerRoleRegistrySnapshot | null,
): string {
  const normalizedWorkerId = normalizeWorkerId(workerId);
  if (!normalizedWorkerId) {
    return '';
  }
  for (const templateId of collectKnownRoleTemplateIds(registrySnapshot)) {
    if (normalizedWorkerId === templateId || normalizedWorkerId === `task-worker-${templateId}`) {
      return templateId;
    }
  }
  return '';
}

export function resolveTaskCardWorkerSlot(meta: Record<string, unknown> | undefined): string | null {
  if (!meta || typeof meta !== 'object') {
    return null;
  }
  const subTaskCard = meta.subTaskCard && typeof meta.subTaskCard === 'object'
    ? meta.subTaskCard as Record<string, unknown>
    : undefined;
  return normalizeWorkerSlot(subTaskCard?.worker)
    || normalizeWorkerSlot(meta.assignedWorker)
    || normalizeWorkerSlot(meta.worker);
}

export function resolveWorkerRoleSource(
  workerId: string,
  enabledAgents: ReadonlyArray<WorkerRoleSource>,
  registrySnapshot?: WorkerRoleRegistrySnapshot | null,
): WorkerRoleSource | null {
  const normalizedWorkerId = normalizeWorkerId(workerId);
  if (!normalizedWorkerId) {
    return null;
  }
  const normalizedRoleId = resolveRuntimeWorkerTemplateId(normalizedWorkerId, registrySnapshot);
  const lookupWorkerId = normalizedRoleId || normalizedWorkerId;

  const matchedEnabledAgent = enabledAgents.find((agent) => {
    const templateId = normalizeWorkerId(agent.templateId);
    const engineId = normalizeWorkerId(agent.engineId);
    return templateId === lookupWorkerId || engineId === lookupWorkerId;
  });
  if (matchedEnabledAgent) {
    const templateMeta = resolveTemplateDisplayMeta(
      normalizeWorkerId(matchedEnabledAgent.templateId) || normalizedWorkerId,
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
    const templateId = normalizeWorkerId(binding.templateId);
    const engineId = normalizeWorkerId(binding.engineId);
    return templateId === lookupWorkerId || engineId === lookupWorkerId;
  });
  if (!matchedBinding) {
    const templateMeta = resolveTemplateDisplayMeta(lookupWorkerId, registrySnapshot);
    if (normalizedRoleId || templateMeta.displayName || templateMeta.colorToken) {
      return {
        templateId: lookupWorkerId,
        displayName: templateMeta.displayName || lookupWorkerId,
        displayNameKey: templateMeta.displayNameKey,
        colorToken: templateMeta.colorToken,
      };
    }
    return null;
  }

  const templateMeta = resolveTemplateDisplayMeta(normalizeWorkerId(matchedBinding.templateId), registrySnapshot);
  return {
    templateId: normalizeWorkerId(matchedBinding.templateId),
    displayName: templateMeta.displayName || normalizeWorkerId(matchedBinding.templateId),
    displayNameKey: templateMeta.displayNameKey,
    modelSource: matchedBinding.modelSource === 'engine' ? 'engine' : 'orchestrator',
    engineId: normalizeWorkerId(matchedBinding.engineId) || undefined,
    colorToken: templateMeta.colorToken,
    enabled: matchedBinding.enabled,
  };
}

export function collectWorkerTabIds(
  projectionWorkerIds: ReadonlyArray<string>,
  enabledAgents: ReadonlyArray<WorkerRoleSource>,
  registrySnapshot?: WorkerRoleRegistrySnapshot | null,
): string[] {
  const workerTabs: string[] = [];
  const seen = new Set<string>();

  // 当前会话真实参与者来自当前 turn/lane 投影。
  // 内置角色默认可调度，但只有真实参与任务后才进入可视化工作台。
  for (const workerId of projectionWorkerIds) {
    const normalizedWorkerId = normalizeWorkerSlot(workerId) || normalizeWorkerId(workerId);
    if (!normalizedWorkerId) {
      continue;
    }
    const matchedRoleSource = resolveWorkerRoleSource(normalizedWorkerId, enabledAgents, registrySnapshot);
    const tabWorkerId = normalizeWorkerId(matchedRoleSource?.templateId) || normalizedWorkerId;
    if (seen.has(tabWorkerId)) {
      continue;
    }
    seen.add(tabWorkerId);
    workerTabs.push(tabWorkerId);
  }

  return workerTabs;
}



export function resolveWorkerDisplayName(
  workerId: string,
  enabledAgents: ReadonlyArray<WorkerRoleSource>,
  registrySnapshot?: WorkerRoleRegistrySnapshot | null,
  translate?: WorkerRoleTranslator,
): string {
  const normalizedWorkerId = normalizeWorkerId(workerId);
  if (!normalizedWorkerId) {
    return '';
  }

  const matchedAgent = resolveWorkerRoleSource(normalizedWorkerId, enabledAgents, registrySnapshot);
  const displayNameKey = normalizeWorkerId(matchedAgent?.displayNameKey)
    || resolveRoleTemplateDisplayNameKey(normalizeWorkerId(matchedAgent?.templateId));
  if (displayNameKey && translate) {
    const translated = translate(displayNameKey);
    if (translated && translated !== displayNameKey) {
      return translated;
    }
  }
  const displayName = normalizeWorkerId(matchedAgent?.displayName);
  if (displayName) {
    return displayName;
  }

  return normalizedWorkerId.charAt(0).toUpperCase() + normalizedWorkerId.slice(1);
}
